//! Application-owned installation policy for untrusted extension packages.
//!
//! Package parsing, lifecycle arbitration, and WebAssembly execution live in
//! reusable crates. This module deliberately contains the product decisions:
//! local packages may be unsigned, external native entrypoints are forbidden,
//! required permissions must be approved before activation, and safe mode is
//! enforced by the semantic host.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::Path,
};

use key_extension_api::{
    DataValue, EventSubscription, ExtensionEntrypoint, ExtensionId, IconRef, LifecycleState,
    MenuItem, MenuItemKind, PackagePath, Permission, PermissionRequest, SnapshotKind, UiNode,
    UiNodeKind,
};
use key_extension_host::{
    ExtensionHost, HostError, PackageMetadata, PackageOrigin, PermissionDecision,
    WasmExtensionAdapter,
};
use key_extension_package::{
    DenyAllSignatureVerifier, LoadedPackage, PackageError, PackageLimits, PackageLoader,
    PackageSourceKind, Sha256Digest, SignaturePolicy,
};
use key_extension_wasm::{
    WasmDiagnostic, WasmHostAdapterConfig, WasmRuntime, WasmRuntimeLimits, compile_host_adapter,
};
use key_pdf_extension_api::PdfCapability;

const MAX_INSTALLED_LOCAL_PACKAGES: usize = 32;
const MAX_ACTIVE_LOCAL_PACKAGES: usize = 8;
const MAX_COMPILED_COMPONENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageActivation {
    Active,
    Activating,
    AwaitingPermissions(Vec<PermissionRequest>),
    Inactive(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInstallReport {
    pub extension: ExtensionId,
    pub name: String,
    pub version: String,
    pub license: String,
    pub publisher_verified: bool,
    pub activation: PackageActivation,
}

/// Immutable package facts shown to the user before any host registry or
/// runtime state is mutated. The private content hash binds approval to the
/// exact package bytes that are subsequently installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInstallPreview {
    pub extension: ExtensionId,
    pub name: String,
    pub version: String,
    pub license: String,
    pub source: PackageSourceKind,
    pub publisher_verified: bool,
    pub required_permissions: Vec<PermissionRequest>,
    pub is_upgrade: bool,
    pub ui_kind: Option<String>,
    content_sha256: Sha256Digest,
    referenced_assets: Vec<(PackagePath, Vec<u8>)>,
}

/// Durable state supplied by the app when restoring one previously reviewed
/// package. Grouping the identity and authority snapshot keeps restoration a
/// single explicit boundary instead of an error-prone positional call.
pub(crate) struct RegisteredPackageRestore<'a> {
    pub path: &'a Path,
    pub expected_extension: &'a ExtensionId,
    pub expected_content_sha256: &'a str,
    pub permission_decisions: &'a [(Permission, PermissionDecision)],
    pub settings: Option<DataValue>,
    pub enabled: bool,
}

impl PackageInstallPreview {
    #[must_use]
    pub fn content_sha256_hex(&self) -> String {
        self.content_sha256.to_hex()
    }

    #[must_use]
    pub fn referenced_assets(&self) -> &[(PackagePath, Vec<u8>)] {
        &self.referenced_assets
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPackageSummary {
    pub extension: ExtensionId,
    pub name: String,
    pub version: String,
    pub license: String,
    pub source: PackageSourceKind,
    pub publisher_verified: bool,
    pub state: LifecycleState,
    /// Validated semantic UI payload kind, when the package declares one.
    /// The host still renders only the bounded contributions in the manifest.
    pub ui_kind: Option<String>,
    pub permissions: Vec<(PermissionRequest, PermissionDecision)>,
    pub restoration_error: Option<String>,
}

#[derive(Debug)]
pub enum ExtensionPackageError {
    Package(PackageError),
    Wasm(WasmDiagnostic),
    Host(HostError),
    ExternalNativeEntrypoint(ExtensionId),
    InvalidDeclarativeUi {
        extension: ExtensionId,
        reason: &'static str,
    },
    UpgradeActivationFailed {
        extension: ExtensionId,
        reason: String,
    },
    UpgradeRollbackFailed {
        extension: ExtensionId,
        reason: String,
    },
    UpgradeWhileSuspended(ExtensionId),
    PublisherMismatch(ExtensionId),
    PackageChangedAfterReview(ExtensionId),
    NotManaged(ExtensionId),
    InstalledPackageLimit {
        maximum: usize,
    },
    CompiledComponentBytesLimit {
        maximum_bytes: usize,
    },
    MissingReferencedAsset {
        extension: ExtensionId,
        path: PackagePath,
    },
    StoredIdentityMismatch {
        expected: ExtensionId,
        actual: ExtensionId,
    },
    StoredContentHashMismatch(ExtensionId),
    StoredPermissionMismatch {
        extension: ExtensionId,
        permission: Permission,
    },
}

impl fmt::Display for ExtensionPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Package(error) => error.fmt(formatter),
            Self::Wasm(error) => error.fmt(formatter),
            Self::Host(error) => error.fmt(formatter),
            Self::ExternalNativeEntrypoint(extension) => write!(
                formatter,
                "package {extension} declares a native entrypoint; installable packages may only be declarative or WebAssembly components"
            ),
            Self::InvalidDeclarativeUi { extension, reason } => {
                write!(
                    formatter,
                    "package {extension} has an invalid declarative UI payload: {reason}"
                )
            }
            Self::UpgradeActivationFailed { extension, reason } => write!(
                formatter,
                "extension {extension} upgrade was rolled back because activation failed: {reason}"
            ),
            Self::UpgradeRollbackFailed { extension, reason } => write!(
                formatter,
                "extension {extension} upgrade failed and the previous package could not be fully restored: {reason}"
            ),
            Self::UpgradeWhileSuspended(extension) => write!(
                formatter,
                "extension {extension} is suspended; leave safe mode or resume it before upgrading"
            ),
            Self::PublisherMismatch(extension) => write!(
                formatter,
                "package {extension} cannot replace an extension owned by a different publisher"
            ),
            Self::PackageChangedAfterReview(extension) => write!(
                formatter,
                "package {extension} changed after it was reviewed; review it again before installing"
            ),
            Self::NotManaged(extension) => {
                write!(
                    formatter,
                    "extension {extension} is not an installed local package"
                )
            }
            Self::InstalledPackageLimit { maximum } => write!(
                formatter,
                "cannot install more than {maximum} local extension packages"
            ),
            Self::CompiledComponentBytesLimit { maximum_bytes } => write!(
                formatter,
                "local extension components exceed the {} MiB compiled-byte budget",
                maximum_bytes / (1024 * 1024)
            ),
            Self::MissingReferencedAsset { extension, path } => write!(
                formatter,
                "package {extension} references missing asset '{}'",
                path.as_str()
            ),
            Self::StoredIdentityMismatch { expected, actual } => write!(
                formatter,
                "stored extension {expected} resolved to package {actual}"
            ),
            Self::StoredContentHashMismatch(extension) => write!(
                formatter,
                "stored package {extension} no longer matches its reviewed content hash"
            ),
            Self::StoredPermissionMismatch {
                extension,
                permission,
            } => write!(
                formatter,
                "stored package {extension} does not declare persisted permission {permission:?}"
            ),
        }
    }
}

#[derive(Clone)]
struct ManagedPackage {
    package: LoadedPackage,
    ui_kind: Option<String>,
    adapter: Option<WasmExtensionAdapter>,
}

impl ManagedPackage {
    fn manifest(&self) -> &key_extension_api::ExtensionManifest {
        self.package.manifest()
    }
}

#[derive(Clone)]
struct PendingUpgrade {
    candidate: ManagedPackage,
    reset_authority: bool,
    permissions_to_approve: Vec<PermissionRequest>,
}

#[derive(Clone)]
struct PermissionSnapshot {
    decisions: Vec<(Permission, PermissionDecision)>,
}

#[derive(Clone)]
struct ActivatingUpgrade {
    old: ManagedPackage,
    old_lifecycle: LifecycleState,
    permission_snapshot: PermissionSnapshot,
}

struct ActivationAttempt {
    report: PackageInstallReport,
    runtime_failed: bool,
}

impl Error for ExtensionPackageError {}

impl From<PackageError> for ExtensionPackageError {
    fn from(value: PackageError) -> Self {
        Self::Package(value)
    }
}

impl From<WasmDiagnostic> for ExtensionPackageError {
    fn from(value: WasmDiagnostic) -> Self {
        Self::Wasm(value)
    }
}

impl From<HostError> for ExtensionPackageError {
    fn from(value: HostError) -> Self {
        Self::Host(value)
    }
}

/// Owns immutable snapshots of locally installed packages and the optional
/// WebAssembly runtime. The app remains the only owner of filesystem paths and
/// user permission decisions.
pub struct InstallableExtensionManager {
    loader: PackageLoader,
    verifier: DenyAllSignatureVerifier,
    wasm: WasmRuntime,
    packages: BTreeMap<ExtensionId, ManagedPackage>,
    /// A permission-gated upgrade is held separately so the currently active
    /// package remains fully usable until the user has made a decision.
    pending_upgrades: BTreeMap<ExtensionId, PendingUpgrade>,
    activating_upgrades: BTreeMap<ExtensionId, ActivatingUpgrade>,
}

impl InstallableExtensionManager {
    pub fn new() -> Result<Self, ExtensionPackageError> {
        let loader = PackageLoader::new(
            PackageLimits::default(),
            SignaturePolicy {
                // A package chosen directly by the user is treated as an
                // unverified local package. Store distribution can inject a
                // strict verifier and require signatures without changing the
                // execution boundary.
                require_signed_archives: false,
                allow_unsigned_development_directories: true,
            },
        );
        Ok(Self {
            loader,
            verifier: DenyAllSignatureVerifier,
            wasm: WasmRuntime::new(WasmRuntimeLimits::default())?,
            packages: BTreeMap::new(),
            pending_upgrades: BTreeMap::new(),
            activating_upgrades: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    pub fn install(
        &mut self,
        host: &mut ExtensionHost,
        path: &Path,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        self.install_or_upgrade(host, path)
    }

    /// Installs a new package or atomically upgrades the manager-owned package
    /// with the same ID. A package that merely collides with a bundled or other
    /// host-owned ID is never treated as an upgrade.
    #[cfg(test)]
    pub fn install_or_upgrade(
        &mut self,
        host: &mut ExtensionHost,
        path: &Path,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let package = self.load_managed(path)?;
        let extension = package.manifest().id.clone();
        if self.packages.contains_key(&extension) {
            self.upgrade_loaded(host, package)
        } else {
            self.install_loaded(host, package)
        }
    }

    /// Parses and validates a package without registering or executing it.
    /// The returned value is safe to retain across an asynchronous user
    /// confirmation and is cryptographically bound to the package contents.
    pub fn preview(
        &self,
        host: &ExtensionHost,
        path: &Path,
    ) -> Result<PackageInstallPreview, ExtensionPackageError> {
        let managed = self.load_managed(path)?;
        self.preview_managed(host, &managed)
    }

    /// Installs the exact package approved by the user. The package is loaded
    /// once for this operation, compared with the earlier preview, and that
    /// same immutable snapshot is passed to installation so a changed local
    /// directory can never activate under stale approval.
    pub fn install_reviewed(
        &mut self,
        host: &mut ExtensionHost,
        path: &Path,
        reviewed: &PackageInstallPreview,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let managed = self.load_managed(path)?;
        let current = self.preview_managed(host, &managed)?;
        if &current != reviewed {
            return Err(ExtensionPackageError::PackageChangedAfterReview(
                current.extension,
            ));
        }
        if current.is_upgrade {
            self.upgrade_loaded(host, managed)
        } else {
            self.install_loaded(host, managed)
        }
    }

    /// Restores one package that was previously reviewed and recorded by the
    /// application. Identity, content, permissions, and settings are checked
    /// before optional activation; a changed source never inherits authority.
    pub(crate) fn restore_registered(
        &mut self,
        host: &mut ExtensionHost,
        restore: RegisteredPackageRestore<'_>,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let RegisteredPackageRestore {
            path,
            expected_extension,
            expected_content_sha256,
            permission_decisions,
            settings,
            enabled,
        } = restore;
        let managed = self.load_managed(path)?;
        let actual_extension = managed.manifest().id.clone();
        if &actual_extension != expected_extension {
            return Err(ExtensionPackageError::StoredIdentityMismatch {
                expected: expected_extension.clone(),
                actual: actual_extension,
            });
        }
        if managed.package.content_sha256().to_hex() != expected_content_sha256 {
            return Err(ExtensionPackageError::StoredContentHashMismatch(
                expected_extension.clone(),
            ));
        }
        for (permission, _) in permission_decisions {
            if !managed
                .manifest()
                .permissions
                .iter()
                .any(|request| request.permission == *permission)
            {
                return Err(ExtensionPackageError::StoredPermissionMismatch {
                    extension: expected_extension.clone(),
                    permission: permission.clone(),
                });
            }
        }

        self.install_loaded_inactive(host, managed)?;
        for (permission, decision) in permission_decisions {
            host.set_permission_decision(expected_extension.clone(), permission.clone(), *decision);
        }
        if let Some(settings) = settings {
            host.restore_extension_settings(expected_extension, settings)?;
        }
        if enabled {
            Ok(self
                .activation_report(host, expected_extension.clone())?
                .report)
        } else {
            host.unload(expected_extension)?;
            let managed = self
                .packages
                .get(expected_extension)
                .expect("restored package is managed");
            Ok(package_report(
                managed,
                PackageActivation::Inactive("extension remains disabled".into()),
            ))
        }
    }

    pub fn grant_permissions_and_activate(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        self.require_managed(extension)?;
        if let Some(pending) = self.pending_upgrades.remove(extension) {
            let retained = pending.clone();
            return match self.commit_upgrade(host, pending) {
                Ok(report) => Ok(report),
                Err(error) => {
                    self.pending_upgrades.insert(extension.clone(), retained);
                    Err(error)
                }
            };
        }
        let manifest = self
            .packages
            .get(extension)
            .expect("managed package exists")
            .manifest()
            .clone();
        for request in manifest
            .permissions
            .iter()
            .filter(|request| request.required)
        {
            host.set_permission_decision(
                extension.clone(),
                request.permission.clone(),
                PermissionDecision::Granted,
            );
        }
        Ok(self.activation_report(host, extension.clone())?.report)
    }

    pub fn deny_permissions(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<(), ExtensionPackageError> {
        self.require_managed(extension)?;
        if self.pending_upgrades.remove(extension).is_some() {
            // Candidate authority is staged outside the host. Rejecting an
            // upgrade must not change grants belonging to the installed
            // package that remains active.
            return Ok(());
        }
        let manifest = self
            .packages
            .get(extension)
            .expect("managed package exists")
            .manifest()
            .clone();
        for request in manifest
            .permissions
            .iter()
            .filter(|request| request.required)
        {
            host.set_permission_decision(
                extension.clone(),
                request.permission.clone(),
                PermissionDecision::Denied,
            );
        }
        host.unload(extension)?;
        Ok(())
    }

    pub fn disable(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<(), ExtensionPackageError> {
        self.require_managed(extension)?;
        self.pending_upgrades.remove(extension);
        self.activating_upgrades.remove(extension);
        host.unload(extension)?;
        Ok(())
    }

    pub fn enable(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        self.require_managed(extension)?;
        let managed = self
            .packages
            .get(extension)
            .expect("managed package exists");
        let permissions = managed
            .manifest()
            .permissions
            .iter()
            .filter(|request| {
                request.required
                    && host.permission_decision(extension, &request.permission)
                        != PermissionDecision::Granted
            })
            .cloned()
            .collect::<Vec<_>>();
        if !permissions.is_empty() {
            return Ok(package_report(
                managed,
                PackageActivation::AwaitingPermissions(permissions),
            ));
        }
        Ok(self.activation_report(host, extension.clone())?.report)
    }

    pub fn remove(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<(), ExtensionPackageError> {
        self.require_managed(extension)?;
        self.pending_upgrades.remove(extension);
        self.activating_upgrades.remove(extension);
        host.remove(extension)?;
        host.remove_wasm_adapter(extension);
        self.packages.remove(extension);
        Ok(())
    }

    #[must_use]
    pub fn is_managed(&self, extension: &ExtensionId) -> bool {
        self.packages.contains_key(extension)
    }

    pub fn set_permission_decision(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
        permission: &Permission,
        decision: PermissionDecision,
    ) -> Result<(), ExtensionPackageError> {
        let managed = self
            .packages
            .get(extension)
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        let request = managed
            .manifest()
            .permissions
            .iter()
            .find(|request| request.permission == *permission)
            .ok_or_else(|| ExtensionPackageError::StoredPermissionMismatch {
                extension: extension.clone(),
                permission: permission.clone(),
            })?;
        host.set_permission_decision(extension.clone(), permission.clone(), decision);
        if request.required
            && decision != PermissionDecision::Granted
            && matches!(
                host.state(extension),
                Some(LifecycleState::Active | LifecycleState::Suspended)
            )
        {
            self.activating_upgrades.remove(extension);
            host.unload(extension)?;
        }
        Ok(())
    }

    pub fn referenced_assets(
        &self,
        extension: &ExtensionId,
    ) -> Result<Vec<(PackagePath, Vec<u8>)>, ExtensionPackageError> {
        let managed = self
            .packages
            .get(extension)
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        referenced_asset_blobs(&managed.package)
    }

    pub fn content_sha256(&self, extension: &ExtensionId) -> Result<String, ExtensionPackageError> {
        self.packages
            .get(extension)
            .map(|managed| managed.package.content_sha256().to_hex())
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))
    }

    pub fn permission_decisions(
        &self,
        host: &ExtensionHost,
        extension: &ExtensionId,
    ) -> Result<Vec<(Permission, PermissionDecision)>, ExtensionPackageError> {
        let managed = self
            .packages
            .get(extension)
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        Ok(managed
            .manifest()
            .permissions
            .iter()
            .map(|request| {
                (
                    request.permission.clone(),
                    host.permission_decision(extension, &request.permission),
                )
            })
            .collect())
    }

    #[must_use]
    pub fn summaries(&self, host: &ExtensionHost) -> Vec<InstalledPackageSummary> {
        self.packages
            .iter()
            .map(|(extension, managed)| InstalledPackageSummary {
                extension: extension.clone(),
                name: managed.manifest().name.clone(),
                version: managed.manifest().version.to_string(),
                license: managed.manifest().license.clone(),
                source: managed.package.source(),
                publisher_verified: managed.package.signature().is_some(),
                state: host.state(extension).unwrap_or(LifecycleState::Removed),
                ui_kind: managed.ui_kind.clone(),
                permissions: managed
                    .manifest()
                    .permissions
                    .iter()
                    .cloned()
                    .map(|request| {
                        let decision = host.permission_decision(extension, &request.permission);
                        (request, decision)
                    })
                    .collect(),
                restoration_error: None,
            })
            .collect()
    }

    fn activation_report(
        &mut self,
        host: &mut ExtensionHost,
        extension: ExtensionId,
    ) -> Result<ActivationAttempt, ExtensionPackageError> {
        let managed = self
            .packages
            .get(&extension)
            .expect("activation is only attempted for a managed package");
        let name = managed.manifest().name.clone();
        let version = managed.manifest().version.to_string();
        let license = managed.manifest().license.clone();
        let publisher_verified = managed.package.signature().is_some();
        let (activation, runtime_failed) = if host.state(&extension) == Some(LifecycleState::Active)
        {
            (PackageActivation::Active, false)
        } else if self.active_local_package_count(host) >= MAX_ACTIVE_LOCAL_PACKAGES {
            host.unload(&extension)?;
            (
                PackageActivation::Inactive(format!(
                    "active local extension limit of {MAX_ACTIVE_LOCAL_PACKAGES} reached"
                )),
                false,
            )
        } else {
            match host.activate(&extension) {
                Ok(_) => (PackageActivation::Active, false),
                Err(HostError::PermissionsRequired { permissions, .. }) => {
                    // Permission preview must never leave an ambiguously
                    // "validated" package looking enabled in management UI.
                    host.unload(&extension)?;
                    (PackageActivation::AwaitingPermissions(permissions), false)
                }
                Err(error) => {
                    let runtime_failed = host.state(&extension) == Some(LifecycleState::Failed);
                    let reason = safe_activation_reason(&error);
                    host.unload(&extension)?;
                    (PackageActivation::Inactive(reason), runtime_failed)
                }
            }
        };
        Ok(ActivationAttempt {
            report: PackageInstallReport {
                extension,
                name,
                version,
                license,
                publisher_verified,
                activation,
            },
            runtime_failed,
        })
    }

    fn install_loaded(
        &mut self,
        host: &mut ExtensionHost,
        managed: ManagedPackage,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let extension = managed.manifest().id.clone();
        self.install_loaded_inactive(host, managed)?;
        Ok(self.activation_report(host, extension)?.report)
    }

    fn install_loaded_inactive(
        &mut self,
        host: &mut ExtensionHost,
        managed: ManagedPackage,
    ) -> Result<(), ExtensionPackageError> {
        let extension = managed.manifest().id.clone();
        self.reject_external_native(&managed.package)?;
        self.check_license_policy(host, managed.manifest())?;

        if self.packages.len() >= MAX_INSTALLED_LOCAL_PACKAGES {
            return Err(ExtensionPackageError::InstalledPackageLimit {
                maximum: MAX_INSTALLED_LOCAL_PACKAGES,
            });
        }
        // The immutable package was compiled during preparation. Install the
        // semantic package before associating its retained adapter so a
        // colliding host-owned ID cannot lose its existing adapter.
        host.install(
            managed.manifest().clone(),
            package_metadata(&managed.package),
        )?;
        register_managed_adapter(host, &managed);
        self.packages.insert(extension, managed);
        Ok(())
    }

    fn upgrade_loaded(
        &mut self,
        host: &mut ExtensionHost,
        managed: ManagedPackage,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let extension = managed.manifest().id.clone();
        self.reject_external_native(&managed.package)?;
        self.check_license_policy(host, managed.manifest())?;
        let old = self
            .packages
            .get(&extension)
            .cloned()
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        if host.state(&extension) == Some(LifecycleState::Suspended) {
            return Err(ExtensionPackageError::UpgradeWhileSuspended(extension));
        }
        if old.manifest().publisher.id != managed.manifest().publisher.id {
            return Err(ExtensionPackageError::PublisherMismatch(extension));
        }
        let inherits_authority = verified_signer_continuity(&old.package, &managed.package);
        self.pending_upgrades.remove(&extension);
        let permissions_to_approve = managed
            .manifest()
            .permissions
            .iter()
            .filter(|request| {
                request.required
                    && (!inherits_authority
                        || host.permission_decision(&extension, &request.permission)
                            != PermissionDecision::Granted)
            })
            .cloned()
            .collect::<Vec<_>>();
        let pending = PendingUpgrade {
            candidate: managed,
            reset_authority: !inherits_authority,
            permissions_to_approve: permissions_to_approve.clone(),
        };
        if !permissions_to_approve.is_empty() {
            let report = package_report(
                &pending.candidate,
                PackageActivation::AwaitingPermissions(permissions_to_approve),
            );
            self.pending_upgrades.insert(extension, pending);
            return Ok(report);
        }
        self.commit_upgrade(host, pending)
    }

    fn commit_upgrade(
        &mut self,
        host: &mut ExtensionHost,
        pending: PendingUpgrade,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let managed = pending.candidate;
        let extension = managed.manifest().id.clone();
        let old = self
            .packages
            .get(&extension)
            .cloned()
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        let old_lifecycle = host
            .state(&extension)
            .ok_or_else(|| ExtensionPackageError::NotManaged(extension.clone()))?;
        if old_lifecycle == LifecycleState::Suspended {
            return Err(ExtensionPackageError::UpgradeWhileSuspended(extension));
        }
        let permission_snapshot =
            capture_permission_snapshot(host, &extension, old.manifest(), managed.manifest());

        host.unload(&extension)?;
        host.remove_wasm_adapter(&extension);
        if pending.reset_authority {
            host.clear_permission_decisions(&extension);
        }
        for request in &pending.permissions_to_approve {
            host.set_permission_decision(
                extension.clone(),
                request.permission.clone(),
                PermissionDecision::Granted,
            );
        }

        if let Err(upgrade_error) = host.replace(
            managed.manifest().clone(),
            package_metadata(&managed.package),
        ) {
            restore_permission_snapshot(host, &extension, &permission_snapshot);
            if let Err(rollback_error) =
                self.restore_unreplaced_package(host, &extension, &old, old_lifecycle)
            {
                return Err(ExtensionPackageError::UpgradeRollbackFailed {
                    extension,
                    reason: safe_package_error_reason(&rollback_error),
                });
            }
            return Err(upgrade_error.into());
        }

        register_managed_adapter(host, &managed);
        self.packages.insert(extension.clone(), managed);
        self.pending_upgrades.remove(&extension);
        let attempt = match self.activate_replacement(host, &extension, old_lifecycle) {
            Ok(attempt) => attempt,
            Err(error) => {
                if let Err(rollback_error) = self.rollback_upgrade(
                    host,
                    &extension,
                    old,
                    old_lifecycle,
                    &permission_snapshot,
                ) {
                    return Err(ExtensionPackageError::UpgradeRollbackFailed {
                        extension,
                        reason: safe_package_error_reason(&rollback_error),
                    });
                }
                return Err(ExtensionPackageError::UpgradeActivationFailed {
                    extension,
                    reason: safe_package_error_reason(&error),
                });
            }
        };
        let mut attempt = attempt;
        if old_lifecycle == LifecycleState::Active && host.pending_extension_work(&extension) > 0 {
            attempt.report.activation = PackageActivation::Activating;
            self.activating_upgrades.insert(
                extension,
                ActivatingUpgrade {
                    old,
                    old_lifecycle,
                    permission_snapshot,
                },
            );
            return Ok(attempt.report);
        }
        let lifecycle_restored = match old_lifecycle {
            LifecycleState::Active => host.state(&extension) == Some(LifecycleState::Active),
            LifecycleState::Disabled => host.state(&extension) == Some(LifecycleState::Disabled),
            LifecycleState::Suspended => false,
            _ => true,
        };
        if attempt.runtime_failed || !lifecycle_restored {
            let reason = match &attempt.report.activation {
                PackageActivation::Inactive(reason) => reason.clone(),
                PackageActivation::AwaitingPermissions(_) => {
                    "additional permissions are required".into()
                }
                PackageActivation::Activating => {
                    "extension runtime did not settle activation".into()
                }
                PackageActivation::Active => "extension runtime failed during activation".into(),
            };
            if let Err(rollback_error) =
                self.rollback_upgrade(host, &extension, old, old_lifecycle, &permission_snapshot)
            {
                return Err(ExtensionPackageError::UpgradeRollbackFailed {
                    extension,
                    reason: safe_package_error_reason(&rollback_error),
                });
            }
            return Err(ExtensionPackageError::UpgradeActivationFailed { extension, reason });
        }
        Ok(attempt.report)
    }

    /// Finalizes asynchronous Wasm upgrades only after the worker's activation
    /// result has crossed the normal host tick. Until then the previous package
    /// snapshot is retained for transactional rollback.
    pub fn settle_activating_upgrades(
        &mut self,
        host: &mut ExtensionHost,
    ) -> Vec<(
        ExtensionId,
        Result<PackageInstallReport, ExtensionPackageError>,
    )> {
        let ready = self
            .activating_upgrades
            .keys()
            .filter(|extension| host.pending_extension_work(extension) == 0)
            .cloned()
            .collect::<Vec<_>>();
        ready
            .into_iter()
            .map(|extension| {
                let pending = self
                    .activating_upgrades
                    .remove(&extension)
                    .expect("ready upgrade remains tracked");
                if host.state(&extension) == Some(LifecycleState::Active) {
                    let managed = self
                        .packages
                        .get(&extension)
                        .expect("activating package remains managed");
                    return (
                        extension,
                        Ok(package_report(managed, PackageActivation::Active)),
                    );
                }
                // Package-facing failures stay deliberately coarse: detailed
                // Wasmtime diagnostics remain available in the host log but
                // are not exposed through a trust or permission prompt.
                let reason = "extension runtime failed during activation".into();
                let result = self
                    .rollback_upgrade(
                        host,
                        &extension,
                        pending.old,
                        pending.old_lifecycle,
                        &pending.permission_snapshot,
                    )
                    .map_err(|rollback| ExtensionPackageError::UpgradeRollbackFailed {
                        extension: extension.clone(),
                        reason: safe_package_error_reason(&rollback),
                    })
                    .and_then(|()| {
                        Err(ExtensionPackageError::UpgradeActivationFailed {
                            extension: extension.clone(),
                            reason,
                        })
                    });
                (extension, result)
            })
            .collect()
    }

    fn activate_replacement(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
        old_lifecycle: LifecycleState,
    ) -> Result<ActivationAttempt, ExtensionPackageError> {
        match old_lifecycle {
            LifecycleState::Active => self.activation_report(host, extension.clone()),
            LifecycleState::Suspended => Err(ExtensionPackageError::UpgradeWhileSuspended(
                extension.clone(),
            )),
            _ => {
                host.unload(extension)?;
                let managed = self
                    .packages
                    .get(extension)
                    .expect("replacement package is managed");
                Ok(ActivationAttempt {
                    report: package_report(
                        managed,
                        PackageActivation::Inactive("extension remains disabled".into()),
                    ),
                    runtime_failed: false,
                })
            }
        }
    }

    fn restore_unreplaced_package(
        &self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
        old: &ManagedPackage,
        old_lifecycle: LifecycleState,
    ) -> Result<(), ExtensionPackageError> {
        register_managed_adapter(host, old);
        restore_lifecycle(host, extension, old_lifecycle)
    }

    fn load_managed(&self, path: &Path) -> Result<ManagedPackage, ExtensionPackageError> {
        let package = self.load(path)?;
        self.reject_external_native(&package)?;
        let ui_kind = validate_declarative_ui(&package)?;
        let _ = referenced_asset_blobs(&package)?;
        let extension = &package.manifest().id;
        if !self.packages.contains_key(extension)
            && self.packages.len() >= MAX_INSTALLED_LOCAL_PACKAGES
        {
            return Err(ExtensionPackageError::InstalledPackageLimit {
                maximum: MAX_INSTALLED_LOCAL_PACKAGES,
            });
        }
        self.check_component_byte_budget(&package)?;
        let adapter = self.compile_component_adapter(&package)?;
        Ok(ManagedPackage {
            package,
            ui_kind,
            adapter,
        })
    }

    fn preview_managed(
        &self,
        host: &ExtensionHost,
        managed: &ManagedPackage,
    ) -> Result<PackageInstallPreview, ExtensionPackageError> {
        self.reject_external_native(&managed.package)?;
        self.check_license_policy(host, managed.manifest())?;
        let manifest = managed.manifest();
        let is_upgrade = self.packages.contains_key(&manifest.id);
        if let Some(current) = self.packages.get(&manifest.id) {
            if host.state(&manifest.id) == Some(LifecycleState::Suspended) {
                return Err(ExtensionPackageError::UpgradeWhileSuspended(
                    manifest.id.clone(),
                ));
            }
            if current.manifest().publisher.id != manifest.publisher.id {
                return Err(ExtensionPackageError::PublisherMismatch(
                    manifest.id.clone(),
                ));
            }
        } else if host.state(&manifest.id).is_some() {
            return Err(ExtensionPackageError::Host(HostError::EventRejected(
                format!(
                    "extension {} is already owned by the host and cannot be replaced by a local package",
                    manifest.id
                ),
            )));
        }
        Ok(PackageInstallPreview {
            extension: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.to_string(),
            license: manifest.license.clone(),
            source: managed.package.source(),
            publisher_verified: managed.package.signature().is_some(),
            required_permissions: manifest
                .permissions
                .iter()
                .filter(|request| request.required)
                .cloned()
                .collect(),
            is_upgrade,
            ui_kind: managed.ui_kind.clone(),
            content_sha256: managed.package.content_sha256(),
            referenced_assets: referenced_asset_blobs(&managed.package)?,
        })
    }

    fn check_license_policy(
        &self,
        host: &ExtensionHost,
        manifest: &key_extension_api::ExtensionManifest,
    ) -> Result<(), ExtensionPackageError> {
        if host
            .config()
            .allowed_license_expressions
            .contains(&manifest.license)
        {
            Ok(())
        } else {
            Err(HostError::LicenseDenied {
                extension: manifest.id.clone(),
                license: manifest.license.clone(),
            }
            .into())
        }
    }

    fn load(&self, path: &Path) -> Result<LoadedPackage, ExtensionPackageError> {
        if path.is_dir() {
            self.loader
                .load_development_directory(path, &self.verifier)
                .map_err(Into::into)
        } else {
            self.loader
                .load_keyext(path, &self.verifier)
                .map_err(Into::into)
        }
    }

    fn reject_external_native(&self, package: &LoadedPackage) -> Result<(), ExtensionPackageError> {
        if matches!(
            package.manifest().entrypoint,
            ExtensionEntrypoint::NativeBuiltin { .. }
        ) {
            Err(ExtensionPackageError::ExternalNativeEntrypoint(
                package.manifest().id.clone(),
            ))
        } else {
            Ok(())
        }
    }

    fn compile_component_adapter(
        &self,
        package: &LoadedPackage,
    ) -> Result<Option<key_extension_host::WasmExtensionAdapter>, ExtensionPackageError> {
        let ExtensionEntrypoint::WasmComponent {
            component, world, ..
        } = &package.manifest().entrypoint
        else {
            return Ok(None);
        };
        let bytes = package
            .component()
            .expect("validated Wasm package retains its declared component")
            .bytes();
        Ok(Some(compile_host_adapter(
            &self.wasm,
            WasmHostAdapterConfig {
                extension: package.manifest().id.clone(),
                component: component.clone(),
                world: world.clone(),
                subscriptions: subscriptions_for_manifest(package.manifest()),
            },
            bytes,
        )?))
    }

    fn check_component_byte_budget(
        &self,
        candidate: &LoadedPackage,
    ) -> Result<(), ExtensionPackageError> {
        let candidate_extension = &candidate.manifest().id;
        let managed_bytes = self
            .packages
            .values()
            .map(|package| component_bytes(&package.package))
            .sum::<usize>();
        let pending_bytes = self
            .pending_upgrades
            .iter()
            .filter(|(extension, _)| *extension != candidate_extension)
            .map(|(_, pending)| component_bytes(&pending.candidate.package))
            .sum::<usize>();
        if !component_budget_allows(managed_bytes, pending_bytes, component_bytes(candidate)) {
            return Err(ExtensionPackageError::CompiledComponentBytesLimit {
                maximum_bytes: MAX_COMPILED_COMPONENT_BYTES,
            });
        }
        Ok(())
    }

    fn active_local_package_count(&self, host: &ExtensionHost) -> usize {
        self.packages
            .keys()
            .filter(|extension| host.state(extension) == Some(LifecycleState::Active))
            .count()
    }

    fn require_managed(&self, extension: &ExtensionId) -> Result<(), ExtensionPackageError> {
        if self.packages.contains_key(extension) {
            Ok(())
        } else {
            Err(ExtensionPackageError::NotManaged(extension.clone()))
        }
    }

    fn rollback_upgrade(
        &mut self,
        host: &mut ExtensionHost,
        extension: &ExtensionId,
        old: ManagedPackage,
        old_lifecycle: LifecycleState,
        permission_snapshot: &PermissionSnapshot,
    ) -> Result<(), ExtensionPackageError> {
        host.unload(extension)?;
        host.remove_wasm_adapter(extension);
        host.replace(old.manifest().clone(), package_metadata(&old.package))?;
        register_managed_adapter(host, &old);
        self.packages.insert(extension.clone(), old);
        restore_permission_snapshot(host, extension, permission_snapshot);
        restore_lifecycle(host, extension, old_lifecycle)
    }
}

fn referenced_asset_blobs(
    package: &LoadedPackage,
) -> Result<Vec<(PackagePath, Vec<u8>)>, ExtensionPackageError> {
    let mut paths = BTreeSet::new();
    for menu in &package.manifest().contributions.menus {
        collect_menu_asset_paths(&menu.items, &mut paths);
    }
    for view in &package.manifest().contributions.views {
        collect_ui_asset_paths(&view.root, &mut paths);
    }
    paths
        .into_iter()
        .map(|path| {
            package
                .assets()
                .get(&path)
                .map(|blob| (path.clone(), blob.bytes().to_vec()))
                .ok_or_else(|| ExtensionPackageError::MissingReferencedAsset {
                    extension: package.manifest().id.clone(),
                    path,
                })
        })
        .collect()
}

fn collect_menu_asset_paths(items: &[MenuItem], paths: &mut BTreeSet<PackagePath>) {
    for item in items {
        match &item.kind {
            MenuItemKind::Command {
                icon: Some(IconRef::Asset(path)),
                ..
            } => {
                paths.insert(path.clone());
            }
            MenuItemKind::Submenu { icon, children, .. } => {
                if let Some(IconRef::Asset(path)) = icon {
                    paths.insert(path.clone());
                }
                collect_menu_asset_paths(children, paths);
            }
            MenuItemKind::Command { .. } | MenuItemKind::Separator => {}
        }
    }
}

fn collect_ui_asset_paths(node: &UiNode, paths: &mut BTreeSet<PackagePath>) {
    match &node.kind {
        UiNodeKind::IconButton {
            icon: IconRef::Asset(path),
            ..
        }
        | UiNodeKind::Image { asset: path, .. } => {
            paths.insert(path.clone());
        }
        UiNodeKind::Tabs { tabs, .. } => {
            for tab in tabs {
                collect_ui_asset_paths(&tab.content, paths);
            }
        }
        _ => {
            for child in node.kind.children() {
                collect_ui_asset_paths(child, paths);
            }
        }
    }
}

fn subscriptions_for_manifest(
    manifest: &key_extension_api::ExtensionManifest,
) -> Vec<EventSubscription> {
    let mut subscriptions = vec![
        EventSubscription::Lifecycle,
        EventSubscription::Commands,
        EventSubscription::Capabilities,
        EventSubscription::EffectResults,
    ];
    let requested_pdf_capabilities = manifest
        .capabilities
        .required
        .iter()
        .chain(&manifest.capabilities.optional)
        .filter_map(|request| PdfCapability::from_name(request.id.as_str()))
        .collect::<Vec<_>>();
    if requested_pdf_capabilities.iter().any(|capability| {
        matches!(
            capability,
            PdfCapability::DocumentMetadata
                | PdfCapability::PageMetadata
                | PdfCapability::Outline
                | PdfCapability::Links
                | PdfCapability::Text
        )
    }) {
        subscriptions.push(EventSubscription::Snapshot(SnapshotKind::Document));
    }
    if requested_pdf_capabilities.contains(&PdfCapability::Viewport) {
        subscriptions.push(EventSubscription::Snapshot(SnapshotKind::Viewport));
    }
    if requested_pdf_capabilities.contains(&PdfCapability::Selection) {
        subscriptions.push(EventSubscription::Snapshot(SnapshotKind::Selection));
    }
    if manifest.permissions.iter().any(|request| {
        matches!(
            request.permission,
            Permission::ReadAnnotations | Permission::WriteAnnotations
        )
    }) {
        subscriptions.push(EventSubscription::Snapshot(SnapshotKind::Annotation));
    }
    subscriptions
}

fn package_report(package: &ManagedPackage, activation: PackageActivation) -> PackageInstallReport {
    PackageInstallReport {
        extension: package.manifest().id.clone(),
        name: package.manifest().name.clone(),
        version: package.manifest().version.to_string(),
        license: package.manifest().license.clone(),
        publisher_verified: package.package.signature().is_some(),
        activation,
    }
}

fn component_bytes(package: &LoadedPackage) -> usize {
    package
        .component()
        .map_or(0, |component| component.bytes().len())
}

fn component_budget_allows(managed: usize, pending: usize, candidate: usize) -> bool {
    managed
        .checked_add(pending)
        .and_then(|bytes| bytes.checked_add(candidate))
        .is_some_and(|bytes| bytes <= MAX_COMPILED_COMPONENT_BYTES)
}

fn register_managed_adapter(host: &mut ExtensionHost, package: &ManagedPackage) {
    if let Some(adapter) = &package.adapter {
        host.register_wasm_adapter(adapter.clone());
    }
}

fn signer_identities_match(old: Option<(&str, &str)>, new: Option<(&str, &str)>) -> bool {
    matches!((old, new), (Some(old), Some(new)) if old == new)
}

fn verified_signer_continuity(old: &LoadedPackage, new: &LoadedPackage) -> bool {
    signer_identities_match(verified_signer_identity(old), verified_signer_identity(new))
}

fn verified_signer_identity(package: &LoadedPackage) -> Option<(&str, &str)> {
    package.signature().map(|signature| {
        (
            signature.signer().key_id.as_str(),
            signature.signer().identity.as_str(),
        )
    })
}

fn capture_permission_snapshot(
    host: &ExtensionHost,
    extension: &ExtensionId,
    old: &key_extension_api::ExtensionManifest,
    new: &key_extension_api::ExtensionManifest,
) -> PermissionSnapshot {
    let mut permissions = Vec::new();
    for request in old.permissions.iter().chain(&new.permissions) {
        if !permissions.contains(&request.permission) {
            permissions.push(request.permission.clone());
        }
    }
    PermissionSnapshot {
        decisions: permissions
            .into_iter()
            .map(|permission| {
                let decision = host.permission_decision(extension, &permission);
                (permission, decision)
            })
            .collect(),
    }
}

fn restore_permission_snapshot(
    host: &mut ExtensionHost,
    extension: &ExtensionId,
    snapshot: &PermissionSnapshot,
) {
    host.clear_permission_decisions(extension);
    for (permission, decision) in &snapshot.decisions {
        host.set_permission_decision(extension.clone(), permission.clone(), *decision);
    }
}

fn restore_lifecycle(
    host: &mut ExtensionHost,
    extension: &ExtensionId,
    lifecycle: LifecycleState,
) -> Result<(), ExtensionPackageError> {
    match lifecycle {
        LifecycleState::Active => {
            host.activate(extension)?;
        }
        LifecycleState::Suspended => {
            return Err(ExtensionPackageError::UpgradeWhileSuspended(
                extension.clone(),
            ));
        }
        LifecycleState::Disabled => host.unload(extension)?,
        LifecycleState::Validated => {}
        LifecycleState::Failed => match host.activate(extension) {
            Err(_) if host.state(extension) == Some(LifecycleState::Failed) => {}
            Ok(_) => host.unload(extension)?,
            Err(error) => return Err(error.into()),
        },
        LifecycleState::Installed | LifecycleState::Unloading | LifecycleState::Removed => {
            host.unload(extension)?;
        }
    }
    Ok(())
}

/// Validate the app-level declarative payload rather than treating any bounded
/// JSON object as an executable UI contract. The host-rendered node tree and
/// commands remain the typed `manifest.contributions`; this descriptor carries
/// only bounded semantic bindings/preset metadata that the trusted host may
/// select by `kind`.
fn validate_declarative_ui(
    package: &LoadedPackage,
) -> Result<Option<String>, ExtensionPackageError> {
    let Some(ui) = package.ui() else {
        return Ok(None);
    };
    let invalid = |reason| ExtensionPackageError::InvalidDeclarativeUi {
        extension: package.manifest().id.clone(),
        reason,
    };
    let value: serde_json::Value =
        serde_json::from_slice(ui.bytes()).map_err(|_| invalid("payload is not valid JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("payload root must be an object"))?;
    const ALLOWED_FIELDS: &[&str] = &[
        "schema_version",
        "kind",
        "bindings",
        "state",
        "updates_from",
        "tokens_only",
        "note",
    ];
    if object
        .keys()
        .any(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
    {
        return Err(invalid("payload contains an unsupported field"));
    }
    if object
        .get("schema_version")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err(invalid("schema_version must be 1"));
    }
    let kind = object
        .get("kind")
        .and_then(|value| value.as_str())
        .filter(|value| semantic_name_is_safe(value))
        .ok_or_else(|| invalid("kind must be a bounded semantic identifier"))?;
    for field in ["bindings", "state"] {
        if let Some(value) = object.get(field) {
            validate_binding_map(value).map_err(invalid)?;
        }
    }
    if object.get("updates_from").is_some_and(|value| {
        value
            .as_str()
            .is_none_or(|value| !semantic_name_is_safe(value))
    }) {
        return Err(invalid(
            "updates_from must be a bounded semantic identifier",
        ));
    }
    if object
        .get("tokens_only")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("tokens_only must be a boolean"));
    }
    if object.get("note").is_some_and(|value| !value.is_string()) {
        return Err(invalid("note must be a string"));
    }
    Ok(Some(kind.to_owned()))
}

fn validate_binding_map(value: &serde_json::Value) -> Result<(), &'static str> {
    let Some(bindings) = value.as_object() else {
        return Err("binding collections must be objects");
    };
    if bindings.is_empty() || bindings.len() > 256 {
        return Err("binding collections must contain 1 to 256 entries");
    }
    for (name, path) in bindings {
        if !semantic_name_is_safe(name) {
            return Err("binding names must be bounded semantic identifiers");
        }
        let Some(segments) = path.as_array() else {
            return Err("binding paths must be arrays");
        };
        if segments.is_empty() || segments.len() > 16 {
            return Err("binding paths must contain 1 to 16 segments");
        }
        if segments.iter().any(|segment| {
            segment
                .as_str()
                .is_none_or(|segment| !semantic_name_is_safe(segment))
        }) {
            return Err("binding path segments must be bounded semantic identifiers");
        }
    }
    Ok(())
}

fn semantic_name_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn safe_activation_reason(error: &HostError) -> String {
    match error {
        HostError::SafeMode(_) => "disabled while safe mode is enabled".into(),
        HostError::LicenseDenied { .. } => "package license is not allowed".into(),
        HostError::DependencyUnavailable { dependency, .. } => {
            format!("required extension dependency {dependency} is unavailable")
        }
        HostError::RequiredCapabilityUnavailable { capability, .. } => {
            format!("required host capability {capability} is unavailable")
        }
        HostError::PermissionsRequired { .. } => "additional permissions are required".into(),
        HostError::PermissionDenied { permission, .. } => {
            format!("required permission {permission:?} was denied")
        }
        HostError::AdapterUnavailable(_) | HostError::UnsupportedEntrypoint(_) => {
            "extension runtime is unavailable".into()
        }
        HostError::ExtensionFailed { .. } => "extension runtime failed during activation".into(),
        HostError::Registry(_)
        | HostError::NotInstalled(_)
        | HostError::InvalidState { .. }
        | HostError::EventRejected(_)
        | HostError::EffectNotPending { .. } => "extension is not in an activatable state".into(),
    }
}

fn safe_package_error_reason(error: &ExtensionPackageError) -> String {
    match error {
        ExtensionPackageError::Host(error) => safe_activation_reason(error),
        ExtensionPackageError::Wasm(_) => "WebAssembly runtime restoration failed".into(),
        ExtensionPackageError::Package(_) => "stored package restoration failed".into(),
        ExtensionPackageError::ExternalNativeEntrypoint(_) => {
            "stored package requested unsupported native execution".into()
        }
        ExtensionPackageError::InvalidDeclarativeUi { .. } => {
            "stored package UI validation failed".into()
        }
        ExtensionPackageError::UpgradeActivationFailed { .. }
        | ExtensionPackageError::UpgradeRollbackFailed { .. } => {
            "nested package rollback failed".into()
        }
        ExtensionPackageError::UpgradeWhileSuspended(_) => {
            "suspended package cannot be upgraded".into()
        }
        ExtensionPackageError::PublisherMismatch(_) => {
            "stored package publisher identity changed".into()
        }
        ExtensionPackageError::PackageChangedAfterReview(_) => {
            "stored package changed after review".into()
        }
        ExtensionPackageError::NotManaged(_) => "stored package is no longer managed".into(),
        ExtensionPackageError::InstalledPackageLimit { .. } => {
            "local package count limit reached".into()
        }
        ExtensionPackageError::CompiledComponentBytesLimit { .. } => {
            "local component byte budget reached".into()
        }
        ExtensionPackageError::MissingReferencedAsset { .. } => {
            "stored package references a missing UI asset".into()
        }
        ExtensionPackageError::StoredIdentityMismatch { .. } => {
            "stored package identity changed".into()
        }
        ExtensionPackageError::StoredContentHashMismatch(_) => {
            "stored package content changed after review".into()
        }
        ExtensionPackageError::StoredPermissionMismatch { .. } => {
            "stored package permission declaration changed".into()
        }
    }
}

fn package_metadata(package: &LoadedPackage) -> PackageMetadata {
    PackageMetadata {
        origin: match package.source() {
            PackageSourceKind::DevelopmentDirectory => PackageOrigin::TrustedLocal,
            PackageSourceKind::KeyextArchive => PackageOrigin::ThirdParty,
        },
        content_hash: Some(package.content_sha256().to_hex()),
        publisher_verified: package.signature().is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        str::FromStr,
        time::{Duration, Instant},
    };

    use key_extension_api::{
        CURRENT_MANIFEST_SCHEMA, CapabilityId, CapabilityRequirements, CommandBehavior,
        CommandBehaviorAction, CommandDefinition, CommandId, CompatibleVersion, ContributionSet,
        DataValue, ExtensionEvent, ExtensionManifest, ExtensionVersion, HostCompatibility,
        PackagePath, Permission, Publisher, SettingsSchema, SnapshotKind, StateBinding,
        StorageRequirements, UiNodeKind,
    };
    use key_extension_host::PackageMetadata;
    use tempfile::TempDir;

    use super::*;

    const HEALTHY_COMPONENT: &[u8] =
        include_bytes!("../../../extensions/reference-document-statistics/package/component.wasm");
    const EMPTY_COMPONENT: &[u8] = b"\0asm\r\0\x01\0";

    fn drain_host(host: &mut ExtensionHost) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            host.process_tick();
            if host.pending_event_count() == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "extension host did not settle before the test deadline"
            );
            std::thread::yield_now();
        }
    }

    fn manifest(id: &str, entrypoint: ExtensionEntrypoint) -> ExtensionManifest {
        ExtensionManifest {
            schema_version: CURRENT_MANIFEST_SCHEMA,
            id: ExtensionId::parse(id).unwrap(),
            name: "Local extension".into(),
            version: ExtensionVersion::from_str("1.0.0").unwrap(),
            publisher: Publisher {
                id: "local-test".into(),
                name: "Local Test".into(),
            },
            description: "Installation fixture".into(),
            license: "MIT".into(),
            compatibility: HostCompatibility {
                extension_api: CompatibleVersion::from_str("^0.1").unwrap(),
                minimum_host: None,
                platforms: Vec::new(),
            },
            entrypoint,
            dependencies: Vec::new(),
            capabilities: CapabilityRequirements::default(),
            permissions: Vec::new(),
            contributions: ContributionSet::default(),
            settings: SettingsSchema::default(),
            storage: StorageRequirements::default(),
        }
    }

    fn development_package(manifest: &ExtensionManifest) -> TempDir {
        development_package_with_ui(manifest, br#"{"schema_version":1,"kind":"test_fixture"}"#)
    }

    fn development_package_with_ui(manifest: &ExtensionManifest, ui_payload: &[u8]) -> TempDir {
        development_package_with_component(manifest, ui_payload, HEALTHY_COMPONENT)
    }

    fn development_package_with_component(
        manifest: &ExtensionManifest,
        ui_payload: &[u8],
        component_bytes: &[u8],
    ) -> TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("manifest.toml"),
            toml::to_string(manifest).unwrap(),
        )
        .unwrap();
        if let ExtensionEntrypoint::Declarative { ui }
        | ExtensionEntrypoint::WasmComponent { ui: Some(ui), .. }
        | ExtensionEntrypoint::NativeBuiltin { ui: Some(ui), .. } = &manifest.entrypoint
        {
            fs::write(directory.path().join(ui.as_str()), ui_payload).unwrap();
        }
        if let ExtensionEntrypoint::WasmComponent { component, .. } = &manifest.entrypoint {
            fs::write(directory.path().join(component.as_str()), component_bytes).unwrap();
        }
        directory
    }

    fn declarative(id: &str) -> ExtensionManifest {
        manifest(
            id,
            ExtensionEntrypoint::Declarative {
                ui: PackagePath::parse("ui.json").unwrap(),
            },
        )
    }

    fn wasm(id: &str) -> ExtensionManifest {
        manifest(
            id,
            ExtensionEntrypoint::WasmComponent {
                component: PackagePath::parse("component.wasm").unwrap(),
                world: "key:extension-runtime/extension@0.1.0".into(),
                ui: Some(PackagePath::parse("ui.json").unwrap()),
            },
        )
    }

    fn at_version(mut manifest: ExtensionManifest, version: &str) -> ExtensionManifest {
        manifest.version = ExtensionVersion::from_str(version).unwrap();
        manifest
    }

    fn add_command(manifest: &mut ExtensionManifest, local: &str) {
        let command = CommandId::parse(format!("{}/{local}", manifest.id)).unwrap();
        manifest.contributions.commands.push(CommandDefinition {
            id: command.clone(),
            title: "Open fixture".into(),
            description: "Lifecycle test contribution".into(),
            category: "Tests".into(),
        });
        if matches!(manifest.entrypoint, ExtensionEntrypoint::Declarative { .. }) {
            manifest
                .contributions
                .command_behaviors
                .push(CommandBehavior {
                    command,
                    action: CommandBehaviorAction::SetState {
                        binding: StateBinding::new(["fixture-open"]),
                    },
                });
        }
    }

    fn reference_workspace() -> std::path::PathBuf {
        let starts = [
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            std::path::PathBuf::from(file!())
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        ];
        for start in starts {
            for ancestor in start.ancestors() {
                if ancestor
                    .join("extensions/reference-theme-pack/package/manifest.toml")
                    .is_file()
                {
                    return ancestor.to_path_buf();
                }
            }
        }
        panic!("reference extension workspace could not be located")
    }

    #[test]
    fn declarative_package_can_be_installed_disabled_enabled_and_removed() {
        let mut package_manifest = declarative("org.example.lifecycle");
        add_command(&mut package_manifest, "open");
        let package = development_package(&package_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let report = manager.install(&mut host, package.path()).unwrap();
        assert_eq!(report.activation, PackageActivation::Active);
        assert_eq!(report.license, "MIT");
        let summary = manager.summaries(&host).pop().unwrap();
        assert_eq!(summary.license, "MIT");
        assert_eq!(summary.ui_kind.as_deref(), Some("test_fixture"));
        let contributions = host.collect_contributions();
        assert_eq!(contributions.commands.len(), 1);
        assert_eq!(contributions.commands[0].owner, report.extension);

        manager.disable(&mut host, &report.extension).unwrap();
        assert_eq!(
            host.state(&report.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(
            manager
                .enable(&mut host, &report.extension)
                .unwrap()
                .activation,
            PackageActivation::Active
        );
        manager.remove(&mut host, &report.extension).unwrap();
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn package_preview_exposes_policy_facts_without_mutating_host() {
        let mut package_manifest = declarative("org.example.review");
        package_manifest.license = "Apache-2.0".into();
        package_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Show reviewed content".into(),
            required: true,
        });
        let package = development_package(&package_manifest);
        let host = ExtensionHost::new(Default::default());
        let manager = InstallableExtensionManager::new().unwrap();

        let preview = manager.preview(&host, package.path()).unwrap();
        assert_eq!(preview.extension, package_manifest.id);
        assert_eq!(preview.license, "Apache-2.0");
        assert_eq!(preview.source, PackageSourceKind::DevelopmentDirectory);
        assert!(!preview.publisher_verified);
        assert!(!preview.is_upgrade);
        assert_eq!(preview.ui_kind.as_deref(), Some("test_fixture"));
        assert_eq!(preview.required_permissions.len(), 1);
        assert_eq!(host.state(&preview.extension), None);
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn reviewed_install_rejects_package_bytes_changed_after_confirmation() {
        let package_manifest = declarative("org.example.review-change");
        let package = development_package(&package_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let preview = manager.preview(&host, package.path()).unwrap();

        let changed = at_version(package_manifest, "1.0.1");
        fs::write(
            package.path().join("manifest.toml"),
            toml::to_string(&changed).unwrap(),
        )
        .unwrap();
        let error = manager
            .install_reviewed(&mut host, package.path(), &preview)
            .unwrap_err();
        assert!(matches!(
            error,
            ExtensionPackageError::PackageChangedAfterReview(_)
        ));
        assert_eq!(host.state(&preview.extension), None);
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn verified_authority_requires_matching_key_and_identity() {
        assert!(signer_identities_match(
            Some(("release-key", "verified-publisher")),
            Some(("release-key", "verified-publisher"))
        ));
        assert!(!signer_identities_match(
            Some(("release-key", "verified-publisher")),
            Some(("rotated-key", "verified-publisher"))
        ));
        assert!(!signer_identities_match(
            Some(("release-key", "verified-publisher")),
            Some(("release-key", "impostor"))
        ));
        assert!(!signer_identities_match(
            Some(("release-key", "verified-publisher")),
            None
        ));
        assert!(!signer_identities_match(None, None));
    }

    #[test]
    fn cumulative_component_budget_is_overflow_safe_and_inclusive() {
        assert!(component_budget_allows(
            32 * 1024 * 1024,
            16 * 1024 * 1024,
            16 * 1024 * 1024
        ));
        assert!(!component_budget_allows(
            32 * 1024 * 1024,
            16 * 1024 * 1024,
            16 * 1024 * 1024 + 1
        ));
        assert!(!component_budget_allows(usize::MAX, 1, 0));
        let error = ExtensionPackageError::CompiledComponentBytesLimit {
            maximum_bytes: MAX_COMPILED_COMPONENT_BYTES,
        };
        assert!(error.to_string().contains("64 MiB"));
    }

    #[test]
    fn local_package_and_active_runtime_limits_fail_closed() {
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        for index in 0..MAX_INSTALLED_LOCAL_PACKAGES {
            let id = format!("org.example.limit-{index}");
            let package = development_package(&declarative(&id));
            let report = manager.install(&mut host, package.path()).unwrap();
            if index < MAX_ACTIVE_LOCAL_PACKAGES {
                assert_eq!(report.activation, PackageActivation::Active);
            } else {
                assert!(matches!(report.activation, PackageActivation::Inactive(_)));
                assert_eq!(
                    host.state(&report.extension),
                    Some(LifecycleState::Disabled)
                );
            }
        }
        assert_eq!(manager.summaries(&host).len(), MAX_INSTALLED_LOCAL_PACKAGES);
        assert_eq!(
            manager.active_local_package_count(&host),
            MAX_ACTIVE_LOCAL_PACKAGES
        );

        let first_active = ExtensionId::parse("org.example.limit-0").unwrap();
        let first_inactive = ExtensionId::parse("org.example.limit-8").unwrap();
        manager.disable(&mut host, &first_active).unwrap();
        assert_eq!(
            manager
                .enable(&mut host, &first_inactive)
                .unwrap()
                .activation,
            PackageActivation::Active,
            "an extension held inactive by the cap must remain reusable"
        );

        let overflow = development_package(&declarative("org.example.limit-overflow"));
        let error = manager.install(&mut host, overflow.path()).unwrap_err();
        assert!(matches!(
            error,
            ExtensionPackageError::InstalledPackageLimit {
                maximum: MAX_INSTALLED_LOCAL_PACKAGES
            }
        ));
        assert!(error.to_string().contains("32"));
    }

    #[test]
    fn installing_same_managed_id_performs_an_upgrade() {
        let first = development_package(&at_version(
            declarative("org.example.install-or-upgrade"),
            "1.0.0",
        ));
        let second = development_package(&at_version(
            declarative("org.example.install-or-upgrade"),
            "2.0.0",
        ));
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        manager.install(&mut host, first.path()).unwrap();

        let report = manager.install(&mut host, second.path()).unwrap();
        assert_eq!(report.version, "2.0.0");
        assert_eq!(report.activation, PackageActivation::Active);
        assert_eq!(manager.summaries(&host)[0].version, "2.0.0");
    }

    #[test]
    fn required_permissions_are_previewed_before_explicit_grant() {
        let mut manifest = declarative("org.example.permissions");
        manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Show document statistics".into(),
            required: true,
        });
        manifest.permissions.push(PermissionRequest {
            permission: Permission::OpenExternalUrl,
            reason: "Optional browser integration".into(),
            required: false,
        });
        let package = development_package(&manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let report = manager.install(&mut host, package.path()).unwrap();
        assert!(matches!(
            report.activation,
            PackageActivation::AwaitingPermissions(ref requests) if requests.len() == 1
        ));
        assert_eq!(
            host.state(&report.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(
            manager
                .grant_permissions_and_activate(&mut host, &report.extension)
                .unwrap()
                .activation,
            PackageActivation::Active
        );
        assert_eq!(
            host.permission_decision(&report.extension, &Permission::OpenExternalUrl),
            PermissionDecision::Undecided,
            "optional permission must never be silently granted"
        );
    }

    #[test]
    fn explicit_permission_denial_keeps_package_disabled() {
        let mut manifest = declarative("org.example.permission-denial");
        manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Show a panel".into(),
            required: true,
        });
        let package = development_package(&manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let report = manager.install(&mut host, package.path()).unwrap();

        manager
            .deny_permissions(&mut host, &report.extension)
            .unwrap();
        assert_eq!(
            host.state(&report.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(
            host.permission_decision(&report.extension, &Permission::AddSidePanel),
            PermissionDecision::Denied
        );
        let retry = manager.enable(&mut host, &report.extension).unwrap();
        assert!(matches!(
            retry.activation,
            PackageActivation::AwaitingPermissions(ref permissions)
                if permissions.len() == 1
                    && permissions[0].permission == Permission::AddSidePanel
        ));
        assert_eq!(
            host.state(&report.extension),
            Some(LifecycleState::Disabled)
        );
    }

    #[test]
    fn individual_permission_changes_preserve_optional_runtime_and_revoke_required_runtime() {
        let mut manifest = declarative("org.example.permission-controls");
        manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Show the extension panel".into(),
            required: true,
        });
        manifest.permissions.push(PermissionRequest {
            permission: Permission::OpenExternalUrl,
            reason: "Open optional documentation".into(),
            required: false,
        });
        let package = development_package(&manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, package.path()).unwrap();
        manager
            .grant_permissions_and_activate(&mut host, &installed.extension)
            .unwrap();

        manager
            .set_permission_decision(
                &mut host,
                &installed.extension,
                &Permission::OpenExternalUrl,
                PermissionDecision::Granted,
            )
            .unwrap();
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active)
        );
        manager
            .set_permission_decision(
                &mut host,
                &installed.extension,
                &Permission::OpenExternalUrl,
                PermissionDecision::Denied,
            )
            .unwrap();
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active),
            "revoking optional authority must not unload an otherwise valid extension"
        );

        manager
            .set_permission_decision(
                &mut host,
                &installed.extension,
                &Permission::AddSidePanel,
                PermissionDecision::Denied,
            )
            .unwrap();
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Disabled),
            "revoking required authority must unload the extension immediately"
        );
        let summary = manager.summaries(&host).pop().unwrap();
        assert!(summary.permissions.iter().any(|(request, decision)| {
            request.permission == Permission::AddSidePanel
                && *decision == PermissionDecision::Denied
        }));
    }

    #[test]
    fn unsigned_upgrade_reprompts_and_commits_only_reapproved_permissions() {
        let id = "org.example.permission-upgrade";
        let mut first_manifest = at_version(declarative(id), "1.0.0");
        first_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Show the existing panel".into(),
            required: true,
        });
        first_manifest.permissions.push(PermissionRequest {
            permission: Permission::OpenExternalUrl,
            reason: "Optional old integration".into(),
            required: false,
        });
        let first = development_package(&first_manifest);
        let mut next_manifest = at_version(declarative(id), "2.0.0");
        next_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Add statistics panel".into(),
            required: true,
        });
        let second = development_package(&next_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let first_report = manager.install(&mut host, first.path()).unwrap();
        manager
            .grant_permissions_and_activate(&mut host, &first_report.extension)
            .unwrap();
        host.set_permission_decision(
            first_report.extension.clone(),
            Permission::OpenExternalUrl,
            PermissionDecision::Granted,
        );

        let preview = manager.install(&mut host, second.path()).unwrap();
        assert!(matches!(
            preview.activation,
            PackageActivation::AwaitingPermissions(ref requests) if requests.len() == 1
        ));
        assert_eq!(
            host.state(&first_report.extension),
            Some(LifecycleState::Active)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
        assert_eq!(
            host.permission_decision(&first_report.extension, &Permission::AddSidePanel),
            PermissionDecision::Granted,
            "pending unsigned candidate must not consume the old grant"
        );
        assert_eq!(
            host.permission_decision(&first_report.extension, &Permission::OpenExternalUrl),
            PermissionDecision::Granted
        );

        let upgraded = manager
            .grant_permissions_and_activate(&mut host, &first_report.extension)
            .unwrap();
        assert_eq!(upgraded.version, "2.0.0");
        assert_eq!(upgraded.activation, PackageActivation::Active);
        assert_eq!(manager.summaries(&host)[0].version, "2.0.0");
        assert_eq!(
            host.permission_decision(&first_report.extension, &Permission::AddSidePanel),
            PermissionDecision::Granted
        );
        assert_eq!(
            host.permission_decision(&first_report.extension, &Permission::OpenExternalUrl),
            PermissionDecision::Undecided,
            "authority discontinuity must clear permissions absent from explicit reapproval"
        );
    }

    #[test]
    fn denying_pending_upgrade_preserves_active_old_version() {
        let id = "org.example.denied-upgrade";
        let mut first_manifest = at_version(declarative(id), "1.0.0");
        first_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Existing panel".into(),
            required: true,
        });
        let first = development_package(&first_manifest);
        let mut next_manifest = at_version(declarative(id), "2.0.0");
        next_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Add panel".into(),
            required: true,
        });
        let second = development_package(&next_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();
        manager
            .grant_permissions_and_activate(&mut host, &installed.extension)
            .unwrap();
        assert!(matches!(
            manager.install(&mut host, second.path()).unwrap().activation,
            PackageActivation::AwaitingPermissions(ref permissions)
                if permissions.len() == 1
        ));

        manager
            .deny_permissions(&mut host, &installed.extension)
            .unwrap();
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
        assert_eq!(
            host.permission_decision(&installed.extension, &Permission::AddSidePanel),
            PermissionDecision::Granted,
            "denying the candidate must leave the old principal untouched"
        );
        assert!(matches!(
            manager.install(&mut host, second.path()).unwrap().activation,
            PackageActivation::AwaitingPermissions(ref permissions)
                if permissions.len() == 1
        ));
        assert_eq!(
            host.permission_decision(&installed.extension, &Permission::AddSidePanel),
            PermissionDecision::Granted
        );
    }

    #[test]
    fn permission_gated_upgrade_preserves_disabled_lifecycle() {
        let id = "org.example.disabled-upgrade";
        let first = development_package(&at_version(declarative(id), "1.0.0"));
        let mut next_manifest = at_version(declarative(id), "2.0.0");
        next_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "New panel".into(),
            required: true,
        });
        let second = development_package(&next_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();
        manager.disable(&mut host, &installed.extension).unwrap();

        assert!(matches!(
            manager
                .install(&mut host, second.path())
                .unwrap()
                .activation,
            PackageActivation::AwaitingPermissions(_)
        ));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");

        let upgraded = manager
            .grant_permissions_and_activate(&mut host, &installed.extension)
            .unwrap();
        assert!(matches!(
            upgraded.activation,
            PackageActivation::Inactive(_)
        ));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(manager.summaries(&host)[0].version, "2.0.0");
    }

    #[test]
    fn suspended_package_upgrade_is_rejected_without_mutation() {
        let id = "org.example.suspended-upgrade";
        let first = development_package(&at_version(declarative(id), "1.0.0"));
        let second = development_package(&at_version(declarative(id), "2.0.0"));
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();
        host.suspend(&installed.extension, "test safe-mode boundary")
            .unwrap();

        let error = manager.install(&mut host, second.path()).unwrap_err();
        assert!(matches!(
            error,
            ExtensionPackageError::UpgradeWhileSuspended(ref extension)
                if extension == &installed.extension
        ));
        assert!(error.to_string().contains("resume"));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Suspended)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
    }

    #[test]
    fn failed_wasm_upgrade_rolls_back_package_adapter_and_active_state() {
        let id = "org.example.rollback";
        let mut first_manifest = at_version(wasm(id), "1.0.0");
        first_manifest.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Existing Wasm panel".into(),
            required: true,
        });
        let first = development_package(&first_manifest);
        let mut broken_manifest = at_version(wasm(id), "2.0.0");
        broken_manifest.permissions.push(PermissionRequest {
            permission: Permission::OpenExternalUrl,
            reason: "Replacement browser authority".into(),
            required: true,
        });
        let broken = development_package_with_component(
            &broken_manifest,
            br#"{"schema_version":1,"kind":"test_fixture"}"#,
            EMPTY_COMPONENT,
        );
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();
        manager
            .grant_permissions_and_activate(&mut host, &installed.extension)
            .unwrap();

        assert!(matches!(
            manager
                .install(&mut host, broken.path())
                .unwrap()
                .activation,
            PackageActivation::AwaitingPermissions(_)
        ));
        assert!(
            manager
                .pending_upgrades
                .get(&installed.extension)
                .and_then(|pending| pending.candidate.adapter.as_ref())
                .is_some(),
            "compiled candidate adapter must be retained across approval"
        );
        let activation = manager
            .grant_permissions_and_activate(&mut host, &installed.extension)
            .unwrap();
        assert_eq!(activation.activation, PackageActivation::Activating);
        drain_host(&mut host);
        let settlements = manager.settle_activating_upgrades(&mut host);
        assert_eq!(settlements.len(), 1);
        let (settled_extension, settlement) = settlements.into_iter().next().unwrap();
        assert_eq!(settled_extension, installed.extension);
        let error = settlement.unwrap_err();
        assert!(matches!(
            error,
            ExtensionPackageError::UpgradeActivationFailed { .. }
        ));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
        assert_eq!(
            host.permission_decision(&installed.extension, &Permission::AddSidePanel),
            PermissionDecision::Granted
        );
        assert_eq!(
            host.permission_decision(&installed.extension, &Permission::OpenExternalUrl),
            PermissionDecision::Undecided
        );
        assert!(manager.packages[&installed.extension].adapter.is_some());
        manager.disable(&mut host, &installed.extension).unwrap();
        assert_eq!(
            manager
                .enable(&mut host, &installed.extension)
                .unwrap()
                .activation,
            PackageActivation::Active,
            "rollback must restore a reusable old adapter"
        );
        assert!(
            error
                .to_string()
                .contains("extension runtime failed during activation")
        );
        assert!(!error.to_string().contains("failed to find export"));
    }

    #[test]
    fn healthy_wasm_upgrade_commits_only_after_worker_settlement() {
        let id = "org.example.async-upgrade";
        let first = development_package(&at_version(wasm(id), "1.0.0"));
        let second = development_package(&at_version(wasm(id), "2.0.0"));
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();

        let provisional = manager.install(&mut host, second.path()).unwrap();
        assert_eq!(provisional.activation, PackageActivation::Activating);
        assert!(manager.settle_activating_upgrades(&mut host).is_empty());

        drain_host(&mut host);
        let settlements = manager.settle_activating_upgrades(&mut host);
        assert_eq!(settlements.len(), 1);
        let (extension, settlement) = settlements.into_iter().next().unwrap();
        assert_eq!(extension, installed.extension);
        let committed = settlement.unwrap();
        assert_eq!(committed.version, "2.0.0");
        assert_eq!(committed.activation, PackageActivation::Active);
        assert_eq!(manager.summaries(&host)[0].version, "2.0.0");
        assert_eq!(host.state(&extension), Some(LifecycleState::Active));
    }

    #[test]
    fn disallowed_license_upgrade_never_touches_active_old_version() {
        let id = "org.example.license-upgrade";
        let first = development_package(&at_version(declarative(id), "1.0.0"));
        let mut forbidden_manifest = at_version(declarative(id), "2.0.0");
        forbidden_manifest.license = "GPL-3.0-only".into();
        let forbidden = development_package(&forbidden_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();

        assert!(matches!(
            manager.install(&mut host, forbidden.path()),
            Err(ExtensionPackageError::Host(HostError::LicenseDenied { .. }))
        ));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
    }

    #[test]
    fn different_publisher_cannot_inherit_extension_identity_or_grants() {
        let id = "org.example.publisher-upgrade";
        let first = development_package(&at_version(declarative(id), "1.0.0"));
        let mut impostor_manifest = at_version(declarative(id), "2.0.0");
        impostor_manifest.publisher.id = "different-publisher".into();
        impostor_manifest.publisher.name = "Different Publisher".into();
        let impostor = development_package(&impostor_manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let installed = manager.install(&mut host, first.path()).unwrap();

        assert!(matches!(
            manager.install(&mut host, impostor.path()),
            Err(ExtensionPackageError::PublisherMismatch(_))
        ));
        assert_eq!(
            host.state(&installed.extension),
            Some(LifecycleState::Active)
        );
        assert_eq!(manager.summaries(&host)[0].version, "1.0.0");
    }

    #[test]
    fn semantic_ui_descriptor_is_validated_beyond_generic_json() {
        let manifest = declarative("org.example.invalid-ui-contract");
        let missing_schema = development_package_with_ui(
            &manifest,
            br#"{"kind":"test_fixture","css":"body { display:none }"}"#,
        );
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();

        assert!(matches!(
            manager.install(&mut host, missing_schema.path()),
            Err(ExtensionPackageError::InvalidDeclarativeUi { .. })
        ));
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn genuine_component_package_runs_through_full_lifecycle() {
        let package = development_package(&wasm("org.example.wasm-lifecycle"));
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        let report = manager.install(&mut host, package.path()).unwrap();
        assert_eq!(report.activation, PackageActivation::Active);

        manager.disable(&mut host, &report.extension).unwrap();
        assert_eq!(
            host.state(&report.extension),
            Some(LifecycleState::Disabled)
        );
        assert_eq!(
            manager
                .enable(&mut host, &report.extension)
                .unwrap()
                .activation,
            PackageActivation::Active
        );
        manager.remove(&mut host, &report.extension).unwrap();
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn checked_in_reference_packages_activate_real_ui_and_wasm_contracts() {
        let workspace = reference_workspace();
        let theme = workspace.join("extensions/reference-theme-pack/package");
        let statistics = workspace.join("extensions/reference-document-statistics/package");
        let mut host = ExtensionHost::new(Default::default());
        host.register_host_capability(
            CapabilityId::parse("key:pdf/document-metadata").unwrap(),
            ExtensionVersion::from_str("0.1.0").unwrap(),
        );
        host.register_host_capability(
            CapabilityId::parse("key:pdf/text").unwrap(),
            ExtensionVersion::from_str("0.1.0").unwrap(),
        );
        let mut manager = InstallableExtensionManager::new().unwrap();

        let theme_report = manager.install(&mut host, &theme).unwrap();
        assert_eq!(theme_report.activation, PackageActivation::Active);
        assert_eq!(
            manager
                .summaries(&host)
                .into_iter()
                .find(|summary| summary.extension == theme_report.extension)
                .unwrap()
                .ui_kind
                .as_deref(),
            Some("theme_preset_pack")
        );

        let statistics_report = manager.install(&mut host, &statistics).unwrap();
        assert!(matches!(
            statistics_report.activation,
            PackageActivation::AwaitingPermissions(ref permissions) if permissions.len() == 3
        ));
        assert_eq!(
            manager
                .grant_permissions_and_activate(&mut host, &statistics_report.extension)
                .unwrap()
                .activation,
            PackageActivation::Active
        );
        host.enqueue_host_event(
            &statistics_report.extension,
            ExtensionEvent::SnapshotChanged {
                snapshot: SnapshotKind::Document,
                value: DataValue::Record(BTreeMap::from([(
                    "statistics".into(),
                    DataValue::Record(BTreeMap::from([
                        ("page-count".into(), DataValue::Integer(12)),
                        ("word-count".into(), DataValue::Integer(3_400)),
                        ("character-count".into(), DataValue::Integer(19_800)),
                    ])),
                )])),
            },
        )
        .unwrap();
        drain_host(&mut host);
        let view = host
            .collect_contributions()
            .views
            .into_iter()
            .find(|view| view.owner == statistics_report.extension)
            .expect("statistics view remains active");
        let UiNodeKind::Column { children } = &view.view.root.kind else {
            panic!("statistics panel must be a column")
        };
        assert_eq!(
            children
                .iter()
                .filter(|node| matches!(node.kind, UiNodeKind::Metric { .. }))
                .count(),
            3
        );
        assert!(matches!(
            view.state
                .get("document")
                .and_then(|value| match value {
                    DataValue::Record(document) => document.get("statistics"),
                    _ => None,
                })
                .and_then(|value| match value {
                    DataValue::Record(statistics) => statistics.get("word-count"),
                    _ => None,
                }),
            Some(DataValue::Integer(3_400))
        ));
    }

    #[test]
    fn collision_with_host_owned_extension_does_not_become_an_upgrade() {
        let package_manifest = declarative("org.example.host-owned");
        let package = development_package(&package_manifest);
        let extension = package_manifest.id.clone();
        let mut host = ExtensionHost::new(Default::default());
        host.install(package_manifest, PackageMetadata::bundled())
            .unwrap();
        host.activate(&extension).unwrap();
        let mut manager = InstallableExtensionManager::new().unwrap();

        assert!(matches!(
            manager.install(&mut host, package.path()),
            Err(ExtensionPackageError::Host(HostError::Registry(_)))
        ));
        assert_eq!(host.state(&extension), Some(LifecycleState::Active));
        assert!(manager.summaries(&host).is_empty());
    }

    #[test]
    fn package_cannot_request_external_native_execution() {
        let manifest = manifest(
            "org.example.native",
            ExtensionEntrypoint::NativeBuiltin {
                adapter: key_extension_api::NativeAdapterId::parse("org.example.native/native")
                    .unwrap(),
                ui: None,
            },
        );
        let package = development_package(&manifest);
        let mut host = ExtensionHost::new(Default::default());
        let mut manager = InstallableExtensionManager::new().unwrap();
        assert!(matches!(
            manager.install(&mut host, package.path()),
            Err(ExtensionPackageError::ExternalNativeEntrypoint(_))
        ));
    }
}
