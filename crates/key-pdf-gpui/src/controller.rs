use crate::planning::{TileDemandPlan, TilePlanningInput, plan_visible_tiles};
use key_pdf_core::{
    DocumentLayout, PAGE_MARGIN, PDF_POINTS_TO_LOGICAL_PIXELS, PageAnchor, PageSize, Rect,
};
use key_pdf_runtime::{ColorMode, PixelColor};
use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

pub const DEFAULT_MIN_ZOOM: f32 = 0.2;
pub const DEFAULT_MAX_ZOOM: f32 = 5.0;
pub const DEFAULT_RENDER_QUANTUM: u32 = 64;
pub const DEFAULT_TILE_SIZE: u32 = 1_024;
pub const DEFAULT_TILE_BLEED: u32 = 32;
pub const DEFAULT_MAX_RASTER_DIMENSION: u32 = 65_536;
pub const DEFAULT_MAX_CACHED_TILES: usize = 48;
pub const DEFAULT_MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
pub const DEFAULT_MAX_CACHED_TEXT_PAGES: usize = 16;
pub const DEFAULT_MAX_PLANNED_TILES: usize = 256;
pub const DEFAULT_MAX_VIEWPORT_DIMENSION: f32 = 32_768.0;

const ABSOLUTE_MIN_TILE_SIZE: u32 = 256;
const ABSOLUTE_MAX_TILE_SIZE: u32 = 4_096;
const ABSOLUTE_MAX_PLANNED_TILES: usize = 4_096;
const ABSOLUTE_MAX_PENDING_EVENTS: usize = 1_024;

/// Resource and geometry ceilings enforced by the reusable viewport.
///
/// Values are public for straightforward host configuration. Every public
/// operation normalizes them against non-zero absolute safety ceilings before
/// use, so malformed extension or preference input cannot request an unbounded
/// raster or tile plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfReaderLimits {
    pub min_zoom: f32,
    pub max_zoom: f32,
    pub render_quantum: u32,
    pub tile_size: u32,
    pub tile_bleed: u32,
    pub max_raster_dimension: u32,
    pub max_cached_tiles: usize,
    pub max_cache_bytes: usize,
    pub max_cached_text_pages: usize,
    pub max_planned_tiles: usize,
    pub max_pending_events: usize,
    pub max_viewport_dimension: f32,
}

impl Default for PdfReaderLimits {
    fn default() -> Self {
        Self {
            min_zoom: DEFAULT_MIN_ZOOM,
            max_zoom: DEFAULT_MAX_ZOOM,
            render_quantum: DEFAULT_RENDER_QUANTUM,
            tile_size: DEFAULT_TILE_SIZE,
            tile_bleed: DEFAULT_TILE_BLEED,
            max_raster_dimension: DEFAULT_MAX_RASTER_DIMENSION,
            max_cached_tiles: DEFAULT_MAX_CACHED_TILES,
            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            max_cached_text_pages: DEFAULT_MAX_CACHED_TEXT_PAGES,
            max_planned_tiles: DEFAULT_MAX_PLANNED_TILES,
            max_pending_events: 128,
            max_viewport_dimension: DEFAULT_MAX_VIEWPORT_DIMENSION,
        }
    }
}

impl PdfReaderLimits {
    #[must_use]
    pub fn normalized(self) -> Self {
        let minimum = finite_or(self.min_zoom, DEFAULT_MIN_ZOOM).clamp(0.01, 100.0);
        let maximum = finite_or(self.max_zoom, DEFAULT_MAX_ZOOM).clamp(minimum, 100.0);
        let max_raster_dimension = self
            .max_raster_dimension
            .clamp(1, DEFAULT_MAX_RASTER_DIMENSION);
        let render_quantum = self.render_quantum.clamp(1, max_raster_dimension);
        let tile_size = self
            .tile_size
            .clamp(ABSOLUTE_MIN_TILE_SIZE, ABSOLUTE_MAX_TILE_SIZE);
        Self {
            min_zoom: minimum,
            max_zoom: maximum,
            render_quantum,
            tile_size,
            tile_bleed: self.tile_bleed.min(tile_size),
            max_raster_dimension,
            max_cached_tiles: self.max_cached_tiles.clamp(1, 4_096),
            max_cache_bytes: self.max_cache_bytes.clamp(1024, 2 * 1024 * 1024 * 1024),
            max_cached_text_pages: self.max_cached_text_pages.clamp(1, 4_096),
            max_planned_tiles: self.max_planned_tiles.clamp(1, ABSOLUTE_MAX_PLANNED_TILES),
            max_pending_events: self
                .max_pending_events
                .clamp(1, ABSOLUTE_MAX_PENDING_EVENTS),
            max_viewport_dimension: finite_or(
                self.max_viewport_dimension,
                DEFAULT_MAX_VIEWPORT_DIMENSION,
            )
            .clamp(1.0, DEFAULT_MAX_VIEWPORT_DIMENSION),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewportColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl ViewportColor {
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

/// Product-neutral PDF canvas colors and engine render transformation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfReaderAppearance {
    pub pane_background: ViewportColor,
    pub paper_background: ViewportColor,
    pub paper_border: ViewportColor,
    pub selection: ViewportColor,
    pub render_color_mode: ColorMode,
}

impl Default for PdfReaderAppearance {
    fn default() -> Self {
        Self {
            pane_background: ViewportColor::rgba(238, 240, 243, 255),
            paper_background: ViewportColor::rgba(255, 255, 255, 255),
            paper_border: ViewportColor::rgba(202, 205, 211, 255),
            selection: ViewportColor::rgba(56, 132, 255, 96),
            render_color_mode: ColorMode::Original,
        }
    }
}

impl From<ViewportColor> for PixelColor {
    fn from(value: ViewportColor) -> Self {
        Self {
            red: value.red,
            green: value.green,
            blue: value.blue,
            alpha: value.alpha,
        }
    }
}

/// Host-level behavior and appearance configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfReaderConfig {
    pub limits: PdfReaderLimits,
    pub appearance: PdfReaderAppearance,
    pub initial_zoom: f32,
    pub fit_width_on_open: bool,
    pub zoom_step: f32,
    pub scroll_step: f32,
    pub page_scroll_fraction: f32,
    pub scroll_animation_response: f32,
    pub fit_width_minimum: f32,
}

impl Default for PdfReaderConfig {
    fn default() -> Self {
        Self {
            limits: PdfReaderLimits::default(),
            appearance: PdfReaderAppearance::default(),
            initial_zoom: 1.0,
            fit_width_on_open: false,
            zoom_step: 1.15,
            scroll_step: 64.0,
            page_scroll_fraction: 0.9,
            scroll_animation_response: 18.0,
            fit_width_minimum: 100.0,
        }
    }
}

impl PdfReaderConfig {
    #[must_use]
    pub fn normalized(self) -> Self {
        let limits = self.limits.normalized();
        Self {
            limits,
            appearance: self.appearance,
            initial_zoom: finite_or(self.initial_zoom, 1.0).clamp(limits.min_zoom, limits.max_zoom),
            fit_width_on_open: self.fit_width_on_open,
            zoom_step: finite_or(self.zoom_step, 1.15).clamp(1.001, 4.0),
            scroll_step: finite_or(self.scroll_step, 64.0).clamp(1.0, 4_096.0),
            page_scroll_fraction: finite_or(self.page_scroll_fraction, 0.9).clamp(0.1, 1.0),
            scroll_animation_response: finite_or(self.scroll_animation_response, 18.0)
                .clamp(1.0, 100.0),
            fit_width_minimum: finite_or(self.fit_width_minimum, 100.0).clamp(1.0, 4_096.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollOffset {
    pub x: f32,
    pub y: f32,
}

impl ScrollOffset {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewportPoint {
    pub x: f32,
    pub y: f32,
}

impl ViewportPoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportMetrics {
    pub width: f32,
    pub height: f32,
    pub right_occlusion: f32,
    pub scale_factor: f32,
}

impl Default for ViewportMetrics {
    fn default() -> Self {
        Self {
            width: 1.0,
            height: 1.0,
            right_occlusion: 0.0,
            scale_factor: 1.0,
        }
    }
}

impl ViewportMetrics {
    #[must_use]
    pub fn safe_width(self) -> f32 {
        (self.width - self.right_occlusion).max(1.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollBehavior {
    Immediate,
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelInput {
    pub delta_x: f32,
    pub delta_y: f32,
    pub precise: bool,
    pub shift: bool,
    pub zoom_modifier: bool,
    pub position: ViewportPoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationCommand {
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
    PageUp,
    PageDown,
    FirstPage,
    LastPage,
    ZoomIn,
    ZoomOut,
    ActualSize,
    FitWidth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDisposition {
    Applied,
    Unchanged,
    IgnoredInvalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemandInvalidation {
    Document,
    Viewport,
    Scroll,
    Zoom,
    Appearance,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PdfReaderEvent {
    DocumentChanged {
        page_count: usize,
    },
    ViewportChanged(ViewportMetrics),
    ScrollChanged {
        offset: ScrollOffset,
        target: ScrollOffset,
    },
    ZoomChanged {
        zoom: f32,
        fit_width: bool,
    },
    CurrentPageChanged {
        page: usize,
    },
    AppearanceChanged(PdfReaderAppearance),
    DemandInvalidated {
        revision: u64,
        cause: DemandInvalidation,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewportSnapshot {
    pub page_count: usize,
    pub metrics: ViewportMetrics,
    pub scroll: ScrollOffset,
    pub scroll_target: ScrollOffset,
    pub zoom: f32,
    pub fit_width: bool,
    pub content_width: f32,
    pub content_height: f32,
    pub current_page: Option<usize>,
    pub visible_pages: Range<usize>,
    pub demand_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportError {
    InvalidPageSize { page: usize },
}

impl fmt::Display for ViewportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageSize { page } => {
                write!(formatter, "page {} has invalid dimensions", page + 1)
            }
        }
    }
}

impl std::error::Error for ViewportError {}

/// Engine-independent state machine for an embeddable PDF viewport.
pub struct ViewportController {
    config: PdfReaderConfig,
    page_sizes: Arc<[PageSize]>,
    layout: Option<DocumentLayout>,
    metrics: ViewportMetrics,
    scroll: ScrollOffset,
    scroll_target: ScrollOffset,
    zoom: f32,
    fit_width: bool,
    demand_revision: u64,
    current_page: Option<usize>,
    events: VecDeque<PdfReaderEvent>,
}

impl fmt::Debug for ViewportController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewportController")
            .field("config", &self.config)
            .field("page_count", &self.page_sizes.len())
            .field("metrics", &self.metrics)
            .field("scroll", &self.scroll)
            .field("scroll_target", &self.scroll_target)
            .field("zoom", &self.zoom)
            .field("fit_width", &self.fit_width)
            .field("demand_revision", &self.demand_revision)
            .finish_non_exhaustive()
    }
}

impl Default for ViewportController {
    fn default() -> Self {
        Self::new(PdfReaderConfig::default())
    }
}

impl ViewportController {
    #[must_use]
    pub fn new(config: PdfReaderConfig) -> Self {
        let config = config.normalized();
        Self {
            zoom: config.initial_zoom,
            fit_width: config.fit_width_on_open,
            config,
            page_sizes: Arc::from([]),
            layout: None,
            metrics: ViewportMetrics::default(),
            scroll: ScrollOffset::default(),
            scroll_target: ScrollOffset::default(),
            demand_revision: 0,
            current_page: None,
            events: VecDeque::new(),
        }
    }

    pub fn with_document(
        config: PdfReaderConfig,
        page_sizes: impl Into<Arc<[PageSize]>>,
    ) -> Result<Self, ViewportError> {
        let mut controller = Self::new(config);
        controller.set_document_pages(page_sizes)?;
        Ok(controller)
    }

    #[must_use]
    pub const fn config(&self) -> &PdfReaderConfig {
        &self.config
    }

    #[must_use]
    pub fn page_sizes(&self) -> &[PageSize] {
        &self.page_sizes
    }

    #[must_use]
    pub const fn layout(&self) -> Option<&DocumentLayout> {
        self.layout.as_ref()
    }

    #[must_use]
    pub const fn zoom(&self) -> f32 {
        self.zoom
    }

    #[must_use]
    pub const fn fit_width_enabled(&self) -> bool {
        self.fit_width
    }

    #[must_use]
    pub const fn metrics(&self) -> ViewportMetrics {
        self.metrics
    }

    #[must_use]
    pub const fn scroll(&self) -> ScrollOffset {
        self.scroll
    }

    #[must_use]
    pub const fn scroll_target(&self) -> ScrollOffset {
        self.scroll_target
    }

    #[must_use]
    pub fn snapshot(&self) -> ViewportSnapshot {
        let (content_width, content_height, visible_pages) =
            self.layout.as_ref().map_or((0.0, 0.0, 0..0), |layout| {
                (
                    layout.content_width,
                    layout.content_height,
                    layout.visible_pages(self.scroll.y, self.metrics.height, 0.0),
                )
            });
        ViewportSnapshot {
            page_count: self.page_sizes.len(),
            metrics: self.metrics,
            scroll: self.scroll,
            scroll_target: self.scroll_target,
            zoom: self.zoom,
            fit_width: self.fit_width,
            content_width,
            content_height,
            current_page: self.current_page,
            visible_pages,
            demand_revision: self.demand_revision,
        }
    }

    pub fn set_document_pages(
        &mut self,
        page_sizes: impl Into<Arc<[PageSize]>>,
    ) -> Result<InputDisposition, ViewportError> {
        let page_sizes = page_sizes.into();
        for (page, size) in page_sizes.iter().enumerate() {
            if !size.width.is_finite()
                || !size.height.is_finite()
                || size.width <= 0.0
                || size.height <= 0.0
            {
                return Err(ViewportError::InvalidPageSize { page });
            }
        }
        self.page_sizes = page_sizes;
        // A replacement document can have the same page count but completely
        // different geometry. Never rescale the preceding document's shared
        // page-size arrays in that case.
        self.layout = None;
        self.zoom = self.config.initial_zoom;
        self.fit_width = self.config.fit_width_on_open;
        if self.fit_width {
            self.zoom = self.fit_width_zoom();
        }
        self.rebuild_layout();
        self.scroll = ScrollOffset::default();
        self.scroll_target = ScrollOffset::default();
        self.current_page = (!self.page_sizes.is_empty()).then_some(0);
        self.push_event(PdfReaderEvent::DocumentChanged {
            page_count: self.page_sizes.len(),
        });
        self.push_event(PdfReaderEvent::ZoomChanged {
            zoom: self.zoom,
            fit_width: self.fit_width,
        });
        self.push_event(PdfReaderEvent::ScrollChanged {
            offset: self.scroll,
            target: self.scroll_target,
        });
        self.invalidate(DemandInvalidation::Document);
        Ok(InputDisposition::Applied)
    }

    pub fn clear_document(&mut self) -> InputDisposition {
        if self.page_sizes.is_empty() {
            return InputDisposition::Unchanged;
        }
        self.page_sizes = Arc::from([]);
        self.layout = None;
        self.scroll = ScrollOffset::default();
        self.scroll_target = ScrollOffset::default();
        self.current_page = None;
        self.fit_width = self.config.fit_width_on_open;
        self.zoom = self.config.initial_zoom;
        self.push_event(PdfReaderEvent::DocumentChanged { page_count: 0 });
        self.invalidate(DemandInvalidation::Document);
        InputDisposition::Applied
    }

    /// Updates the viewport while retaining the PDF point at the center of the
    /// unobscured region. This is the same operation for a window resize, a
    /// classic sidebar changing the viewport width, or a fluid overlay changing
    /// `right_occlusion`.
    pub fn set_viewport(&mut self, metrics: ViewportMetrics) -> InputDisposition {
        let Some(metrics) = self.normalized_metrics(metrics) else {
            return InputDisposition::IgnoredInvalid;
        };
        if metrics == self.metrics {
            return InputDisposition::Unchanged;
        }
        let anchor = self.center_anchor();
        let old_zoom = self.zoom;
        self.metrics = metrics;
        if self.fit_width && !self.page_sizes.is_empty() {
            self.zoom = self.fit_width_zoom();
        }
        self.rebuild_layout();
        self.restore_center_anchor(anchor);
        self.push_event(PdfReaderEvent::ViewportChanged(metrics));
        if (self.zoom - old_zoom).abs() > 0.0001 {
            self.push_event(PdfReaderEvent::ZoomChanged {
                zoom: self.zoom,
                fit_width: true,
            });
        }
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Viewport);
        InputDisposition::Applied
    }

    pub fn set_appearance(&mut self, appearance: PdfReaderAppearance) -> InputDisposition {
        if appearance == self.config.appearance {
            return InputDisposition::Unchanged;
        }
        self.config.appearance = appearance;
        self.push_event(PdfReaderEvent::AppearanceChanged(appearance));
        self.invalidate(DemandInvalidation::Appearance);
        InputDisposition::Applied
    }

    pub fn scroll_by(&mut self, delta: ScrollOffset, behavior: ScrollBehavior) -> InputDisposition {
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return InputDisposition::IgnoredInvalid;
        }
        if self.layout.is_none() {
            return InputDisposition::Unchanged;
        }
        let old_scroll = self.scroll;
        let old_target = self.scroll_target;
        if behavior == ScrollBehavior::Immediate {
            self.scroll_target = self.scroll;
        }
        self.scroll_target.x = bounded_add(self.scroll_target.x, delta.x);
        self.scroll_target.y = bounded_add(self.scroll_target.y, delta.y);
        self.clamp_scroll();
        if behavior == ScrollBehavior::Immediate {
            self.scroll = self.scroll_target;
        }
        if self.scroll == old_scroll && self.scroll_target == old_target {
            return InputDisposition::Unchanged;
        }
        self.update_current_page();
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Scroll);
        InputDisposition::Applied
    }

    pub fn set_scroll(&mut self, offset: ScrollOffset) -> InputDisposition {
        if !offset.x.is_finite() || !offset.y.is_finite() {
            return InputDisposition::IgnoredInvalid;
        }
        if self.layout.is_none() {
            return InputDisposition::Unchanged;
        }
        let old_scroll = self.scroll;
        let old_target = self.scroll_target;
        self.scroll = offset;
        self.scroll_target = offset;
        self.clamp_scroll();
        if self.scroll == old_scroll && self.scroll_target == old_target {
            return InputDisposition::Unchanged;
        }
        self.update_current_page();
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Scroll);
        InputDisposition::Applied
    }

    /// Moves to an absolute document offset either immediately or as the next
    /// smooth-navigation target.
    pub fn scroll_to(
        &mut self,
        offset: ScrollOffset,
        behavior: ScrollBehavior,
    ) -> InputDisposition {
        if behavior == ScrollBehavior::Immediate {
            return self.set_scroll(offset);
        }
        if !offset.x.is_finite() || !offset.y.is_finite() {
            return InputDisposition::IgnoredInvalid;
        }
        if self.layout.is_none() {
            return InputDisposition::Unchanged;
        }
        let old_target = self.scroll_target;
        self.scroll_target = offset;
        self.clamp_scroll();
        if self.scroll_target == old_target {
            return InputDisposition::Unchanged;
        }
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Scroll);
        InputDisposition::Applied
    }

    /// Leaves the current zoom unchanged while disabling automatic fit-width
    /// updates during panel transitions or direct manipulation.
    pub fn disable_fit_width(&mut self) -> InputDisposition {
        if !self.fit_width {
            return InputDisposition::Unchanged;
        }
        self.fit_width = false;
        self.push_event(PdfReaderEvent::ZoomChanged {
            zoom: self.zoom,
            fit_width: false,
        });
        InputDisposition::Applied
    }

    #[must_use]
    pub fn maximum_scroll(&self) -> ScrollOffset {
        self.layout.as_ref().map_or_else(ScrollOffset::default, |layout| {
            ScrollOffset {
                x: (layout.content_width - self.metrics.width + self.metrics.right_occlusion)
                    .max(0.0),
                y: (layout.content_height - self.metrics.height).max(0.0),
            }
        })
    }

    #[must_use]
    pub fn is_scrolling(&self) -> bool {
        (self.scroll_target.x - self.scroll.x).abs() + (self.scroll_target.y - self.scroll.y).abs()
            >= 0.35
    }

    /// Advances smooth scrolling using the reader's frame-rate-independent
    /// exponential response. Returns whether another frame is required.
    pub fn advance_navigation(&mut self, elapsed_seconds: f32) -> bool {
        if !self.is_scrolling() {
            self.scroll = self.scroll_target;
            return false;
        }
        if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return true;
        }
        let elapsed_seconds = elapsed_seconds.clamp(1.0 / 240.0, 0.05);
        let blend = 1.0 - (-self.config.scroll_animation_response * elapsed_seconds).exp();
        self.scroll.x += (self.scroll_target.x - self.scroll.x) * blend;
        self.scroll.y += (self.scroll_target.y - self.scroll.y) * blend;
        if !self.is_scrolling() {
            self.scroll = self.scroll_target;
        }
        self.update_current_page();
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Scroll);
        self.is_scrolling()
    }

    pub fn zoom_at(&mut self, requested_zoom: f32, point: ViewportPoint) -> InputDisposition {
        self.zoom_at_with_mode(requested_zoom, point, false)
    }

    fn zoom_at_with_mode(
        &mut self,
        requested_zoom: f32,
        point: ViewportPoint,
        fit_width: bool,
    ) -> InputDisposition {
        if self.layout.is_none()
            || !requested_zoom.is_finite()
            || !point.x.is_finite()
            || !point.y.is_finite()
        {
            return if self.layout.is_none() {
                InputDisposition::Unchanged
            } else {
                InputDisposition::IgnoredInvalid
            };
        }
        let new_zoom =
            requested_zoom.clamp(self.config.limits.min_zoom, self.config.limits.max_zoom);
        if (new_zoom - self.zoom).abs() < 0.0001 {
            if self.fit_width == fit_width {
                return InputDisposition::Unchanged;
            }
            self.fit_width = fit_width;
            self.push_event(PdfReaderEvent::ZoomChanged {
                zoom: self.zoom,
                fit_width,
            });
            return InputDisposition::Applied;
        }
        let point = ViewportPoint {
            x: point.x.clamp(0.0, self.metrics.width),
            y: point.y.clamp(0.0, self.metrics.height),
        };
        let anchor = self.layout.as_ref().and_then(|layout| {
            layout.anchor_at_content_point(self.scroll.x + point.x, self.scroll.y + point.y)
        });
        self.zoom = new_zoom;
        self.fit_width = fit_width;
        self.rebuild_layout();
        if let Some(anchor) = anchor
            && let Some((x, y)) = self
                .layout
                .as_ref()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.scroll = ScrollOffset::new(x - point.x, y - point.y);
            self.scroll_target = self.scroll;
        }
        self.clamp_scroll();
        self.update_current_page();
        self.push_event(PdfReaderEvent::ZoomChanged {
            zoom: self.zoom,
            fit_width,
        });
        self.push_scroll_event();
        self.invalidate(DemandInvalidation::Zoom);
        InputDisposition::Applied
    }

    pub fn zoom_by(&mut self, factor: f32, point: ViewportPoint) -> InputDisposition {
        if !factor.is_finite() || factor <= 0.0 {
            return InputDisposition::IgnoredInvalid;
        }
        self.zoom_at(self.zoom * factor, point)
    }

    pub fn zoom_from_wheel(&mut self, delta_y: f32, point: ViewportPoint) -> InputDisposition {
        let Some(factor) = command_wheel_zoom_factor(delta_y) else {
            return InputDisposition::IgnoredInvalid;
        };
        self.zoom_by(factor, point)
    }

    pub fn fit_width(&mut self) -> InputDisposition {
        if self.layout.is_none() {
            return InputDisposition::Unchanged;
        }
        let target = self.fit_width_zoom();
        let center = ViewportPoint::new(self.metrics.safe_width() * 0.5, self.metrics.height * 0.5);
        self.zoom_at_with_mode(target, center, true)
    }

    pub fn apply_wheel(&mut self, input: WheelInput) -> InputDisposition {
        if !input.delta_x.is_finite() || !input.delta_y.is_finite() {
            return InputDisposition::IgnoredInvalid;
        }
        if input.zoom_modifier {
            return self.zoom_from_wheel(input.delta_y, input.position);
        }
        let (x, y) = if input.shift && input.delta_x.abs() < f32::EPSILON {
            (-input.delta_y, 0.0)
        } else {
            (-input.delta_x, -input.delta_y)
        };
        self.scroll_by(
            ScrollOffset::new(x, y),
            if input.precise {
                ScrollBehavior::Immediate
            } else {
                ScrollBehavior::Smooth
            },
        )
    }

    pub fn apply_navigation_command(&mut self, command: NavigationCommand) -> InputDisposition {
        let center = ViewportPoint::new(self.metrics.safe_width() * 0.5, self.metrics.height * 0.5);
        match command {
            NavigationCommand::ScrollUp => self.scroll_by(
                ScrollOffset::new(0.0, -self.config.scroll_step),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::ScrollDown => self.scroll_by(
                ScrollOffset::new(0.0, self.config.scroll_step),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::ScrollLeft => self.scroll_by(
                ScrollOffset::new(-self.config.scroll_step, 0.0),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::ScrollRight => self.scroll_by(
                ScrollOffset::new(self.config.scroll_step, 0.0),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::PageUp => self.scroll_by(
                ScrollOffset::new(0.0, -self.metrics.height * self.config.page_scroll_fraction),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::PageDown => self.scroll_by(
                ScrollOffset::new(0.0, self.metrics.height * self.config.page_scroll_fraction),
                ScrollBehavior::Smooth,
            ),
            NavigationCommand::FirstPage => self.set_scroll(ScrollOffset::default()),
            NavigationCommand::LastPage => {
                let y = self
                    .layout
                    .as_ref()
                    .map_or(0.0, |layout| layout.content_height);
                self.set_scroll(ScrollOffset::new(self.scroll.x, y))
            }
            NavigationCommand::ZoomIn => self.zoom_by(self.config.zoom_step, center),
            NavigationCommand::ZoomOut => self.zoom_by(1.0 / self.config.zoom_step, center),
            NavigationCommand::ActualSize => self.zoom_at(1.0, center),
            NavigationCommand::FitWidth => self.fit_width(),
        }
    }

    #[must_use]
    pub fn visible_pages(&self, overscan: f32) -> Range<usize> {
        let Some(layout) = self.layout.as_ref() else {
            return 0..0;
        };
        layout.visible_pages(
            self.scroll.y,
            self.metrics.height,
            if overscan.is_finite() {
                overscan.max(0.0)
            } else {
                0.0
            },
        )
    }

    #[must_use]
    pub fn plan_tiles(&self) -> TileDemandPlan {
        let Some(layout) = self.layout.as_ref() else {
            return TileDemandPlan {
                visible_pages: 0..0,
                tiles: Vec::new(),
            };
        };
        plan_visible_tiles(
            layout,
            &self.page_sizes,
            TilePlanningInput::new(
                Rect {
                    x: self.scroll.x,
                    y: self.scroll.y,
                    width: self.metrics.width,
                    height: self.metrics.height,
                },
                self.metrics.scale_factor,
            ),
            &self.config.limits,
        )
    }

    #[must_use]
    pub fn page_paint_rects(&self) -> Vec<(usize, Rect)> {
        self.visible_pages(0.0)
            .filter_map(|page| {
                self.layout
                    .as_ref()?
                    .page_rect(page)
                    .map(|rect| (page, rect))
            })
            .collect()
    }

    pub fn drain_events(&mut self) -> impl Iterator<Item = PdfReaderEvent> + '_ {
        self.events.drain(..)
    }

    fn normalized_metrics(&self, metrics: ViewportMetrics) -> Option<ViewportMetrics> {
        if !metrics.width.is_finite()
            || !metrics.height.is_finite()
            || !metrics.right_occlusion.is_finite()
            || !metrics.scale_factor.is_finite()
            || metrics.width <= 0.0
            || metrics.height <= 0.0
            || metrics.scale_factor <= 0.0
        {
            return None;
        }
        let maximum = self.config.limits.max_viewport_dimension;
        let width = metrics.width.min(maximum);
        Some(ViewportMetrics {
            width,
            height: metrics.height.min(maximum),
            right_occlusion: metrics.right_occlusion.clamp(0.0, (width - 1.0).max(0.0)),
            scale_factor: metrics.scale_factor.clamp(0.25, 16.0),
        })
    }

    fn rebuild_layout(&mut self) {
        if self.page_sizes.is_empty() {
            self.layout = None;
            return;
        }
        self.layout = Some(match self.layout.take() {
            Some(layout) if layout.page_count() == self.page_sizes.len() => {
                layout.rescaled(self.zoom, self.metrics.width)
            }
            _ => DocumentLayout::new(&self.page_sizes, self.zoom, self.metrics.width),
        });
    }

    fn fit_width_zoom(&self) -> f32 {
        let widest = self
            .page_sizes
            .iter()
            .map(|page| page.width)
            .fold(1.0_f32, f32::max);
        let available =
            (self.metrics.safe_width() - PAGE_MARGIN * 2.0).max(self.config.fit_width_minimum);
        (available / (widest * PDF_POINTS_TO_LOGICAL_PIXELS))
            .clamp(self.config.limits.min_zoom, self.config.limits.max_zoom)
    }

    fn center_anchor(&self) -> Option<PageAnchor> {
        self.layout.as_ref()?.anchor_at_content_point(
            self.scroll.x + self.metrics.safe_width() * 0.5,
            self.scroll.y + self.metrics.height * 0.5,
        )
    }

    fn restore_center_anchor(&mut self, anchor: Option<PageAnchor>) {
        if let Some(anchor) = anchor
            && let Some((x, y)) = self
                .layout
                .as_ref()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.scroll = ScrollOffset::new(
                x - self.metrics.safe_width() * 0.5,
                y - self.metrics.height * 0.5,
            );
            self.scroll_target = self.scroll;
        }
        self.clamp_scroll();
        self.update_current_page();
    }

    fn clamp_scroll(&mut self) {
        let Some(_) = self.layout.as_ref() else {
            self.scroll = ScrollOffset::default();
            self.scroll_target = ScrollOffset::default();
            return;
        };
        let ScrollOffset { x: max_x, y: max_y } = self.maximum_scroll();
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
        self.scroll_target.x = self.scroll_target.x.clamp(0.0, max_x);
        self.scroll_target.y = self.scroll_target.y.clamp(0.0, max_y);
    }

    fn update_current_page(&mut self) {
        let current = self
            .layout
            .as_ref()
            .map(|layout| layout.current_page(self.scroll.y, self.metrics.height));
        if current != self.current_page {
            self.current_page = current;
            if let Some(page) = current {
                self.push_event(PdfReaderEvent::CurrentPageChanged { page });
            }
        }
    }

    fn push_scroll_event(&mut self) {
        self.push_event(PdfReaderEvent::ScrollChanged {
            offset: self.scroll,
            target: self.scroll_target,
        });
    }

    fn invalidate(&mut self, cause: DemandInvalidation) {
        self.demand_revision = self.demand_revision.wrapping_add(1);
        self.push_event(PdfReaderEvent::DemandInvalidated {
            revision: self.demand_revision,
            cause,
        });
    }

    fn push_event(&mut self, event: PdfReaderEvent) {
        if self.events.len() == self.config.limits.max_pending_events {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

#[must_use]
pub fn command_wheel_zoom_factor(delta_y: f32) -> Option<f32> {
    delta_y
        .is_finite()
        .then(|| (delta_y / 420.0).clamp(-1.5, 1.5).exp())
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn bounded_add(left: f32, right: f32) -> f32 {
    (f64::from(left) + f64::from(right)).clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
}
