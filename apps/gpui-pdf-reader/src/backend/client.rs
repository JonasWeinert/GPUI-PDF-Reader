use super::protocol::{
    PreviewSpec, RenderAppearance, RenderRequest, TileRequest, WorkerCommand, WorkerEvent,
};
use super::worker::{AdapterInput, PdfDocumentClient, PdfEngineSupervisor, run_worker};
use crate::search::SearchQuery;
use key_pdf_runtime::CancellationSource;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

const DOCUMENT_MAILBOX_CAPACITY: usize = 128;
const DOCUMENT_ORCHESTRATOR_STACK_BYTES: usize = 512 * 1024;

#[derive(Debug)]
struct WorkerCancellationState {
    document: CancellationSource,
    render: Option<CancellationSource>,
    preview: Option<CancellationSource>,
    explicit_text: Option<CancellationSource>,
    search: Option<CancellationSource>,
}

impl Default for WorkerCancellationState {
    fn default() -> Self {
        Self {
            document: CancellationSource::new(),
            render: None,
            preview: None,
            explicit_text: None,
            search: None,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct WorkerCancellations {
    state: Mutex<WorkerCancellationState>,
}

impl WorkerCancellations {
    pub(super) fn begin_document(&self) -> CancellationSource {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.document.cancel();
        cancel_slot(&mut state.render);
        cancel_slot(&mut state.preview);
        cancel_slot(&mut state.explicit_text);
        cancel_slot(&mut state.search);
        state.document = CancellationSource::new();
        state.document.clone()
    }

    pub(super) fn replace_render(&self) -> CancellationSource {
        self.replace(|state| &mut state.render)
    }

    pub(super) fn replace_preview(&self) -> CancellationSource {
        self.replace(|state| &mut state.preview)
    }

    pub(super) fn replace_explicit_text(&self) -> CancellationSource {
        self.replace(|state| &mut state.explicit_text)
    }

    pub(super) fn retain_or_begin_explicit_text(&self) -> CancellationSource {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .explicit_text
            .get_or_insert_with(CancellationSource::new)
            .clone()
    }

    pub(super) fn replace_search(&self) -> CancellationSource {
        self.replace(|state| &mut state.search)
    }

    pub(super) fn cancel_explicit_text(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        cancel_slot(&mut state.explicit_text);
    }

    pub(super) fn cancel_search(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        cancel_slot(&mut state.search);
    }

    fn replace(
        &self,
        select: impl FnOnce(&mut WorkerCancellationState) -> &mut Option<CancellationSource>,
    ) -> CancellationSource {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let slot = select(&mut state);
        cancel_slot(slot);
        let source = CancellationSource::new();
        *slot = Some(source.clone());
        source
    }
}

fn cancel_slot(slot: &mut Option<CancellationSource>) {
    if let Some(source) = slot.take() {
        source.cancel();
    }
}

#[derive(Clone)]
pub struct PdfWorker {
    commands: mpsc::SyncSender<AdapterInput>,
    cancellations: Arc<WorkerCancellations>,
    engine_document: PdfDocumentClient,
    _lifetime: Arc<WorkerLifetime>,
}

struct WorkerLifetime {
    commands: mpsc::SyncSender<AdapterInput>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        // This is the last public adapter handle. The flag guarantees a full
        // mailbox cannot strand the orchestration thread: it checks the flag
        // after consuming any queued input. The wake handles an empty mailbox.
        self.shutdown.store(true, Ordering::Release);
        let _ = self.commands.try_send(AdapterInput::Shutdown);
    }
}

impl PdfWorker {
    pub(crate) fn start(supervisor: PdfEngineSupervisor) -> (Self, mpsc::Receiver<WorkerEvent>) {
        // Commands and routed engine events share one bounded mailbox. The
        // orchestration thread can therefore sleep without polling or a
        // second forwarding thread, while a busy document remains bounded.
        let (command_tx, command_rx) = mpsc::sync_channel(DOCUMENT_MAILBOX_CAPACITY);
        // A tile is under five MiB. Back-pressure prevents bitmap copies from
        // accumulating if the UI thread is temporarily busy.
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        let cancellations = Arc::new(WorkerCancellations::default());
        // Preserve the old one-event raster back-pressure even though engine
        // events and commands now share the orchestration mailbox.
        let supervisor_event_pending = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let routed = command_tx.clone();
        let route_pending = supervisor_event_pending.clone();
        let document = supervisor
            .attach_routed(move |event| {
                if route_pending.swap(true, Ordering::AcqRel) {
                    return Err(key_pdf_runtime::SupervisorRouteError::Full(event));
                }
                match routed.try_send(AdapterInput::Supervisor(event)) {
                    Ok(()) => Ok(()),
                    Err(mpsc::TrySendError::Full(AdapterInput::Supervisor(event))) => {
                        route_pending.store(false, Ordering::Release);
                        Err(key_pdf_runtime::SupervisorRouteError::Full(event))
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        route_pending.store(false, Ordering::Release);
                        Err(key_pdf_runtime::SupervisorRouteError::Disconnected)
                    }
                    Err(mpsc::TrySendError::Full(AdapterInput::Command(_))) => {
                        unreachable!("the supervisor route only sends supervisor events")
                    }
                    Err(mpsc::TrySendError::Full(AdapterInput::Shutdown)) => {
                        unreachable!("the supervisor route only sends supervisor events")
                    }
                }
            })
            .expect("the process-wide PDF engine supervisor is unavailable");

        let worker_shutdown = shutdown.clone();
        let adapter_document = document.clone();
        thread::Builder::new()
            .name(format!("pdf-document-{}", document.id().get()))
            .stack_size(DOCUMENT_ORCHESTRATOR_STACK_BYTES)
            .spawn(move || {
                run_worker(
                    command_rx,
                    event_tx,
                    adapter_document,
                    supervisor_event_pending,
                    worker_shutdown,
                )
            })
            .expect("failed to start the PDF document orchestration thread");

        (
            Self {
                commands: command_tx.clone(),
                cancellations,
                engine_document: document.clone(),
                _lifetime: Arc::new(WorkerLifetime {
                    commands: command_tx,
                    shutdown,
                }),
            },
            event_rx,
        )
    }

    pub fn open(&self, generation: u64, path: PathBuf) -> bool {
        let cancellation = self.cancellations.begin_document();
        let _ = self.engine_document.close();
        self.send(WorkerCommand::Open {
            generation,
            path,
            cancellation,
        })
    }

    pub fn render_viewport(
        &self,
        generation: u64,
        appearance: RenderAppearance,
        tiles: &[TileRequest],
        visible_tile_count: usize,
        text_pages: &[usize],
    ) -> bool {
        let cancellation = self.cancellations.replace_render();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::RenderViewport);
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::VisibleText);
        let requests = tiles
            .iter()
            .copied()
            .enumerate()
            .map(|(priority, tile)| RenderRequest {
                generation,
                appearance,
                tile,
                priority,
                prefetch: priority >= visible_tile_count,
            })
            .collect();
        self.send(WorkerCommand::RenderViewport {
            generation,
            requests,
            text_pages: text_pages.to_vec(),
            cancellation,
        })
    }

    pub fn extract_text(&self, generation: u64, page: usize) -> bool {
        let cancellation = self.cancellations.replace_explicit_text();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::CopyText);
        self.send(WorkerCommand::ExtractText {
            generation,
            page,
            cancellation,
        })
    }

    pub fn ensure_text_pages(&self, generation: u64, pages: Vec<usize>) -> bool {
        let cancellation = self.cancellations.retain_or_begin_explicit_text();
        self.send(WorkerCommand::EnsureTextPages {
            generation,
            pages,
            cancellation,
        })
    }

    pub fn cancel_explicit_text(&self, generation: u64) -> bool {
        self.cancellations.cancel_explicit_text();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::CopyText);
        self.send(WorkerCommand::CancelExplicitText { generation })
    }

    pub fn search(&self, generation: u64, revision: u64, query: SearchQuery) -> bool {
        let cancellation = self.cancellations.replace_search();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::SearchText);
        self.send(WorkerCommand::Search {
            generation,
            revision,
            query,
            cancellation,
        })
    }

    /// Cancels search revisions older than `next_revision` immediately while
    /// allowing a debounced replacement with `next_revision` to start later.
    pub fn cancel_search(&self, generation: u64, next_revision: u64) -> bool {
        self.cancellations.cancel_search();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::SearchText);
        self.send(WorkerCommand::CancelSearch {
            generation,
            next_revision,
        })
    }

    pub fn render_preview(
        &self,
        generation: u64,
        revision: u64,
        appearance: RenderAppearance,
        spec: PreviewSpec,
    ) -> bool {
        let cancellation = self.cancellations.replace_preview();
        let _ = self
            .engine_document
            .cancel(key_pdf_runtime::WorkClass::Preview);
        self.send(WorkerCommand::RenderPreview {
            generation,
            revision,
            appearance,
            spec,
            cancellation,
        })
    }

    fn send(&self, command: WorkerCommand) -> bool {
        self.commands
            .try_send(AdapterInput::Command(command))
            .is_ok()
    }
}
