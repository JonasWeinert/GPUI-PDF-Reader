//! Host-owned execution for semantic storage and background-task effects.
//!
//! Extensions only describe effects. This module routes those effects to
//! injected, namespaced services and never exposes filesystem, thread, or
//! runtime handles to extension code.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
    thread,
};

use key_extension_api::{
    DataValue, EffectResult, ExtensionEffect, ExtensionError, ExtensionErrorCode, ExtensionId,
    GenerationId, StorageArea, TaskId,
};

use crate::ArbitratedEffect;

/// The outcome of routing one already-arbitrated host effect.
#[derive(Clone, Debug, PartialEq)]
pub enum ServiceDispatch {
    /// The service completed without leaving the caller's thread.
    Immediate(EffectResult),
    /// A bounded background task owns completion of the effect.
    Deferred,
    /// This effect belongs to the application or another capability provider.
    Unsupported,
}

/// One background completion ready to be returned to [`crate::ExtensionHost`].
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceCompletion {
    pub effect: ArbitratedEffect,
    pub result: EffectResult,
}

/// Cancellation authority retained by the host, never by an extension.
#[derive(Clone, Debug)]
pub struct TaskCancellation {
    cancelled: Arc<AtomicBool>,
}

impl TaskCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Immutable identity attached by the host to a semantic task invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskContext {
    pub extension: ExtensionId,
    pub generation: GenerationId,
    pub task: TaskId,
}

/// A host-registered task operation. Operation names select handlers; guest
/// input remains a bounded [`DataValue`].
pub trait ExtensionTaskHandler: Send + Sync + 'static {
    fn run(
        &self,
        context: TaskContext,
        input: DataValue,
        cancellation: TaskCancellation,
    ) -> EffectResult;
}

impl<F> ExtensionTaskHandler for F
where
    F: Fn(TaskContext, DataValue, TaskCancellation) -> EffectResult + Send + Sync + 'static,
{
    fn run(
        &self,
        context: TaskContext,
        input: DataValue,
        cancellation: TaskCancellation,
    ) -> EffectResult {
        self(context, input, cancellation)
    }
}

/// Namespaced storage implementation selected by the application shell.
pub trait ExtensionStorage: Send + Sync + 'static {
    fn get(&self, extension: &ExtensionId, area: StorageArea, key: &str) -> EffectResult;

    fn put(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
        value: DataValue,
        quota_bytes: u64,
    ) -> EffectResult;

    fn delete(&self, extension: &ExtensionId, area: StorageArea, key: &str) -> EffectResult;

    fn clear_area(&self, extension: &ExtensionId, area: StorageArea);
}

/// In-memory reference store used by embedders that do not inject persistence.
/// It still enforces extension/area namespaces and declared byte quotas.
#[derive(Default)]
pub struct MemoryExtensionStorage {
    values: Mutex<BTreeMap<(ExtensionId, u8, String), DataValue>>,
}

impl ExtensionStorage for MemoryExtensionStorage {
    fn get(&self, extension: &ExtensionId, area: StorageArea, key: &str) -> EffectResult {
        validate_storage_key(key)?;
        Ok(self
            .values
            .lock()
            .map_err(|_| internal_error("extension storage lock is poisoned"))?
            .get(&(extension.clone(), area_key(area), key.to_owned()))
            .cloned()
            .unwrap_or(DataValue::Null))
    }

    fn put(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
        value: DataValue,
        quota_bytes: u64,
    ) -> EffectResult {
        validate_storage_key(key)?;
        let mut values = self
            .values
            .lock()
            .map_err(|_| internal_error("extension storage lock is poisoned"))?;
        let namespace = (extension.clone(), area_key(area));
        let replacement_bytes = data_value_size(&value);
        let current_bytes = values
            .iter()
            .filter(|((owner, stored_area, _), _)| {
                owner == &namespace.0 && *stored_area == namespace.1
            })
            .map(|((_, _, stored_key), stored_value)| {
                stored_key
                    .len()
                    .saturating_add(data_value_size(stored_value))
            })
            .sum::<usize>();
        let previous_bytes = values
            .get(&(extension.clone(), namespace.1, key.to_owned()))
            .map_or(0, data_value_size);
        let next_bytes = current_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(key.len())
            .saturating_add(replacement_bytes);
        if u64::try_from(next_bytes).unwrap_or(u64::MAX) > quota_bytes {
            return Err(ExtensionError {
                code: ExtensionErrorCode::QuotaExceeded,
                message: format!(
                    "extension storage would use {next_bytes} bytes; quota is {quota_bytes}"
                ),
                retryable: false,
            });
        }
        values.insert((extension.clone(), namespace.1, key.to_owned()), value);
        Ok(DataValue::Null)
    }

    fn delete(&self, extension: &ExtensionId, area: StorageArea, key: &str) -> EffectResult {
        validate_storage_key(key)?;
        self.values
            .lock()
            .map_err(|_| internal_error("extension storage lock is poisoned"))?
            .remove(&(extension.clone(), area_key(area), key.to_owned()));
        Ok(DataValue::Null)
    }

    fn clear_area(&self, extension: &ExtensionId, area: StorageArea) {
        if let Ok(mut values) = self.values.lock() {
            let area = area_key(area);
            values.retain(|(owner, stored_area, _), _| owner != extension || *stored_area != area);
        }
    }
}

struct ActiveTask {
    effect: ArbitratedEffect,
    cancellation: TaskCancellation,
}

struct BackgroundCompletion {
    extension: ExtensionId,
    task: TaskId,
    effect: ArbitratedEffect,
    result: EffectResult,
}

/// Bounded semantic service router owned by the trusted host integration.
pub struct HostServiceRouter {
    storage: Arc<dyn ExtensionStorage>,
    handlers: BTreeMap<String, Arc<dyn ExtensionTaskHandler>>,
    active_tasks: BTreeMap<(ExtensionId, TaskId), ActiveTask>,
    maximum_active_tasks: usize,
    completion_tx: Sender<BackgroundCompletion>,
    completion_rx: Receiver<BackgroundCompletion>,
}

impl Default for HostServiceRouter {
    fn default() -> Self {
        Self::new(Arc::new(MemoryExtensionStorage::default()), 16)
    }
}

impl HostServiceRouter {
    #[must_use]
    pub fn new(storage: Arc<dyn ExtensionStorage>, maximum_active_tasks: usize) -> Self {
        let (completion_tx, completion_rx) = channel();
        Self {
            storage,
            handlers: BTreeMap::new(),
            active_tasks: BTreeMap::new(),
            maximum_active_tasks: maximum_active_tasks.max(1),
            completion_tx,
            completion_rx,
        }
    }

    pub fn register_task_handler(
        &mut self,
        operation: impl Into<String>,
        handler: impl ExtensionTaskHandler,
    ) -> Result<(), ExtensionError> {
        let operation = operation.into();
        if operation.is_empty() || operation.len() > 128 || !operation.is_ascii() {
            return Err(invalid_error("task operation must be bounded ASCII"));
        }
        if self.handlers.insert(operation, Arc::new(handler)).is_some() {
            return Err(ExtensionError {
                code: ExtensionErrorCode::Conflict,
                message: "task operation is already registered".into(),
                retryable: false,
            });
        }
        Ok(())
    }

    pub fn dispatch(&mut self, effect: &ArbitratedEffect, storage_quota: u64) -> ServiceDispatch {
        match &effect.request.effect {
            ExtensionEffect::StorageGet { area, key } => {
                ServiceDispatch::Immediate(self.storage.get(&effect.extension, *area, key))
            }
            ExtensionEffect::StoragePut { area, key, value } => ServiceDispatch::Immediate(
                self.storage
                    .put(&effect.extension, *area, key, value.clone(), storage_quota),
            ),
            ExtensionEffect::StorageDelete { area, key } => {
                ServiceDispatch::Immediate(self.storage.delete(&effect.extension, *area, key))
            }
            ExtensionEffect::StartTask {
                task,
                operation,
                input,
            } => self.start_task(effect, task, operation, input.clone()),
            ExtensionEffect::CancelTask { task } => {
                let key = (effect.extension.clone(), task.clone());
                if let Some(active) = self.active_tasks.get(&key) {
                    active.cancellation.cancel();
                }
                ServiceDispatch::Immediate(Ok(DataValue::Null))
            }
            _ => ServiceDispatch::Unsupported,
        }
    }

    fn start_task(
        &mut self,
        effect: &ArbitratedEffect,
        task: &TaskId,
        operation: &str,
        input: DataValue,
    ) -> ServiceDispatch {
        if self.active_tasks.len() >= self.maximum_active_tasks {
            return ServiceDispatch::Immediate(Err(ExtensionError {
                code: ExtensionErrorCode::QuotaExceeded,
                message: "extension background-task limit reached".into(),
                retryable: true,
            }));
        }
        let key = (effect.extension.clone(), task.clone());
        if self.active_tasks.contains_key(&key) {
            return ServiceDispatch::Immediate(Err(ExtensionError {
                code: ExtensionErrorCode::Conflict,
                message: "task ID is already active".into(),
                retryable: false,
            }));
        }
        let Some(handler) = self.handlers.get(operation).cloned() else {
            return ServiceDispatch::Immediate(Err(ExtensionError {
                code: ExtensionErrorCode::CapabilityUnavailable,
                message: format!("task operation {operation:?} is not provided by this host"),
                retryable: false,
            }));
        };
        let cancellation = TaskCancellation::new();
        let thread_cancellation = cancellation.clone();
        let completion_tx = self.completion_tx.clone();
        let effect_for_thread = effect.clone();
        let context = TaskContext {
            extension: effect.extension.clone(),
            generation: effect.generation,
            task: task.clone(),
        };
        let completion_extension = effect.extension.clone();
        let completion_task = task.clone();
        let spawn = thread::Builder::new()
            .name(format!("key-extension-task-{operation}"))
            .spawn(move || {
                let result = if thread_cancellation.is_cancelled() {
                    Err(cancelled_error())
                } else {
                    handler.run(context, input, thread_cancellation)
                };
                let _ = completion_tx.send(BackgroundCompletion {
                    extension: completion_extension,
                    task: completion_task,
                    effect: effect_for_thread,
                    result,
                });
            });
        if let Err(error) = spawn {
            return ServiceDispatch::Immediate(Err(ExtensionError {
                code: ExtensionErrorCode::TemporarilyUnavailable,
                message: format!("could not start extension task: {error}"),
                retryable: true,
            }));
        }
        self.active_tasks.insert(
            key,
            ActiveTask {
                effect: effect.clone(),
                cancellation,
            },
        );
        ServiceDispatch::Deferred
    }

    #[must_use]
    pub fn poll_ready(&mut self, maximum: usize) -> Vec<ServiceCompletion> {
        let mut ready = Vec::new();
        while ready.len() < maximum {
            let completion = match self.completion_rx.try_recv() {
                Ok(completion) => completion,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let key = (completion.extension, completion.task);
            let Some(active) = self.active_tasks.remove(&key) else {
                continue;
            };
            if active.effect.generation != completion.effect.generation
                || active.effect.request.id != completion.effect.request.id
            {
                continue;
            }
            ready.push(ServiceCompletion {
                effect: completion.effect,
                result: completion.result,
            });
        }
        ready
    }

    pub fn cancel_extension(&mut self, extension: &ExtensionId) {
        self.active_tasks.retain(|(owner, _), active| {
            if owner == extension {
                active.cancellation.cancel();
                false
            } else {
                true
            }
        });
        self.storage
            .clear_area(extension, StorageArea::EphemeralCache);
    }

    pub fn cancel_all(&mut self) {
        for active in self.active_tasks.values() {
            active.cancellation.cancel();
        }
        self.active_tasks.clear();
    }

    #[must_use]
    pub fn active_task_count(&self) -> usize {
        self.active_tasks.len()
    }
}

fn area_key(area: StorageArea) -> u8 {
    match area {
        StorageArea::Settings => 0,
        StorageArea::Document => 1,
        StorageArea::EphemeralCache => 2,
    }
}

fn validate_storage_key(key: &str) -> Result<(), ExtensionError> {
    if key.is_empty()
        || key.len() > 256
        || !key.is_ascii()
        || key.starts_with('.')
        || key.contains("..")
        || key.contains(['/', '\\'])
    {
        Err(invalid_error("storage key is not a bounded logical key"))
    } else {
        Ok(())
    }
}

fn data_value_size(value: &DataValue) -> usize {
    match value {
        DataValue::Null => 1,
        DataValue::Boolean(_) => 1,
        DataValue::Integer(_) | DataValue::Number(_) => 8,
        DataValue::String(value) => value.len(),
        DataValue::List(values) => values.iter().map(data_value_size).sum(),
        DataValue::Record(values) => values
            .iter()
            .map(|(key, value)| key.len().saturating_add(data_value_size(value)))
            .sum(),
    }
}

fn invalid_error(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::InvalidRequest,
        message: message.into(),
        retryable: false,
    }
}

fn internal_error(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::Internal,
        message: message.into(),
        retryable: true,
    }
}

fn cancelled_error() -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::Cancelled,
        message: "extension task was cancelled".into(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use key_extension_api::{CauseContext, CauseId, EffectId, EffectRequest, TaskId};

    use super::*;

    fn extension() -> ExtensionId {
        ExtensionId::parse("org.example.services").unwrap()
    }

    fn effect(id: &str, effect: ExtensionEffect) -> ArbitratedEffect {
        ArbitratedEffect {
            extension: extension(),
            generation: GenerationId(4),
            request: EffectRequest {
                id: EffectId::parse(format!("org.example.services/{id}")).unwrap(),
                cause: CauseContext {
                    id: CauseId::new(0, 1),
                    parent: None,
                    depth: 0,
                },
                effect,
            },
            coalesced: Vec::new(),
        }
    }

    #[test]
    fn storage_is_namespaced_and_enforces_quota() {
        let mut router = HostServiceRouter::default();
        let put = effect(
            "put",
            ExtensionEffect::StoragePut {
                area: StorageArea::Settings,
                key: "accent".into(),
                value: DataValue::String("blue".into()),
            },
        );
        assert_eq!(
            router.dispatch(&put, 32),
            ServiceDispatch::Immediate(Ok(DataValue::Null))
        );
        let get = effect(
            "get",
            ExtensionEffect::StorageGet {
                area: StorageArea::Settings,
                key: "accent".into(),
            },
        );
        assert_eq!(
            router.dispatch(&get, 32),
            ServiceDispatch::Immediate(Ok(DataValue::String("blue".into())))
        );
        let oversized = effect(
            "oversized",
            ExtensionEffect::StoragePut {
                area: StorageArea::Settings,
                key: "large".into(),
                value: DataValue::String("x".repeat(128)),
            },
        );
        assert!(matches!(
            router.dispatch(&oversized, 32),
            ServiceDispatch::Immediate(Err(ExtensionError {
                code: ExtensionErrorCode::QuotaExceeded,
                ..
            }))
        ));
    }

    #[test]
    fn task_handlers_are_bounded_polled_and_generation_scoped() {
        let mut router = HostServiceRouter::default();
        router
            .register_task_handler(
                "echo",
                |context: TaskContext, input: DataValue, cancellation: TaskCancellation| {
                    assert_eq!(context.generation, GenerationId(4));
                    assert!(!cancellation.is_cancelled());
                    Ok(input)
                },
            )
            .unwrap();
        let start = effect(
            "start",
            ExtensionEffect::StartTask {
                task: TaskId::parse("org.example.services/echo").unwrap(),
                operation: "echo".into(),
                input: DataValue::String("ready".into()),
            },
        );
        assert_eq!(router.dispatch(&start, 0), ServiceDispatch::Deferred);
        let mut ready = Vec::new();
        for _ in 0..100 {
            ready = router.poll_ready(1);
            if !ready.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].result, Ok(DataValue::String("ready".into())));
        assert_eq!(router.active_task_count(), 0);
    }

    #[test]
    fn cancellation_drops_late_task_results() {
        let mut router = HostServiceRouter::default();
        router
            .register_task_handler(
                "wait",
                |_context, _input, cancellation: TaskCancellation| {
                    while !cancellation.is_cancelled() {
                        thread::yield_now();
                    }
                    Err(cancelled_error())
                },
            )
            .unwrap();
        let start = effect(
            "start-wait",
            ExtensionEffect::StartTask {
                task: TaskId::parse("org.example.services/wait").unwrap(),
                operation: "wait".into(),
                input: DataValue::Null,
            },
        );
        assert_eq!(router.dispatch(&start, 0), ServiceDispatch::Deferred);
        router.cancel_extension(&extension());
        for _ in 0..100 {
            if router.poll_ready(8).is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(router.active_task_count(), 0);
        assert!(router.poll_ready(8).is_empty());
    }
}
