//! Document-local orchestration over the process-wide PDF engine supervisor.
//!
//! This thread performs scheduling, text caching, search matching, and
//! scientific-document analysis. It never owns a PDFium engine or document;
//! every PDF operation is submitted through `DocumentClient` and runs on the
//! single owner thread created by `ApplicationHost`.

#[cfg(test)]
use super::client::WorkerCancellations;
use super::protocol::{RenderAppearance, RenderRequest, WorkerCommand, WorkerEvent};
#[cfg(test)]
use super::{PdfWorker, PreviewSpec, RenderColor, TileRequest};
#[cfg(test)]
use crate::model::{PixelRect, TileKey};
use crate::model::{RasterSize, TextLayer};
use crate::scientific::ScientificAnalyzer;
use crate::search::{
    MAX_SEARCH_RESULTS, SearchPageOutcome, SearchPageResults, SearchQuery, search_page,
};
use key_pdf_runtime::{
    CachePolicy, CancellationSource, DemandIntent, DemandPriority, DocumentClient,
    DocumentGeneration, DocumentSession, EngineSupervisor, PixelFormat, PreviewEvent, RenderEvent,
    RequestId, SupervisorEvent, SupervisorPolicy, TextDemandPurpose, TextEvent, WorkClass,
    start_engine_supervisor,
};
#[cfg(test)]
use key_pdf_runtime::{ColorMode, PixelColor};
use key_pdfium::{PdfiumDocumentSource, PdfiumEngine, PdfiumEngineError, PdfiumLibraryConfig};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const AUTOMATIC_TEXT_IDLE_DELAY: Duration = Duration::from_millis(200);
const MAX_SEARCH_HIGHLIGHT_RUNS: usize = 100_000;
const MAX_PENDING_ENGINE_WORK_PER_DOCUMENT: usize = 8_192;

pub(crate) type PdfEngineSupervisor = EngineSupervisor<PdfiumDocumentSource, PdfiumEngineError>;
pub(crate) type PdfDocumentClient = DocumentClient<PdfiumDocumentSource, PdfiumEngineError>;
type DemandKey = (DocumentGeneration, RequestId);
type WorkerEventSender = flume::Sender<WorkerEvent>;

pub(crate) enum AdapterInput {
    Command(WorkerCommand),
    Supervisor(SupervisorEvent<PdfiumEngineError>),
    Shutdown,
}

pub(crate) fn start_pdf_engine_supervisor() -> std::io::Result<PdfEngineSupervisor> {
    let config = pdfium_library_config();
    let policy = SupervisorPolicy::new(MAX_PENDING_ENGINE_WORK_PER_DOCUMENT, 1)
        .expect("the PDF supervisor policy is statically valid");
    start_engine_supervisor("pdfium-engine-owner", policy, move || {
        PdfiumEngine::new(config)
    })
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

#[derive(Clone, Copy)]
enum RenderTier {
    Visible,
    Prefetch,
}

#[derive(Clone, Debug)]
struct PreviewRequest {
    generation: u64,
    revision: u64,
    appearance: RenderAppearance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingTextKind {
    Visible,
    Copy,
    Search { revision: u64 },
    Analysis,
}

#[derive(Clone, Copy, Debug)]
struct PendingText {
    generation: u64,
    page: usize,
    kind: PendingTextKind,
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
    waiting: bool,
}

struct ScientificJob {
    generation: u64,
    analyzer: ScientificAnalyzer,
    next_page: usize,
    waiting: bool,
}

struct VisibleTextDemand {
    due: Instant,
    pages: Vec<usize>,
}

struct WorkerState {
    generation: Option<u64>,
    path: Option<PathBuf>,
    session: Option<DocumentSession>,
    page_count: usize,
    text_cache: HashMap<usize, Arc<TextLayer>>,
    latest_search_revision: Option<u64>,
    search: Option<SearchJob>,
    scientific: Option<ScientificJob>,
    visible_text: Option<VisibleTextDemand>,
    pending_renders: HashMap<DemandKey, RenderRequest>,
    pending_preview: HashMap<DemandKey, PreviewRequest>,
    pending_text: HashMap<DemandKey, PendingText>,
    background_enabled: bool,
    hibernated: bool,
    resume_pending: bool,
    has_opened: bool,
}

impl WorkerState {
    fn new() -> Self {
        Self {
            generation: None,
            path: None,
            session: None,
            page_count: 0,
            text_cache: HashMap::new(),
            latest_search_revision: None,
            search: None,
            scientific: None,
            visible_text: None,
            pending_renders: HashMap::new(),
            pending_preview: HashMap::new(),
            pending_text: HashMap::new(),
            background_enabled: true,
            hibernated: false,
            resume_pending: false,
            has_opened: false,
        }
    }

    fn reset_for_open(&mut self, generation: u64, path: PathBuf) {
        self.generation = Some(generation);
        self.path = Some(path);
        self.session = None;
        self.page_count = 0;
        self.text_cache.clear();
        self.latest_search_revision = None;
        self.search = None;
        self.scientific = None;
        self.visible_text = None;
        self.pending_renders.clear();
        self.pending_preview.clear();
        self.pending_text.clear();
        self.background_enabled = true;
        self.hibernated = false;
        self.resume_pending = false;
        self.has_opened = false;
    }

    fn pause_background(&mut self, document: &PdfDocumentClient) {
        self.background_enabled = false;
        self.visible_text = None;
        clear_text_kind_matches(self, |kind| {
            matches!(
                kind,
                PendingTextKind::Search { .. } | PendingTextKind::Analysis
            )
        });
        if let Some(search) = self.search.as_mut() {
            search.waiting = false;
        }
        if let Some(scientific) = self.scientific.as_mut() {
            scientific.waiting = false;
        }
        let _ = document.cancel(WorkClass::SearchText);
        let _ = document.cancel(WorkClass::DocumentAnalysisText);
    }
}

pub(crate) fn run_worker(
    mailbox: mpsc::Receiver<AdapterInput>,
    events: WorkerEventSender,
    document: PdfDocumentClient,
    supervisor_event_pending: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) {
    let mut state = WorkerState::new();
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        while let Ok(input) = mailbox.try_recv() {
            if matches!(&input, AdapterInput::Supervisor(_)) {
                supervisor_event_pending.store(false, Ordering::Release);
            }
            if !accept_input(input, &events, &document, &mut state) {
                return;
            }
            if shutdown.load(Ordering::Acquire) {
                return;
            }
        }

        if !schedule_visible_text_if_due(&events, &document, &mut state) {
            return;
        }
        if drive_one_background_step(&events, &document, &mut state) {
            continue;
        }

        let received = match state.visible_text.as_ref() {
            Some(visible) => {
                mailbox.recv_timeout(visible.due.saturating_duration_since(Instant::now()))
            }
            None => match mailbox.recv() {
                Ok(input) => {
                    if matches!(&input, AdapterInput::Supervisor(_)) {
                        supervisor_event_pending.store(false, Ordering::Release);
                    }
                    if !accept_input(input, &events, &document, &mut state) {
                        return;
                    }
                    if shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    continue;
                }
                Err(_) => return,
            },
        };
        match received {
            Ok(input) => {
                if matches!(&input, AdapterInput::Supervisor(_)) {
                    supervisor_event_pending.store(false, Ordering::Release);
                }
                if !accept_input(input, &events, &document, &mut state) {
                    return;
                }
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn accept_input(
    input: AdapterInput,
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    match input {
        AdapterInput::Command(command) => accept_command(command, events, document, state),
        AdapterInput::Supervisor(event) => accept_supervisor_event(event, events, document, state),
        AdapterInput::Shutdown => false,
    }
}

fn accept_command(
    command: WorkerCommand,
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
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
            state.reset_for_open(generation, path.clone());
            if let Err(error) = document.open(PdfiumDocumentSource::file(path)) {
                return send_error(
                    events,
                    Some(generation),
                    format!("Could not open PDF: {error}"),
                );
            }
        }
        WorkerCommand::SetBackgroundEnabled {
            generation,
            enabled,
        } => {
            if state.generation == Some(generation) {
                if enabled {
                    state.background_enabled = true;
                } else {
                    state.pause_background(document);
                }
            }
        }
        WorkerCommand::Hibernate { generation } => {
            if state.generation == Some(generation) {
                state.pause_background(document);
                state.session = None;
                state.text_cache.clear();
                state.pending_renders.clear();
                state.pending_preview.clear();
                state.pending_text.clear();
                state.hibernated = true;
                state.resume_pending = false;
                let _ = document.close();
            }
        }
        WorkerCommand::Resume { generation } => {
            if state.generation == Some(generation) && state.hibernated {
                let Some(path) = state.path.clone() else {
                    return true;
                };
                state.resume_pending = state.has_opened;
                state.hibernated = false;
                if let Err(error) = document.open(PdfiumDocumentSource::file(path)) {
                    return send_error(events, Some(generation), error.to_string());
                }
            }
        }
        WorkerCommand::RenderViewport {
            generation,
            requests,
            text_pages,
            cancellation,
        } => {
            if state.generation != Some(generation) || cancellation.is_cancelled() {
                return true;
            }
            if !replace_render_viewport(document, state, events, requests) {
                return false;
            }
            clear_text_kind(state, PendingTextKind::Visible);
            let _ = document.cancel(WorkClass::VisibleText);
            state.visible_text = Some(VisibleTextDemand {
                due: Instant::now() + AUTOMATIC_TEXT_IDLE_DELAY,
                pages: deduplicate_pages(text_pages, state.page_count),
            });
        }
        WorkerCommand::ExtractText {
            generation,
            page,
            cancellation,
        } => {
            if state.generation == Some(generation)
                && !cancellation.is_cancelled()
                && !replace_copy_text(document, state, events, generation, vec![page])
            {
                return false;
            }
        }
        WorkerCommand::EnsureTextPages {
            generation,
            pages,
            cancellation,
        } => {
            if state.generation == Some(generation)
                && !cancellation.is_cancelled()
                && !replace_copy_text(document, state, events, generation, pages)
            {
                return false;
            }
        }
        WorkerCommand::CancelExplicitText { generation } => {
            if state.generation == Some(generation) {
                clear_text_kind(state, PendingTextKind::Copy);
                let _ = document.cancel(WorkClass::CopyText);
            }
        }
        WorkerCommand::Search {
            generation,
            revision,
            query,
            cancellation,
        } => {
            if !accept_search_demand(
                state,
                events,
                document,
                generation,
                revision,
                query,
                cancellation,
            ) {
                return false;
            }
        }
        WorkerCommand::CancelSearch {
            generation,
            next_revision,
        } => {
            cancel_searches_before(state, generation, next_revision);
            clear_text_kind_matches(state, |kind| matches!(kind, PendingTextKind::Search { .. }));
            let _ = document.cancel(WorkClass::SearchText);
        }
        WorkerCommand::RenderPreview {
            generation,
            revision,
            appearance,
            spec,
            cancellation,
        } => {
            if state.generation == Some(generation)
                && !cancellation.is_cancelled()
                && !replace_preview(
                    document, state, events, generation, revision, appearance, spec,
                )
            {
                return false;
            }
        }
    }
    true
}

fn accept_supervisor_event(
    event: SupervisorEvent<PdfiumEngineError>,
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    match event {
        SupervisorEvent::Attached { document: id, .. } => {
            debug_assert_eq!(id, document.id());
            events.send(WorkerEvent::Ready).is_ok()
        }
        SupervisorEvent::Opened {
            document: id,
            session,
            descriptor,
            ..
        } => {
            debug_assert_eq!(id, document.id());
            let (Some(generation), Some(path)) = (state.generation, state.path.clone()) else {
                return true;
            };
            state.session = Some(session);
            state.page_count = descriptor.page_count();
            state.has_opened = true;
            if state.resume_pending {
                state.resume_pending = false;
                return events.send(WorkerEvent::Resumed { generation }).is_ok();
            }
            state.scientific = Some(ScientificJob {
                generation,
                analyzer: ScientificAnalyzer::new(descriptor.page_count(), descriptor.links()),
                next_page: 0,
                waiting: false,
            });
            events
                .send(WorkerEvent::Opened {
                    generation,
                    path,
                    pages: descriptor.pages().to_vec(),
                    toc: descriptor.table_of_contents().to_vec(),
                    links: descriptor.links().to_vec(),
                })
                .is_ok()
        }
        SupervisorEvent::OpenFailed {
            generation: _,
            error,
            ..
        } => send_error(
            events,
            state.generation,
            format!("Could not open PDF: {error}"),
        ),
        SupervisorEvent::OpenCancelled { .. } | SupervisorEvent::Closed { .. } => true,
        SupervisorEvent::Rendered { event, .. } => accept_render_event(event, events, state),
        SupervisorEvent::PreviewRendered { event, .. } => {
            accept_preview_event(event, events, state)
        }
        SupervisorEvent::TextExtracted { event, .. } => {
            accept_text_event(event, events, document, state)
        }
        SupervisorEvent::WorkRejected {
            class, rejected, ..
        } => {
            // A later settled viewport/search revision will naturally replace
            // evicted work. Keep this non-fatal; the queue is deliberately a
            // hard memory bound rather than an application failure.
            debug_assert!(rejected > 0, "empty rejection event for {class:?}");
            true
        }
    }
}

fn replace_render_viewport(
    document: &PdfDocumentClient,
    state: &mut WorkerState,
    events: &WorkerEventSender,
    requests: Vec<RenderRequest>,
) -> bool {
    state.pending_renders.clear();
    let Some(session) = state.session.as_ref() else {
        return true;
    };
    let mut demands = Vec::with_capacity(requests.len());
    for request in requests {
        let (_, priority, intent) = render_priority(&request);
        match session.render_demand(
            request.tile.key,
            request.tile.core_rect,
            request.tile.render_rect,
            request.appearance.color_mode(),
            priority,
            intent,
        ) {
            Ok(demand) => {
                state.pending_renders.insert(
                    (demand.stamp().generation(), demand.stamp().request()),
                    request,
                );
                demands.push(demand);
            }
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
            }
        }
    }
    if let Err(error) = document.replace_render_viewport(demands) {
        return send_error(events, state.generation, error.to_string());
    }
    true
}

fn accept_render_event(
    event: RenderEvent<PdfiumEngineError>,
    events: &WorkerEventSender,
    state: &mut WorkerState,
) -> bool {
    match event {
        RenderEvent::Ready { demand, tile } => {
            let key = (demand.stamp().generation(), demand.stamp().request());
            let Some(request) = state.pending_renders.remove(&key) else {
                return true;
            };
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
        RenderEvent::Failed { stamp, error } => {
            let Some(request) = state
                .pending_renders
                .remove(&(stamp.generation(), stamp.request()))
            else {
                return true;
            };
            events
                .send(WorkerEvent::TileFailed {
                    generation: request.generation,
                    appearance: request.appearance,
                    key: request.tile.key,
                    message: format!(
                        "Could not render page {}: {error}",
                        request.tile.key.page + 1
                    ),
                })
                .is_ok()
        }
        RenderEvent::Cancelled { stamp } | RenderEvent::Discarded { stamp } => {
            state
                .pending_renders
                .remove(&(stamp.generation(), stamp.request()));
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn replace_preview(
    document: &PdfDocumentClient,
    state: &mut WorkerState,
    events: &WorkerEventSender,
    generation: u64,
    revision: u64,
    appearance: RenderAppearance,
    spec: super::PreviewSpec,
) -> bool {
    state.pending_preview.clear();
    let Some(session) = state.session.as_ref() else {
        return events
            .send(WorkerEvent::PreviewFailed {
                generation,
                revision,
                message: "no document is open".into(),
            })
            .is_ok();
    };
    let demand = match session.preview_demand(
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
    ) {
        Ok(demand) => demand,
        Err(error) => {
            return events
                .send(WorkerEvent::PreviewFailed {
                    generation,
                    revision,
                    message: error.to_string(),
                })
                .is_ok();
        }
    };
    state.pending_preview.insert(
        (demand.stamp().generation(), demand.stamp().request()),
        PreviewRequest {
            generation,
            revision,
            appearance,
        },
    );
    if let Err(error) = document.replace_preview(Some(demand)) {
        return events
            .send(WorkerEvent::PreviewFailed {
                generation,
                revision,
                message: error.to_string(),
            })
            .is_ok();
    }
    true
}

fn accept_preview_event(
    event: PreviewEvent<PdfiumEngineError>,
    events: &WorkerEventSender,
    state: &mut WorkerState,
) -> bool {
    match event {
        PreviewEvent::Ready { demand, preview } => {
            let Some(request) = state
                .pending_preview
                .remove(&(demand.stamp().generation(), demand.stamp().request()))
            else {
                return true;
            };
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
        PreviewEvent::Failed { stamp, error } => {
            let Some(request) = state
                .pending_preview
                .remove(&(stamp.generation(), stamp.request()))
            else {
                return true;
            };
            events
                .send(WorkerEvent::PreviewFailed {
                    generation: request.generation,
                    revision: request.revision,
                    message: error.to_string(),
                })
                .is_ok()
        }
        PreviewEvent::Cancelled { stamp } | PreviewEvent::Discarded { stamp } => {
            state
                .pending_preview
                .remove(&(stamp.generation(), stamp.request()));
            true
        }
    }
}

fn schedule_visible_text_if_due(
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    if state
        .visible_text
        .as_ref()
        .is_none_or(|demand| demand.due > Instant::now())
    {
        return true;
    }
    let visible = state
        .visible_text
        .take()
        .expect("visible demand checked above");
    let Some(generation) = state.generation else {
        return true;
    };
    schedule_text_pages(
        document,
        state,
        events,
        generation,
        visible.pages,
        PendingTextKind::Visible,
        WorkClass::VisibleText,
        TextDemandPurpose::VisibleLayer,
        DemandPriority::new(32_767),
        DemandIntent::Visible,
        true,
    )
}

fn replace_copy_text(
    document: &PdfDocumentClient,
    state: &mut WorkerState,
    events: &WorkerEventSender,
    generation: u64,
    pages: Vec<usize>,
) -> bool {
    schedule_text_pages(
        document,
        state,
        events,
        generation,
        deduplicate_pages(pages, state.page_count),
        PendingTextKind::Copy,
        WorkClass::CopyText,
        TextDemandPurpose::Copy,
        DemandPriority::INTERACTIVE,
        DemandIntent::Explicit,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn schedule_text_pages(
    document: &PdfDocumentClient,
    state: &mut WorkerState,
    events: &WorkerEventSender,
    generation: u64,
    pages: Vec<usize>,
    kind: PendingTextKind,
    class: WorkClass,
    purpose: TextDemandPurpose,
    priority: DemandPriority,
    intent: DemandIntent,
    publish_cached: bool,
) -> bool {
    clear_text_kind_matches(state, |candidate| same_text_domain(candidate, kind));
    let Some(session) = state.session.as_ref().cloned() else {
        return true;
    };
    let mut demands = Vec::new();
    for page in pages {
        if let Some(text) = state.text_cache.get(&page).cloned() {
            if publish_cached
                && events
                    .send(WorkerEvent::TextExtracted {
                        generation,
                        page,
                        text,
                    })
                    .is_err()
            {
                return false;
            }
            continue;
        }
        match session.text_demand(page, purpose, priority, intent) {
            Ok(demand) => {
                state.pending_text.insert(
                    (demand.stamp().generation(), demand.stamp().request()),
                    PendingText {
                        generation,
                        page,
                        kind,
                    },
                );
                demands.push(demand);
            }
            Err(error) => {
                if !handle_text_failure(
                    events,
                    document,
                    state,
                    PendingText {
                        generation,
                        page,
                        kind,
                    },
                    error.to_string(),
                ) {
                    return false;
                }
            }
        }
    }
    if let Err(error) = document.replace_text(class, demands) {
        return send_error(events, Some(generation), error.to_string());
    }
    true
}

fn accept_text_event(
    event: TextEvent<PdfiumEngineError>,
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    match event {
        TextEvent::Ready { demand, text } => {
            let Some(pending) = state
                .pending_text
                .remove(&(demand.stamp().generation(), demand.stamp().request()))
            else {
                return true;
            };
            let layer = cache_completed_text(state, pending.page, text.layer);
            handle_text_ready(events, document, state, pending, layer)
        }
        TextEvent::Failed { stamp, error } => {
            let Some(pending) = state
                .pending_text
                .remove(&(stamp.generation(), stamp.request()))
            else {
                return true;
            };
            handle_text_failure(events, document, state, pending, error.to_string())
        }
        TextEvent::Cancelled { stamp } | TextEvent::Discarded { stamp } => {
            state
                .pending_text
                .remove(&(stamp.generation(), stamp.request()));
            true
        }
    }
}

fn handle_text_ready(
    events: &WorkerEventSender,
    _document: &PdfDocumentClient,
    state: &mut WorkerState,
    pending: PendingText,
    text: Arc<TextLayer>,
) -> bool {
    match pending.kind {
        PendingTextKind::Visible | PendingTextKind::Copy => events
            .send(WorkerEvent::TextExtracted {
                generation: pending.generation,
                page: pending.page,
                text,
            })
            .is_ok(),
        PendingTextKind::Search { revision } => {
            let Some(search) = state.search.as_mut() else {
                return true;
            };
            if search.generation != pending.generation || search.revision != revision {
                return true;
            }
            search.waiting = false;
            process_search_page(events, state, pending.page, text)
        }
        PendingTextKind::Analysis => {
            let Some(job) = state.scientific.as_mut() else {
                return true;
            };
            if job.generation != pending.generation {
                return true;
            }
            job.waiting = false;
            ingest_scientific_page(events, state, pending.page, text)
        }
    }
}

fn handle_text_failure(
    events: &WorkerEventSender,
    _document: &PdfDocumentClient,
    state: &mut WorkerState,
    pending: PendingText,
    message: String,
) -> bool {
    let empty = cache_completed_text(state, pending.page, Arc::new(TextLayer::empty()));
    match pending.kind {
        PendingTextKind::Visible | PendingTextKind::Copy => events
            .send(WorkerEvent::TextFailed {
                generation: pending.generation,
                page: pending.page,
                message: format!(
                    "Text selection is unavailable on page {}: {message}",
                    pending.page + 1
                ),
            })
            .is_ok(),
        PendingTextKind::Search { revision } => {
            let Some(search) = state.search.as_mut() else {
                return true;
            };
            if search.generation != pending.generation || search.revision != revision {
                return true;
            }
            search.waiting = false;
            search.next_page = search.next_page.max(pending.page + 1);
            search.skipped_pages += 1;
            events
                .send(WorkerEvent::SearchWarning {
                    generation: pending.generation,
                    revision,
                    page: pending.page,
                    message: format!("Could not search page {}: {message}", pending.page + 1),
                })
                .is_ok()
        }
        PendingTextKind::Analysis => {
            if let Some(job) = state.scientific.as_mut() {
                job.waiting = false;
            }
            ingest_scientific_page(events, state, pending.page, empty)
        }
    }
}

fn drive_one_background_step(
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    if !state.background_enabled || state.hibernated || state.session.is_none() {
        return false;
    }
    if drive_search(events, document, state) {
        return true;
    }
    drive_scientific(events, document, state)
}

fn drive_search(
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    let Some(search) = state.search.as_ref() else {
        return false;
    };
    if search.waiting {
        return false;
    }
    if search.cancellation.is_cancelled() {
        state.search = None;
        return true;
    }
    if search.next_page >= search.page_count {
        let finished = state.search.take().expect("search checked above");
        return send_search_finished(events, &finished);
    }
    let page = search.next_page;
    if let Some(text) = state.text_cache.get(&page).cloned() {
        return process_search_page(events, state, page, text);
    }
    let (generation, revision) = (search.generation, search.revision);
    let Some(session) = state.session.as_ref() else {
        return false;
    };
    let demand = match session.text_demand(
        page,
        TextDemandPurpose::Search,
        DemandPriority::new(32_766),
        DemandIntent::Explicit,
    ) {
        Ok(demand) => demand,
        Err(error) => {
            return handle_text_failure(
                events,
                document,
                state,
                PendingText {
                    generation,
                    page,
                    kind: PendingTextKind::Search { revision },
                },
                error.to_string(),
            );
        }
    };
    state.pending_text.insert(
        (demand.stamp().generation(), demand.stamp().request()),
        PendingText {
            generation,
            page,
            kind: PendingTextKind::Search { revision },
        },
    );
    if let Some(search) = state.search.as_mut() {
        search.waiting = true;
    }
    if let Err(error) = document.replace_text(WorkClass::SearchText, vec![demand]) {
        let _ = send_error(events, Some(generation), error.to_string());
    }
    false
}

fn process_search_page(
    events: &WorkerEventSender,
    state: &mut WorkerState,
    page: usize,
    text: Arc<TextLayer>,
) -> bool {
    let Some(active) = state.search.clone() else {
        return true;
    };
    if active.next_page != page || active.cancellation.is_cancelled() {
        return true;
    }
    let remaining = MAX_SEARCH_RESULTS.saturating_sub(active.total_results);
    let SearchPageOutcome::Complete(mut results) =
        search_page(page, text.as_slice(), &active.query, remaining, || {
            active.cancellation.is_cancelled()
        })
    else {
        return true;
    };
    if active.cancellation.is_cancelled() {
        return true;
    }
    let remaining_runs = MAX_SEARCH_HIGHLIGHT_RUNS.saturating_sub(active.total_highlight_runs);
    let added_runs = cap_search_highlight_runs(&mut results, remaining_runs);
    let added_results = results.matches.len();
    let stop = results.truncated;
    let finished = {
        let search = state.search.as_mut().expect("search checked above");
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

fn drive_scientific(
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    state: &mut WorkerState,
) -> bool {
    let Some(job) = state.scientific.as_ref() else {
        return false;
    };
    let generation = job.generation;
    let waiting = job.waiting;
    let page = job.analyzer.page_order().get(job.next_page).copied();
    if waiting {
        return false;
    }
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
    if let Some(text) = state.text_cache.get(&page).cloned() {
        return ingest_scientific_page(events, state, page, text);
    }
    let Some(session) = state.session.as_ref() else {
        return false;
    };
    let demand = match session.text_demand(
        page,
        TextDemandPurpose::DocumentAnalysis,
        DemandPriority::BACKGROUND,
        DemandIntent::Background,
    ) {
        Ok(demand) => demand,
        Err(_) => {
            return ingest_scientific_page(events, state, page, Arc::new(TextLayer::empty()));
        }
    };
    state.pending_text.insert(
        (demand.stamp().generation(), demand.stamp().request()),
        PendingText {
            generation,
            page,
            kind: PendingTextKind::Analysis,
        },
    );
    if let Some(job) = state.scientific.as_mut() {
        job.waiting = true;
    }
    if let Err(error) = document.replace_text(WorkClass::DocumentAnalysisText, vec![demand]) {
        let _ = send_error(events, Some(generation), error.to_string());
    }
    false
}

fn ingest_scientific_page(
    _events: &WorkerEventSender,
    state: &mut WorkerState,
    page: usize,
    text: Arc<TextLayer>,
) -> bool {
    let Some(job) = state.scientific.as_mut() else {
        return true;
    };
    if job.analyzer.page_order().get(job.next_page).copied() != Some(page) {
        return true;
    }
    job.analyzer.ingest_page(page, &text);
    job.next_page += 1;
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

fn deduplicate_pages(pages: Vec<usize>, page_count: usize) -> Vec<usize> {
    let mut unique = BTreeSet::new();
    pages
        .into_iter()
        .filter(|page| *page < page_count && unique.insert(*page))
        .collect()
}

fn same_text_domain(left: PendingTextKind, right: PendingTextKind) -> bool {
    matches!(
        (left, right),
        (PendingTextKind::Visible, PendingTextKind::Visible)
            | (PendingTextKind::Copy, PendingTextKind::Copy)
            | (
                PendingTextKind::Search { .. },
                PendingTextKind::Search { .. }
            )
            | (PendingTextKind::Analysis, PendingTextKind::Analysis)
    )
}

fn clear_text_kind(state: &mut WorkerState, kind: PendingTextKind) {
    clear_text_kind_matches(state, |candidate| same_text_domain(candidate, kind));
}

fn clear_text_kind_matches(state: &mut WorkerState, matches: impl Fn(PendingTextKind) -> bool) {
    state
        .pending_text
        .retain(|_, pending| !matches(pending.kind));
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
        CachePolicy::default().text_pages(),
        page,
    );
    text
}

#[cfg(test)]
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

fn send_search_finished(events: &WorkerEventSender, search: &SearchJob) -> bool {
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
    events: &WorkerEventSender,
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

#[allow(clippy::too_many_arguments)]
fn accept_search_demand(
    state: &mut WorkerState,
    events: &WorkerEventSender,
    document: &PdfDocumentClient,
    generation: u64,
    revision: u64,
    query: SearchQuery,
    cancellation: CancellationSource,
) -> bool {
    if cancellation.is_cancelled() {
        return true;
    }
    if state.session.is_none() {
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
    clear_text_kind_matches(state, |kind| matches!(kind, PendingTextKind::Search { .. }));
    let _ = document.cancel(WorkClass::SearchText);
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
        waiting: false,
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
        && let Some(search) = state.search.take()
    {
        search.cancellation.cancel();
    }
}

fn send_error(events: &WorkerEventSender, generation: Option<u64>, message: String) -> bool {
    events
        .send(WorkerEvent::Error {
            generation,
            message,
        })
        .is_ok()
}

#[cfg(test)]
mod tests;
