use crate::PdfReaderLimits;
use key_pdf_core::{DocumentLayout, PageSize, PixelRect, RasterSize, Rect, TileKey};
use key_pdf_runtime::{
    ColorMode, DemandError, DemandIntent, DemandPriority, DocumentSession, RenderDemand,
};
use std::collections::BTreeSet;
use std::ops::Range;

/// A tile intersecting the viewport is always ordered before overscan work.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DemandTier {
    Visible,
    Prefetch,
}

/// Engine-neutral tile geometry. A runtime adapter can turn this into a
/// `key_pdf_runtime::RenderDemand` using its active document session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TileRequest {
    pub key: TileKey,
    pub core_rect: PixelRect,
    pub render_rect: PixelRect,
}

impl TileRequest {
    /// Binds engine-neutral viewport geometry to an active runtime session.
    pub fn render_demand(
        self,
        session: &DocumentSession,
        color_mode: ColorMode,
        priority: DemandPriority,
        intent: DemandIntent,
    ) -> Result<RenderDemand, DemandError> {
        session.render_demand(
            self.key,
            self.core_rect,
            self.render_rect,
            color_mode,
            priority,
            intent,
        )
    }
}

/// One prioritized tile request from the viewport planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannedTile {
    pub request: TileRequest,
    pub tier: DemandTier,
    /// Squared distance from the viewport center, in logical pixels.
    pub distance: u64,
}

/// Logical paint rectangles for a rendered tile. Only `core_rect` should be
/// exposed; the larger render rectangle is the PDFium edge-culling gutter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TilePaintGeometry {
    pub core_rect: Rect,
    pub render_rect: Rect,
}

/// Logical viewport and output scale used for one planning pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TilePlanningInput {
    pub viewport: Rect,
    pub scale_factor: f32,
}

impl TilePlanningInput {
    #[must_use]
    pub const fn new(viewport: Rect, scale_factor: f32) -> Self {
        Self {
            viewport,
            scale_factor,
        }
    }
}

impl TilePaintGeometry {
    #[must_use]
    pub fn new(page_rect: Rect, request: TileRequest) -> Self {
        Self {
            core_rect: tile_logical_rect(page_rect, request.key.raster, request.core_rect),
            render_rect: tile_logical_rect(page_rect, request.key.raster, request.render_rect),
        }
    }
}

/// Complete bounded demand plan for one viewport state.
#[derive(Clone, Debug, PartialEq)]
pub struct TileDemandPlan {
    pub visible_pages: Range<usize>,
    pub tiles: Vec<PlannedTile>,
}

impl TileDemandPlan {
    #[must_use]
    pub fn visible_tile_count(&self) -> usize {
        self.tiles
            .iter()
            .take_while(|tile| tile.tier == DemandTier::Visible)
            .count()
    }

    #[must_use]
    pub fn text_pages(&self, maximum: usize) -> Vec<usize> {
        let mut seen = BTreeSet::new();
        self.tiles
            .iter()
            .filter(|tile| tile.tier == DemandTier::Visible)
            .filter_map(|tile| {
                seen.insert(tile.request.key.page)
                    .then_some(tile.request.key.page)
            })
            .take(maximum)
            .collect()
    }
}

/// Calculates a sharp page raster while retaining the reader's 64-pixel
/// quantization and hard 65,536-pixel safety ceiling by default.
#[must_use]
pub fn desired_raster_size(
    page_rect: Rect,
    scale_factor: f32,
    limits: &PdfReaderLimits,
) -> RasterSize {
    let limits = limits.normalized();
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
    let maximum = limits.max_raster_dimension;
    let width_cap = ((f64::from(maximum) / aspect).floor() as u32).clamp(1, maximum);
    let raw_width = (f64::from(page_rect.width) * f64::from(scale_factor)).ceil();
    let raw_width = if raw_width.is_finite() {
        raw_width.clamp(1.0, f64::from(width_cap)) as u32
    } else {
        width_cap
    };
    let width = raw_width
        .div_ceil(limits.render_quantum)
        .saturating_mul(limits.render_quantum)
        .min(width_cap)
        .max(1);
    let height = (aspect * f64::from(width))
        .round()
        .clamp(1.0, f64::from(maximum)) as u32;
    RasterSize { width, height }
}

/// Plans visible and one-tile overscan work. The result is deterministic,
/// deduplicated, visible-first, center-first, and capped by `max_planned_tiles`.
#[must_use]
pub fn plan_visible_tiles(
    layout: &DocumentLayout,
    page_sizes: &[PageSize],
    input: TilePlanningInput,
    limits: &PdfReaderLimits,
) -> TileDemandPlan {
    let limits = limits.normalized();
    if !input.viewport.x.is_finite()
        || !input.viewport.y.is_finite()
        || input.viewport.width <= 0.0
        || input.viewport.height <= 0.0
        || !input.viewport.width.is_finite()
        || !input.viewport.height.is_finite()
    {
        return TileDemandPlan {
            visible_pages: 0..0,
            tiles: Vec::new(),
        };
    }
    let viewport_width = input.viewport.width.min(limits.max_viewport_dimension);
    let viewport_height = input.viewport.height.min(limits.max_viewport_dimension);
    let scale_factor = if input.scale_factor.is_finite() && input.scale_factor > 0.0 {
        input.scale_factor.clamp(0.25, 16.0)
    } else {
        1.0
    };
    let viewport = Rect {
        x: input.viewport.x,
        y: input.viewport.y,
        width: viewport_width,
        height: viewport_height,
    };
    let vertical_overscan = limits.tile_size as f32 / scale_factor;
    let visible_pages = layout.visible_pages(viewport.y, viewport_height, 0.0);
    let planned_pages = layout.visible_pages(viewport.y, viewport_height, vertical_overscan);
    let viewport_center = (
        f64::from(viewport.x + viewport.width * 0.5),
        f64::from(viewport.y + viewport.height * 0.5),
    );
    let mut result = Vec::new();

    for page in planned_pages {
        let (Some(page_rect), Some(_)) = (layout.page_rect(page), page_sizes.get(page)) else {
            continue;
        };
        let raster = desired_raster_size(page_rect, scale_factor, &limits);
        let tile_logical_width = page_rect.width * limits.tile_size as f32 / raster.width as f32;
        let tile_logical_height = page_rect.height * limits.tile_size as f32 / raster.height as f32;
        let expanded = Rect {
            x: viewport.x - tile_logical_width,
            y: viewport.y - tile_logical_height,
            width: viewport.width + tile_logical_width * 2.0,
            height: viewport.height + tile_logical_height * 2.0,
        };
        let Some(intersection) = intersect_rect(page_rect, expanded) else {
            continue;
        };

        let left = logical_to_pixel_x(intersection.x, page_rect, raster, false);
        let right = logical_to_pixel_x(intersection.right(), page_rect, raster, true);
        let top = logical_to_pixel_y(intersection.y, page_rect, raster, false);
        let bottom = logical_to_pixel_y(intersection.bottom(), page_rect, raster, true);
        if left >= right || top >= bottom {
            continue;
        }
        let first_column = left / limits.tile_size;
        let last_column = (right - 1) / limits.tile_size;
        let first_row = top / limits.tile_size;
        let last_row = (bottom - 1) / limits.tile_size;

        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let key = TileKey {
                    page,
                    raster,
                    column,
                    row,
                };
                let Some(core_rect) = tile_core_rect_with_size(key, limits.tile_size) else {
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
                        render_rect: inflate_tile_rect_with_limits(core_rect, raster, &limits),
                    },
                    tier,
                    distance,
                });
            }
        }
    }
    result.sort_by_key(|tile| (tile.tier, tile.distance, tile.request.key));
    result.dedup_by_key(|tile| tile.request.key);
    result.truncate(limits.max_planned_tiles);
    TileDemandPlan {
        visible_pages,
        tiles: result,
    }
}

#[must_use]
pub fn tile_core_rect(key: TileKey, limits: &PdfReaderLimits) -> Option<PixelRect> {
    tile_core_rect_with_size(key, limits.normalized().tile_size)
}

fn tile_core_rect_with_size(key: TileKey, tile_size: u32) -> Option<PixelRect> {
    let x = key.column.checked_mul(tile_size)?;
    let y = key.row.checked_mul(tile_size)?;
    if x >= key.raster.width || y >= key.raster.height {
        return None;
    }
    Some(PixelRect {
        x,
        y,
        width: tile_size.min(key.raster.width - x),
        height: tile_size.min(key.raster.height - y),
    })
}

#[must_use]
pub fn inflate_tile_rect(
    core: PixelRect,
    raster: RasterSize,
    limits: &PdfReaderLimits,
) -> PixelRect {
    inflate_tile_rect_with_limits(core, raster, &limits.normalized())
}

fn inflate_tile_rect_with_limits(
    core: PixelRect,
    raster: RasterSize,
    limits: &PdfReaderLimits,
) -> PixelRect {
    let x = core.x.saturating_sub(limits.tile_bleed);
    let y = core.y.saturating_sub(limits.tile_bleed);
    let right = core
        .x
        .saturating_add(core.width)
        .saturating_add(limits.tile_bleed)
        .min(raster.width);
    let bottom = core
        .y
        .saturating_add(core.height)
        .saturating_add(limits.tile_bleed)
        .min(raster.height);
    PixelRect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

#[must_use]
pub fn tile_logical_rect(page: Rect, raster: RasterSize, pixels: PixelRect) -> Rect {
    if raster.width == 0 || raster.height == 0 {
        return Rect::default();
    }
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

fn logical_to_pixel_x(value: f32, page: Rect, raster: RasterSize, round_up: bool) -> u32 {
    let value =
        ((value - page.x) / page.width * raster.width as f32).clamp(0.0, raster.width as f32);
    (if round_up {
        value.ceil()
    } else {
        value.floor()
    }) as u32
}

fn logical_to_pixel_y(value: f32, page: Rect, raster: RasterSize, round_up: bool) -> u32 {
    let value =
        ((value - page.y) / page.height * raster.height as f32).clamp(0.0, raster.height as f32);
    (if round_up {
        value.ceil()
    } else {
        value.floor()
    }) as u32
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
