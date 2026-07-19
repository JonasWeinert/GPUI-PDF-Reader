//! Embeddable, engine-independent PDF viewport behavior and rendering for GPUI
//! applications.
//!
//! The controller owns document layout, bounded navigation, and tile-demand
//! planning. The rendered canvas owns paper, shadows, decoded-tile clipping,
//! scrollbars, and a bounded page-overlay hook. The crate deliberately has no
//! dependency on a PDF engine, filesystem, network client, sidecar store,
//! application menu, or product UI.

#![forbid(unsafe_code)]

mod canvas_model;
mod controller;
mod planning;

#[cfg(target_os = "macos")]
mod canvas;
#[cfg(target_os = "macos")]
mod gpui_adapter;

pub use canvas_model::{
    DEFAULT_MAX_CANVAS_PAGES, DEFAULT_MAX_CANVAS_TILES, PdfCanvasFramePlan, PdfCanvasLimits,
    PdfCanvasMetrics, PdfCanvasPageGeometry, PdfCanvasScrollbars, PlannedCanvasPage,
    page_viewport_rect, pdf_canvas_scrollbars, plan_pdf_canvas_frame,
};

pub use controller::{
    DEFAULT_MAX_CACHE_BYTES, DEFAULT_MAX_CACHED_TEXT_PAGES, DEFAULT_MAX_CACHED_TILES,
    DEFAULT_MAX_PLANNED_TILES, DEFAULT_MAX_RASTER_DIMENSION, DEFAULT_MAX_VIEWPORT_DIMENSION,
    DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_RENDER_QUANTUM, DEFAULT_TILE_BLEED,
    DEFAULT_TILE_SIZE, DemandInvalidation, InputDisposition, NavigationCommand,
    PdfReaderAppearance, PdfReaderConfig, PdfReaderEvent, PdfReaderLimits, ScrollBehavior,
    ScrollOffset, ViewportColor, ViewportController, ViewportError, ViewportMetrics, ViewportPoint,
    ViewportSnapshot, WheelInput, command_wheel_zoom_factor,
};
pub use planning::{
    DemandTier, PlannedTile, TileDemandPlan, TilePaintGeometry, TilePlanningInput, TileRequest,
    desired_raster_size, inflate_tile_rect, plan_visible_tiles, tile_core_rect, tile_logical_rect,
};

#[cfg(target_os = "macos")]
pub use canvas::{
    PdfCanvasPage, PdfCanvasPagePaintContext, PdfCanvasSnapshot, PdfCanvasStyle, PdfCanvasTile,
    content_rect_to_bounds, pdf_canvas, pdf_canvas_measured,
};
#[cfg(target_os = "macos")]
pub use gpui_adapter::{PdfViewport, appearance_from_theme};
