use super::protocol::{
    PreviewSpec, RenderAppearance, RenderRequest, TileRequest, WorkerCommand, WorkerEvent,
};
use super::worker::run_worker;
use crate::search::SearchQuery;
use key_pdf_runtime::CancellationSource;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

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

#[derive(Clone, Debug)]
pub struct PdfWorker {
    commands: mpsc::Sender<WorkerCommand>,
    cancellations: Arc<WorkerCancellations>,
}

impl PdfWorker {
    pub fn start() -> (Self, mpsc::Receiver<WorkerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        // A tile is under five MiB. Back-pressure prevents bitmap copies from
        // accumulating if the UI thread is temporarily busy.
        let (event_tx, event_rx) = mpsc::sync_channel(1);
        let cancellations = Arc::new(WorkerCancellations::default());

        thread::Builder::new()
            .name("pdfium-renderer".into())
            .spawn(move || run_worker(command_rx, event_tx))
            .expect("failed to start the PDFium renderer thread");

        (
            Self {
                commands: command_tx,
                cancellations,
            },
            event_rx,
        )
    }

    pub fn open(&self, generation: u64, path: PathBuf) -> bool {
        let cancellation = self.cancellations.begin_document();
        self.commands
            .send(WorkerCommand::Open {
                generation,
                path,
                cancellation,
            })
            .is_ok()
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
        self.commands
            .send(WorkerCommand::RenderViewport {
                generation,
                requests,
                text_pages: text_pages.to_vec(),
                cancellation,
            })
            .is_ok()
    }

    pub fn extract_text(&self, generation: u64, page: usize) -> bool {
        let cancellation = self.cancellations.replace_explicit_text();
        self.commands
            .send(WorkerCommand::ExtractText {
                generation,
                page,
                cancellation,
            })
            .is_ok()
    }

    pub fn ensure_text_pages(&self, generation: u64, pages: Vec<usize>) -> bool {
        let cancellation = self.cancellations.retain_or_begin_explicit_text();
        self.commands
            .send(WorkerCommand::EnsureTextPages {
                generation,
                pages,
                cancellation,
            })
            .is_ok()
    }

    pub fn cancel_explicit_text(&self, generation: u64) -> bool {
        self.cancellations.cancel_explicit_text();
        self.commands
            .send(WorkerCommand::CancelExplicitText { generation })
            .is_ok()
    }

    pub fn search(&self, generation: u64, revision: u64, query: SearchQuery) -> bool {
        let cancellation = self.cancellations.replace_search();
        self.commands
            .send(WorkerCommand::Search {
                generation,
                revision,
                query,
                cancellation,
            })
            .is_ok()
    }

    /// Cancels search revisions older than `next_revision` immediately while
    /// allowing a debounced replacement with `next_revision` to start later.
    pub fn cancel_search(&self, generation: u64, next_revision: u64) -> bool {
        self.cancellations.cancel_search();
        self.commands
            .send(WorkerCommand::CancelSearch {
                generation,
                next_revision,
            })
            .is_ok()
    }

    pub fn render_preview(
        &self,
        generation: u64,
        revision: u64,
        appearance: RenderAppearance,
        spec: PreviewSpec,
    ) -> bool {
        let cancellation = self.cancellations.replace_preview();
        self.commands
            .send(WorkerCommand::RenderPreview {
                generation,
                revision,
                appearance,
                spec,
                cancellation,
            })
            .is_ok()
    }
}
