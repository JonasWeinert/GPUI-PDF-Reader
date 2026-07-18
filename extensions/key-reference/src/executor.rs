//! Process-level bounded execution and cache accounting for reference work.

use key_safe_http::{CancellationSource, CancellationToken, DocumentCache, DocumentCacheUsage};
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};

const DEFAULT_WORKERS: usize = 4;
const DEFAULT_QUEUE_CAPACITY: usize = 64;
const DEFAULT_MEMORY_CACHE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_FILE_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_WORKERS: usize = 16;
const MAX_QUEUE_CAPACITY: usize = 4_096;

type Task = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ScopeId(u64);

/// Hard process-level limits for reference-preview background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceExecutorConfig {
    pub worker_count: usize,
    pub queue_capacity: usize,
    pub cache_memory_bytes: usize,
    pub cache_file_bytes: usize,
}

impl Default for ReferenceExecutorConfig {
    fn default() -> Self {
        Self {
            worker_count: DEFAULT_WORKERS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            cache_memory_bytes: DEFAULT_MEMORY_CACHE_BYTES,
            cache_file_bytes: DEFAULT_FILE_CACHE_BYTES,
        }
    }
}

impl ReferenceExecutorConfig {
    fn validate(self) -> Result<Self, String> {
        if self.worker_count == 0 || self.worker_count > MAX_WORKERS {
            return Err(format!(
                "reference worker count must be between 1 and {MAX_WORKERS}"
            ));
        }
        if self.queue_capacity == 0 || self.queue_capacity > MAX_QUEUE_CAPACITY {
            return Err(format!(
                "reference queue capacity must be between 1 and {MAX_QUEUE_CAPACITY}"
            ));
        }
        Ok(self)
    }
}

/// A cheap diagnostic snapshot for application resource coordinators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferenceExecutorSnapshot {
    pub worker_count: usize,
    pub active_jobs: usize,
    pub queued_jobs: usize,
    pub rejected_jobs: u64,
    pub cache_entries: usize,
    pub cache_memory_bytes: usize,
    pub cache_file_bytes: usize,
}

/// Cloneable process-level service shared by website and scholarly fetchers.
///
/// Jobs are dispatched round-robin by document scope, so one PDF cannot fill
/// a FIFO queue ahead of every other open document. Submission is nonblocking:
/// callers receive `false` when the bounded queue is full and can surface a
/// terminal unavailable state rather than hanging indefinitely.
#[derive(Clone)]
pub struct ReferenceExecutor {
    inner: Arc<ExecutorInner>,
}

impl ReferenceExecutor {
    /// Returns the lazily created process-wide service used by compatibility
    /// constructors such as `LinkPreviewFetcher::new()`.
    #[must_use]
    pub fn global() -> Self {
        static GLOBAL: OnceLock<ReferenceExecutor> = OnceLock::new();
        GLOBAL
            .get_or_init(|| {
                Self::new(ReferenceExecutorConfig::default())
                    .expect("the default reference executor configuration is valid")
            })
            .clone()
    }

    pub fn new(config: ReferenceExecutorConfig) -> Result<Self, String> {
        let config = config.validate()?;
        let queue = Arc::new(FairQueue::new(config.queue_capacity));
        let active_jobs = Arc::new(AtomicUsize::new(0));
        let cache = Arc::new(CacheLedger::new(
            config.cache_memory_bytes,
            config.cache_file_bytes,
        ));
        let mut workers = Vec::with_capacity(config.worker_count);
        for index in 0..config.worker_count {
            let worker_queue = Arc::clone(&queue);
            let worker_active = Arc::clone(&active_jobs);
            let worker_cache = Arc::clone(&cache);
            match thread::Builder::new()
                .name(format!("reference-worker-{index}"))
                .spawn(move || worker_loop(worker_queue, worker_active, worker_cache))
            {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    queue.close();
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(format!("could not start reference worker: {error}"));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(ExecutorInner {
                queue,
                active_jobs,
                cache,
                next_scope: AtomicU64::new(1),
                workers: Mutex::new(workers),
                worker_count: config.worker_count,
            }),
        })
    }

    /// Creates one cancellation and fairness domain for an open document.
    #[must_use]
    pub fn document_scope(&self) -> ReferenceDocumentScope {
        let id = ScopeId(self.inner.next_scope.fetch_add(1, Ordering::Relaxed));
        ReferenceDocumentScope {
            lease: Arc::new(ScopeLease {
                executor: self.clone(),
                id,
                state: Arc::new(Mutex::new(GenerationState {
                    generation: None,
                    cancellation: CancellationSource::new(),
                })),
            }),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ReferenceExecutorSnapshot {
        let queue = self.inner.queue.snapshot();
        let cache = self.inner.cache.usage();
        ReferenceExecutorSnapshot {
            worker_count: self.inner.worker_count,
            active_jobs: self.inner.active_jobs.load(Ordering::Acquire),
            queued_jobs: queue.queued,
            rejected_jobs: queue.rejected,
            cache_entries: cache.entries,
            cache_memory_bytes: cache.memory_bytes,
            cache_file_bytes: cache.file_bytes,
        }
    }

    fn submit(&self, scope: ScopeId, task: Task) -> bool {
        self.inner.queue.submit(Job { scope, task })
    }

    fn cancel_scope(&self, scope: ScopeId) {
        self.inner.queue.cancel_scope(scope);
    }

    fn register_cache(&self, scope: ScopeId, cache: &Arc<DocumentCache>) {
        self.inner.cache.register(scope, cache);
    }

    fn purge_scope(&self, scope: ScopeId) {
        self.inner.cache.purge_scope(scope);
    }
}

struct ExecutorInner {
    queue: Arc<FairQueue>,
    active_jobs: Arc<AtomicUsize>,
    cache: Arc<CacheLedger>,
    next_scope: AtomicU64,
    workers: Mutex<Vec<JoinHandle<()>>>,
    worker_count: usize,
}

impl Drop for ExecutorInner {
    fn drop(&mut self) {
        self.queue.close();
        let workers = std::mem::take(
            self.workers
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for worker in workers {
            let _ = worker.join();
        }
    }
}

/// Shared document lifetime used by both fetcher adapters. Dropping the last
/// public handle cancels running work, removes queued work, and purges preview
/// files even though a provider may still be unwinding on a worker thread.
#[derive(Clone)]
pub struct ReferenceDocumentScope {
    lease: Arc<ScopeLease>,
}

impl ReferenceDocumentScope {
    pub fn begin_generation(&self, generation: u64) {
        let changed = {
            let mut state = self
                .lease
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation == Some(generation) {
                false
            } else {
                state.cancellation.cancel();
                state.cancellation = CancellationSource::new();
                state.generation = Some(generation);
                true
            }
        };
        if changed {
            self.lease.executor.cancel_scope(self.lease.id);
            self.lease.executor.purge_scope(self.lease.id);
        }
    }

    pub fn cancel(&self) {
        self.lease.cancel_and_purge();
    }

    fn token_for(&self, generation: u64) -> Option<CancellationToken> {
        let state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.generation == Some(generation)).then(|| state.cancellation.token())
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        let state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.generation == Some(generation) && !state.cancellation.token().is_cancelled()
    }

    pub(crate) fn execute(
        &self,
        generation: u64,
        task: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> bool {
        let Some(cancellation) = self.token_for(generation) else {
            return false;
        };
        self.lease.executor.submit(
            self.lease.id,
            Box::new(move || {
                if !cancellation.is_cancelled() {
                    task(cancellation);
                }
            }),
        )
    }

    pub(crate) fn register_cache(&self, cache: &Arc<DocumentCache>) {
        self.lease.executor.register_cache(self.lease.id, cache);
    }
}

struct ScopeLease {
    executor: ReferenceExecutor,
    id: ScopeId,
    state: Arc<Mutex<GenerationState>>,
}

impl ScopeLease {
    fn cancel_and_purge(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.cancellation.cancel();
        state.generation = None;
        drop(state);
        self.executor.cancel_scope(self.id);
        self.executor.purge_scope(self.id);
    }
}

impl Drop for ScopeLease {
    fn drop(&mut self) {
        self.cancel_and_purge();
    }
}

struct GenerationState {
    generation: Option<u64>,
    cancellation: CancellationSource,
}

struct Job {
    scope: ScopeId,
    task: Task,
}

struct FairQueue {
    capacity: usize,
    state: Mutex<QueueState>,
    available: Condvar,
    rejected: AtomicU64,
}

struct QueueState {
    by_scope: HashMap<ScopeId, VecDeque<Job>>,
    ready_scopes: VecDeque<ScopeId>,
    queued: usize,
    closed: bool,
}

#[derive(Clone, Copy)]
struct QueueSnapshot {
    queued: usize,
    rejected: u64,
}

impl FairQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(QueueState {
                by_scope: HashMap::new(),
                ready_scopes: VecDeque::new(),
                queued: 0,
                closed: false,
            }),
            available: Condvar::new(),
            rejected: AtomicU64::new(0),
        }
    }

    fn submit(&self, job: Job) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed || state.queued >= self.capacity {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let scope = job.scope;
        let queue = state.by_scope.entry(scope).or_default();
        let was_empty = queue.is_empty();
        queue.push_back(job);
        if was_empty {
            state.ready_scopes.push_back(scope);
        }
        state.queued += 1;
        self.available.notify_one();
        true
    }

    fn take(&self) -> Option<Job> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(scope) = state.ready_scopes.pop_front() {
                let (job, has_more) = {
                    let queue = state
                        .by_scope
                        .get_mut(&scope)
                        .expect("a ready scope has a queue");
                    (queue.pop_front(), !queue.is_empty())
                };
                if has_more {
                    state.ready_scopes.push_back(scope);
                } else {
                    state.by_scope.remove(&scope);
                }
                if job.is_some() {
                    state.queued -= 1;
                }
                return job;
            }
            if state.closed {
                return None;
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn cancel_scope(&self, scope: ScopeId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(jobs) = state.by_scope.remove(&scope) {
            state.queued = state.queued.saturating_sub(jobs.len());
        }
        state.ready_scopes.retain(|candidate| *candidate != scope);
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.by_scope.clear();
        state.ready_scopes.clear();
        state.queued = 0;
        self.available.notify_all();
    }

    fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            queued: self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .queued,
            rejected: self.rejected.load(Ordering::Acquire),
        }
    }
}

fn worker_loop(queue: Arc<FairQueue>, active_jobs: Arc<AtomicUsize>, cache: Arc<CacheLedger>) {
    while let Some(job) = queue.take() {
        active_jobs.fetch_add(1, Ordering::AcqRel);
        let scope = job.scope;
        let _ = catch_unwind(AssertUnwindSafe(job.task));
        active_jobs.fetch_sub(1, Ordering::AcqRel);
        cache.reconcile(Some(scope));
    }
}

struct CacheRecord {
    cache: Weak<DocumentCache>,
    touched: u64,
}

struct CacheLedger {
    memory_limit: usize,
    file_limit: usize,
    clock: AtomicU64,
    records: Mutex<HashMap<ScopeId, CacheRecord>>,
}

impl CacheLedger {
    fn new(memory_limit: usize, file_limit: usize) -> Self {
        Self {
            memory_limit,
            file_limit,
            clock: AtomicU64::new(1),
            records: Mutex::new(HashMap::new()),
        }
    }

    fn register(&self, scope: ScopeId, cache: &Arc<DocumentCache>) {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = records.insert(
            scope,
            CacheRecord {
                cache: Arc::downgrade(cache),
                touched: self.clock.fetch_add(1, Ordering::Relaxed),
            },
        ) && !previous.cache.ptr_eq(&Arc::downgrade(cache))
            && let Some(previous) = previous.cache.upgrade()
        {
            previous.purge();
        }
    }

    fn purge_scope(&self, scope: ScopeId) {
        let record = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&scope);
        if let Some(cache) = record.and_then(|record| record.cache.upgrade()) {
            cache.purge();
        }
    }

    fn usage(&self) -> DocumentCacheUsage {
        self.reconcile(None);
        let records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records
            .values()
            .filter_map(|record| record.cache.upgrade())
            .map(|cache| cache.usage())
            .fold(DocumentCacheUsage::default(), add_usage)
    }

    fn reconcile(&self, protected: Option<ScopeId>) {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records.retain(|_, record| record.cache.strong_count() > 0);
        if let Some(scope) = protected
            && let Some(record) = records.get_mut(&scope)
        {
            record.touched = self.clock.fetch_add(1, Ordering::Relaxed);
        }

        loop {
            let usage = records
                .values()
                .filter_map(|record| record.cache.upgrade())
                .map(|cache| cache.usage())
                .fold(DocumentCacheUsage::default(), add_usage);
            if usage.memory_bytes <= self.memory_limit && usage.file_bytes <= self.file_limit {
                break;
            }
            let candidate = records
                .iter()
                .filter(|(scope, _)| Some(**scope) != protected)
                .min_by_key(|(_, record)| record.touched)
                .map(|(scope, _)| *scope)
                .or_else(|| {
                    records
                        .iter()
                        .min_by_key(|(_, record)| record.touched)
                        .map(|(scope, _)| *scope)
                });
            let Some(candidate) = candidate else {
                break;
            };
            if let Some(record) = records.remove(&candidate)
                && let Some(cache) = record.cache.upgrade()
            {
                cache.purge();
            }
        }
    }
}

fn add_usage(mut total: DocumentCacheUsage, usage: DocumentCacheUsage) -> DocumentCacheUsage {
    total.entries = total.entries.saturating_add(usage.entries);
    total.memory_bytes = total.memory_bytes.saturating_add(usage.memory_bytes);
    total.file_bytes = total.file_bytes.saturating_add(usage.file_bytes);
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use key_safe_http::DocumentCacheLimits;
    use std::io::Cursor;
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    fn executor(workers: usize, capacity: usize) -> ReferenceExecutor {
        ReferenceExecutor::new(ReferenceExecutorConfig {
            worker_count: workers,
            queue_capacity: capacity,
            cache_memory_bytes: 8,
            cache_file_bytes: 8,
        })
        .unwrap()
    }

    #[test]
    fn fixed_workers_bound_concurrency_under_stress() {
        let executor = executor(3, 64);
        let scope = executor.document_scope();
        scope.begin_generation(1);
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        for _ in 0..48 {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            let completed = Arc::clone(&completed);
            assert!(scope.execute(1, move |_| {
                let current = active.fetch_add(1, Ordering::AcqRel) + 1;
                maximum.fetch_max(current, Ordering::AcqRel);
                thread::sleep(Duration::from_millis(2));
                active.fetch_sub(1, Ordering::AcqRel);
                completed.fetch_add(1, Ordering::AcqRel);
            }));
        }
        wait_until(|| completed.load(Ordering::Acquire) == 48);
        assert!(maximum.load(Ordering::Acquire) <= 3);
        assert_eq!(executor.snapshot().worker_count, 3);
    }

    #[test]
    fn queue_is_bounded_and_reports_backpressure() {
        let executor = executor(1, 2);
        let scope = executor.document_scope();
        scope.begin_generation(1);
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        assert!(scope.execute(1, move |_| {
            worker_barrier.wait();
        }));
        wait_until(|| executor.snapshot().active_jobs == 1);
        assert!(scope.execute(1, |_| {}));
        assert!(scope.execute(1, |_| {}));
        assert!(!scope.execute(1, |_| {}));
        assert_eq!(executor.snapshot().queued_jobs, 2);
        assert_eq!(executor.snapshot().rejected_jobs, 1);
        barrier.wait();
    }

    #[test]
    fn dispatch_is_round_robin_between_document_scopes() {
        let executor = executor(1, 8);
        let first = executor.document_scope();
        let second = executor.document_scope();
        first.begin_generation(1);
        second.begin_generation(1);
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        assert!(first.execute(1, move |_| {
            worker_barrier.wait();
        }));
        wait_until(|| executor.snapshot().active_jobs == 1);
        let order = Arc::new(Mutex::new(Vec::new()));
        for label in ["a1", "a2", "a3"] {
            let order = Arc::clone(&order);
            assert!(first.execute(1, move |_| order.lock().unwrap().push(label)));
        }
        let order_for_second = Arc::clone(&order);
        assert!(second.execute(1, move |_| { order_for_second.lock().unwrap().push("b1") }));
        barrier.wait();
        wait_until(|| order.lock().unwrap().len() == 4);
        let order = order.lock().unwrap().clone();
        assert!(order.iter().position(|item| *item == "b1").unwrap() < 2);
    }

    #[test]
    fn changing_generation_cancels_running_and_removes_queued_work() {
        let executor = executor(1, 8);
        let scope = executor.document_scope();
        scope.begin_generation(1);
        let started = Arc::new(Barrier::new(2));
        let started_worker = Arc::clone(&started);
        let observed_cancel = Arc::new(AtomicUsize::new(0));
        let observed_cancel_worker = Arc::clone(&observed_cancel);
        assert!(scope.execute(1, move |token| {
            started_worker.wait();
            while !token.is_cancelled() {
                thread::yield_now();
            }
            observed_cancel_worker.store(1, Ordering::Release);
        }));
        started.wait();
        let stale_ran = Arc::new(AtomicUsize::new(0));
        let stale_ran_worker = Arc::clone(&stale_ran);
        assert!(scope.execute(1, move |_| {
            stale_ran_worker.fetch_add(1, Ordering::AcqRel);
        }));
        scope.begin_generation(2);
        wait_until(|| observed_cancel.load(Ordering::Acquire) == 1);
        assert_eq!(stale_ran.load(Ordering::Acquire), 0);
        assert_eq!(executor.snapshot().queued_jobs, 0);
        assert!(!scope.execute(1, |_| {}));
        assert!(scope.execute(2, |_| {}));
    }

    #[test]
    fn dropping_last_document_scope_cancels_running_work() {
        let executor = executor(1, 4);
        let scope = executor.document_scope();
        scope.begin_generation(1);
        let started = Arc::new(Barrier::new(2));
        let started_worker = Arc::clone(&started);
        let observed_cancel = Arc::new(AtomicUsize::new(0));
        let observed_cancel_worker = Arc::clone(&observed_cancel);
        assert!(scope.execute(1, move |token| {
            started_worker.wait();
            while !token.is_cancelled() {
                thread::yield_now();
            }
            observed_cancel_worker.store(1, Ordering::Release);
        }));
        started.wait();
        drop(scope);
        wait_until(|| observed_cancel.load(Ordering::Acquire) == 1);
    }

    #[test]
    fn cache_budget_and_scope_drop_purge_ephemeral_files() {
        let executor = executor(1, 8);
        let first = executor.document_scope();
        let second = executor.document_scope();
        first.begin_generation(1);
        second.begin_generation(1);
        let limits = DocumentCacheLimits {
            memory_bytes: 1,
            file_bytes: 64,
            entry_bytes: 64,
            entries: 4,
        };
        let first_cache = Arc::new(DocumentCache::new(limits).unwrap());
        let second_cache = Arc::new(DocumentCache::new(limits).unwrap());
        first.register_cache(&first_cache);
        second.register_cache(&second_cache);
        first_cache
            .insert_file(
                "first",
                Cursor::new([1_u8; 8]),
                &CancellationToken::active(),
            )
            .unwrap();
        second_cache
            .insert_file(
                "second",
                Cursor::new([2_u8; 8]),
                &CancellationToken::active(),
            )
            .unwrap();
        assert!(second.execute(1, |_| {}));
        wait_until(|| executor.snapshot().active_jobs == 0);
        let snapshot = executor.snapshot();
        assert!(snapshot.cache_file_bytes <= 8);
        assert_eq!(first_cache.usage().file_bytes, 0);
        assert_eq!(second_cache.usage().file_bytes, 8);
        drop(second);
        assert_eq!(second_cache.usage(), DocumentCacheUsage::default());
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
