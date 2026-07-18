use crate::{AnnotationStore, DocumentIdentity, DocumentKey, JsonSidecarStore, StoreError};
use key_pdf_core::AnnotationSet;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::Duration;

const COMMAND_CAPACITY: usize = 256;
const EVENT_CAPACITY: usize = 64;
const PENDING_EVENT_CAPACITY: usize = 8;
const EVENT_RETRY_INTERVAL: Duration = Duration::from_millis(5);
const SERVICE_STACK_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AnnotationClientId(u64);

impl AnnotationClientId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AnnotationDocumentId(u64);

impl AnnotationDocumentId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnotationServiceOperation {
    Load,
    Save,
}

#[derive(Debug)]
pub enum AnnotationServiceEventKind {
    Loaded {
        identity: DocumentIdentity,
        annotations: AnnotationSet,
    },
    Saved {
        revision: u64,
    },
    Failed {
        operation: AnnotationServiceOperation,
        revision: Option<u64>,
        message: String,
    },
}

#[derive(Debug)]
pub struct AnnotationServiceEvent {
    pub client_id: AnnotationClientId,
    pub document_id: AnnotationDocumentId,
    pub generation: u64,
    pub kind: AnnotationServiceEventKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnnotationServiceDiagnostics {
    pub clients: usize,
    pub documents: usize,
    pub pending_operations: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnotationServiceStartError {
    WorkerUnavailable,
    IdsExhausted,
}

impl Display for AnnotationServiceStartError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkerUnavailable => formatter.write_str("the annotation service is unavailable"),
            Self::IdsExhausted => formatter.write_str("annotation service IDs are exhausted"),
        }
    }
}

impl std::error::Error for AnnotationServiceStartError {}

enum Command {
    Attach {
        client_id: AnnotationClientId,
        document_id: AnnotationDocumentId,
        events: flume::Sender<AnnotationServiceEvent>,
        lease: Weak<ClientLease>,
    },
    Load {
        client_id: AnnotationClientId,
        document_id: AnnotationDocumentId,
        generation: u64,
        path: PathBuf,
        page_count: usize,
    },
    Save {
        client_id: AnnotationClientId,
        document_id: AnnotationDocumentId,
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    },
    Inspect {
        reply: mpsc::SyncSender<AnnotationServiceDiagnostics>,
    },
    Wake,
    Shutdown,
}

enum Operation {
    Load {
        generation: u64,
        path: PathBuf,
        page_count: usize,
    },
    Save {
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    },
    Detach,
}

struct DocumentLane {
    client_id: AnnotationClientId,
    lease: Weak<ClientLease>,
    operations: VecDeque<Operation>,
    scheduled: bool,
    closing: bool,
    active_generation: Option<u64>,
    active_path: Option<PathBuf>,
    observed_disk_revision: Option<u64>,
}

impl DocumentLane {
    fn new(client_id: AnnotationClientId, lease: Weak<ClientLease>) -> Self {
        Self {
            client_id,
            lease,
            operations: VecDeque::new(),
            scheduled: false,
            closing: false,
            active_generation: None,
            active_path: None,
            observed_disk_revision: None,
        }
    }
}

struct ClientSink {
    events: flume::Sender<AnnotationServiceEvent>,
    pending: VecDeque<AnnotationServiceEvent>,
}

struct ClientLease {
    detached: AtomicBool,
}

struct ServiceInner {
    commands: mpsc::SyncSender<Command>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    next_client_id: AtomicU64,
    next_document_id: AtomicU64,
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        // Shutdown is ordered after every command already accepted by the
        // bounded mailbox, so the actor drains pending saves before joining.
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = thread.join();
        }
    }
}

/// One process-level actor for annotation persistence across all open documents.
///
/// The service owns exactly one writer thread. Each attached client gets an
/// isolated document lane; the actor executes one operation per ready lane in
/// round-robin order and collapses adjacent saves for the same generation to
/// the newest snapshot.
#[derive(Clone)]
pub struct AnnotationService {
    inner: Arc<ServiceInner>,
}

impl AnnotationService {
    pub fn start() -> Self {
        Self::start_with_store(Arc::new(JsonSidecarStore))
    }

    pub fn start_with_store(store: Arc<dyn AnnotationStore>) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let thread = thread::Builder::new()
            .name("annotation-service".into())
            .stack_size(SERVICE_STACK_BYTES)
            .spawn(move || run_service(store, command_rx))
            .expect("failed to start the annotation service thread");
        Self {
            inner: Arc::new(ServiceInner {
                commands: command_tx,
                thread: Mutex::new(Some(thread)),
                next_client_id: AtomicU64::new(1),
                next_document_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn attach(
        &self,
    ) -> Result<
        (
            AnnotationServiceClient,
            flume::Receiver<AnnotationServiceEvent>,
        ),
        AnnotationServiceStartError,
    > {
        let client_id = next_id(&self.inner.next_client_id)
            .map(AnnotationClientId)
            .ok_or(AnnotationServiceStartError::IdsExhausted)?;
        let document_id = next_id(&self.inner.next_document_id)
            .map(AnnotationDocumentId)
            .ok_or(AnnotationServiceStartError::IdsExhausted)?;
        let (event_tx, event_rx) = flume::bounded(EVENT_CAPACITY);
        let lease = Arc::new(ClientLease {
            detached: AtomicBool::new(false),
        });
        self.inner
            .commands
            .send(Command::Attach {
                client_id,
                document_id,
                events: event_tx,
                lease: Arc::downgrade(&lease),
            })
            .map_err(|_| AnnotationServiceStartError::WorkerUnavailable)?;
        Ok((
            AnnotationServiceClient {
                service: Arc::downgrade(&self.inner),
                client_id,
                document_id,
                lease,
            },
            event_rx,
        ))
    }

    /// Returns an idle-boundary snapshot. The reply is sent only after every
    /// command accepted before this call (including detach cleanup) is applied.
    pub fn diagnostics(
        &self,
        timeout: Duration,
    ) -> Result<AnnotationServiceDiagnostics, AnnotationServiceStartError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.inner
            .commands
            .send(Command::Inspect { reply: reply_tx })
            .map_err(|_| AnnotationServiceStartError::WorkerUnavailable)?;
        reply_rx
            .recv_timeout(timeout)
            .map_err(|_| AnnotationServiceStartError::WorkerUnavailable)
    }
}

fn next_id(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
}

pub struct AnnotationServiceClient {
    service: Weak<ServiceInner>,
    client_id: AnnotationClientId,
    document_id: AnnotationDocumentId,
    lease: Arc<ClientLease>,
}

impl AnnotationServiceClient {
    pub fn client_id(&self) -> AnnotationClientId {
        self.client_id
    }

    pub fn document_id(&self) -> AnnotationDocumentId {
        self.document_id
    }

    pub fn load(&self, generation: u64, path: PathBuf, page_count: usize) -> bool {
        self.submit(Command::Load {
            client_id: self.client_id,
            document_id: self.document_id,
            generation,
            path,
            page_count,
        })
    }

    pub fn save(
        &self,
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    ) -> bool {
        self.submit(Command::Save {
            client_id: self.client_id,
            document_id: self.document_id,
            generation,
            path,
            identity,
            expected_disk_revision,
            annotations,
        })
    }

    fn submit(&self, command: Command) -> bool {
        self.service
            .upgrade()
            .is_some_and(|service| service.commands.try_send(command).is_ok())
    }
}

impl Drop for AnnotationServiceClient {
    fn drop(&mut self) {
        self.lease.detached.store(true, Ordering::Release);
        if let Some(service) = self.service.upgrade() {
            // The lease is the reliable close signal. Wake is only a
            // best-effort nudge for an idle actor; if the mailbox is full, the
            // actor is already guaranteed to wake and observe the lease after
            // draining commands accepted before this drop.
            let _ = service.commands.try_send(Command::Wake);
        }
    }
}

fn run_service(store: Arc<dyn AnnotationStore>, commands: mpsc::Receiver<Command>) {
    let mut clients = HashMap::<AnnotationClientId, ClientSink>::new();
    let mut documents = HashMap::<AnnotationDocumentId, DocumentLane>::new();
    let mut ready = VecDeque::<AnnotationDocumentId>::new();
    let mut ready_set = HashSet::<AnnotationDocumentId>::new();
    let mut inspections = Vec::<mpsc::SyncSender<AnnotationServiceDiagnostics>>::new();
    let mut shutting_down = false;
    let mut mailbox_open = true;
    let mut last_served = None;

    loop {
        if ready.is_empty()
            && inspections.is_empty()
            && !clients.values().any(|sink| !sink.pending.is_empty())
            && !shutting_down
            && mailbox_open
        {
            match commands.recv() {
                Ok(command) => ingest(
                    command,
                    &mut clients,
                    &mut documents,
                    &mut ready,
                    &mut ready_set,
                    &mut inspections,
                    &mut shutting_down,
                ),
                Err(_) => mailbox_open = false,
            }
        }

        while mailbox_open {
            match commands.try_recv() {
                Ok(command) => ingest(
                    command,
                    &mut clients,
                    &mut documents,
                    &mut ready,
                    &mut ready_set,
                    &mut inspections,
                    &mut shutting_down,
                ),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    mailbox_open = false;
                    break;
                }
            }
        }

        reap_detached(&mut clients, &mut documents, &mut ready, &mut ready_set);
        flush_all_clients(&mut clients);

        // Commands can arrive while a store call is running. If that document
        // becomes ready first again, rotate it behind another ready lane so a
        // busy producer cannot take two turns while a peer waits.
        if ready.len() > 1
            && ready.front().copied() == last_served
            && let Some(document_id) = ready.pop_front()
        {
            ready.push_back(document_id);
        }
        let mut selected = None;
        for _ in 0..ready.len() {
            let Some(document_id) = ready.pop_front() else {
                break;
            };
            let eligible = documents.get(&document_id).is_none_or(|lane| {
                lane.closing || client_can_accept_event(&mut clients, lane.client_id)
            });
            if eligible {
                selected = Some(document_id);
                break;
            }
            ready.push_back(document_id);
        }
        if let Some(document_id) = selected {
            ready_set.remove(&document_id);
            process_one(
                document_id,
                store.as_ref(),
                &mut clients,
                &mut documents,
                &mut ready,
                &mut ready_set,
            );
            last_served = Some(document_id);
            continue;
        }

        if (!ready.is_empty() || clients.values().any(|sink| !sink.pending.is_empty()))
            && mailbox_open
            && !shutting_down
        {
            match commands.recv_timeout(EVENT_RETRY_INTERVAL) {
                Ok(command) => ingest(
                    command,
                    &mut clients,
                    &mut documents,
                    &mut ready,
                    &mut ready_set,
                    &mut inspections,
                    &mut shutting_down,
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => mailbox_open = false,
            }
            continue;
        }

        if !inspections.is_empty() {
            let diagnostics = AnnotationServiceDiagnostics {
                clients: clients.len(),
                documents: documents.len(),
                pending_operations: documents.values().map(|lane| lane.operations.len()).sum(),
            };
            for reply in inspections.drain(..) {
                let _ = reply.try_send(diagnostics);
            }
        }

        if shutting_down || !mailbox_open {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ingest(
    command: Command,
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    documents: &mut HashMap<AnnotationDocumentId, DocumentLane>,
    ready: &mut VecDeque<AnnotationDocumentId>,
    ready_set: &mut HashSet<AnnotationDocumentId>,
    inspections: &mut Vec<mpsc::SyncSender<AnnotationServiceDiagnostics>>,
    shutting_down: &mut bool,
) {
    match command {
        Command::Attach {
            client_id,
            document_id,
            events,
            lease,
        } => {
            clients.insert(
                client_id,
                ClientSink {
                    events,
                    pending: VecDeque::new(),
                },
            );
            documents.insert(document_id, DocumentLane::new(client_id, lease));
        }
        Command::Load {
            client_id,
            document_id,
            generation,
            path,
            page_count,
        } => {
            if let Some(lane) = documents
                .get_mut(&document_id)
                .filter(|lane| lane.client_id == client_id && !lane.closing)
            {
                lane.operations.push_back(Operation::Load {
                    generation,
                    path,
                    page_count,
                });
                schedule(document_id, lane, ready, ready_set);
            }
        }
        Command::Save {
            client_id,
            document_id,
            generation,
            path,
            identity,
            expected_disk_revision,
            annotations,
        } => {
            if let Some(lane) = documents
                .get_mut(&document_id)
                .filter(|lane| lane.client_id == client_id && !lane.closing)
            {
                let mut coalesced = false;
                if let Some(Operation::Save {
                    generation: pending_generation,
                    path: pending_path,
                    identity: pending_identity,
                    annotations: pending_annotations,
                    ..
                }) = lane.operations.back_mut()
                    && *pending_generation == generation
                    && *pending_path == path
                    && annotations.revision() >= pending_annotations.revision()
                {
                    *pending_identity = identity.clone();
                    *pending_annotations = annotations.clone();
                    coalesced = true;
                }
                if !coalesced {
                    lane.operations.push_back(Operation::Save {
                        generation,
                        path,
                        identity,
                        expected_disk_revision,
                        annotations,
                    });
                }
                schedule(document_id, lane, ready, ready_set);
            }
        }
        Command::Inspect { reply } => inspections.push(reply),
        Command::Wake => {}
        Command::Shutdown => {
            *shutting_down = true;
            clients.clear();
            for (document_id, lane) in documents.iter_mut() {
                if !lane.closing {
                    lane.closing = true;
                    lane.operations.push_back(Operation::Detach);
                    schedule(*document_id, lane, ready, ready_set);
                }
            }
        }
    }
}

fn reap_detached(
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    documents: &mut HashMap<AnnotationDocumentId, DocumentLane>,
    ready: &mut VecDeque<AnnotationDocumentId>,
    ready_set: &mut HashSet<AnnotationDocumentId>,
) {
    for (document_id, lane) in documents.iter_mut() {
        let detached = lane
            .lease
            .upgrade()
            .is_none_or(|lease| lease.detached.load(Ordering::Acquire));
        if detached && !lane.closing {
            // No receiver is expected after client drop. Discarding queued UI
            // notifications lets accepted disk writes drain without coupling
            // document cleanup to event backpressure.
            clients.remove(&lane.client_id);
            lane.closing = true;
            lane.operations.push_back(Operation::Detach);
            schedule(*document_id, lane, ready, ready_set);
        }
    }
}

fn flush_all_clients(clients: &mut HashMap<AnnotationClientId, ClientSink>) {
    clients.retain(|_, sink| flush_sink(sink));
}

fn client_can_accept_event(
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    client_id: AnnotationClientId,
) -> bool {
    let Some(sink) = clients.get_mut(&client_id) else {
        return true;
    };
    if !flush_sink(sink) {
        clients.remove(&client_id);
        return true;
    }
    sink.pending.len() < PENDING_EVENT_CAPACITY
}

fn flush_sink(sink: &mut ClientSink) -> bool {
    while let Some(event) = sink.pending.pop_front() {
        match sink.events.try_send(event) {
            Ok(()) => {}
            Err(flume::TrySendError::Full(event)) => {
                sink.pending.push_front(event);
                return true;
            }
            Err(flume::TrySendError::Disconnected(_)) => return false,
        }
    }
    true
}

fn schedule(
    document_id: AnnotationDocumentId,
    lane: &mut DocumentLane,
    ready: &mut VecDeque<AnnotationDocumentId>,
    ready_set: &mut HashSet<AnnotationDocumentId>,
) {
    if !lane.scheduled && ready_set.insert(document_id) {
        lane.scheduled = true;
        ready.push_back(document_id);
    }
}

fn process_one(
    document_id: AnnotationDocumentId,
    store: &dyn AnnotationStore,
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    documents: &mut HashMap<AnnotationDocumentId, DocumentLane>,
    ready: &mut VecDeque<AnnotationDocumentId>,
    ready_set: &mut HashSet<AnnotationDocumentId>,
) {
    let Some(mut lane) = documents.remove(&document_id) else {
        return;
    };
    lane.scheduled = false;
    let Some(operation) = lane.operations.pop_front() else {
        documents.insert(document_id, lane);
        return;
    };

    let detach = matches!(operation, Operation::Detach);
    match operation {
        Operation::Load {
            generation,
            path,
            page_count,
        } => {
            lane.active_generation = Some(generation);
            lane.active_path = Some(path.clone());
            lane.observed_disk_revision = None;
            let result = DocumentKey::from_pdf(path, page_count).and_then(|document| {
                store
                    .load(&document)
                    .map(|annotations| (document.identity().clone(), annotations))
            });
            match result {
                Ok((identity, annotations)) => {
                    lane.observed_disk_revision = Some(annotations.revision());
                    send_event(
                        clients,
                        lane.client_id,
                        document_id,
                        generation,
                        AnnotationServiceEventKind::Loaded {
                            identity,
                            annotations,
                        },
                    );
                }
                Err(error) => send_failure(
                    clients,
                    lane.client_id,
                    document_id,
                    generation,
                    AnnotationServiceOperation::Load,
                    None,
                    error,
                ),
            }
        }
        Operation::Save {
            generation,
            path,
            identity,
            expected_disk_revision,
            annotations,
        } => {
            let revision = annotations.revision();
            let stale = lane
                .active_generation
                .is_some_and(|active| active != generation)
                || lane
                    .active_path
                    .as_ref()
                    .is_some_and(|active| active != &path);
            if stale {
                send_event(
                    clients,
                    lane.client_id,
                    document_id,
                    generation,
                    AnnotationServiceEventKind::Failed {
                        operation: AnnotationServiceOperation::Save,
                        revision: Some(revision),
                        message: "ignored a stale annotation document generation".into(),
                    },
                );
            } else {
                lane.active_generation = Some(generation);
                lane.active_path = Some(path.clone());
                let expected = lane
                    .observed_disk_revision
                    .unwrap_or(expected_disk_revision);
                let document = DocumentKey::new(path, identity);
                match store.compare_and_save(&document, expected, &annotations) {
                    Ok(receipt) => {
                        lane.observed_disk_revision = Some(receipt.saved_revision);
                        send_event(
                            clients,
                            lane.client_id,
                            document_id,
                            generation,
                            AnnotationServiceEventKind::Saved {
                                revision: receipt.saved_revision,
                            },
                        );
                    }
                    Err(error) => send_failure(
                        clients,
                        lane.client_id,
                        document_id,
                        generation,
                        AnnotationServiceOperation::Save,
                        Some(revision),
                        error,
                    ),
                }
            }
        }
        Operation::Detach => {
            clients.remove(&lane.client_id);
        }
    }

    if !detach {
        if !lane.operations.is_empty() {
            schedule(document_id, &mut lane, ready, ready_set);
        }
        documents.insert(document_id, lane);
    }
}

fn send_failure(
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    client_id: AnnotationClientId,
    document_id: AnnotationDocumentId,
    generation: u64,
    operation: AnnotationServiceOperation,
    revision: Option<u64>,
    error: StoreError,
) {
    send_event(
        clients,
        client_id,
        document_id,
        generation,
        AnnotationServiceEventKind::Failed {
            operation,
            revision,
            message: error.to_string(),
        },
    );
}

fn send_event(
    clients: &mut HashMap<AnnotationClientId, ClientSink>,
    client_id: AnnotationClientId,
    document_id: AnnotationDocumentId,
    generation: u64,
    kind: AnnotationServiceEventKind,
) {
    let event = AnnotationServiceEvent {
        client_id,
        document_id,
        generation,
        kind,
    };
    let Some(sink) = clients.get_mut(&client_id) else {
        return;
    };
    if sink.pending.is_empty() {
        match sink.events.try_send(event) {
            Ok(()) => {}
            Err(flume::TrySendError::Full(event)) => sink.pending.push_back(event),
            Err(flume::TrySendError::Disconnected(_)) => {
                clients.remove(&client_id);
            }
        }
    } else {
        // `process_one()` is admitted only while this bounded queue has room.
        debug_assert!(sink.pending.len() < PENDING_EVENT_CAPACITY);
        sink.pending.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryAnnotationStore, SaveReceipt};
    use key_pdf_core::{HighlightColor, TextPosition, TextRange};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_pdf(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "key-annotation-service-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("document.pdf");
        std::fs::write(&path, format!("fixture {name}")).unwrap();
        path
    }

    fn revision(page_count: usize, color: HighlightColor) -> AnnotationSet {
        let mut annotations = AnnotationSet::new(page_count);
        annotations
            .add(
                TextRange::new(
                    TextPosition { page: 0, index: 0 },
                    TextPosition { page: 0, index: 1 },
                ),
                Some(color),
                None,
            )
            .unwrap();
        annotations
    }

    fn receive(events: &flume::Receiver<AnnotationServiceEvent>) -> AnnotationServiceEventKind {
        events.recv_timeout(Duration::from_secs(2)).unwrap().kind
    }

    #[test]
    fn documents_are_isolated_and_saves_advance_independently() {
        let service = AnnotationService::start_with_store(Arc::new(MemoryAnnotationStore::new()));
        let (first, first_events) = service.attach().unwrap();
        let (second, second_events) = service.attach().unwrap();
        let first_path = temp_pdf("isolation-first");
        let second_path = temp_pdf("isolation-second");
        let first_identity = DocumentIdentity::from_pdf(&first_path, 1).unwrap();
        let second_identity = DocumentIdentity::from_pdf(&second_path, 1).unwrap();

        assert!(first.load(1, first_path.clone(), 1));
        assert!(second.load(2, second_path.clone(), 1));
        assert!(matches!(
            receive(&first_events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(matches!(
            receive(&second_events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(first.save(
            1,
            first_path,
            first_identity,
            0,
            revision(1, HighlightColor::Green),
        ));
        assert!(second.save(
            2,
            second_path,
            second_identity,
            0,
            revision(1, HighlightColor::Purple),
        ));
        assert!(matches!(
            receive(&first_events),
            AnnotationServiceEventKind::Saved { revision: 1 }
        ));
        assert!(matches!(
            receive(&second_events),
            AnnotationServiceEventKind::Saved { revision: 1 }
        ));
    }

    #[test]
    fn stale_generation_is_rejected_before_it_reaches_storage() {
        let store = Arc::new(MemoryAnnotationStore::new());
        let service = AnnotationService::start_with_store(store);
        let (client, events) = service.attach().unwrap();
        let path = temp_pdf("stale-generation");
        let identity = DocumentIdentity::from_pdf(&path, 1).unwrap();

        assert!(client.load(8, path.clone(), 1));
        assert!(matches!(
            receive(&events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(client.save(7, path, identity, 0, revision(1, HighlightColor::Yellow),));
        match receive(&events) {
            AnnotationServiceEventKind::Failed { message, .. } => {
                assert!(message.contains("stale annotation document generation"));
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn stale_second_writer_cannot_overwrite_the_first() {
        let store = Arc::new(MemoryAnnotationStore::new());
        let service = AnnotationService::start_with_store(store);
        let (first, first_events) = service.attach().unwrap();
        let (second, second_events) = service.attach().unwrap();
        let path = temp_pdf("stale-writer");
        let identity = DocumentIdentity::from_pdf(&path, 1).unwrap();

        assert!(first.load(1, path.clone(), 1));
        assert!(second.load(1, path.clone(), 1));
        assert!(matches!(
            receive(&first_events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(matches!(
            receive(&second_events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(first.save(
            1,
            path.clone(),
            identity.clone(),
            0,
            revision(1, HighlightColor::Green),
        ));
        assert!(matches!(
            receive(&first_events),
            AnnotationServiceEventKind::Saved { revision: 1 }
        ));
        assert!(second.save(1, path, identity, 0, revision(1, HighlightColor::Pink),));
        assert!(matches!(
            receive(&second_events),
            AnnotationServiceEventKind::Failed {
                operation: AnnotationServiceOperation::Save,
                ..
            }
        ));
    }

    struct TracingStore {
        inner: MemoryAnnotationStore,
        trace: Arc<Mutex<Vec<(PathBuf, AnnotationServiceOperation)>>>,
        blocked_path: PathBuf,
        gate: Arc<(Mutex<(bool, bool)>, std::sync::Condvar)>,
    }

    impl AnnotationStore for TracingStore {
        fn load(&self, document: &DocumentKey) -> Result<AnnotationSet, StoreError> {
            self.trace.lock().unwrap().push((
                document.source_path().to_path_buf(),
                AnnotationServiceOperation::Load,
            ));
            if document.source_path() == self.blocked_path {
                let (state, changed) = &*self.gate;
                let mut state = state.lock().unwrap();
                state.0 = true;
                changed.notify_all();
                while !state.1 {
                    state = changed.wait(state).unwrap();
                }
            }
            self.inner.load(document)
        }

        fn compare_and_save(
            &self,
            document: &DocumentKey,
            expected_revision: u64,
            annotations: &AnnotationSet,
        ) -> Result<SaveReceipt, StoreError> {
            self.trace.lock().unwrap().push((
                document.source_path().to_path_buf(),
                AnnotationServiceOperation::Save,
            ));
            self.inner
                .compare_and_save(document, expected_revision, annotations)
        }
    }

    #[test]
    fn ready_documents_are_served_round_robin() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let first_path = temp_pdf("fair-first");
        let second_path = temp_pdf("fair-second");
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let service = AnnotationService::start_with_store(Arc::new(TracingStore {
            inner: MemoryAnnotationStore::new(),
            trace: Arc::clone(&trace),
            blocked_path: first_path.clone(),
            gate: Arc::clone(&gate),
        }));
        let (first, first_events) = service.attach().unwrap();
        let (second, second_events) = service.attach().unwrap();
        let first_identity = DocumentIdentity::from_pdf(&first_path, 1).unwrap();
        let second_identity = DocumentIdentity::from_pdf(&second_path, 1).unwrap();

        assert!(first.load(1, first_path.clone(), 1));
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        }
        assert!(first.save(
            1,
            first_path.clone(),
            first_identity,
            0,
            revision(1, HighlightColor::Green),
        ));
        assert!(second.load(2, second_path.clone(), 1));
        assert!(second.save(
            2,
            second_path.clone(),
            second_identity,
            0,
            revision(1, HighlightColor::Blue),
        ));
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            state.1 = true;
            changed.notify_all();
        }
        for events in [&first_events, &second_events] {
            assert!(matches!(
                receive(events),
                AnnotationServiceEventKind::Loaded { .. }
            ));
            assert!(matches!(
                receive(events),
                AnnotationServiceEventKind::Saved { .. }
            ));
        }

        assert_eq!(
            *trace.lock().unwrap(),
            vec![
                (first_path.clone(), AnnotationServiceOperation::Load),
                (second_path.clone(), AnnotationServiceOperation::Load),
                // One operation per lane is serviced before either lane gets its
                // second turn.
                (first_path, AnnotationServiceOperation::Save),
                (second_path, AnnotationServiceOperation::Save),
            ]
        );
    }

    #[test]
    fn saturated_event_client_does_not_drop_events_or_block_another_document() {
        let service = AnnotationService::start_with_store(Arc::new(MemoryAnnotationStore::new()));
        let (saturated, saturated_events) = service.attach().unwrap();
        let (peer, peer_events) = service.attach().unwrap();
        let saturated_path = temp_pdf("event-saturation");
        let peer_path = temp_pdf("event-saturation-peer");
        let event_count = EVENT_CAPACITY + PENDING_EVENT_CAPACITY + 8;

        for generation in 0..event_count {
            assert!(saturated.load(
                u64::try_from(generation).unwrap(),
                saturated_path.clone(),
                1,
            ));
        }
        assert!(peer.load(999, peer_path, 1));

        // The saturated lane is paused once both bounded delivery tiers are
        // full, but round-robin scheduling still serves the peer.
        assert!(matches!(
            receive(&peer_events),
            AnnotationServiceEventKind::Loaded { .. }
        ));

        for generation in 0..event_count {
            let event = saturated_events
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            assert_eq!(event.generation, u64::try_from(generation).unwrap());
            assert!(matches!(
                event.kind,
                AnnotationServiceEventKind::Loaded { .. }
            ));
        }
        assert!(matches!(
            saturated_events.recv_timeout(Duration::from_millis(50)),
            Err(flume::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn dropping_client_never_blocks_on_a_full_mailbox_or_slow_store() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let path = temp_pdf("nonblocking-detach");
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let store = Arc::new(TracingStore {
            inner: MemoryAnnotationStore::new(),
            trace: Arc::clone(&trace),
            blocked_path: path.clone(),
            gate: Arc::clone(&gate),
        });
        let service = AnnotationService::start_with_store(store);
        let (client, _events) = service.attach().unwrap();
        let identity = DocumentIdentity::from_pdf(&path, 1).unwrap();
        let annotations = revision(1, HighlightColor::Green);

        assert!(client.load(1, path.clone(), 1));
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        }
        let mut accepted = 0;
        while client.save(1, path.clone(), identity.clone(), 0, annotations.clone()) {
            accepted += 1;
        }
        assert!(accepted >= COMMAND_CAPACITY / 2);

        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let drop_thread = thread::spawn(move || {
            drop(client);
            let _ = dropped_tx.send(());
        });
        let prompt_drop = dropped_rx.recv_timeout(Duration::from_millis(100));

        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            state.1 = true;
            changed.notify_all();
        }
        drop_thread.join().unwrap();
        assert!(
            prompt_drop.is_ok(),
            "client drop blocked behind the saturated mailbox"
        );
        assert_eq!(
            service.diagnostics(Duration::from_secs(2)).unwrap(),
            AnnotationServiceDiagnostics::default()
        );
        assert_eq!(
            trace
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, operation)| *operation == AnnotationServiceOperation::Save)
                .count(),
            1
        );
    }

    #[test]
    fn adjacent_saves_collapse_to_the_latest_document_snapshot() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let path = temp_pdf("coalesce");
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let service = AnnotationService::start_with_store(Arc::new(TracingStore {
            inner: MemoryAnnotationStore::new(),
            trace: Arc::clone(&trace),
            blocked_path: path.clone(),
            gate: Arc::clone(&gate),
        }));
        let (client, events) = service.attach().unwrap();
        let identity = DocumentIdentity::from_pdf(&path, 1).unwrap();
        let first = revision(1, HighlightColor::Green);
        let mut latest = first.clone();
        latest
            .add(
                TextRange::new(
                    TextPosition { page: 0, index: 3 },
                    TextPosition { page: 0, index: 4 },
                ),
                Some(HighlightColor::Purple),
                None,
            )
            .unwrap();

        assert!(client.load(4, path.clone(), 1));
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        }
        assert!(client.save(4, path.clone(), identity.clone(), 0, first));
        assert!(client.save(4, path.clone(), identity, 0, latest));
        {
            let (state, changed) = &*gate;
            let mut state = state.lock().unwrap();
            state.1 = true;
            changed.notify_all();
        }

        assert!(matches!(
            receive(&events),
            AnnotationServiceEventKind::Loaded { .. }
        ));
        assert!(matches!(
            receive(&events),
            AnnotationServiceEventKind::Saved { revision: 2 }
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_millis(100)),
            Err(flume::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            *trace.lock().unwrap(),
            vec![
                (path.clone(), AnnotationServiceOperation::Load),
                (path, AnnotationServiceOperation::Save),
            ]
        );
    }

    #[test]
    fn detach_flushes_and_removes_client_and_document_state() {
        let store = Arc::new(MemoryAnnotationStore::new());
        let service = AnnotationService::start_with_store(store.clone());
        let (client, _events) = service.attach().unwrap();
        let path = temp_pdf("detach");
        let identity = DocumentIdentity::from_pdf(&path, 1).unwrap();
        let document = DocumentKey::new(path.clone(), identity.clone());
        assert!(client.save(3, path, identity, 0, revision(1, HighlightColor::Yellow),));
        drop(client);
        assert_eq!(
            service.diagnostics(Duration::from_secs(2)).unwrap(),
            AnnotationServiceDiagnostics::default()
        );
        assert_eq!(store.load(&document).unwrap().revision(), 1);
    }
}
