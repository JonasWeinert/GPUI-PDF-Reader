//! GPUI PDF Reader's composition root for bundled and installed extensions.
//!
//! Feature implementations speak the runtime-neutral extension protocol. The
//! reader shell remains responsible for executing approved effects against
//! GPUI, PDF sessions, storage, and operating-system services.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

#[cfg(feature = "installable-extensions")]
use std::path::Path;

use gpui::MenuItem as GpuiMenuItem;
use gpui_component::{ThemeConfig, ThemeMode};
use key_extension_api::{
    BooleanSource, CURRENT_MANIFEST_SCHEMA, CapabilityId, CapabilityRequest,
    CapabilityRequirements, CapabilityScope, CommandDefinition, CommandId, CompatibleVersion,
    ContributionId, ContributionOrder, ContributionSet, DataValue, EffectId, EffectRequest,
    EffectResult, EventEnvelope, EventSubscription, ExtensionEffect, ExtensionEntrypoint,
    ExtensionError, ExtensionErrorCode, ExtensionEvent, ExtensionId, ExtensionManifest,
    ExtensionVersion, HostCompatibility, LocalId, MenuContribution, MenuItem, MenuItemKind,
    MenuItemOrder, MenuSlotId, NativeAdapterId, Permission, Publisher, SettingsSchema,
    SnapshotKind, StorageRequirements,
};
use key_extension_gpui::{native_menu_slots, resolve_menu_slots};
#[cfg(feature = "installable-extensions")]
use key_extension_host::OwnedCommand;
use key_extension_host::PermissionDecision;
use key_extension_host::{
    ArbitratedEffect, CollectedContributions, ExtensionHost, HostConfig, HostError,
    HostServiceRouter, NativeExtension, NativeExtensionAdapter, NativeUpdate, OwnedView,
    PackageMetadata, ServiceCompletion, ServiceDispatch, TickReport,
};
#[cfg(feature = "installable-extensions")]
use key_extension_package::PackageSourceKind;
use key_pdf_extension_api::{PDF_EXTENSION_API_VERSION, PageIndex, PdfCapability};
use key_sidecar_store::{DocumentKey, JsonExtensionStorage, extension_document_namespace};

use crate::extension_assets::ExtensionAssetStore;
#[cfg(feature = "installable-extensions")]
use crate::extension_registry::{
    ExtensionRegistry, ExtensionRegistryEntry, ExtensionRegistryEntryInput,
    RequiredPermissionDecision, StoredPermissionDecision, default_app_data_root,
};
use crate::pdf_capability_bridge::PdfCapabilityBridge;

#[cfg(feature = "installable-extensions")]
use crate::extension_packages::{
    ExtensionPackageError, InstallableExtensionManager, InstalledPackageSummary,
    PackageInstallPreview, PackageInstallReport,
};

const THEME_EXTENSION: &str = "com.jonasweinert.gpuipdf.theme";
const THEME_ADAPTER: &str = "com.jonasweinert.gpuipdf.theme/native";
const SELECT_THEME_COMMAND: &str = "com.jonasweinert.gpuipdf.theme/select";
const THEME_CAPABILITY: &str = "key:ui/theme";
const THEME_MENU: &str = "com.jonasweinert.gpuipdf.theme/view-theme-menu";

/// Owns all extension lifecycle state for one reader application instance.
/// It is deliberately independent from GPUI's global application state.
pub struct ReaderExtensions {
    host: ExtensionHost,
    services: HostServiceRouter,
    extension_storage: Option<Arc<JsonExtensionStorage>>,
    theme_command: CommandId,
    pdf_capabilities: Arc<PdfCapabilityBridge>,
    extension_assets: Arc<ExtensionAssetStore>,
    startup_error: Option<String>,
    /// Last host snapshot by kind. Equality coalescing keeps high-frequency
    /// viewport input from turning into needless guest invocations.
    last_snapshots: Vec<(SnapshotKind, DataValue)>,
    #[cfg(feature = "installable-extensions")]
    packages: Option<InstallableExtensionManager>,
    #[cfg(feature = "installable-extensions")]
    registry: Option<ExtensionRegistry>,
    #[cfg(feature = "installable-extensions")]
    restoration_failures: BTreeMap<ExtensionId, ExtensionRestoreFailure>,
    #[cfg(feature = "installable-extensions")]
    pending_sources: BTreeMap<ExtensionId, std::path::PathBuf>,
}

#[cfg(feature = "installable-extensions")]
#[derive(Clone)]
struct ExtensionRestoreFailure {
    entry: ExtensionRegistryEntry,
    reason: String,
}

impl ReaderExtensions {
    #[cfg(test)]
    pub fn new(themes: &[ThemeConfig], safe_mode: bool) -> Result<Self, HostError> {
        Self::new_with_assets_internal(
            themes,
            safe_mode,
            Arc::new(ExtensionAssetStore::default()),
            false,
        )
    }

    pub fn new_with_assets(
        themes: &[ThemeConfig],
        safe_mode: bool,
        extension_assets: Arc<ExtensionAssetStore>,
    ) -> Result<Self, HostError> {
        Self::new_with_assets_internal(themes, safe_mode, extension_assets, true)
    }

    fn new_with_assets_internal(
        themes: &[ThemeConfig],
        safe_mode: bool,
        extension_assets: Arc<ExtensionAssetStore>,
        restore_installables: bool,
    ) -> Result<Self, HostError> {
        #[cfg(not(feature = "installable-extensions"))]
        let _ = restore_installables;
        let extension = extension_id();
        let adapter = native_adapter_id();
        let theme_command = theme_command_id();
        let capability = theme_capability_id();

        let config = HostConfig {
            safe_mode,
            ..HostConfig::default()
        };
        let mut host = ExtensionHost::new(config);
        host.register_host_capability(capability.clone(), version("1.0.0"));
        register_pdf_capabilities(&mut host);
        let pdf_capabilities = Arc::new(PdfCapabilityBridge::default());
        let command_for_adapter = theme_command.clone();
        host.register_native_adapter(NativeExtensionAdapter::new(adapter, move || {
            Box::new(ThemeExtension {
                command: command_for_adapter.clone(),
                capability: capability.clone(),
            }) as Box<dyn NativeExtension>
        }));
        host.install(theme_manifest(themes), PackageMetadata::bundled())?;
        host.activate(&extension)?;
        host.register_native_adapter(key_pdf_toc::native_adapter());
        let toc_manifest = key_pdf_toc::manifest();
        let toc_startup_error = (|| -> Result<(), HostError> {
            host.install(toc_manifest.clone(), PackageMetadata::bundled())?;
            for request in &toc_manifest.permissions {
                host.set_permission_decision(
                    toc_manifest.id.clone(),
                    request.permission.clone(),
                    PermissionDecision::Granted,
                );
            }
            host.activate(&toc_manifest.id)?;
            Ok(())
        })()
        .err()
        .map(|error| format!("PDF outline extension is unavailable: {error}"));

        #[cfg(feature = "installable-extensions")]
        let (packages, registry, restoration_failures, mut startup_error) = if restore_installables
        {
            restore_installable_extensions(&mut host, &extension_assets, safe_mode)
        } else {
            match InstallableExtensionManager::new() {
                Ok(packages) => (Some(packages), None, BTreeMap::new(), None),
                Err(error) => (
                    None,
                    None,
                    BTreeMap::new(),
                    Some(format!("Installable extensions are unavailable: {error}")),
                ),
            }
        };
        #[cfg(not(feature = "installable-extensions"))]
        let mut startup_error = None;
        append_startup_error(&mut startup_error, toc_startup_error);
        let (services, extension_storage, storage_error) = extension_services(restore_installables);
        append_startup_error(&mut startup_error, storage_error);

        Ok(Self {
            host,
            services,
            extension_storage,
            theme_command,
            pdf_capabilities,
            extension_assets,
            startup_error,
            last_snapshots: Vec::new(),
            #[cfg(feature = "installable-extensions")]
            packages,
            #[cfg(feature = "installable-extensions")]
            registry,
            #[cfg(feature = "installable-extensions")]
            restoration_failures,
            #[cfg(feature = "installable-extensions")]
            pending_sources: BTreeMap::new(),
        })
    }

    /// Keeps the product usable if a bundled package is corrupt or
    /// incompatible. The failed feature contributes no UI and the reason is
    /// surfaced by the reader instead of aborting application startup.
    #[cfg(test)]
    pub fn disabled(safe_mode: bool, error: impl Into<String>) -> Self {
        Self::disabled_with_assets(safe_mode, error, Arc::new(ExtensionAssetStore::default()))
    }

    pub fn disabled_with_assets(
        safe_mode: bool,
        error: impl Into<String>,
        extension_assets: Arc<ExtensionAssetStore>,
    ) -> Self {
        let config = HostConfig {
            safe_mode,
            ..HostConfig::default()
        };
        let mut host = ExtensionHost::new(config);
        register_pdf_capabilities(&mut host);
        let (services, extension_storage, _) = extension_services(true);
        Self {
            host,
            services,
            extension_storage,
            theme_command: theme_command_id(),
            pdf_capabilities: Arc::new(PdfCapabilityBridge::default()),
            extension_assets,
            startup_error: Some(error.into()),
            last_snapshots: Vec::new(),
            #[cfg(feature = "installable-extensions")]
            packages: None,
            #[cfg(feature = "installable-extensions")]
            registry: None,
            #[cfg(feature = "installable-extensions")]
            restoration_failures: BTreeMap::new(),
            #[cfg(feature = "installable-extensions")]
            pending_sources: BTreeMap::new(),
        }
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    /// Returns the app-owned implementation of the PDF capability boundary.
    pub fn pdf_capabilities(&self) -> Arc<PdfCapabilityBridge> {
        self.pdf_capabilities.clone()
    }

    /// Cancels old-document extension work before the reader increments its
    /// document generation. New snapshots are therefore always republished
    /// and cannot be confused with a cached prior-document value.
    pub fn invalidate_document_scope(&mut self) {
        const DOCUMENT_SNAPSHOTS: [SnapshotKind; 4] = [
            SnapshotKind::Document,
            SnapshotKind::Viewport,
            SnapshotKind::Selection,
            SnapshotKind::Annotation,
        ];
        self.last_snapshots
            .retain(|(kind, _)| !DOCUMENT_SNAPSHOTS.contains(kind));
        self.host.invalidate_document_scope(DOCUMENT_SNAPSHOTS);
        self.services.cancel_all();
        if let Some(storage) = &self.extension_storage {
            let _ = storage.select_document(None);
        }
    }

    pub fn begin_document_storage(&self, path: PathBuf, page_count: usize) -> Result<(), String> {
        let Some(storage) = &self.extension_storage else {
            return Ok(());
        };
        let document =
            DocumentKey::from_pdf(path, page_count).map_err(|error| error.to_string())?;
        storage
            .select_document(Some(extension_document_namespace(&document)))
            .map_err(|error| error.message)
    }

    /// Routes storage and task effects through host-owned semantic services.
    /// OS, GPUI, and PDF effects remain the application shell's responsibility.
    pub fn dispatch_service_effect(&mut self, effect: &ArbitratedEffect) -> ServiceDispatch {
        let storage_quota = match &effect.request.effect {
            ExtensionEffect::StorageGet { area, .. }
            | ExtensionEffect::StoragePut { area, .. }
            | ExtensionEffect::StorageDelete { area, .. } => {
                self.extension_storage_quota(&effect.extension, *area)
            }
            _ => 0,
        };
        self.services.dispatch(effect, storage_quota)
    }

    fn extension_storage_quota(
        &self,
        extension: &ExtensionId,
        area: key_extension_api::StorageArea,
    ) -> u64 {
        let Some(record) = self.host.registry().get(extension) else {
            return 0;
        };
        let requested = match area {
            key_extension_api::StorageArea::Settings => record.manifest().storage.settings_bytes,
            key_extension_api::StorageArea::Document => record.manifest().storage.document_bytes,
            key_extension_api::StorageArea::EphemeralCache => {
                record.manifest().storage.ephemeral_cache_bytes
            }
        };
        let granted = record
            .manifest()
            .permissions
            .iter()
            .filter_map(|request| match &request.permission {
                Permission::Storage(storage)
                    if storage.area == area
                        && self
                            .host
                            .permission_decision(extension, &request.permission)
                            == PermissionDecision::Granted =>
                {
                    Some(storage.quota_bytes)
                }
                _ => None,
            })
            .max()
            .unwrap_or(0);
        requested.min(granted)
    }

    /// Drains a bounded number of background service completions. The reader
    /// returns them through `complete_effect`, preserving normal generation and
    /// stale-result validation.
    pub fn poll_service_completions(&mut self, maximum: usize) -> Vec<ServiceCompletion> {
        self.services.poll_ready(maximum)
    }

    #[must_use]
    pub fn has_pending_service_work(&self) -> bool {
        self.services.active_task_count() > 0
    }

    pub fn contributions(&mut self) -> CollectedContributions {
        self.host.collect_contributions()
    }

    /// Resolve a host-owned menubar slot into native GPUI menu items. The
    /// extension contributes semantic data only; GPUI actions are constructed
    /// by the trusted bridge.
    pub fn native_menu_items(&mut self, slot: &str) -> Vec<GpuiMenuItem> {
        let Ok(slot) = MenuSlotId::parse(slot) else {
            return Vec::new();
        };
        let contributions = self.contributions();
        let resolved = resolve_menu_slots(&contributions);
        native_menu_slots(&resolved)
            .into_iter()
            .find(|candidate| candidate.slot == slot)
            .map_or_else(Vec::new, |candidate| candidate.items)
    }

    /// Queues a command through the bounded host dispatcher and returns only
    /// effects that survived capability, permission, size, and loop checks.
    pub fn invoke_command(
        &mut self,
        command: &CommandId,
        payload: Option<DataValue>,
    ) -> Result<Vec<ArbitratedEffect>, HostError> {
        self.host
            .invoke_command(command, payload.unwrap_or(DataValue::Null))?;
        Ok(self.host.process_tick().effects)
    }

    pub fn invoke_toc_navigation(
        &mut self,
        title: impl Into<String>,
        page: usize,
    ) -> Result<Vec<ArbitratedEffect>, HostError> {
        let page = u32::try_from(page).map_err(|_| {
            HostError::EventRejected("TOC destination page does not fit the PDF API".into())
        })?;
        self.invoke_command(
            &key_pdf_toc::navigate_command_id(),
            Some(key_pdf_toc::TocSelection::new(title, PageIndex(page)).into_payload()),
        )
    }

    pub fn complete_effect(
        &mut self,
        effect: &ArbitratedEffect,
        result: EffectResult,
    ) -> Result<TickReport, HostError> {
        self.host.complete_effect(effect, result)?;
        Ok(self.host.process_tick())
    }

    /// Publish small, host-authored state summaries to active extensions.
    ///
    /// Targets are derived from active, host-owned contributions rather than
    /// guest callbacks. Payloads are constructed by the reader and remain far
    /// below the protocol bounds; full PDF text stays behind capability calls.
    pub fn publish_snapshots(
        &mut self,
        snapshots: impl IntoIterator<Item = (SnapshotKind, DataValue)>,
    ) -> Result<TickReport, HostError> {
        let changed = snapshots
            .into_iter()
            .filter(|(kind, value)| {
                !self
                    .last_snapshots
                    .iter()
                    .any(|(current_kind, current)| current_kind == kind && current == value)
            })
            .collect::<Vec<_>>();
        if changed.is_empty() && self.host.pending_event_count() == 0 {
            return Ok(TickReport::default());
        }

        for (snapshot, value) in &changed {
            for target in self.host.snapshot_targets(*snapshot) {
                self.host.enqueue_host_event(
                    &target,
                    ExtensionEvent::SnapshotChanged {
                        snapshot: *snapshot,
                        value: value.clone(),
                    },
                )?;
            }
            // Cache only after every intended recipient accepted the event.
            // A queue-capacity failure therefore remains retryable.
            self.last_snapshots
                .retain(|(current_kind, _)| current_kind != snapshot);
            self.last_snapshots.push((*snapshot, value.clone()));
        }
        Ok(self.host.process_tick())
    }

    /// Fetch one immutable, validated contribution snapshot for the trusted
    /// GPUI renderer. No extension code runs while GPUI paints the result.
    pub fn contribution_view(&mut self, contribution: &ContributionId) -> Option<OwnedView> {
        self.host
            .collect_contributions()
            .views
            .into_iter()
            .find(|owned| owned.view.id == *contribution)
    }

    pub fn theme_command(&self) -> &CommandId {
        &self.theme_command
    }

    pub fn latest_diagnostic_message(&self) -> Option<String> {
        self.host
            .diagnostics()
            .last()
            .map(|diagnostic| diagnostic.message.clone())
    }

    #[cfg(feature = "installable-extensions")]
    pub fn preview_package(
        &self,
        path: &Path,
    ) -> Result<PackageInstallPreview, ExtensionPackageError> {
        let packages = self.packages.as_ref().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        packages.preview(&self.host, path)
    }

    #[cfg(feature = "installable-extensions")]
    pub fn install_reviewed_package(
        &mut self,
        path: &Path,
        reviewed: &PackageInstallPreview,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        // Decode and validate every referenced image before changing the
        // runtime. The temporary namespace proves the candidate is renderable
        // without replacing assets belonging to an active prior version.
        ExtensionAssetStore::default()
            .replace_extension(
                &reviewed.extension,
                reviewed.referenced_assets().iter().cloned(),
            )
            .map_err(extension_asset_error)?;
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        let report = packages.install_reviewed(&mut self.host, path, reviewed)?;
        self.pending_sources
            .insert(reviewed.extension.clone(), path.to_path_buf());
        let committed = packages
            .content_sha256(&reviewed.extension)
            .is_ok_and(|hash| hash == reviewed.content_sha256_hex());
        if committed {
            let enabled = report.activation == crate::extension_packages::PackageActivation::Active;
            self.persist_managed_package(&reviewed.extension, Some(path), enabled)?;
            self.sync_extension_assets(&reviewed.extension, enabled)?;
            self.pending_sources.remove(&reviewed.extension);
        } else if !reviewed.is_upgrade {
            self.persist_managed_package(&reviewed.extension, Some(path), false)?;
        }
        self.restoration_failures.remove(&reviewed.extension);
        self.last_snapshots.clear();
        Ok(report)
    }

    #[cfg(feature = "installable-extensions")]
    pub fn approve_package(
        &mut self,
        extension: &ExtensionId,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        let report = packages.grant_permissions_and_activate(&mut self.host, extension)?;
        let source = self.pending_sources.remove(extension);
        let enabled = report.activation == crate::extension_packages::PackageActivation::Active;
        self.persist_managed_package(extension, source.as_deref(), enabled)?;
        self.sync_extension_assets(extension, enabled)?;
        self.last_snapshots.clear();
        Ok(report)
    }

    #[cfg(feature = "installable-extensions")]
    pub fn deny_package_permissions(
        &mut self,
        extension: &ExtensionId,
    ) -> Result<(), ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        packages.deny_permissions(&mut self.host, extension)?;
        let source = self.pending_sources.remove(extension);
        if packages.is_managed(extension) && source.is_none() {
            self.persist_managed_package(extension, None, false)?;
        }
        self.sync_extension_assets(extension, false)?;
        Ok(())
    }

    #[cfg(feature = "installable-extensions")]
    pub fn disable_package(
        &mut self,
        extension: &ExtensionId,
    ) -> Result<(), ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        packages.disable(&mut self.host, extension)?;
        self.services.cancel_extension(extension);
        self.persist_managed_package(extension, None, false)?;
        self.sync_extension_assets(extension, false)?;
        self.last_snapshots.clear();
        Ok(())
    }

    #[cfg(feature = "installable-extensions")]
    pub fn enable_package(
        &mut self,
        extension: &ExtensionId,
    ) -> Result<PackageInstallReport, ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        let report = packages.enable(&mut self.host, extension)?;
        let enabled = report.activation == crate::extension_packages::PackageActivation::Active;
        self.persist_managed_package(extension, None, enabled)?;
        self.sync_extension_assets(extension, enabled)?;
        self.last_snapshots.clear();
        Ok(report)
    }

    #[cfg(feature = "installable-extensions")]
    pub fn remove_package(&mut self, extension: &ExtensionId) -> Result<(), ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        if packages.is_managed(extension) {
            packages.remove(&mut self.host, extension)?;
        } else if !self.restoration_failures.contains_key(extension) {
            return Err(ExtensionPackageError::NotManaged(extension.clone()));
        }
        self.services.cancel_extension(extension);
        if let Some(registry) = self.registry.as_mut() {
            registry
                .remove(extension)
                .map_err(extension_registry_error)?;
        }
        self.restoration_failures.remove(extension);
        self.pending_sources.remove(extension);
        self.extension_assets.remove_extension(extension);
        self.last_snapshots.clear();
        Ok(())
    }

    #[cfg(feature = "installable-extensions")]
    pub fn set_package_permission(
        &mut self,
        extension: &ExtensionId,
        permission: &Permission,
        decision: PermissionDecision,
    ) -> Result<(), ExtensionPackageError> {
        let packages = self.packages.as_mut().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        packages.set_permission_decision(&mut self.host, extension, permission, decision)?;
        self.persist_managed_package(
            extension,
            None,
            self.host.state(extension) == Some(key_extension_api::LifecycleState::Active),
        )?;
        if self.host.state(extension) != Some(key_extension_api::LifecycleState::Active) {
            self.services.cancel_extension(extension);
            self.extension_assets.remove_extension(extension);
        }
        self.last_snapshots.clear();
        Ok(())
    }

    #[cfg(feature = "installable-extensions")]
    pub fn installed_packages(&self) -> Vec<InstalledPackageSummary> {
        let mut summaries = self
            .packages
            .as_ref()
            .map_or_else(Vec::new, |packages| packages.summaries(&self.host));
        summaries.extend(self.restoration_failures.values().map(|failure| {
            InstalledPackageSummary {
                extension: failure.entry.extension.clone(),
                name: failure.entry.extension.to_string(),
                version: "Unavailable".into(),
                license: "Unknown".into(),
                source: if failure.entry.source_path.is_dir() {
                    PackageSourceKind::DevelopmentDirectory
                } else {
                    PackageSourceKind::KeyextArchive
                },
                publisher_verified: false,
                state: key_extension_api::LifecycleState::Failed,
                ui_kind: None,
                permissions: Vec::new(),
                restoration_error: Some(failure.reason.clone()),
            }
        }));
        summaries.sort_by(|left, right| left.extension.cmp(&right.extension));
        summaries
    }

    /// Commands from active installable packages are exposed from the trusted
    /// manager panel. GPUI's native menubar is built once during application
    /// startup, so rebuilding it for untrusted runtime packages would be both
    /// platform-fragile and prone to stale action objects.
    #[cfg(feature = "installable-extensions")]
    pub fn installed_commands(&mut self) -> Vec<OwnedCommand> {
        let installed = self
            .packages
            .as_ref()
            .map_or_else(BTreeSet::new, |packages| {
                packages
                    .summaries(&self.host)
                    .into_iter()
                    .map(|summary| summary.extension)
                    .collect()
            });
        self.host
            .collect_contributions()
            .commands
            .into_iter()
            .filter(|command| installed.contains(&command.owner))
            .collect()
    }

    #[cfg(feature = "installable-extensions")]
    fn persist_managed_package(
        &mut self,
        extension: &ExtensionId,
        source: Option<&Path>,
        enabled: bool,
    ) -> Result<(), ExtensionPackageError> {
        let registry = self.registry.as_ref().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the durable extension registry is unavailable".into(),
            ))
        })?;
        let source_path = source
            .map(Path::to_path_buf)
            .or_else(|| {
                registry
                    .get(extension)
                    .map(|entry| entry.source_path.clone())
            })
            .ok_or_else(|| {
                ExtensionPackageError::Host(HostError::EventRejected(format!(
                    "extension {extension} has no durable package source"
                )))
            })?;
        let packages = self.packages.as_ref().ok_or_else(|| {
            ExtensionPackageError::Host(HostError::EventRejected(
                "the installable extension runtime is unavailable".into(),
            ))
        })?;
        let expected_content_sha256 = packages.content_sha256(extension)?;
        let required_permissions = packages
            .permission_decisions(&self.host, extension)?
            .into_iter()
            .filter_map(|(permission, decision)| match decision {
                PermissionDecision::Granted => Some(StoredPermissionDecision {
                    permission,
                    decision: RequiredPermissionDecision::Granted,
                }),
                PermissionDecision::Denied => Some(StoredPermissionDecision {
                    permission,
                    decision: RequiredPermissionDecision::Denied,
                }),
                PermissionDecision::Undecided => None,
            })
            .collect();
        let settings = self.host.extension_settings(extension);
        let registry = self.registry.as_mut().expect("registry was checked");
        registry
            .upsert(ExtensionRegistryEntryInput {
                extension: extension.clone(),
                source_path,
                expected_content_sha256,
                enabled,
                required_permissions,
                settings,
            })
            .map_err(extension_registry_error)?;
        Ok(())
    }

    #[cfg(feature = "installable-extensions")]
    fn sync_extension_assets(
        &self,
        extension: &ExtensionId,
        enabled: bool,
    ) -> Result<(), ExtensionPackageError> {
        if !enabled {
            self.extension_assets.remove_extension(extension);
            return Ok(());
        }
        let assets = self
            .packages
            .as_ref()
            .ok_or_else(|| {
                ExtensionPackageError::Host(HostError::EventRejected(
                    "the installable extension runtime is unavailable".into(),
                ))
            })?
            .referenced_assets(extension)?;
        self.extension_assets
            .replace_extension(extension, assets)
            .map_err(extension_asset_error)
    }
}

#[cfg(feature = "installable-extensions")]
fn extension_registry_error(
    error: crate::extension_registry::ExtensionRegistryError,
) -> ExtensionPackageError {
    ExtensionPackageError::Host(HostError::EventRejected(format!(
        "could not persist extension state: {error}"
    )))
}

#[cfg(feature = "installable-extensions")]
fn extension_asset_error(
    error: crate::extension_assets::ExtensionAssetError,
) -> ExtensionPackageError {
    ExtensionPackageError::Host(HostError::EventRejected(format!(
        "extension assets were rejected: {error}"
    )))
}

#[cfg(feature = "installable-extensions")]
fn restore_installable_extensions(
    host: &mut ExtensionHost,
    assets: &Arc<ExtensionAssetStore>,
    safe_mode: bool,
) -> (
    Option<InstallableExtensionManager>,
    Option<ExtensionRegistry>,
    BTreeMap<ExtensionId, ExtensionRestoreFailure>,
    Option<String>,
) {
    let mut errors = Vec::new();
    let mut failures = BTreeMap::new();
    let mut packages = match InstallableExtensionManager::new() {
        Ok(packages) => packages,
        Err(error) => {
            return (
                None,
                None,
                failures,
                Some(format!("Installable extensions are unavailable: {error}")),
            );
        }
    };
    let registry = match default_app_data_root().and_then(ExtensionRegistry::load_from_root) {
        Ok(registry) => registry,
        Err(error) => {
            return (
                Some(packages),
                None,
                failures,
                Some(format!("Extension registry is unavailable: {error}")),
            );
        }
    };

    for entry in registry.list() {
        let decisions = entry
            .required_permissions
            .iter()
            .map(|stored| {
                (
                    stored.permission.clone(),
                    match stored.decision {
                        RequiredPermissionDecision::Granted => PermissionDecision::Granted,
                        RequiredPermissionDecision::Denied => PermissionDecision::Denied,
                    },
                )
            })
            .collect::<Vec<_>>();
        let restored = packages.restore_registered(
            host,
            &entry.source_path,
            &entry.extension,
            &entry.expected_content_sha256,
            &decisions,
            entry.settings.clone(),
            entry.enabled && !safe_mode,
        );
        let result = restored.and_then(|report| {
            if report.activation == crate::extension_packages::PackageActivation::Active {
                assets
                    .replace_extension(
                        &entry.extension,
                        packages.referenced_assets(&entry.extension)?,
                    )
                    .map_err(|error| {
                        ExtensionPackageError::Host(HostError::EventRejected(format!(
                            "extension assets were rejected: {error}"
                        )))
                    })?;
            }
            Ok(())
        });
        if let Err(error) = result {
            let _ = packages.remove(host, &entry.extension);
            assets.remove_extension(&entry.extension);
            let reason = error.to_string();
            errors.push(format!("{}: {reason}", entry.extension));
            failures.insert(
                entry.extension.clone(),
                ExtensionRestoreFailure { entry, reason },
            );
        }
    }

    let startup_error = (!errors.is_empty()).then(|| {
        format!(
            "Some extensions could not be restored and remain disabled: {}",
            errors.join("; ")
        )
    });
    (Some(packages), Some(registry), failures, startup_error)
}

struct ThemeExtension {
    command: CommandId,
    capability: CapabilityId,
}

impl NativeExtension for ThemeExtension {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        vec![EventSubscription::Commands]
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError> {
        let ExtensionEvent::CommandInvoked { command, payload } = &event.event else {
            return Ok(NativeUpdate::default());
        };
        if command != &self.command {
            return Ok(NativeUpdate::default());
        }
        let DataValue::String(theme) = payload else {
            return Err(ExtensionError {
                code: ExtensionErrorCode::InvalidRequest,
                message: "theme selection requires a string payload".into(),
                retryable: false,
            });
        };
        let id = EffectId::parse(format!("{THEME_EXTENSION}/apply-theme-{}", event.sequence))
            .expect("generated effect ID is canonical");
        Ok(NativeUpdate::with_effects(vec![EffectRequest {
            id,
            cause: event.cause,
            effect: ExtensionEffect::CapabilityCall {
                capability: self.capability.clone(),
                operation: "select".into(),
                input: DataValue::String(theme.clone()),
            },
        }]))
    }
}

fn theme_manifest(themes: &[ThemeConfig]) -> ExtensionManifest {
    let command = theme_command_id();
    let mut theme_items = vec![command_item(
        "follow-system",
        "Follow System (Default)",
        &command,
        "",
    )];
    let mode_items = [
        theme_submenu("Light", "light", ThemeMode::Light, themes, &command),
        theme_submenu("Dark", "dark", ThemeMode::Dark, themes, &command),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !mode_items.is_empty() {
        theme_items.push(separator("mode-separator"));
        theme_items.extend(mode_items);
    }
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension_id(),
        name: "Reader themes".into(),
        version: version("1.0.0"),
        publisher: Publisher {
            id: "jonasweinert".into(),
            name: "Jonas Weinert".into(),
        },
        description: "Bundled theme selection for GPUI PDF Reader".into(),
        license: "MIT".into(),
        compatibility: HostCompatibility {
            extension_api: compatible("^0.1"),
            minimum_host: Some(version("0.1.0")),
            platforms: Vec::new(),
        },
        entrypoint: ExtensionEntrypoint::NativeBuiltin {
            adapter: native_adapter_id(),
            ui: None,
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements {
            required: vec![CapabilityRequest {
                id: theme_capability_id(),
                version: compatible("^1.0"),
                scope: CapabilityScope::Application,
            }],
            optional: Vec::new(),
            provided: Vec::new(),
        },
        permissions: Vec::new(),
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: command.clone(),
                title: "Select theme".into(),
                description: "Apply a bundled reader theme or follow the system appearance".into(),
                category: "Appearance".into(),
            }],
            command_behaviors: Vec::new(),
            menus: vec![MenuContribution {
                id: ContributionId::parse(THEME_MENU).expect("static contribution ID is valid"),
                slot: MenuSlotId::parse("view.appearance").expect("static menu slot is valid"),
                order: ContributionOrder::default(),
                items: vec![submenu("theme-root", "Theme", theme_items)],
            }],
            views: Vec::new(),
        },
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn theme_submenu(
    label: &str,
    id: &str,
    mode: ThemeMode,
    themes: &[ThemeConfig],
    command: &CommandId,
) -> Option<MenuItem> {
    let children = themes
        .iter()
        .filter(|theme| theme.mode == mode)
        .enumerate()
        .map(|(index, theme)| {
            command_item(
                &format!("{id}/theme-{index}"),
                theme.name.as_ref(),
                command,
                theme.name.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    (!children.is_empty()).then(|| submenu(id, label, children))
}

fn command_item(id: &str, label: &str, command: &CommandId, payload: &str) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("generated menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Command {
            label: label.into(),
            command: command.clone(),
            payload: Some(DataValue::String(payload.into())),
            icon: None,
            enabled: BooleanSource::Constant(true),
            checked: None,
        },
    }
}

fn submenu(id: &str, label: &str, children: Vec<MenuItem>) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("static menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Submenu {
            label: label.into(),
            icon: None,
            children,
        },
    }
}

fn separator(id: &str) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("static menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Separator,
    }
}

fn extension_services(
    persistent: bool,
) -> (
    HostServiceRouter,
    Option<Arc<JsonExtensionStorage>>,
    Option<String>,
) {
    if !persistent {
        return (HostServiceRouter::default(), None, None);
    }
    #[cfg(not(feature = "installable-extensions"))]
    {
        return (HostServiceRouter::default(), None, None);
    }
    #[cfg(feature = "installable-extensions")]
    match default_app_data_root() {
        Ok(root) => {
            let storage = Arc::new(JsonExtensionStorage::new(root.join("extension-storage")));
            (
                HostServiceRouter::new(storage.clone(), 16),
                Some(storage),
                None,
            )
        }
        Err(error) => (
            HostServiceRouter::default(),
            None,
            Some(format!(
                "Durable extension storage is unavailable; this session uses memory only: {error}"
            )),
        ),
    }
}

fn append_startup_error(current: &mut Option<String>, error: Option<String>) {
    if let Some(error) = error {
        *current = Some(match current.take() {
            Some(existing) => format!("{existing}\n{error}"),
            None => error,
        });
    }
}

fn extension_id() -> ExtensionId {
    ExtensionId::parse(THEME_EXTENSION).expect("static extension ID is valid")
}

fn native_adapter_id() -> NativeAdapterId {
    NativeAdapterId::parse(THEME_ADAPTER).expect("static adapter ID is valid")
}

fn theme_command_id() -> CommandId {
    CommandId::parse(SELECT_THEME_COMMAND).expect("static command ID is valid")
}

fn theme_capability_id() -> CapabilityId {
    CapabilityId::parse(THEME_CAPABILITY).expect("static capability ID is valid")
}

fn register_pdf_capabilities(host: &mut ExtensionHost) {
    let version = version(PDF_EXTENSION_API_VERSION);
    for capability in PdfCapability::ALL {
        let id = capability.capability_id();
        host.register_host_capability(id.clone(), version.clone());
        host.require_capability_permission(id, capability.required_permission());
    }
    host.require_snapshot_permissions(
        SnapshotKind::Document,
        [
            Permission::ReadDocumentMetadata,
            Permission::ReadDocumentText(key_extension_api::DocumentAccess::ActiveDocument),
        ],
    );
    host.require_snapshot_permissions(SnapshotKind::Viewport, [Permission::ReadDocumentMetadata]);
    host.require_snapshot_permissions(SnapshotKind::Selection, [Permission::ReadSelection]);
    host.require_snapshot_permissions(SnapshotKind::Annotation, [Permission::ReadAnnotations]);
}

fn version(value: &str) -> ExtensionVersion {
    ExtensionVersion::from_str(value).expect("static extension version is valid")
}

fn compatible(value: &str) -> CompatibleVersion {
    CompatibleVersion::from_str(value).expect("static compatibility range is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme(name: &str, mode: ThemeMode) -> ThemeConfig {
        ThemeConfig {
            name: name.to_owned().into(),
            mode,
            ..ThemeConfig::default()
        }
    }

    #[test]
    fn bundled_theme_manifest_is_valid_and_nested_in_appearance_slot() {
        let themes = [
            test_theme("Paper", ThemeMode::Light),
            test_theme("Midnight", ThemeMode::Dark),
        ];
        let manifest = theme_manifest(&themes);
        manifest.validate().expect("bundled manifest must validate");
        assert_eq!(
            manifest.contributions.menus[0].slot.as_str(),
            "view.appearance"
        );
        let MenuItemKind::Submenu { children, .. } = &manifest.contributions.menus[0].items[0].kind
        else {
            panic!("theme root must be a submenu");
        };
        assert_eq!(children.len(), 4);
    }

    #[test]
    fn theme_command_round_trips_through_host_arbitration() {
        let themes = [test_theme("Midnight", ThemeMode::Dark)];
        let mut extensions = ReaderExtensions::new(&themes, false).expect("host starts");
        #[cfg(feature = "installable-extensions")]
        assert!(
            extensions.registry.is_none(),
            "unit tests must never load or execute packages from the user's durable registry"
        );
        let command = extensions.theme_command().clone();
        let effects = extensions
            .invoke_command(&command, Some(DataValue::String("Midnight".into())))
            .expect("command dispatches");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0].request.effect,
            ExtensionEffect::CapabilityCall {
                capability,
                operation,
                input: DataValue::String(name),
            } if capability.as_str() == THEME_CAPABILITY
                && operation == "select"
                && name == "Midnight"
        ));
        extensions
            .complete_effect(&effects[0], Ok(DataValue::Null))
            .expect("effect completion is accepted");
    }

    #[test]
    fn bundled_toc_navigation_enters_the_permissioned_capability_chain() {
        let mut extensions = ReaderExtensions::new(&[], false).expect("host starts");
        let effects = extensions
            .invoke_toc_navigation("Results", 3)
            .expect("bundled TOC command dispatches");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0].request.effect,
            ExtensionEffect::CapabilityCall {
                capability,
                operation,
                input: DataValue::Null,
            } if capability == &PdfCapability::DocumentMetadata.capability_id()
                && operation == "active-document"
        ));
    }

    #[test]
    fn bundled_extension_remains_available_in_safe_mode() {
        let mut extensions = ReaderExtensions::new(&[], true).expect("bundled extension starts");
        assert_eq!(extensions.contributions().menus.len(), 1);
    }

    #[test]
    fn failed_bundled_extension_can_be_isolated_without_losing_the_host() {
        let mut extensions = ReaderExtensions::disabled(true, "broken package");
        assert_eq!(extensions.startup_error(), Some("broken package"));
        assert!(extensions.native_menu_items("view.appearance").is_empty());
    }

    #[test]
    fn all_pdf_contract_capabilities_are_registered_at_the_exact_api_version() {
        let mut extensions = ReaderExtensions::new(&[], false).expect("host starts");
        let probe = ExtensionId::parse("org.example.pdf-contract-probe").expect("valid probe ID");
        let mut manifest = theme_manifest(&[]);
        manifest.id = probe.clone();
        manifest.name = "PDF capability probe".into();
        manifest.entrypoint = ExtensionEntrypoint::Declarative {
            ui: key_extension_api::PackagePath::parse("ui.json").expect("valid package path"),
        };
        manifest.capabilities = CapabilityRequirements {
            required: PdfCapability::ALL
                .into_iter()
                .map(|capability| CapabilityRequest {
                    id: capability.capability_id(),
                    version: compatible(&format!("={PDF_EXTENSION_API_VERSION}")),
                    scope: CapabilityScope::ActiveDocument,
                })
                .collect(),
            optional: Vec::new(),
            provided: Vec::new(),
        };
        manifest.contributions = ContributionSet::default();
        manifest.validate().expect("probe manifest validates");

        extensions
            .host
            .install(manifest, PackageMetadata::bundled())
            .expect("probe installs");
        extensions
            .host
            .activate(&probe)
            .expect("all exact-version capabilities negotiate");
        assert_eq!(
            extensions.host.state(&probe),
            Some(key_extension_api::LifecycleState::Active)
        );
    }
}
