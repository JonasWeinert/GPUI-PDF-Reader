mod engine;

use self::engine::{
    extract_runtime_text, pdfium_library_config, process_preview_work, process_render_work,
    process_text_work,
};
#[cfg(test)]
use super::client::WorkerCancellations;
use super::protocol::{RenderAppearance, RenderRequest, WorkerCommand, WorkerEvent};
#[cfg(test)]
use super::{PdfWorker, PreviewSpec, RenderColor, TileRequest};
#[cfg(test)]
use crate::model::PixelRect;
use crate::model::{RasterSize, TextLayer, TileKey};
use crate::scientific::ScientificAnalyzer;
use crate::search::{
    MAX_SEARCH_RESULTS, SearchPageOutcome, SearchPageResults, SearchQuery, search_page,
};
use key_pdf_runtime::{
    CachePolicy, CancellationSource, CompletionDisposition, DemandIntent, DemandPriority,
    DocumentEvent, LatestWinsQueue, PdfRuntime, PreviewDemand, RenderDemand, ScheduleOutcome,
    ScheduledDemand, TextDemandPurpose,
};
#[cfg(test)]
use key_pdf_runtime::{ColorMode, PixelColor};
#[cfg(test)]
use key_pdfium::PdfiumLibraryConfig;
use key_pdfium::{PdfiumDocumentSource, PdfiumEngine};
use std::collections::{BTreeSet, HashMap, VecDeque};
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const AUTOMATIC_TEXT_IDLE_DELAY: Duration = Duration::from_millis(200);
const MAX_SEARCH_HIGHLIGHT_RUNS: usize = 100_000;
const MAX_PENDING_RENDER_DEMANDS_PER_TIER: usize = 4_096;

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

pub(super) fn run_worker(
    commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::SyncSender<WorkerEvent>,
) {
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
mod tests;
