//! Process-wide, multi-document engine ownership and scheduling.
//!
//! The engine factory is moved to one dedicated owner thread and invoked
//! there, so even an engine and its documents that are deliberately `!Send`
//! never cross a thread boundary. UI clients only exchange engine-independent
//! sources, demands, descriptors, sessions, and result events.

use crate::{
    CancellationSource, CancellationToken, DemandPriority, DocumentDescriptor, DocumentGeneration,
    DocumentResource, DocumentSession, DocumentSessionManager, EngineCapabilities, EngineDocument,
    EngineOutputError, PdfEngine, PreviewDemand, PreviewEvent, PreviewResource, RenderDemand,
    RenderEvent, RenderResource, RenderedPreview, RenderedTile, ResourceHandle, RuntimeFailure,
    TextDemand, TextEvent, TextPage, TextResource,
};
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    hash::Hash,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const IDLE_EVENT_RETRY: Duration = Duration::from_millis(8);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SupervisorDocumentId(NonZeroU64);

impl SupervisorDocumentId {
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Independent replacement domains. Replacing one viewport does not cancel
/// explicit copy/search work; replacing a preview does not cancel tiles.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WorkClass {
    RenderViewport,
    Preview,
    VisibleText,
    CopyText,
    SearchText,
    LinkResolutionText,
    DocumentAnalysisText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisorPolicy {
    max_pending_per_document: usize,
    event_channel_capacity: usize,
}

impl SupervisorPolicy {
    pub fn new(
        max_pending_per_document: usize,
        event_channel_capacity: usize,
    ) -> Result<Self, SupervisorPolicyError> {
        if max_pending_per_document == 0 {
            return Err(SupervisorPolicyError::EmptyWorkQueue);
        }
        if event_channel_capacity == 0 {
            return Err(SupervisorPolicyError::EmptyEventChannel);
        }
        Ok(Self {
            max_pending_per_document,
            event_channel_capacity,
        })
    }

    pub fn max_pending_per_document(self) -> usize {
        self.max_pending_per_document
    }

    pub fn event_channel_capacity(self) -> usize {
        self.event_channel_capacity
    }
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            max_pending_per_document: 4_096,
            // One raster can be several MiB. Back-pressure is per document so
            // an inactive window cannot accumulate bitmaps or block others.
            event_channel_capacity: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorPolicyError {
    EmptyWorkQueue,
    EmptyEventChannel,
}

impl fmt::Display for SupervisorPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorkQueue => {
                formatter.write_str("supervisor work queue capacity must be non-zero")
            }
            Self::EmptyEventChannel => {
                formatter.write_str("supervisor event channel capacity must be non-zero")
            }
        }
    }
}

impl std::error::Error for SupervisorPolicyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorSendError {
    Disconnected,
    DocumentIdsExhausted,
    InvalidWorkClass,
}

impl fmt::Display for SupervisorSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => formatter.write_str("PDF engine supervisor is disconnected"),
            Self::DocumentIdsExhausted => formatter.write_str("PDF document IDs are exhausted"),
            Self::InvalidWorkClass => {
                formatter.write_str("the selected work class does not accept text demands")
            }
        }
    }
}

impl std::error::Error for SupervisorSendError {}

/// Back-pressure result returned by a routed supervisor event sink.
///
/// A route must return the original event when it is temporarily full so the
/// owner thread can retain and retry it without cloning multi-megabyte raster
/// buffers. A disconnected route detaches only its document.
#[derive(Debug)]
pub enum SupervisorRouteError<E> {
    Full(SupervisorEvent<E>),
    Disconnected,
}

#[derive(Debug)]
pub enum SupervisorEvent<E> {
    Attached {
        document: SupervisorDocumentId,
        capabilities: EngineCapabilities,
    },
    Opened {
        document: SupervisorDocumentId,
        session: DocumentSession,
        resource: ResourceHandle<DocumentResource>,
        descriptor: DocumentDescriptor,
    },
    OpenCancelled {
        document: SupervisorDocumentId,
        generation: DocumentGeneration,
    },
    OpenFailed {
        document: SupervisorDocumentId,
        generation: Option<DocumentGeneration>,
        error: RuntimeFailure<E>,
    },
    Closed {
        document: SupervisorDocumentId,
        generation: Option<DocumentGeneration>,
    },
    Rendered {
        document: SupervisorDocumentId,
        event: RenderEvent<E>,
    },
    TextExtracted {
        document: SupervisorDocumentId,
        event: TextEvent<E>,
    },
    PreviewRendered {
        document: SupervisorDocumentId,
        event: PreviewEvent<E>,
    },
    WorkRejected {
        document: SupervisorDocumentId,
        class: WorkClass,
        rejected: usize,
    },
}

pub type SupervisorEvents<E> = mpsc::Receiver<SupervisorEvent<E>>;

type EventRoute<E> =
    Box<dyn Fn(SupervisorEvent<E>) -> Result<(), SupervisorRouteError<E>> + Send + 'static>;

type EngineDocuments<E> = HashMap<
    SupervisorDocumentId,
    ManagedDocument<<E as PdfEngine>::Document, <E as PdfEngine>::Source, <E as PdfEngine>::Error>,
>;

/// Cloneable process-level handle. The final handle/client drop shuts down and
/// joins the owner thread; individual document client drops detach only that
/// document.
pub struct EngineSupervisor<S, E> {
    inner: Arc<SupervisorInner<S, E>>,
}

impl<S, E> Clone for EngineSupervisor<S, E> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct SupervisorInner<S, E> {
    commands: mpsc::Sender<Command<S, E>>,
    next_document: AtomicU64,
    policy: SupervisorPolicy,
    owner: Mutex<Option<JoinHandle<()>>>,
}

impl<S, E> Drop for SupervisorInner<S, E> {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(owner) = self
            .owner
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = owner.join();
        }
    }
}

/// Starts the only thread on which `factory`, the engine, and all engine
/// documents are ever used. `E` itself intentionally has no `Send` bound.
pub fn start_engine_supervisor<E, F>(
    thread_name: impl Into<String>,
    policy: SupervisorPolicy,
    factory: F,
) -> std::io::Result<EngineSupervisor<E::Source, E::Error>>
where
    E: PdfEngine + 'static,
    E::Source: Send + 'static,
    E::Error: Send + 'static,
    F: FnOnce() -> E + Send + 'static,
{
    let (commands, receiver) = mpsc::channel();
    let owner = thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || run_owner(factory(), policy, receiver))?;
    Ok(EngineSupervisor {
        inner: Arc::new(SupervisorInner {
            commands,
            next_document: AtomicU64::new(1),
            policy,
            owner: Mutex::new(Some(owner)),
        }),
    })
}

impl<S, E> EngineSupervisor<S, E>
where
    S: Send + 'static,
    E: Send + 'static,
{
    pub fn attach(
        &self,
    ) -> Result<(DocumentClient<S, E>, SupervisorEvents<E>), SupervisorSendError> {
        let (events, receiver) = mpsc::sync_channel(self.inner.policy.event_channel_capacity());
        let client = self.attach_routed(move |event| match events.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(event)) => Err(SupervisorRouteError::Full(event)),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(SupervisorRouteError::Disconnected),
        })?;
        Ok((client, receiver))
    }

    /// Attaches a document to a caller-provided bounded route.
    ///
    /// This lets a controller merge supervisor events with its own command
    /// mailbox and block on one receiver, avoiding both polling and a second
    /// forwarding thread. The route is invoked only on the engine owner
    /// thread and must therefore be non-blocking; return `Full(event)` to
    /// apply per-document back-pressure.
    pub fn attach_routed<F>(&self, route: F) -> Result<DocumentClient<S, E>, SupervisorSendError>
    where
        F: Fn(SupervisorEvent<E>) -> Result<(), SupervisorRouteError<E>> + Send + 'static,
    {
        let raw = self
            .inner
            .next_document
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| SupervisorSendError::DocumentIdsExhausted)?;
        let id = NonZeroU64::new(raw)
            .map(SupervisorDocumentId)
            .ok_or(SupervisorSendError::DocumentIdsExhausted)?;
        self.inner
            .commands
            .send(Command::Attach {
                id,
                events: Box::new(route),
            })
            .map_err(|_| SupervisorSendError::Disconnected)?;
        Ok(DocumentClient {
            inner: Arc::new(DocumentClientInner {
                id,
                supervisor: self.inner.clone(),
                cancellations: Mutex::new(ClientCancellations::default()),
            }),
        })
    }
}

pub struct DocumentClient<S, E> {
    inner: Arc<DocumentClientInner<S, E>>,
}

impl<S, E> Clone for DocumentClient<S, E> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct DocumentClientInner<S, E> {
    id: SupervisorDocumentId,
    supervisor: Arc<SupervisorInner<S, E>>,
    cancellations: Mutex<ClientCancellations>,
}

#[derive(Debug, Default)]
struct ClientCancellations {
    open: Option<CancellationSource>,
    classes: HashMap<WorkClass, CancellationSource>,
}

impl ClientCancellations {
    fn cancel_all(&mut self) {
        if let Some(open) = self.open.take() {
            open.cancel();
        }
        for (_, cancellation) in self.classes.drain() {
            cancellation.cancel();
        }
    }

    fn replace_open(&mut self) -> CancellationSource {
        self.cancel_all();
        let cancellation = CancellationSource::new();
        self.open = Some(cancellation.clone());
        cancellation
    }

    fn replace_class(&mut self, class: WorkClass) -> CancellationSource {
        if let Some(previous) = self.classes.remove(&class) {
            previous.cancel();
        }
        let cancellation = CancellationSource::new();
        self.classes.insert(class, cancellation.clone());
        cancellation
    }

    fn cancel_class(&mut self, class: WorkClass) {
        if let Some(previous) = self.classes.remove(&class) {
            previous.cancel();
        }
    }
}

impl<S, E> Drop for DocumentClientInner<S, E> {
    fn drop(&mut self) {
        self.cancellations
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .cancel_all();
        let _ = self
            .supervisor
            .commands
            .send(Command::Detach { id: self.id });
    }
}

impl<S, E> DocumentClient<S, E>
where
    S: Send + 'static,
    E: Send + 'static,
{
    pub fn id(&self) -> SupervisorDocumentId {
        self.inner.id
    }

    pub fn open(&self, source: S) -> Result<(), SupervisorSendError> {
        let cancellation = self
            .inner
            .cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace_open();
        self.send(Command::Open {
            id: self.id(),
            source,
            cancellation,
        })
    }

    pub fn close(&self) -> Result<(), SupervisorSendError> {
        self.inner
            .cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel_all();
        self.send(Command::Close { id: self.id() })
    }

    pub fn replace_render_viewport(
        &self,
        demands: Vec<RenderDemand>,
    ) -> Result<(), SupervisorSendError> {
        self.replace(
            WorkClass::RenderViewport,
            demands.into_iter().map(Work::Render),
        )
    }

    pub fn replace_preview(
        &self,
        demand: Option<PreviewDemand>,
    ) -> Result<(), SupervisorSendError> {
        self.replace(WorkClass::Preview, demand.into_iter().map(Work::Preview))
    }

    pub fn replace_text(
        &self,
        class: WorkClass,
        demands: Vec<TextDemand>,
    ) -> Result<(), SupervisorSendError> {
        if !matches!(
            class,
            WorkClass::VisibleText
                | WorkClass::CopyText
                | WorkClass::SearchText
                | WorkClass::LinkResolutionText
                | WorkClass::DocumentAnalysisText
        ) {
            return Err(SupervisorSendError::InvalidWorkClass);
        }
        self.replace(class, demands.into_iter().map(Work::Text))
    }

    pub fn cancel(&self, class: WorkClass) -> Result<(), SupervisorSendError> {
        self.inner
            .cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel_class(class);
        self.send(Command::CancelClass {
            id: self.id(),
            class,
        })
    }

    fn replace(
        &self,
        class: WorkClass,
        work: impl IntoIterator<Item = Work>,
    ) -> Result<(), SupervisorSendError> {
        let cancellation = self
            .inner
            .cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace_class(class);
        self.send(Command::Replace {
            id: self.id(),
            class,
            work: work.into_iter().collect(),
            cancellation,
        })
    }

    fn send(&self, command: Command<S, E>) -> Result<(), SupervisorSendError> {
        self.inner
            .supervisor
            .commands
            .send(command)
            .map_err(|_| SupervisorSendError::Disconnected)
    }
}

enum Command<S, E> {
    Attach {
        id: SupervisorDocumentId,
        events: EventRoute<E>,
    },
    Open {
        id: SupervisorDocumentId,
        source: S,
        cancellation: CancellationSource,
    },
    Close {
        id: SupervisorDocumentId,
    },
    Replace {
        id: SupervisorDocumentId,
        class: WorkClass,
        work: Vec<Work>,
        cancellation: CancellationSource,
    },
    CancelClass {
        id: SupervisorDocumentId,
        class: WorkClass,
    },
    Detach {
        id: SupervisorDocumentId,
    },
    Shutdown,
}

enum Work {
    Render(RenderDemand),
    Text(TextDemand),
    Preview(PreviewDemand),
}

impl Work {
    fn priority(&self) -> DemandPriority {
        match self {
            Self::Render(demand) => demand.stamp().priority(),
            Self::Text(demand) => demand.stamp().priority(),
            Self::Preview(demand) => demand.stamp().priority(),
        }
    }

    fn key(&self, class: WorkClass) -> WorkKey {
        match self {
            Self::Render(demand) => WorkKey::Render(demand.deduplication_key()),
            Self::Text(demand) => WorkKey::Text(class, demand.page()),
            Self::Preview(_) => WorkKey::Preview,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum WorkKey {
    Open,
    Render(crate::RenderDemandKey),
    Text(WorkClass, usize),
    Preview,
}

struct PendingWork<S> {
    sequence: u128,
    priority: DemandPriority,
    class: Option<WorkClass>,
    cancellation: CancellationSource,
    kind: PendingKind<S>,
}

enum PendingKind<S> {
    Open(S),
    Engine(Work),
}

struct ManagedDocument<D, S, E> {
    sessions: DocumentSessionManager,
    document: Option<D>,
    document_handle: Option<ResourceHandle<DocumentResource>>,
    descriptor: Option<DocumentDescriptor>,
    events: EventRoute<E>,
    pending_events: VecDeque<SupervisorEvent<E>>,
    pending: HashMap<WorkKey, PendingWork<S>>,
    next_sequence: u128,
}

impl<D, S, E> ManagedDocument<D, S, E> {
    fn new(events: EventRoute<E>) -> Self {
        Self {
            sessions: DocumentSessionManager::new(),
            document: None,
            document_handle: None,
            descriptor: None,
            events,
            pending_events: VecDeque::new(),
            pending: HashMap::new(),
            next_sequence: 1,
        }
    }

    fn allocate_sequence(&mut self) -> u128 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        sequence
    }

    fn cancel_and_clear(&mut self) {
        self.sessions.close();
        for (_, pending) in self.pending.drain() {
            pending.cancellation.cancel();
        }
        self.document = None;
        self.document_handle = None;
        self.descriptor = None;
    }

    fn replace_class(
        &mut self,
        class: WorkClass,
        work: Vec<Work>,
        cancellation: CancellationSource,
        capacity: usize,
    ) -> usize {
        self.pending.retain(|_, pending| {
            if pending.class == Some(class) {
                pending.cancellation.cancel();
                false
            } else {
                true
            }
        });
        let mut rejected = 0;
        for work in work {
            let key = work.key(class);
            let priority = work.priority();
            let sequence = self.allocate_sequence();
            if let Some(previous) = self.pending.insert(
                key,
                PendingWork {
                    sequence,
                    priority,
                    class: Some(class),
                    cancellation: cancellation.clone(),
                    kind: PendingKind::Engine(work),
                },
            ) {
                previous.cancellation.cancel();
            }
            while self.pending.len() > capacity {
                let candidate = self
                    .pending
                    .iter()
                    .filter(|(key, _)| **key != WorkKey::Open)
                    .min_by_key(|(_, pending)| (pending.priority, pending.sequence))
                    .map(|(key, _)| *key);
                let Some(candidate) = candidate else {
                    break;
                };
                if let Some(evicted) = self.pending.remove(&candidate) {
                    evicted.cancellation.cancel();
                    rejected += 1;
                }
            }
        }
        rejected
    }

    fn pop_next(&mut self) -> Option<PendingWork<S>> {
        let key = self
            .pending
            .iter()
            .max_by_key(|(_, pending)| (pending.priority, pending.sequence))
            .map(|(key, _)| *key)?;
        self.pending.remove(&key)
    }

    fn highest_pending_priority(&self) -> Option<DemandPriority> {
        self.pending.values().map(|pending| pending.priority).max()
    }
}

impl<D, S, E> Drop for ManagedDocument<D, S, E> {
    fn drop(&mut self) {
        self.cancel_and_clear();
    }
}

fn run_owner<E: PdfEngine>(
    mut engine: E,
    policy: SupervisorPolicy,
    commands: mpsc::Receiver<Command<E::Source, E::Error>>,
) {
    let capabilities = engine.capabilities();
    let mut documents = EngineDocuments::<E>::new();
    let mut rotation = VecDeque::<SupervisorDocumentId>::new();
    let mut running = true;

    while running {
        running = drain_commands(
            &mut engine,
            capabilities,
            policy,
            &commands,
            &mut documents,
            &mut rotation,
        );
        if !running {
            break;
        }
        flush_pending_events(&mut documents, &mut rotation);

        if let Some(id) = next_runnable(&mut documents, &mut rotation) {
            let pending = documents.get_mut(&id).and_then(ManagedDocument::pop_next);
            if let Some(pending) = pending {
                process_work(&mut engine, id, pending, &mut documents);
            }
            continue;
        }

        let has_backpressured_events = documents
            .values()
            .any(|document| !document.pending_events.is_empty());
        let received = if has_backpressured_events {
            commands.recv_timeout(IDLE_EVENT_RETRY).ok()
        } else {
            commands.recv().ok()
        };
        match received {
            Some(command) => {
                running = accept_command(
                    &mut engine,
                    capabilities,
                    policy,
                    command,
                    &mut documents,
                    &mut rotation,
                );
            }
            None if has_backpressured_events => {}
            None => break,
        }
    }
}

fn drain_commands<E: PdfEngine>(
    engine: &mut E,
    capabilities: EngineCapabilities,
    policy: SupervisorPolicy,
    commands: &mpsc::Receiver<Command<E::Source, E::Error>>,
    documents: &mut EngineDocuments<E>,
    rotation: &mut VecDeque<SupervisorDocumentId>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => {
                if !accept_command(engine, capabilities, policy, command, documents, rotation) {
                    return false;
                }
            }
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

fn accept_command<E: PdfEngine>(
    _engine: &mut E,
    capabilities: EngineCapabilities,
    policy: SupervisorPolicy,
    command: Command<E::Source, E::Error>,
    documents: &mut EngineDocuments<E>,
    rotation: &mut VecDeque<SupervisorDocumentId>,
) -> bool {
    match command {
        Command::Attach { id, events } => {
            if documents.contains_key(&id) {
                return true;
            }
            let mut document = ManagedDocument::new(events);
            document
                .pending_events
                .push_back(SupervisorEvent::Attached {
                    document: id,
                    capabilities,
                });
            documents.insert(id, document);
            rotation.push_back(id);
        }
        Command::Open {
            id,
            source,
            cancellation,
        } => {
            let Some(document) = documents.get_mut(&id) else {
                return true;
            };
            document.cancel_and_clear();
            let sequence = document.allocate_sequence();
            document.pending.insert(
                WorkKey::Open,
                PendingWork {
                    sequence,
                    priority: DemandPriority::INTERACTIVE,
                    class: None,
                    cancellation,
                    kind: PendingKind::Open(source),
                },
            );
        }
        Command::Close { id } => {
            if let Some(document) = documents.get_mut(&id) {
                let generation = document.sessions.current().map(DocumentSession::generation);
                document.cancel_and_clear();
                document.pending_events.push_back(SupervisorEvent::Closed {
                    document: id,
                    generation,
                });
            }
        }
        Command::Replace {
            id,
            class,
            work,
            cancellation,
        } => {
            if let Some(document) = documents.get_mut(&id) {
                let rejected = document.replace_class(
                    class,
                    work,
                    cancellation,
                    policy.max_pending_per_document(),
                );
                if rejected > 0 {
                    document
                        .pending_events
                        .push_back(SupervisorEvent::WorkRejected {
                            document: id,
                            class,
                            rejected,
                        });
                }
            }
        }
        Command::CancelClass { id, class } => {
            if let Some(document) = documents.get_mut(&id) {
                document.pending.retain(|_, pending| {
                    if pending.class == Some(class) {
                        pending.cancellation.cancel();
                        false
                    } else {
                        true
                    }
                });
            }
        }
        Command::Detach { id } => {
            documents.remove(&id);
            rotation.retain(|candidate| *candidate != id);
        }
        Command::Shutdown => return false,
    }
    true
}

fn flush_pending_events<D, S, E>(
    documents: &mut HashMap<SupervisorDocumentId, ManagedDocument<D, S, E>>,
    rotation: &mut VecDeque<SupervisorDocumentId>,
) {
    let mut disconnected = Vec::new();
    for (id, document) in documents.iter_mut() {
        while let Some(event) = document.pending_events.pop_front() {
            match (document.events)(event) {
                Ok(()) => {}
                Err(SupervisorRouteError::Full(event)) => {
                    document.pending_events.push_front(event);
                    break;
                }
                Err(SupervisorRouteError::Disconnected) => {
                    disconnected.push(*id);
                    break;
                }
            }
        }
    }
    for id in disconnected {
        documents.remove(&id);
        rotation.retain(|candidate| *candidate != id);
    }
}

fn next_runnable<D, S, E>(
    documents: &mut HashMap<SupervisorDocumentId, ManagedDocument<D, S, E>>,
    rotation: &mut VecDeque<SupervisorDocumentId>,
) -> Option<SupervisorDocumentId> {
    let highest = rotation
        .iter()
        .filter_map(|id| {
            documents.get(id).and_then(|document| {
                document
                    .pending_events
                    .is_empty()
                    .then(|| document.highest_pending_priority())
                    .flatten()
            })
        })
        .max()?;
    let count = rotation.len();
    for _ in 0..count {
        let id = rotation.pop_front()?;
        rotation.push_back(id);
        if documents.get(&id).is_some_and(|document| {
            document.pending_events.is_empty()
                && document.highest_pending_priority() == Some(highest)
        }) {
            return Some(id);
        }
    }
    None
}

fn process_work<E: PdfEngine>(
    engine: &mut E,
    id: SupervisorDocumentId,
    pending: PendingWork<E::Source>,
    documents: &mut EngineDocuments<E>,
) {
    let Some(document) = documents.get_mut(&id) else {
        return;
    };
    match pending.kind {
        PendingKind::Open(source) => {
            process_open(engine, id, source, pending.cancellation, document)
        }
        PendingKind::Engine(Work::Render(demand)) => {
            let event = execute_render(document, demand, &pending.cancellation.token());
            document
                .pending_events
                .push_back(SupervisorEvent::Rendered {
                    document: id,
                    event,
                });
        }
        PendingKind::Engine(Work::Text(demand)) => {
            let event = execute_text(document, demand, &pending.cancellation.token());
            document
                .pending_events
                .push_back(SupervisorEvent::TextExtracted {
                    document: id,
                    event,
                });
        }
        PendingKind::Engine(Work::Preview(demand)) => {
            let event = execute_preview(document, demand, &pending.cancellation.token());
            document
                .pending_events
                .push_back(SupervisorEvent::PreviewRendered {
                    document: id,
                    event,
                });
        }
    }
}

fn process_open<E: PdfEngine>(
    engine: &mut E,
    id: SupervisorDocumentId,
    source: E::Source,
    operation: CancellationSource,
    document: &mut ManagedDocument<E::Document, E::Source, E::Error>,
) {
    document.document = None;
    document.document_handle = None;
    document.descriptor = None;
    let session = match document.sessions.begin() {
        Ok(session) => session,
        Err(error) => {
            document
                .pending_events
                .push_back(SupervisorEvent::OpenFailed {
                    document: id,
                    generation: None,
                    error: RuntimeFailure::Session(error),
                });
            return;
        }
    };
    let generation = session.generation();
    let cancellation = session.cancellation().combined(&operation.token());
    match engine.open(source, &cancellation) {
        Ok(opened) if cancellation.is_cancelled() => {
            drop(opened);
            document
                .pending_events
                .push_back(SupervisorEvent::OpenCancelled {
                    document: id,
                    generation,
                });
        }
        Ok(opened) => {
            let descriptor = opened.descriptor().clone();
            match session.allocate_resource::<DocumentResource>() {
                Ok(resource) => {
                    document.document = Some(opened);
                    document.document_handle = Some(resource);
                    document.descriptor = Some(descriptor.clone());
                    document.pending_events.push_back(SupervisorEvent::Opened {
                        document: id,
                        session,
                        resource,
                        descriptor,
                    });
                }
                Err(error) => document
                    .pending_events
                    .push_back(SupervisorEvent::OpenFailed {
                        document: id,
                        generation: Some(generation),
                        error: RuntimeFailure::Allocation(error),
                    }),
            }
        }
        Err(_) if cancellation.is_cancelled() => {
            document
                .pending_events
                .push_back(SupervisorEvent::OpenCancelled {
                    document: id,
                    generation,
                });
        }
        Err(error) => document
            .pending_events
            .push_back(SupervisorEvent::OpenFailed {
                document: id,
                generation: Some(generation),
                error: RuntimeFailure::Engine(error),
            }),
    }
}

fn execute_render<D: EngineDocument, S>(
    document: &mut ManagedDocument<D, S, D::Error>,
    demand: RenderDemand,
    operation: &CancellationToken,
) -> RenderEvent<D::Error> {
    let stamp = demand.stamp();
    let Some(session) = current_session(document, stamp.generation()) else {
        return RenderEvent::Discarded { stamp };
    };
    let cancellation = session.cancellation().combined(operation);
    if cancellation.is_cancelled() {
        return RenderEvent::Cancelled { stamp };
    }
    if let Err(error) = validate_page(document.descriptor.as_ref(), demand.page()) {
        return RenderEvent::Failed { stamp, error };
    }
    let Some(engine_document) = document.document.as_mut() else {
        return RenderEvent::Failed {
            stamp,
            error: RuntimeFailure::NoDocument,
        };
    };
    let result = engine_document.render(&demand, &cancellation);
    if cancellation.is_cancelled() {
        return RenderEvent::Cancelled { stamp };
    }
    match result {
        Ok(image) => {
            let expected = demand.render_rect();
            if image.width() != expected.width || image.height() != expected.height {
                return RenderEvent::Failed {
                    stamp,
                    error: RuntimeFailure::InvalidEngineOutput(
                        EngineOutputError::UnexpectedDimensions {
                            expected_width: expected.width,
                            expected_height: expected.height,
                            actual_width: image.width(),
                            actual_height: image.height(),
                        },
                    ),
                };
            }
            match session.allocate_resource::<RenderResource>() {
                Ok(resource) => RenderEvent::Ready {
                    demand,
                    tile: RenderedTile { resource, image },
                },
                Err(error) => RenderEvent::Failed {
                    stamp,
                    error: RuntimeFailure::Allocation(error),
                },
            }
        }
        Err(error) => RenderEvent::Failed {
            stamp,
            error: RuntimeFailure::Engine(error),
        },
    }
}

fn execute_text<D: EngineDocument, S>(
    document: &mut ManagedDocument<D, S, D::Error>,
    demand: TextDemand,
    operation: &CancellationToken,
) -> TextEvent<D::Error> {
    let stamp = demand.stamp();
    let Some(session) = current_session(document, stamp.generation()) else {
        return TextEvent::Discarded { stamp };
    };
    let cancellation = session.cancellation().combined(operation);
    if cancellation.is_cancelled() {
        return TextEvent::Cancelled { stamp };
    }
    if let Err(error) = validate_page(document.descriptor.as_ref(), demand.page()) {
        return TextEvent::Failed { stamp, error };
    }
    let Some(engine_document) = document.document.as_mut() else {
        return TextEvent::Failed {
            stamp,
            error: RuntimeFailure::NoDocument,
        };
    };
    let result = engine_document.extract_text(&demand, &cancellation);
    if cancellation.is_cancelled() {
        return TextEvent::Cancelled { stamp };
    }
    match result {
        Ok(layer) => match session.allocate_resource::<TextResource>() {
            Ok(resource) => TextEvent::Ready {
                text: TextPage {
                    resource,
                    page: demand.page(),
                    layer: Arc::new(layer),
                },
                demand,
            },
            Err(error) => TextEvent::Failed {
                stamp,
                error: RuntimeFailure::Allocation(error),
            },
        },
        Err(error) => TextEvent::Failed {
            stamp,
            error: RuntimeFailure::Engine(error),
        },
    }
}

fn execute_preview<D: EngineDocument, S>(
    document: &mut ManagedDocument<D, S, D::Error>,
    demand: PreviewDemand,
    operation: &CancellationToken,
) -> PreviewEvent<D::Error> {
    let stamp = demand.stamp();
    let Some(session) = current_session(document, stamp.generation()) else {
        return PreviewEvent::Discarded { stamp };
    };
    let cancellation = session.cancellation().combined(operation);
    if cancellation.is_cancelled() {
        return PreviewEvent::Cancelled { stamp };
    }
    if let Err(error) = validate_page(document.descriptor.as_ref(), demand.page()) {
        return PreviewEvent::Failed { stamp, error };
    }
    let Some(engine_document) = document.document.as_mut() else {
        return PreviewEvent::Failed {
            stamp,
            error: RuntimeFailure::NoDocument,
        };
    };
    let result = engine_document.render_preview(&demand, &cancellation);
    if cancellation.is_cancelled() {
        return PreviewEvent::Cancelled { stamp };
    }
    match result {
        Ok(image) => {
            let expected = demand.region();
            if image.width() != expected.width || image.height() != expected.height {
                return PreviewEvent::Failed {
                    stamp,
                    error: RuntimeFailure::InvalidEngineOutput(
                        EngineOutputError::UnexpectedDimensions {
                            expected_width: expected.width,
                            expected_height: expected.height,
                            actual_width: image.width(),
                            actual_height: image.height(),
                        },
                    ),
                };
            }
            match session.allocate_resource::<PreviewResource>() {
                Ok(resource) => PreviewEvent::Ready {
                    preview: RenderedPreview {
                        resource,
                        page: demand.page(),
                        image,
                    },
                    demand,
                },
                Err(error) => PreviewEvent::Failed {
                    stamp,
                    error: RuntimeFailure::Allocation(error),
                },
            }
        }
        Err(error) => PreviewEvent::Failed {
            stamp,
            error: RuntimeFailure::Engine(error),
        },
    }
}

fn current_session<D, S, E>(
    document: &ManagedDocument<D, S, E>,
    generation: DocumentGeneration,
) -> Option<DocumentSession> {
    document
        .sessions
        .current()
        .filter(|session| session.generation() == generation && !session.is_cancelled())
        .cloned()
}

fn validate_page<E>(
    descriptor: Option<&DocumentDescriptor>,
    page: usize,
) -> Result<(), RuntimeFailure<E>> {
    let Some(descriptor) = descriptor else {
        return Err(RuntimeFailure::NoDocument);
    };
    if page >= descriptor.page_count() {
        Err(RuntimeFailure::InvalidPage {
            page,
            page_count: descriptor.page_count(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
