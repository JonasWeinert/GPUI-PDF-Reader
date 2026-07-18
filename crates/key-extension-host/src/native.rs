use std::sync::Arc;

use key_extension_api::{
    CapabilitySnapshot, CauseContext, EventEnvelope, EventSubscription, ExtensionEntrypoint,
    ExtensionError, ExtensionId, ExtensionUpdate, GenerationId, NativeAdapterId,
};

#[derive(Clone, Debug)]
pub struct ActivationContext {
    pub extension: ExtensionId,
    pub generation: GenerationId,
    pub cause: CauseContext,
    pub capabilities: CapabilitySnapshot,
}

/// Compatibility name for the runtime-neutral update used by every adapter.
pub type NativeUpdate = ExtensionUpdate;

/// Describes how an adapter delivers the result of a lifecycle or event call.
///
/// Trusted, inexpensive built-ins normally return an update inline. Sandboxed
/// runtimes use `Deferred`: the call only places immutable input in a bounded
/// worker mailbox and the host later obtains the result through
/// [`NativeExtension::drain_deferred_updates`]. This distinction lets the
/// semantic host preserve one arbitration path without ever waiting for guest
/// code on a platform UI thread.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateDelivery {
    #[default]
    Immediate,
    Deferred,
}

/// The original authority context for an update completed by a background
/// adapter. The host, rather than the adapter, still owns command behavior,
/// state replacement, effect validation, and permission arbitration.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredCall {
    Activation { cause: CauseContext },
    Event(EventEnvelope),
    Resume { cause: CauseContext },
}

/// One immutable background result. `generation` is copied from the host's
/// activation context and prevents a result from a replaced runtime being
/// applied to its successor.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredNativeUpdate {
    pub generation: GenerationId,
    pub call: DeferredCall,
    pub result: Result<NativeUpdate, ExtensionError>,
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

    /// Whether successful callback returns are placeholders whose real update
    /// will be delivered by [`Self::drain_deferred_updates`]. Errors returned
    /// directly still mean the mailbox rejected the call.
    fn update_delivery(&self) -> UpdateDelivery {
        UpdateDelivery::Immediate
    }

    /// Drain at most `maximum` completed background calls without blocking.
    /// Implementations must retain any additional results for a future tick.
    fn drain_deferred_updates(&mut self, _maximum: usize) -> Vec<DeferredNativeUpdate> {
        Vec::new()
    }

    /// Total queued, executing, and completed calls retained by the adapter.
    /// The host uses this only to schedule another bounded tick.
    fn pending_deferred_work(&self) -> usize {
        0
    }

    /// Cancel event work tied to the previous document generation while
    /// retaining the extension instance and lifecycle operations.
    fn cancel_deferred_events(&mut self) {}

    /// Establishes a new document-generation boundary inside the adapter.
    ///
    /// The host has already discarded queued events and outstanding effect
    /// tokens when it calls this hook. Inline adapters should clear any
    /// document-scoped state machines or cached opaque resources. Deferred
    /// adapters inherit cancellation as the safe default, so prior-generation
    /// guest results cannot cross the boundary.
    fn invalidate_document_scope(&mut self) {
        self.cancel_deferred_events();
    }

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
