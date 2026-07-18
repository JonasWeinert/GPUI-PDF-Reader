use crate::ScrollOffset;
use key_pdf_core::Rect;

/// Default ceiling for pages submitted to one painted PDF frame.
///
/// Hosts normally submit only visible pages. This independent ceiling makes a
/// malformed or stale host snapshot safe to embed without turning one frame
/// into unbounded work.
pub const DEFAULT_MAX_CANVAS_PAGES: usize = 512;

/// Default ceiling for raster tiles submitted to one painted PDF frame.
pub const DEFAULT_MAX_CANVAS_TILES: usize = 4_096;

const ABSOLUTE_MAX_CANVAS_PAGES: usize = 4_096;
const ABSOLUTE_MAX_CANVAS_TILES: usize = 65_536;

/// Resource ceilings for the reusable canvas renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfCanvasLimits {
    pub max_pages_per_frame: usize,
    pub max_tiles_per_frame: usize,
}

impl Default for PdfCanvasLimits {
    fn default() -> Self {
        Self {
            max_pages_per_frame: DEFAULT_MAX_CANVAS_PAGES,
            max_tiles_per_frame: DEFAULT_MAX_CANVAS_TILES,
        }
    }
}

impl PdfCanvasLimits {
    /// Clamps caller-provided limits to non-zero hard safety ceilings.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            max_pages_per_frame: self.max_pages_per_frame.clamp(1, ABSOLUTE_MAX_CANVAS_PAGES),
            max_tiles_per_frame: self.max_tiles_per_frame.clamp(1, ABSOLUTE_MAX_CANVAS_TILES),
        }
    }
}

/// Product-neutral geometry needed to paint one PDF viewport frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfCanvasMetrics {
    pub scroll: ScrollOffset,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub content_height: f32,
    pub max_scroll_x: f32,
}

impl PdfCanvasMetrics {
    #[must_use]
    pub fn new(
        scroll: ScrollOffset,
        viewport_width: f32,
        viewport_height: f32,
        content_height: f32,
        max_scroll_x: f32,
    ) -> Self {
        Self {
            scroll,
            viewport_width,
            viewport_height,
            content_height,
            max_scroll_x,
        }
    }

    /// Returns finite, non-negative geometry suitable for a paint pass.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            scroll: ScrollOffset::new(
                finite_non_negative(self.scroll.x),
                finite_non_negative(self.scroll.y),
            ),
            viewport_width: finite_positive(self.viewport_width),
            viewport_height: finite_positive(self.viewport_height),
            content_height: finite_positive(self.content_height),
            max_scroll_x: finite_non_negative(self.max_scroll_x),
        }
    }

    #[must_use]
    pub fn content_viewport(self) -> Rect {
        let metrics = self.normalized();
        Rect {
            x: metrics.scroll.x,
            y: metrics.scroll.y,
            width: metrics.viewport_width,
            height: metrics.viewport_height,
        }
    }
}

/// Geometry-only page input used by the public frame planner and harnesses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfCanvasPageGeometry {
    pub page_index: usize,
    pub rect: Rect,
    pub tile_count: usize,
}

/// A page admitted by the bounded frame planner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlannedCanvasPage {
    /// Position in the caller's page slice. This deliberately does not assume
    /// pages are ordered by PDF page number.
    pub source_index: usize,
    pub page_index: usize,
    /// Page rectangle relative to the viewport's top-left origin.
    pub viewport_rect: Rect,
    pub tile_count: usize,
}

/// Scrollbar rectangles relative to the viewport's top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PdfCanvasScrollbars {
    pub vertical: Option<Rect>,
    pub horizontal: Option<Rect>,
}

/// Bounded, allocation-light frame plan suitable for component harnesses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfCanvasFramePlan {
    pub pages: Vec<PlannedCanvasPage>,
    pub scrollbars: PdfCanvasScrollbars,
    pub admitted_tiles: usize,
    pub dropped_pages: usize,
    pub dropped_tiles: usize,
}

/// Plans the same page and scrollbar geometry used by the GPUI renderer.
///
/// Invalid page rectangles are omitted. Page and tile work is bounded even if
/// a host accidentally submits a complete, very large document rather than a
/// visible-page window.
#[must_use]
pub fn plan_pdf_canvas_frame(
    pages: &[PdfCanvasPageGeometry],
    metrics: PdfCanvasMetrics,
    limits: PdfCanvasLimits,
) -> PdfCanvasFramePlan {
    let metrics = metrics.normalized();
    let limits = limits.normalized();
    let mut result = PdfCanvasFramePlan {
        pages: Vec::with_capacity(pages.len().min(limits.max_pages_per_frame)),
        scrollbars: pdf_canvas_scrollbars(metrics),
        ..PdfCanvasFramePlan::default()
    };
    let mut remaining_tiles = limits.max_tiles_per_frame;

    for (source_index, page) in pages.iter().enumerate() {
        if result.pages.len() == limits.max_pages_per_frame {
            result.dropped_pages = pages.len().saturating_sub(source_index);
            result.dropped_tiles = result.dropped_tiles.saturating_add(
                pages[source_index..]
                    .iter()
                    .fold(0usize, |total, page| total.saturating_add(page.tile_count)),
            );
            break;
        }
        if !valid_rect(page.rect) {
            result.dropped_pages = result.dropped_pages.saturating_add(1);
            result.dropped_tiles = result.dropped_tiles.saturating_add(page.tile_count);
            continue;
        }
        let admitted = page.tile_count.min(remaining_tiles);
        remaining_tiles -= admitted;
        result.admitted_tiles = result.admitted_tiles.saturating_add(admitted);
        result.dropped_tiles = result
            .dropped_tiles
            .saturating_add(page.tile_count - admitted);
        result.pages.push(PlannedCanvasPage {
            source_index,
            page_index: page.page_index,
            viewport_rect: page_viewport_rect(page.rect, metrics.scroll),
            tile_count: admitted,
        });
    }

    result
}

#[must_use]
pub fn page_viewport_rect(page: Rect, scroll: ScrollOffset) -> Rect {
    Rect {
        x: page.x - scroll.x,
        y: page.y - scroll.y,
        width: page.width,
        height: page.height,
    }
}

/// Computes the slim overlay scrollbars used by the reusable PDF canvas.
#[must_use]
pub fn pdf_canvas_scrollbars(metrics: PdfCanvasMetrics) -> PdfCanvasScrollbars {
    let metrics = metrics.normalized();
    let max_y = (metrics.content_height - metrics.viewport_height).max(0.0);
    let vertical = (max_y > 0.0).then(|| {
        let thumb_height = (metrics.viewport_height * metrics.viewport_height
            / metrics.content_height)
            .max(38.0)
            .min(metrics.viewport_height);
        let travel = (metrics.viewport_height - thumb_height - 8.0).max(0.0);
        let y = 4.0 + travel * (metrics.scroll.y / max_y).clamp(0.0, 1.0);
        Rect {
            x: metrics.viewport_width - 8.0,
            y,
            width: 5.0,
            height: thumb_height,
        }
    });

    let horizontal = (metrics.max_scroll_x > 0.0).then(|| {
        let effective_content_width = metrics.viewport_width + metrics.max_scroll_x;
        let thumb_width = (metrics.viewport_width * metrics.viewport_width
            / effective_content_width)
            .max(38.0)
            .min(metrics.viewport_width);
        let travel = (metrics.viewport_width - thumb_width - 12.0).max(0.0);
        let x = 4.0 + travel * (metrics.scroll.x / metrics.max_scroll_x).clamp(0.0, 1.0);
        Rect {
            x,
            y: metrics.viewport_height - 8.0,
            width: thumb_width,
            height: 5.0,
        }
    });

    PdfCanvasScrollbars {
        vertical,
        horizontal,
    }
}

pub(crate) fn valid_rect(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_positive(value: f32) -> f32 {
    if value.is_finite() {
        value.max(1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_planning_is_bounded_and_keeps_caller_order() {
        let pages = [
            PdfCanvasPageGeometry {
                page_index: 8,
                rect: Rect {
                    x: 20.0,
                    y: 40.0,
                    width: 400.0,
                    height: 600.0,
                },
                tile_count: 3,
            },
            PdfCanvasPageGeometry {
                page_index: 2,
                rect: Rect {
                    x: 20.0,
                    y: 664.0,
                    width: 400.0,
                    height: 600.0,
                },
                tile_count: 4,
            },
            PdfCanvasPageGeometry {
                page_index: 4,
                rect: Rect {
                    x: 20.0,
                    y: 1_288.0,
                    width: 400.0,
                    height: 600.0,
                },
                tile_count: 5,
            },
        ];
        let plan = plan_pdf_canvas_frame(
            &pages,
            PdfCanvasMetrics::new(ScrollOffset::new(5.0, 100.0), 500.0, 700.0, 2_000.0, 20.0),
            PdfCanvasLimits {
                max_pages_per_frame: 2,
                max_tiles_per_frame: 5,
            },
        );
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(plan.pages[0].page_index, 8);
        assert_eq!(plan.pages[0].viewport_rect.x, 15.0);
        assert_eq!(plan.pages[0].viewport_rect.y, -60.0);
        assert_eq!(plan.pages[0].tile_count, 3);
        assert_eq!(plan.pages[1].page_index, 2);
        assert_eq!(plan.pages[1].tile_count, 2);
        assert_eq!(plan.admitted_tiles, 5);
        assert_eq!(plan.dropped_pages, 1);
        assert_eq!(plan.dropped_tiles, 7);
    }

    #[test]
    fn malformed_geometry_is_dropped_and_metrics_are_finite() {
        let pages = [PdfCanvasPageGeometry {
            page_index: 0,
            rect: Rect {
                x: f32::NAN,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            tile_count: usize::MAX,
        }];
        let plan = plan_pdf_canvas_frame(
            &pages,
            PdfCanvasMetrics::new(
                ScrollOffset::new(f32::NAN, f32::INFINITY),
                f32::NAN,
                -1.0,
                f32::INFINITY,
                f32::NAN,
            ),
            PdfCanvasLimits::default(),
        );
        assert!(plan.pages.is_empty());
        assert_eq!(plan.dropped_pages, 1);
        assert_eq!(plan.dropped_tiles, usize::MAX);
        assert_eq!(plan.scrollbars, PdfCanvasScrollbars::default());
    }

    #[test]
    fn scrollbar_geometry_matches_viewport_contract() {
        let metrics = PdfCanvasMetrics::new(
            ScrollOffset::new(300.0, 600.0),
            1_000.0,
            800.0,
            2_400.0,
            1_000.0,
        );
        let bars = pdf_canvas_scrollbars(metrics);
        let vertical = bars.vertical.unwrap();
        let horizontal = bars.horizontal.unwrap();
        assert_eq!(vertical.x, 992.0);
        assert_eq!(vertical.width, 5.0);
        assert!(vertical.y >= 4.0 && vertical.bottom() <= 800.0);
        assert_eq!(horizontal.y, 792.0);
        assert_eq!(horizontal.height, 5.0);
        assert!(horizontal.x >= 4.0 && horizontal.right() <= 1_000.0);
    }
}
