use crate::backend::{PdfWorker, TileRequest, WorkerEvent};
use crate::model::{
    DocumentLayout, PageSize, PixelRect, RasterSize, Rect, TextLayer, TextPosition, TextSelection,
    TileKey, append_selected_page_text,
};
use crate::{
    ActualSize, CopySelection, FirstPage, FitWidth, LastPage, OpenDocument, PageDown, PageUp,
    ScrollDown, ScrollLeft, ScrollRight, ScrollUp, SelectAll, ZoomIn, ZoomOut,
};
use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, ContentMask, Context, Corners, CursorStyle, Entity,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, Pixels, Point, Render, RenderImage, ScrollWheelEvent, SharedString, Task,
    Window, canvas, div, point, prelude::*, px, quad, rgb, rgba, size,
};
#[cfg(debug_assertions)]
use gpui::{Modifiers, ScrollDelta, TouchPhase};
use image::{Frame, RgbaImage};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const TOOLBAR_HEIGHT: f32 = 54.0;
const ERROR_BAR_HEIGHT: f32 = 34.0;
const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 5.0;
const RENDER_QUANTUM: u32 = 64;
const TILE_SIZE: u32 = 1_024;
// PDFium may cull rotated glyphs close to a clipped bitmap edge. A 32-pixel
// gutter eliminated the visible/culling differences across real 1024px
// boundaries in 0°, 90°, and CropBox fixtures (only <=1-channel rounding
// differences remain); only the core is displayed.
const TILE_BLEED: u32 = 32;
const MAX_RASTER_DIMENSION: u32 = 65_536;
const MAX_CACHED_TILES: usize = 48;
const MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CACHED_TEXT_PAGES: usize = 16;
const MAX_COPY_TEXT_BYTES: usize = 64 * 1024 * 1024;
const ZOOM_RENDER_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, Default)]
struct Offset {
    x: f32,
    y: f32,
}

#[derive(Debug)]
struct DocumentState {
    path: PathBuf,
    pages: Vec<PageSize>,
}

#[derive(Clone)]
struct CachedTile {
    core_rect: PixelRect,
    render_rect: PixelRect,
    byte_len: usize,
    image: Arc<RenderImage>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DemandTier {
    Visible,
    Prefetch,
}

#[derive(Clone, Copy, Debug)]
struct PlannedTile {
    request: TileRequest,
    tier: DemandTier,
    distance: u64,
}

#[derive(Clone, Copy, Debug)]
struct PanState {
    pointer: Point<Pixels>,
    scroll: Offset,
}

#[derive(Debug)]
struct PendingCopy {
    selection: TextSelection,
    next_page: usize,
    end_page: usize,
    text: String,
}

#[derive(Clone, Debug)]
enum ReaderStatus {
    Initializing,
    Empty,
    Loading(SharedString),
    Ready,
    Error(SharedString),
}

pub struct PdfReader {
    worker: PdfWorker,
    generation: u64,
    document: Option<DocumentState>,
    layout: Option<Arc<DocumentLayout>>,
    rendered: HashMap<TileKey, CachedTile>,
    page_text: HashMap<usize, Arc<TextLayer>>,
    pending: HashSet<TileKey>,
    render_viewport: Vec<(TileKey, DemandTier)>,
    text_viewport: Vec<usize>,
    text_pending: HashSet<usize>,
    copy_pending: Option<PendingCopy>,
    status: ReaderStatus,
    warning: Option<SharedString>,
    focus_handle: FocusHandle,
    zoom: f32,
    fit_width: bool,
    scroll: Offset,
    scroll_target: Offset,
    viewport_width: f32,
    viewport_height: f32,
    selection: Option<TextSelection>,
    selecting: bool,
    pan: Option<PanState>,
    animation_active: bool,
    animation_frame_queued: bool,
    last_animation_tick: Instant,
    zoom_render_revision: u64,
    render_debounce_until: Option<Instant>,
    zoom_render_task: Option<Task<()>>,
}

impl PdfReader {
    pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let (worker, events) = PdfWorker::start();
        let entity = cx.new(|cx| Self {
            worker,
            generation: 0,
            document: None,
            layout: None,
            rendered: HashMap::new(),
            page_text: HashMap::new(),
            pending: HashSet::new(),
            render_viewport: Vec::new(),
            text_viewport: Vec::new(),
            text_pending: HashSet::new(),
            copy_pending: None,
            status: ReaderStatus::Initializing,
            warning: None,
            focus_handle: cx.focus_handle(),
            zoom: 1.0,
            fit_width: false,
            scroll: Offset::default(),
            scroll_target: Offset::default(),
            viewport_width: 1.0,
            viewport_height: 1.0,
            selection: None,
            selecting: false,
            pan: None,
            animation_active: false,
            animation_frame_queued: false,
            last_animation_tick: Instant::now(),
            zoom_render_revision: 0,
            render_debounce_until: None,
            zoom_render_task: None,
        });

        Self::listen_for_worker_events(&entity, events, window, cx);
        Self::listen_for_native_pinch(&entity, window, cx);

        if let Some(path) = initial_path {
            entity.update(cx, |reader, cx| reader.open_path(path, window, cx));
        }
        entity
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }

    #[cfg(debug_assertions)]
    pub fn qa_report(&self) -> String {
        let cached_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .sum::<usize>();
        let visible_exact: Vec<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(*key))
            .collect();
        let exact_cached = visible_exact
            .iter()
            .filter(|key| self.rendered.contains_key(key))
            .count();
        let visible_pages = visible_exact
            .iter()
            .map(|key| key.page)
            .collect::<HashSet<_>>()
            .len();
        let max_tile_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .max()
            .unwrap_or(0);
        format!(
            "GPUI_PDF_READER_QA zoom={:.3} cached_tiles={} cached_bytes={} max_tile_bytes={} cached_text_pages={} text_desired={} pending={} desired={} visible_exact={}/{} visible_pages={} debouncing={} scroll=({:.2},{:.2}) status={:?}",
            self.zoom,
            self.rendered.len(),
            cached_bytes,
            max_tile_bytes,
            self.page_text.len(),
            self.text_viewport.len(),
            self.pending.len(),
            self.render_viewport.len(),
            exact_cached,
            visible_exact.len(),
            visible_pages,
            u8::from(self.render_debounce_until.is_some()),
            self.scroll.x,
            self.scroll.y,
            self.status,
        )
    }

    #[cfg(debug_assertions)]
    pub fn qa_viewport_is_settled(&self) -> bool {
        if !matches!(self.status, ReaderStatus::Ready)
            || self.render_debounce_until.is_some()
            || !self.pending.is_empty()
        {
            return false;
        }
        let mut visible = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(key));
        let Some(first) = visible.next() else {
            return false;
        };
        self.rendered.contains_key(first) && visible.all(|key| self.rendered.contains_key(key))
    }

    #[cfg(debug_assertions)]
    pub fn qa_command_wheel(
        &mut self,
        delta_y: f32,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_scroll_wheel(
            &ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(Point::new(px(0.0), px(delta_y))),
                modifiers: Modifiers {
                    platform: true,
                    ..Default::default()
                },
                touch_phase: TouchPhase::Moved,
            },
            window,
            cx,
        );
    }

    fn listen_for_worker_events(
        entity: &Entity<Self>,
        events: mpsc::Receiver<WorkerEvent>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let events = Arc::new(Mutex::new(events));
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    let events = events.clone();
                    let receive = async_cx
                        .background_executor()
                        .spawn(async move { events.lock().unwrap().recv() });
                    let Ok(event) = receive.await else {
                        break;
                    };
                    let _ = async_cx.update(|window, cx| {
                        weak.update(cx, |reader, cx| {
                            reader.handle_worker_event(event, window, cx)
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn listen_for_native_pinch(entity: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let receiver = Arc::new(Mutex::new(crate::native_gestures::install_pinch_monitor()));
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    let receiver = receiver.clone();
                    let receive = async_cx
                        .background_executor()
                        .spawn(async move { receiver.lock().unwrap().recv() });
                    let Ok(pinch) = receive.await else {
                        break;
                    };
                    let _ = async_cx.update(|window, cx| {
                        let window_height = f32::from(window.viewport_size().height);
                        weak.update(cx, |reader, cx| {
                            let position = point(px(pinch.x), px(window_height - pinch.cocoa_y));
                            reader.zoom_at(reader.zoom * (1.0 + pinch.delta), position, window, cx);
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn handle_worker_event(
        &mut self,
        event: WorkerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkerEvent::Ready => {
                if self.document.is_none() && !matches!(self.status, ReaderStatus::Loading(_)) {
                    self.status = ReaderStatus::Empty;
                }
            }
            WorkerEvent::Opened {
                generation,
                path,
                pages,
            } if generation == self.generation => {
                self.drop_all_images(window, cx);
                self.document = Some(DocumentState {
                    path: path.clone(),
                    pages,
                });
                self.page_text.clear();
                self.pending.clear();
                self.render_viewport.clear();
                self.text_viewport.clear();
                self.render_debounce_until = None;
                self.zoom_render_task = None;
                self.text_pending.clear();
                self.copy_pending = None;
                self.selection = None;
                self.scroll = Offset::default();
                self.scroll_target = Offset::default();
                self.status = ReaderStatus::Ready;
                self.fit_width = true;
                self.apply_fit_width();
                self.rebuild_layout();
                let title = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PDF");
                window.set_window_title(&format!("{title} — GPUI PDF Reader"));
                self.request_visible_tiles(window);
            }
            WorkerEvent::TileRendered {
                generation,
                key,
                core_rect,
                render_rect,
                width,
                height,
                bgra,
            } if generation == self.generation => {
                let page = key.page;
                self.pending.remove(&key);

                let expected_len = width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .and_then(|bytes| usize::try_from(bytes).ok());
                if expected_len != Some(bgra.len())
                    || width != render_rect.width
                    || height != render_rect.height
                {
                    self.status = ReaderStatus::Error(
                        format!("PDFium returned an invalid tile for page {}", page + 1).into(),
                    );
                    cx.notify();
                    return;
                }

                // GPUI documents RenderImage's frame data as BGRA. The image
                // crate buffer is only the owned four-byte carrier.
                if let Some(buffer) = RgbaImage::from_raw(width, height, bgra) {
                    let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
                    if let Some(previous) = self.rendered.insert(
                        key,
                        CachedTile {
                            core_rect,
                            render_rect,
                            byte_len: expected_len.unwrap(),
                            image,
                        },
                    ) {
                        Self::retire_images(vec![previous.image], window, cx);
                    }
                }
                self.evict_distant_tiles(window, cx);
                self.request_visible_tiles(window);
            }
            WorkerEvent::TileFailed {
                generation,
                key,
                message,
            } if generation == self.generation => {
                self.pending.remove(&key);
                self.warning = Some(message.into());
            }
            WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            } if generation == self.generation => {
                self.page_text.entry(page).or_insert(text);
                self.text_pending.remove(&page);
                self.continue_pending_copy(cx);
                self.evict_distant_text();
            }
            WorkerEvent::TextFailed {
                generation,
                page,
                message,
            } if generation == self.generation => {
                // Complete selection/copy bookkeeping with an empty layer;
                // text failure on one page must not stall rendering or an
                // operation spanning the rest of the document.
                self.page_text
                    .entry(page)
                    .or_insert_with(|| Arc::new(TextLayer::empty()));
                self.text_pending.remove(&page);
                self.warning = Some(message.into());
                self.continue_pending_copy(cx);
                self.evict_distant_text();
            }
            WorkerEvent::Error {
                generation,
                message,
            } if generation.is_none() || generation == Some(self.generation) => {
                self.pending.clear();
                self.render_viewport.clear();
                self.text_viewport.clear();
                self.render_debounce_until = None;
                self.zoom_render_task = None;
                self.text_pending.clear();
                self.copy_pending = None;
                self.status = ReaderStatus::Error(message.into());
            }
            _ => {}
        }
        cx.notify();
    }

    fn open_dialog(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open PDF".into()),
        });
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = prompt.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let _ = cx.update(|window, cx| {
                        weak.update(cx, |reader, cx| reader.open_path(path, window, cx))
                            .ok();
                    });
                }
            })
            .detach();
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.drop_all_images(window, cx);
        self.document = None;
        self.layout = None;
        self.page_text.clear();
        self.pending.clear();
        self.render_viewport.clear();
        self.text_viewport.clear();
        self.zoom_render_revision = self.zoom_render_revision.wrapping_add(1);
        self.render_debounce_until = None;
        self.zoom_render_task = None;
        self.text_pending.clear();
        self.copy_pending = None;
        self.warning = None;
        self.selection = None;
        self.scroll = Offset::default();
        self.scroll_target = Offset::default();
        self.animation_active = false;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
            .to_owned();
        self.status = ReaderStatus::Loading(format!("Opening {name}…").into());
        if !self.worker.open(self.generation, path) {
            self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
        }
        cx.notify();
    }

    fn layout(&self) -> Option<&DocumentLayout> {
        self.layout.as_deref()
    }

    fn rebuild_layout(&mut self) {
        let Some(document) = self.document.as_ref() else {
            self.layout = None;
            return;
        };
        let layout = if let Some(layout) = self.layout.as_deref() {
            // `open_path()` clears the layout before replacing the document,
            // so an existing layout always owns this document's geometry.
            debug_assert_eq!(layout.page_count(), document.pages.len());
            layout.rescaled(self.zoom, self.viewport_width)
        } else {
            DocumentLayout::new(&document.pages, self.zoom, self.viewport_width)
        };
        self.layout = Some(Arc::new(layout));
    }

    fn update_viewport(&mut self, window: &Window) {
        let size = window.viewport_size();
        let previous_width = self.viewport_width;
        let previous_zoom = self.zoom;
        self.viewport_width = f32::from(size.width).max(1.0);
        let error_height = self.content_top() - TOOLBAR_HEIGHT;
        self.viewport_height = (f32::from(size.height) - TOOLBAR_HEIGHT - error_height).max(1.0);
        if self.fit_width {
            self.apply_fit_width();
        }
        if (self.viewport_width - previous_width).abs() > 0.01
            || (self.zoom - previous_zoom).abs() > 0.0001
        {
            self.rebuild_layout();
        }
        self.clamp_scroll();
    }

    fn content_top(&self) -> f32 {
        TOOLBAR_HEIGHT
            + if matches!(self.status, ReaderStatus::Error(_)) {
                ERROR_BAR_HEIGHT
            } else {
                0.0
            }
    }

    fn apply_fit_width(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let widest = document
            .pages
            .iter()
            .map(|page| page.width)
            .fold(1.0_f32, f32::max);
        let available = (self.viewport_width - crate::model::PAGE_MARGIN * 2.0).max(100.0);
        self.zoom = (available / (widest * crate::model::PDF_POINTS_TO_LOGICAL_PIXELS))
            .clamp(MIN_ZOOM, MAX_ZOOM);
    }

    fn request_visible_tiles(&mut self, window: &Window) {
        // While a replacement document is opening, `self.document` still describes
        // the previous PDF. Never pair that stale layout with the new generation.
        if !matches!(self.status, ReaderStatus::Ready) {
            return;
        }
        if self
            .render_debounce_until
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        self.render_debounce_until = None;
        let (Some(layout), Some(document)) = (self.layout(), self.document.as_ref()) else {
            return;
        };
        let mut planned = plan_visible_tiles(
            layout,
            &document.pages,
            self.scroll,
            self.viewport_width,
            self.viewport_height,
            window.scale_factor(),
        );
        planned.sort_by_key(|tile| (tile.tier, tile.distance, tile.request.key));
        let mut viewport: Vec<_> = planned
            .iter()
            .map(|tile| (tile.request.key, tile.tier))
            .collect();
        viewport.sort_by_key(|(key, tier)| (*tier, *key));
        let mut seen_text_pages = HashSet::new();
        let text_viewport: Vec<_> = planned
            .iter()
            .filter(|tile| tile.tier == DemandTier::Visible)
            .filter_map(|tile| {
                seen_text_pages
                    .insert(tile.request.key.page)
                    .then_some(tile.request.key.page)
            })
            .take(MAX_CACHED_TEXT_PAGES)
            .collect();

        // Completion does not change these full desired signatures. That
        // matters: re-sending shrinking lists after every result can race the
        // worker and reinsert work it has just finished.
        if viewport != self.render_viewport || text_viewport != self.text_viewport {
            let demand: Vec<_> = planned
                .iter()
                .filter_map(|tile| {
                    (!self.rendered.contains_key(&tile.request.key)).then_some(*tile)
                })
                .collect();
            let visible_tile_count = demand
                .iter()
                .take_while(|tile| tile.tier == DemandTier::Visible)
                .count();
            let tile_requests: Vec<_> = demand.iter().map(|tile| tile.request).collect();
            let text_pages: Vec<_> = text_viewport
                .iter()
                .copied()
                .filter(|page| !self.page_text.contains_key(page))
                .collect();
            if self.worker.render_viewport(
                self.generation,
                &tile_requests,
                visible_tile_count,
                &text_pages,
            ) {
                self.pending.clear();
                self.pending
                    .extend(tile_requests.iter().map(|tile| tile.key));
                self.render_viewport = viewport;
                self.text_viewport = text_viewport;
                self.warning = None;
            } else {
                self.pending.clear();
                self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
            }
        }
    }

    fn retire_images(images: Vec<Arc<RenderImage>>, window: &mut Window, cx: &mut Context<Self>) {
        if images.is_empty() {
            return;
        }
        // GPUI's Blade renderer submits Metal command buffers with unretained
        // texture references. Removing an atlas image immediately can destroy a
        // texture that the preceding frame is still sampling. Cross two frame
        // callbacks first: the intervening draw waits for the preceding GPU
        // submission, and the following callback can then retire the texture.
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            window.on_next_frame(move |window, _| {
                for image in images {
                    let _ = window.drop_image(image);
                }
            });
            weak.update(cx, |_, cx| cx.notify()).ok();
        });
        cx.notify();
    }

    fn drop_all_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let images = self.rendered.drain().map(|(_, tile)| tile.image).collect();
        Self::retire_images(images, window, cx);
    }

    fn evict_distant_tiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut desired_by_page = HashMap::<usize, (RasterSize, Vec<TileKey>)>::new();
        for (key, tier) in &self.render_viewport {
            let entry = desired_by_page
                .entry(key.page)
                .or_insert((key.raster, Vec::new()));
            if *tier == DemandTier::Visible {
                entry.1.push(*key);
            }
        }
        let exact_pages: HashSet<_> = desired_by_page
            .iter()
            .filter_map(|(page, (_, visible))| {
                (!visible.is_empty() && visible.iter().all(|key| self.rendered.contains_key(key)))
                    .then_some(*page)
            })
            .collect();
        let obsolete: Vec<_> = self
            .rendered
            .keys()
            .copied()
            .filter(|key| {
                exact_pages.contains(&key.page)
                    && desired_by_page
                        .get(&key.page)
                        .is_some_and(|(raster, _)| *raster != key.raster)
            })
            .collect();
        let mut retired = Vec::new();
        for key in obsolete {
            if let Some(tile) = self.rendered.remove(&key) {
                retired.push(tile.image);
            }
        }

        let mut cached_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .sum::<usize>();
        if self.rendered.len() <= MAX_CACHED_TILES && cached_bytes <= MAX_CACHE_BYTES {
            Self::retire_images(retired, window, cx);
            return;
        }
        let protected: HashSet<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(*key))
            .collect();
        let mut candidates: Vec<_> = self
            .rendered
            .keys()
            .copied()
            .filter(|key| !protected.contains(key))
            .collect();
        candidates.sort_by_key(|key| {
            let desired = self
                .render_viewport
                .iter()
                .find_map(|(desired, _)| (desired.page == key.page).then_some(desired.raster));
            let stale_scale = desired.is_some_and(|raster| raster != key.raster);
            (
                !stale_scale,
                std::cmp::Reverse(tile_distance_from_viewport(
                    *key,
                    self.layout(),
                    self.scroll,
                    self.viewport_width,
                    self.viewport_height,
                )),
            )
        });
        for key in candidates {
            if self.rendered.len() <= MAX_CACHED_TILES && cached_bytes <= MAX_CACHE_BYTES {
                break;
            }
            if let Some(cache) = self.rendered.remove(&key) {
                cached_bytes = cached_bytes.saturating_sub(cache.byte_len);
                retired.push(cache.image);
            }
        }
        Self::retire_images(retired, window, cx);
    }

    fn evict_distant_text(&mut self) {
        if self.page_text.len() <= MAX_CACHED_TEXT_PAGES {
            return;
        }
        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height))
            .unwrap_or(0);
        let mut candidates: Vec<_> = self.page_text.keys().copied().collect();
        candidates.retain(|page| !self.text_viewport.contains(page));
        candidates.sort_by_key(|page| std::cmp::Reverse(page.abs_diff(current_page)));
        for page in candidates {
            if self.page_text.len() <= MAX_CACHED_TEXT_PAGES {
                break;
            }
            self.page_text.remove(&page);
        }
    }

    fn clamp_scroll(&mut self) {
        let Some(layout) = self.layout() else {
            self.scroll = Offset::default();
            self.scroll_target = Offset::default();
            return;
        };
        let max_x = (layout.content_width - self.viewport_width).max(0.0);
        let max_y = (layout.content_height - self.viewport_height).max(0.0);
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
        self.scroll_target.x = self.scroll_target.x.clamp(0.0, max_x);
        self.scroll_target.y = self.scroll_target.y.clamp(0.0, max_y);
    }

    fn scroll_by(
        &mut self,
        x: f32,
        y: f32,
        immediate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if immediate {
            self.animation_active = false;
            self.scroll_target = self.scroll;
        }
        self.scroll_target.x += x;
        self.scroll_target.y += y;
        self.clamp_scroll();
        if immediate {
            self.scroll = self.scroll_target;
            self.request_visible_tiles(window);
            cx.notify();
        } else {
            self.start_animation(window, cx);
        }
    }

    fn start_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.animation_active {
            return;
        }
        self.animation_active = true;
        self.last_animation_tick = Instant::now();
        self.queue_animation_frame(window, cx);
    }

    fn queue_animation_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.animation_frame_queued {
            return;
        }
        self.animation_frame_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            weak.update(cx, |reader, cx| reader.animation_tick(window, cx))
                .ok();
        });
        // `request_animation_frame()` requires an active GPUI paint context and
        // panics from input handlers. Notifying the entity schedules the frame;
        // the callback above then chains the animation tick after it renders.
        cx.notify();
    }

    fn animation_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.animation_frame_queued = false;
        if !self.animation_active {
            return;
        }
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_animation_tick)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 0.05);
        self.last_animation_tick = now;
        let blend = 1.0 - (-18.0 * dt).exp();
        self.scroll.x += (self.scroll_target.x - self.scroll.x) * blend;
        self.scroll.y += (self.scroll_target.y - self.scroll.y) * blend;
        let distance = (self.scroll_target.x - self.scroll.x).abs()
            + (self.scroll_target.y - self.scroll.y).abs();
        if distance < 0.35 {
            self.scroll = self.scroll_target;
            self.animation_active = false;
        }
        self.request_visible_tiles(window);
        cx.notify();
        if self.animation_active {
            self.queue_animation_frame(window, cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(44.0));
        let dx = f32::from(delta.x);
        let dy = f32::from(delta.y);
        if !dx.is_finite() || !dy.is_finite() {
            cx.stop_propagation();
            return;
        }
        if event.modifiers.platform || event.modifiers.control {
            // A single accelerated wheel packet must not overflow the zoom
            // calculation or jump through the entire range in one event.
            let factor = command_wheel_zoom_factor(dy).expect("finite delta checked above");
            self.zoom_at(self.zoom * factor, event.position, window, cx);
        } else {
            let (x, y) = if event.modifiers.shift && dx.abs() < f32::EPSILON {
                (-dy, 0.0)
            } else {
                (-dx, -dy)
            };
            self.scroll_by(x, y, event.delta.precise(), window, cx);
        }
        cx.stop_propagation();
    }

    fn zoom_at(
        &mut self,
        zoom: f32,
        window_position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw_x = f32::from(window_position.x);
        let raw_y = f32::from(window_position.y);
        if self.document.is_none() || !zoom.is_finite() || !raw_x.is_finite() || !raw_y.is_finite()
        {
            return;
        }
        let new_zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < 0.0001 {
            return;
        }
        let local_x = raw_x.clamp(0.0, self.viewport_width);
        let local_y = (raw_y - self.content_top()).clamp(0.0, self.viewport_height);
        let anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(self.scroll.x + local_x, self.scroll.y + local_y)
        });

        self.zoom = new_zoom;
        self.fit_width = false;
        self.rebuild_layout();
        if let Some(anchor) = anchor
            && let Some((x, y)) = self
                .layout()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.scroll = Offset {
                x: x - local_x,
                y: y - local_y,
            };
            self.scroll_target = self.scroll;
        }
        self.clamp_scroll();
        self.defer_render_after_zoom(window, cx);
        cx.notify();
    }

    fn defer_render_after_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.render_debounce_until.is_none() {
            // Debouncing new work is not enough: without this replacement the
            // worker continues the old multi-tile queue throughout the zoom.
            // Cancel it once at the beginning of a burst; at most the one tile
            // already inside PDFium can still complete.
            self.render_viewport.clear();
            self.text_viewport.clear();
            self.pending.clear();
            if !self.worker.render_viewport(self.generation, &[], 0, &[]) {
                self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
                return;
            }
        }
        self.zoom_render_revision = self.zoom_render_revision.wrapping_add(1);
        let revision = self.zoom_render_revision;
        self.render_debounce_until = Some(Instant::now() + ZOOM_RENDER_DEBOUNCE);
        let weak = cx.weak_entity();
        self.zoom_render_task = Some(window.spawn(cx, async move |cx| {
            cx.background_executor().timer(ZOOM_RENDER_DEBOUNCE).await;
            let _ = cx.update(|window, cx| {
                weak.update(cx, |reader, cx| {
                    if reader.zoom_render_revision == revision {
                        reader.render_debounce_until = None;
                        reader.request_visible_tiles(window);
                        cx.notify();
                    }
                })
                .ok();
            });
        }));
    }

    fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(self.zoom * 1.15, self.viewport_center(), window, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(self.zoom / 1.15, self.viewport_center(), window, cx);
    }

    fn actual_size(&mut self, _: &ActualSize, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(1.0, self.viewport_center(), window, cx);
    }

    fn fit_width(&mut self, _: &FitWidth, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.zoom;
        self.fit_width = true;
        self.apply_fit_width();
        let target = self.zoom;
        self.zoom = before;
        self.zoom_at(target, self.viewport_center(), window, cx);
        self.fit_width = true;
    }

    fn viewport_center(&self) -> Point<Pixels> {
        point(
            px(self.viewport_width * 0.5),
            px(self.content_top() + self.viewport_height * 0.5),
        )
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, -64.0, false, window, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, 64.0, false, window, cx);
    }

    fn scroll_left(&mut self, _: &ScrollLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-64.0, 0.0, false, window, cx);
    }

    fn scroll_right(&mut self, _: &ScrollRight, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(64.0, 0.0, false, window, cx);
    }

    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, -self.viewport_height * 0.9, false, window, cx);
    }

    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, self.viewport_height * 0.9, false, window, cx);
    }

    fn first_page(&mut self, _: &FirstPage, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_target.y = 0.0;
        self.start_animation(window, cx);
    }

    fn last_page(&mut self, _: &LastPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(layout) = self.layout() {
            self.scroll_target.y = (layout.content_height - self.viewport_height).max(0.0);
            self.start_animation(window, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        match event.button {
            MouseButton::Left => {
                if let Some(position) = self.hit_test_text(event.position, false) {
                    if event.click_count >= 2 {
                        self.select_word(position);
                    } else if event.modifiers.shift {
                        if let Some(selection) = self.selection.as_mut() {
                            selection.focus = position;
                        } else {
                            self.selection = Some(TextSelection {
                                anchor: position,
                                focus: position,
                            });
                        }
                    } else {
                        self.selection = Some(TextSelection {
                            anchor: position,
                            focus: position,
                        });
                    }
                    self.selecting = true;
                } else if !event.modifiers.shift {
                    self.selection = None;
                }
            }
            MouseButton::Middle => {
                self.pan = Some(PanState {
                    pointer: event.position,
                    scroll: self.scroll,
                });
                self.animation_active = false;
            }
            _ => {}
        }
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pan) = self.pan {
            let delta = event.position - pan.pointer;
            self.scroll = Offset {
                x: pan.scroll.x - f32::from(delta.x),
                y: pan.scroll.y - f32::from(delta.y),
            };
            self.scroll_target = self.scroll;
            self.clamp_scroll();
            self.request_visible_tiles(window);
            cx.notify();
            return;
        }

        if self.selecting
            && event.pressed_button == Some(MouseButton::Left)
            && let Some(position) = self.hit_test_text(event.position, true)
        {
            if let Some(selection) = self.selection.as_mut() {
                selection.focus = position;
            }
            let local_y = f32::from(event.position.y) - self.content_top();
            if local_y < 28.0 {
                self.scroll_by(0.0, -28.0, true, window, cx);
            } else if local_y > self.viewport_height - 28.0 {
                self.scroll_by(0.0, 28.0, true, window, cx);
            }
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        self.pan = None;
        cx.notify();
    }

    fn hit_test_text(&self, position: Point<Pixels>, nearest: bool) -> Option<TextPosition> {
        let layout = self.layout()?;
        let x = self.scroll.x + f32::from(position.x);
        let y = self.scroll.y + f32::from(position.y) - self.content_top();
        let page = layout.page_at_content_point(x, y).or_else(|| {
            nearest.then(|| {
                layout
                    .visible_pages(self.scroll.y, self.viewport_height, self.viewport_height)
                    .min_by_key(|page| {
                        let rect = layout.page_rect(*page).unwrap();
                        if y < rect.y {
                            (rect.y - y) as i32
                        } else if y > rect.bottom() {
                            (y - rect.bottom()) as i32
                        } else {
                            0
                        }
                    })
            })?
        })?;
        let chars = self.page_text.get(&page)?;
        let page_rect = layout.page_rect(page)?;
        chars
            .hit_test(page_rect, x, y, nearest)
            .map(|index| TextPosition { page, index })
    }

    fn select_word(&mut self, position: TextPosition) {
        let Some(chars) = self.page_text.get(&position.page) else {
            return;
        };
        let Some(clicked) = chars.get(position.index) else {
            return;
        };
        let is_word = |value: char| value.is_alphanumeric() || value == '_' || value == '-';
        let category = is_word(clicked.value);
        let mut start = position.index;
        let mut end = position.index;
        while start > 0 && is_word(chars[start - 1].value) == category {
            start -= 1;
        }
        while end + 1 < chars.len() && is_word(chars[end + 1].value) == category {
            end += 1;
        }
        self.selection = Some(TextSelection {
            anchor: TextPosition {
                page: position.page,
                index: start,
            },
            focus: TextPosition {
                page: position.page,
                index: end,
            },
        });
    }

    fn copy_selection(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection else {
            return;
        };
        let (start, end) = selection.ordered();
        let Some(last_document_page) = self
            .document
            .as_ref()
            .and_then(|document| document.pages.len().checked_sub(1))
        else {
            return;
        };
        self.warning = None;
        // The worker treats explicit extraction as latest-wins. Mirror that
        // replacement locally so an abandoned page cannot remain marked
        // pending and suppress a future request for the same page.
        self.text_pending.clear();
        let _ = self.worker.cancel_explicit_text(self.generation);
        self.copy_pending = Some(PendingCopy {
            selection,
            next_page: start.page.min(last_document_page),
            end_page: end.page.min(last_document_page),
            text: String::new(),
        });
        self.continue_pending_copy(cx);
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(page_count) = self
            .document
            .as_ref()
            .map(|document| document.pages.len())
            .filter(|count| *count > 0)
        else {
            return;
        };
        // Character indices on the end page are clamped against its actual
        // layer when painted or copied, so Select All is O(1) even for a very
        // long document and never queues every page at once.
        self.selection = Some(TextSelection {
            anchor: TextPosition { page: 0, index: 0 },
            focus: TextPosition {
                page: page_count - 1,
                index: usize::MAX,
            },
        });
        cx.notify();
    }

    fn continue_pending_copy(&mut self, cx: &mut Context<Self>) {
        loop {
            let Some(mut pending) = self.copy_pending.take() else {
                return;
            };
            if pending.next_page > pending.end_page {
                if !pending.text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(pending.text));
                }
                return;
            }

            let page = pending.next_page;
            let Some(text) = self.page_text.get(&page).cloned() else {
                self.copy_pending = Some(pending);
                if self.text_pending.insert(page)
                    && !self.worker.extract_text(self.generation, page)
                {
                    self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
                    self.text_pending.remove(&page);
                    self.copy_pending = None;
                }
                return;
            };

            append_selected_page_text(&mut pending.text, pending.selection, page, text.as_slice());
            if pending.text.len() > MAX_COPY_TEXT_BYTES {
                self.warning = Some(
                    "Copy stopped because the selected text exceeds the 64 MiB safety limit".into(),
                );
                return;
            }
            pending.next_page += 1;
            self.copy_pending = Some(pending);
        }
    }

    fn paint_tiles_for_page(
        &self,
        page: usize,
        page_rect: Rect,
        desired: RasterSize,
    ) -> Vec<PaintTile> {
        let visible_exact: Vec<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| {
                (key.page == page && *tier == DemandTier::Visible).then_some(*key)
            })
            .collect();
        let exact_complete = !visible_exact.is_empty()
            && visible_exact
                .iter()
                .all(|key| self.rendered.contains_key(key));

        let mut rasters: Vec<_> = self
            .rendered
            .keys()
            .filter_map(|key| (key.page == page && key.raster != desired).then_some(key.raster))
            .collect();
        rasters.sort();
        rasters.dedup();
        let viewport = Rect {
            x: self.scroll.x,
            y: self.scroll.y,
            width: self.viewport_width,
            height: self.viewport_height,
        };
        let fallback = (!exact_complete)
            .then(|| {
                rasters.into_iter().max_by(|a, b| {
                    let coverage = |raster: RasterSize| {
                        self.rendered
                            .iter()
                            .filter(|(key, _)| key.page == page && key.raster == raster)
                            .filter_map(|(_, tile)| {
                                let tile = tile_logical_rect(page_rect, raster, tile.core_rect);
                                intersect_rect(tile, viewport)
                            })
                            .map(|visible| f64::from(visible.width) * f64::from(visible.height))
                            .sum::<f64>()
                    };
                    coverage(*a).total_cmp(&coverage(*b)).then_with(|| {
                        let distance = |raster: RasterSize| {
                            u64::from(raster.width.abs_diff(desired.width))
                                + u64::from(raster.height.abs_diff(desired.height))
                        };
                        distance(*b).cmp(&distance(*a))
                    })
                })
            })
            .flatten();

        let mut layers = Vec::with_capacity(2);
        if let Some(fallback) = fallback {
            layers.push(fallback);
        }
        layers.push(desired);

        let mut result = Vec::new();
        for raster in layers {
            let mut tiles: Vec<_> = self
                .rendered
                .iter()
                .filter(|(key, _)| key.page == page && key.raster == raster)
                .collect();
            tiles.sort_by_key(|(key, _)| **key);
            result.extend(tiles.into_iter().map(|(_, tile)| PaintTile {
                core_rect: tile_logical_rect(page_rect, raster, tile.core_rect),
                render_rect: tile_logical_rect(page_rect, raster, tile.render_rect),
                image: tile.image.clone(),
            }));
        }
        result
    }

    fn status_text(&self) -> SharedString {
        match &self.status {
            ReaderStatus::Initializing => "Starting PDFium…".into(),
            ReaderStatus::Empty => "Open a PDF to begin".into(),
            ReaderStatus::Loading(message) => message.clone(),
            ReaderStatus::Error(message) => message.clone(),
            ReaderStatus::Ready if self.copy_pending.is_some() => "Reading text for copy…".into(),
            ReaderStatus::Ready if self.warning.is_some() => self.warning.clone().unwrap(),
            ReaderStatus::Ready if !self.pending.is_empty() => format!(
                "Rendering {} tile{}…",
                self.pending.len(),
                if self.pending.len() == 1 { "" } else { "s" }
            )
            .into(),
            ReaderStatus::Ready => "Ready".into(),
        }
    }

    fn toolbar_button(
        id: &'static str,
        label: impl Into<SharedString>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(30.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x3b4658))
            .bg(rgb(0x252d3a))
            .text_color(rgb(0xe7eaf0))
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x354155)))
            .active(|style| style.bg(rgb(0x18202b)))
            .on_click(handler)
            .child(label.into())
    }

    fn paint_document(snapshot: PaintSnapshot, bounds: Bounds<Pixels>, window: &mut Window) {
        let content_viewport = Rect {
            x: snapshot.scroll.x,
            y: snapshot.scroll.y,
            width: snapshot.viewport_width,
            height: snapshot.viewport_height,
        };
        for page in snapshot.pages {
            let rect = page.rect;
            let page_bounds = Bounds::new(
                point(
                    bounds.left() + px(rect.x - snapshot.scroll.x),
                    bounds.top() + px(rect.y - snapshot.scroll.y),
                ),
                size(px(rect.width), px(rect.height)),
            );
            let shadow_bounds = Bounds::new(
                page_bounds.origin + point(px(0.0), px(3.0)),
                page_bounds.size + size(px(0.0), px(5.0)),
            );
            window.paint_quad(quad(
                shadow_bounds,
                px(3.0),
                rgba(0x0b101833),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
            window.paint_quad(quad(
                page_bounds,
                px(2.0),
                gpui::white(),
                px(1.0),
                rgb(0xc8ccd3),
                Default::default(),
            ));

            for tile in page.tiles {
                let render_bounds =
                    content_rect_to_bounds(bounds, tile.render_rect, snapshot.scroll);
                let core_bounds = content_rect_to_bounds(bounds, tile.core_rect, snapshot.scroll);
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: core_bounds,
                    }),
                    |window| {
                        let _ = window.paint_image(
                            render_bounds,
                            Corners::default(),
                            tile.image,
                            0,
                            false,
                        );
                    },
                );
            }

            if let (Some(selection), Some(chars)) = (snapshot.selection, page.text)
                && let Some(range) = selection.indices_on_page(page.index, chars.len())
            {
                // Dense text pages are indexed once on the worker. Paint only
                // selected glyphs that intersect this frame's viewport, and
                // clip malformed PDF geometry to the physical page.
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: page_bounds,
                    }),
                    |window| {
                        chars.for_each_visible_in_range(
                            rect,
                            content_viewport,
                            range,
                            |_, highlight| {
                                let highlight_bounds =
                                    content_rect_to_bounds(bounds, highlight, snapshot.scroll);
                                window.paint_quad(quad(
                                    highlight_bounds,
                                    px(1.0),
                                    rgba(0x2575e650),
                                    px(0.0),
                                    gpui::transparent_black(),
                                    Default::default(),
                                ));
                            },
                        );
                    },
                );
            }
        }

        let max_y = (snapshot.layout.content_height - snapshot.viewport_height).max(0.0);
        if max_y > 0.0 {
            let thumb_height = (snapshot.viewport_height * snapshot.viewport_height
                / snapshot.layout.content_height)
                .max(38.0)
                .min(snapshot.viewport_height);
            let travel = snapshot.viewport_height - thumb_height - 8.0;
            let y = 4.0 + travel * (snapshot.scroll.y / max_y);
            let thumb = Bounds::new(
                point(bounds.right() - px(8.0), bounds.top() + px(y)),
                size(px(5.0), px(thumb_height)),
            );
            window.paint_quad(quad(
                thumb,
                px(2.5),
                rgba(0x4c586e99),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }

        let max_x = (snapshot.layout.content_width - snapshot.viewport_width).max(0.0);
        if max_x > 0.0 {
            let thumb_width = (snapshot.viewport_width * snapshot.viewport_width
                / snapshot.layout.content_width)
                .max(38.0)
                .min(snapshot.viewport_width);
            let travel = snapshot.viewport_width - thumb_width - 12.0;
            let x = 4.0 + travel * (snapshot.scroll.x / max_x);
            let thumb = Bounds::new(
                point(bounds.left() + px(x), bounds.bottom() - px(8.0)),
                size(px(thumb_width), px(5.0)),
            );
            window.paint_quad(quad(
                thumb,
                px(2.5),
                rgba(0x4c586e99),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }
    }
}

impl Focusable for PdfReader {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PdfReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_viewport(window);
        self.request_visible_tiles(window);

        let page_count = self
            .document
            .as_ref()
            .map(|document| document.pages.len())
            .unwrap_or(0);
        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height) + 1)
            .unwrap_or(0);
        let filename: SharedString = self
            .document
            .as_ref()
            .and_then(|document| document.path.file_name())
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| "GPUI PDF Reader".into());
        let zoom_label: SharedString = format!("{}%", (self.zoom * 100.0).round() as u32).into();
        let status_text = self.status_text();

        let toolbar = div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .bg(rgb(0x171d27))
            .border_b_1()
            .border_color(rgb(0x303949))
            .text_color(rgb(0xd9dde5))
            .child(Self::toolbar_button(
                "open-document",
                "Open",
                cx.listener(|reader, _, window, cx| reader.open_dialog(&OpenDocument, window, cx)),
            ))
            .child(div().w(px(1.0)).h(px(24.0)).bg(rgb(0x394354)))
            .child(Self::toolbar_button(
                "zoom-out",
                "−",
                cx.listener(|reader, _, window, cx| reader.zoom_out(&ZoomOut, window, cx)),
            ))
            .child(div().w(px(58.0)).text_center().text_sm().child(zoom_label))
            .child(Self::toolbar_button(
                "zoom-in",
                "+",
                cx.listener(|reader, _, window, cx| reader.zoom_in(&ZoomIn, window, cx)),
            ))
            .child(Self::toolbar_button(
                "fit-width",
                "Fit",
                cx.listener(|reader, _, window, cx| reader.fit_width(&FitWidth, window, cx)),
            ))
            .child(
                div()
                    .ml_2()
                    .px_2()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .bg(rgb(0x10151d))
                    .text_sm()
                    .child(if page_count == 0 {
                        "No document".to_owned()
                    } else {
                        format!("Page {current_page} / {page_count}")
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .px_3()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_sm()
                    .text_color(rgb(0xaeb6c5))
                    .child(filename),
            )
            .child(div().text_xs().text_color(rgb(0x8f9aab)).child(status_text));

        let content = if let Some(layout) = self.layout() {
            let visible = layout.visible_pages(
                self.scroll.y,
                self.viewport_height,
                self.viewport_height * 0.2,
            );
            let pages = visible
                .filter_map(|index| {
                    let rect = layout.page_rect(index)?;
                    let desired = desired_raster_size(rect, window.scale_factor());
                    Some(PaintPage {
                        index,
                        rect,
                        tiles: self.paint_tiles_for_page(index, rect, desired),
                        text: self.page_text.get(&index).cloned(),
                    })
                })
                .collect();
            let snapshot = PaintSnapshot {
                layout: self.layout.as_ref().unwrap().clone(),
                pages,
                scroll: self.scroll,
                selection: self.selection,
                viewport_width: self.viewport_width,
                viewport_height: self.viewport_height,
            };
            div()
                .id("document-viewport")
                .flex_1()
                .w_full()
                .overflow_hidden()
                .bg(rgb(0x454d59))
                .cursor(if self.pan.is_some() {
                    CursorStyle::ClosedHand
                } else {
                    CursorStyle::IBeam
                })
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| Self::paint_document(snapshot, bounds, window),
                    )
                    .size_full(),
                )
                .into_any_element()
        } else {
            div()
                .id("empty-state")
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .bg(rgb(0x343c48))
                .text_color(rgb(0xe7eaf0))
                .child(div().text_3xl().child("GPUI PDF Reader"))
                .child(
                    div()
                        .max_w(px(520.0))
                        .text_center()
                        .text_color(rgb(0xaeb7c5))
                        .child("Fast native PDF reading with smooth two-axis scrolling, zoom, and selectable text."),
                )
                .child(Self::toolbar_button(
                    "empty-open-document",
                    "Open a PDF",
                    cx.listener(|reader, _, window, cx| {
                        reader.open_dialog(&OpenDocument, window, cx)
                    }),
                ))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(rgb(0x8894a6))
                        .child("Trackpad or wheel to scroll • Pinch or ⌘-wheel to zoom • Middle-drag to pan"),
                )
                .into_any_element()
        };

        let error_bar = if let ReaderStatus::Error(message) = &self.status {
            Some(
                div()
                    .h(px(ERROR_BAR_HEIGHT))
                    .flex_none()
                    .w_full()
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(rgb(0x6f2730))
                    .text_color(rgb(0xffe8eb))
                    .text_sm()
                    .child(message.clone()),
            )
        } else {
            None
        };

        div()
            .key_context("PdfReader")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x343c48))
            .on_action(cx.listener(Self::open_dialog))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::actual_size))
            .on_action(cx.listener(Self::fit_width))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::scroll_left))
            .on_action(cx.listener(Self::scroll_right))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::first_page))
            .on_action(cx.listener(Self::last_page))
            .child(toolbar)
            .children(error_bar)
            .child(content)
    }
}

#[derive(Clone)]
struct PaintPage {
    index: usize,
    rect: Rect,
    tiles: Vec<PaintTile>,
    text: Option<Arc<TextLayer>>,
}

#[derive(Clone)]
struct PaintTile {
    core_rect: Rect,
    render_rect: Rect,
    image: Arc<RenderImage>,
}

#[derive(Clone)]
struct PaintSnapshot {
    layout: Arc<DocumentLayout>,
    pages: Vec<PaintPage>,
    scroll: Offset,
    selection: Option<TextSelection>,
    viewport_width: f32,
    viewport_height: f32,
}

fn desired_raster_size(page_rect: Rect, scale_factor: f32) -> RasterSize {
    if !page_rect.width.is_finite()
        || !page_rect.height.is_finite()
        || page_rect.width <= 0.0
        || page_rect.height <= 0.0
    {
        return RasterSize {
            width: 1,
            height: 1,
        };
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let aspect = f64::from(page_rect.height) / f64::from(page_rect.width);
    let width_cap =
        ((f64::from(MAX_RASTER_DIMENSION) / aspect).floor() as u32).clamp(1, MAX_RASTER_DIMENSION);
    let raw_width = (f64::from(page_rect.width) * f64::from(scale_factor)).ceil();
    let raw_width = if raw_width.is_finite() {
        raw_width.clamp(1.0, f64::from(width_cap)) as u32
    } else {
        width_cap
    };
    let width = raw_width
        .div_ceil(RENDER_QUANTUM)
        .saturating_mul(RENDER_QUANTUM)
        .min(width_cap)
        .max(1);
    let height = (aspect * f64::from(width))
        .round()
        .clamp(1.0, f64::from(MAX_RASTER_DIMENSION)) as u32;
    RasterSize { width, height }
}

fn plan_visible_tiles(
    layout: &DocumentLayout,
    page_sizes: &[PageSize],
    scroll: Offset,
    viewport_width: f32,
    viewport_height: f32,
    scale_factor: f32,
) -> Vec<PlannedTile> {
    if viewport_width <= 0.0
        || viewport_height <= 0.0
        || !viewport_width.is_finite()
        || !viewport_height.is_finite()
    {
        return Vec::new();
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let viewport = Rect {
        x: scroll.x,
        y: scroll.y,
        width: viewport_width,
        height: viewport_height,
    };
    let vertical_overscan = TILE_SIZE as f32 / scale_factor;
    let viewport_center = (
        f64::from(viewport.x + viewport.width * 0.5),
        f64::from(viewport.y + viewport.height * 0.5),
    );
    let mut result = Vec::new();

    for page in layout.visible_pages(scroll.y, viewport_height, vertical_overscan) {
        let (Some(page_rect), Some(_)) = (layout.page_rect(page), page_sizes.get(page)) else {
            continue;
        };
        let raster = desired_raster_size(page_rect, scale_factor);
        let tile_logical_width = page_rect.width * TILE_SIZE as f32 / raster.width as f32;
        let tile_logical_height = page_rect.height * TILE_SIZE as f32 / raster.height as f32;
        let expanded = Rect {
            x: viewport.x - tile_logical_width,
            y: viewport.y - tile_logical_height,
            width: viewport.width + tile_logical_width * 2.0,
            height: viewport.height + tile_logical_height * 2.0,
        };
        let Some(intersection) = intersect_rect(page_rect, expanded) else {
            continue;
        };

        let pixel_x = |content_x: f32, round_up: bool| {
            let value = ((content_x - page_rect.x) / page_rect.width * raster.width as f32)
                .clamp(0.0, raster.width as f32);
            (if round_up {
                value.ceil()
            } else {
                value.floor()
            }) as u32
        };
        let pixel_y = |content_y: f32, round_up: bool| {
            let value = ((content_y - page_rect.y) / page_rect.height * raster.height as f32)
                .clamp(0.0, raster.height as f32);
            (if round_up {
                value.ceil()
            } else {
                value.floor()
            }) as u32
        };
        let left = pixel_x(intersection.x, false);
        let right = pixel_x(intersection.right(), true);
        let top = pixel_y(intersection.y, false);
        let bottom = pixel_y(intersection.bottom(), true);
        if left >= right || top >= bottom {
            continue;
        }
        let first_column = left / TILE_SIZE;
        let last_column = (right - 1) / TILE_SIZE;
        let first_row = top / TILE_SIZE;
        let last_row = (bottom - 1) / TILE_SIZE;

        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let key = TileKey {
                    page,
                    raster,
                    column,
                    row,
                };
                let Some(core_rect) = tile_core_rect(key) else {
                    continue;
                };
                let logical = tile_logical_rect(page_rect, raster, core_rect);
                let tier = if intersect_rect(logical, viewport).is_some() {
                    DemandTier::Visible
                } else {
                    DemandTier::Prefetch
                };
                let center_x = f64::from(logical.x + logical.width * 0.5);
                let center_y = f64::from(logical.y + logical.height * 0.5);
                let distance = ((center_x - viewport_center.0).powi(2)
                    + (center_y - viewport_center.1).powi(2))
                .round()
                .clamp(0.0, u64::MAX as f64) as u64;
                result.push(PlannedTile {
                    request: TileRequest {
                        key,
                        core_rect,
                        render_rect: inflate_tile_rect(core_rect, raster),
                    },
                    tier,
                    distance,
                });
            }
        }
    }
    result
}

fn tile_core_rect(key: TileKey) -> Option<PixelRect> {
    let x = key.column.checked_mul(TILE_SIZE)?;
    let y = key.row.checked_mul(TILE_SIZE)?;
    if x >= key.raster.width || y >= key.raster.height {
        return None;
    }
    Some(PixelRect {
        x,
        y,
        width: TILE_SIZE.min(key.raster.width - x),
        height: TILE_SIZE.min(key.raster.height - y),
    })
}

fn inflate_tile_rect(core: PixelRect, raster: RasterSize) -> PixelRect {
    let x = core.x.saturating_sub(TILE_BLEED);
    let y = core.y.saturating_sub(TILE_BLEED);
    let right = core
        .x
        .saturating_add(core.width)
        .saturating_add(TILE_BLEED)
        .min(raster.width);
    let bottom = core
        .y
        .saturating_add(core.height)
        .saturating_add(TILE_BLEED)
        .min(raster.height);
    PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

fn tile_logical_rect(page: Rect, raster: RasterSize, pixels: PixelRect) -> Rect {
    let x0 =
        f64::from(page.x) + f64::from(page.width) * f64::from(pixels.x) / f64::from(raster.width);
    let x1 = f64::from(page.x)
        + f64::from(page.width) * f64::from(pixels.x.saturating_add(pixels.width))
            / f64::from(raster.width);
    let y0 =
        f64::from(page.y) + f64::from(page.height) * f64::from(pixels.y) / f64::from(raster.height);
    let y1 = f64::from(page.y)
        + f64::from(page.height) * f64::from(pixels.y.saturating_add(pixels.height))
            / f64::from(raster.height);
    Rect {
        x: x0 as f32,
        y: y0 as f32,
        width: (x1 - x0) as f32,
        height: (y1 - y0) as f32,
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn command_wheel_zoom_factor(delta_y: f32) -> Option<f32> {
    delta_y
        .is_finite()
        .then(|| (delta_y / 420.0).clamp(-1.5, 1.5).exp())
}

fn tile_distance_from_viewport(
    key: TileKey,
    layout: Option<&DocumentLayout>,
    scroll: Offset,
    viewport_width: f32,
    viewport_height: f32,
) -> u64 {
    let Some(logical) = layout
        .and_then(|layout| layout.page_rect(key.page))
        .zip(tile_core_rect(key))
        .map(|(page, core)| tile_logical_rect(page, key.raster, core))
    else {
        return u64::MAX;
    };
    let dx = f64::from(logical.x + logical.width * 0.5 - scroll.x - viewport_width * 0.5);
    let dy = f64::from(logical.y + logical.height * 0.5 - scroll.y - viewport_height * 0.5);
    (dx.powi(2) + dy.powi(2))
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

fn content_rect_to_bounds(canvas: Bounds<Pixels>, rect: Rect, scroll: Offset) -> Bounds<Pixels> {
    Bounds::new(
        point(
            canvas.left() + px(rect.x - scroll.x),
            canvas.top() + px(rect.y - scroll.y),
        ),
        size(px(rect.width), px(rect.height)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter_pages(count: usize) -> Vec<PageSize> {
        vec![
            PageSize {
                width: 612.0,
                height: 792.0,
            };
            count
        ]
    }

    #[test]
    fn high_zoom_raster_is_sharp_without_allocating_a_full_page() {
        let raster = desired_raster_size(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 4_080.0,
                height: 5_280.0,
            },
            2.0,
        );
        assert!(raster.width > 4_096);
        assert!(raster.height > 4_096);
        assert!(raster.width <= MAX_RASTER_DIMENSION);
        assert!(raster.height <= MAX_RASTER_DIMENSION);

        let key = TileKey {
            page: 0,
            raster,
            column: 3,
            row: 4,
        };
        let core = tile_core_rect(key).unwrap();
        let rendered = inflate_tile_rect(core, raster);
        assert!(core.width <= TILE_SIZE && core.height <= TILE_SIZE);
        assert!(rendered.width <= TILE_SIZE + TILE_BLEED * 2);
        assert!(rendered.height <= TILE_SIZE + TILE_BLEED * 2);
        assert!(rendered.width as usize * rendered.height as usize * 4 < 5 * 1024 * 1024);
    }

    #[test]
    fn tile_grid_clips_partial_edges_without_zero_sized_tiles() {
        let raster = RasterSize {
            width: 2_050,
            height: 1_025,
        };
        let first = tile_core_rect(TileKey {
            page: 0,
            raster,
            column: 0,
            row: 0,
        })
        .unwrap();
        let last = tile_core_rect(TileKey {
            page: 0,
            raster,
            column: 2,
            row: 1,
        })
        .unwrap();
        assert_eq!(first.width, 1_024);
        assert_eq!(
            last,
            PixelRect {
                x: 2_048,
                y: 1_024,
                width: 2,
                height: 1
            }
        );
        assert!(
            tile_core_rect(TileKey {
                page: 0,
                raster,
                column: 3,
                row: 0,
            })
            .is_none()
        );
        assert!(
            tile_core_rect(TileKey {
                page: 0,
                raster,
                column: u32::MAX,
                row: 0,
            })
            .is_none()
        );
    }

    #[test]
    fn planner_requests_bounded_tiles_from_both_partially_visible_pages() {
        let pages = letter_pages(2);
        let layout = DocumentLayout::new(&pages, 1.0, 1_100.0);
        let first = layout.page_rect(0).unwrap();
        let second = layout.page_rect(1).unwrap();
        let scroll = Offset {
            x: 0.0,
            y: first.bottom() - 80.0,
        };
        let viewport_height = second.y - scroll.y + 90.0;
        let planned = plan_visible_tiles(&layout, &pages, scroll, 1_100.0, viewport_height, 2.0);
        let visible_pages: HashSet<_> = planned
            .iter()
            .filter_map(|tile| (tile.tier == DemandTier::Visible).then_some(tile.request.key.page))
            .collect();
        assert_eq!(visible_pages, HashSet::from([0, 1]));
        assert!(planned.iter().all(|tile| {
            tile.request.render_rect.width <= TILE_SIZE + TILE_BLEED * 2
                && tile.request.render_rect.height <= TILE_SIZE + TILE_BLEED * 2
                && tile.request.core_rect.width > 0
                && tile.request.core_rect.height > 0
        }));
        let unique: HashSet<_> = planned.iter().map(|tile| tile.request.key).collect();
        assert_eq!(unique.len(), planned.len());
    }

    #[test]
    fn horizontal_panning_only_demands_nearby_columns() {
        let pages = letter_pages(1);
        let layout = DocumentLayout::new(&pages, 5.0, 900.0);
        let page = layout.page_rect(0).unwrap();
        let raster = desired_raster_size(page, 2.0);
        let scroll = Offset {
            x: page.x + page.width * 0.65,
            y: page.y + 400.0,
        };
        let planned = plan_visible_tiles(&layout, &pages, scroll, 700.0, 600.0, 2.0);
        let columns: HashSet<_> = planned.iter().map(|tile| tile.request.key.column).collect();
        assert!(!columns.is_empty());
        assert!(columns.len() <= 4);
        assert!(
            columns
                .iter()
                .all(|column| *column < raster.width.div_ceil(TILE_SIZE))
        );
        assert!(!columns.contains(&0));
    }

    #[test]
    fn adjacent_tile_destinations_share_the_same_global_edge() {
        let page = Rect {
            x: 12.25,
            y: 30.5,
            width: 777.3,
            height: 1_005.8,
        };
        let raster = RasterSize {
            width: 1_663,
            height: 2_151,
        };
        let left = tile_logical_rect(
            page,
            raster,
            PixelRect {
                x: 0,
                y: 0,
                width: 1_024,
                height: 1_024,
            },
        );
        let right = tile_logical_rect(
            page,
            raster,
            PixelRect {
                x: 1_024,
                y: 0,
                width: raster.width - 1_024,
                height: 1_024,
            },
        );
        assert!((left.right() - right.x).abs() < 0.0001);
        assert!((right.right() - page.right()).abs() < 0.0001);
    }

    #[test]
    fn invalid_raster_inputs_are_finite_and_bounded() {
        for rect in [
            Rect {
                x: 0.0,
                y: 0.0,
                width: f32::NAN,
                height: 1.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: f32::INFINITY,
            },
        ] {
            assert_eq!(
                desired_raster_size(rect, f32::INFINITY),
                RasterSize {
                    width: 1,
                    height: 1
                }
            );
        }
    }

    #[test]
    fn accelerated_command_wheel_zoom_is_finite_and_clamped_per_packet() {
        assert_eq!(command_wheel_zoom_factor(0.0), Some(1.0));
        assert_eq!(command_wheel_zoom_factor(f32::NAN), None);
        assert_eq!(command_wheel_zoom_factor(f32::INFINITY), None);

        let maximum = command_wheel_zoom_factor(f32::MAX).unwrap();
        let minimum = command_wheel_zoom_factor(-f32::MAX).unwrap();
        assert!((maximum - 1.5_f32.exp()).abs() < f32::EPSILON);
        assert!((minimum - (-1.5_f32).exp()).abs() < f32::EPSILON);
        assert!(minimum > 0.0 && maximum.is_finite());
    }
}
