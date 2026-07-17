use crate::model::{
    PageSize, PdfLink as DocumentLink, PdfLinkTarget, PixelRect, RasterSize, TextBounds, TextChar,
    TextLayer, TileKey, TocEntry,
};
#[cfg(test)]
use crate::model::{TextPosition, TextSelection, selected_text};
use crate::scientific::{ScientificAnalysis, ScientificAnalyzer};
use crate::search::{
    MAX_SEARCH_RESULTS, SearchPageOutcome, SearchPageResults, SearchQuery, search_page,
};
use pdfium_render::prelude::*;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use url::Url;

const MAX_RASTER_DIMENSION: u32 = 65_536;
const MAX_TILE_DIMENSION: u32 = 1_088;
const MAX_PAGE_POINTS: f32 = 1_000_000.0;
const MAX_PAGE_TEXT_CHARS: usize = 100_000;
const MAX_CACHED_TEXT_PAGES: usize = 16;
const TEXT_CANCEL_INTERVAL: usize = 64;
const AUTOMATIC_TEXT_IDLE_DELAY: Duration = Duration::from_millis(200);
const MAX_SEARCH_HIGHLIGHT_RUNS: usize = 100_000;
const MAX_TOC_ENTRIES: usize = 512;
const MAX_TOC_DEPTH: u16 = 32;
const MAX_TOC_TITLE_UTF16_BYTES: usize = 2_048;
const MAX_DOCUMENT_LINKS: usize = 20_000;
const MAX_LINK_URI_BYTES: usize = 8_192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl RenderColor {
    fn as_pdfium(self) -> PdfColor {
        // pdfium-render's PdfColor encoder stores colors in Pdfium's native
        // ABGR integer order. Swap the semantic red/blue inputs so PDFium's
        // forced-color and bitmap-clear APIs receive the intended RGB value.
        PdfColor::new(self.blue, self.green, self.red, 255)
    }
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
}

struct ScientificJob {
    generation: u64,
    analyzer: ScientificAnalyzer,
    next_page: usize,
    partial: Option<(usize, Vec<TextChar>)>,
}

impl PdfWorker {
    pub fn start() -> (Self, mpsc::Receiver<WorkerEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        // A tile is under five MiB. A bounded queue applies back-pressure if
        // the UI thread is briefly busy instead of accumulating bitmap copies.
        let (event_tx, event_rx) = mpsc::sync_channel(1);

        thread::Builder::new()
            .name("pdfium-renderer".into())
            .spawn(move || run_worker(command_rx, event_tx))
            .expect("failed to start the PDFium renderer thread");

        (
            Self {
                commands: command_tx,
            },
            event_rx,
        )
    }

    pub fn open(&self, generation: u64, path: PathBuf) -> bool {
        self.commands
            .send(WorkerCommand::Open { generation, path })
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
            })
            .is_ok()
    }

    pub fn extract_text(&self, generation: u64, page: usize) -> bool {
        self.commands
            .send(WorkerCommand::ExtractText { generation, page })
            .is_ok()
    }

    pub fn ensure_text_pages(&self, generation: u64, pages: Vec<usize>) -> bool {
        self.commands
            .send(WorkerCommand::EnsureTextPages { generation, pages })
            .is_ok()
    }

    pub fn cancel_explicit_text(&self, generation: u64) -> bool {
        self.commands
            .send(WorkerCommand::CancelExplicitText { generation })
            .is_ok()
    }

    pub fn search(&self, generation: u64, revision: u64, query: SearchQuery) -> bool {
        self.commands
            .send(WorkerCommand::Search {
                generation,
                revision,
                query,
            })
            .is_ok()
    }

    /// Cancels search revisions older than `next_revision` immediately while
    /// still allowing the debounced search for `next_revision` to start.
    pub fn cancel_search(&self, generation: u64, next_revision: u64) -> bool {
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
        self.commands
            .send(WorkerCommand::RenderPreview {
                request: preview_render_request(
                    generation,
                    revision,
                    appearance,
                    spec.page,
                    spec.raster,
                    spec.center_x,
                    spec.center_y,
                ),
            })
            .is_ok()
    }
}

#[derive(Debug)]
enum WorkerCommand {
    Open {
        generation: u64,
        path: PathBuf,
    },
    RenderViewport {
        generation: u64,
        requests: Vec<RenderRequest>,
        text_pages: Vec<usize>,
    },
    ExtractText {
        generation: u64,
        page: usize,
    },
    EnsureTextPages {
        generation: u64,
        pages: Vec<usize>,
    },
    CancelExplicitText {
        generation: u64,
    },
    Search {
        generation: u64,
        revision: u64,
        query: SearchQuery,
    },
    CancelSearch {
        generation: u64,
        next_revision: u64,
    },
    RenderPreview {
        request: PreviewRequest,
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
struct PreviewRequest {
    revision: u64,
    render: RenderRequest,
}

struct WorkerState {
    document: Option<PdfDocument<'static>>,
    generation: Option<u64>,
    text_cache: HashMap<usize, Arc<TextLayer>>,
    partial_text: HashMap<usize, Vec<TextChar>>,
    automatic_text_needs_quiet: bool,
    page_count: usize,
    search: Option<SearchJob>,
    latest_search_revision: Option<u64>,
    search_partial: Option<(usize, Vec<TextChar>)>,
    scientific: Option<ScientificJob>,
    preview: Option<PreviewRequest>,
}

enum TextExtraction {
    Complete(Vec<TextChar>),
    Cancelled(Vec<TextChar>),
}

enum WorkerWork {
    Tile(TileKey),
    Text { page: usize, explicit: bool },
    Search,
    Scientific,
    Preview,
}

fn run_worker(commands: mpsc::Receiver<WorkerCommand>, events: mpsc::SyncSender<WorkerEvent>) {
    let pdfium = match initialize_pdfium() {
        Ok(pdfium) => pdfium,
        Err(message) => {
            let _ = events.send(WorkerEvent::Error {
                generation: None,
                message,
            });
            return;
        }
    };
    if events.send(WorkerEvent::Ready).is_err() {
        return;
    }

    let mut state = WorkerState {
        document: None,
        generation: None,
        text_cache: HashMap::new(),
        partial_text: HashMap::new(),
        automatic_text_needs_quiet: false,
        page_count: 0,
        search: None,
        latest_search_revision: None,
        search_partial: None,
        scientific: None,
        preview: None,
    };
    let mut pending = HashMap::<TileKey, RenderRequest>::new();
    let mut explicit_text = BTreeSet::<usize>::new();
    let mut automatic_text = VecDeque::<usize>::new();

    loop {
        if pending.is_empty()
            && explicit_text.is_empty()
            && automatic_text.is_empty()
            && state.search.is_none()
            && state.scientific.is_none()
            && state.preview.is_none()
        {
            match commands.recv() {
                Ok(command) => {
                    if !accept_command(
                        command,
                        pdfium,
                        &events,
                        &mut state,
                        &mut pending,
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
            pdfium,
            &events,
            &mut state,
            &mut pending,
            &mut explicit_text,
            &mut automatic_text,
        ) {
            return;
        }

        let next_visible_tile = pending
            .iter()
            .filter(|(_, request)| !request.prefetch)
            .min_by_key(|(_, request)| request.priority)
            .map(|(key, _)| *key);
        let work = if let Some(key) = next_visible_tile {
            WorkerWork::Tile(key)
        } else if state.preview.is_some() {
            WorkerWork::Preview
        } else if let Some(page) = explicit_text.pop_first() {
            WorkerWork::Text {
                page,
                explicit: true,
            }
        } else if let Some(page) = automatic_text.front().copied() {
            if state.text_cache.contains_key(&page) {
                automatic_text.pop_front();
                WorkerWork::Text {
                    page,
                    explicit: false,
                }
            } else if state.automatic_text_needs_quiet {
                // Let a new scroll/zoom command replace stale automatic text work
                // before entering PDFium's synchronous text walk. Explicit copy
                // requests above do not pay this idle delay. Only the first
                // missing page in a viewport batch pays it.
                match commands.recv_timeout(AUTOMATIC_TEXT_IDLE_DELAY) {
                    Ok(command) => {
                        if !accept_command(
                            command,
                            pdfium,
                            &events,
                            &mut state,
                            &mut pending,
                            &mut explicit_text,
                            &mut automatic_text,
                        ) {
                            return;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        state.automatic_text_needs_quiet = false;
                        automatic_text.pop_front();
                        WorkerWork::Text {
                            page,
                            explicit: false,
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                automatic_text.pop_front();
                WorkerWork::Text {
                    page,
                    explicit: false,
                }
            }
        } else if state.search.is_some() {
            WorkerWork::Search
        } else if let Some(key) = pending
            .iter()
            .min_by_key(|(_, request)| request.priority)
            .map(|(key, _)| *key)
        {
            WorkerWork::Tile(key)
        } else if state.scientific.is_some() {
            WorkerWork::Scientific
        } else {
            continue;
        };

        if matches!(work, WorkerWork::Preview) {
            let request = state.preview.take().expect("preview work checked above");
            if state.generation != Some(request.render.generation) {
                continue;
            }
            let result = render_tile(&mut state, &request.render);
            if state.preview.as_ref().is_some_and(|latest| {
                latest.revision > request.revision
                    || latest.render.generation != request.render.generation
            }) {
                continue;
            }
            match result {
                Ok((width, height, bgra)) => {
                    if events
                        .send(WorkerEvent::PreviewRendered {
                            generation: request.render.generation,
                            revision: request.revision,
                            appearance: request.render.appearance,
                            width,
                            height,
                            bgra,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(message) => {
                    if events
                        .send(WorkerEvent::PreviewFailed {
                            generation: request.render.generation,
                            revision: request.revision,
                            message,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
            continue;
        }

        if let WorkerWork::Tile(next_tile) = work {
            // Leave the in-flight request in `pending`. A viewport replacement
            // received while PDFium is rendering will clear or replace it;
            // that gives completion a definitive latest-demand check.
            let request = pending.get(&next_tile).unwrap().clone();

            if state.generation != Some(request.generation) {
                continue;
            }

            let result = render_tile(&mut state, &request);

            // A replacement viewport may have arrived while PDFium was
            // rendering. Apply it before publishing the result, then remove
            // the completed key from that latest demand so it cannot be
            // reinserted and rendered twice.
            for command in commands.try_iter() {
                if !accept_command(
                    command,
                    pdfium,
                    &events,
                    &mut state,
                    &mut pending,
                    &mut explicit_text,
                    &mut automatic_text,
                ) {
                    return;
                }
            }
            if !should_publish_completed_render(state.generation, &request, &mut pending) {
                continue;
            }

            match result {
                Ok((width, height, bgra)) => {
                    if events
                        .send(WorkerEvent::TileRendered {
                            generation: request.generation,
                            appearance: request.appearance,
                            key: request.tile.key,
                            core_rect: request.tile.core_rect,
                            render_rect: request.tile.render_rect,
                            width,
                            height,
                            bgra,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(message) => {
                    if events
                        .send(WorkerEvent::TileFailed {
                            generation: request.generation,
                            appearance: request.appearance,
                            key: request.tile.key,
                            message: format!(
                                "Could not render page {}: {message}",
                                request.tile.key.page + 1
                            ),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
            continue;
        }

        if matches!(work, WorkerWork::Search) {
            if !process_search_work(
                &commands,
                pdfium,
                &events,
                &mut state,
                &mut pending,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        if matches!(work, WorkerWork::Scientific) {
            if !process_scientific_work(
                &commands,
                pdfium,
                &events,
                &mut state,
                &mut pending,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
            continue;
        }

        let WorkerWork::Text {
            page: page_index,
            explicit,
        } = work
        else {
            unreachable!()
        };
        let Some(generation) = state.generation else {
            continue;
        };
        if let Some(text) = state.text_cache.get(&page_index).cloned() {
            state.partial_text.remove(&page_index);
            explicit_text.remove(&page_index);
            automatic_text.retain(|page| *page != page_index);
            if events
                .send(WorkerEvent::TextExtracted {
                    generation,
                    page: page_index,
                    text,
                })
                .is_err()
            {
                return;
            }
            continue;
        }
        let partial = state
            .partial_text
            .remove(&page_index)
            .or_else(|| take_search_partial(&mut state, page_index))
            .unwrap_or_default();
        let mut deferred_commands = Vec::new();
        let mut explicit_replaced = false;
        let extracted = extract_page_text(&state, page_index, partial, || {
            let mut cancel = false;
            for command in commands.try_iter() {
                let replaces_explicit = command_supersedes_text(&command, page_index, explicit);
                explicit_replaced |= replaces_explicit;
                cancel |= matches!(
                    &command,
                    WorkerCommand::Open { .. } | WorkerCommand::RenderViewport { .. }
                ) || replaces_explicit;
                deferred_commands.push(command);
            }
            cancel
        });
        let previous_generation = state.generation;
        for command in deferred_commands {
            if !accept_command(
                command,
                pdfium,
                &events,
                &mut state,
                &mut pending,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
        }
        let extracted = match extracted {
            Ok(TextExtraction::Cancelled(partial)) => {
                if explicit && !explicit_replaced && state.generation == previous_generation {
                    state.partial_text.insert(page_index, partial);
                    explicit_text.insert(page_index);
                }
                continue;
            }
            Ok(TextExtraction::Complete(text)) => Ok(text),
            Err(message) => Err(message),
        };
        if state.generation != Some(generation) {
            continue;
        }

        // Close the small gap between the final character-walk poll and the
        // spatial-index build. A completed explicit page is retained as a
        // resumable prefix, so viewport work can run without throwing it away.
        let mut viewport_superseded = false;
        let mut text_superseded = false;
        for command in commands.try_iter() {
            viewport_superseded |= matches!(
                &command,
                WorkerCommand::Open { .. } | WorkerCommand::RenderViewport { .. }
            );
            text_superseded |= command_supersedes_text(&command, page_index, explicit);
            if !accept_command(
                command,
                pdfium,
                &events,
                &mut state,
                &mut pending,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
        }
        if state.generation != Some(generation) {
            continue;
        }
        if text_superseded || (viewport_superseded && !explicit) {
            continue;
        }
        let extracted = if viewport_superseded {
            match extracted {
                Ok(text) => {
                    state.partial_text.insert(page_index, text);
                    explicit_text.insert(page_index);
                    continue;
                }
                Err(message) => Err(message),
            }
        } else {
            extracted
        };
        explicit_text.remove(&page_index);
        let (text, warning) = cache_text_layer(&mut state.text_cache, page_index, || extracted);
        let mut viewport_changed = false;
        let mut text_superseded = false;
        for command in commands.try_iter() {
            viewport_changed |= matches!(
                &command,
                WorkerCommand::Open { .. } | WorkerCommand::RenderViewport { .. }
            );
            text_superseded |= command_supersedes_text(&command, page_index, explicit);
            if !accept_command(
                command,
                pdfium,
                &events,
                &mut state,
                &mut pending,
                &mut explicit_text,
                &mut automatic_text,
            ) {
                return;
            }
        }
        if state.generation != Some(generation)
            || text_superseded
            || (viewport_changed && !explicit && !automatic_text.contains(&page_index))
        {
            continue;
        }
        explicit_text.remove(&page_index);
        automatic_text.retain(|page| *page != page_index);
        match (text, warning) {
            (Some(text), None) => {
                if events
                    .send(WorkerEvent::TextExtracted {
                        generation,
                        page: page_index,
                        text,
                    })
                    .is_err()
                {
                    return;
                }
            }
            (Some(_), Some(message)) => {
                if events
                    .send(WorkerEvent::TextFailed {
                        generation,
                        page: page_index,
                        message,
                    })
                    .is_err()
                {
                    return;
                }
            }
            (None, None) => {}
            (None, Some(_)) => unreachable!("a warning always carries a cached empty layer"),
        }
    }
}

fn accept_available_commands(
    commands: &mpsc::Receiver<WorkerCommand>,
    pdfium: &'static Pdfium,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    pending: &mut HashMap<TileKey, RenderRequest>,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => {
                if !accept_command(
                    command,
                    pdfium,
                    events,
                    state,
                    pending,
                    explicit_text,
                    automatic_text,
                ) {
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
    pdfium: &'static Pdfium,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    pending: &mut HashMap<TileKey, RenderRequest>,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    for command in deferred {
        if !accept_command(
            command,
            pdfium,
            events,
            state,
            pending,
            explicit_text,
            automatic_text,
        ) {
            return false;
        }
    }
    true
}

fn process_search_work(
    commands: &mpsc::Receiver<WorkerCommand>,
    pdfium: &'static Pdfium,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    pending: &mut HashMap<TileKey, RenderRequest>,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let Some(active) = state.search.clone() else {
        return true;
    };
    if state.generation != Some(active.generation) {
        state.search = None;
        state.search_partial = None;
        return true;
    }
    if active.next_page >= active.page_count {
        state.search = None;
        state.search_partial = None;
        return send_search_finished(events, &active);
    }

    let page = active.next_page;
    let text = if let Some(text) = state.text_cache.get(&page).cloned() {
        text
    } else {
        let partial = take_search_partial(state, page).unwrap_or_default();
        let mut deferred = Vec::new();
        let mut connected = true;
        let extracted = extract_page_text(state, page, partial, || {
            connected &= collect_available_commands(commands, &mut deferred);
            !connected || !deferred.is_empty()
        });
        if !connected {
            return false;
        }
        if !accept_deferred_commands(
            deferred,
            pdfium,
            events,
            state,
            pending,
            explicit_text,
            automatic_text,
        ) {
            return false;
        }

        match extracted {
            Ok(TextExtraction::Cancelled(partial)) => {
                preserve_partial_text(state, page, partial, explicit_text, automatic_text);
                return true;
            }
            Ok(TextExtraction::Complete(characters)) => {
                if state.generation != Some(active.generation) {
                    return true;
                }
                cache_completed_text(state, page, characters)
            }
            Err(message) => {
                if search_job_is_current(state.search.as_ref(), &active) {
                    if let Some(search) = state.search.as_mut() {
                        search.next_page += 1;
                        search.skipped_pages += 1;
                    }
                    if events
                        .send(WorkerEvent::SearchWarning {
                            generation: active.generation,
                            revision: active.revision,
                            page,
                            message: format!("Could not search page {}: {message}", page + 1),
                        })
                        .is_err()
                    {
                        return false;
                    }
                }
                return true;
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
    // Close the publication gap after the matcher's final cancellation poll.
    connected &= collect_available_commands(commands, &mut deferred);
    if !connected {
        return false;
    }
    if !accept_deferred_commands(
        deferred,
        pdfium,
        events,
        state,
        pending,
        explicit_text,
        automatic_text,
    ) {
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
        state.search_partial = None;
        return send_search_finished(events, &finished);
    }
    true
}

fn process_scientific_work(
    commands: &mpsc::Receiver<WorkerCommand>,
    pdfium: &'static Pdfium,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    pending: &mut HashMap<TileKey, RenderRequest>,
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
        let partial = state
            .scientific
            .as_mut()
            .and_then(|job| job.partial.take())
            .filter(|(partial_page, _)| *partial_page == page)
            .map(|(_, partial)| partial)
            .unwrap_or_default();
        let mut deferred = Vec::new();
        let mut connected = true;
        let extracted = extract_page_text(state, page, partial, || {
            connected &= collect_available_commands(commands, &mut deferred);
            !connected || !deferred.is_empty()
        });
        if !connected {
            return false;
        }
        if !accept_deferred_commands(
            deferred,
            pdfium,
            events,
            state,
            pending,
            explicit_text,
            automatic_text,
        ) {
            return false;
        }
        if state.generation != Some(generation) || state.scientific.is_none() {
            return true;
        }
        match extracted {
            Ok(TextExtraction::Cancelled(partial)) => {
                if let Some(job) = state.scientific.as_mut() {
                    job.partial = Some((page, partial));
                }
                return true;
            }
            Ok(TextExtraction::Complete(characters)) => {
                cache_completed_text(state, page, characters)
            }
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
    job.partial = None;
    true
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

fn take_search_partial(state: &mut WorkerState, page: usize) -> Option<Vec<TextChar>> {
    if state
        .search_partial
        .as_ref()
        .is_some_and(|(partial_page, _)| *partial_page == page)
    {
        state.search_partial.take().map(|(_, partial)| partial)
    } else {
        None
    }
}

fn preserve_partial_text(
    state: &mut WorkerState,
    page: usize,
    partial: Vec<TextChar>,
    explicit_text: &BTreeSet<usize>,
    automatic_text: &VecDeque<usize>,
) {
    if explicit_text.contains(&page) || automatic_text.contains(&page) {
        state.partial_text.insert(page, partial);
    } else if state
        .search
        .as_ref()
        .is_some_and(|search| search.next_page == page)
    {
        state.search_partial = Some((page, partial));
    }
}

fn cache_completed_text(
    state: &mut WorkerState,
    page: usize,
    characters: Vec<TextChar>,
) -> Arc<TextLayer> {
    if let Some(text) = state.text_cache.get(&page) {
        return text.clone();
    }
    let (text, warning) = cache_text_layer(&mut state.text_cache, page, || Ok(characters));
    debug_assert!(warning.is_none());
    state.search_partial = None;
    text.or_else(|| state.text_cache.get(&page).cloned())
        .expect("completed text must be cached")
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
    state.search = None;
    state.search_partial = None;
    true
}

fn accept_search_demand(
    state: &mut WorkerState,
    events: &mpsc::SyncSender<WorkerEvent>,
    generation: u64,
    revision: u64,
    query: SearchQuery,
) -> bool {
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
    if state.document.is_none() {
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
        state.search = None;
        state.search_partial = None;
    }
}

fn reset_search_for_open(state: &mut WorkerState) {
    state.page_count = 0;
    state.search = None;
    state.latest_search_revision = None;
    state.search_partial = None;
    state.scientific = None;
}

fn accept_command(
    command: WorkerCommand,
    pdfium: &'static Pdfium,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    pending: &mut HashMap<TileKey, RenderRequest>,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    match command {
        WorkerCommand::Open { generation, path } => {
            pending.clear();
            explicit_text.clear();
            automatic_text.clear();
            state.text_cache.clear();
            state.partial_text.clear();
            state.automatic_text_needs_quiet = false;
            reset_search_for_open(state);
            state.document = None;
            state.preview = None;
            state.generation = Some(generation);

            match open_document(pdfium, &path) {
                Ok((document, pages)) => {
                    let toc = extract_table_of_contents(&document, &pages);
                    let links = extract_document_links(&document, &pages);
                    state.page_count = pages.len();
                    state.scientific = Some(ScientificJob {
                        generation,
                        analyzer: ScientificAnalyzer::new(pages.len(), &links),
                        next_page: 0,
                        partial: None,
                    });
                    state.document = Some(document);
                    if events
                        .send(WorkerEvent::Opened {
                            generation,
                            path,
                            pages,
                            toc,
                            links,
                        })
                        .is_err()
                    {
                        return false;
                    }
                }
                Err(message) => {
                    if events
                        .send(WorkerEvent::Error {
                            generation: Some(generation),
                            message: format!("Could not open PDF: {message}"),
                        })
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            true
        }
        WorkerCommand::RenderViewport {
            generation,
            requests,
            text_pages,
        } => {
            if state.generation == Some(generation) {
                replace_render_demand(state.generation, generation, requests, pending);
                replace_automatic_text_demand(text_pages, automatic_text);
                state.automatic_text_needs_quiet = !automatic_text.is_empty();
            }
            true
        }
        WorkerCommand::ExtractText { generation, page } => {
            if state.generation == Some(generation) {
                explicit_text.clear();
                state.partial_text.retain(|candidate, _| *candidate == page);
                explicit_text.insert(page);
            }
            true
        }
        WorkerCommand::EnsureTextPages { generation, pages } => {
            if state.generation == Some(generation) {
                explicit_text.extend(pages.into_iter().filter(|page| *page < state.page_count));
            }
            true
        }
        WorkerCommand::CancelExplicitText { generation } => {
            if state.generation == Some(generation) {
                explicit_text.clear();
                state.partial_text.clear();
            }
            true
        }
        WorkerCommand::Search {
            generation,
            revision,
            query,
        } => accept_search_demand(state, events, generation, revision, query),
        WorkerCommand::CancelSearch {
            generation,
            next_revision,
        } => {
            cancel_searches_before(state, generation, next_revision);
            true
        }
        WorkerCommand::RenderPreview { request } => {
            if state.generation == Some(request.render.generation) {
                state.preview = Some(request);
            }
            true
        }
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
        | WorkerCommand::CancelSearch { .. } => false,
        WorkerCommand::RenderPreview { .. } => false,
    }
}

fn preview_render_request(
    generation: u64,
    revision: u64,
    appearance: RenderAppearance,
    page: usize,
    raster: RasterSize,
    center_x: f32,
    center_y: f32,
) -> PreviewRequest {
    const PREVIEW_WIDTH: u32 = 360;
    const PREVIEW_HEIGHT: u32 = 204;
    let width = PREVIEW_WIDTH.min(raster.width.max(1));
    let height = PREVIEW_HEIGHT.min(raster.height.max(1));
    let center_x = (center_x.clamp(0.0, 1.0) * raster.width as f32).round() as u32;
    let center_y = (center_y.clamp(0.0, 1.0) * raster.height as f32).round() as u32;
    let rect = PixelRect {
        x: center_x.saturating_sub(width / 2).min(raster.width - width),
        y: center_y
            .saturating_sub(height / 2)
            .min(raster.height - height),
        width,
        height,
    };
    PreviewRequest {
        revision,
        render: RenderRequest {
            generation,
            appearance,
            tile: TileRequest {
                key: TileKey {
                    page,
                    raster,
                    column: rect.x,
                    row: rect.y,
                },
                core_rect: rect,
                render_rect: rect,
            },
            priority: 0,
            prefetch: false,
        },
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

fn replace_render_demand(
    current_generation: Option<u64>,
    generation: u64,
    requests: Vec<RenderRequest>,
    pending: &mut HashMap<TileKey, RenderRequest>,
) {
    if current_generation == Some(generation) {
        pending.clear();
        for request in requests {
            pending.insert(request.tile.key, request);
        }
    }
}

fn should_publish_completed_render(
    current_generation: Option<u64>,
    request: &RenderRequest,
    pending: &mut HashMap<TileKey, RenderRequest>,
) -> bool {
    let still_demanded = pending
        .get(&request.tile.key)
        .is_some_and(|latest| latest.appearance == request.appearance);
    if still_demanded {
        pending.remove(&request.tile.key);
    }
    current_generation == Some(request.generation) && still_demanded
}

fn initialize_pdfium() -> Result<&'static Pdfium, String> {
    let library_name = Pdfium::pdfium_platform_library_name();
    let mut candidates = Vec::new();

    if let Some(configured) = std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH") {
        let configured = PathBuf::from(configured);
        candidates.push(if configured.is_dir() {
            configured.join(&library_name)
        } else {
            configured
        });
    }

    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join(&library_name));
        candidates.push(directory.join("../Resources").join(&library_name));
    }

    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/pdfium/lib")
            .join(&library_name),
    );

    let mut failures = Vec::new();
    for candidate in candidates {
        if candidate.exists() {
            match Pdfium::bind_to_library(&candidate) {
                Ok(bindings) => {
                    return Ok(Box::leak(Box::new(Pdfium::new(bindings))));
                }
                Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                    return Ok(Box::leak(Box::new(Pdfium::default())));
                }
                Err(error) => failures.push(format!("{} ({error})", candidate.display())),
            }
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Box::leak(Box::new(Pdfium::new(bindings)))),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
            Ok(Box::leak(Box::new(Pdfium::default())))
        }
        Err(error) => {
            let detail = if failures.is_empty() {
                String::new()
            } else {
                format!(" Tried: {}.", failures.join(", "))
            };
            Err(format!(
                "PDFium is not installed. Run scripts/fetch-pdfium.sh, or set \
                 PDFIUM_DYNAMIC_LIB_PATH to {}.{detail} System lookup: {error}",
                library_name.to_string_lossy()
            ))
        }
    }
}

fn open_document(
    pdfium: &'static Pdfium,
    path: &Path,
) -> Result<(PdfDocument<'static>, Vec<PageSize>), String> {
    let document: PdfDocument<'static> = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|error| error.to_string())?;
    let pages = document
        .pages()
        .page_sizes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .enumerate()
        .map(|(index, rect)| {
            let width = rect.width().value;
            let height = rect.height().value;
            if !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
                || width > MAX_PAGE_POINTS
                || height > MAX_PAGE_POINTS
            {
                return Err(format!("page {} has invalid dimensions", index + 1));
            }
            Ok(PageSize { width, height })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((document, pages))
}

fn toc_destination_data(
    document: &PdfDocument<'_>,
    destination: &PdfDestination<'_>,
    page_count: usize,
) -> Option<DestinationData> {
    let page = destination
        .page_index()
        .ok()
        .and_then(|page| usize::try_from(page).ok())
        .filter(|page| *page < page_count)?;
    let page_index = i32::try_from(page).ok()?;
    let pdf_page = document.pages().get(page_index).ok()?;
    let boundary = pdf_page
        .boundaries()
        .crop()
        .or_else(|_| pdf_page.boundaries().media())
        .map(|boundary| boundary.bounds)
        .unwrap_or_else(|_| pdf_page.page_size());
    let left = boundary.left();
    let bottom = boundary.bottom();
    let point = match destination.view_settings().ok() {
        Some(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(x, Some(y), _)) => {
            Some((x.unwrap_or(left), y))
        }
        Some(PdfDestinationViewSettings::FitPageHorizontallyToWindow(Some(y)))
        | Some(PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(Some(y))) => {
            Some((left, y))
        }
        Some(PdfDestinationViewSettings::FitPageToRectangle(rect)) => {
            Some((rect.left(), rect.top()))
        }
        Some(PdfDestinationViewSettings::FitPageVerticallyToWindow(Some(x)))
        | Some(PdfDestinationViewSettings::FitBoundsVerticallyToWindow(Some(x))) => {
            Some((x, bottom))
        }
        _ => None,
    };
    let (x_fraction, y_fraction) = point.map_or((None, None), |(x, y)| {
        let (width, height) =
            precision_text_raster(pdf_page.width().value, pdf_page.height().value);
        let config = PdfRenderConfig::new().set_fixed_size(width, height);
        let Ok((device_x, device_y)) = pdf_page.points_to_pixels(x, y, &config) else {
            return (None, None);
        };
        (
            normalized_device_coordinate(device_x, width),
            normalized_device_coordinate(device_y, height),
        )
    });
    Some(DestinationData {
        page,
        x_fraction,
        y_fraction,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DestinationData {
    page: usize,
    x_fraction: Option<f32>,
    y_fraction: Option<f32>,
}

fn normalized_device_coordinate(value: i32, extent: i32) -> Option<f32> {
    if extent <= 0 {
        return None;
    }
    let normalized = value as f32 / extent as f32;
    normalized.is_finite().then_some(normalized.clamp(0.0, 1.0))
}

fn extract_table_of_contents(document: &PdfDocument<'_>, pages: &[PageSize]) -> Vec<TocEntry> {
    let Some(root) = document.bookmarks().root() else {
        return Vec::new();
    };
    let mut pending = vec![(root, 0_u16)];
    let mut visited = std::collections::HashSet::new();
    let mut entries = Vec::new();

    while let Some((bookmark, depth)) = pending.pop() {
        if visited.len() >= MAX_TOC_ENTRIES {
            break;
        }
        if !visited.insert(bookmark.clone()) {
            continue;
        }
        if let Some(sibling) = bookmark.next_sibling() {
            pending.push((sibling, depth));
        }
        if depth < MAX_TOC_DEPTH
            && let Some(child) = bookmark.first_child()
        {
            pending.push((child, depth + 1));
        }

        let Some(title) = bookmark.title_with_limit(MAX_TOC_TITLE_UTF16_BYTES) else {
            continue;
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            continue;
        }
        let destination = bookmark
            .destination()
            .and_then(|destination| toc_destination_data(document, &destination, pages.len()))
            .or_else(|| {
                let action = bookmark.action()?;
                let destination = action.as_local_destination_action()?.destination().ok()?;
                toc_destination_data(document, &destination, pages.len())
            });
        if let Some(destination) = destination {
            entries.push(TocEntry {
                title,
                page: destination.page,
                depth,
                destination_y: destination.y_fraction,
            });
        }
    }

    entries
}

fn extract_document_links(document: &PdfDocument<'_>, pages: &[PageSize]) -> Vec<DocumentLink> {
    let mut result = Vec::new();
    for page_index in 0..pages.len() {
        if result.len() >= MAX_DOCUMENT_LINKS {
            break;
        }
        let Ok(pdf_page_index) = i32::try_from(page_index) else {
            break;
        };
        let Ok(page) = document.pages().get(pdf_page_index) else {
            continue;
        };
        let (width, height) = precision_text_raster(page.width().value, page.height().value);
        let config = PdfRenderConfig::new().set_fixed_size(width, height);
        for link in page
            .links()
            .iter()
            .take(MAX_DOCUMENT_LINKS.saturating_sub(result.len()))
        {
            let Some(bounds) = link
                .rect()
                .ok()
                .and_then(|rect| normalized_link_bounds(&page, rect, &config, width, height))
            else {
                continue;
            };
            let destination = link
                .destination()
                .and_then(|destination| toc_destination_data(document, &destination, pages.len()));
            let target = destination
                .or_else(|| {
                    let action = link.action()?;
                    let destination = action.as_local_destination_action()?.destination().ok()?;
                    toc_destination_data(document, &destination, pages.len())
                })
                .map(|destination| PdfLinkTarget::Internal {
                    page: destination.page,
                    x_fraction: destination.x_fraction,
                    y_fraction: destination.y_fraction,
                })
                .or_else(|| {
                    let action = link.action()?;
                    let uri = action.as_uri_action()?.uri().ok()?;
                    validated_link_url(&uri).map(|url| PdfLinkTarget::External { url })
                });
            let Some(target) = target else {
                continue;
            };
            result.push(DocumentLink {
                id: result.len(),
                page: page_index,
                bounds,
                target,
            });
        }
    }
    result
}

fn normalized_link_bounds(
    page: &PdfPage<'_>,
    rect: PdfRect,
    config: &PdfRenderConfig,
    width: i32,
    height: i32,
) -> Option<TextBounds> {
    let points = [
        (rect.left(), rect.top()),
        (rect.right(), rect.top()),
        (rect.right(), rect.bottom()),
        (rect.left(), rect.bottom()),
    ];
    let mut left = f32::INFINITY;
    let mut top = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for (x, y) in points {
        let (device_x, device_y) = page.points_to_pixels(x, y, config).ok()?;
        let normalized_x = normalized_device_coordinate(device_x, width)?;
        let normalized_y = normalized_device_coordinate(device_y, height)?;
        left = left.min(normalized_x);
        top = top.min(normalized_y);
        right = right.max(normalized_x);
        bottom = bottom.max(normalized_y);
    }
    (right > left && bottom > top).then_some(TextBounds {
        left,
        top,
        right,
        bottom,
    })
}

fn validated_link_url(uri: &str) -> Option<String> {
    if uri.len() > MAX_LINK_URI_BYTES {
        return None;
    }
    let parsed = Url::parse(uri).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

type RenderOutput = (u32, u32, Vec<u8>);

fn render_tile(state: &mut WorkerState, request: &RenderRequest) -> Result<RenderOutput, String> {
    validate_tile_request(request.tile)?;
    let document = state.document.as_ref().ok_or("no document is open")?;
    let page_index = i32::try_from(request.tile.key.page).map_err(|_| "page index is too large")?;
    let page = document
        .pages()
        .get(page_index)
        .map_err(|error| error.to_string())?;

    let full_width = i32::try_from(request.tile.key.raster.width)
        .map_err(|_| "page raster width is too large")?;
    let full_height = i32::try_from(request.tile.key.raster.height)
        .map_err(|_| "page raster height is too large")?;
    let render_left =
        i32::try_from(request.tile.render_rect.x).map_err(|_| "tile x origin is too large")?;
    let render_top =
        i32::try_from(request.tile.render_rect.y).map_err(|_| "tile y origin is too large")?;
    let render_width =
        i32::try_from(request.tile.render_rect.width).map_err(|_| "tile width is too large")?;
    let render_height =
        i32::try_from(request.tile.render_rect.height).map_err(|_| "tile height is too large")?;
    let mut config = PdfRenderConfig::new()
        .set_fixed_size(full_width, full_height)
        // GPUI's RenderImage upload path expects BGRA on macOS. Keeping
        // PDFium's native byte order avoids a tile-wide channel conversion.
        .set_reverse_byte_order(false)
        .render_annotations(true)
        .limit_render_image_cache_size(true)
        .render_form_data(true);
    if let RenderAppearance::ForcedColors {
        background,
        foreground,
    } = request.appearance
    {
        let foreground = foreground.as_pdfium();
        config = config
            .set_clear_color(background.as_pdfium())
            .set_color_scheme(PdfPageRenderColorScheme::new(
                foreground, foreground, foreground, foreground,
            ))
            .render_fills_as_strokes(true);
    }
    let bitmap = page
        .render_tile_with_config(
            &config,
            render_left,
            render_top,
            render_width,
            render_height,
        )
        .map_err(|error| error.to_string())?;
    let rendered_width = u32::try_from(bitmap.width()).map_err(|_| "invalid tile width")?;
    let rendered_height = u32::try_from(bitmap.height()).map_err(|_| "invalid tile height")?;
    if rendered_width != request.tile.render_rect.width
        || rendered_height != request.tile.render_rect.height
    {
        return Err("PDFium returned an unexpected tile size".into());
    }
    let bgra = bitmap.as_raw_bytes();
    let expected_len = rendered_width
        .checked_mul(rendered_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or("render tile byte count overflows")?;
    if bgra.len() != expected_len {
        return Err("PDFium returned an invalid tile buffer".into());
    }

    Ok((rendered_width, rendered_height, bgra))
}

fn precision_text_raster(page_width: f32, page_height: f32) -> (i32, i32) {
    let longest = page_width.max(page_height).max(f32::MIN_POSITIVE);
    let scaled = |dimension: f32| {
        ((dimension / longest) * MAX_RASTER_DIMENSION as f32)
            .round()
            .clamp(1.0, MAX_RASTER_DIMENSION as f32) as i32
    };
    (scaled(page_width), scaled(page_height))
}

fn cache_text_layer(
    cache: &mut HashMap<usize, Arc<TextLayer>>,
    page: usize,
    extract: impl FnOnce() -> Result<Vec<TextChar>, String>,
) -> (Option<Arc<TextLayer>>, Option<String>) {
    if cache.contains_key(&page) {
        return (None, None);
    }
    let result = match extract() {
        Ok(extracted) => {
            let extracted = Arc::new(TextLayer::new(extracted));
            cache.insert(page, extracted.clone());
            (Some(extracted), None)
        }
        Err(message) => {
            // A malformed or unsupported text layer must never discard a
            // successfully rendered bitmap. Cache an empty layer so every
            // tile on the page does not repeat the same failing call.
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

    while cache.len() > MAX_CACHED_TEXT_PAGES {
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
    result
}

fn validate_tile_request(tile: TileRequest) -> Result<(), String> {
    let raster = tile.key.raster;
    if raster.width == 0
        || raster.height == 0
        || raster.width > MAX_RASTER_DIMENSION
        || raster.height > MAX_RASTER_DIMENSION
    {
        return Err("page raster dimensions are outside the supported range".into());
    }
    if tile.core_rect.width == 0
        || tile.core_rect.height == 0
        || tile.render_rect.width == 0
        || tile.render_rect.height == 0
        || tile.render_rect.width > MAX_TILE_DIMENSION
        || tile.render_rect.height > MAX_TILE_DIMENSION
    {
        return Err("tile dimensions are outside the supported range".into());
    }

    let core_right = rect_right(tile.core_rect).ok_or("tile core overflows")?;
    let core_bottom = rect_bottom(tile.core_rect).ok_or("tile core overflows")?;
    let render_right = rect_right(tile.render_rect).ok_or("render tile overflows")?;
    let render_bottom = rect_bottom(tile.render_rect).ok_or("render tile overflows")?;
    if core_right > raster.width
        || core_bottom > raster.height
        || render_right > raster.width
        || render_bottom > raster.height
        || tile.render_rect.x > tile.core_rect.x
        || tile.render_rect.y > tile.core_rect.y
        || render_right < core_right
        || render_bottom < core_bottom
    {
        return Err("tile lies outside its page raster".into());
    }
    Ok(())
}

fn rect_right(rect: PixelRect) -> Option<u32> {
    rect.x.checked_add(rect.width)
}

fn rect_bottom(rect: PixelRect) -> Option<u32> {
    rect.y.checked_add(rect.height)
}

fn extract_page_text(
    state: &WorkerState,
    page: usize,
    extracted: Vec<TextChar>,
    should_cancel: impl FnMut() -> bool,
) -> Result<TextExtraction, String> {
    let document = state.document.as_ref().ok_or("no document is open")?;
    extract_page_text_from_document(document, page, extracted, should_cancel)
}

fn extract_page_text_from_document(
    document: &PdfDocument<'static>,
    page: usize,
    mut extracted: Vec<TextChar>,
    mut should_cancel: impl FnMut() -> bool,
) -> Result<TextExtraction, String> {
    let page_index = i32::try_from(page).map_err(|_| "page index is too large")?;
    let page = document
        .pages()
        .get(page_index)
        .map_err(|error| error.to_string())?;
    if should_cancel() {
        return Ok(TextExtraction::Cancelled(extracted));
    }

    // FPDFText_LoadPage is synchronous, but checking immediately before and
    // after it prevents the much longer per-character walk from delaying a
    // replacement viewport.
    let text = page.text().map_err(|error| error.to_string())?;
    if should_cancel() {
        return Ok(TextExtraction::Cancelled(extracted));
    }
    let character_count = validate_text_character_count(text.len())?;
    if extracted.len() > character_count {
        extracted.clear();
    }
    let (text_width, text_height) = precision_text_raster(page.width().value, page.height().value);
    let config = PdfRenderConfig::new().set_fixed_size(text_width, text_height);
    let pixel_width = u32::try_from(text_width).map_err(|_| "invalid text coordinate width")?;
    let pixel_height = u32::try_from(text_height).map_err(|_| "invalid text coordinate height")?;
    extracted.reserve(character_count.saturating_sub(extracted.len()));

    for index in extracted.len()..character_count {
        if index.is_multiple_of(TEXT_CANCEL_INTERVAL) && should_cancel() {
            return Ok(TextExtraction::Cancelled(extracted));
        }
        // SAFETY: `character_count` came from this live PdfPageText, was
        // validated against our 100k cap, and the loop stays strictly below it.
        let character = unsafe { text.char_at_unchecked(index) };
        let bounds = character.loose_bounds().ok().and_then(|bounds| {
            let top_left = page
                .points_to_pixels(bounds.left(), bounds.top(), &config)
                .ok()?;
            let top_right = page
                .points_to_pixels(bounds.right(), bounds.top(), &config)
                .ok()?;
            let bottom_left = page
                .points_to_pixels(bounds.left(), bounds.bottom(), &config)
                .ok()?;
            let bottom_right = page
                .points_to_pixels(bounds.right(), bounds.bottom(), &config)
                .ok()?;
            normalized_text_bounds(
                [top_left, top_right, bottom_left, bottom_right],
                pixel_width,
                pixel_height,
            )
        });
        extracted.push(TextChar {
            value: if character.unicode_value() == 0 {
                '\0'
            } else {
                character.unicode_char().unwrap_or('\0')
            },
            bounds,
        });
    }

    if should_cancel() {
        Ok(TextExtraction::Cancelled(extracted))
    } else {
        Ok(TextExtraction::Complete(extracted))
    }
}

fn validate_text_character_count(count: i32) -> Result<usize, String> {
    let count = usize::try_from(count).map_err(|_| "PDFium returned a negative character count")?;
    if count > MAX_PAGE_TEXT_CHARS {
        Err(format!(
            "the page text layer has {count} characters; the safety limit is {MAX_PAGE_TEXT_CHARS}"
        ))
    } else {
        Ok(count)
    }
}

fn normalized_text_bounds(
    pixels: [(i32, i32); 4],
    pixel_width: u32,
    pixel_height: u32,
) -> Option<TextBounds> {
    if pixel_width == 0 || pixel_height == 0 {
        return None;
    }
    let mut left = pixels.iter().map(|(x, _)| *x).min()? as f32 / pixel_width as f32;
    let mut right = pixels.iter().map(|(x, _)| *x).max()? as f32 / pixel_width as f32;
    let mut top = pixels.iter().map(|(_, y)| *y).min()? as f32 / pixel_height as f32;
    let mut bottom = pixels.iter().map(|(_, y)| *y).max()? as f32 / pixel_height as f32;
    if ![left, right, top, bottom].into_iter().all(f32::is_finite) {
        return None;
    }
    left = left.clamp(0.0, 1.0);
    right = right.clamp(0.0, 1.0);
    top = top.clamp(0.0, 1.0);
    bottom = bottom.clamp(0.0, 1.0);
    (right > left && bottom > top).then_some(TextBounds {
        left,
        top,
        right,
        bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    static PDFIUM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn preview_crop_is_bounded_and_clamped_at_page_edges() {
        let request = preview_render_request(
            4,
            9,
            RenderAppearance::Normal,
            3,
            RasterSize {
                width: 800,
                height: 1_200,
            },
            1.0,
            0.0,
        );
        assert_eq!(request.revision, 9);
        assert_eq!(request.render.tile.key.page, 3);
        assert_eq!(request.render.tile.render_rect.width, 360);
        assert_eq!(request.render.tile.render_rect.height, 204);
        assert_eq!(request.render.tile.render_rect.x, 440);
        assert_eq!(request.render.tile.render_rect.y, 0);
        validate_tile_request(request.render.tile).unwrap();
    }

    #[test]
    fn pdfium_opens_renders_and_extracts_the_integration_fixture() {
        let _pdfium_guard = PDFIUM_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pdfium = initialize_pdfium().expect("the pinned PDFium binary should load");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interaction.pdf");
        let (document, pages) = open_document(pdfium, &path).expect("fixture should open");
        assert_eq!(
            pages,
            [
                PageSize {
                    width: 612.0,
                    height: 792.0,
                },
                PageSize {
                    width: 612.0,
                    height: 792.0,
                },
                PageSize {
                    width: 648.0,
                    height: 360.0,
                },
            ]
        );
        let root_bookmark = document.bookmarks().root().expect("fixture has an outline");
        assert_eq!(root_bookmark.title_with_limit(2), None);
        assert_eq!(
            root_bookmark.title_with_limit(MAX_TOC_TITLE_UTF16_BYTES),
            Some("Getting Started".to_owned())
        );
        let toc = extract_table_of_contents(&document, &pages);
        assert_eq!(toc.len(), 4);
        assert_eq!(
            toc.iter()
                .map(|entry| (entry.title.as_str(), entry.page, entry.depth))
                .collect::<Vec<_>>(),
            [
                ("Getting Started", 0, 0),
                ("Selecting text", 0, 1),
                ("Page 2 - Rotate 90", 1, 0),
                ("Wide documents", 2, 0),
            ]
        );
        assert!(
            toc[0]
                .destination_y
                .is_some_and(|y| (0.0..0.1).contains(&y))
        );
        assert!(
            toc[1]
                .destination_y
                .is_some_and(|y| (0.2..0.5).contains(&y))
        );
        assert_eq!(toc[2].destination_y, None);
        assert!(toc[3].destination_y.is_some());
        let links = extract_document_links(&document, &pages);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].page, 0);
        assert!(links[0].bounds.left < links[0].bounds.right);
        assert_eq!(
            links[0].target,
            PdfLinkTarget::External {
                url: "https://example.com/research?source=pdf".to_owned(),
            }
        );
        assert_eq!(links[1].page, 0);
        match links[1].target {
            PdfLinkTarget::Internal {
                page,
                x_fraction,
                y_fraction,
            } => {
                assert_eq!(page, 2);
                assert!(x_fraction.is_some_and(|value| (0.0..=1.0).contains(&value)));
                assert!(y_fraction.is_some_and(|value| (0.0..=1.0).contains(&value)));
            }
            _ => panic!("second fixture link should be internal"),
        }

        let mut state = WorkerState {
            document: Some(document),
            generation: Some(1),
            text_cache: HashMap::new(),
            partial_text: HashMap::new(),
            automatic_text_needs_quiet: false,
            page_count: pages.len(),
            search: None,
            latest_search_revision: None,
            search_partial: None,
            scientific: None,
            preview: None,
        };
        let source_text = match extract_page_text(&state, 0, Vec::new(), || false)
            .expect("fixture source text should extract")
        {
            TextExtraction::Complete(characters) => TextLayer::new(characters),
            TextExtraction::Cancelled(_) => panic!("fixture source text unexpectedly cancelled"),
        };
        let target_text = match extract_page_text(&state, 2, Vec::new(), || false)
            .expect("fixture target text should extract")
        {
            TextExtraction::Complete(characters) => TextLayer::new(characters),
            TextExtraction::Cancelled(_) => panic!("fixture target text unexpectedly cancelled"),
        };
        let source = crate::link_resolution::link_source_text(&source_text, links[1].bounds);
        let resolved = match links[1].target {
            PdfLinkTarget::Internal {
                page,
                x_fraction,
                y_fraction,
            } => crate::link_resolution::resolve_internal_link(
                &source_text,
                links[1].bounds,
                &target_text,
                page,
                x_fraction,
                y_fraction,
            ),
            _ => panic!("fixture link should remain internal"),
        };
        assert!(
            resolved.matched_source,
            "PDFium link source {source:?} should resolve to its reference entry"
        );
        assert!(resolved.preview.starts_with("[12] Synthetic reference"));
        assert!(resolved.preview.contains("Continued journal and DOI"));
        assert!(!resolved.preview.contains("[13]"));
        assert!(matches!(
            extract_page_text(&state, 0, Vec::new(), || true),
            Ok(TextExtraction::Cancelled(_))
        ));
        let mut cancellation_checks = 0;
        let partial = match extract_page_text(&state, 0, Vec::new(), || {
            cancellation_checks += 1;
            cancellation_checks >= 4
        })
        .expect("cancellable extraction should not fail")
        {
            TextExtraction::Cancelled(partial) => partial,
            TextExtraction::Complete(_) => panic!("the fixture should reach a cancellation poll"),
        };
        assert_eq!(
            cancellation_checks, 4,
            "the character walk must poll periodically after FPDFText_LoadPage"
        );
        assert!(
            !partial.is_empty(),
            "cancellation must retain completed work"
        );
        let resumed_page_zero = match extract_page_text(&state, 0, partial, || false)
            .expect("a partial explicit extraction should resume")
        {
            TextExtraction::Complete(text) => text,
            TextExtraction::Cancelled(_) => panic!("resume was not cancelled"),
        };
        let mut rendered_text = Vec::new();
        for page in 0..pages.len() {
            let (width, height) = [(612, 792), (612, 792), (612, 340)][page];
            let request = RenderRequest {
                generation: 1,
                appearance: RenderAppearance::Normal,
                tile: TileRequest {
                    key: TileKey {
                        page,
                        raster: RasterSize { width, height },
                        column: 0,
                        row: 0,
                    },
                    core_rect: PixelRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    render_rect: PixelRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                },
                priority: 0,
                prefetch: false,
            };
            let (width, height, bgra) =
                render_tile(&mut state, &request).expect("fixture page should render");
            let text = match extract_page_text(&state, page, Vec::new(), || false)
                .expect("the deferred text path should work")
            {
                TextExtraction::Complete(text) => Arc::new(TextLayer::new(text)),
                TextExtraction::Cancelled(_) => {
                    panic!("an extraction with no cancellation changed")
                }
            };
            assert_eq!(bgra.len(), width as usize * height as usize * 4);
            assert_eq!((width, height), [(612, 792), (612, 792), (612, 340)][page]);
            let extracted: String = text
                .iter()
                .filter_map(|character| (character.value != '\0').then_some(character.value))
                .collect();
            assert!(extracted.contains(["integration fixture", "Rotate 90", "wide CropBox"][page]));
            if page == 0 {
                assert_eq!(text.as_slice(), resumed_page_zero.as_slice());
                assert!(extracted.contains("GPUI PDF Reader © Ω 你好—"));
                assert_eq!(pixel(&bgra, width, 100, 100), [0, 0, 255, 255]);
                assert_eq!(pixel(&bgra, width, 300, 100), [0, 255, 0, 255]);
                assert_eq!(pixel(&bgra, width, 480, 100), [255, 0, 0, 255]);
            }
            assert!(text.iter().filter_map(|character| character.bounds).count() > 20);
            assert!(
                text.iter()
                    .filter_map(|character| character.bounds)
                    .all(|bounds| {
                        bounds.left >= -0.02
                            && bounds.top >= -0.02
                            && bounds.right <= 1.02
                            && bounds.bottom <= 1.02
                            && bounds.left <= bounds.right
                            && bounds.top <= bounds.bottom
                    })
            );

            rendered_text.push(text);
        }

        for (page, text) in rendered_text.iter().enumerate() {
            let query = SearchQuery::new("page").unwrap();
            let SearchPageOutcome::Complete(results) =
                search_page(page, text.as_slice(), &query, MAX_SEARCH_RESULTS, || false)
            else {
                panic!("fixture search unexpectedly cancelled");
            };
            assert!(
                !results.matches.is_empty(),
                "the common term must be found on fixture page {}",
                page + 1
            );
            assert!(results.matches.iter().all(|result| {
                result.id.page == page
                    && !result.highlight_runs.is_empty()
                    && result.highlight_runs.iter().all(|run| {
                        [run.left, run.top, run.right, run.bottom]
                            .into_iter()
                            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
                    })
            }));
        }

        for (page, query_text) in [
            (0, "gpui pdf reader"),
            (1, "rotate 90"),
            (2, "wide cropbox"),
            (0, "ω"),
        ] {
            let query = SearchQuery::new(query_text).unwrap();
            let SearchPageOutcome::Complete(results) = search_page(
                page,
                rendered_text[page].as_slice(),
                &query,
                MAX_SEARCH_RESULTS,
                || false,
            ) else {
                panic!("fixture search unexpectedly cancelled");
            };
            let result = results
                .matches
                .first()
                .unwrap_or_else(|| panic!("{query_text:?} should match page {}", page + 1));
            let source: String = rendered_text[page][result.id.range()]
                .iter()
                .filter_map(|character| (character.value != '\0').then_some(character.value))
                .collect();
            assert_eq!(source.to_lowercase(), query_text);
            assert!(!result.highlight_runs.is_empty());
        }

        let high_resolution_tile = RenderRequest {
            generation: 1,
            appearance: RenderAppearance::Normal,
            tile: TileRequest {
                key: TileKey {
                    page: 0,
                    raster: RasterSize {
                        width: 8_192,
                        height: 10_604,
                    },
                    column: 1,
                    row: 1,
                },
                core_rect: PixelRect {
                    x: 1_024,
                    y: 1_024,
                    width: 1_024,
                    height: 1_024,
                },
                render_rect: PixelRect {
                    x: 992,
                    y: 992,
                    width: 1_088,
                    height: 1_088,
                },
            },
            priority: 0,
            prefetch: false,
        };
        let (width, height, bgra) = render_tile(&mut state, &high_resolution_tile)
            .expect("a high-resolution request should allocate only its bounded tile");
        assert_eq!((width, height), (1_088, 1_088));
        assert_eq!(bgra.len(), 1_088 * 1_088 * 4);

        let page_text: Vec<_> = rendered_text
            .iter()
            .map(|text| Some(text.as_slice()))
            .collect();
        let copied = selected_text(
            TextSelection {
                anchor: TextPosition { page: 0, index: 0 },
                focus: TextPosition {
                    page: rendered_text.len() - 1,
                    index: rendered_text.last().unwrap().len() - 1,
                },
            },
            &page_text,
        );
        assert!(copied.contains("GPUI PDF Reader © Ω 你好—"));
        assert!(copied.contains("Page 2 - Rotate 90"));
        assert!(copied.contains("Page 3 - wide CropBox"));
        assert!(copied.matches("\n\n").count() >= 2);
        assert!(!copied.contains('\0'));
    }

    #[test]
    fn pdfium_scientific_analysis_recovers_unannotated_superscript_references() {
        let _pdfium_guard = PDFIUM_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pdfium = initialize_pdfium().expect("the pinned PDFium binary should load");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scientific-unlinked.pdf");
        let (document, pages) = open_document(pdfium, &fixture).expect("fixture should open");
        let links = extract_document_links(&document, &pages);
        assert!(
            links.is_empty(),
            "the fixture must prove inferred references rather than authored links"
        );
        let state = WorkerState {
            document: Some(document),
            generation: Some(1),
            text_cache: HashMap::new(),
            partial_text: HashMap::new(),
            automatic_text_needs_quiet: false,
            page_count: pages.len(),
            search: None,
            latest_search_revision: None,
            search_partial: None,
            scientific: None,
            preview: None,
        };
        let mut analyzer = ScientificAnalyzer::new(pages.len(), &links);
        for page in analyzer.page_order().to_vec() {
            let text = match extract_page_text(&state, page, Vec::new(), || false)
                .expect("fixture text should extract")
            {
                TextExtraction::Complete(characters) => TextLayer::new(characters),
                TextExtraction::Cancelled(_) => {
                    panic!("fixture text extraction unexpectedly cancelled")
                }
            };
            analyzer.ingest_page(page, &text);
        }
        let analysis = analyzer.finish();
        assert!(analysis.is_scientific);
        assert_eq!(analysis.signals.reference_entries, 8);
        assert!(analysis.signals.doi_entries >= 2);
        assert_eq!(analysis.signals.bracket_citations, 1);
        assert!(
            analysis.signals.superscript_citations >= 4,
            "signals were {:?}",
            analysis.signals
        );
        assert_eq!(analysis.synthetic_links.len(), 5);
        assert!(
            analysis
                .synthetic_links
                .iter()
                .all(|link| { matches!(link.target, PdfLinkTarget::Internal { page: 2, .. }) })
        );
    }

    #[test]
    fn link_urls_accept_only_bounded_http_and_https_targets() {
        assert_eq!(
            validated_link_url("https://example.com/a b"),
            Some("https://example.com/a%20b".to_owned())
        );
        assert_eq!(
            validated_link_url("http://example.com"),
            Some("http://example.com/".to_owned())
        );
        assert_eq!(validated_link_url("javascript:alert(1)"), None);
        assert_eq!(validated_link_url("file:///etc/passwd"), None);
        assert_eq!(
            validated_link_url(&"https://example.com/".repeat(600)),
            None
        );
    }

    #[test]
    fn tile_validation_rejects_unbounded_or_non_containing_requests() {
        let key = TileKey {
            page: 0,
            raster: RasterSize {
                width: 2_000,
                height: 3_000,
            },
            column: 0,
            row: 0,
        };
        let valid = TileRequest {
            key,
            core_rect: PixelRect {
                x: 8,
                y: 8,
                width: 1_024,
                height: 1_024,
            },
            render_rect: PixelRect {
                x: 0,
                y: 0,
                width: 1_088,
                height: 1_088,
            },
        };
        assert!(validate_tile_request(valid).is_ok());
        assert!(
            validate_tile_request(TileRequest {
                render_rect: PixelRect {
                    x: 9,
                    ..valid.render_rect
                },
                ..valid
            })
            .is_err()
        );
        assert!(
            validate_tile_request(TileRequest {
                key: TileKey {
                    raster: RasterSize {
                        width: MAX_RASTER_DIMENSION + 1,
                        height: 3_000,
                    },
                    ..key
                },
                ..valid
            })
            .is_err()
        );
    }

    #[test]
    fn viewport_scheduler_keeps_sibling_tiles_and_replaces_stale_demand() {
        let mut pending = HashMap::new();
        let request = |column, row, priority| {
            let key = TileKey {
                page: 0,
                raster: RasterSize {
                    width: 2_048,
                    height: 2_048,
                },
                column,
                row,
            };
            RenderRequest {
                generation: 7,
                appearance: RenderAppearance::Normal,
                tile: TileRequest {
                    key,
                    core_rect: PixelRect {
                        x: column * 1_024,
                        y: row * 1_024,
                        width: 1_024,
                        height: 1_024,
                    },
                    render_rect: PixelRect {
                        x: column * 1_024,
                        y: row * 1_024,
                        width: 1_024,
                        height: 1_024,
                    },
                },
                priority,
                prefetch: false,
            }
        };
        let requests = vec![
            request(0, 0, 0),
            request(1, 0, 1),
            request(0, 1, 2),
            request(1, 1, 3),
            request(0, 0, 4),
        ];
        replace_render_demand(Some(7), 7, requests, &mut pending);
        assert_eq!(
            pending.len(),
            4,
            "sibling tiles must not overwrite one another"
        );
        assert_eq!(pending[&request(0, 0, 0).tile.key].priority, 4);

        replace_render_demand(Some(7), 7, vec![request(1, 1, 0)], &mut pending);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&request(1, 1, 0).tile.key));

        replace_render_demand(Some(8), 7, vec![request(0, 0, 0)], &mut pending);
        assert_eq!(pending.len(), 1, "stale generations must be ignored");

        let in_flight = request(0, 0, 0);
        replace_render_demand(Some(7), 7, vec![in_flight.clone()], &mut pending);
        replace_render_demand(Some(7), 7, Vec::new(), &mut pending);
        assert!(
            !should_publish_completed_render(Some(7), &in_flight, &mut pending),
            "a canceled in-flight completion, including an error, must be discarded"
        );

        replace_render_demand(Some(7), 7, vec![in_flight.clone()], &mut pending);
        assert!(should_publish_completed_render(
            Some(7),
            &in_flight,
            &mut pending
        ));
        assert!(pending.is_empty(), "a completion must publish only once");

        let forced_colors = RenderAppearance::ForcedColors {
            background: RenderColor {
                red: 24,
                green: 24,
                blue: 24,
            },
            foreground: RenderColor {
                red: 224,
                green: 224,
                blue: 224,
            },
        };
        let mut dark_replacement = in_flight.clone();
        dark_replacement.appearance = forced_colors;
        replace_render_demand(Some(7), 7, vec![dark_replacement.clone()], &mut pending);
        assert!(
            !should_publish_completed_render(Some(7), &in_flight, &mut pending),
            "an in-flight light tile must not satisfy replacement dark demand"
        );
        assert_eq!(pending.len(), 1, "stale completion must retain new demand");
        assert!(should_publish_completed_render(
            Some(7),
            &dark_replacement,
            &mut pending
        ));

        replace_render_demand(Some(7), 7, vec![in_flight.clone()], &mut pending);
        assert!(
            !should_publish_completed_render(Some(8), &in_flight, &mut pending),
            "a completion from the previous document must be discarded"
        );

        let mut automatic_text = VecDeque::new();
        replace_automatic_text_demand(vec![8, 4, 8, 5], &mut automatic_text);
        assert_eq!(automatic_text, VecDeque::from([8, 4, 5]));
        replace_automatic_text_demand(vec![9, 3], &mut automatic_text);
        assert_eq!(
            automatic_text,
            VecDeque::from([9, 3]),
            "a new viewport must replace stale pages while preserving priority"
        );
    }

    #[test]
    fn pdfium_forced_colors_render_dark_background_and_light_content() {
        let _pdfium_guard = PDFIUM_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pdfium = initialize_pdfium().expect("the pinned PDFium binary should load");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interaction.pdf");
        let (document, pages) = open_document(pdfium, &fixture).expect("fixture should open");
        let mut state = WorkerState {
            document: Some(document),
            generation: Some(1),
            text_cache: HashMap::new(),
            partial_text: HashMap::new(),
            automatic_text_needs_quiet: false,
            page_count: pages.len(),
            search: None,
            latest_search_revision: None,
            search_partial: None,
            scientific: None,
            preview: None,
        };
        let key = TileKey {
            page: 0,
            raster: RasterSize {
                width: 612,
                height: 792,
            },
            column: 0,
            row: 0,
        };
        let tile = TileRequest {
            key,
            core_rect: PixelRect {
                x: 0,
                y: 0,
                width: 612,
                height: 792,
            },
            render_rect: PixelRect {
                x: 0,
                y: 0,
                width: 612,
                height: 792,
            },
        };
        let normal = RenderRequest {
            generation: 1,
            appearance: RenderAppearance::Normal,
            tile,
            priority: 0,
            prefetch: false,
        };
        let dark = RenderRequest {
            appearance: RenderAppearance::ForcedColors {
                background: RenderColor {
                    red: 18,
                    green: 52,
                    blue: 86,
                },
                foreground: RenderColor {
                    red: 171,
                    green: 205,
                    blue: 239,
                },
            },
            ..normal.clone()
        };
        let (_, _, normal_bgra) = render_tile(&mut state, &normal).expect("normal tile renders");
        let (_, _, dark_bgra) = render_tile(&mut state, &dark).expect("dark tile renders");

        assert_ne!(dark_bgra, normal_bgra);
        let dark_background = dark_bgra
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == [86, 52, 18])
            .count();
        let light_content = dark_bgra
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == [239, 205, 171])
            .count();
        assert!(dark_background > 300_000, "most of the page should be dark");
        assert!(
            light_content > 100,
            "forced text/path pixels should be light"
        );
    }

    #[test]
    fn text_failure_is_cached_as_a_non_fatal_empty_layer() {
        let calls = std::cell::Cell::new(0);
        let mut cache = HashMap::new();
        let (text, warning) = cache_text_layer(&mut cache, 2, || {
            calls.set(calls.get() + 1);
            Err("synthetic text failure".into())
        });
        assert!(text.unwrap().is_empty());
        assert!(warning.unwrap().contains("synthetic text failure"));

        let (text, warning) = cache_text_layer(&mut cache, 2, || {
            calls.set(calls.get() + 1);
            Ok(Vec::new())
        });
        assert!(text.is_none());
        assert!(warning.is_none());
        assert_eq!(calls.get(), 1, "sibling tiles must not retry failed text");
    }

    #[test]
    fn text_coordinates_use_a_stable_high_precision_raster() {
        assert_eq!(precision_text_raster(612.0, 792.0), (50_641, 65_536));
        assert_eq!(precision_text_raster(648.0, 360.0), (65_536, 36_409));
        assert_eq!(precision_text_raster(65_536.0, 1.0), (65_536, 1));
    }

    #[test]
    fn text_character_counts_and_bounds_are_strictly_bounded() {
        assert_eq!(validate_text_character_count(42).unwrap(), 42);
        assert!(validate_text_character_count(-1).is_err());
        assert!(validate_text_character_count(MAX_PAGE_TEXT_CHARS as i32 + 1).is_err());

        assert_eq!(
            normalized_text_bounds([(-20, -30), (120, -30), (-20, 80), (120, 80)], 100, 100),
            Some(TextBounds {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 0.8,
            })
        );
        assert_eq!(
            normalized_text_bounds([(200, 20), (220, 20), (200, 40), (220, 40)], 100, 100),
            None,
            "a character entirely outside the page must not create a highlight"
        );
    }

    #[test]
    fn worker_text_cache_evicts_distant_pages() {
        let mut cache = HashMap::new();
        for page in 0..=MAX_CACHED_TEXT_PAGES {
            let (layer, warning) = cache_text_layer(&mut cache, page, || Ok(Vec::new()));
            assert!(layer.is_some());
            assert!(warning.is_none());
        }
        assert_eq!(cache.len(), MAX_CACHED_TEXT_PAGES);
        assert!(cache.contains_key(&MAX_CACHED_TEXT_PAGES));
        assert!(!cache.contains_key(&0));
    }

    #[test]
    fn explicit_text_replacement_cancels_only_superseded_work() {
        let replacement = WorkerCommand::ExtractText {
            generation: 7,
            page: 4,
        };
        assert!(command_supersedes_text(&replacement, 2, true));
        assert!(command_supersedes_text(&replacement, 2, false));
        assert!(!command_supersedes_text(&replacement, 4, true));

        let ensure = WorkerCommand::EnsureTextPages {
            generation: 7,
            pages: vec![2, 4],
        };
        assert!(!command_supersedes_text(&ensure, 2, true));
        assert!(!command_supersedes_text(&ensure, 3, true));

        let cancel = WorkerCommand::CancelExplicitText { generation: 7 };
        assert!(command_supersedes_text(&cancel, 2, true));
        assert!(!command_supersedes_text(&cancel, 2, false));
    }

    #[test]
    fn search_highlight_storage_stops_before_exceeding_the_global_run_cap() {
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

        let mut exact = SearchPageResults {
            page: 0,
            matches: vec![result(0, 2), result(1, 2)],
            truncated: false,
        };
        assert_eq!(cap_search_highlight_runs(&mut exact, 4), 4);
        assert_eq!(exact.matches.len(), 2);
        assert!(!exact.truncated);
    }

    fn empty_worker_state(generation: u64) -> WorkerState {
        WorkerState {
            document: None,
            generation: Some(generation),
            text_cache: HashMap::new(),
            partial_text: HashMap::new(),
            automatic_text_needs_quiet: false,
            page_count: 3,
            search: None,
            latest_search_revision: None,
            search_partial: None,
            scientific: None,
            preview: None,
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
        }
    }

    #[test]
    fn preempted_search_text_is_transferred_to_higher_priority_consumers() {
        let character = || TextChar {
            value: 'P',
            bounds: None,
        };
        let mut state = empty_worker_state(7);
        state.search = Some(search_job(7, 1));

        let explicit = BTreeSet::from([0]);
        preserve_partial_text(
            &mut state,
            0,
            vec![character()],
            &explicit,
            &VecDeque::new(),
        );
        assert_eq!(state.partial_text.remove(&0).unwrap(), vec![character()]);
        assert!(state.search_partial.is_none());

        preserve_partial_text(
            &mut state,
            0,
            vec![character()],
            &BTreeSet::new(),
            &VecDeque::from([0]),
        );
        assert_eq!(state.partial_text.remove(&0).unwrap(), vec![character()]);
        assert!(state.search_partial.is_none());

        preserve_partial_text(
            &mut state,
            0,
            vec![character()],
            &BTreeSet::new(),
            &VecDeque::new(),
        );
        assert_eq!(state.search_partial, Some((0, vec![character()])));
        assert!(state.partial_text.is_empty());
    }

    #[test]
    fn search_partial_survives_interleaved_text_work_for_another_page() {
        let partial = vec![TextChar {
            value: 'P',
            bounds: None,
        }];
        let mut state = empty_worker_state(7);
        state.search_partial = Some((0, partial.clone()));

        assert_eq!(take_search_partial(&mut state, 1), None);
        assert_eq!(state.search_partial, Some((0, partial.clone())));

        assert_eq!(take_search_partial(&mut state, 0), Some(partial));
        assert!(state.search_partial.is_none());
    }

    #[test]
    fn latest_search_revision_replaces_and_rejects_stale_demand() {
        let mut state = empty_worker_state(7);
        assert!(advance_search_revision(&mut state, 7, 10));
        state.search = Some(search_job(7, 10));
        state.search_partial = Some((
            0,
            vec![TextChar {
                value: 'P',
                bounds: None,
            }],
        ));

        assert!(advance_search_revision(&mut state, 7, 11));
        assert!(state.search.is_none());
        assert!(state.search_partial.is_none());
        state.search = Some(search_job(7, 11));

        assert!(!advance_search_revision(&mut state, 7, 10));
        assert_eq!(state.search.as_ref().unwrap().revision, 11);
        assert!(!advance_search_revision(&mut state, 6, 12));
        assert_eq!(state.search.as_ref().unwrap().revision, 11);
    }

    #[test]
    fn cancellation_barrier_and_open_replacement_clear_only_stale_searches() {
        let mut state = empty_worker_state(7);
        state.latest_search_revision = Some(10);
        state.search = Some(search_job(7, 10));
        state.search_partial = Some((0, Vec::new()));

        cancel_searches_before(&mut state, 7, 11);
        assert!(state.search.is_none());
        assert!(state.search_partial.is_none());
        assert_eq!(state.latest_search_revision, Some(10));

        assert!(advance_search_revision(&mut state, 7, 11));
        state.search = Some(search_job(7, 11));
        cancel_searches_before(&mut state, 7, 11);
        assert_eq!(state.search.as_ref().unwrap().revision, 11);

        reset_search_for_open(&mut state);
        assert!(state.search.is_none());
        assert!(state.latest_search_revision.is_none());
        assert!(state.search_partial.is_none());
        assert_eq!(state.page_count, 0);
    }

    #[test]
    fn no_match_page_emits_no_empty_result_event_and_finishes_cleanly() {
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
        assert!(results.matches.is_empty());

        let (events, received) = mpsc::sync_channel(1);
        assert!(send_search_page_results(&events, 7, 4, results));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let mut finished = search_job(7, 4);
        finished.next_page = 3;
        assert!(send_search_finished(&events, &finished));
        assert!(matches!(
            received.recv().unwrap(),
            WorkerEvent::SearchFinished {
                generation: 7,
                revision: 4,
                searched_pages: 3,
                total_results: 0,
                truncated: false,
                ..
            }
        ));
    }

    #[test]
    fn search_without_document_emits_a_terminal_revision_specific_failure() {
        let mut state = empty_worker_state(9);
        let (events, received) = mpsc::sync_channel(1);
        assert!(accept_search_demand(
            &mut state,
            &events,
            9,
            3,
            SearchQuery::new("page").unwrap(),
        ));
        assert!(matches!(
            received.recv().unwrap(),
            WorkerEvent::SearchFailed {
                generation: 9,
                revision: 3,
                ..
            }
        ));
        assert!(state.search.is_none());
        assert_eq!(state.latest_search_revision, Some(3));

        assert!(accept_search_demand(
            &mut state,
            &events,
            9,
            2,
            SearchQuery::new("stale").unwrap(),
        ));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        state.generation = None;
        assert!(accept_search_demand(
            &mut state,
            &events,
            0,
            1,
            SearchQuery::new("before open").unwrap(),
        ));
        assert!(matches!(
            received.recv().unwrap(),
            WorkerEvent::SearchFailed {
                generation: 0,
                revision: 1,
                ..
            }
        ));
    }

    fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * width + x) * 4) as usize;
        bytes[start..start + 4].try_into().unwrap()
    }
}
