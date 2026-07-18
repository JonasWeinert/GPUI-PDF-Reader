//! Thread boundary for untrusted component execution.
//!
//! The proxy is the only value seen by `ExtensionHost`. A dedicated worker
//! creates and exclusively owns the Wasmtime `Store` and component instance;
//! the two sides exchange only owned extension-protocol values through a
//! bounded mailbox. There is deliberately no `unsafe impl Send` anywhere.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex},
    thread,
};

use key_extension_api::{
    CapabilityScope, DataValue, EventEnvelope, EventSource, EventSubscription, ExtensionError,
    ExtensionErrorCode, ExtensionEvent, ExtensionUpdate, GenerationId, LifecycleEvent,
};
use key_extension_host::{
    ActivationContext, DeferredCall, DeferredNativeUpdate, NativeExtension, NativeUpdate,
    UpdateDelivery,
};

use crate::{WasmRuntime, host_adapter::extension_error_from_diagnostic};

pub(crate) struct WasmWorkerExtension {
    subscriptions: Vec<EventSubscription>,
    shared: Arc<SharedMailbox>,
    host_generation: Option<GenerationId>,
    active: bool,
    suspended: bool,
    closed: bool,
}

impl WasmWorkerExtension {
    pub(crate) fn spawn(
        runtime: WasmRuntime,
        component_bytes: Arc<[u8]>,
        subscriptions: Vec<EventSubscription>,
    ) -> Result<Self, ExtensionError> {
        let shared = Arc::new(SharedMailbox::new(
            runtime.limits().maximum_pending_events,
            runtime.limits().maximum_pending_results,
            runtime.limits().maximum_input_bytes,
        ));
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("key-wasm-component".into())
            .spawn(move || worker_loop(runtime, component_bytes, worker_shared))
            .map_err(|error| ExtensionError {
                code: ExtensionErrorCode::Internal,
                message: format!("could not start WebAssembly component worker: {error}"),
                retryable: true,
            })?;
        Ok(Self {
            subscriptions,
            shared,
            host_generation: None,
            active: false,
            suspended: false,
            closed: false,
        })
    }

    fn enqueue_event(&self, event: EventEnvelope) -> Result<(), ExtensionError> {
        let mut mailbox = self.shared.lock();
        if mailbox.closed {
            return Err(unavailable("WebAssembly component worker is closed"));
        }
        let lifecycle = mailbox.lifecycle;
        let work = mailbox.work;
        let retained_bytes = retained_event_bytes(&event);
        if retained_bytes > mailbox.maximum_input_bytes {
            return Err(ExtensionError {
                code: ExtensionErrorCode::QuotaExceeded,
                message: format!(
                    "WebAssembly event requires approximately {retained_bytes} bytes; input limit is {} bytes",
                    mailbox.maximum_input_bytes
                ),
                retryable: false,
            });
        }
        let command = WorkerCommand::Event {
            lifecycle,
            work,
            host_generation: self
                .host_generation
                .expect("an active proxy has a host generation"),
            event,
        };

        if let Some(key) = command.coalescing_key()
            && let Some(index) = trailing_command_match(&mailbox.commands, lifecycle, work, &key)
        {
            mailbox.commands.remove(index);
            mailbox.commands.push_back(command);
            self.shared.ready.notify_one();
            return Ok(());
        }

        let pending_events = mailbox
            .commands
            .iter()
            .filter(|pending| matches!(pending, WorkerCommand::Event { .. }))
            .count();
        if pending_events >= mailbox.maximum_pending_events {
            return Err(ExtensionError {
                code: ExtensionErrorCode::QuotaExceeded,
                message: format!(
                    "WebAssembly event mailbox reached its {} event limit",
                    mailbox.maximum_pending_events
                ),
                retryable: true,
            });
        }
        mailbox.commands.push_back(command);
        self.shared.ready.notify_one();
        Ok(())
    }

    fn begin_lifecycle(&mut self, context: &ActivationContext) -> Result<(), ExtensionError> {
        let mut mailbox = self.shared.lock();
        if mailbox.closed {
            return Err(unavailable("WebAssembly component worker is closed"));
        }
        mailbox.lifecycle = mailbox.lifecycle.saturating_add(1);
        mailbox.work = mailbox.work.saturating_add(1);
        mailbox.commands.clear();
        mailbox.results.clear();
        let lifecycle = mailbox.lifecycle;
        mailbox.commands.push_back(WorkerCommand::Activate {
            lifecycle,
            host_generation: context.generation,
            context: context.clone(),
        });
        self.host_generation = Some(context.generation);
        self.active = true;
        self.suspended = false;
        self.shared.ready.notify_all();
        Ok(())
    }

    fn cancel_event_generation(&self, enqueue_suspend: bool) {
        let mut mailbox = self.shared.lock();
        if mailbox.closed {
            return;
        }
        mailbox.work = mailbox.work.saturating_add(1);
        let lifecycle = mailbox.lifecycle;
        let work = mailbox.work;
        mailbox
            .commands
            .retain(|command| !matches!(command, WorkerCommand::Event { .. }));
        mailbox.results.retain(|result| result.work.is_none());
        if enqueue_suspend {
            mailbox
                .commands
                .push_back(WorkerCommand::Suspend { lifecycle, work });
        }
        self.shared.ready.notify_all();
    }

    fn close_without_waiting(&mut self) {
        if self.closed {
            return;
        }
        let mut mailbox = self.shared.lock();
        mailbox.lifecycle = mailbox.lifecycle.saturating_add(1);
        mailbox.work = mailbox.work.saturating_add(1);
        mailbox.commands.clear();
        mailbox.results.clear();
        mailbox.closed = true;
        let lifecycle = mailbox.lifecycle;
        mailbox
            .commands
            .push_back(WorkerCommand::Unload { lifecycle });
        self.closed = true;
        self.active = false;
        self.suspended = false;
        self.shared.ready.notify_all();
    }
}

impl NativeExtension for WasmWorkerExtension {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        self.subscriptions.clone()
    }

    fn activate(&mut self, context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        self.begin_lifecycle(context)?;
        Ok(NativeUpdate::default())
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError> {
        if !self.active || self.suspended {
            return Err(unavailable("WebAssembly component is not active"));
        }
        self.enqueue_event(event.clone())?;
        Ok(NativeUpdate::default())
    }

    fn update_delivery(&self) -> UpdateDelivery {
        UpdateDelivery::Deferred
    }

    fn drain_deferred_updates(&mut self, maximum: usize) -> Vec<DeferredNativeUpdate> {
        if maximum == 0 {
            return Vec::new();
        }
        let mut mailbox = self.shared.lock();
        let lifecycle = mailbox.lifecycle;
        let work = mailbox.work;
        let mut updates = Vec::with_capacity(maximum.min(mailbox.results.len()));
        while updates.len() < maximum {
            let Some(result) = mailbox.results.pop_front() else {
                break;
            };
            if result.lifecycle == lifecycle
                && result.work.is_none_or(|result_work| result_work == work)
            {
                updates.push(result.update);
            }
        }
        self.shared.ready.notify_all();
        updates
    }

    fn pending_deferred_work(&self) -> usize {
        let mailbox = self.shared.lock();
        if !mailbox.worker_alive {
            return 0;
        }
        mailbox
            .commands
            .len()
            .saturating_add(mailbox.results.len())
            .saturating_add(mailbox.in_flight)
    }

    fn cancel_deferred_events(&mut self) {
        self.cancel_event_generation(false);
    }

    fn suspend(&mut self, _reason: &str) -> Result<(), ExtensionError> {
        if !self.active || self.suspended {
            return Err(unavailable("WebAssembly component is not active"));
        }
        self.cancel_event_generation(true);
        self.suspended = true;
        Ok(())
    }

    fn resume(&mut self, context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        if !self.active || !self.suspended {
            return Err(unavailable("WebAssembly component is not suspended"));
        }
        let mut mailbox = self.shared.lock();
        if mailbox.closed {
            return Err(unavailable("WebAssembly component worker is closed"));
        }
        let lifecycle = mailbox.lifecycle;
        mailbox.commands.push_back(WorkerCommand::Resume {
            lifecycle,
            host_generation: context.generation,
            context: context.clone(),
        });
        self.host_generation = Some(context.generation);
        self.suspended = false;
        self.shared.ready.notify_all();
        Ok(NativeUpdate::default())
    }

    fn unload(&mut self) {
        self.close_without_waiting();
    }
}

impl Drop for WasmWorkerExtension {
    fn drop(&mut self) {
        self.close_without_waiting();
    }
}

struct SharedMailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

impl SharedMailbox {
    fn new(
        maximum_pending_events: usize,
        maximum_pending_results: usize,
        maximum_input_bytes: usize,
    ) -> Self {
        Self {
            state: Mutex::new(MailboxState {
                lifecycle: 0,
                work: 0,
                commands: VecDeque::new(),
                results: VecDeque::new(),
                in_flight: 0,
                maximum_pending_events,
                maximum_pending_results,
                maximum_input_bytes,
                closed: false,
                worker_alive: true,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MailboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct MailboxState {
    lifecycle: u64,
    work: u64,
    commands: VecDeque<WorkerCommand>,
    results: VecDeque<WorkerResult>,
    in_flight: usize,
    maximum_pending_events: usize,
    maximum_pending_results: usize,
    maximum_input_bytes: usize,
    closed: bool,
    worker_alive: bool,
}

enum WorkerCommand {
    Activate {
        lifecycle: u64,
        host_generation: GenerationId,
        context: ActivationContext,
    },
    Event {
        lifecycle: u64,
        work: u64,
        host_generation: GenerationId,
        event: EventEnvelope,
    },
    Suspend {
        lifecycle: u64,
        work: u64,
    },
    Resume {
        lifecycle: u64,
        host_generation: GenerationId,
        context: ActivationContext,
    },
    Unload {
        lifecycle: u64,
    },
}

impl WorkerCommand {
    const fn lifecycle(&self) -> u64 {
        match self {
            Self::Activate { lifecycle, .. }
            | Self::Event { lifecycle, .. }
            | Self::Suspend { lifecycle, .. }
            | Self::Resume { lifecycle, .. }
            | Self::Unload { lifecycle } => *lifecycle,
        }
    }

    const fn work(&self) -> Option<u64> {
        match self {
            Self::Event { work, .. } | Self::Suspend { work, .. } => Some(*work),
            Self::Activate { .. } | Self::Resume { .. } | Self::Unload { .. } => None,
        }
    }

    fn coalescing_key(&self) -> Option<CoalescingKey> {
        let Self::Event { event, .. } = self else {
            return None;
        };
        event_coalescing_key(&event.event)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CoalescingKey {
    Snapshot(key_extension_api::SnapshotKind),
    Capabilities,
}

fn event_coalescing_key(event: &ExtensionEvent) -> Option<CoalescingKey> {
    match event {
        ExtensionEvent::SnapshotChanged { snapshot, .. } => {
            Some(CoalescingKey::Snapshot(*snapshot))
        }
        ExtensionEvent::CapabilitiesChanged { .. } => Some(CoalescingKey::Capabilities),
        _ => None,
    }
}

fn trailing_command_match(
    commands: &VecDeque<WorkerCommand>,
    lifecycle: u64,
    work: u64,
    key: &CoalescingKey,
) -> Option<usize> {
    commands
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, command)| {
            command.lifecycle() == lifecycle
                && command.work() == Some(work)
                && command.coalescing_key().is_some()
        })
        .find_map(|(index, command)| {
            (command.coalescing_key().as_ref() == Some(key)).then_some(index)
        })
}

struct WorkerResult {
    lifecycle: u64,
    work: Option<u64>,
    update: DeferredNativeUpdate,
}

fn worker_loop(runtime: WasmRuntime, component_bytes: Arc<[u8]>, shared: Arc<SharedMailbox>) {
    let mut instance = None;
    loop {
        let command = {
            let mut mailbox = shared.lock();
            while mailbox.commands.is_empty() {
                mailbox = shared
                    .ready
                    .wait(mailbox)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let command = mailbox
                .commands
                .pop_front()
                .expect("non-empty mailbox has a command");
            mailbox.in_flight = mailbox.in_flight.saturating_add(1);
            command
        };
        let exits = matches!(command, WorkerCommand::Unload { .. });
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            execute_worker_command(&runtime, &component_bytes, &mut instance, &command)
        }))
        .unwrap_or_else(|_| panic_result(&command));

        {
            let mut mailbox = shared.lock();
            mailbox.in_flight = mailbox.in_flight.saturating_sub(1);
        }
        if let Some(result) = outcome {
            retain_worker_result(&shared, result);
        }
        if exits {
            break;
        }
    }
    let mut mailbox = shared.lock();
    mailbox.worker_alive = false;
    mailbox.commands.clear();
    mailbox.in_flight = 0;
    shared.ready.notify_all();
}

fn execute_worker_command(
    runtime: &WasmRuntime,
    component_bytes: &[u8],
    instance: &mut Option<crate::WasmExtensionInstance>,
    command: &WorkerCommand,
) -> Option<WorkerResult> {
    match command {
        WorkerCommand::Activate {
            lifecycle,
            host_generation,
            context,
        } => {
            if let Some(previous) = instance.as_mut() {
                let _ = previous.unload();
            }
            *instance = None;
            let result = runtime
                .compile(component_bytes)
                .and_then(|component| component.instantiate())
                .and_then(|mut loaded| {
                    loaded.activate()?;
                    *instance = Some(loaded);
                    Ok(ExtensionUpdate::default())
                })
                .map_err(extension_error_from_diagnostic);
            Some(WorkerResult {
                lifecycle: *lifecycle,
                work: None,
                update: DeferredNativeUpdate {
                    generation: *host_generation,
                    call: DeferredCall::Activation {
                        cause: context.cause,
                    },
                    result,
                },
            })
        }
        WorkerCommand::Event {
            lifecycle,
            work,
            host_generation,
            event,
        } => {
            let result = instance.as_mut().map_or_else(
                || Err(unavailable("WebAssembly component has not activated")),
                |loaded| {
                    loaded
                        .dispatch(event)
                        .map_err(extension_error_from_diagnostic)
                },
            );
            Some(WorkerResult {
                lifecycle: *lifecycle,
                work: Some(*work),
                update: DeferredNativeUpdate {
                    generation: *host_generation,
                    call: DeferredCall::Event(event.clone()),
                    result,
                },
            })
        }
        WorkerCommand::Suspend { .. } => {
            if let Some(loaded) = instance.as_mut() {
                let _ = loaded.suspend();
            }
            None
        }
        WorkerCommand::Resume {
            lifecycle,
            host_generation,
            context,
        } => {
            let result = instance.as_mut().map_or_else(
                || Err(unavailable("WebAssembly component has not activated")),
                |loaded| {
                    loaded
                        .resume()
                        .map(|()| ExtensionUpdate::default())
                        .map_err(extension_error_from_diagnostic)
                },
            );
            Some(WorkerResult {
                lifecycle: *lifecycle,
                work: None,
                update: DeferredNativeUpdate {
                    generation: *host_generation,
                    call: DeferredCall::Resume {
                        cause: context.cause,
                    },
                    result,
                },
            })
        }
        WorkerCommand::Unload { .. } => {
            if let Some(mut loaded) = instance.take() {
                let _ = loaded.unload();
            }
            None
        }
    }
}

fn retain_worker_result(shared: &SharedMailbox, result: WorkerResult) {
    let mut mailbox = shared.lock();
    if let Some(key) = result.coalescing_key()
        && let Some(index) = trailing_result_match(&mailbox.results, &result, &key)
    {
        mailbox.results.remove(index);
        mailbox.results.push_back(result);
        shared.ready.notify_all();
        return;
    }
    while mailbox.results.len() >= mailbox.maximum_pending_results
        && result.lifecycle == mailbox.lifecycle
        && result.work.is_none_or(|work| work == mailbox.work)
        && !mailbox.closed
    {
        mailbox = shared
            .ready
            .wait(mailbox)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    if result.lifecycle == mailbox.lifecycle
        && result.work.is_none_or(|work| work == mailbox.work)
        && !mailbox.closed
    {
        mailbox.results.push_back(result);
    }
    shared.ready.notify_all();
}

fn trailing_result_match(
    results: &VecDeque<WorkerResult>,
    incoming: &WorkerResult,
    key: &CoalescingKey,
) -> Option<usize> {
    results
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, result)| {
            result.lifecycle == incoming.lifecycle
                && result.work == incoming.work
                && result.coalescing_key().is_some()
        })
        .find_map(|(index, result)| {
            (result.coalescing_key().as_ref() == Some(key)).then_some(index)
        })
}

impl WorkerResult {
    fn coalescing_key(&self) -> Option<CoalescingKey> {
        if !matches!(&self.update.result, Ok(update) if update.effects.is_empty()) {
            // Once a guest emitted an effect, the host must either arbitrate it
            // or return its completion. Only state-only latest-wins updates
            // may be replaced after execution.
            return None;
        }
        let DeferredCall::Event(event) = &self.update.call else {
            return None;
        };
        event_coalescing_key(&event.event)
    }
}

fn panic_result(command: &WorkerCommand) -> Option<WorkerResult> {
    let error = Err(ExtensionError {
        code: ExtensionErrorCode::Internal,
        message: "WebAssembly component worker panicked".into(),
        retryable: false,
    });
    match command {
        WorkerCommand::Activate {
            lifecycle,
            host_generation,
            context,
        } => Some(WorkerResult {
            lifecycle: *lifecycle,
            work: None,
            update: DeferredNativeUpdate {
                generation: *host_generation,
                call: DeferredCall::Activation {
                    cause: context.cause,
                },
                result: error,
            },
        }),
        WorkerCommand::Event {
            lifecycle,
            work,
            host_generation,
            event,
        } => Some(WorkerResult {
            lifecycle: *lifecycle,
            work: Some(*work),
            update: DeferredNativeUpdate {
                generation: *host_generation,
                call: DeferredCall::Event(event.clone()),
                result: error,
            },
        }),
        WorkerCommand::Resume {
            lifecycle,
            host_generation,
            context,
        } => Some(WorkerResult {
            lifecycle: *lifecycle,
            work: None,
            update: DeferredNativeUpdate {
                generation: *host_generation,
                call: DeferredCall::Resume {
                    cause: context.cause,
                },
                result: error,
            },
        }),
        WorkerCommand::Suspend { .. } | WorkerCommand::Unload { .. } => None,
    }
}

/// Conservative, allocation-free upper estimate for the JSON transport input.
/// JSON escaping can expand one UTF-8 byte to six ASCII bytes, so arbitrary
/// strings use that multiplier. The fixed allowance covers tags, field names,
/// numeric formatting, causes, and punctuation. This preflight keeps a single
/// enormous but structurally valid `DataValue` from being cloned into the
/// bounded worker mailbox only to be rejected after serialization.
fn retained_event_bytes(envelope: &EventEnvelope) -> usize {
    let mut bytes = 512usize;
    if let EventSource::Extension(extension) = &envelope.source {
        bytes = bytes.saturating_add(extension.as_str().len());
    }
    match &envelope.event {
        ExtensionEvent::Lifecycle { event } => match event {
            LifecycleEvent::Suspended { reason } => {
                bytes = bytes.saturating_add(escaped_string_bound(reason));
            }
            LifecycleEvent::SettingsChanged { keys } => {
                for key in keys {
                    bytes = bytes
                        .saturating_add(16)
                        .saturating_add(escaped_string_bound(key));
                }
            }
            LifecycleEvent::Installed
            | LifecycleEvent::Validated
            | LifecycleEvent::Activated
            | LifecycleEvent::ApplicationReady
            | LifecycleEvent::DocumentOpening { .. }
            | LifecycleEvent::DocumentOpened { .. }
            | LifecycleEvent::DocumentClosing { .. }
            | LifecycleEvent::DocumentClosed { .. }
            | LifecycleEvent::Resumed
            | LifecycleEvent::Upgrading { .. }
            | LifecycleEvent::Unloading => {}
        },
        ExtensionEvent::CommandInvoked { command, payload } => {
            bytes = bytes
                .saturating_add(command.as_str().len())
                .saturating_add(retained_value_bytes(payload));
        }
        ExtensionEvent::CapabilitiesChanged { snapshot } => {
            for grant in &snapshot.granted {
                bytes = bytes
                    .saturating_add(256)
                    .saturating_add(grant.extension.as_str().len())
                    .saturating_add(grant.capability.as_str().len())
                    .saturating_add(match &grant.scope {
                        CapabilityScope::Domains(domains) => {
                            domains.iter().fold(0usize, |total, domain| {
                                total.saturating_add(16).saturating_add(domain.0.len())
                            })
                        }
                        CapabilityScope::Application
                        | CapabilityScope::ActiveDocument
                        | CapabilityScope::DocumentSet
                        | CapabilityScope::NamespacedStorage(_) => 32,
                    });
            }
            for capability in &snapshot.missing_optional {
                bytes = bytes
                    .saturating_add(64)
                    .saturating_add(capability.as_str().len());
            }
        }
        ExtensionEvent::SnapshotChanged { value, .. } => {
            bytes = bytes.saturating_add(retained_value_bytes(value));
        }
        ExtensionEvent::Custom { event, payload } => {
            bytes = bytes
                .saturating_add(event.as_str().len())
                .saturating_add(retained_value_bytes(payload));
        }
        ExtensionEvent::EffectCompleted { effect, result } => {
            bytes = bytes.saturating_add(effect.as_str().len());
            bytes = bytes.saturating_add(match result {
                Ok(value) => retained_value_bytes(value),
                Err(error) => escaped_string_bound(&error.message).saturating_add(64),
            });
        }
        ExtensionEvent::TaskCancelled { task, reason } => {
            bytes = bytes
                .saturating_add(task.as_str().len())
                .saturating_add(escaped_string_bound(reason));
        }
    }
    bytes
}

fn retained_value_bytes(value: &DataValue) -> usize {
    const NODE_OVERHEAD: usize = 64;
    match value {
        DataValue::String(value) => NODE_OVERHEAD.saturating_add(escaped_string_bound(value)),
        DataValue::List(values) => values.iter().fold(NODE_OVERHEAD, |total, value| {
            total.saturating_add(retained_value_bytes(value))
        }),
        DataValue::Record(values) => values.iter().fold(NODE_OVERHEAD, |total, (key, value)| {
            total
                .saturating_add(escaped_string_bound(key))
                .saturating_add(retained_value_bytes(value))
        }),
        DataValue::Null | DataValue::Boolean(_) | DataValue::Integer(_) | DataValue::Number(_) => {
            NODE_OVERHEAD
        }
    }
}

fn escaped_string_bound(value: &str) -> usize {
    value.len().saturating_mul(6).saturating_add(2)
}

fn unavailable(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::Conflict,
        message: message.into(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use key_extension_api::{
        CapabilitySnapshot, CauseContext, CauseId, DataValue, EffectId, EffectRequest, EventSource,
        ExtensionEffect, ExtensionEvent, ExtensionId, GenerationId, NotificationTone, SnapshotKind,
    };

    use super::*;
    use crate::WasmRuntimeLimits;

    fn transport_component() -> Arc<[u8]> {
        Arc::from(
            wat::parse_str(
                r#"
                (component
                    (core module $guest
                        (memory (export "memory") 1)
                        (data (i32.const 8) "[]")
                        (global $heap (mut i32) (i32.const 16))
                        (func (export "cabi_realloc")
                            (param i32 i32 i32 i32) (result i32)
                            global.get $heap
                            global.get $heap
                            local.get 3
                            i32.add
                            global.set $heap)
                        (func (export "activate"))
                        (func (export "deactivate"))
                        (func (export "handle-event")
                            (param i32 i32) (result i32)
                            i32.const 0 i32.const 8 i32.store
                            i32.const 4 i32.const 2 i32.store
                            i32.const 0))
                    (core instance $guest (instantiate $guest))
                    (func $activate (canon lift (core func $guest "activate")))
                    (func $deactivate (canon lift (core func $guest "deactivate")))
                    (func $handle-event (param "event-json" (list u8)) (result (list u8))
                        (canon lift (core func $guest "handle-event")
                            (memory $guest "memory")
                            (realloc (func $guest "cabi_realloc"))))
                    (export "activate" (func $activate))
                    (export "deactivate" (func $deactivate))
                    (export "handle-event" (func $handle-event)))
                "#,
            )
            .unwrap(),
        )
    }

    fn spinning_transport_component() -> Arc<[u8]> {
        Arc::from(
            wat::parse_str(
                r#"
                (component
                    (core module $guest
                        (memory (export "memory") 1)
                        (global $heap (mut i32) (i32.const 16))
                        (func (export "cabi_realloc")
                            (param i32 i32 i32 i32) (result i32)
                            global.get $heap
                            global.get $heap
                            local.get 3
                            i32.add
                            global.set $heap)
                        (func (export "activate"))
                        (func (export "deactivate"))
                        (func (export "handle-event")
                            (param i32 i32) (result i32)
                            (loop $forever br $forever)
                            i32.const 0))
                    (core instance $guest (instantiate $guest))
                    (func $activate (canon lift (core func $guest "activate")))
                    (func $deactivate (canon lift (core func $guest "deactivate")))
                    (func $handle-event (param "event-json" (list u8)) (result (list u8))
                        (canon lift (core func $guest "handle-event")
                            (memory $guest "memory")
                            (realloc (func $guest "cabi_realloc"))))
                    (export "activate" (func $activate))
                    (export "deactivate" (func $deactivate))
                    (export "handle-event" (func $handle-event)))
                "#,
            )
            .unwrap(),
        )
    }

    fn context() -> ActivationContext {
        ActivationContext {
            extension: ExtensionId::parse("org.example.worker").unwrap(),
            generation: GenerationId(7),
            cause: CauseContext {
                id: CauseId::new(0, 1),
                parent: None,
                depth: 0,
            },
            capabilities: CapabilitySnapshot {
                granted: Vec::new(),
                missing_optional: Vec::new(),
            },
        }
    }

    fn wait_for(extension: &mut WasmWorkerExtension, expected: usize) -> Vec<DeferredNativeUpdate> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let updates = extension.drain_deferred_updates(expected);
            if !updates.is_empty() || Instant::now() >= deadline {
                return updates;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "worker did not settle before timeout"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn event(sequence: u64, kind: ExtensionEvent) -> EventEnvelope {
        EventEnvelope {
            cause: context().cause,
            source: EventSource::Host,
            sequence,
            event: kind,
        }
    }

    fn queued_event(sequence: u64, kind: ExtensionEvent) -> WorkerCommand {
        WorkerCommand::Event {
            lifecycle: 1,
            work: 1,
            host_generation: context().generation,
            event: event(sequence, kind),
        }
    }

    #[test]
    fn coalescing_moves_latest_input_forward_without_crossing_event_barriers() {
        let viewport = |sequence| {
            queued_event(
                sequence,
                ExtensionEvent::SnapshotChanged {
                    snapshot: SnapshotKind::Viewport,
                    value: DataValue::Integer(i64::try_from(sequence).unwrap()),
                },
            )
        };
        let selection = queued_event(
            2,
            ExtensionEvent::SnapshotChanged {
                snapshot: SnapshotKind::Selection,
                value: DataValue::Null,
            },
        );
        let mut commands = VecDeque::from([viewport(1), selection]);
        let latest = viewport(3);
        let key = latest.coalescing_key().unwrap();
        let index = trailing_command_match(&commands, 1, 1, &key).unwrap();
        commands.remove(index);
        commands.push_back(latest);
        let sequences = commands
            .iter()
            .filter_map(|command| match command {
                WorkerCommand::Event { event, .. } => Some(event.sequence),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![2, 3]);

        commands.push_back(queued_event(
            4,
            ExtensionEvent::Lifecycle {
                event: key_extension_api::LifecycleEvent::ApplicationReady,
            },
        ));
        let after_barrier = viewport(5);
        assert_eq!(
            trailing_command_match(&commands, 1, 1, &after_barrier.coalescing_key().unwrap()),
            None,
            "a newer snapshot cannot be moved ahead of an intervening command/lifecycle event"
        );
    }

    #[test]
    fn completed_events_with_effects_are_never_coalesced_away() {
        let envelope = event(
            1,
            ExtensionEvent::SnapshotChanged {
                snapshot: SnapshotKind::Viewport,
                value: DataValue::Null,
            },
        );
        let result = WorkerResult {
            lifecycle: 1,
            work: Some(1),
            update: DeferredNativeUpdate {
                generation: context().generation,
                call: DeferredCall::Event(envelope.clone()),
                result: Ok(NativeUpdate::with_effects(vec![EffectRequest {
                    id: EffectId::parse("org.example.worker/notify").unwrap(),
                    cause: envelope.cause,
                    effect: ExtensionEffect::Notify {
                        message: "ready".into(),
                        tone: NotificationTone::Success,
                    },
                }])),
            },
        };
        assert_eq!(result.coalescing_key(), None);
    }

    #[test]
    fn retained_event_estimate_bounds_json_and_rejects_before_queueing() {
        let envelope = event(
            1,
            ExtensionEvent::Custom {
                event: key_extension_api::NamespacedId::parse("org.example.worker/sample").unwrap(),
                payload: DataValue::Record(std::collections::BTreeMap::from([(
                    "content".into(),
                    DataValue::String("\0\n\"\\ sample".repeat(200)),
                )])),
            },
        );
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert!(retained_event_bytes(&envelope) >= encoded.len());

        let runtime = WasmRuntime::new(WasmRuntimeLimits {
            maximum_input_bytes: 4 * 1024,
            ..WasmRuntimeLimits::default()
        })
        .unwrap();
        let mut extension = WasmWorkerExtension::spawn(runtime, transport_component(), vec![])
            .expect("worker starts");
        extension.activate(&context()).unwrap();
        let pending_before = extension.pending_deferred_work();
        let error = extension.handle_event(&envelope).unwrap_err();
        assert_eq!(error.code, ExtensionErrorCode::QuotaExceeded);
        assert_eq!(extension.pending_deferred_work(), pending_before);
        extension.unload();
    }

    #[test]
    fn compilation_and_dispatch_cross_the_worker_boundary() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
        let mut extension = WasmWorkerExtension::spawn(runtime, transport_component(), vec![])
            .expect("worker starts");
        extension.activate(&context()).unwrap();
        assert_eq!(extension.update_delivery(), UpdateDelivery::Deferred);
        assert!(extension.pending_deferred_work() > 0);
        let activation = wait_for(&mut extension, 1);
        assert!(activation[0].result.is_ok());

        extension
            .handle_event(&EventEnvelope {
                cause: context().cause,
                source: EventSource::Host,
                sequence: 2,
                event: ExtensionEvent::SnapshotChanged {
                    snapshot: SnapshotKind::Document,
                    value: DataValue::Null,
                },
            })
            .unwrap();
        let event = wait_for(&mut extension, 1);
        assert!(matches!(event[0].call, DeferredCall::Event(_)));
        assert!(event[0].result.is_ok());
        extension.unload();
    }

    #[test]
    fn latest_snapshot_replaces_queued_snapshot_and_document_cancel_drops_it() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
        let mut extension = WasmWorkerExtension::spawn(runtime, transport_component(), vec![])
            .expect("worker starts");
        extension.activate(&context()).unwrap();
        let activation = wait_for(&mut extension, 1);
        assert!(activation[0].result.is_ok());
        for sequence in 1..=20 {
            extension
                .handle_event(&event(
                    sequence,
                    ExtensionEvent::SnapshotChanged {
                        snapshot: SnapshotKind::Viewport,
                        value: DataValue::Integer(i64::try_from(sequence).unwrap()),
                    },
                ))
                .unwrap();
        }
        wait_until(|| {
            let mailbox = extension.shared.lock();
            mailbox.commands.is_empty() && mailbox.in_flight == 0
        });
        let snapshots = extension.drain_deferred_updates(32);
        assert_eq!(
            snapshots.len(),
            1,
            "completed snapshots are latest-wins too"
        );
        assert!(matches!(
            &snapshots[0].call,
            DeferredCall::Event(EventEnvelope { sequence: 20, .. })
        ));

        extension
            .handle_event(&event(
                21,
                ExtensionEvent::SnapshotChanged {
                    snapshot: SnapshotKind::Viewport,
                    value: DataValue::Integer(21),
                },
            ))
            .unwrap();
        extension.cancel_deferred_events();
        wait_until(|| extension.pending_deferred_work() == 0);
        assert!(extension.drain_deferred_updates(32).is_empty());
        extension.unload();
    }

    #[test]
    fn invalid_component_failure_is_deferred_from_activation() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits::default()).unwrap();
        let mut extension =
            WasmWorkerExtension::spawn(runtime, Arc::<[u8]>::from(&b"not a component"[..]), vec![])
                .expect("worker starts before validation");
        assert!(
            extension.activate(&context()).is_ok(),
            "activation callback only enqueues immutable work"
        );
        let completed = wait_for(&mut extension, 1);
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].result.as_ref().unwrap_err().code,
            ExtensionErrorCode::InvalidRequest
        );
        extension.unload();
    }

    #[test]
    fn trapping_guest_is_contained_as_a_deferred_error() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits {
            fuel_per_invocation: 10_000,
            epoch_ticks_per_invocation: 1_000,
            epoch_tick_interval: Duration::from_secs(1),
            ..WasmRuntimeLimits::default()
        })
        .unwrap();
        let mut extension =
            WasmWorkerExtension::spawn(runtime, spinning_transport_component(), vec![])
                .expect("worker starts");
        extension.activate(&context()).unwrap();
        assert!(wait_for(&mut extension, 1)[0].result.is_ok());
        extension
            .handle_event(&event(
                1,
                ExtensionEvent::Lifecycle {
                    event: key_extension_api::LifecycleEvent::ApplicationReady,
                },
            ))
            .unwrap();
        let completed = wait_for(&mut extension, 1);
        let error = completed[0].result.as_ref().unwrap_err();
        assert_eq!(error.code, ExtensionErrorCode::QuotaExceeded);
        assert!(error.message.contains("FuelExhausted"));
        extension.unload();
    }

    #[test]
    fn event_mailbox_is_bounded_without_blocking_the_caller() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits {
            maximum_pending_events: 2,
            fuel_per_invocation: u64::MAX,
            epoch_ticks_per_invocation: 50,
            epoch_tick_interval: Duration::from_millis(2),
            ..WasmRuntimeLimits::default()
        })
        .unwrap();
        let mut extension =
            WasmWorkerExtension::spawn(runtime, spinning_transport_component(), vec![])
                .expect("worker starts");
        extension.activate(&context()).unwrap();
        assert!(wait_for(&mut extension, 1)[0].result.is_ok());
        let lifecycle = || ExtensionEvent::Lifecycle {
            event: key_extension_api::LifecycleEvent::ApplicationReady,
        };
        extension.handle_event(&event(1, lifecycle())).unwrap();
        wait_until(|| extension.shared.lock().in_flight == 1);
        extension.handle_event(&event(2, lifecycle())).unwrap();
        extension.handle_event(&event(3, lifecycle())).unwrap();
        let error = extension.handle_event(&event(4, lifecycle())).unwrap_err();
        assert_eq!(error.code, ExtensionErrorCode::QuotaExceeded);
        extension.cancel_deferred_events();
        extension.unload();
    }

    #[test]
    fn unload_is_non_blocking_and_worker_releases_eventually() {
        let runtime = WasmRuntime::new(WasmRuntimeLimits {
            fuel_per_invocation: u64::MAX,
            epoch_ticks_per_invocation: 50,
            epoch_tick_interval: Duration::from_millis(2),
            ..WasmRuntimeLimits::default()
        })
        .unwrap();
        let mut extension =
            WasmWorkerExtension::spawn(runtime, spinning_transport_component(), vec![])
                .expect("worker starts");
        extension.activate(&context()).unwrap();
        assert!(wait_for(&mut extension, 1)[0].result.is_ok());
        extension
            .handle_event(&event(
                1,
                ExtensionEvent::Lifecycle {
                    event: key_extension_api::LifecycleEvent::ApplicationReady,
                },
            ))
            .unwrap();
        wait_until(|| extension.shared.lock().in_flight == 1);
        let started = Instant::now();
        extension.unload();
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "unload must only revoke the mailbox, not join guest execution"
        );
        wait_until(|| !extension.shared.lock().worker_alive);
        assert_eq!(extension.pending_deferred_work(), 0);
        assert!(extension.drain_deferred_updates(32).is_empty());
    }
}
