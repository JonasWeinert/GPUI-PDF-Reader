use crate::canvas_model::{
    PdfCanvasLimits, PdfCanvasMetrics, page_viewport_rect, pdf_canvas_scrollbars, valid_rect,
};
use gpui::{
    Bounds, ContentMask, Corners, Hsla, IntoElement, Pixels, RenderImage, Styled, Window, canvas,
    point, px, quad, size,
};
use key_pdf_core::Rect;
use std::sync::Arc;

/// Trusted visual tokens for the base PDF canvas.
///
/// Product UI owns token resolution; the component only consumes the resolved
/// colors and paints pages consistently. Overlay colors remain the host's or a
/// feature component's responsibility.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfCanvasStyle {
    pub pane_background: Hsla,
    pub paper_background: Hsla,
    pub paper_border: Hsla,
    pub paper_shadow: Hsla,
    pub scrollbar: Hsla,
}

impl PdfCanvasStyle {
    #[must_use]
    pub const fn new(
        pane_background: Hsla,
        paper_background: Hsla,
        paper_border: Hsla,
        paper_shadow: Hsla,
        scrollbar: Hsla,
    ) -> Self {
        Self {
            pane_background,
            paper_background,
            paper_border,
            paper_shadow,
            scrollbar,
        }
    }
}

impl Default for PdfCanvasStyle {
    fn default() -> Self {
        Self {
            pane_background: gpui::rgb(0xEEF0F3).into(),
            paper_background: gpui::white(),
            paper_border: gpui::rgb(0xCACDD3).into(),
            paper_shadow: gpui::black().opacity(0.15),
            scrollbar: gpui::black().opacity(0.4),
        }
    }
}

/// One already-decoded raster tile and its logical destination geometry.
///
/// Image decoding and cache ownership intentionally stay outside this crate.
#[derive(Clone)]
pub struct PdfCanvasTile {
    pub core_rect: Rect,
    pub render_rect: Rect,
    pub image: Arc<RenderImage>,
}

impl PdfCanvasTile {
    #[must_use]
    pub fn new(core_rect: Rect, render_rect: Rect, image: Arc<RenderImage>) -> Self {
        Self {
            core_rect,
            render_rect,
            image,
        }
    }
}

/// One page submitted to the PDF canvas.
///
/// `overlay` is caller-defined data. It lets search, selection, annotations,
/// links, or future host features paint after the page raster without coupling
/// this reusable component to any product model.
#[derive(Clone)]
pub struct PdfCanvasPage<O> {
    pub page_index: usize,
    pub rect: Rect,
    pub tiles: Vec<PdfCanvasTile>,
    pub overlay: O,
}

impl<O> PdfCanvasPage<O> {
    #[must_use]
    pub fn new(page_index: usize, rect: Rect, tiles: Vec<PdfCanvasTile>, overlay: O) -> Self {
        Self {
            page_index,
            rect,
            tiles,
            overlay,
        }
    }
}

/// Immutable data needed to render one PDF canvas frame.
#[derive(Clone)]
pub struct PdfCanvasSnapshot<O> {
    pub pages: Vec<PdfCanvasPage<O>>,
    pub metrics: PdfCanvasMetrics,
    pub style: PdfCanvasStyle,
    pub limits: PdfCanvasLimits,
}

impl<O> PdfCanvasSnapshot<O> {
    #[must_use]
    pub fn new(
        pages: Vec<PdfCanvasPage<O>>,
        metrics: PdfCanvasMetrics,
        style: PdfCanvasStyle,
    ) -> Self {
        Self {
            pages,
            metrics,
            style,
            limits: PdfCanvasLimits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: PdfCanvasLimits) -> Self {
        self.limits = limits.normalized();
        self
    }
}

/// Bounded context supplied once per admitted page after its raster tiles are
/// painted. Coordinates remain in shared PDF content space, while `page_bounds`
/// and `canvas_bounds` are ready for GPUI overlay painting.
pub struct PdfCanvasPagePaintContext<'a, O> {
    pub page_index: usize,
    pub page_rect: Rect,
    pub page_bounds: Bounds<Pixels>,
    pub canvas_bounds: Bounds<Pixels>,
    pub content_viewport: Rect,
    pub scroll: crate::ScrollOffset,
    pub overlay: &'a O,
}

/// Creates the complete low-level PDF canvas element.
///
/// The component paints the pane, paper, shadow, decoded tiles, clipping, and
/// scrollbars. The callback is invoked at most once per admitted page and only
/// after the page's base content, providing a bounded integration point for
/// selection, annotations, search, and other host-owned overlays.
pub fn pdf_canvas<O>(
    snapshot: PdfCanvasSnapshot<O>,
    mut paint_page_overlay: impl 'static + FnMut(PdfCanvasPagePaintContext<'_, O>, &mut Window),
) -> impl IntoElement + Styled
where
    O: 'static,
{
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            paint_pdf_canvas(snapshot, bounds, window, &mut paint_page_overlay);
        },
    )
}

fn paint_pdf_canvas<O>(
    snapshot: PdfCanvasSnapshot<O>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    paint_page_overlay: &mut impl FnMut(PdfCanvasPagePaintContext<'_, O>, &mut Window),
) {
    let metrics = snapshot.metrics.normalized();
    let limits = snapshot.limits.normalized();
    let content_viewport = metrics.content_viewport();
    let style = snapshot.style;

    window.paint_quad(quad(
        bounds,
        px(0.0),
        style.pane_background,
        px(0.0),
        gpui::transparent_black(),
        Default::default(),
    ));

    let mut remaining_tiles = limits.max_tiles_per_frame;
    for page in snapshot
        .pages
        .into_iter()
        .filter(|page| valid_rect(page.rect))
        .take(limits.max_pages_per_frame)
    {
        let page_bounds =
            relative_rect_to_bounds(bounds, page_viewport_rect(page.rect, metrics.scroll));
        let shadow_bounds = Bounds::new(
            page_bounds.origin + point(px(0.0), px(4.0)),
            page_bounds.size + size(px(0.0), px(8.0)),
        );
        window.paint_quad(quad(
            shadow_bounds,
            px(5.0),
            style.paper_shadow,
            px(0.0),
            gpui::transparent_black(),
            Default::default(),
        ));
        window.paint_quad(quad(
            page_bounds,
            px(2.0),
            style.paper_background,
            px(1.0),
            style.paper_border,
            Default::default(),
        ));

        let tile_count = page.tiles.len().min(remaining_tiles);
        for tile in page.tiles.into_iter().take(tile_count) {
            if !valid_rect(tile.core_rect) || !valid_rect(tile.render_rect) {
                continue;
            }
            let render_bounds = content_rect_to_bounds(bounds, tile.render_rect, metrics.scroll);
            let core_bounds = content_rect_to_bounds(bounds, tile.core_rect, metrics.scroll);
            if !core_bounds.intersects(&page_bounds) || !core_bounds.intersects(&bounds) {
                continue;
            }
            // A tile's bleed is useful for PDFium glyph culling, but neither
            // malformed host geometry nor that bleed may paint outside the
            // physical page or the component viewport.
            let core_bounds = core_bounds.intersect(&page_bounds).intersect(&bounds);
            window.with_content_mask(
                Some(ContentMask {
                    bounds: core_bounds,
                }),
                |window| {
                    let _ =
                        window.paint_image(render_bounds, Corners::default(), tile.image, 0, false);
                },
            );
        }
        remaining_tiles -= tile_count;

        paint_page_overlay(
            PdfCanvasPagePaintContext {
                page_index: page.page_index,
                page_rect: page.rect,
                page_bounds,
                canvas_bounds: bounds,
                content_viewport,
                scroll: metrics.scroll,
                overlay: &page.overlay,
            },
            window,
        );
    }

    let scrollbars = pdf_canvas_scrollbars(metrics);
    if let Some(rect) = scrollbars.vertical {
        window.paint_quad(quad(
            relative_rect_to_bounds(bounds, rect),
            px(2.5),
            style.scrollbar,
            px(0.0),
            gpui::transparent_black(),
            Default::default(),
        ));
    }
    if let Some(rect) = scrollbars.horizontal {
        window.paint_quad(quad(
            relative_rect_to_bounds(bounds, rect),
            px(2.5),
            style.scrollbar,
            px(0.0),
            gpui::transparent_black(),
            Default::default(),
        ));
    }
}

/// Converts a content-space PDF rectangle to GPUI bounds for feature overlays.
#[must_use]
pub fn content_rect_to_bounds(
    canvas: Bounds<Pixels>,
    rect: Rect,
    scroll: crate::ScrollOffset,
) -> Bounds<Pixels> {
    relative_rect_to_bounds(canvas, page_viewport_rect(rect, scroll))
}

fn relative_rect_to_bounds(canvas: Bounds<Pixels>, rect: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(canvas.left() + px(rect.x), canvas.top() + px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}
