use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
};

use key_extension_api::{
    CapabilityGrant, CapabilityId, CapabilityRequest, CapabilitySnapshot, CauseContext, CauseId,
    CommandBehavior, CommandBehaviorAction, CommandDefinition, CommandId, ContributionId,
    ContributionOrder, ContributionSlot, DataValue, EffectId, EffectRequest, EffectResult,
    EventEnvelope, EventSource, EventSubscription, ExtensionEffect, ExtensionEntrypoint,
    ExtensionError, ExtensionErrorCode, ExtensionEvent, ExtensionId, ExtensionManifest,
    ExtensionVersion, GenerationId, LifecycleState, MenuContribution, MenuItem, MenuItemKind,
    Permission, PermissionRequest, ProvidedCapability, SnapshotKind, StateBinding, StorageArea,
    UiContribution, ValidationLimits,
};

use crate::{
    ActivationContext, DeferredCall, DeferredNativeUpdate, DiagnosticCode, DiagnosticLog,
    DiagnosticSeverity, HostDiagnostic, NativeExtension, NativeExtensionAdapter, NativeUpdate,
    PackageMetadata, PackageRecord, PackageRegistry, RegistryError, UpdateDelivery,
    WasmExtensionAdapter,
};

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub validation_limits: ValidationLimits,
    pub safe_mode: bool,
    pub maximum_queued_events: usize,
    pub maximum_events_per_tick: usize,
    pub maximum_events_per_cause_per_tick: usize,
    pub maximum_effects_per_event: usize,
    pub maximum_dispatch_depth: u16,
    pub violations_before_suspension: u16,
    pub diagnostic_capacity: usize,
    /// Exact SPDX expressions accepted by this product build. Keeping policy
    /// data outside the manifest validator makes a different Key application
    /// free to choose a different store policy.
    pub allowed_license_expressions: BTreeSet<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        let validation_limits = ValidationLimits::default();
        let allowed_license_expressions = [
            "0BSD",
            "Apache-2.0",
            "BSD-2-Clause",
            "BSD-3-Clause",
            "CC0-1.0",
            "ISC",
            "MIT",
            "Unlicense",
            "Zlib",
            "MIT OR Apache-2.0",
            "Apache-2.0 OR MIT",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        Self {
            safe_mode: false,
            maximum_queued_events: 4_096,
            maximum_events_per_tick: validation_limits.maximum_event_batch,
            maximum_events_per_cause_per_tick: 32,
            maximum_effects_per_event: validation_limits.maximum_effect_batch,
            maximum_dispatch_depth: validation_limits.maximum_dispatch_depth,
            violations_before_suspension: 3,
            diagnostic_capacity: 512,
            allowed_license_expressions,
            validation_limits,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PermissionDecision {
    Granted,
    Denied,
    #[default]
    Undecided,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PermissionEntry {
    permission: Permission,
    decision: PermissionDecision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostError {
    Registry(RegistryError),
    NotInstalled(ExtensionId),
    InvalidState {
        extension: ExtensionId,
        state: LifecycleState,
    },
    SafeMode(ExtensionId),
    LicenseDenied {
        extension: ExtensionId,
        license: String,
    },
    DependencyUnavailable {
        extension: ExtensionId,
        dependency: ExtensionId,
        reason: String,
    },
    RequiredCapabilityUnavailable {
        extension: ExtensionId,
        capability: CapabilityId,
    },
    PermissionsRequired {
        extension: ExtensionId,
        permissions: Vec<PermissionRequest>,
    },
    PermissionDenied {
        extension: ExtensionId,
        permission: Permission,
    },
    AdapterUnavailable(ExtensionId),
    UnsupportedEntrypoint(ExtensionId),
    ExtensionFailed {
        extension: ExtensionId,
        error: ExtensionError,
    },
    EventRejected(String),
    EffectNotPending {
        extension: ExtensionId,
        effect: EffectId,
    },
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(formatter),
            Self::NotInstalled(id) => write!(formatter, "extension {id} is not installed"),
            Self::InvalidState { extension, state } => {
                write!(formatter, "extension {extension} is in state {state:?}")
            }
            Self::SafeMode(id) => write!(formatter, "safe mode excludes extension {id}"),
            Self::LicenseDenied { extension, license } => write!(
                formatter,
                "extension {extension} uses disallowed license expression {license}"
            ),
            Self::DependencyUnavailable {
                extension,
                dependency,
                reason,
            } => write!(
                formatter,
                "extension {extension} dependency {dependency} is unavailable: {reason}"
            ),
            Self::RequiredCapabilityUnavailable {
                extension,
                capability,
            } => write!(
                formatter,
                "extension {extension} requires unavailable capability {capability}"
            ),
            Self::PermissionsRequired {
                extension,
                permissions,
            } => write!(
                formatter,
                "extension {extension} awaits {} permission decision(s)",
                permissions.len()
            ),
            Self::PermissionDenied {
                extension,
                permission,
            } => write!(
                formatter,
                "extension {extension} was denied permission {permission:?}"
            ),
            Self::AdapterUnavailable(id) => {
                write!(
                    formatter,
                    "native adapter for extension {id} is unavailable"
                )
            }
            Self::UnsupportedEntrypoint(id) => {
                write!(formatter, "extension {id} requires an unavailable runtime")
            }
            Self::ExtensionFailed { extension, error } => {
                write!(formatter, "extension {extension} failed: {}", error.message)
            }
            Self::EventRejected(message) => formatter.write_str(message),
            Self::EffectNotPending { extension, effect } => {
                write!(formatter, "effect {effect} is not pending for {extension}")
            }
        }
    }
}

impl Error for HostError {}

impl From<RegistryError> for HostError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActivationReport {
    pub extension: ExtensionId,
    pub generation: GenerationId,
    pub capabilities: CapabilitySnapshot,
    pub effects: Vec<ArbitratedEffect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArbitratedEffect {
    pub extension: ExtensionId,
    pub generation: GenerationId,
    pub request: EffectRequest,
    /// Requests in the same dispatch batch that were exactly identical and
    /// share this execution result.
    pub coalesced: Vec<EffectId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickReport {
    pub tick: u64,
    pub processed_events: usize,
    pub deferred_events: usize,
    pub dropped_events: usize,
    pub effects: Vec<ArbitratedEffect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OwnedCommand {
    pub owner: ExtensionId,
    pub command: CommandDefinition,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OwnedMenu {
    pub owner: ExtensionId,
    pub menu: MenuContribution,
    /// Immutable state snapshot resolved in the owning extension's namespace.
    pub state: BTreeMap<String, DataValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OwnedView {
    pub owner: ExtensionId,
    pub view: UiContribution,
    /// Immutable state snapshot resolved by the trusted host renderer.
    pub state: BTreeMap<String, DataValue>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollectedContributions {
    pub commands: Vec<OwnedCommand>,
    pub menus: Vec<OwnedMenu>,
    pub views: Vec<OwnedView>,
}

#[derive(Clone, Debug)]
struct CapabilityOffer {
    capability: ProvidedCapability,
}

struct ActiveRuntime {
    generation: GenerationId,
    capabilities: CapabilitySnapshot,
    subscriptions: Vec<EventSubscription>,
    instance: Option<Box<dyn NativeExtension>>,
    state: BTreeMap<String, DataValue>,
    violations: u16,
}

#[derive(Clone, Debug)]
struct QueuedEvent {
    target: ExtensionId,
    envelope: EventEnvelope,
    root_cause: CauseId,
    eligible_tick: u64,
}

#[derive(Clone, Debug)]
struct PendingEffect {
    generation: GenerationId,
    cause: CauseContext,
    leader: EffectId,
}

/// Single-threaded semantic host. Runtime adapters may perform background work,
/// but must return messages through this bounded dispatcher.
pub struct ExtensionHost {
    config: HostConfig,
    registry: PackageRegistry,
    adapters: BTreeMap<key_extension_api::NativeAdapterId, NativeExtensionAdapter>,
    wasm_adapters: BTreeMap<ExtensionId, WasmExtensionAdapter>,
    runtimes: BTreeMap<ExtensionId, ActiveRuntime>,
    host_capabilities: BTreeMap<CapabilityId, ExtensionVersion>,
    capability_permissions: BTreeMap<CapabilityId, Permission>,
    snapshot_permissions: Vec<(SnapshotKind, Vec<Permission>)>,
    permission_decisions: BTreeMap<ExtensionId, Vec<PermissionEntry>>,
    persisted_settings: BTreeMap<ExtensionId, DataValue>,
    queue: VecDeque<QueuedEvent>,
    pending_effects: BTreeMap<(ExtensionId, EffectId), PendingEffect>,
    diagnostics: DiagnosticLog,
    tick: u64,
    next_cause: u64,
    next_event_sequence: u64,
    next_generation: u64,
}

impl ExtensionHost {
    #[must_use]
    pub fn new(config: HostConfig) -> Self {
        let registry = PackageRegistry::new(config.validation_limits.clone());
        let diagnostics = DiagnosticLog::new(config.diagnostic_capacity);
        Self {
            config,
            registry,
            adapters: BTreeMap::new(),
            wasm_adapters: BTreeMap::new(),
            runtimes: BTreeMap::new(),
            host_capabilities: BTreeMap::new(),
            capability_permissions: BTreeMap::new(),
            snapshot_permissions: Vec::new(),
            permission_decisions: BTreeMap::new(),
            persisted_settings: BTreeMap::new(),
            queue: VecDeque::new(),
            pending_effects: BTreeMap::new(),
            diagnostics,
            tick: 0,
            next_cause: 1,
            next_event_sequence: 1,
            next_generation: 1,
        }
    }

    #[must_use]
    pub const fn config(&self) -> &HostConfig {
        &self.config
    }

    #[must_use]
    pub const fn registry(&self) -> &PackageRegistry {
        &self.registry
    }

    pub fn install(
        &mut self,
        manifest: ExtensionManifest,
        metadata: PackageMetadata,
    ) -> Result<(), HostError> {
        let id = manifest.id.clone();
        if !self
            .config
            .allowed_license_expressions
            .contains(&manifest.license)
        {
            self.diagnostic(
                Some(id.clone()),
                DiagnosticSeverity::Error,
                DiagnosticCode::ValidationFailed,
                format!("license expression {} is not allowed", manifest.license),
            );
            return Err(HostError::LicenseDenied {
                extension: id,
                license: manifest.license,
            });
        }
        match self.registry.install(manifest, metadata) {
            Ok(()) => {
                self.diagnostic(
                    Some(id),
                    DiagnosticSeverity::Info,
                    DiagnosticCode::PackageInstalled,
                    "package validated and installed",
                );
                Ok(())
            }
            Err(error @ RegistryError::InvalidManifest { .. }) => {
                self.diagnostic(
                    Some(id),
                    DiagnosticSeverity::Error,
                    DiagnosticCode::ValidationFailed,
                    error.to_string(),
                );
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn replace(
        &mut self,
        manifest: ExtensionManifest,
        metadata: PackageMetadata,
    ) -> Result<(), HostError> {
        let id = manifest.id.clone();
        self.registry.replace(manifest, metadata)?;
        self.diagnostic(
            Some(id),
            DiagnosticSeverity::Info,
            DiagnosticCode::PackageReplaced,
            "inactive package replaced and revalidated",
        );
        Ok(())
    }

    pub fn register_native_adapter(&mut self, adapter: NativeExtensionAdapter) {
        self.adapters.insert(adapter.id().clone(), adapter);
    }

    /// Associates verified component bytes/runtime state with one package.
    /// Registration alone grants no capability and performs no guest work.
    pub fn register_wasm_adapter(&mut self, adapter: WasmExtensionAdapter) {
        self.wasm_adapters
            .insert(adapter.extension().clone(), adapter);
    }

    pub fn remove_wasm_adapter(&mut self, extension: &ExtensionId) {
        self.wasm_adapters.remove(extension);
    }

    pub fn register_host_capability(&mut self, id: CapabilityId, version: ExtensionVersion) {
        self.host_capabilities.insert(id, version);
        self.reconcile_active_capabilities();
        self.broadcast_capability_changes();
    }

    /// Binds a semantic capability to the user-sensitive authority required
    /// for every invocation. Protocol availability and permission grants are
    /// deliberately separate, so registering a capability never grants data
    /// access by itself.
    pub fn require_capability_permission(&mut self, id: CapabilityId, permission: Permission) {
        self.capability_permissions.insert(id, permission);
    }

    /// Binds a host-authored snapshot to all permissions required before it
    /// may be delivered or retained in extension state. Delivery additionally
    /// requires an explicit runtime subscription.
    pub fn require_snapshot_permissions(
        &mut self,
        snapshot: SnapshotKind,
        permissions: impl IntoIterator<Item = Permission>,
    ) {
        let permissions = permissions.into_iter().collect::<Vec<_>>();
        if let Some((_, current)) = self
            .snapshot_permissions
            .iter_mut()
            .find(|(kind, _)| *kind == snapshot)
        {
            *current = permissions;
        } else {
            self.snapshot_permissions.push((snapshot, permissions));
        }
    }

    pub fn remove_host_capability(&mut self, id: &CapabilityId) {
        self.host_capabilities.remove(id);
        self.capability_permissions.remove(id);
        self.reconcile_active_capabilities();
        self.broadcast_capability_changes();
    }

    pub fn set_permission_decision(
        &mut self,
        extension: ExtensionId,
        permission: Permission,
        decision: PermissionDecision,
    ) {
        let entries = self
            .permission_decisions
            .entry(extension.clone())
            .or_default();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.permission == permission)
        {
            entry.decision = decision;
        } else {
            entries.push(PermissionEntry {
                permission: permission.clone(),
                decision,
            });
        }
        if decision != PermissionDecision::Granted {
            self.purge_snapshot_namespaces_for_permission(&extension, &permission);
        }
    }

    /// Clears all remembered authority for an extension. Installers use this
    /// when an unverified upgrade crosses a trust boundary and must be reviewed
    /// as a new principal rather than inheriting grants by self-declared ID.
    pub fn clear_permission_decisions(&mut self, extension: &ExtensionId) {
        let permissions = self
            .permission_decisions
            .remove(extension)
            .into_iter()
            .flatten()
            .map(|entry| entry.permission)
            .collect::<Vec<_>>();
        for permission in permissions {
            self.purge_snapshot_namespaces_for_permission(extension, &permission);
        }
    }

    #[must_use]
    pub fn permission_decision(
        &self,
        extension: &ExtensionId,
        permission: &Permission,
    ) -> PermissionDecision {
        self.permission_decisions
            .get(extension)
            .and_then(|entries| entries.iter().find(|entry| &entry.permission == permission))
            .map_or(PermissionDecision::Undecided, |entry| entry.decision)
    }

    #[must_use]
    pub fn state(&self, extension: &ExtensionId) -> Option<LifecycleState> {
        self.registry.get(extension).map(PackageRecord::state)
    }

    /// Returns active extension IDs that explicitly subscribe to and are
    /// authorized for one host snapshot. The lightweight ID list avoids
    /// cloning complete contribution trees merely to discover recipients.
    #[must_use]
    pub fn snapshot_targets(&self, snapshot: SnapshotKind) -> Vec<ExtensionId> {
        self.runtimes
            .keys()
            .filter(|extension| self.snapshot_is_authorized(extension, snapshot))
            .cloned()
            .collect()
    }

    /// Number of bounded dispatcher events waiting for a later host tick.
    #[must_use]
    pub fn pending_event_count(&self) -> usize {
        self.queue.len().saturating_add(
            self.runtimes
                .values()
                .filter_map(|runtime| runtime.instance.as_ref())
                .map(|instance| instance.pending_deferred_work())
                .sum::<usize>(),
        )
    }

    /// Establishes a document-state barrier. Host snapshots from the prior
    /// document are removed, queued work is discarded, and outstanding effect
    /// tokens are invalidated so an old completion cannot gain authority over
    /// a newly opened document.
    pub fn invalidate_document_scope(&mut self, snapshots: impl IntoIterator<Item = SnapshotKind>) {
        let namespaces = snapshots
            .into_iter()
            .map(snapshot_namespace)
            .collect::<BTreeSet<_>>();
        self.queue.clear();
        self.pending_effects.clear();
        for runtime in self.runtimes.values_mut() {
            if let Some(instance) = runtime.instance.as_mut() {
                instance.invalidate_document_scope();
            }
            for namespace in &namespaces {
                runtime.state.remove(*namespace);
            }
        }
    }

    /// Returns the current immutable extension-owned UI state snapshot. State
    /// only exists while an extension runtime is active or suspended.
    #[must_use]
    pub fn extension_state(&self, extension: &ExtensionId) -> Option<&BTreeMap<String, DataValue>> {
        self.runtimes.get(extension).map(|runtime| &runtime.state)
    }

    /// Returns the non-sensitive settings snapshot retained across runtime
    /// disable/enable cycles. Sensitive settings never enter extension-visible
    /// state and are therefore never returned here.
    #[must_use]
    pub fn extension_settings(&self, extension: &ExtensionId) -> Option<DataValue> {
        self.runtimes
            .get(extension)
            .and_then(|runtime| runtime.state.get("settings"))
            .cloned()
            .or_else(|| self.persisted_settings.get(extension).cloned())
    }

    /// Restores a bounded settings snapshot after package installation and
    /// before activation. Unknown, sensitive, or wrongly typed keys are
    /// rejected instead of being exposed to the extension runtime.
    pub fn restore_extension_settings(
        &mut self,
        extension: &ExtensionId,
        settings: DataValue,
    ) -> Result<(), HostError> {
        let manifest = self
            .registry
            .get(extension)
            .ok_or_else(|| HostError::NotInstalled(extension.clone()))?
            .manifest();
        let settings =
            normalize_persisted_settings(manifest, settings, &self.config.validation_limits)
                .map_err(HostError::EventRejected)?;
        self.persisted_settings
            .insert(extension.clone(), settings.clone());
        if let Some(runtime) = self.runtimes.get_mut(extension) {
            runtime.state.insert("settings".into(), settings);
        }
        Ok(())
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<HostDiagnostic> {
        self.diagnostics.snapshot()
    }

    pub fn drain_diagnostics(&mut self) -> Vec<HostDiagnostic> {
        self.diagnostics.drain()
    }

    pub fn activate(&mut self, extension: &ExtensionId) -> Result<ActivationReport, HostError> {
        let record = self
            .registry
            .get(extension)
            .ok_or_else(|| HostError::NotInstalled(extension.clone()))?;
        let manifest = record.manifest().clone();
        let metadata = record.metadata().clone();
        let state = record.state();
        if state == LifecycleState::Disabled || state == LifecycleState::Failed {
            self.registry
                .get_mut(extension)
                .expect("record exists")
                .transition(LifecycleState::Validated)?;
        } else if state != LifecycleState::Validated {
            return Err(HostError::InvalidState {
                extension: extension.clone(),
                state,
            });
        }
        if self.config.safe_mode && !metadata.origin.allowed_in_safe_mode() {
            self.diagnostic(
                Some(extension.clone()),
                DiagnosticSeverity::Warning,
                DiagnosticCode::SafeModeSkipped,
                "safe mode permits bundled extensions only",
            );
            return Err(HostError::SafeMode(extension.clone()));
        }
        self.check_dependencies(&manifest)?;
        let capabilities = self.resolve_capabilities(&manifest)?;
        self.check_activation_permissions(&manifest)?;

        let generation = self.fresh_generation();
        let cause = self.root_cause();
        let context = ActivationContext {
            extension: extension.clone(),
            generation,
            cause,
            capabilities: capabilities.clone(),
        };
        let (instance, subscriptions, update) = match &manifest.entrypoint {
            ExtensionEntrypoint::Declarative { .. } => (None, Vec::new(), NativeUpdate::default()),
            ExtensionEntrypoint::NativeBuiltin { adapter, .. } => {
                let Some(factory) = self.adapters.get(adapter).cloned() else {
                    self.fail_activation(
                        extension,
                        DiagnosticCode::AdapterUnavailable,
                        "native adapter is not registered",
                    );
                    return Err(HostError::AdapterUnavailable(extension.clone()));
                };
                let mut instance = factory.instantiate();
                let subscriptions = instance.subscriptions();
                match instance.activate(&context) {
                    Ok(update) => (Some(instance), subscriptions, update),
                    Err(error) => {
                        self.fail_activation(
                            extension,
                            DiagnosticCode::ActivationFailed,
                            error.message.clone(),
                        );
                        return Err(HostError::ExtensionFailed {
                            extension: extension.clone(),
                            error,
                        });
                    }
                }
            }
            entrypoint @ ExtensionEntrypoint::WasmComponent { .. } => {
                let Some(adapter) = self.wasm_adapters.get(extension).cloned() else {
                    self.fail_activation(
                        extension,
                        DiagnosticCode::UnsupportedEntrypoint,
                        "no WebAssembly adapter is installed for this package",
                    );
                    return Err(HostError::UnsupportedEntrypoint(extension.clone()));
                };
                let mut instance = match adapter.instantiate(entrypoint) {
                    Ok(instance) => instance,
                    Err(error) => {
                        self.fail_activation(
                            extension,
                            DiagnosticCode::ActivationFailed,
                            error.message.clone(),
                        );
                        return Err(HostError::ExtensionFailed {
                            extension: extension.clone(),
                            error,
                        });
                    }
                };
                let subscriptions = instance.subscriptions();
                match instance.activate(&context) {
                    Ok(update) => (Some(instance), subscriptions, update),
                    Err(error) => {
                        self.fail_activation(
                            extension,
                            DiagnosticCode::ActivationFailed,
                            error.message.clone(),
                        );
                        return Err(HostError::ExtensionFailed {
                            extension: extension.clone(),
                            error,
                        });
                    }
                }
            }
        };

        self.registry
            .get_mut(extension)
            .expect("record exists")
            .transition(LifecycleState::Active)?;
        let mut state = initial_extension_state(&manifest);
        if let Some(settings) = self.persisted_settings.get(extension).cloned() {
            match normalize_persisted_settings(&manifest, settings, &self.config.validation_limits)
            {
                Ok(settings) => {
                    self.persisted_settings
                        .insert(extension.clone(), settings.clone());
                    state.insert("settings".into(), settings);
                }
                Err(_) => {
                    self.persisted_settings.remove(extension);
                }
            }
        }
        self.runtimes.insert(
            extension.clone(),
            ActiveRuntime {
                generation,
                capabilities: capabilities.clone(),
                subscriptions,
                instance,
                state,
                violations: 0,
            },
        );
        let mut effects = Vec::new();
        self.arbitrate_update(extension, generation, cause, update, &mut effects);
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Info,
            DiagnosticCode::Activated,
            "extension activated",
        );
        self.broadcast_capability_changes();
        Ok(ActivationReport {
            extension: extension.clone(),
            generation,
            capabilities,
            effects,
        })
    }

    fn check_dependencies(&self, manifest: &ExtensionManifest) -> Result<(), HostError> {
        let mut visiting = BTreeSet::new();
        self.check_dependency_cycle(&manifest.id, &mut visiting, &mut BTreeSet::new())?;
        for dependency in &manifest.dependencies {
            let Some(record) = self.registry.get(&dependency.id) else {
                if dependency.optional {
                    continue;
                }
                return Err(HostError::DependencyUnavailable {
                    extension: manifest.id.clone(),
                    dependency: dependency.id.clone(),
                    reason: "not installed".into(),
                });
            };
            if !dependency.version.matches(&record.manifest().version) {
                return Err(HostError::DependencyUnavailable {
                    extension: manifest.id.clone(),
                    dependency: dependency.id.clone(),
                    reason: format!(
                        "installed version {} does not match {}",
                        record.manifest().version,
                        dependency.version
                    ),
                });
            }
            if !dependency.optional && record.state() != LifecycleState::Active {
                return Err(HostError::DependencyUnavailable {
                    extension: manifest.id.clone(),
                    dependency: dependency.id.clone(),
                    reason: "required dependency is not active".into(),
                });
            }
        }
        Ok(())
    }

    fn check_dependency_cycle(
        &self,
        extension: &ExtensionId,
        visiting: &mut BTreeSet<ExtensionId>,
        visited: &mut BTreeSet<ExtensionId>,
    ) -> Result<(), HostError> {
        if visited.contains(extension) {
            return Ok(());
        }
        if !visiting.insert(extension.clone()) {
            return Err(HostError::DependencyUnavailable {
                extension: extension.clone(),
                dependency: extension.clone(),
                reason: "dependency cycle".into(),
            });
        }
        if let Some(record) = self.registry.get(extension) {
            for dependency in &record.manifest().dependencies {
                if self.registry.get(&dependency.id).is_some() {
                    self.check_dependency_cycle(&dependency.id, visiting, visited)?;
                }
            }
        }
        visiting.remove(extension);
        visited.insert(extension.clone());
        Ok(())
    }

    fn capability_offers(&self) -> Vec<CapabilityOffer> {
        let mut offers = self
            .host_capabilities
            .iter()
            .map(|(id, version)| CapabilityOffer {
                capability: ProvidedCapability {
                    id: id.clone(),
                    version: version.clone(),
                },
            })
            .collect::<Vec<_>>();
        for (provider, runtime) in &self.runtimes {
            if self.state(provider) != Some(LifecycleState::Active) {
                continue;
            }
            let _ = runtime;
            if let Some(record) = self.registry.get(provider) {
                offers.extend(
                    record
                        .manifest()
                        .capabilities
                        .provided
                        .iter()
                        .cloned()
                        .map(|capability| CapabilityOffer { capability }),
                );
            }
        }
        offers
    }

    fn matching_offer<'a>(
        offers: &'a [CapabilityOffer],
        request: &CapabilityRequest,
    ) -> Option<&'a CapabilityOffer> {
        offers.iter().find(|offer| {
            offer.capability.id == request.id && request.version.matches(&offer.capability.version)
        })
    }

    fn resolve_capabilities(
        &self,
        manifest: &ExtensionManifest,
    ) -> Result<CapabilitySnapshot, HostError> {
        let offers = self.capability_offers();
        let mut granted = Vec::new();
        for request in &manifest.capabilities.required {
            if Self::matching_offer(&offers, request).is_none() {
                return Err(HostError::RequiredCapabilityUnavailable {
                    extension: manifest.id.clone(),
                    capability: request.id.clone(),
                });
            }
            granted.push(CapabilityGrant {
                extension: manifest.id.clone(),
                capability: request.id.clone(),
                scope: request.scope.clone(),
            });
        }
        let mut missing_optional = Vec::new();
        for request in &manifest.capabilities.optional {
            if Self::matching_offer(&offers, request).is_some() {
                granted.push(CapabilityGrant {
                    extension: manifest.id.clone(),
                    capability: request.id.clone(),
                    scope: request.scope.clone(),
                });
            } else {
                missing_optional.push(request.id.clone());
            }
        }
        Ok(CapabilitySnapshot {
            granted,
            missing_optional,
        })
    }

    fn check_activation_permissions(
        &mut self,
        manifest: &ExtensionManifest,
    ) -> Result<(), HostError> {
        let mut undecided = Vec::new();
        for request in &manifest.permissions {
            match self.permission_decision(&manifest.id, &request.permission) {
                PermissionDecision::Denied if request.required => {
                    self.diagnostic(
                        Some(manifest.id.clone()),
                        DiagnosticSeverity::Warning,
                        DiagnosticCode::PermissionDenied,
                        format!("required permission denied: {:?}", request.permission),
                    );
                    return Err(HostError::PermissionDenied {
                        extension: manifest.id.clone(),
                        permission: request.permission.clone(),
                    });
                }
                PermissionDecision::Undecided if request.required => {
                    undecided.push(request.clone());
                }
                PermissionDecision::Granted
                | PermissionDecision::Denied
                | PermissionDecision::Undecided => {}
            }
        }
        if undecided.is_empty() {
            Ok(())
        } else {
            self.diagnostic(
                Some(manifest.id.clone()),
                DiagnosticSeverity::Info,
                DiagnosticCode::PermissionRequired,
                format!("{} permission decision(s) required", undecided.len()),
            );
            Err(HostError::PermissionsRequired {
                extension: manifest.id.clone(),
                permissions: undecided,
            })
        }
    }

    fn fail_activation(
        &mut self,
        extension: &ExtensionId,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        if let Some(record) = self.registry.get_mut(extension) {
            let _ = record.transition(LifecycleState::Failed);
        }
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Error,
            code,
            message,
        );
    }

    fn fresh_generation(&mut self) -> GenerationId {
        let generation = GenerationId(self.next_generation);
        self.next_generation = self.next_generation.saturating_add(1);
        generation
    }

    fn root_cause(&mut self) -> CauseContext {
        let id = CauseId::new(0, self.next_cause);
        self.next_cause = self.next_cause.saturating_add(1);
        CauseContext {
            id,
            parent: None,
            depth: 0,
        }
    }

    fn child_cause(&mut self, parent: CauseContext) -> Option<CauseContext> {
        if parent.depth >= self.config.maximum_dispatch_depth {
            return None;
        }
        let id = CauseId::new(0, self.next_cause);
        self.next_cause = self.next_cause.saturating_add(1);
        Some(CauseContext {
            id,
            parent: Some(parent.id),
            depth: parent.depth + 1,
        })
    }

    pub fn enqueue_host_event(
        &mut self,
        target: &ExtensionId,
        event: ExtensionEvent,
    ) -> Result<CauseContext, HostError> {
        let cause = self.root_cause();
        self.enqueue_event_internal(target.clone(), EventSource::Host, event, cause, cause.id)?;
        Ok(cause)
    }

    pub fn enqueue_extension_event(
        &mut self,
        source: &ExtensionId,
        target: &ExtensionId,
        parent: CauseContext,
        event: ExtensionEvent,
    ) -> Result<CauseContext, HostError> {
        if self.state(source) != Some(LifecycleState::Active) {
            return Err(HostError::EventRejected(format!(
                "event source {source} is not active"
            )));
        }
        let Some(cause) = self.child_cause(parent) else {
            self.record_violation(
                target,
                DiagnosticCode::DispatchDepthExceeded,
                "extension event exceeded maximum dispatch depth",
            );
            return Err(HostError::EventRejected(
                "maximum dispatch depth exceeded".into(),
            ));
        };
        self.enqueue_event_internal(
            target.clone(),
            EventSource::Extension(source.clone()),
            event,
            cause,
            parent.id,
        )?;
        Ok(cause)
    }

    fn enqueue_event_internal(
        &mut self,
        target: ExtensionId,
        source: EventSource,
        event: ExtensionEvent,
        cause: CauseContext,
        root_cause: CauseId,
    ) -> Result<(), HostError> {
        if self.registry.get(&target).is_none() {
            return Err(HostError::NotInstalled(target));
        }
        if cause.depth > self.config.maximum_dispatch_depth {
            self.record_violation(
                &target,
                DiagnosticCode::DispatchDepthExceeded,
                "event exceeded maximum dispatch depth",
            );
            return Err(HostError::EventRejected(
                "maximum dispatch depth exceeded".into(),
            ));
        }
        if self.queue.len() >= self.config.maximum_queued_events {
            self.record_violation(
                &target,
                DiagnosticCode::EventDropped,
                "event queue capacity exceeded",
            );
            return Err(HostError::EventRejected("event queue is full".into()));
        }
        let sequence = self.next_event_sequence;
        self.next_event_sequence = self.next_event_sequence.saturating_add(1);
        self.queue.push_back(QueuedEvent {
            target,
            envelope: EventEnvelope {
                cause,
                source,
                sequence,
                event,
            },
            root_cause,
            eligible_tick: self.tick.saturating_add(1),
        });
        Ok(())
    }

    pub fn invoke_command(
        &mut self,
        command: &CommandId,
        payload: DataValue,
    ) -> Result<CauseContext, HostError> {
        let owner = command.owner();
        let declared = self.registry.get(&owner).is_some_and(|record| {
            record
                .manifest()
                .contributions
                .commands
                .iter()
                .any(|definition| definition.id == *command)
        });
        if !declared {
            return Err(HostError::EventRejected(format!(
                "command {command} is not declared"
            )));
        }
        if !value_is_bounded(&payload, &self.config.validation_limits) {
            return Err(HostError::EventRejected(
                "command payload exceeds host limits or contains invalid state keys".into(),
            ));
        }
        self.enqueue_host_event(
            &owner,
            ExtensionEvent::CommandInvoked {
                command: command.clone(),
                payload,
            },
        )
    }

    pub fn process_tick(&mut self) -> TickReport {
        self.tick = self.tick.saturating_add(1);
        let mut report = TickReport {
            tick: self.tick,
            ..TickReport::default()
        };

        // Background adapters only exchange owned protocol values with this
        // single-threaded host. Pull a bounded batch before dispatching new
        // work, then run the exact same state/effect arbitration used by an
        // inline adapter. No guest callback occurs in this method.
        let completed = self.drain_deferred_adapter_updates();
        let dispatch_budget = self
            .config
            .maximum_events_per_tick
            .saturating_sub(completed.len());
        for (extension, update) in completed {
            self.apply_deferred_adapter_update(&extension, update, &mut report);
        }

        let mut selected = Vec::new();
        let initial_len = self.queue.len();
        for _ in 0..initial_len {
            let Some(event) = self.queue.pop_front() else {
                break;
            };
            if event.eligible_tick <= self.tick && selected.len() < dispatch_budget {
                selected.push(event);
            } else {
                self.queue.push_back(event);
            }
        }
        if self
            .queue
            .iter()
            .any(|event| event.eligible_tick <= self.tick)
        {
            self.diagnostic(
                None,
                DiagnosticSeverity::Warning,
                DiagnosticCode::EventBatchLimited,
                "event batch limit reached; remaining events deferred",
            );
        }
        let mut cause_counts = BTreeMap::<(ExtensionId, CauseId), usize>::new();
        for queued in selected {
            let key = (queued.target.clone(), queued.root_cause);
            let count = cause_counts.entry(key).or_default();
            *count += 1;
            if *count > self.config.maximum_events_per_cause_per_tick {
                report.dropped_events += 1;
                self.record_violation(
                    &queued.target,
                    DiagnosticCode::FeedbackLoopDetected,
                    "too many events from one cause in a single tick",
                );
                continue;
            }
            let state = self.state(&queued.target);
            if state != Some(LifecycleState::Active) {
                report.dropped_events += 1;
                self.diagnostic(
                    Some(queued.target),
                    DiagnosticSeverity::Info,
                    DiagnosticCode::EventDropped,
                    format!("event ignored while extension is {state:?}"),
                );
                continue;
            }
            if let ExtensionEvent::SnapshotChanged { snapshot, .. } = &queued.envelope.event
                && !self.snapshot_is_authorized(&queued.target, *snapshot)
            {
                report.dropped_events += 1;
                self.diagnostic(
                    Some(queued.target),
                    DiagnosticSeverity::Info,
                    DiagnosticCode::EventDropped,
                    format!(
                        "{snapshot:?} snapshot ignored without both an explicit subscription and its required permissions"
                    ),
                );
                continue;
            }
            let command_behaviors =
                self.command_behaviors_for_event(&queued.target, &queued.envelope);
            let deliver_to_adapter = self.runtimes.get(&queued.target).is_some_and(|runtime| {
                event_is_mandatory(&queued.envelope.event)
                    || runtime.subscriptions.iter().any(|subscription| {
                        subscription_matches(subscription, &queued.envelope.event)
                    })
            });
            let snapshot_state = snapshot_state_update(&queued.envelope);
            if !deliver_to_adapter && command_behaviors.is_empty() {
                if let Some((namespace, value)) = snapshot_state {
                    self.replace_snapshot_state(&queued.target, namespace, value);
                }
                continue;
            }
            let generation = self
                .runtimes
                .get(&queued.target)
                .expect("active extension has runtime")
                .generation;
            let (outcome, delivery) = if deliver_to_adapter {
                let runtime = self
                    .runtimes
                    .get_mut(&queued.target)
                    .expect("active extension has runtime");
                match runtime.instance.as_mut() {
                    Some(instance) => {
                        let delivery = instance.update_delivery();
                        (Some(instance.handle_event(&queued.envelope)), delivery)
                    }
                    None => (None, UpdateDelivery::Immediate),
                }
            } else {
                (None, UpdateDelivery::Immediate)
            };
            report.processed_events += 1;
            if delivery == UpdateDelivery::Deferred && matches!(outcome, Some(Ok(_))) {
                // Command behavior and snapshot replacement must wait for the
                // guest update so their deterministic merge order stays
                // identical to the inline path.
                continue;
            }
            let adapter_update = match outcome {
                None => NativeUpdate::default(),
                Some(Ok(update)) => update,
                Some(Err(error)) => {
                    self.diagnostic(
                        Some(queued.target.clone()),
                        DiagnosticSeverity::Error,
                        DiagnosticCode::ExtensionFault,
                        error.message,
                    );
                    self.record_violation(
                        &queued.target,
                        DiagnosticCode::ExtensionFault,
                        "extension event handler returned an error",
                    );
                    NativeUpdate::default()
                }
            };
            let update = self.merge_command_behaviors(
                &queued.target,
                &queued.envelope,
                &command_behaviors,
                adapter_update,
            );
            self.arbitrate_update_internal(
                &queued.target,
                generation,
                queued.envelope.cause,
                update,
                &mut report.effects,
                matches!(queued.envelope.event, ExtensionEvent::CommandInvoked { .. }),
            );
            if let Some((namespace, value)) = snapshot_state {
                self.replace_snapshot_state(&queued.target, namespace, value);
            }
        }
        report.deferred_events = self.pending_event_count();
        report
    }

    fn drain_deferred_adapter_updates(&mut self) -> Vec<(ExtensionId, DeferredNativeUpdate)> {
        let mut remaining = self.config.maximum_events_per_tick;
        let mut completed = Vec::new();
        if remaining == 0 {
            return completed;
        }
        for (extension, runtime) in &mut self.runtimes {
            let Some(instance) = runtime.instance.as_mut() else {
                continue;
            };
            if instance.update_delivery() != UpdateDelivery::Deferred {
                continue;
            }
            let updates = instance.drain_deferred_updates(remaining);
            remaining = remaining.saturating_sub(updates.len());
            completed.extend(
                updates
                    .into_iter()
                    .map(|update| (extension.clone(), update)),
            );
            if remaining == 0 {
                break;
            }
        }
        completed
    }

    fn apply_deferred_adapter_update(
        &mut self,
        extension: &ExtensionId,
        completed: DeferredNativeUpdate,
        report: &mut TickReport,
    ) {
        let Some(runtime) = self.runtimes.get(extension) else {
            return;
        };
        if runtime.generation != completed.generation {
            return;
        }
        let generation = runtime.generation;
        match completed.call {
            DeferredCall::Activation { cause } | DeferredCall::Resume { cause } => {
                match completed.result {
                    Ok(update) if self.state(extension) == Some(LifecycleState::Active) => {
                        self.arbitrate_update(
                            extension,
                            generation,
                            cause,
                            update,
                            &mut report.effects,
                        );
                    }
                    Err(error)
                        if matches!(
                            self.state(extension),
                            Some(LifecycleState::Active | LifecycleState::Suspended)
                        ) =>
                    {
                        self.fail_deferred_runtime(extension, error);
                        report.dropped_events = report.dropped_events.saturating_add(1);
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            DeferredCall::Event(envelope) => {
                if self.state(extension) != Some(LifecycleState::Active) {
                    return;
                }
                let command_behaviors = self.command_behaviors_for_event(extension, &envelope);
                let snapshot_state = snapshot_state_update(&envelope);
                let adapter_update = match completed.result {
                    Ok(update) => update,
                    Err(error) => {
                        self.diagnostic(
                            Some(extension.clone()),
                            DiagnosticSeverity::Error,
                            DiagnosticCode::ExtensionFault,
                            error.message,
                        );
                        self.record_violation(
                            extension,
                            DiagnosticCode::ExtensionFault,
                            "deferred extension event handler returned an error",
                        );
                        NativeUpdate::default()
                    }
                };
                // A repeated violation may suspend and remove authority while
                // the result is being diagnosed. Never apply it afterward.
                if self.state(extension) != Some(LifecycleState::Active) {
                    return;
                }
                let update = self.merge_command_behaviors(
                    extension,
                    &envelope,
                    &command_behaviors,
                    adapter_update,
                );
                self.arbitrate_update_internal(
                    extension,
                    generation,
                    envelope.cause,
                    update,
                    &mut report.effects,
                    matches!(envelope.event, ExtensionEvent::CommandInvoked { .. }),
                );
                if let Some((namespace, value)) = snapshot_state {
                    self.replace_snapshot_state(extension, namespace, value);
                }
            }
        }
    }

    fn fail_deferred_runtime(&mut self, extension: &ExtensionId, error: ExtensionError) {
        self.queue.retain(|event| event.target != *extension);
        self.pending_effects
            .retain(|(owner, _), _| owner != extension);
        if let Some(mut runtime) = self.runtimes.remove(extension)
            && let Some(instance) = runtime.instance.as_mut()
        {
            instance.unload();
        }
        if let Some(record) = self.registry.get_mut(extension) {
            let _ = record.transition(LifecycleState::Failed);
        }
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Error,
            DiagnosticCode::ExtensionFault,
            error.message,
        );
        self.reconcile_active_capabilities();
        self.broadcast_capability_changes();
    }

    fn command_behaviors_for_event(
        &self,
        extension: &ExtensionId,
        envelope: &EventEnvelope,
    ) -> Vec<CommandBehavior> {
        if envelope.source != EventSource::Host {
            return Vec::new();
        }
        let ExtensionEvent::CommandInvoked { command, .. } = &envelope.event else {
            return Vec::new();
        };
        self.registry
            .get(extension)
            .map(|record| {
                record
                    .manifest()
                    .contributions
                    .command_behaviors
                    .iter()
                    .filter(|behavior| behavior.command == *command)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn replace_snapshot_state(
        &mut self,
        extension: &ExtensionId,
        namespace: &str,
        value: DataValue,
    ) {
        if !value_is_bounded(&value, &self.config.validation_limits) {
            self.record_violation(
                extension,
                DiagnosticCode::EffectRejected,
                "snapshot state exceeded host bounds",
            );
            return;
        }
        let Some(runtime) = self.runtimes.get_mut(extension) else {
            return;
        };
        let previous = runtime.state.insert(namespace.to_owned(), value);
        if !state_is_bounded(&runtime.state, &self.config.validation_limits) {
            match previous {
                Some(previous) => {
                    runtime.state.insert(namespace.to_owned(), previous);
                }
                None => {
                    runtime.state.remove(namespace);
                }
            }
            self.record_violation(
                extension,
                DiagnosticCode::EffectRejected,
                "snapshot state exceeded host bounds",
            );
        }
    }

    /// Merge host-declared no-code behavior with one adapter update. Manifest
    /// behavior is applied in declaration order after adapter state, so a
    /// control's command payload deterministically wins at its bound path.
    /// Generated effects precede adapter effects and pass through the same
    /// bounded arbitration and permission checks.
    fn merge_command_behaviors(
        &mut self,
        extension: &ExtensionId,
        envelope: &EventEnvelope,
        behaviors: &[CommandBehavior],
        mut update: NativeUpdate,
    ) -> NativeUpdate {
        let ExtensionEvent::CommandInvoked { payload, .. } = &envelope.event else {
            return update;
        };
        let has_state_behavior = behaviors
            .iter()
            .any(|behavior| matches!(behavior.action, CommandBehaviorAction::SetState { .. }));
        if has_state_behavior {
            let current = self
                .runtimes
                .get(extension)
                .map(|runtime| runtime.state.clone())
                .unwrap_or_default();
            let mut state = match update.state.take() {
                Some(mut state) if state_is_bounded(&state, &self.config.validation_limits) => {
                    // Host snapshots and settings are reserved. Guest/native
                    // state may replace its own snapshot but cannot forge or
                    // discard these host-owned channels.
                    preserve_host_state_namespaces(&current, &mut state);
                    state
                }
                Some(_) => {
                    self.record_violation(
                        extension,
                        DiagnosticCode::EffectRejected,
                        "extension state update exceeded host limits or used an invalid key",
                    );
                    current
                }
                None => current,
            };
            for behavior in behaviors {
                if let CommandBehaviorAction::SetState { binding } = &behavior.action {
                    let setting_accepts = if binding
                        .0
                        .first()
                        .is_some_and(|segment| segment == "settings")
                    {
                        let key = binding
                            .0
                            .iter()
                            .skip(1)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(".");
                        self.registry
                            .get(extension)
                            .and_then(|record| {
                                record
                                    .manifest()
                                    .settings
                                    .fields
                                    .iter()
                                    .find(|setting| setting.key == key && !setting.sensitive)
                            })
                            .is_some_and(|setting| setting.value_type.accepts(payload))
                    } else {
                        true
                    };
                    if setting_accepts {
                        set_state_binding(&mut state, binding, payload.clone());
                    } else {
                        self.record_violation(
                            extension,
                            DiagnosticCode::EffectRejected,
                            "command payload does not satisfy the bound setting type",
                        );
                    }
                }
            }
            update.state = Some(state);
        } else if let Some(state) = update.state.as_mut() {
            // A command adapter update without SetState still cannot replace
            // or discard host-owned state namespaces.
            let current = self
                .runtimes
                .get(extension)
                .map(|runtime| runtime.state.clone())
                .unwrap_or_default();
            preserve_host_state_namespaces(&current, state);
        }

        let mut behavior_effects = Vec::new();
        for (index, behavior) in behaviors.iter().enumerate() {
            let effect = match &behavior.action {
                CommandBehaviorAction::SetState { .. } => continue,
                CommandBehaviorAction::OpenContribution { contribution } => {
                    ExtensionEffect::OpenContribution {
                        contribution: contribution.clone(),
                    }
                }
                CommandBehaviorAction::CloseContribution { contribution } => {
                    ExtensionEffect::CloseContribution {
                        contribution: contribution.clone(),
                    }
                }
            };
            let Ok(id) = EffectId::parse(format!(
                "{extension}/behavior-{}-{index}",
                envelope.sequence
            )) else {
                self.record_violation(
                    extension,
                    DiagnosticCode::EffectRejected,
                    "host could not allocate a bounded declarative effect ID",
                );
                continue;
            };
            behavior_effects.push(EffectRequest {
                id,
                cause: envelope.cause,
                effect,
            });
        }
        behavior_effects.append(&mut update.effects);
        update.effects = behavior_effects;
        update
    }

    fn arbitrate_update(
        &mut self,
        extension: &ExtensionId,
        generation: GenerationId,
        cause: CauseContext,
        update: NativeUpdate,
        accepted: &mut Vec<ArbitratedEffect>,
    ) {
        self.arbitrate_update_internal(extension, generation, cause, update, accepted, false);
    }

    fn arbitrate_update_internal(
        &mut self,
        extension: &ExtensionId,
        generation: GenerationId,
        cause: CauseContext,
        mut update: NativeUpdate,
        accepted: &mut Vec<ArbitratedEffect>,
        allow_host_settings_update: bool,
    ) {
        if let Some(state) = update.state.take() {
            let mut state = state;
            if !allow_host_settings_update {
                let current = self
                    .runtimes
                    .get(extension)
                    .map(|runtime| runtime.state.clone())
                    .unwrap_or_default();
                preserve_host_state_namespaces(&current, &mut state);
            }
            if state_is_bounded(&state, &self.config.validation_limits) {
                let settings = if let Some(runtime) = self.runtimes.get_mut(extension)
                    && runtime.generation == generation
                {
                    runtime.state = state;
                    runtime.state.get("settings").cloned()
                } else {
                    None
                };
                if let Some(settings) = settings {
                    self.persisted_settings.insert(extension.clone(), settings);
                }
            } else {
                self.record_violation(
                    extension,
                    DiagnosticCode::EffectRejected,
                    "extension state update exceeded host limits or used an invalid key",
                );
            }
        }
        if update.effects.len() > self.config.maximum_effects_per_event {
            self.record_violation(
                extension,
                DiagnosticCode::EffectRejected,
                format!(
                    "effect batch contained {} requests; limit is {}",
                    update.effects.len(),
                    self.config.maximum_effects_per_event
                ),
            );
        }
        for request in update
            .effects
            .into_iter()
            .take(self.config.maximum_effects_per_event)
        {
            if let Err(error) = self.validate_effect(extension, generation, cause, &request) {
                self.diagnostic(
                    Some(extension.clone()),
                    DiagnosticSeverity::Warning,
                    DiagnosticCode::EffectRejected,
                    error.message.clone(),
                );
                self.queue_effect_result(extension, generation, &request, Err(error));
                continue;
            }
            if let Some(existing) = accepted.iter_mut().find(|effect| {
                effect.extension == *extension
                    && effect.generation == generation
                    && effect.request.effect == request.effect
            }) {
                existing.coalesced.push(request.id.clone());
                self.pending_effects.insert(
                    (extension.clone(), request.id.clone()),
                    PendingEffect {
                        generation,
                        cause: request.cause,
                        leader: existing.request.id.clone(),
                    },
                );
                self.diagnostic(
                    Some(extension.clone()),
                    DiagnosticSeverity::Info,
                    DiagnosticCode::EffectCoalesced,
                    "identical effect in one dispatch batch was coalesced",
                );
                continue;
            }
            self.pending_effects.insert(
                (extension.clone(), request.id.clone()),
                PendingEffect {
                    generation,
                    cause: request.cause,
                    leader: request.id.clone(),
                },
            );
            accepted.push(ArbitratedEffect {
                extension: extension.clone(),
                generation,
                request,
                coalesced: Vec::new(),
            });
        }
    }

    fn validate_effect(
        &self,
        extension: &ExtensionId,
        generation: GenerationId,
        cause: CauseContext,
        request: &EffectRequest,
    ) -> Result<(), ExtensionError> {
        let invalid = |message: &str| ExtensionError {
            code: ExtensionErrorCode::InvalidRequest,
            message: message.into(),
            retryable: false,
        };
        if request.id.owner() != *extension {
            return Err(invalid(
                "effect ID is not owned by the requesting extension",
            ));
        }
        if request.cause != cause {
            return Err(invalid("effect cause does not match the dispatched event"));
        }
        if self
            .pending_effects
            .contains_key(&(extension.clone(), request.id.clone()))
        {
            return Err(ExtensionError {
                code: ExtensionErrorCode::Conflict,
                message: "effect ID is already pending".into(),
                retryable: false,
            });
        }
        if self
            .runtimes
            .get(extension)
            .is_none_or(|runtime| runtime.generation != generation)
        {
            return Err(ExtensionError {
                code: ExtensionErrorCode::StaleResource,
                message: "extension generation is stale".into(),
                retryable: false,
            });
        }
        if !effect_shape_is_bounded(&request.effect, &self.config.validation_limits) {
            return Err(invalid("effect payload exceeds host limits"));
        }
        match &request.effect {
            ExtensionEffect::CapabilityCall { capability, .. } => {
                if !self.capability_is_granted(extension, capability) {
                    return Err(ExtensionError {
                        code: ExtensionErrorCode::CapabilityUnavailable,
                        message: format!("capability {capability} was not granted"),
                        retryable: false,
                    });
                }
                if let Some(permission) = self.capability_permissions.get(capability) {
                    self.require_permission(extension, permission)?;
                }
            }
            ExtensionEffect::OpenContribution { contribution } => {
                if contribution.owner() != *extension
                    || !self.contribution_exists(extension, contribution)
                {
                    return Err(invalid(
                        "contribution is not owned and declared by extension",
                    ));
                }
                if let Some(permission) = self.contribution_permission(extension, contribution)
                    && !self.permission_is_granted(extension, &permission)
                {
                    return Err(permission_error(permission));
                }
            }
            ExtensionEffect::CloseContribution { contribution } => {
                if contribution.owner() != *extension
                    || !self.contribution_exists(extension, contribution)
                {
                    return Err(invalid(
                        "contribution is not owned and declared by extension",
                    ));
                }
                // Closing host UI must remain possible after a permission is
                // revoked; it reduces authority and visible surface.
            }
            ExtensionEffect::StorageGet { area, .. }
            | ExtensionEffect::StoragePut { area, .. }
            | ExtensionEffect::StorageDelete { area, .. } => {
                if !self.storage_permission_is_granted(extension, *area) {
                    return Err(permission_error(Permission::Storage(
                        key_extension_api::StoragePermission {
                            area: *area,
                            quota_bytes: 0,
                        },
                    )));
                }
            }
            ExtensionEffect::CopyText { .. } => {
                self.require_permission(extension, &Permission::ClipboardWrite)?;
            }
            ExtensionEffect::OpenBrowserUrl { url } => {
                self.require_permission(extension, &Permission::OpenExternalUrl)?;
                if !bounded_http_url(url) {
                    return Err(invalid("browser URL must be a bounded HTTP(S) URL"));
                }
            }
            ExtensionEffect::StartTask { task, .. } | ExtensionEffect::CancelTask { task } => {
                if task.owner() != *extension {
                    return Err(invalid("task ID is not owned by extension"));
                }
            }
            ExtensionEffect::Notify { .. } | ExtensionEffect::Confirm { .. } => {}
        }
        Ok(())
    }

    fn require_permission(
        &self,
        extension: &ExtensionId,
        permission: &Permission,
    ) -> Result<(), ExtensionError> {
        if self.permission_is_granted(extension, permission) {
            Ok(())
        } else {
            Err(permission_error(permission.clone()))
        }
    }

    fn permission_is_granted(&self, extension: &ExtensionId, permission: &Permission) -> bool {
        self.registry.get(extension).is_some_and(|record| {
            record
                .manifest()
                .permissions
                .iter()
                .any(|request| request.permission == *permission)
        }) && self.permission_decision(extension, permission) == PermissionDecision::Granted
    }

    fn purge_snapshot_namespaces_for_permission(
        &mut self,
        extension: &ExtensionId,
        permission: &Permission,
    ) {
        let namespaces = self
            .snapshot_permissions
            .iter()
            .filter(|(_, required)| required.contains(permission))
            .map(|(snapshot, _)| snapshot_namespace(*snapshot))
            .collect::<Vec<_>>();
        if let Some(runtime) = self.runtimes.get_mut(extension) {
            for namespace in namespaces {
                runtime.state.remove(namespace);
            }
        }
    }

    fn snapshot_is_authorized(&self, extension: &ExtensionId, snapshot: SnapshotKind) -> bool {
        let subscribed = self.runtimes.get(extension).is_some_and(|runtime| {
            runtime.subscriptions.iter().any(
                |subscription| matches!(subscription, EventSubscription::Snapshot(kind) if *kind == snapshot),
            )
        });
        subscribed
            && self
                .snapshot_permissions
                .iter()
                .find(|(kind, _)| *kind == snapshot)
                .is_none_or(|(_, permissions)| {
                    permissions
                        .iter()
                        .all(|permission| self.permission_is_granted(extension, permission))
                })
    }

    fn storage_permission_is_granted(&self, extension: &ExtensionId, area: StorageArea) -> bool {
        self.registry.get(extension).is_some_and(|record| {
            record.manifest().permissions.iter().any(|request| {
                matches!(&request.permission, Permission::Storage(storage) if storage.area == area)
                    && self.permission_decision(extension, &request.permission)
                        == PermissionDecision::Granted
            })
        })
    }

    fn capability_is_granted(&self, extension: &ExtensionId, capability: &CapabilityId) -> bool {
        self.runtimes.get(extension).is_some_and(|runtime| {
            runtime
                .capabilities
                .granted
                .iter()
                .any(|grant| grant.capability == *capability)
        })
    }

    fn contribution_exists(&self, extension: &ExtensionId, contribution: &ContributionId) -> bool {
        self.registry.get(extension).is_some_and(|record| {
            let contributions = &record.manifest().contributions;
            contributions
                .menus
                .iter()
                .any(|item| item.id == *contribution)
                || contributions
                    .views
                    .iter()
                    .any(|item| item.id == *contribution)
        })
    }

    fn contribution_permission(
        &self,
        extension: &ExtensionId,
        contribution: &ContributionId,
    ) -> Option<Permission> {
        let record = self.registry.get(extension)?;
        let view = record
            .manifest()
            .contributions
            .views
            .iter()
            .find(|view| view.id == *contribution)?;
        match view.slot {
            ContributionSlot::SidePanel | ContributionSlot::SettingsPanel => {
                Some(Permission::AddSidePanel)
            }
            ContributionSlot::DocumentOverlay => Some(Permission::AddDocumentOverlays),
            _ => None,
        }
    }

    fn queue_effect_result(
        &mut self,
        extension: &ExtensionId,
        generation: GenerationId,
        request: &EffectRequest,
        result: EffectResult,
    ) {
        if self
            .runtimes
            .get(extension)
            .is_none_or(|runtime| runtime.generation != generation)
        {
            return;
        }
        let Some(cause) = self.child_cause(request.cause) else {
            self.record_violation(
                extension,
                DiagnosticCode::DispatchDepthExceeded,
                "effect result exceeded maximum dispatch depth",
            );
            return;
        };
        let _ = self.enqueue_event_internal(
            extension.clone(),
            EventSource::Host,
            ExtensionEvent::EffectCompleted {
                effect: request.id.clone(),
                result,
            },
            cause,
            request.cause.id,
        );
    }

    pub fn complete_effect(
        &mut self,
        effect: &ArbitratedEffect,
        result: EffectResult,
    ) -> Result<(), HostError> {
        let key = (effect.extension.clone(), effect.request.id.clone());
        let Some(pending) = self.pending_effects.remove(&key) else {
            return Err(HostError::EffectNotPending {
                extension: effect.extension.clone(),
                effect: effect.request.id.clone(),
            });
        };
        if pending.generation != effect.generation {
            return Err(HostError::EffectNotPending {
                extension: effect.extension.clone(),
                effect: effect.request.id.clone(),
            });
        }
        self.queue_completed_effect(
            &effect.extension,
            effect.generation,
            &effect.request.id,
            pending.cause,
            result.clone(),
        );
        for follower in &effect.coalesced {
            if let Some(follower_pending) = self
                .pending_effects
                .remove(&(effect.extension.clone(), follower.clone()))
            {
                debug_assert_eq!(follower_pending.leader, effect.request.id);
                self.queue_completed_effect(
                    &effect.extension,
                    effect.generation,
                    follower,
                    follower_pending.cause,
                    result.clone(),
                );
            }
        }
        Ok(())
    }

    fn queue_completed_effect(
        &mut self,
        extension: &ExtensionId,
        generation: GenerationId,
        effect: &EffectId,
        parent: CauseContext,
        result: EffectResult,
    ) {
        if self
            .runtimes
            .get(extension)
            .is_none_or(|runtime| runtime.generation != generation)
        {
            return;
        }
        let Some(cause) = self.child_cause(parent) else {
            self.record_violation(
                extension,
                DiagnosticCode::DispatchDepthExceeded,
                "effect completion exceeded maximum dispatch depth",
            );
            return;
        };
        let _ = self.enqueue_event_internal(
            extension.clone(),
            EventSource::Host,
            ExtensionEvent::EffectCompleted {
                effect: effect.clone(),
                result,
            },
            cause,
            parent.id,
        );
    }

    pub fn suspend(
        &mut self,
        extension: &ExtensionId,
        reason: impl Into<String>,
    ) -> Result<(), HostError> {
        let reason = reason.into();
        let state = self
            .state(extension)
            .ok_or_else(|| HostError::NotInstalled(extension.clone()))?;
        if state != LifecycleState::Active {
            return Err(HostError::InvalidState {
                extension: extension.clone(),
                state,
            });
        }
        let callback_error = self
            .runtimes
            .get_mut(extension)
            .and_then(|runtime| runtime.instance.as_mut())
            .and_then(|instance| instance.suspend(&reason).err());
        self.registry
            .get_mut(extension)
            .expect("record exists")
            .transition(LifecycleState::Suspended)?;
        self.queue.retain(|event| event.target != *extension);
        self.pending_effects
            .retain(|(owner, _), _| owner != extension);
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Warning,
            DiagnosticCode::ExtensionSuspended,
            reason,
        );
        if let Some(error) = callback_error {
            self.diagnostic(
                Some(extension.clone()),
                DiagnosticSeverity::Error,
                DiagnosticCode::ExtensionFault,
                error.message,
            );
        }
        self.reconcile_active_capabilities();
        self.broadcast_capability_changes();
        Ok(())
    }

    pub fn resume(&mut self, extension: &ExtensionId) -> Result<Vec<ArbitratedEffect>, HostError> {
        let record = self
            .registry
            .get(extension)
            .ok_or_else(|| HostError::NotInstalled(extension.clone()))?;
        if record.state() != LifecycleState::Suspended {
            return Err(HostError::InvalidState {
                extension: extension.clone(),
                state: record.state(),
            });
        }
        let manifest = record.manifest().clone();
        let metadata = record.metadata().clone();
        if self.config.safe_mode && !metadata.origin.allowed_in_safe_mode() {
            return Err(HostError::SafeMode(extension.clone()));
        }
        self.check_dependencies(&manifest)?;
        let capabilities = self.resolve_capabilities(&manifest)?;
        self.check_activation_permissions(&manifest)?;
        let cause = self.root_cause();
        let generation = self
            .runtimes
            .get(extension)
            .expect("suspended extension retains runtime")
            .generation;
        let context = ActivationContext {
            extension: extension.clone(),
            generation,
            cause,
            capabilities: capabilities.clone(),
        };
        let update = {
            let runtime = self.runtimes.get_mut(extension).expect("runtime exists");
            runtime.capabilities = capabilities;
            match runtime.instance.as_mut() {
                Some(instance) => {
                    instance
                        .resume(&context)
                        .map_err(|error| HostError::ExtensionFailed {
                            extension: extension.clone(),
                            error,
                        })?
                }
                None => NativeUpdate::default(),
            }
        };
        self.registry
            .get_mut(extension)
            .expect("record exists")
            .transition(LifecycleState::Active)?;
        let mut effects = Vec::new();
        self.arbitrate_update(extension, generation, cause, update, &mut effects);
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Info,
            DiagnosticCode::ExtensionResumed,
            "extension resumed",
        );
        self.broadcast_capability_changes();
        Ok(effects)
    }

    pub fn unload(&mut self, extension: &ExtensionId) -> Result<(), HostError> {
        let state = self
            .state(extension)
            .ok_or_else(|| HostError::NotInstalled(extension.clone()))?;
        if state == LifecycleState::Disabled {
            return Ok(());
        }
        match state {
            LifecycleState::Active | LifecycleState::Suspended => {
                self.registry
                    .get_mut(extension)
                    .expect("record exists")
                    .transition(LifecycleState::Unloading)?;
                if let Some(mut runtime) = self.runtimes.remove(extension) {
                    if let Some(settings) = runtime.state.get("settings").cloned() {
                        self.persisted_settings.insert(extension.clone(), settings);
                    }
                    if let Some(instance) = runtime.instance.as_mut() {
                        instance.unload();
                    }
                }
                self.registry
                    .get_mut(extension)
                    .expect("record exists")
                    .transition(LifecycleState::Disabled)?;
            }
            LifecycleState::Installed
            | LifecycleState::Validated
            | LifecycleState::Failed
            | LifecycleState::Unloading => {
                self.registry
                    .get_mut(extension)
                    .expect("record exists")
                    .transition(LifecycleState::Disabled)?;
            }
            LifecycleState::Removed => {
                return Err(HostError::NotInstalled(extension.clone()));
            }
            LifecycleState::Disabled => {}
        }
        self.queue.retain(|event| event.target != *extension);
        self.pending_effects
            .retain(|(owner, _), _| owner != extension);
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Info,
            DiagnosticCode::ExtensionUnloaded,
            "extension runtime, work, and contributions unloaded",
        );
        self.reconcile_active_capabilities();
        self.broadcast_capability_changes();
        Ok(())
    }

    pub fn remove(&mut self, extension: &ExtensionId) -> Result<ExtensionManifest, HostError> {
        self.unload(extension)?;
        self.registry
            .get_mut(extension)
            .expect("record exists")
            .transition(LifecycleState::Removed)?;
        self.permission_decisions.remove(extension);
        self.persisted_settings.remove(extension);
        let record = self
            .registry
            .remove_record(extension)
            .expect("record exists");
        Ok(record.manifest().clone())
    }

    pub fn set_safe_mode(&mut self, enabled: bool) {
        self.config.safe_mode = enabled;
        if !enabled {
            return;
        }
        let to_suspend = self
            .registry
            .iter()
            .filter(|(_, record)| {
                record.state() == LifecycleState::Active
                    && !record.metadata().origin.allowed_in_safe_mode()
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for extension in to_suspend {
            let _ = self.suspend(&extension, "safe mode enabled");
        }
    }

    #[must_use]
    pub fn is_safe_mode(&self) -> bool {
        self.config.safe_mode
    }

    pub fn collect_contributions(&mut self) -> CollectedContributions {
        let active = self
            .registry
            .iter()
            .filter(|(_, record)| record.state() == LifecycleState::Active)
            .map(|(id, record)| (id.clone(), record.manifest().contributions.clone()))
            .collect::<Vec<_>>();
        let mut result = CollectedContributions::default();
        for (owner, contributions) in active {
            result
                .commands
                .extend(
                    contributions
                        .commands
                        .into_iter()
                        .map(|command| OwnedCommand {
                            owner: owner.clone(),
                            command,
                        }),
                );
            result
                .menus
                .extend(contributions.menus.into_iter().map(|menu| {
                    OwnedMenu {
                        owner: owner.clone(),
                        menu,
                        state: self
                            .runtimes
                            .get(&owner)
                            .map_or_else(BTreeMap::new, |runtime| runtime.state.clone()),
                    }
                }));
            result
                .views
                .extend(contributions.views.into_iter().map(|view| {
                    OwnedView {
                        owner: owner.clone(),
                        view,
                        state: self
                            .runtimes
                            .get(&owner)
                            .map_or_else(BTreeMap::new, |runtime| runtime.state.clone()),
                    }
                }));
        }
        result
            .commands
            .sort_by(|left, right| left.command.id.cmp(&right.command.id));
        let menu_cycle = order_owned_menus(&mut result.menus);
        let view_cycle = order_owned_views(&mut result.views);
        for menu in &mut result.menus {
            if sort_menu_tree(&mut menu.menu.items) {
                self.diagnostic(
                    Some(menu.owner.clone()),
                    DiagnosticSeverity::Warning,
                    DiagnosticCode::ContributionOrderFallback,
                    "cyclic nested menu ordering used deterministic fallback",
                );
            }
        }
        if menu_cycle || view_cycle {
            self.diagnostic(
                None,
                DiagnosticSeverity::Warning,
                DiagnosticCode::ContributionOrderFallback,
                "cyclic contribution ordering used deterministic fallback",
            );
        }
        result
    }

    fn reconcile_active_capabilities(&mut self) {
        let active = self
            .registry
            .iter()
            .filter(|(_, record)| record.state() == LifecycleState::Active)
            .map(|(id, record)| (id.clone(), record.manifest().clone()))
            .collect::<Vec<_>>();
        let mut suspend = Vec::new();
        let mut updates = Vec::new();
        for (id, manifest) in active {
            match self.resolve_capabilities(&manifest) {
                Ok(snapshot) => updates.push((id, snapshot)),
                Err(_) => suspend.push(id),
            }
        }
        for (id, snapshot) in updates {
            if let Some(runtime) = self.runtimes.get_mut(&id) {
                runtime.capabilities = snapshot;
            }
        }
        for id in suspend {
            let _ = self.suspend(&id, "required capability provider became unavailable");
        }
    }

    fn broadcast_capability_changes(&mut self) {
        let updates = self
            .runtimes
            .iter()
            .filter(|(id, _)| self.state(id) == Some(LifecycleState::Active))
            .map(|(id, runtime)| (id.clone(), runtime.capabilities.clone()))
            .collect::<Vec<_>>();
        for (id, snapshot) in updates {
            let _ = self.enqueue_host_event(&id, ExtensionEvent::CapabilitiesChanged { snapshot });
        }
    }

    fn record_violation(
        &mut self,
        extension: &ExtensionId,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        let message = message.into();
        self.diagnostic(
            Some(extension.clone()),
            DiagnosticSeverity::Warning,
            code,
            message.clone(),
        );
        let should_suspend = self.runtimes.get_mut(extension).is_some_and(|runtime| {
            runtime.violations = runtime.violations.saturating_add(1);
            runtime.violations >= self.config.violations_before_suspension
        });
        if should_suspend && self.state(extension) == Some(LifecycleState::Active) {
            let _ = self.suspend(
                extension,
                format!("repeated host budget violations: {message}"),
            );
        }
    }

    fn diagnostic(
        &mut self,
        extension: Option<ExtensionId>,
        severity: DiagnosticSeverity,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.diagnostics
            .push(extension, severity, code, message.into());
    }
}

fn permission_error(permission: Permission) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::PermissionDenied,
        message: format!("permission {permission:?} was not granted"),
        retryable: false,
    }
}

fn event_is_mandatory(event: &ExtensionEvent) -> bool {
    matches!(
        event,
        ExtensionEvent::Lifecycle { .. } | ExtensionEvent::EffectCompleted { .. }
    )
}

fn subscription_matches(subscription: &EventSubscription, event: &ExtensionEvent) -> bool {
    match (subscription, event) {
        (EventSubscription::Lifecycle, ExtensionEvent::Lifecycle { .. })
        | (EventSubscription::Commands, ExtensionEvent::CommandInvoked { .. })
        | (EventSubscription::Capabilities, ExtensionEvent::CapabilitiesChanged { .. })
        | (EventSubscription::EffectResults, ExtensionEvent::EffectCompleted { .. }) => true,
        (
            EventSubscription::Snapshot(expected),
            ExtensionEvent::SnapshotChanged { snapshot, .. },
        ) => expected == snapshot,
        (EventSubscription::Custom(expected), ExtensionEvent::Custom { event, .. }) => {
            expected == event
        }
        _ => false,
    }
}

fn bounded_http_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.is_empty()
        && url.len() <= 8_192
        && !url.chars().any(char::is_control)
        && !rest.starts_with('/')
}

fn effect_shape_is_bounded(effect: &ExtensionEffect, limits: &ValidationLimits) -> bool {
    let strings = match effect {
        ExtensionEffect::CapabilityCall {
            operation, input, ..
        }
        | ExtensionEffect::StartTask {
            operation, input, ..
        } => {
            return operation.len() <= limits.maximum_string_bytes
                && value_is_bounded(input, limits);
        }
        ExtensionEffect::StorageGet { key, .. } | ExtensionEffect::StorageDelete { key, .. } => {
            vec![key.as_str()]
        }
        ExtensionEffect::StoragePut { key, value, .. } => {
            return key.len() <= limits.maximum_string_bytes && value_is_bounded(value, limits);
        }
        ExtensionEffect::CopyText { text } => vec![text.as_str()],
        ExtensionEffect::OpenBrowserUrl { url } => vec![url.as_str()],
        ExtensionEffect::Notify { message, .. } => vec![message.as_str()],
        ExtensionEffect::Confirm { title, message, .. } => {
            vec![title.as_str(), message.as_str()]
        }
        ExtensionEffect::OpenContribution { .. }
        | ExtensionEffect::CloseContribution { .. }
        | ExtensionEffect::CancelTask { .. } => Vec::new(),
    };
    strings
        .into_iter()
        .all(|value| value.len() <= limits.maximum_string_bytes)
}

fn value_is_bounded(value: &DataValue, limits: &ValidationLimits) -> bool {
    fn visit(
        value: &DataValue,
        limits: &ValidationLimits,
        depth: usize,
        nodes: &mut usize,
    ) -> bool {
        *nodes = nodes.saturating_add(1);
        if *nodes > limits.maximum_value_nodes || depth > limits.maximum_value_depth {
            return false;
        }
        match value {
            DataValue::Number(value) => value.is_finite(),
            DataValue::String(value) => value.len() <= limits.maximum_string_bytes,
            DataValue::List(values) => {
                values.len() <= limits.maximum_list_items
                    && values
                        .iter()
                        .all(|value| visit(value, limits, depth + 1, nodes))
            }
            DataValue::Record(values) => {
                values.len() <= limits.maximum_list_items
                    && values.iter().all(|(key, value)| {
                        state_key_is_safe(key, limits) && visit(value, limits, depth + 1, nodes)
                    })
            }
            DataValue::Null | DataValue::Boolean(_) | DataValue::Integer(_) => true,
        }
    }
    visit(value, limits, 1, &mut 0)
}

fn state_is_bounded(state: &BTreeMap<String, DataValue>, limits: &ValidationLimits) -> bool {
    state.len() <= limits.maximum_list_items
        && state.keys().all(|key| state_key_is_safe(key, limits))
        && value_is_bounded(&DataValue::Record(state.clone()), limits)
}

fn state_key_is_safe(key: &str, limits: &ValidationLimits) -> bool {
    !key.is_empty()
        && key.len() <= limits.maximum_string_bytes
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

const HOST_STATE_NAMESPACES: [&str; 8] = [
    "settings",
    "application",
    "document",
    "viewport",
    "selection",
    "annotation",
    "theme",
    "capabilities",
];

fn preserve_host_state_namespaces(
    current: &BTreeMap<String, DataValue>,
    incoming: &mut BTreeMap<String, DataValue>,
) {
    for namespace in HOST_STATE_NAMESPACES {
        incoming.remove(namespace);
        if let Some(value) = current.get(namespace) {
            incoming.insert(namespace.to_owned(), value.clone());
        }
    }
}

fn snapshot_state_update(envelope: &EventEnvelope) -> Option<(&'static str, DataValue)> {
    if envelope.source != EventSource::Host {
        return None;
    }
    let ExtensionEvent::SnapshotChanged { snapshot, value } = &envelope.event else {
        return None;
    };
    Some((snapshot_namespace(*snapshot), value.clone()))
}

const fn snapshot_namespace(snapshot: SnapshotKind) -> &'static str {
    match snapshot {
        key_extension_api::SnapshotKind::Application => "application",
        key_extension_api::SnapshotKind::Document => "document",
        key_extension_api::SnapshotKind::Viewport => "viewport",
        key_extension_api::SnapshotKind::Selection => "selection",
        key_extension_api::SnapshotKind::Annotation => "annotation",
        key_extension_api::SnapshotKind::Theme => "theme",
        key_extension_api::SnapshotKind::Capabilities => "capabilities",
    }
}

fn initial_extension_state(manifest: &ExtensionManifest) -> BTreeMap<String, DataValue> {
    let mut state = BTreeMap::new();
    for setting in manifest
        .settings
        .fields
        .iter()
        .filter(|setting| !setting.sensitive)
    {
        let binding = StateBinding::new(std::iter::once("settings").chain(setting.key.split('.')));
        set_state_binding(&mut state, &binding, setting.default.clone());
    }
    state
}

fn normalize_persisted_settings(
    manifest: &ExtensionManifest,
    settings: DataValue,
    limits: &ValidationLimits,
) -> Result<DataValue, String> {
    let DataValue::Record(incoming) = settings else {
        return Err("persisted extension settings must be a record".into());
    };
    if !value_is_bounded(&DataValue::Record(incoming.clone()), limits) {
        return Err("persisted extension settings exceed host bounds".into());
    }

    fn get_path<'a>(
        record: &'a BTreeMap<String, DataValue>,
        path: &[&str],
    ) -> Option<&'a DataValue> {
        let (head, tail) = path.split_first()?;
        let value = record.get(*head)?;
        if tail.is_empty() {
            Some(value)
        } else if let DataValue::Record(record) = value {
            get_path(record, tail)
        } else {
            None
        }
    }

    let mut normalized = initial_extension_state(manifest);
    let mut observed = BTreeMap::new();
    for setting in manifest
        .settings
        .fields
        .iter()
        .filter(|setting| !setting.sensitive)
    {
        let path = setting.key.split('.').collect::<Vec<_>>();
        let Some(value) = get_path(&incoming, &path).cloned() else {
            continue;
        };
        if !setting.value_type.accepts(&value) {
            return Err(format!(
                "persisted value for setting '{}' has the wrong type or range",
                setting.key
            ));
        }
        let binding = StateBinding::new(std::iter::once("settings").chain(path.iter().copied()));
        set_state_binding(&mut normalized, &binding, value.clone());
        set_state_binding(&mut observed, &binding, value);
    }
    let observed = match observed.remove("settings") {
        Some(DataValue::Record(observed)) => observed,
        None => BTreeMap::new(),
        Some(_) => unreachable!("settings bindings always build records"),
    };
    if observed != incoming {
        return Err("persisted extension settings contain unknown or sensitive keys".into());
    }
    Ok(normalized
        .remove("settings")
        .unwrap_or_else(|| DataValue::Record(BTreeMap::new())))
}

fn set_state_binding(
    state: &mut BTreeMap<String, DataValue>,
    binding: &StateBinding,
    value: DataValue,
) {
    fn set_path(record: &mut BTreeMap<String, DataValue>, path: &[String], value: DataValue) {
        let Some((head, tail)) = path.split_first() else {
            return;
        };
        if tail.is_empty() {
            record.insert(head.clone(), value);
            return;
        }
        let child = record
            .entry(head.clone())
            .or_insert_with(|| DataValue::Record(BTreeMap::new()));
        if !matches!(child, DataValue::Record(_)) {
            *child = DataValue::Record(BTreeMap::new());
        }
        let DataValue::Record(child) = child else {
            unreachable!("record was initialized above")
        };
        set_path(child, tail, value);
    }

    set_path(state, &binding.0, value);
}

fn order_owned_menus(menus: &mut Vec<OwnedMenu>) -> bool {
    stable_order(
        menus,
        |item| item.menu.id.as_str().to_owned(),
        |item| &item.menu.order,
        |item| item.menu.slot.as_str().to_owned(),
    )
}

fn order_owned_views(views: &mut Vec<OwnedView>) -> bool {
    stable_order(
        views,
        |item| item.view.id.as_str().to_owned(),
        |item| &item.view.order,
        |item| format!("{:?}", item.view.slot),
    )
}

fn stable_order<T: Clone>(
    items: &mut Vec<T>,
    id: impl Fn(&T) -> String,
    order: impl Fn(&T) -> &ContributionOrder,
    group: impl Fn(&T) -> String,
) -> bool {
    let original = std::mem::take(items);
    let mut groups = BTreeMap::<String, Vec<T>>::new();
    for item in original {
        groups.entry(group(&item)).or_default().push(item);
    }
    let mut cycle = false;
    for (_, mut group_items) in groups {
        let by_id = group_items
            .iter()
            .enumerate()
            .map(|(index, item)| (id(item), index))
            .collect::<BTreeMap<_, _>>();
        let mut edges = vec![BTreeSet::<usize>::new(); group_items.len()];
        let mut indegree = vec![0usize; group_items.len()];
        for (index, item) in group_items.iter().enumerate() {
            for target in &order(item).before {
                if let Some(&target) = by_id.get(target.as_str())
                    && edges[index].insert(target)
                {
                    indegree[target] += 1;
                }
            }
            for source in &order(item).after {
                if let Some(&source) = by_id.get(source.as_str())
                    && edges[source].insert(index)
                {
                    indegree[index] += 1;
                }
            }
        }
        let mut remaining = (0..group_items.len()).collect::<BTreeSet<_>>();
        while !remaining.is_empty() {
            let mut ready = remaining
                .iter()
                .copied()
                .filter(|index| indegree[*index] == 0)
                .collect::<Vec<_>>();
            if ready.is_empty() {
                cycle = true;
                ready.extend(remaining.iter().copied());
            }
            ready.sort_by(|left, right| {
                order(&group_items[*right])
                    .priority
                    .cmp(&order(&group_items[*left]).priority)
                    .then_with(|| id(&group_items[*left]).cmp(&id(&group_items[*right])))
            });
            let selected = ready[0];
            remaining.remove(&selected);
            for target in &edges[selected] {
                indegree[*target] = indegree[*target].saturating_sub(1);
            }
            items.push(group_items[selected].clone());
        }
        group_items.clear();
    }
    cycle
}

fn sort_menu_tree(items: &mut Vec<MenuItem>) -> bool {
    let mut cycle = false;
    for item in items.iter_mut() {
        if let MenuItemKind::Submenu { children, .. } = &mut item.kind {
            cycle |= sort_menu_tree(children);
        }
    }
    let original = std::mem::take(items);
    let by_id = original
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id.as_str().to_owned(), index))
        .collect::<BTreeMap<_, _>>();
    let mut edges = vec![BTreeSet::<usize>::new(); original.len()];
    let mut indegree = vec![0usize; original.len()];
    for (index, item) in original.iter().enumerate() {
        for target in &item.order.before {
            if let Some(&target) = by_id.get(target.as_str())
                && edges[index].insert(target)
            {
                indegree[target] += 1;
            }
        }
        for source in &item.order.after {
            if let Some(&source) = by_id.get(source.as_str())
                && edges[source].insert(index)
            {
                indegree[index] += 1;
            }
        }
    }
    let mut remaining = (0..original.len()).collect::<BTreeSet<_>>();
    while !remaining.is_empty() {
        let mut ready = remaining
            .iter()
            .copied()
            .filter(|index| indegree[*index] == 0)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            cycle = true;
            ready.extend(remaining.iter().copied());
        }
        ready.sort_by(|left, right| {
            original[*right]
                .order
                .priority
                .cmp(&original[*left].order.priority)
                .then_with(|| original[*left].id.cmp(&original[*right].id))
        });
        let selected = ready[0];
        remaining.remove(&selected);
        for target in &edges[selected] {
            indegree[*target] = indegree[*target].saturating_sub(1);
        }
        items.push(original[selected].clone());
    }
    cycle
}

#[cfg(test)]
mod tests {
    use std::{
        str::FromStr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use key_extension_api::{
        BooleanSource, CURRENT_MANIFEST_SCHEMA, CapabilityRequirements, CapabilityScope,
        CompatibleVersion, ContributionSet, ExtensionDependency, HostCompatibility, LifecycleEvent,
        LocalId, MenuItemOrder, MenuSlotId, NativeAdapterId, NotificationTone, PermissionRequest,
        Platform, Publisher, SettingDefinition, SettingType, SettingsSchema, StorageRequirements,
        UiNode, UiNodeKind,
    };

    use crate::PackageOrigin;

    use super::*;

    fn id(value: &str) -> ExtensionId {
        ExtensionId::parse(value).unwrap()
    }

    fn version(value: &str) -> ExtensionVersion {
        ExtensionVersion::from_str(value).unwrap()
    }

    fn requirement(value: &str) -> CompatibleVersion {
        CompatibleVersion::from_str(value).unwrap()
    }

    fn manifest(extension: &str, entrypoint: ExtensionEntrypoint) -> ExtensionManifest {
        ExtensionManifest {
            schema_version: CURRENT_MANIFEST_SCHEMA,
            id: id(extension),
            name: extension.into(),
            version: version("1.0.0"),
            publisher: Publisher {
                id: "example".into(),
                name: "Example".into(),
            },
            description: "test extension".into(),
            license: "MIT".into(),
            compatibility: HostCompatibility {
                extension_api: requirement("^0.1"),
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

    fn declarative_manifest(extension: &str) -> ExtensionManifest {
        manifest(
            extension,
            ExtensionEntrypoint::Declarative {
                ui: key_extension_api::PackagePath::parse("ui/main.toml").unwrap(),
            },
        )
    }

    fn native_manifest(extension: &str) -> ExtensionManifest {
        manifest(
            extension,
            ExtensionEntrypoint::NativeBuiltin {
                adapter: NativeAdapterId::parse(format!("{extension}/native")).unwrap(),
                ui: None,
            },
        )
    }

    fn wasm_manifest(extension: &str) -> ExtensionManifest {
        manifest(
            extension,
            ExtensionEntrypoint::WasmComponent {
                component: key_extension_api::PackagePath::parse("component.wasm").unwrap(),
                world: "key:extension-runtime/extension@0.1.0".into(),
                ui: None,
            },
        )
    }

    #[derive(Clone, Copy)]
    enum Behavior {
        None,
        State,
        Copy,
        DuplicateNotify,
        Fail,
    }

    struct TestExtension {
        extension: ExtensionId,
        behavior: Behavior,
        events: Arc<Mutex<Vec<ExtensionEvent>>>,
        unloaded: Arc<AtomicBool>,
    }

    struct DeferredTestExtension {
        generation: Option<GenerationId>,
        pending: VecDeque<DeferredNativeUpdate>,
        canceled: Arc<AtomicBool>,
        unloaded: Arc<AtomicBool>,
        fail_activation: bool,
    }

    impl NativeExtension for DeferredTestExtension {
        fn subscriptions(&self) -> Vec<EventSubscription> {
            vec![EventSubscription::Commands]
        }

        fn activate(
            &mut self,
            context: &ActivationContext,
        ) -> Result<NativeUpdate, ExtensionError> {
            self.generation = Some(context.generation);
            self.pending.push_back(DeferredNativeUpdate {
                generation: context.generation,
                call: DeferredCall::Activation {
                    cause: context.cause,
                },
                result: if self.fail_activation {
                    Err(ExtensionError {
                        code: ExtensionErrorCode::Internal,
                        message: "background activation failed".into(),
                        retryable: false,
                    })
                } else {
                    Ok(NativeUpdate::default())
                },
            });
            Ok(NativeUpdate::default())
        }

        fn handle_event(
            &mut self,
            envelope: &EventEnvelope,
        ) -> Result<NativeUpdate, ExtensionError> {
            let value = match &envelope.event {
                ExtensionEvent::CommandInvoked { payload, .. } => payload.clone(),
                _ => DataValue::Null,
            };
            self.pending.push_back(DeferredNativeUpdate {
                generation: self.generation.expect("activation establishes generation"),
                call: DeferredCall::Event(envelope.clone()),
                result: Ok(NativeUpdate::with_state(BTreeMap::from([(
                    "deferred".into(),
                    value,
                )]))),
            });
            Ok(NativeUpdate::default())
        }

        fn update_delivery(&self) -> UpdateDelivery {
            UpdateDelivery::Deferred
        }

        fn drain_deferred_updates(&mut self, maximum: usize) -> Vec<DeferredNativeUpdate> {
            (0..maximum)
                .filter_map(|_| self.pending.pop_front())
                .collect()
        }

        fn pending_deferred_work(&self) -> usize {
            self.pending.len()
        }

        fn cancel_deferred_events(&mut self) {
            self.canceled.store(true, Ordering::SeqCst);
            self.pending
                .retain(|update| !matches!(update.call, DeferredCall::Event(_)));
        }

        fn unload(&mut self) {
            self.pending.clear();
            self.unloaded.store(true, Ordering::SeqCst);
        }
    }

    impl NativeExtension for TestExtension {
        fn subscriptions(&self) -> Vec<EventSubscription> {
            vec![
                EventSubscription::Commands,
                EventSubscription::Capabilities,
                EventSubscription::Snapshot(SnapshotKind::Document),
                EventSubscription::EffectResults,
            ]
        }

        fn handle_event(
            &mut self,
            envelope: &EventEnvelope,
        ) -> Result<NativeUpdate, ExtensionError> {
            self.events.lock().unwrap().push(envelope.event.clone());
            if !matches!(envelope.event, ExtensionEvent::CommandInvoked { .. }) {
                return Ok(NativeUpdate::default());
            }
            match self.behavior {
                Behavior::None => Ok(NativeUpdate::default()),
                Behavior::State => Ok(NativeUpdate::with_state(BTreeMap::from([(
                    "status".into(),
                    DataValue::String("ready".into()),
                )]))),
                Behavior::Fail => Err(ExtensionError {
                    code: ExtensionErrorCode::Internal,
                    message: "test failure".into(),
                    retryable: false,
                }),
                Behavior::Copy => Ok(NativeUpdate::with_effects(vec![EffectRequest {
                    id: EffectId::parse(format!("{}/copy", self.extension)).unwrap(),
                    cause: envelope.cause,
                    effect: ExtensionEffect::CopyText {
                        text: "copied".into(),
                    },
                }])),
                Behavior::DuplicateNotify => Ok(NativeUpdate::with_effects(vec![
                    EffectRequest {
                        id: EffectId::parse(format!("{}/notify-one", self.extension)).unwrap(),
                        cause: envelope.cause,
                        effect: ExtensionEffect::Notify {
                            message: "done".into(),
                            tone: NotificationTone::Success,
                        },
                    },
                    EffectRequest {
                        id: EffectId::parse(format!("{}/notify-two", self.extension)).unwrap(),
                        cause: envelope.cause,
                        effect: ExtensionEffect::Notify {
                            message: "done".into(),
                            tone: NotificationTone::Success,
                        },
                    },
                ])),
            }
        }

        fn unload(&mut self) {
            self.unloaded.store(true, Ordering::SeqCst);
        }
    }

    fn install_native(
        host: &mut ExtensionHost,
        manifest: ExtensionManifest,
        behavior: Behavior,
        events: Arc<Mutex<Vec<ExtensionEvent>>>,
        unloaded: Arc<AtomicBool>,
    ) {
        let extension = manifest.id.clone();
        let ExtensionEntrypoint::NativeBuiltin { adapter, .. } = &manifest.entrypoint else {
            panic!("native test manifest expected");
        };
        host.register_native_adapter(NativeExtensionAdapter::new(adapter.clone(), move || {
            Box::new(TestExtension {
                extension: extension.clone(),
                behavior,
                events: events.clone(),
                unloaded: unloaded.clone(),
            }) as Box<dyn NativeExtension>
        }));
        host.install(manifest, PackageMetadata::bundled()).unwrap();
    }

    fn command_for(extension: &ExtensionId) -> CommandId {
        CommandId::parse(format!("{extension}/run")).unwrap()
    }

    fn add_command(manifest: &mut ExtensionManifest) {
        manifest.contributions.commands.push(CommandDefinition {
            id: command_for(&manifest.id),
            title: "Run".into(),
            description: String::new(),
            category: "Tests".into(),
        });
    }

    #[test]
    fn registry_validation_and_license_policy_fail_closed() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let mut invalid = declarative_manifest("org.example.invalid");
        invalid.schema_version = 99;
        assert!(matches!(
            host.install(invalid, PackageMetadata::bundled()),
            Err(HostError::Registry(RegistryError::InvalidManifest { .. }))
        ));

        let mut copyleft = declarative_manifest("org.example.copyleft");
        copyleft.license = "GPL-3.0".into();
        assert!(matches!(
            host.install(copyleft, PackageMetadata::bundled()),
            Err(HostError::LicenseDenied { .. })
        ));
        assert_eq!(host.diagnostics().len(), 2);
    }

    #[test]
    fn native_updates_publish_bounded_state_through_the_shared_contract() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.state");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        install_native(
            &mut host,
            package,
            Behavior::State,
            Arc::default(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();
        host.invoke_command(&command_for(&extension), DataValue::Null)
            .unwrap();
        host.process_tick();
        assert_eq!(
            host.extension_state(&extension)
                .and_then(|state| state.get("status")),
            Some(&DataValue::String("ready".into()))
        );
        host.unload(&extension).unwrap();
        assert!(host.extension_state(&extension).is_none());
    }

    #[test]
    fn declarative_set_state_runs_without_adapter_subscriptions() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.declarative-state");
        let mut package = declarative_manifest(extension.as_str());
        add_command(&mut package);
        package.settings.fields.push(SettingDefinition {
            key: "theme-preset".into(),
            label: "Theme preset".into(),
            description: String::new(),
            value_type: SettingType::String {
                maximum_bytes: Some(64),
            },
            default: DataValue::String("paper".into()),
            sensitive: false,
        });
        package.settings.fields.push(SettingDefinition {
            key: "api-token".into(),
            label: "API token".into(),
            description: String::new(),
            value_type: SettingType::String {
                maximum_bytes: Some(64),
            },
            default: DataValue::String("must-not-enter-ui-state".into()),
            sensitive: true,
        });
        package
            .contributions
            .command_behaviors
            .push(CommandBehavior {
                command: command_for(&extension),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["settings", "theme-preset"]),
                },
            });
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.activate(&extension).unwrap();

        host.invoke_command(
            &command_for(&extension),
            DataValue::String("graphite".into()),
        )
        .unwrap();
        assert_eq!(host.process_tick().processed_events, 1);
        let settings = host
            .extension_state(&extension)
            .and_then(|state| state.get("settings"))
            .and_then(|value| match value {
                DataValue::Record(settings) => Some(settings),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            settings.get("theme-preset"),
            Some(&DataValue::String("graphite".into()))
        );
        assert!(
            !settings.contains_key("api-token"),
            "sensitive settings must remain outside extension-visible UI state"
        );

        host.invoke_command(&command_for(&extension), DataValue::Integer(7))
            .unwrap();
        host.process_tick();
        let settings = host
            .extension_state(&extension)
            .and_then(|state| state.get("settings"))
            .and_then(|value| match value {
                DataValue::Record(settings) => Some(settings),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            settings.get("theme-preset"),
            Some(&DataValue::String("graphite".into())),
            "wrongly typed command payload must not mutate settings"
        );

        let root = host
            .enqueue_host_event(
                &extension,
                ExtensionEvent::Lifecycle {
                    event: LifecycleEvent::ApplicationReady,
                },
            )
            .unwrap();
        host.enqueue_extension_event(
            &extension,
            &extension,
            root,
            ExtensionEvent::CommandInvoked {
                command: command_for(&extension),
                payload: DataValue::String("forged".into()),
            },
        )
        .unwrap();
        host.process_tick();
        let settings = host
            .extension_state(&extension)
            .and_then(|state| state.get("settings"))
            .and_then(|value| match value {
                DataValue::Record(settings) => Some(settings),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            settings.get("theme-preset"),
            Some(&DataValue::String("graphite".into())),
            "extension-sourced command events must not execute host behaviors"
        );

        let persisted = host.extension_settings(&extension).unwrap();
        host.unload(&extension).unwrap();
        assert_eq!(host.extension_settings(&extension), Some(persisted));
        host.activate(&extension).unwrap();
        assert!(matches!(
            host.extension_settings(&extension),
            Some(DataValue::Record(settings))
                if settings.get("theme-preset")
                    == Some(&DataValue::String("graphite".into()))
        ));

        assert!(
            host.restore_extension_settings(
                &extension,
                DataValue::Record(BTreeMap::from([(
                    "api-token".into(),
                    DataValue::String("must stay private".into()),
                )])),
            )
            .is_err()
        );
    }

    #[test]
    fn host_snapshots_atomically_replace_their_stable_namespace() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.document-snapshot");
        install_native(
            &mut host,
            native_manifest(extension.as_str()),
            Behavior::None,
            Arc::default(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();

        host.enqueue_host_event(
            &extension,
            ExtensionEvent::SnapshotChanged {
                snapshot: key_extension_api::SnapshotKind::Document,
                value: DataValue::Record(BTreeMap::from([(
                    "statistics".into(),
                    DataValue::Record(BTreeMap::from([(
                        "page-count".into(),
                        DataValue::Integer(12),
                    )])),
                )])),
            },
        )
        .unwrap();
        host.process_tick();
        host.enqueue_host_event(
            &extension,
            ExtensionEvent::SnapshotChanged {
                snapshot: key_extension_api::SnapshotKind::Document,
                value: DataValue::Record(BTreeMap::from([(
                    "statistics".into(),
                    DataValue::Record(BTreeMap::from([(
                        "word-count".into(),
                        DataValue::Integer(1_024),
                    )])),
                )])),
            },
        )
        .unwrap();
        host.process_tick();

        let statistics = host
            .extension_state(&extension)
            .and_then(|state| state.get("document"))
            .and_then(|value| match value {
                DataValue::Record(document) => document.get("statistics"),
                _ => None,
            })
            .and_then(|value| match value {
                DataValue::Record(statistics) => Some(statistics),
                _ => None,
            })
            .expect("document statistics snapshot");
        assert_eq!(statistics.get("page-count"), None);
        assert_eq!(
            statistics.get("word-count"),
            Some(&DataValue::Integer(1_024))
        );
    }

    #[test]
    fn snapshots_require_both_subscription_and_explicit_permission() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.permissioned-snapshot");
        let permission = Permission::ReadDocumentMetadata;
        host.require_snapshot_permissions(SnapshotKind::Document, [permission.clone()]);
        let mut package = native_manifest(extension.as_str());
        package.permissions.push(PermissionRequest {
            permission: permission.clone(),
            reason: "Read the active document summary".into(),
            required: false,
        });
        install_native(
            &mut host,
            package,
            Behavior::None,
            Arc::default(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();

        let event = ExtensionEvent::SnapshotChanged {
            snapshot: SnapshotKind::Document,
            value: DataValue::Record(BTreeMap::from([(
                "title".into(),
                DataValue::String("Sensitive title".into()),
            )])),
        };
        host.enqueue_host_event(&extension, event.clone()).unwrap();
        let denied = host.process_tick();
        assert_eq!(denied.dropped_events, 1);
        assert!(
            host.extension_state(&extension)
                .is_some_and(|state| !state.contains_key("document"))
        );

        host.set_permission_decision(
            extension.clone(),
            permission.clone(),
            PermissionDecision::Granted,
        );
        host.enqueue_host_event(&extension, event).unwrap();
        assert_eq!(host.process_tick().processed_events, 1);
        assert!(
            host.extension_state(&extension)
                .is_some_and(|state| state.contains_key("document"))
        );

        host.set_permission_decision(extension.clone(), permission, PermissionDecision::Denied);
        assert!(
            host.extension_state(&extension)
                .is_some_and(|state| !state.contains_key("document")),
            "revocation must purge the host-owned snapshot namespace"
        );
    }

    #[test]
    fn capability_calls_require_the_registered_permission() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.permissioned-capability");
        let capability = CapabilityId::parse("key:test/document-metadata").unwrap();
        let permission = Permission::ReadDocumentMetadata;
        host.register_host_capability(capability.clone(), version("1.0.0"));
        host.require_capability_permission(capability.clone(), permission.clone());

        let mut package = declarative_manifest(extension.as_str());
        package.capabilities.required.push(CapabilityRequest {
            id: capability.clone(),
            version: requirement("^1"),
            scope: CapabilityScope::ActiveDocument,
        });
        package.permissions.push(PermissionRequest {
            permission: permission.clone(),
            reason: "Read document metadata".into(),
            required: false,
        });
        host.install(package, PackageMetadata::bundled()).unwrap();
        let activation = host.activate(&extension).unwrap();
        let cause = host.root_cause();
        let request = EffectRequest {
            id: EffectId::parse(format!("{extension}/read")).unwrap(),
            cause,
            effect: ExtensionEffect::CapabilityCall {
                capability,
                operation: "read".into(),
                input: DataValue::Null,
            },
        };

        let denied = host
            .validate_effect(&extension, activation.generation, cause, &request)
            .unwrap_err();
        assert_eq!(denied.code, ExtensionErrorCode::PermissionDenied);

        host.set_permission_decision(extension.clone(), permission, PermissionDecision::Granted);
        host.validate_effect(&extension, activation.generation, cause, &request)
            .unwrap();
    }

    #[test]
    fn native_adapter_state_and_manifest_behavior_merge_deterministically() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.merged-state");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        package.settings.fields.push(SettingDefinition {
            key: "preset".into(),
            label: "Preset".into(),
            description: String::new(),
            value_type: SettingType::String {
                maximum_bytes: Some(64),
            },
            default: DataValue::String("paper".into()),
            sensitive: false,
        });
        package
            .contributions
            .command_behaviors
            .push(CommandBehavior {
                command: command_for(&extension),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["settings", "preset"]),
                },
            });
        install_native(
            &mut host,
            package,
            Behavior::State,
            Arc::default(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();
        host.enqueue_host_event(
            &extension,
            ExtensionEvent::SnapshotChanged {
                snapshot: key_extension_api::SnapshotKind::Document,
                value: DataValue::Record(BTreeMap::from([(
                    "statistics".into(),
                    DataValue::Record(BTreeMap::from([(
                        "page-count".into(),
                        DataValue::Integer(12),
                    )])),
                )])),
            },
        )
        .unwrap();
        host.process_tick();
        host.invoke_command(
            &command_for(&extension),
            DataValue::String("graphite".into()),
        )
        .unwrap();
        host.process_tick();
        let state = host.extension_state(&extension).unwrap();
        assert_eq!(
            state.get("status"),
            Some(&DataValue::String("ready".into()))
        );
        assert!(matches!(
            state.get("document"),
            Some(DataValue::Record(document))
                if matches!(
                    document.get("statistics"),
                    Some(DataValue::Record(statistics))
                        if statistics.get("page-count") == Some(&DataValue::Integer(12))
                )
        ));
        assert!(matches!(
            state.get("settings"),
            Some(DataValue::Record(settings))
                if settings.get("preset") == Some(&DataValue::String("graphite".into()))
        ));
    }

    #[test]
    fn open_is_permission_checked_but_close_remains_available_after_revocation() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.panel-behavior");
        let open = CommandId::parse(format!("{extension}/open")).unwrap();
        let close = CommandId::parse(format!("{extension}/close")).unwrap();
        let panel = ContributionId::parse(format!("{extension}/panel")).unwrap();
        let mut package = declarative_manifest(extension.as_str());
        for command in [&open, &close] {
            package.contributions.commands.push(CommandDefinition {
                id: command.clone(),
                title: "Panel".into(),
                description: String::new(),
                category: String::new(),
            });
        }
        package.permissions.push(PermissionRequest {
            permission: Permission::AddSidePanel,
            reason: "Open the panel".into(),
            required: true,
        });
        package.contributions.views.push(UiContribution {
            id: panel.clone(),
            slot: ContributionSlot::SidePanel,
            order: ContributionOrder::default(),
            root: UiNode {
                id: LocalId::parse("root").unwrap(),
                visible: BooleanSource::Constant(true),
                kind: UiNodeKind::Text {
                    text: "Panel".into(),
                    selectable: true,
                },
            },
        });
        package.contributions.command_behaviors = vec![
            CommandBehavior {
                command: open.clone(),
                action: CommandBehaviorAction::OpenContribution {
                    contribution: panel.clone(),
                },
            },
            CommandBehavior {
                command: close.clone(),
                action: CommandBehaviorAction::CloseContribution {
                    contribution: panel.clone(),
                },
            },
        ];
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.set_permission_decision(
            extension.clone(),
            Permission::AddSidePanel,
            PermissionDecision::Granted,
        );
        host.activate(&extension).unwrap();
        host.set_permission_decision(
            extension.clone(),
            Permission::AddSidePanel,
            PermissionDecision::Denied,
        );

        host.invoke_command(&open, DataValue::Null).unwrap();
        assert!(host.process_tick().effects.is_empty());
        host.invoke_command(&close, DataValue::Null).unwrap();
        let effects = host.process_tick().effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0].request.effect,
            ExtensionEffect::CloseContribution {
                contribution: panel
            }
        );
    }

    #[test]
    fn command_payload_rejects_unsafe_nested_state_keys_before_queueing() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.payload-bounds");
        let mut package = declarative_manifest(extension.as_str());
        add_command(&mut package);
        package
            .contributions
            .command_behaviors
            .push(CommandBehavior {
                command: command_for(&extension),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["payload"]),
                },
            });
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.activate(&extension).unwrap();
        let payload = DataValue::Record(BTreeMap::from([(
            "unsafe.key".into(),
            DataValue::Boolean(true),
        )]));
        assert!(matches!(
            host.invoke_command(&command_for(&extension), payload),
            Err(HostError::EventRejected(_))
        ));
    }

    #[test]
    fn activation_negotiates_capabilities_and_permissions_before_native_code() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.activation");
        let capability = CapabilityId::parse("key:pdf/text").unwrap();
        let mut package = native_manifest(extension.as_str());
        package.capabilities.required.push(CapabilityRequest {
            id: capability.clone(),
            version: requirement("^1"),
            scope: CapabilityScope::ActiveDocument,
        });
        package.permissions.push(PermissionRequest {
            permission: Permission::ClipboardWrite,
            reason: "copy results".into(),
            required: true,
        });
        install_native(
            &mut host,
            package,
            Behavior::None,
            Arc::default(),
            Arc::default(),
        );
        assert!(matches!(
            host.activate(&extension),
            Err(HostError::RequiredCapabilityUnavailable { .. })
        ));
        host.register_host_capability(capability, version("1.2.0"));
        assert!(matches!(
            host.activate(&extension),
            Err(HostError::PermissionsRequired { .. })
        ));
        host.set_permission_decision(
            extension.clone(),
            Permission::ClipboardWrite,
            PermissionDecision::Granted,
        );
        let activation = host.activate(&extension).unwrap();
        assert_eq!(activation.capabilities.granted.len(), 1);
        assert_eq!(host.state(&extension), Some(LifecycleState::Active));
    }

    #[test]
    fn wasm_packages_require_an_explicit_package_scoped_adapter() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.wasm");
        let mut package = wasm_manifest(extension.as_str());
        add_command(&mut package);
        package.settings.fields.push(SettingDefinition {
            key: "mode".into(),
            label: "Mode".into(),
            description: String::new(),
            value_type: SettingType::String {
                maximum_bytes: Some(32),
            },
            default: DataValue::String("default".into()),
            sensitive: false,
        });
        package
            .contributions
            .command_behaviors
            .push(CommandBehavior {
                command: command_for(&extension),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["settings", "mode"]),
                },
            });
        host.install(package, PackageMetadata::bundled()).unwrap();
        assert_eq!(
            host.activate(&extension),
            Err(HostError::UnsupportedEntrypoint(extension.clone()))
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_factory = events.clone();
        let extension_for_factory = extension.clone();
        host.register_wasm_adapter(WasmExtensionAdapter::new(
            extension.clone(),
            move |entrypoint: &ExtensionEntrypoint| {
                assert!(matches!(
                    entrypoint,
                    ExtensionEntrypoint::WasmComponent { .. }
                ));
                Ok(Box::new(TestExtension {
                    extension: extension_for_factory.clone(),
                    behavior: Behavior::None,
                    events: events_for_factory.clone(),
                    unloaded: Arc::default(),
                }) as Box<dyn NativeExtension>)
            },
        ));
        host.activate(&extension).unwrap();
        assert_eq!(host.state(&extension), Some(LifecycleState::Active));
        host.invoke_command(
            &command_for(&extension),
            DataValue::String("sandboxed".into()),
        )
        .unwrap();
        host.process_tick();
        assert!(matches!(
            host.extension_state(&extension)
                .and_then(|state| state.get("settings")),
            Some(DataValue::Record(settings))
                if settings.get("mode") == Some(&DataValue::String("sandboxed".into()))
        ));
    }

    #[test]
    fn deferred_adapters_apply_only_polled_results_and_cancel_document_work() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.deferred");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        let adapter = match &package.entrypoint {
            ExtensionEntrypoint::NativeBuiltin { adapter, .. } => adapter.clone(),
            _ => unreachable!("native test package has a native entrypoint"),
        };
        let canceled = Arc::new(AtomicBool::new(false));
        let unloaded = Arc::new(AtomicBool::new(false));
        let factory_canceled = Arc::clone(&canceled);
        let factory_unloaded = Arc::clone(&unloaded);
        host.register_native_adapter(NativeExtensionAdapter::new(adapter, move || {
            Box::new(DeferredTestExtension {
                generation: None,
                pending: VecDeque::new(),
                canceled: Arc::clone(&factory_canceled),
                unloaded: Arc::clone(&factory_unloaded),
                fail_activation: false,
            }) as Box<dyn NativeExtension>
        }));
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.activate(&extension).unwrap();
        assert!(host.pending_event_count() >= 1);
        host.process_tick();

        host.invoke_command(&command_for(&extension), DataValue::String("first".into()))
            .unwrap();
        let dispatch = host.process_tick();
        assert_eq!(dispatch.processed_events, 1);
        assert!(dispatch.effects.is_empty());
        assert_eq!(dispatch.deferred_events, 1);
        assert!(
            host.extension_state(&extension)
                .and_then(|state| state.get("deferred"))
                .is_none(),
            "the enqueue placeholder must never be mistaken for guest output"
        );

        let completion = host.process_tick();
        assert_eq!(completion.deferred_events, 0);
        assert_eq!(
            host.extension_state(&extension)
                .and_then(|state| state.get("deferred")),
            Some(&DataValue::String("first".into()))
        );

        host.invoke_command(&command_for(&extension), DataValue::String("stale".into()))
            .unwrap();
        host.process_tick();
        assert_eq!(host.pending_event_count(), 1);
        host.invalidate_document_scope([]);
        assert!(canceled.load(Ordering::SeqCst));
        assert_eq!(host.pending_event_count(), 0);
        host.process_tick();
        assert_eq!(
            host.extension_state(&extension)
                .and_then(|state| state.get("deferred")),
            Some(&DataValue::String("first".into())),
            "a prior-document result must not cross the invalidation barrier"
        );

        host.unload(&extension).unwrap();
        assert!(unloaded.load(Ordering::SeqCst));
    }

    #[test]
    fn deferred_activation_failure_isolated_on_the_next_host_tick() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.deferred-failure");
        let package = native_manifest(extension.as_str());
        let adapter = match &package.entrypoint {
            ExtensionEntrypoint::NativeBuiltin { adapter, .. } => adapter.clone(),
            _ => unreachable!("native test package has a native entrypoint"),
        };
        let unloaded = Arc::new(AtomicBool::new(false));
        let factory_unloaded = Arc::clone(&unloaded);
        host.register_native_adapter(NativeExtensionAdapter::new(adapter, move || {
            Box::new(DeferredTestExtension {
                generation: None,
                pending: VecDeque::new(),
                canceled: Arc::new(AtomicBool::new(false)),
                unloaded: Arc::clone(&factory_unloaded),
                fail_activation: true,
            }) as Box<dyn NativeExtension>
        }));
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.activate(&extension)
            .expect("background activation is accepted without executing it inline");
        assert_eq!(host.state(&extension), Some(LifecycleState::Active));
        let report = host.process_tick();
        assert_eq!(report.dropped_events, 1);
        assert_eq!(host.state(&extension), Some(LifecycleState::Failed));
        assert!(unloaded.load(Ordering::SeqCst));
        assert!(host.collect_contributions().commands.is_empty());
    }

    #[test]
    fn command_effect_and_completion_follow_deferred_protocol() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.copy");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        package.permissions.push(PermissionRequest {
            permission: Permission::ClipboardWrite,
            reason: "copy".into(),
            required: true,
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        install_native(
            &mut host,
            package,
            Behavior::Copy,
            events.clone(),
            Arc::default(),
        );
        host.set_permission_decision(
            extension.clone(),
            Permission::ClipboardWrite,
            PermissionDecision::Granted,
        );
        host.activate(&extension).unwrap();
        host.invoke_command(&command_for(&extension), DataValue::Null)
            .unwrap();
        let report = host.process_tick();
        assert_eq!(report.effects.len(), 1);
        assert!(matches!(
            report.effects[0].request.effect,
            ExtensionEffect::CopyText { .. }
        ));
        host.complete_effect(&report.effects[0], Ok(DataValue::Null))
            .unwrap();
        let before = events.lock().unwrap().len();
        host.process_tick();
        let events = events.lock().unwrap();
        assert!(events.len() > before);
        assert!(
            events.iter().any(|event| matches!(
                event,
                ExtensionEvent::EffectCompleted { result: Ok(_), .. }
            ))
        );
    }

    #[test]
    fn denied_effect_returns_an_error_event_instead_of_escaping_host() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.denied");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        let events = Arc::new(Mutex::new(Vec::new()));
        install_native(
            &mut host,
            package,
            Behavior::Copy,
            events.clone(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();
        host.invoke_command(&command_for(&extension), DataValue::Null)
            .unwrap();
        assert!(host.process_tick().effects.is_empty());
        host.process_tick();
        assert!(events.lock().unwrap().iter().any(|event| matches!(
            event,
            ExtensionEvent::EffectCompleted {
                result: Err(ExtensionError {
                    code: ExtensionErrorCode::PermissionDenied,
                    ..
                }),
                ..
            }
        )));
    }

    #[test]
    fn identical_effects_are_coalesced_and_both_ids_receive_completion() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.coalesce");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        let events = Arc::new(Mutex::new(Vec::new()));
        install_native(
            &mut host,
            package,
            Behavior::DuplicateNotify,
            events.clone(),
            Arc::default(),
        );
        host.activate(&extension).unwrap();
        host.invoke_command(&command_for(&extension), DataValue::Null)
            .unwrap();
        let report = host.process_tick();
        assert_eq!(report.effects.len(), 1);
        assert_eq!(report.effects[0].coalesced.len(), 1);
        host.complete_effect(&report.effects[0], Ok(DataValue::Null))
            .unwrap();
        host.process_tick();
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, ExtensionEvent::EffectCompleted { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn failures_and_depth_violations_suspend_only_the_faulting_extension() {
        let config = HostConfig {
            violations_before_suspension: 1,
            maximum_dispatch_depth: 0,
            ..HostConfig::default()
        };
        let mut host = ExtensionHost::new(config);
        let failing = id("org.example.failing");
        let healthy = id("org.example.healthy");
        for (extension, behavior) in [(&failing, Behavior::Fail), (&healthy, Behavior::None)] {
            let mut package = native_manifest(extension.as_str());
            add_command(&mut package);
            install_native(&mut host, package, behavior, Arc::default(), Arc::default());
            host.activate(extension).unwrap();
        }
        host.invoke_command(&command_for(&failing), DataValue::Null)
            .unwrap();
        host.process_tick();
        assert_eq!(host.state(&failing), Some(LifecycleState::Suspended));
        assert_eq!(host.state(&healthy), Some(LifecycleState::Active));

        let root = host
            .enqueue_host_event(
                &healthy,
                ExtensionEvent::Lifecycle {
                    event: LifecycleEvent::ApplicationReady,
                },
            )
            .unwrap();
        assert!(
            host.enqueue_extension_event(
                &healthy,
                &healthy,
                root,
                ExtensionEvent::Lifecycle {
                    event: LifecycleEvent::ApplicationReady,
                },
            )
            .is_err()
        );
        assert_eq!(host.state(&healthy), Some(LifecycleState::Suspended));
    }

    #[test]
    fn safe_mode_suspends_third_party_extensions_and_preserves_bundled() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let bundled = id("org.example.bundled");
        let third_party = id("org.example.third-party");
        host.install(
            declarative_manifest(bundled.as_str()),
            PackageMetadata::bundled(),
        )
        .unwrap();
        host.install(
            declarative_manifest(third_party.as_str()),
            PackageMetadata {
                origin: PackageOrigin::ThirdParty,
                content_hash: Some("abc".into()),
                publisher_verified: false,
            },
        )
        .unwrap();
        host.activate(&bundled).unwrap();
        host.activate(&third_party).unwrap();
        host.set_safe_mode(true);
        assert_eq!(host.state(&bundled), Some(LifecycleState::Active));
        assert_eq!(host.state(&third_party), Some(LifecycleState::Suspended));
    }

    #[test]
    fn menu_folders_are_preserved_and_ordered_with_active_contributions() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.menu");
        let mut package = declarative_manifest(extension.as_str());
        add_command(&mut package);
        package
            .contributions
            .command_behaviors
            .push(CommandBehavior {
                command: command_for(&extension),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["menu-action"]),
                },
            });
        package.settings.fields.push(SettingDefinition {
            key: "menu.compact".into(),
            label: "Compact menu".into(),
            description: String::new(),
            value_type: SettingType::Boolean,
            default: DataValue::Boolean(true),
            sensitive: false,
        });
        let command = command_for(&extension);
        let first = MenuItem {
            id: LocalId::parse("first").unwrap(),
            order: MenuItemOrder {
                priority: 0,
                before: vec![LocalId::parse("second").unwrap()],
                after: Vec::new(),
            },
            visible: BooleanSource::Constant(true),
            kind: MenuItemKind::Command {
                label: "First".into(),
                command: command.clone(),
                payload: None,
                icon: None,
                enabled: BooleanSource::Constant(true),
                checked: None,
            },
        };
        let second = MenuItem {
            id: LocalId::parse("second").unwrap(),
            order: MenuItemOrder::default(),
            visible: BooleanSource::Constant(true),
            kind: MenuItemKind::Command {
                label: "Second".into(),
                command,
                payload: None,
                icon: None,
                enabled: BooleanSource::Constant(true),
                checked: None,
            },
        };
        package.contributions.menus.push(MenuContribution {
            id: ContributionId::parse(format!("{extension}/menu")).unwrap(),
            slot: MenuSlotId::parse("view.extensions").unwrap(),
            order: ContributionOrder::default(),
            items: vec![MenuItem {
                id: LocalId::parse("folder").unwrap(),
                order: MenuItemOrder::default(),
                visible: BooleanSource::Constant(true),
                kind: MenuItemKind::Submenu {
                    label: "Folder".into(),
                    icon: None,
                    children: vec![second, first],
                },
            }],
        });
        host.install(package, PackageMetadata::bundled()).unwrap();
        host.activate(&extension).unwrap();
        let contributions = host.collect_contributions();
        assert!(matches!(
            contributions.menus[0].state.get("settings"),
            Some(DataValue::Record(settings))
                if matches!(
                    settings.get("menu"),
                    Some(DataValue::Record(menu))
                        if menu.get("compact") == Some(&DataValue::Boolean(true))
                )
        ));
        let MenuItemKind::Submenu { children, .. } = &contributions.menus[0].menu.items[0].kind
        else {
            panic!("submenu preserved");
        };
        assert_eq!(children[0].id.as_str(), "first");
        assert_eq!(children[1].id.as_str(), "second");
        host.suspend(&extension, "test").unwrap();
        assert!(host.collect_contributions().menus.is_empty());
    }

    #[test]
    fn provider_unload_suspends_required_consumers() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let provider = id("org.example.provider");
        let consumer = id("org.example.consumer");
        let capability = CapabilityId::parse("key:service/index").unwrap();
        let mut provider_manifest = declarative_manifest(provider.as_str());
        provider_manifest
            .capabilities
            .provided
            .push(ProvidedCapability {
                id: capability.clone(),
                version: version("1.0.0"),
            });
        let mut consumer_manifest = declarative_manifest(consumer.as_str());
        consumer_manifest.dependencies.push(ExtensionDependency {
            id: provider.clone(),
            version: requirement("^1"),
            optional: false,
        });
        consumer_manifest
            .capabilities
            .required
            .push(CapabilityRequest {
                id: capability,
                version: requirement("^1"),
                scope: CapabilityScope::Application,
            });
        host.install(provider_manifest, PackageMetadata::bundled())
            .unwrap();
        host.install(consumer_manifest, PackageMetadata::bundled())
            .unwrap();
        host.activate(&provider).unwrap();
        host.activate(&consumer).unwrap();
        host.unload(&provider).unwrap();
        assert_eq!(host.state(&provider), Some(LifecycleState::Disabled));
        assert_eq!(host.state(&consumer), Some(LifecycleState::Suspended));
    }

    #[test]
    fn unload_cancels_runtime_work_and_calls_native_adapter() {
        let mut host = ExtensionHost::new(HostConfig::default());
        let extension = id("org.example.unload");
        let mut package = native_manifest(extension.as_str());
        add_command(&mut package);
        let unloaded = Arc::new(AtomicBool::new(false));
        install_native(
            &mut host,
            package,
            Behavior::DuplicateNotify,
            Arc::default(),
            unloaded.clone(),
        );
        host.activate(&extension).unwrap();
        host.invoke_command(&command_for(&extension), DataValue::Null)
            .unwrap();
        let effects = host.process_tick().effects;
        assert_eq!(effects.len(), 1);
        host.unload(&extension).unwrap();
        assert!(unloaded.load(Ordering::SeqCst));
        assert!(matches!(
            host.complete_effect(&effects[0], Ok(DataValue::Null)),
            Err(HostError::EffectNotPending { .. })
        ));
        assert!(host.collect_contributions().commands.is_empty());
    }

    #[test]
    fn dependency_graph_validation_detects_cross_package_cycles() {
        let mut registry = PackageRegistry::new(ValidationLimits::default());
        let a = id("org.example.a");
        let b = id("org.example.b");
        let mut a_manifest = declarative_manifest(a.as_str());
        let mut b_manifest = declarative_manifest(b.as_str());
        a_manifest.dependencies.push(ExtensionDependency {
            id: b.clone(),
            version: requirement("^1"),
            optional: false,
        });
        b_manifest.dependencies.push(ExtensionDependency {
            id: a,
            version: requirement("^1"),
            optional: false,
        });
        registry
            .install(a_manifest, PackageMetadata::bundled())
            .unwrap();
        registry
            .install(b_manifest, PackageMetadata::bundled())
            .unwrap();
        let errors = registry.validate_installed_graph().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| { error.code == key_extension_api::ValidationCode::DependencyCycle })
        );
    }

    #[test]
    fn diagnostics_are_bounded_and_deterministically_sequenced() {
        let config = HostConfig {
            diagnostic_capacity: 2,
            ..HostConfig::default()
        };
        let mut host = ExtensionHost::new(config);
        for name in ["one", "two", "three"] {
            host.install(
                declarative_manifest(&format!("org.example.{name}")),
                PackageMetadata::bundled(),
            )
            .unwrap();
        }
        let diagnostics = host.diagnostics();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].sequence + 1, diagnostics[1].sequence);
    }

    #[test]
    fn platform_requirement_type_remains_runtime_neutral() {
        let requirement = key_extension_api::PlatformRequirement {
            platform: Platform::Macos,
            minimum_version: Some("14".into()),
        };
        assert_eq!(requirement.platform, Platform::Macos);
    }
}
