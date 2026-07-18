use crate::model::{
    PageSize, PdfLink as DocumentLink, PixelRect, RasterSize, TextLayer, TileKey, TocEntry,
};
use crate::scientific::{ScientificAnalysis, ScientificAnalyzer};
use crate::search::{
    MAX_SEARCH_RESULTS, SearchPageOutcome, SearchPageResults, SearchQuery, search_page,
};
use key_pdf_runtime::{
    CachePolicy, CancellationSource, CancellationToken, ColorMode, CompletionDisposition,
    DemandIntent, DemandPriority, DocumentEvent, LatestWinsQueue, PdfRuntime, PixelColor,
    PixelFormat, PreviewDemand, PreviewEvent, RenderDemand, RenderEvent, ScheduleOutcome,
    ScheduledDemand, TextDemandPurpose, TextEvent,
};
use key_pdfium::{PdfiumDocumentSource, PdfiumEngine, PdfiumLibraryConfig};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const AUTOMATIC_TEXT_IDLE_DELAY: Duration = Duration::from_millis(200);
const MAX_SEARCH_HIGHLIGHT_RUNS: usize = 100_000;
const MAX_PENDING_RENDER_DEMANDS_PER_TIER: usize = 4_096;

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
struct WorkerCancellations {
    state: Mutex<WorkerCancellationState>,
}

impl WorkerCancellations {
    fn begin_document(&self) -> CancellationSource {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.document.cancel();
        cancel_slot(&mut state.render);
        cancel_slot(&mut state.preview);
        cancel_slot(&mut state.explicit_text);
        cancel_slot(&mut state.search);
        state.document = CancellationSource::new();
        state.document.clone()
    }

    fn replace_render(&self) -> CancellationSource {
        self.replace(|state| &mut state.render)
    }

    fn replace_preview(&self) -> CancellationSource {
        self.replace(|state| &mut state.preview)
    }

    fn replace_explicit_text(&self) -> CancellationSource {
        self.replace(|state| &mut state.explicit_text)
    }

    fn retain_or_begin_explicit_text(&self) -> CancellationSource {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .explicit_text
            .get_or_insert_with(CancellationSource::new)
            .clone()
    }

    fn replace_search(&self) -> CancellationSource {
        self.replace(|state| &mut state.search)
    }

    fn cancel_explicit_text(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        cancel_slot(&mut state.explicit_text);
    }

    fn cancel_search(&self) {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RenderAppearance {
    #[default]
    Normal,
    ForcedColors {
        background: RenderColor,
        foreground: RenderColor,
    },
}

impl RenderAppearance {
    fn color_mode(self) -> ColorMode {
        match self {
            Self::Normal => ColorMode::Original,
            Self::ForcedColors {
                background,
                foreground,
            } => ColorMode::Forced {
                background: runtime_color(background),
                foreground: runtime_color(foreground),
            },
        }
    }
}

fn runtime_color(color: RenderColor) -> PixelColor {
    PixelColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: u8::MAX,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileRequest {
    pub key: TileKey,
    pub core_rect: PixelRect,
    pub render_rect: PixelRect,
}

#[derive(Clone, Copy, Debug)]
pub struct PreviewSpec {
    pub page: usize,
    pub raster: RasterSize,
    pub center_x: f32,
    pub center_y: f32,
}

#[derive(Debug)]
pub enum WorkerEvent {
    Ready,
    Opened {
        generation: u64,
        path: PathBuf,
        pages: Vec<PageSize>,
        toc: Vec<TocEntry>,
        links: Vec<DocumentLink>,
    },
    TileRendered {
        generation: u64,
        appearance: RenderAppearance,
        key: TileKey,
        core_rect: PixelRect,
        render_rect: PixelRect,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
    TileFailed {
        generation: u64,
        appearance: RenderAppearance,
        key: TileKey,
        message: String,
    },
    PreviewRendered {
        generation: u64,
        revision: u64,
        appearance: RenderAppearance,
        width: u32,
        height: u32,
        bgra: Vec<u8>,
    },
    PreviewFailed {
        generation: u64,
        revision: u64,
        message: String,
    },
    TextExtracted {
        generation: u64,
        page: usize,
        text: Arc<TextLayer>,
    },
    TextFailed {
        generation: u64,
        page: usize,
        message: String,
    },
    SearchPageResults {
        generation: u64,
        revision: u64,
        results: SearchPageResults,
    },
    SearchFinished {
        generation: u64,
        revision: u64,
        searched_pages: usize,
        total_results: usize,
        total_highlight_runs: usize,
        skipped_pages: usize,
        truncated: bool,
    },
    SearchWarning {
        generation: u64,
        revision: u64,
        page: usize,
        message: String,
    },
    SearchFailed {
        generation: u64,
        revision: u64,
        message: String,
    },
    ScientificAnalysisComplete {
        generation: u64,
        analysis: ScientificAnalysis,
    },
    Error {
        generation: Option<u64>,
        message: String,
    },
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

#[derive(Debug)]
enum WorkerCommand {
    Open {
        generation: u64,
        path: PathBuf,
        cancellation: CancellationSource,
    },
    RenderViewport {
        generation: u64,
        requests: Vec<RenderRequest>,
        text_pages: Vec<usize>,
        cancellation: CancellationSource,
    },
    ExtractText {
        generation: u64,
        page: usize,
        cancellation: CancellationSource,
    },
    EnsureTextPages {
        generation: u64,
        pages: Vec<usize>,
        cancellation: CancellationSource,
    },
    CancelExplicitText {
        generation: u64,
    },
    Search {
        generation: u64,
        revision: u64,
        query: SearchQuery,
        cancellation: CancellationSource,
    },
    CancelSearch {
        generation: u64,
        next_revision: u64,
    },
    RenderPreview {
        generation: u64,
        revision: u64,
        appearance: RenderAppearance,
        spec: PreviewSpec,
        cancellation: CancellationSource,
    },
}

#[derive(Clone, Debug)]
struct RenderRequest {
    generation: u64,
    appearance: RenderAppearance,
    tile: TileRequest,
    priority: usize,
    prefetch: bool,
}

#[derive(Clone, Debug)]
struct QueuedRender {
    request: RenderRequest,
    demand: RenderDemand,
    cancellation: CancellationSource,
}

#[derive(Clone, Debug)]
struct PreviewRequest {
    generation: u64,
    revision: u64,
    appearance: RenderAppearance,
    demand: PreviewDemand,
    cancellation: CancellationSource,
}

#[derive(Clone, Debug)]
struct SearchJob {
    generation: u64,
    revision: u64,
    query: SearchQuery,
    next_page: usize,
    page_count: usize,
    total_results: usize,
    total_highlight_runs: usize,
    skipped_pages: usize,
    truncated: bool,
    cancellation: CancellationSource,
}

struct ScientificJob {
    generation: u64,
    analyzer: ScientificAnalyzer,
    next_page: usize,
}

#[derive(Clone, Copy)]
enum RenderTier {
    Visible,
    Prefetch,
}

struct RenderQueues {
    visible: LatestWinsQueue<TileKey, QueuedRender>,
    prefetch: LatestWinsQueue<TileKey, QueuedRender>,
}

impl RenderQueues {
    fn new() -> Self {
        Self {
            visible: LatestWinsQueue::new(MAX_PENDING_RENDER_DEMANDS_PER_TIER),
            prefetch: LatestWinsQueue::new(MAX_PENDING_RENDER_DEMANDS_PER_TIER),
        }
    }

    fn is_empty(&self) -> bool {
        self.visible.is_empty() && self.prefetch.is_empty()
    }

    fn clear_pending(&mut self) {
        self.visible.clear_pending();
        self.prefetch.clear_pending();
    }

    fn schedule(
        &mut self,
        tier: RenderTier,
        queued: QueuedRender,
    ) -> ScheduleOutcome<TileKey, QueuedRender> {
        let key = queued.request.tile.key;
        let priority = queued.demand.stamp().priority();
        match tier {
            RenderTier::Visible => self.visible.schedule(key, priority, queued),
            RenderTier::Prefetch => self.prefetch.schedule(key, priority, queued),
        }
    }

    fn pop_visible(&mut self) -> Option<ScheduledDemand<TileKey, QueuedRender>> {
        self.visible.pop_next()
    }

    fn pop_prefetch(&mut self) -> Option<ScheduledDemand<TileKey, QueuedRender>> {
        self.prefetch.pop_next()
    }

    fn finish(
        &mut self,
        tier: RenderTier,
        demand: &ScheduledDemand<TileKey, QueuedRender>,
    ) -> CompletionDisposition {
        match tier {
            RenderTier::Visible => self.visible.finish(demand),
            RenderTier::Prefetch => self.prefetch.finish(demand),
        }
    }
}

struct WorkerState {
    runtime: PdfRuntime<PdfiumEngine>,
    generation: Option<u64>,
    document_cancellation: CancellationSource,
    automatic_text_cancellation: CancellationSource,
    explicit_text_cancellation: CancellationSource,
    text_cache: HashMap<usize, Arc<TextLayer>>,
    automatic_text_needs_quiet: bool,
    page_count: usize,
    search: Option<SearchJob>,
    latest_search_revision: Option<u64>,
    scientific: Option<ScientificJob>,
    renders: RenderQueues,
    previews: LatestWinsQueue<(), PreviewRequest>,
}

fn run_worker(commands: mpsc::Receiver<WorkerCommand>, events: mpsc::SyncSender<WorkerEvent>) {
    // Constructing the adapter on this thread permanently binds PDFium and all
    // documents to the single renderer owner thread.
    let engine = PdfiumEngine::new(pdfium_library_config());
    let runtime = PdfRuntime::new(engine, CachePolicy::default());
    let mut state = WorkerState {
        runtime,
        generation: None,
        document_cancellation: CancellationSource::new(),
        automatic_text_cancellation: CancellationSource::new(),
        explicit_text_cancellation: CancellationSource::new(),
        text_cache: HashMap::new(),
        automatic_text_needs_quiet: false,
        page_count: 0,
        search: None,
        latest_search_revision: None,
        scientific: None,
        renders: RenderQueues::new(),
        previews: LatestWinsQueue::new(1),
    };
    if events.send(WorkerEvent::Ready).is_err() {
        return;
    }

    let mut explicit_text = BTreeSet::<usize>::new();
    let mut automatic_text = VecDeque::<usize>::new();

    loop {
        if state.renders.is_empty()
            && state.previews.is_empty()
            && explicit_text.is_empty()
            && automatic_text.is_empty()
            && state.search.is_none()
            && state.scientific.is_none()
        {
            match commands.recv() {
                Ok(command) => {
                    if !accept_command(
                        command,
                        &events,
                        &mut state,
                        &mut explicit_text,
                        &mut automatic_text,
                    ) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }

        if !accept_available_commands(
            &commands,
            &events,
            &mut state,
            &mut explicit_text,
            &mut automatic_text,
        ) {
            return;
        }

        if let Some(demand) = state.renders.pop_visible() {
            if !process_render_work(
                RenderTier::Visible,
                demand,
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if let Some(demand) = state.previews.pop_next() {
            if !process_preview_work(
                demand,
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if let Some(page) = explicit_text.pop_first() {
            if !process_text_work(
                page,
                true,
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if let Some(page) = automatic_text.front().copied() {
            if !state.text_cache.contains_key(&page) && state.automatic_text_needs_quiet {
                // Automatic text work waits briefly for a settled viewport so
                // rapid zooming converges on the newest demand before PDFium's
                // synchronous character walk begins.
                match commands.recv_timeout(AUTOMATIC_TEXT_IDLE_DELAY) {
                    Ok(command) => {
                        if !accept_command(
                            command,
                            &events,
                            &mut state,
                            &mut explicit_text,
                            &mut automatic_text,
                        ) {
                            return;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        state.automatic_text_needs_quiet = false;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            let Some(page) = automatic_text.pop_front() else {
                continue;
            };
            if !process_text_work(
                page,
                false,
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if state.search.is_some() {
            if !process_search_work(
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if let Some(demand) = state.renders.pop_prefetch() {
            if !process_render_work(
                RenderTier::Prefetch,
                demand,
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if state.scientific.is_some()
            && !process_scientific_work(
                &commands,
                &events,
                &mut state,
                &mut explicit_text,
                &mut automatic_text,
            )
        {
            return;
        }
    }
}

fn pdfium_library_config() -> PdfiumLibraryConfig {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.to_path_buf());
        candidates.push(directory.join("../Resources"));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("vendor/pdfium/lib"),
    );
    PdfiumLibraryConfig::new(candidates).with_system_fallback(true)
}

fn process_render_work(
    tier: RenderTier,
    scheduled: ScheduledDemand<TileKey, QueuedRender>,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let operation_cancellation = state
        .document_cancellation
        .token()
        .combined(&scheduled.value().cancellation.token())
        .combined(&scheduled.cancellation());
    let runtime_event = state
        .runtime
        .render_with_cancellation(scheduled.value().demand.clone(), &operation_cancellation);

    // Apply replacement viewport/open commands before deciding whether an
    // in-flight completion is still current.
    if !accept_available_commands(commands, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.renders.finish(tier, &scheduled) != CompletionDisposition::Publish {
        return true;
    }

    let request = &scheduled.value().request;
    if state.generation != Some(request.generation) {
        return true;
    }
    match runtime_event {
        RenderEvent::Ready { tile, .. } => {
            if tile.image.format() != PixelFormat::Bgra8Premultiplied {
                return events
                    .send(WorkerEvent::TileFailed {
                        generation: request.generation,
                        appearance: request.appearance,
                        key: request.tile.key,
                        message: "PDF engine returned an unsupported pixel format".into(),
                    })
                    .is_ok();
            }
            events
                .send(WorkerEvent::TileRendered {
                    generation: request.generation,
                    appearance: request.appearance,
                    key: request.tile.key,
                    core_rect: request.tile.core_rect,
                    render_rect: request.tile.render_rect,
                    width: tile.image.width(),
                    height: tile.image.height(),
                    bgra: tile.image.pixels().to_vec(),
                })
                .is_ok()
        }
        RenderEvent::Failed { error, .. } => events
            .send(WorkerEvent::TileFailed {
                generation: request.generation,
                appearance: request.appearance,
                key: request.tile.key,
                message: format!(
                    "Could not render page {}: {error}",
                    request.tile.key.page + 1
                ),
            })
            .is_ok(),
        RenderEvent::Cancelled { .. } | RenderEvent::Discarded { .. } => true,
    }
}

fn process_preview_work(
    scheduled: ScheduledDemand<(), PreviewRequest>,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let operation_cancellation = state
        .document_cancellation
        .token()
        .combined(&scheduled.value().cancellation.token())
        .combined(&scheduled.cancellation());
    let runtime_event = state.runtime.render_preview_with_cancellation(
        scheduled.value().demand.clone(),
        &operation_cancellation,
    );
    if !accept_available_commands(commands, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.previews.finish(&scheduled) != CompletionDisposition::Publish {
        return true;
    }

    let request = scheduled.value();
    if state.generation != Some(request.generation) {
        return true;
    }
    match runtime_event {
        PreviewEvent::Ready { preview, .. } => {
            if preview.image.format() != PixelFormat::Bgra8Premultiplied {
                return events
                    .send(WorkerEvent::PreviewFailed {
                        generation: request.generation,
                        revision: request.revision,
                        message: "PDF engine returned an unsupported pixel format".into(),
                    })
                    .is_ok();
            }
            events
                .send(WorkerEvent::PreviewRendered {
                    generation: request.generation,
                    revision: request.revision,
                    appearance: request.appearance,
                    width: preview.image.width(),
                    height: preview.image.height(),
                    bgra: preview.image.pixels().to_vec(),
                })
                .is_ok()
        }
        PreviewEvent::Failed { error, .. } => events
            .send(WorkerEvent::PreviewFailed {
                generation: request.generation,
                revision: request.revision,
                message: error.to_string(),
            })
            .is_ok(),
        PreviewEvent::Cancelled { .. } | PreviewEvent::Discarded { .. } => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn process_text_work(
    page: usize,
    explicit: bool,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let Some(generation) = state.generation else {
        return true;
    };
    if let Some(text) = state.text_cache.get(&page).cloned() {
        return events
            .send(WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            })
            .is_ok();
    }

    let purpose = if explicit {
        TextDemandPurpose::Copy
    } else {
        TextDemandPurpose::VisibleLayer
    };
    let operation_cancellation = if explicit {
        state.explicit_text_cancellation.token()
    } else {
        state.automatic_text_cancellation.token()
    };
    let extracted = extract_runtime_text(state, page, purpose, &operation_cancellation);

    let mut deferred = Vec::new();
    if !collect_available_commands(commands, &mut deferred) {
        return false;
    }
    let viewport_changed = deferred.iter().any(|command| {
        matches!(
            command,
            WorkerCommand::Open { .. } | WorkerCommand::RenderViewport { .. }
        )
    });
    let text_superseded = deferred
        .iter()
        .any(|command| command_supersedes_text(command, page, explicit));
    if !accept_deferred_commands(deferred, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.generation != Some(generation) {
        return true;
    }

    let (text, warning) = cache_text_layer(
        &mut state.text_cache,
        state.runtime.cache_policy().text_pages(),
        page,
        || extracted,
    );
    if text_superseded || (viewport_changed && !explicit && !automatic_text.contains(&page)) {
        return true;
    }
    match (text, warning) {
        (Some(text), None) => events
            .send(WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            })
            .is_ok(),
        (Some(_), Some(message)) => events
            .send(WorkerEvent::TextFailed {
                generation,
                page,
                message,
            })
            .is_ok(),
        (None, None) => true,
        (None, Some(_)) => unreachable!("a text warning always carries a cached empty layer"),
    }
}

fn extract_runtime_text(
    state: &mut WorkerState,
    page: usize,
    purpose: TextDemandPurpose,
    operation_cancellation: &CancellationToken,
) -> Result<Arc<TextLayer>, String> {
    let session = state
        .runtime
        .session()
        .ok_or_else(|| "no document is open".to_owned())?;
    let (priority, intent) = match purpose {
        TextDemandPurpose::Copy | TextDemandPurpose::LinkResolution => {
            (DemandPriority::INTERACTIVE, DemandIntent::Explicit)
        }
        TextDemandPurpose::VisibleLayer => (DemandPriority::VISIBLE, DemandIntent::Visible),
        TextDemandPurpose::Search | TextDemandPurpose::DocumentAnalysis => {
            (DemandPriority::BACKGROUND, DemandIntent::Background)
        }
    };
    let demand = session
        .text_demand(page, purpose, priority, intent)
        .map_err(|error| error.to_string())?;
    let cancellation = state
        .document_cancellation
        .token()
        .combined(operation_cancellation);
    match state
        .runtime
        .extract_text_with_cancellation(demand, &cancellation)
    {
        TextEvent::Ready { text, .. } => Ok(text.layer),
        TextEvent::Failed { error, .. } => Err(error.to_string()),
        TextEvent::Cancelled { .. } => Err("text extraction was cancelled".into()),
        TextEvent::Discarded { .. } => Err("text extraction belongs to a stale document".into()),
    }
}

fn accept_available_commands(
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => {
                if !accept_command(command, events, state, explicit_text, automatic_text) {
                    return false;
                }
            }
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

fn collect_available_commands(
    commands: &mpsc::Receiver<WorkerCommand>,
    deferred: &mut Vec<WorkerCommand>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => deferred.push(command),
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

fn accept_deferred_commands(
    deferred: Vec<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    for command in deferred {
        if !accept_command(command, events, state, explicit_text, automatic_text) {
            return false;
        }
    }
    true
}

fn process_search_work(
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let Some(active) = state.search.clone() else {
        return true;
    };
    if state.generation != Some(active.generation) {
        state.search = None;
        return true;
    }
    if active.next_page >= active.page_count {
        state.search = None;
        return send_search_finished(events, &active);
    }

    let page = active.next_page;
    let text = if let Some(text) = state.text_cache.get(&page).cloned() {
        text
    } else {
        let search_cancellation = active.cancellation.token();
        let extracted =
            extract_runtime_text(state, page, TextDemandPurpose::Search, &search_cancellation);
        let mut deferred = Vec::new();
        let connected = collect_available_commands(commands, &mut deferred);
        if !connected {
            return false;
        }
        if !accept_deferred_commands(deferred, events, state, explicit_text, automatic_text) {
            return false;
        }
        if !search_job_is_current(state.search.as_ref(), &active) {
            return true;
        }
        match extracted {
            Ok(text) => cache_completed_text(state, page, text),
            Err(message) => {
                if let Some(search) = state.search.as_mut() {
                    search.next_page += 1;
                    search.skipped_pages += 1;
                }
                return events
                    .send(WorkerEvent::SearchWarning {
                        generation: active.generation,
                        revision: active.revision,
                        page,
                        message: format!("Could not search page {}: {message}", page + 1),
                    })
                    .is_ok();
            }
        }
    };

    if !search_job_is_current(state.search.as_ref(), &active) {
        return true;
    }
    let remaining = MAX_SEARCH_RESULTS.saturating_sub(active.total_results);
    let mut deferred = Vec::new();
    let mut connected = true;
    let outcome = search_page(page, text.as_slice(), &active.query, remaining, || {
        connected &= collect_available_commands(commands, &mut deferred);
        !connected || !deferred.is_empty()
    });
    if !connected {
        return false;
    }
    connected &= collect_available_commands(commands, &mut deferred);
    if !connected {
        return false;
    }
    if !accept_deferred_commands(deferred, events, state, explicit_text, automatic_text) {
        return false;
    }
    if !search_job_is_current(state.search.as_ref(), &active) {
        return true;
    }
    let SearchPageOutcome::Complete(mut results) = outcome else {
        return true;
    };

    let remaining_runs = MAX_SEARCH_HIGHLIGHT_RUNS.saturating_sub(active.total_highlight_runs);
    let added_runs = cap_search_highlight_runs(&mut results, remaining_runs);
    let added_results = results.matches.len();
    let stop = results.truncated;
    let finished = {
        let search = state
            .search
            .as_mut()
            .expect("the current search job was checked above");
        search.next_page += 1;
        search.total_results += added_results;
        search.total_highlight_runs += added_runs;
        search.truncated |= stop;
        (stop || search.next_page >= search.page_count).then(|| search.clone())
    };

    if !send_search_page_results(events, active.generation, active.revision, results) {
        return false;
    }
    if let Some(finished) = finished {
        state.search = None;
        return send_search_finished(events, &finished);
    }
    true
}

fn process_scientific_work(
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let Some(job) = state.scientific.as_ref() else {
        return true;
    };
    let generation = job.generation;
    if state.generation != Some(generation) {
        state.scientific = None;
        return true;
    }
    let page = job.analyzer.page_order().get(job.next_page).copied();
    let Some(page) = page else {
        let analysis = state
            .scientific
            .take()
            .expect("scientific job checked above")
            .analyzer
            .finish();
        return events
            .send(WorkerEvent::ScientificAnalysisComplete {
                generation,
                analysis,
            })
            .is_ok();
    };

    let text = if let Some(text) = state.text_cache.get(&page).cloned() {
        text
    } else {
        let document_cancellation = state.document_cancellation.token();
        let extracted = extract_runtime_text(
            state,
            page,
            TextDemandPurpose::DocumentAnalysis,
            &document_cancellation,
        );
        let mut deferred = Vec::new();
        if !collect_available_commands(commands, &mut deferred) {
            return false;
        }
        if !accept_deferred_commands(deferred, events, state, explicit_text, automatic_text) {
            return false;
        }
        if state.generation != Some(generation) || state.scientific.is_none() {
            return true;
        }
        match extracted {
            Ok(text) => cache_completed_text(state, page, text),
            Err(_) => Arc::new(TextLayer::empty()),
        }
    };

    let Some(job) = state.scientific.as_mut() else {
        return true;
    };
    if job.generation != generation
        || job.analyzer.page_order().get(job.next_page).copied() != Some(page)
    {
        return true;
    }
    job.analyzer.ingest_page(page, &text);
    job.next_page += 1;
    true
}

fn accept_command(
    command: WorkerCommand,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    match command {
        WorkerCommand::Open {
            generation,
            path,
            cancellation,
        } => {
            if cancellation.is_cancelled() {
                return true;
            }
            state.renders.clear_pending();
            state.previews.clear_pending();
            explicit_text.clear();
            automatic_text.clear();
            state.text_cache.clear();
            state.automatic_text_needs_quiet = false;
            reset_search_for_open(state);
            state.generation = Some(generation);
            state.document_cancellation = cancellation;
            state.automatic_text_cancellation = CancellationSource::new();
            state.explicit_text_cancellation = CancellationSource::new();

            let opened = state.runtime.open_with_cancellation(
                PdfiumDocumentSource::file(path.clone()),
                &state.document_cancellation.token(),
            );
            match opened {
                Ok(DocumentEvent::Opened { descriptor, .. }) => {
                    let pages = descriptor.pages().to_vec();
                    let toc = descriptor.table_of_contents().to_vec();
                    let links = descriptor.links().to_vec();
                    state.page_count = pages.len();
                    state.scientific = Some(ScientificJob {
                        generation,
                        analyzer: ScientificAnalyzer::new(pages.len(), &links),
                        next_page: 0,
                    });
                    events
                        .send(WorkerEvent::Opened {
                            generation,
                            path,
                            pages,
                            toc,
                            links,
                        })
                        .is_ok()
                }
                Ok(DocumentEvent::Failed { error, .. }) => events
                    .send(WorkerEvent::Error {
                        generation: Some(generation),
                        message: format!("Could not open PDF: {error}"),
                    })
                    .is_ok(),
                Ok(DocumentEvent::Cancelled { .. } | DocumentEvent::Closed { .. }) => true,
                Err(error) => events
                    .send(WorkerEvent::Error {
                        generation: Some(generation),
                        message: format!("Could not open PDF: {error}"),
                    })
                    .is_ok(),
            }
        }
        WorkerCommand::RenderViewport {
            generation,
            requests,
            text_pages,
            cancellation,
        } => {
            if state.generation == Some(generation) {
                state.automatic_text_cancellation = cancellation.clone();
                if !replace_render_demand(state, events, requests, cancellation) {
                    return false;
                }
                replace_automatic_text_demand(text_pages, automatic_text);
                state.automatic_text_needs_quiet = !automatic_text.is_empty();
            }
            true
        }
        WorkerCommand::ExtractText {
            generation,
            page,
            cancellation,
        } => {
            if state.generation == Some(generation) && page < state.page_count {
                state.explicit_text_cancellation = cancellation;
                explicit_text.clear();
                explicit_text.insert(page);
            }
            true
        }
        WorkerCommand::EnsureTextPages {
            generation,
            pages,
            cancellation,
        } => {
            if state.generation == Some(generation) {
                state.explicit_text_cancellation = cancellation;
                explicit_text.extend(pages.into_iter().filter(|page| *page < state.page_count));
            }
            true
        }
        WorkerCommand::CancelExplicitText { generation } => {
            if state.generation == Some(generation) {
                state.explicit_text_cancellation.cancel();
                explicit_text.clear();
            }
            true
        }
        WorkerCommand::Search {
            generation,
            revision,
            query,
            cancellation,
        } => accept_search_demand(state, events, generation, revision, query, cancellation),
        WorkerCommand::CancelSearch {
            generation,
            next_revision,
        } => {
            if let Some(search) = state.search.as_ref() {
                search.cancellation.cancel();
            }
            cancel_searches_before(state, generation, next_revision);
            true
        }
        WorkerCommand::RenderPreview {
            generation,
            revision,
            appearance,
            spec,
            cancellation,
        } => {
            if state.generation != Some(generation) {
                return true;
            }
            let Some(session) = state.runtime.session() else {
                return events
                    .send(WorkerEvent::PreviewFailed {
                        generation,
                        revision,
                        message: "no document is open".into(),
                    })
                    .is_ok();
            };
            let demand = session.preview_demand(
                spec.page,
                spec.raster,
                RasterSize {
                    width: 360,
                    height: 204,
                },
                spec.center_x,
                spec.center_y,
                appearance.color_mode(),
                DemandPriority::INTERACTIVE,
            );
            match demand {
                Ok(demand) => {
                    let request = PreviewRequest {
                        generation,
                        revision,
                        appearance,
                        demand,
                        cancellation,
                    };
                    let _ = state
                        .previews
                        .schedule((), DemandPriority::INTERACTIVE, request);
                    true
                }
                Err(error) => events
                    .send(WorkerEvent::PreviewFailed {
                        generation,
                        revision,
                        message: error.to_string(),
                    })
                    .is_ok(),
            }
        }
    }
}

fn replace_render_demand(
    state: &mut WorkerState,
    events: &mpsc::SyncSender<WorkerEvent>,
    requests: Vec<RenderRequest>,
    cancellation: CancellationSource,
) -> bool {
    state.renders.clear_pending();
    let Some(session) = state.runtime.session() else {
        return true;
    };
    for request in requests {
        let (tier, priority, intent) = render_priority(&request);
        let demand = session.render_demand(
            request.tile.key,
            request.tile.core_rect,
            request.tile.render_rect,
            request.appearance.color_mode(),
            priority,
            intent,
        );
        let demand = match demand {
            Ok(demand) => demand,
            Err(error) => {
                if events
                    .send(WorkerEvent::TileFailed {
                        generation: request.generation,
                        appearance: request.appearance,
                        key: request.tile.key,
                        message: error.to_string(),
                    })
                    .is_err()
                {
                    return false;
                }
                continue;
            }
        };
        let outcome = state.renders.schedule(
            tier,
            QueuedRender {
                request,
                demand,
                cancellation: cancellation.clone(),
            },
        );
        if let ScheduleOutcome::Rejected { value } = outcome
            && events
                .send(WorkerEvent::TileFailed {
                    generation: value.request.generation,
                    appearance: value.request.appearance,
                    key: value.request.tile.key,
                    message: "The bounded PDF render queue is full".into(),
                })
                .is_err()
        {
            return false;
        }
    }
    true
}

fn render_priority(request: &RenderRequest) -> (RenderTier, DemandPriority, DemandIntent) {
    let rank = u16::try_from(request.priority.min(32_767)).unwrap_or(32_767);
    if request.prefetch {
        (
            RenderTier::Prefetch,
            DemandPriority::new(32_767_u16.saturating_sub(rank)),
            DemandIntent::Prefetch,
        )
    } else {
        (
            RenderTier::Visible,
            DemandPriority::new(65_534_u16.saturating_sub(rank)),
            DemandIntent::Visible,
        )
    }
}

fn command_supersedes_text(
    command: &WorkerCommand,
    current_page: usize,
    current_is_explicit: bool,
) -> bool {
    match command {
        WorkerCommand::ExtractText { page, .. } => *page != current_page,
        WorkerCommand::EnsureTextPages { .. } => false,
        WorkerCommand::CancelExplicitText { .. } => current_is_explicit,
        WorkerCommand::Open { .. }
        | WorkerCommand::RenderViewport { .. }
        | WorkerCommand::Search { .. }
        | WorkerCommand::CancelSearch { .. }
        | WorkerCommand::RenderPreview { .. } => false,
    }
}

fn replace_automatic_text_demand(pages: Vec<usize>, pending: &mut VecDeque<usize>) {
    pending.clear();
    for page in pages {
        if !pending.contains(&page) {
            pending.push_back(page);
        }
    }
}

fn cache_text_layer(
    cache: &mut HashMap<usize, Arc<TextLayer>>,
    max_pages: usize,
    page: usize,
    extract: impl FnOnce() -> Result<Arc<TextLayer>, String>,
) -> (Option<Arc<TextLayer>>, Option<String>) {
    if cache.contains_key(&page) {
        return (None, None);
    }
    let result = match extract() {
        Ok(extracted) => {
            cache.insert(page, extracted.clone());
            (Some(extracted), None)
        }
        Err(message) => {
            // A malformed text layer must not invalidate a successfully
            // rendered page. Cache an empty layer to avoid repeated failures.
            let empty = Arc::new(TextLayer::empty());
            cache.insert(page, empty.clone());
            (
                Some(empty),
                Some(format!(
                    "Text selection is unavailable on page {}: {message}",
                    page + 1
                )),
            )
        }
    };
    evict_text_cache(cache, max_pages, page);
    result
}

fn cache_completed_text(
    state: &mut WorkerState,
    page: usize,
    text: Arc<TextLayer>,
) -> Arc<TextLayer> {
    if let Some(cached) = state.text_cache.get(&page) {
        return cached.clone();
    }
    state.text_cache.insert(page, text.clone());
    evict_text_cache(
        &mut state.text_cache,
        state.runtime.cache_policy().text_pages(),
        page,
    );
    text
}

fn evict_text_cache(cache: &mut HashMap<usize, Arc<TextLayer>>, max_pages: usize, page: usize) {
    while cache.len() > max_pages {
        let Some(evict) = cache
            .keys()
            .copied()
            .filter(|candidate| *candidate != page)
            .max_by_key(|candidate| candidate.abs_diff(page))
        else {
            break;
        };
        cache.remove(&evict);
    }
}

fn cap_search_highlight_runs(results: &mut SearchPageResults, remaining_runs: usize) -> usize {
    let mut added_runs = 0_usize;
    let mut retained = 0;
    for result in &results.matches {
        let next_runs = added_runs.saturating_add(result.highlight_runs.len());
        if next_runs > remaining_runs {
            results.truncated = true;
            break;
        }
        added_runs = next_runs;
        retained += 1;
    }
    if retained != results.matches.len() {
        results.matches.truncate(retained);
        results.truncated = true;
    }
    added_runs
}

fn send_search_finished(events: &mpsc::SyncSender<WorkerEvent>, search: &SearchJob) -> bool {
    events
        .send(WorkerEvent::SearchFinished {
            generation: search.generation,
            revision: search.revision,
            searched_pages: search.next_page,
            total_results: search.total_results,
            total_highlight_runs: search.total_highlight_runs,
            skipped_pages: search.skipped_pages,
            truncated: search.truncated,
        })
        .is_ok()
}

fn send_search_page_results(
    events: &mpsc::SyncSender<WorkerEvent>,
    generation: u64,
    revision: u64,
    results: SearchPageResults,
) -> bool {
    results.matches.is_empty()
        || events
            .send(WorkerEvent::SearchPageResults {
                generation,
                revision,
                results,
            })
            .is_ok()
}

fn search_job_is_current(current: Option<&SearchJob>, expected: &SearchJob) -> bool {
    current.is_some_and(|current| {
        current.generation == expected.generation && current.revision == expected.revision
    })
}

fn revision_is_newer(candidate: u64, current: u64) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < (1_u64 << 63)
}

fn revision_is_current_or_newer(candidate: u64, current: u64) -> bool {
    candidate == current || revision_is_newer(candidate, current)
}

fn advance_search_revision(state: &mut WorkerState, generation: u64, revision: u64) -> bool {
    if state.generation != Some(generation)
        || state
            .latest_search_revision
            .is_some_and(|current| !revision_is_newer(revision, current))
    {
        return false;
    }
    state.latest_search_revision = Some(revision);
    if let Some(search) = state.search.take() {
        search.cancellation.cancel();
    }
    true
}

fn accept_search_demand(
    state: &mut WorkerState,
    events: &mpsc::SyncSender<WorkerEvent>,
    generation: u64,
    revision: u64,
    query: SearchQuery,
    cancellation: CancellationSource,
) -> bool {
    if cancellation.is_cancelled() {
        return true;
    }
    if state.generation.is_none() {
        return events
            .send(WorkerEvent::SearchFailed {
                generation,
                revision,
                message: "Cannot search because no PDF document is open".into(),
            })
            .is_ok();
    }
    if !advance_search_revision(state, generation, revision) {
        return true;
    }
    if state.runtime.descriptor().is_none() {
        return events
            .send(WorkerEvent::SearchFailed {
                generation,
                revision,
                message: "Cannot search because no PDF document is open".into(),
            })
            .is_ok();
    }
    state.search = Some(SearchJob {
        generation,
        revision,
        query,
        next_page: 0,
        page_count: state.page_count,
        total_results: 0,
        total_highlight_runs: 0,
        skipped_pages: 0,
        truncated: false,
        cancellation,
    });
    true
}

fn cancel_searches_before(state: &mut WorkerState, generation: u64, next_revision: u64) {
    if state.generation != Some(generation) {
        return;
    }
    let floor = next_revision.wrapping_sub(1);
    if state
        .latest_search_revision
        .is_some_and(|current| !revision_is_current_or_newer(floor, current))
    {
        return;
    }
    state.latest_search_revision = Some(floor);
    if state
        .search
        .as_ref()
        .is_some_and(|search| revision_is_newer(next_revision, search.revision))
    {
        if let Some(search) = state.search.as_ref() {
            search.cancellation.cancel();
        }
        state.search = None;
    }
}

fn reset_search_for_open(state: &mut WorkerState) {
    state.page_count = 0;
    if let Some(search) = state.search.take() {
        search.cancellation.cancel();
    }
    state.latest_search_revision = None;
    state.scientific = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TextBounds, TextChar};
    use std::time::Instant;

    fn empty_worker_state(generation: u64) -> WorkerState {
        WorkerState {
            runtime: PdfRuntime::new(
                PdfiumEngine::new(PdfiumLibraryConfig::new(Vec::<PathBuf>::new())),
                CachePolicy::default(),
            ),
            generation: Some(generation),
            document_cancellation: CancellationSource::new(),
            automatic_text_cancellation: CancellationSource::new(),
            explicit_text_cancellation: CancellationSource::new(),
            text_cache: HashMap::new(),
            automatic_text_needs_quiet: false,
            page_count: 3,
            search: None,
            latest_search_revision: None,
            scientific: None,
            renders: RenderQueues::new(),
            previews: LatestWinsQueue::new(1),
        }
    }

    fn search_job(generation: u64, revision: u64) -> SearchJob {
        SearchJob {
            generation,
            revision,
            query: SearchQuery::new("page").unwrap(),
            next_page: 0,
            page_count: 3,
            total_results: 0,
            total_highlight_runs: 0,
            skipped_pages: 0,
            truncated: false,
            cancellation: CancellationSource::new(),
        }
    }

    #[test]
    fn caller_side_replacement_cancels_in_flight_work_immediately() {
        let cancellations = WorkerCancellations::default();
        let first_render = cancellations.replace_render();
        let second_render = cancellations.replace_render();
        assert!(first_render.is_cancelled());
        assert!(!second_render.is_cancelled());

        let search = cancellations.replace_search();
        let document = cancellations.begin_document();
        assert!(second_render.is_cancelled());
        assert!(search.is_cancelled());
        assert!(!document.is_cancelled());
    }

    #[test]
    fn appearance_mapping_preserves_semantic_rgb_values() {
        let mode = RenderAppearance::ForcedColors {
            background: RenderColor {
                red: 1,
                green: 2,
                blue: 3,
            },
            foreground: RenderColor {
                red: 4,
                green: 5,
                blue: 6,
            },
        }
        .color_mode();
        assert_eq!(
            mode,
            ColorMode::Forced {
                background: PixelColor {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
                foreground: PixelColor {
                    red: 4,
                    green: 5,
                    blue: 6,
                    alpha: 255,
                },
            }
        );
    }

    #[test]
    fn render_priority_keeps_visible_work_above_prefetch_work() {
        let tile = TileRequest {
            key: TileKey {
                page: 0,
                raster: RasterSize {
                    width: 100,
                    height: 100,
                },
                column: 0,
                row: 0,
            },
            core_rect: PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            render_rect: PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        };
        let visible = RenderRequest {
            generation: 1,
            appearance: RenderAppearance::Normal,
            tile,
            priority: 9,
            prefetch: false,
        };
        let prefetch = RenderRequest {
            prefetch: true,
            priority: 0,
            ..visible.clone()
        };
        assert!(render_priority(&visible).1 > render_priority(&prefetch).1);
    }

    #[test]
    fn text_failure_is_cached_as_a_non_fatal_empty_layer() {
        let calls = std::cell::Cell::new(0);
        let mut cache = HashMap::new();
        let (text, warning) = cache_text_layer(&mut cache, 16, 2, || {
            calls.set(calls.get() + 1);
            Err("synthetic text failure".into())
        });
        assert!(text.unwrap().is_empty());
        assert!(warning.unwrap().contains("synthetic text failure"));

        let (text, warning) = cache_text_layer(&mut cache, 16, 2, || {
            calls.set(calls.get() + 1);
            Ok(Arc::new(TextLayer::empty()))
        });
        assert!(text.is_none());
        assert!(warning.is_none());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn text_cache_evicts_the_page_farthest_from_new_work() {
        let mut cache = HashMap::new();
        for page in 0..=16 {
            let (layer, warning) =
                cache_text_layer(&mut cache, 16, page, || Ok(Arc::new(TextLayer::empty())));
            assert!(layer.is_some());
            assert!(warning.is_none());
        }
        assert_eq!(cache.len(), 16);
        assert!(cache.contains_key(&16));
        assert!(!cache.contains_key(&0));
    }

    #[test]
    fn latest_search_revision_replaces_and_rejects_stale_demand() {
        let mut state = empty_worker_state(7);
        assert!(advance_search_revision(&mut state, 7, 10));
        state.search = Some(search_job(7, 10));
        assert!(advance_search_revision(&mut state, 7, 11));
        assert!(state.search.is_none());
        state.search = Some(search_job(7, 11));
        assert!(!advance_search_revision(&mut state, 7, 10));
        assert_eq!(state.search.as_ref().unwrap().revision, 11);
        assert!(!advance_search_revision(&mut state, 6, 12));
    }

    #[test]
    fn cancellation_barrier_preserves_the_replacement_revision() {
        let mut state = empty_worker_state(7);
        state.latest_search_revision = Some(10);
        state.search = Some(search_job(7, 10));
        cancel_searches_before(&mut state, 7, 11);
        assert!(state.search.is_none());
        assert_eq!(state.latest_search_revision, Some(10));

        assert!(advance_search_revision(&mut state, 7, 11));
        state.search = Some(search_job(7, 11));
        cancel_searches_before(&mut state, 7, 11);
        assert_eq!(state.search.as_ref().unwrap().revision, 11);
    }

    #[test]
    fn search_highlight_storage_stops_before_the_global_run_cap() {
        let bounds = TextBounds {
            left: 0.1,
            top: 0.1,
            right: 0.2,
            bottom: 0.2,
        };
        let result = |start, run_count| crate::search::SearchMatch {
            id: crate::search::SearchMatchId {
                page: 0,
                start,
                end: start,
            },
            preview: String::new(),
            preview_match: 0..0,
            highlight_runs: vec![bounds; run_count],
        };
        let mut results = SearchPageResults {
            page: 0,
            matches: vec![result(0, 2), result(1, 2)],
            truncated: false,
        };
        assert_eq!(cap_search_highlight_runs(&mut results, 3), 2);
        assert_eq!(results.matches.len(), 1);
        assert!(results.truncated);
    }

    #[test]
    fn no_match_page_emits_no_empty_result_event() {
        let query = SearchQuery::new("absent").unwrap();
        let text = [TextChar {
            value: 'x',
            bounds: None,
        }];
        let SearchPageOutcome::Complete(results) =
            search_page(2, &text, &query, MAX_SEARCH_RESULTS, || false)
        else {
            panic!("search unexpectedly cancelled");
        };
        let (events, received) = mpsc::sync_channel(1);
        assert!(send_search_page_results(&events, 7, 4, results));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn worker_maps_runtime_open_render_text_preview_and_search_events() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tests/fixtures/interaction.pdf");
        assert!(fixture.is_file());

        let (worker, events) = PdfWorker::start();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            WorkerEvent::Ready
        ));
        let generation = 41;
        assert!(worker.open(generation, fixture));
        let deadline = Instant::now() + Duration::from_secs(10);
        let pages = loop {
            let event = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker should open the fixture");
            match event {
                WorkerEvent::Opened {
                    generation: opened_generation,
                    pages,
                    toc,
                    links,
                    ..
                } => {
                    assert_eq!(opened_generation, generation);
                    assert_eq!(pages.len(), 3);
                    assert_eq!(toc.len(), 4);
                    assert_eq!(links.len(), 2);
                    break pages;
                }
                WorkerEvent::Error { message, .. } => panic!("fixture open failed: {message}"),
                _ => {}
            }
        };

        let raster = RasterSize {
            width: pages[0].width.round() as u32,
            height: pages[0].height.round() as u32,
        };
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        };
        let tile = TileRequest {
            key: TileKey {
                page: 0,
                raster,
                column: 0,
                row: 0,
            },
            core_rect: rect,
            render_rect: rect,
        };
        assert!(worker.render_viewport(generation, RenderAppearance::Normal, &[tile], 1, &[0]));

        let mut rendered = false;
        let mut extracted = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !(rendered && extracted) {
            let event = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker should render and extract text");
            match event {
                WorkerEvent::TileRendered {
                    generation: event_generation,
                    key,
                    width,
                    height,
                    bgra,
                    ..
                } if key == tile.key => {
                    assert_eq!(event_generation, generation);
                    assert_eq!((width, height), (256, 256));
                    assert_eq!(bgra.len(), 256 * 256 * 4);
                    rendered = true;
                }
                WorkerEvent::TextExtracted {
                    generation: event_generation,
                    page: 0,
                    text,
                } => {
                    assert_eq!(event_generation, generation);
                    let content: String = text.iter().map(|character| character.value).collect();
                    assert!(content.contains("GPUI PDF Reader"));
                    extracted = true;
                }
                WorkerEvent::TileFailed { message, .. }
                | WorkerEvent::TextFailed { message, .. }
                | WorkerEvent::Error { message, .. } => {
                    panic!("worker operation failed: {message}")
                }
                _ => {}
            }
        }

        assert!(worker.render_preview(
            generation,
            3,
            RenderAppearance::Normal,
            PreviewSpec {
                page: 0,
                raster,
                center_x: 0.5,
                center_y: 0.5,
            },
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker should render the preview")
            {
                WorkerEvent::PreviewRendered {
                    generation: event_generation,
                    revision: 3,
                    width,
                    height,
                    bgra,
                    ..
                } => {
                    assert_eq!(event_generation, generation);
                    assert_eq!((width, height), (360, 204));
                    assert_eq!(bgra.len(), 360 * 204 * 4);
                    break;
                }
                WorkerEvent::PreviewFailed { message, .. } | WorkerEvent::Error { message, .. } => {
                    panic!("preview failed: {message}")
                }
                _ => {}
            }
        }

        assert!(worker.search(generation, 7, SearchQuery::new("page").unwrap(),));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut page_results = 0;
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("worker should finish the search")
            {
                WorkerEvent::SearchPageResults {
                    generation: event_generation,
                    revision: 7,
                    results,
                } => {
                    assert_eq!(event_generation, generation);
                    page_results += results.matches.len();
                }
                WorkerEvent::SearchFinished {
                    generation: event_generation,
                    revision: 7,
                    searched_pages,
                    total_results,
                    ..
                } => {
                    assert_eq!(event_generation, generation);
                    assert_eq!(searched_pages, 3);
                    assert_eq!(total_results, page_results);
                    assert!(total_results > 0);
                    break;
                }
                WorkerEvent::SearchFailed { message, .. } | WorkerEvent::Error { message, .. } => {
                    panic!("search failed: {message}")
                }
                _ => {}
            }
        }
    }
}
