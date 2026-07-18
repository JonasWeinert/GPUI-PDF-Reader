use std::{
    fmt,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

use key_extension_api::{EffectRequest, EventEnvelope, ExtensionUpdate, LifecycleState};
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap,
    component::{Component, Instance, Linker},
};

use crate::{WasmDiagnostic, WasmDiagnosticCode, WasmRuntimeLimits, WasmStage};

#[derive(Clone)]
pub struct WasmRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    engine: Engine,
    limits: WasmRuntimeLimits,
    ticker: EpochTicker,
}

impl WasmRuntime {
    pub fn new(limits: WasmRuntimeLimits) -> Result<Self, WasmDiagnostic> {
        limits.validate()?;
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true)
            .max_wasm_stack(limits.maximum_wasm_stack_bytes);
        let engine = Engine::new(&config).map_err(|error| {
            WasmDiagnostic::new(
                WasmDiagnosticCode::InvalidConfiguration,
                WasmStage::Configuration,
                error.to_string(),
            )
        })?;
        let ticker = EpochTicker::new(engine.clone(), limits.epoch_tick_interval)?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                engine,
                limits,
                ticker,
            }),
        })
    }

    #[must_use]
    pub fn limits(&self) -> &WasmRuntimeLimits {
        &self.inner.limits
    }

    pub fn compile(&self, bytes: &[u8]) -> Result<CompiledExtensionComponent, WasmDiagnostic> {
        if bytes.len() > self.inner.limits.maximum_component_bytes {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::ComponentTooLarge,
                WasmStage::Validation,
                format!(
                    "component is {} bytes; limit is {} bytes",
                    bytes.len(),
                    self.inner.limits.maximum_component_bytes
                ),
            ));
        }
        let component = Component::new(&self.inner.engine, bytes)
            .map_err(|error| map_wasmtime_error(error, WasmStage::Compilation))?;
        Ok(CompiledExtensionComponent {
            runtime: Arc::clone(&self.inner),
            component,
            source_bytes: bytes.len(),
        })
    }

    pub fn instantiate(&self, bytes: &[u8]) -> Result<WasmExtensionInstance, WasmDiagnostic> {
        self.compile(bytes)?.instantiate()
    }

    /// Advance the engine epoch manually. The runtime also advances it from a
    /// sleeping ticker while calls are active, but this hook makes deterministic
    /// host scheduling and tests possible.
    pub fn increment_epoch(&self) {
        self.inner.engine.increment_epoch();
    }
}

pub struct CompiledExtensionComponent {
    runtime: Arc<RuntimeInner>,
    component: Component,
    source_bytes: usize,
}

impl fmt::Debug for CompiledExtensionComponent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledExtensionComponent")
            .field("source_bytes", &self.source_bytes)
            .finish_non_exhaustive()
    }
}

impl CompiledExtensionComponent {
    #[must_use]
    pub fn source_bytes(&self) -> usize {
        self.source_bytes
    }

    pub fn instantiate(&self) -> Result<WasmExtensionInstance, WasmDiagnostic> {
        let limits = &self.runtime.limits;
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.maximum_memory_bytes)
            .table_elements(limits.maximum_table_elements)
            .instances(limits.maximum_instances)
            .memories(limits.maximum_memories)
            .tables(limits.maximum_tables)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.runtime.engine,
            WasmStoreData {
                store_limits,
                budget: InvocationBudget::new(limits),
            },
        );
        store.limiter(|state| &mut state.store_limits);
        prepare_store(&mut store, limits)?;
        let _active_call = self.runtime.ticker.enter();
        // The linker is intentionally empty. WASI, filesystem, sockets,
        // clocks, randomness, and host capabilities are absent unless a later
        // permissioned adapter explicitly adds a narrow interface.
        let linker = Linker::<WasmStoreData>::new(&self.runtime.engine);
        let instance = linker
            .instantiate(&mut store, &self.component)
            .map_err(|error| map_wasmtime_error(error, WasmStage::Instantiation))?;
        Ok(WasmExtensionInstance {
            runtime: Arc::clone(&self.runtime),
            store: Some(store),
            instance: Some(instance),
            state: LifecycleState::Validated,
        })
    }
}

pub struct WasmExtensionInstance {
    runtime: Arc<RuntimeInner>,
    store: Option<Store<WasmStoreData>>,
    instance: Option<Instance>,
    state: LifecycleState,
}

impl fmt::Debug for WasmExtensionInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WasmExtensionInstance")
            .field("state", &self.state)
            .field("loaded", &self.is_loaded())
            .finish_non_exhaustive()
    }
}

impl WasmExtensionInstance {
    #[must_use]
    pub const fn state(&self) -> LifecycleState {
        self.state
    }

    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.store.is_some() && self.instance.is_some()
    }

    pub fn activate(&mut self) -> Result<(), WasmDiagnostic> {
        if self.state != LifecycleState::Validated {
            return Err(
                self.lifecycle_error(WasmStage::Activation, "activation requires validated state")
            );
        }
        self.call_unit_raw("activate", WasmStage::Activation)?;
        self.state = LifecycleState::Active;
        Ok(())
    }

    pub fn suspend(&mut self) -> Result<(), WasmDiagnostic> {
        if self.state != LifecycleState::Active {
            return Err(self.lifecycle_error(
                WasmStage::Invocation,
                "only an active extension can be suspended",
            ));
        }
        self.state = LifecycleState::Suspended;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), WasmDiagnostic> {
        if self.state != LifecycleState::Suspended {
            return Err(self.lifecycle_error(
                WasmStage::Invocation,
                "only a suspended extension can be resumed",
            ));
        }
        self.state = LifecycleState::Active;
        Ok(())
    }

    /// Invoke a unit-typed diagnostic or extension export. Normal extension
    /// traffic should use [`Self::dispatch`].
    pub fn call_unit(&mut self, export: &str) -> Result<(), WasmDiagnostic> {
        self.require_active()?;
        self.call_unit_raw(export, WasmStage::Invocation)
    }

    /// Call the byte transport defined by `wit/extension.wit`.
    pub fn call_bytes(&mut self, export: &str, input: &[u8]) -> Result<Vec<u8>, WasmDiagnostic> {
        self.require_active()?;
        if input.len() > self.runtime.limits.maximum_input_bytes {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::InputLimitExceeded,
                WasmStage::Invocation,
                format!(
                    "input is {} bytes; limit is {} bytes",
                    input.len(),
                    self.runtime.limits.maximum_input_bytes
                ),
            ));
        }
        let runtime = Arc::clone(&self.runtime);
        let (store, instance) = self.loaded_parts_mut(WasmStage::Invocation)?;
        prepare_store(store, &runtime.limits)?;
        let _active_call = runtime.ticker.enter();
        let function = instance
            .get_typed_func::<(Vec<u8>,), (Vec<u8>,)>(&mut *store, export)
            .map_err(|error| map_export_error(error, WasmStage::Invocation))?;
        let result = function
            .call(&mut *store, (input.to_vec(),))
            .map_err(|error| map_wasmtime_error(error, WasmStage::Invocation));
        let output = match result {
            Ok((output,)) => output,
            Err(error) => {
                self.mark_failed(&error);
                return Err(error);
            }
        };
        if output.len() > runtime.limits.maximum_output_bytes {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::OutputLimitExceeded,
                WasmStage::Invocation,
                format!(
                    "output is {} bytes; limit is {} bytes",
                    output.len(),
                    runtime.limits.maximum_output_bytes
                ),
            ));
        }
        Ok(output)
    }

    /// Serialize one typed host event and deserialize the bounded state/effect
    /// update returned by the component.
    pub fn dispatch(&mut self, event: &EventEnvelope) -> Result<ExtensionUpdate, WasmDiagnostic> {
        let input = serde_json::to_vec(event).map_err(|error| {
            WasmDiagnostic::new(
                WasmDiagnosticCode::Serialization,
                WasmStage::Invocation,
                error.to_string(),
            )
        })?;
        let output = self.call_bytes("handle-event", &input)?;
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum GuestUpdate {
            Current(ExtensionUpdate),
            Legacy(Vec<EffectRequest>),
        }
        let update: GuestUpdate = serde_json::from_slice(&output).map_err(|error| {
            WasmDiagnostic::new(
                WasmDiagnosticCode::Serialization,
                WasmStage::Invocation,
                error.to_string(),
            )
        })?;
        let update = match update {
            GuestUpdate::Current(update) => update,
            GuestUpdate::Legacy(effects) => ExtensionUpdate::with_effects(effects),
        };
        if update.effects.len() > self.runtime.limits.maximum_effects_per_event {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::TooManyEffects,
                WasmStage::Invocation,
                format!(
                    "component returned {} effects; limit is {}",
                    update.effects.len(),
                    self.runtime.limits.maximum_effects_per_event
                ),
            ));
        }
        Ok(update)
    }

    /// Deactivate and drop the entire Store. All component memories, tables,
    /// instances, and host resource accounting are released here.
    pub fn unload(&mut self) -> Result<(), WasmDiagnostic> {
        if !self.is_loaded() {
            return Ok(());
        }
        let result = if matches!(
            self.state,
            LifecycleState::Active | LifecycleState::Suspended
        ) {
            self.state = LifecycleState::Unloading;
            self.call_unit_raw("deactivate", WasmStage::Deactivation)
        } else {
            Ok(())
        };
        self.instance.take();
        self.store.take();
        self.state = LifecycleState::Disabled;
        result
    }

    fn call_unit_raw(&mut self, export: &str, stage: WasmStage) -> Result<(), WasmDiagnostic> {
        let runtime = Arc::clone(&self.runtime);
        let (store, instance) = self.loaded_parts_mut(stage)?;
        prepare_store(store, &runtime.limits)?;
        let _active_call = runtime.ticker.enter();
        let function = instance
            .get_typed_func::<(), ()>(&mut *store, export)
            .map_err(|error| map_export_error(error, stage))?;
        if let Err(error) = function.call(&mut *store, ()) {
            let diagnostic = map_wasmtime_error(error, stage);
            self.mark_failed(&diagnostic);
            return Err(diagnostic);
        }
        Ok(())
    }

    fn loaded_parts_mut(
        &mut self,
        stage: WasmStage,
    ) -> Result<(&mut Store<WasmStoreData>, Instance), WasmDiagnostic> {
        let Some(store) = self.store.as_mut() else {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::Unloaded,
                stage,
                "extension store has been unloaded",
            ));
        };
        let Some(instance) = self.instance else {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::Unloaded,
                stage,
                "extension instance has been unloaded",
            ));
        };
        Ok((store, instance))
    }

    fn require_active(&self) -> Result<(), WasmDiagnostic> {
        if self.state == LifecycleState::Active {
            Ok(())
        } else {
            Err(self.lifecycle_error(WasmStage::Invocation, "extension is not active"))
        }
    }

    fn lifecycle_error(&self, stage: WasmStage, message: &str) -> WasmDiagnostic {
        WasmDiagnostic::new(
            WasmDiagnosticCode::InvalidLifecycle,
            stage,
            format!("{message}; current state is {:?}", self.state),
        )
    }

    fn mark_failed(&mut self, diagnostic: &WasmDiagnostic) {
        if matches!(
            diagnostic.code,
            WasmDiagnosticCode::FuelExhausted
                | WasmDiagnosticCode::DeadlineExceeded
                | WasmDiagnosticCode::Trap
                | WasmDiagnosticCode::MemoryLimitExceeded
                | WasmDiagnosticCode::TableLimitExceeded
        ) {
            self.state = LifecycleState::Failed;
        }
    }
}

impl Drop for WasmExtensionInstance {
    fn drop(&mut self) {
        self.instance.take();
        self.store.take();
    }
}

struct WasmStoreData {
    store_limits: StoreLimits,
    budget: InvocationBudget,
}

#[derive(Clone, Debug)]
pub struct InvocationBudget {
    maximum_host_calls: u64,
    maximum_resources: usize,
    host_calls: u64,
    live_resources: usize,
}

impl InvocationBudget {
    #[must_use]
    pub fn new(limits: &WasmRuntimeLimits) -> Self {
        Self {
            maximum_host_calls: limits.maximum_host_calls_per_invocation,
            maximum_resources: limits.maximum_host_resources,
            host_calls: 0,
            live_resources: 0,
        }
    }

    pub fn begin_invocation(&mut self) {
        self.host_calls = 0;
    }

    pub fn charge_host_call(&mut self) -> Result<(), WasmDiagnostic> {
        self.host_calls = self.host_calls.saturating_add(1);
        if self.host_calls > self.maximum_host_calls {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::HostCallLimitExceeded,
                WasmStage::Invocation,
                format!("host-call limit of {} exceeded", self.maximum_host_calls),
            ));
        }
        Ok(())
    }

    pub fn acquire_resource(&mut self) -> Result<(), WasmDiagnostic> {
        if self.live_resources >= self.maximum_resources {
            return Err(WasmDiagnostic::new(
                WasmDiagnosticCode::ResourceLimitExceeded,
                WasmStage::Invocation,
                format!("host-resource limit of {} exceeded", self.maximum_resources),
            ));
        }
        self.live_resources += 1;
        Ok(())
    }

    pub fn release_resource(&mut self) {
        self.live_resources = self.live_resources.saturating_sub(1);
    }

    #[must_use]
    pub const fn host_calls(&self) -> u64 {
        self.host_calls
    }

    #[must_use]
    pub const fn live_resources(&self) -> usize {
        self.live_resources
    }
}

fn prepare_store(
    store: &mut Store<WasmStoreData>,
    limits: &WasmRuntimeLimits,
) -> Result<(), WasmDiagnostic> {
    store.data_mut().budget.begin_invocation();
    store
        .set_fuel(limits.fuel_per_invocation)
        .map_err(|error| {
            WasmDiagnostic::new(
                WasmDiagnosticCode::InvalidConfiguration,
                WasmStage::Configuration,
                error.to_string(),
            )
        })?;
    store.set_epoch_deadline(limits.epoch_ticks_per_invocation);
    store.epoch_deadline_trap();
    Ok(())
}

fn map_export_error(error: wasmtime::Error, stage: WasmStage) -> WasmDiagnostic {
    let message = format!("{error:#}");
    let code = if message.contains("failed to find function export") {
        WasmDiagnosticCode::MissingExport
    } else if message.contains("failed to convert function") {
        WasmDiagnosticCode::SignatureMismatch
    } else {
        WasmDiagnosticCode::InvalidComponent
    };
    WasmDiagnostic::new(code, stage, message)
}

fn map_wasmtime_error(error: wasmtime::Error, stage: WasmStage) -> WasmDiagnostic {
    let message = format!("{error:#}");
    let code = match error.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => WasmDiagnosticCode::FuelExhausted,
        Some(Trap::Interrupt) => WasmDiagnosticCode::DeadlineExceeded,
        Some(_) => WasmDiagnosticCode::Trap,
        None if message.contains("memory")
            && (message.contains("limit") || message.contains("growing")) =>
        {
            WasmDiagnosticCode::MemoryLimitExceeded
        }
        None if message.contains("table")
            && (message.contains("limit") || message.contains("growing")) =>
        {
            WasmDiagnosticCode::TableLimitExceeded
        }
        None if stage == WasmStage::Instantiation
            && (message.contains("unknown import")
                || message.contains("import") && message.contains("not defined")
                || message.contains("imports function")
                    && message.contains("implementation is missing")) =>
        {
            WasmDiagnosticCode::MissingImport
        }
        None if stage == WasmStage::Instantiation => WasmDiagnosticCode::InstantiationFailed,
        None => WasmDiagnosticCode::InvalidComponent,
    };
    WasmDiagnostic::new(code, stage, message)
}

struct EpochTicker {
    state: Arc<(Mutex<EpochState>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct EpochState {
    active_calls: usize,
    stop: bool,
}

impl EpochTicker {
    fn new(engine: Engine, interval: std::time::Duration) -> Result<Self, WasmDiagnostic> {
        let state = Arc::new((Mutex::new(EpochState::default()), Condvar::new()));
        let thread_state = Arc::clone(&state);
        let handle = thread::Builder::new()
            .name("key-wasm-epoch".into())
            .spawn(move || epoch_loop(&engine, &thread_state, interval))
            .map_err(|error| {
                WasmDiagnostic::new(
                    WasmDiagnosticCode::InvalidConfiguration,
                    WasmStage::Configuration,
                    format!("could not start epoch timer: {error}"),
                )
            })?;
        Ok(Self {
            state,
            thread: Mutex::new(Some(handle)),
        })
    }

    fn enter(&self) -> ActiveEpochCall {
        let (lock, condition) = &*self.state;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_calls += 1;
        condition.notify_one();
        drop(state);
        ActiveEpochCall {
            state: Arc::clone(&self.state),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        let (lock, condition) = &*self.state;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stop = true;
        condition.notify_one();
        drop(state);
        if let Some(handle) = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = handle.join();
        }
    }
}

struct ActiveEpochCall {
    state: Arc<(Mutex<EpochState>, Condvar)>,
}

impl Drop for ActiveEpochCall {
    fn drop(&mut self) {
        let (lock, condition) = &*self.state;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_calls = state.active_calls.saturating_sub(1);
        condition.notify_one();
    }
}

fn epoch_loop(
    engine: &Engine,
    shared: &(Mutex<EpochState>, Condvar),
    interval: std::time::Duration,
) {
    let (lock, condition) = shared;
    let mut state = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        while state.active_calls == 0 && !state.stop {
            state = condition
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if state.stop {
            return;
        }
        let (next, timeout) = condition
            .wait_timeout(state, interval)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
        if state.stop {
            return;
        }
        if timeout.timed_out() && state.active_calls > 0 {
            drop(state);
            engine.increment_epoch();
            state = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_call_and_resource_budgets_are_independent() {
        let limits = WasmRuntimeLimits {
            maximum_host_calls_per_invocation: 1,
            maximum_host_resources: 1,
            ..WasmRuntimeLimits::default()
        };
        let mut budget = InvocationBudget::new(&limits);
        budget.charge_host_call().unwrap();
        assert_eq!(
            budget.charge_host_call().unwrap_err().code,
            WasmDiagnosticCode::HostCallLimitExceeded
        );
        budget.acquire_resource().unwrap();
        assert_eq!(
            budget.acquire_resource().unwrap_err().code,
            WasmDiagnosticCode::ResourceLimitExceeded
        );
        budget.begin_invocation();
        assert_eq!(budget.host_calls(), 0);
        assert_eq!(budget.live_resources(), 1);
        budget.release_resource();
        assert_eq!(budget.live_resources(), 0);
    }
}
