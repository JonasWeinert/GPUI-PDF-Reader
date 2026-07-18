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
    EventEnvelope, EventSubscription, ExtensionError, ExtensionErrorCode, ExtensionEvent,
    ExtensionUpdate, GenerationId,
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
        let command = WorkerCommand::Event {
            lifecycle,
            work,
            host_generation: self
                .host_generation
                .expect("an active proxy has a host generation"),
            event,
        };

        if let Some(key) = command.coalescing_key()
            && let Some(existing) = mailbox.commands.iter_mut().rev().find(|pending| {
                pending.lifecycle() == lifecycle
                    && pending.work() == Some(work)
                    && pending.coalescing_key().as_ref() == Some(&key)
            })
        {
            *existing = command;
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
    fn new(maximum_pending_events: usize, maximum_pending_results: usize) -> Self {
        Self {
            state: Mutex::new(MailboxState {
                lifecycle: 0,
                work: 0,
                commands: VecDeque::new(),
                results: VecDeque::new(),
                in_flight: 0,
                maximum_pending_events,
                maximum_pending_results,
                closed: false,
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
    closed: bool,
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
        match &event.event {
            ExtensionEvent::SnapshotChanged { snapshot, .. } => {
                Some(CoalescingKey::Snapshot(*snapshot))
            }
            ExtensionEvent::CapabilitiesChanged { .. } => Some(CoalescingKey::Capabilities),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CoalescingKey {
    Snapshot(key_extension_api::SnapshotKind),
    Capabilities,
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
            execute_worker_command(&runtime, &component_bytes, &mut instance, command)
        }))
        .unwrap_or_else(|_| {
            Some(WorkerResult {
                lifecycle: current_lifecycle(&shared),
                work: None,
                update: DeferredNativeUpdate {
                    generation: GenerationId(0),
                    call: DeferredCall::Activation {
                        cause: key_extension_api::CauseContext {
                            id: key_extension_api::CauseId::new(0, 0),
                            parent: None,
                            depth: 0,
                        },
                    },
                    result: Err(ExtensionError {
                        code: ExtensionErrorCode::Internal,
                        message: "WebAssembly component worker panicked".into(),
                        retryable: false,
                    }),
                },
            })
        });

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
}

fn execute_worker_command(
    runtime: &WasmRuntime,
    component_bytes: &[u8],
    instance: &mut Option<crate::WasmExtensionInstance>,
    command: WorkerCommand,
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
                lifecycle,
                work: None,
                update: DeferredNativeUpdate {
                    generation: host_generation,
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
                        .dispatch(&event)
                        .map_err(extension_error_from_diagnostic)
                },
            );
            Some(WorkerResult {
                lifecycle,
                work: Some(work),
                update: DeferredNativeUpdate {
                    generation: host_generation,
                    call: DeferredCall::Event(event),
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
                lifecycle,
                work: None,
                update: DeferredNativeUpdate {
                    generation: host_generation,
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
        && (!mailbox.closed || matches!(result.update.call, DeferredCall::Activation { .. }))
    {
        mailbox.results.push_back(result);
    }
    shared.ready.notify_all();
}

fn current_lifecycle(shared: &SharedMailbox) -> u64 {
    shared.lock().lifecycle
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
        CapabilitySnapshot, CauseContext, CauseId, DataValue, EventSource, ExtensionEvent,
        ExtensionId, GenerationId, SnapshotKind,
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
        for sequence in 1..=20 {
            extension
                .handle_event(&EventEnvelope {
                    cause: context().cause,
                    source: EventSource::Host,
                    sequence,
                    event: ExtensionEvent::SnapshotChanged {
                        snapshot: SnapshotKind::Viewport,
                        value: DataValue::Integer(i64::try_from(sequence).unwrap()),
                    },
                })
                .unwrap();
        }
        assert!(extension.pending_deferred_work() <= 3);
        extension.cancel_deferred_events();
        let _ = wait_for(&mut extension, 4);
        assert!(
            extension
                .drain_deferred_updates(32)
                .into_iter()
                .all(|update| !matches!(update.call, DeferredCall::Event(_)))
        );
        extension.unload();
    }
}
