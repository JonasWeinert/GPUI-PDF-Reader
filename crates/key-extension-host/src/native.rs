use std::sync::Arc;

use key_extension_api::{
    CapabilitySnapshot, CauseContext, EffectRequest, EventEnvelope, EventSubscription,
    ExtensionEntrypoint, ExtensionError, ExtensionId, GenerationId, NativeAdapterId,
};

#[derive(Clone, Debug)]
pub struct ActivationContext {
    pub extension: ExtensionId,
    pub generation: GenerationId,
    pub cause: CauseContext,
    pub capabilities: CapabilitySnapshot,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NativeUpdate {
    pub effects: Vec<EffectRequest>,
}

impl NativeUpdate {
    #[must_use]
    pub fn with_effects(effects: Vec<EffectRequest>) -> Self {
        Self { effects }
    }
}

/// Trusted built-in implementation of the same semantic protocol used by
/// sandbox adapters. Calls are always made outside platform paint callbacks.
pub trait NativeExtension: Send {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        Vec::new()
    }

    fn activate(&mut self, _context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        Ok(NativeUpdate::default())
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError>;

    fn suspend(&mut self, _reason: &str) -> Result<(), ExtensionError> {
        Ok(())
    }

    fn resume(&mut self, _context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        Ok(NativeUpdate::default())
    }

    fn unload(&mut self) {}
}

pub trait NativeExtensionFactory: Send + Sync {
    fn create(&self) -> Box<dyn NativeExtension>;
}

impl<F> NativeExtensionFactory for F
where
    F: Fn() -> Box<dyn NativeExtension> + Send + Sync,
{
    fn create(&self) -> Box<dyn NativeExtension> {
        self()
    }
}

/// A host-registered factory. A package may only select an adapter whose ID is
/// namespaced to that package; package files never supply native code.
#[derive(Clone)]
pub struct NativeExtensionAdapter {
    id: NativeAdapterId,
    factory: Arc<dyn NativeExtensionFactory>,
}

impl NativeExtensionAdapter {
    #[must_use]
    pub fn new(id: NativeAdapterId, factory: impl NativeExtensionFactory + 'static) -> Self {
        Self {
            id,
            factory: Arc::new(factory),
        }
    }

    #[must_use]
    pub fn id(&self) -> &NativeAdapterId {
        &self.id
    }

    pub(crate) fn instantiate(&self) -> Box<dyn NativeExtension> {
        self.factory.create()
    }
}

/// Host-supplied factory for one already-validated WebAssembly package.
///
/// The package manifest can select a component path, but it cannot register
/// this adapter or provide native code. The installer must associate verified
/// component bytes with the matching extension ID before activation.
pub trait WasmExtensionFactory: Send + Sync {
    fn create(
        &self,
        entrypoint: &ExtensionEntrypoint,
    ) -> Result<Box<dyn NativeExtension>, ExtensionError>;
}

impl<F> WasmExtensionFactory for F
where
    F: Fn(&ExtensionEntrypoint) -> Result<Box<dyn NativeExtension>, ExtensionError> + Send + Sync,
{
    fn create(
        &self,
        entrypoint: &ExtensionEntrypoint,
    ) -> Result<Box<dyn NativeExtension>, ExtensionError> {
        self(entrypoint)
    }
}

/// Runtime adapter registered for exactly one installed extension. This keeps
/// Wasmtime and component bytes outside the runtime-neutral host crate while
/// giving native and sandboxed instances the same semantic lifecycle.
#[derive(Clone)]
pub struct WasmExtensionAdapter {
    extension: ExtensionId,
    factory: Arc<dyn WasmExtensionFactory>,
}

impl WasmExtensionAdapter {
    #[must_use]
    pub fn new(extension: ExtensionId, factory: impl WasmExtensionFactory + 'static) -> Self {
        Self {
            extension,
            factory: Arc::new(factory),
        }
    }

    #[must_use]
    pub fn extension(&self) -> &ExtensionId {
        &self.extension
    }

    pub(crate) fn instantiate(
        &self,
        entrypoint: &ExtensionEntrypoint,
    ) -> Result<Box<dyn NativeExtension>, ExtensionError> {
        self.factory.create(entrypoint)
    }
}
