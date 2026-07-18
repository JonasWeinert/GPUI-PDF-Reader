use crate::{AllocationError, DocumentGeneration, DocumentSession, RequestId};
use key_pdf_core::{PixelRect, RasterSize, TileKey};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DemandPriority(u16);

impl DemandPriority {
    pub const BACKGROUND: Self = Self(0);
    pub const PREFETCH: Self = Self(64);
    pub const VISIBLE: Self = Self(128);
    pub const INTERACTIVE: Self = Self(u16::MAX);

    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for DemandPriority {
    fn default() -> Self {
        Self::VISIBLE
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum DemandIntent {
    #[default]
    Visible,
    Interactive,
    Prefetch,
    Explicit,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DemandStamp {
    generation: DocumentGeneration,
    request: RequestId,
    priority: DemandPriority,
    intent: DemandIntent,
}

impl DemandStamp {
    pub fn generation(self) -> DocumentGeneration {
        self.generation
    }

    pub fn request(self) -> RequestId {
        self.request
    }

    pub fn priority(self) -> DemandPriority {
        self.priority
    }

    pub fn intent(self) -> DemandIntent {
        self.intent
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PixelColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ColorMode {
    #[default]
    Original,
    Forced {
        background: PixelColor,
        foreground: PixelColor,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RenderDemandKey {
    pub tile: TileKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderDemand {
    stamp: DemandStamp,
    key: TileKey,
    core_rect: PixelRect,
    render_rect: PixelRect,
    color_mode: ColorMode,
}

impl RenderDemand {
    pub fn stamp(&self) -> DemandStamp {
        self.stamp
    }

    pub fn key(&self) -> TileKey {
        self.key
    }

    pub fn deduplication_key(&self) -> RenderDemandKey {
        RenderDemandKey { tile: self.key }
    }

    pub fn page(&self) -> usize {
        self.key.page
    }

    pub fn raster(&self) -> RasterSize {
        self.key.raster
    }

    pub fn core_rect(&self) -> PixelRect {
        self.core_rect
    }

    pub fn render_rect(&self) -> PixelRect {
        self.render_rect
    }

    pub fn color_mode(&self) -> ColorMode {
        self.color_mode
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TextDemandPurpose {
    #[default]
    VisibleLayer,
    Copy,
    Search,
    LinkResolution,
    DocumentAnalysis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextDemand {
    stamp: DemandStamp,
    page: usize,
    purpose: TextDemandPurpose,
}

impl TextDemand {
    pub fn stamp(&self) -> DemandStamp {
        self.stamp
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn purpose(&self) -> TextDemandPurpose {
        self.purpose
    }

    pub fn deduplication_key(&self) -> usize {
        self.page
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewDemand {
    stamp: DemandStamp,
    page: usize,
    raster: RasterSize,
    region: PixelRect,
    color_mode: ColorMode,
}

impl PreviewDemand {
    pub fn stamp(&self) -> DemandStamp {
        self.stamp
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn raster(&self) -> RasterSize {
        self.raster
    }

    pub fn region(&self) -> PixelRect {
        self.region
    }

    pub fn color_mode(&self) -> ColorMode {
        self.color_mode
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemandError {
    Allocation(AllocationError),
    EmptyRaster,
    EmptyRegion,
    RegionOutsideRaster,
    CoreOutsideRenderRegion,
    NonFiniteCoordinate,
}

impl fmt::Display for DemandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation(error) => error.fmt(formatter),
            Self::EmptyRaster => formatter.write_str("render raster must be non-empty"),
            Self::EmptyRegion => formatter.write_str("render region must be non-empty"),
            Self::RegionOutsideRaster => formatter.write_str("render region lies outside raster"),
            Self::CoreOutsideRenderRegion => {
                formatter.write_str("tile core must be contained by its render region")
            }
            Self::NonFiniteCoordinate => formatter.write_str("preview center must be finite"),
        }
    }
}

impl std::error::Error for DemandError {}

impl From<AllocationError> for DemandError {
    fn from(value: AllocationError) -> Self {
        Self::Allocation(value)
    }
}

impl DocumentSession {
    pub fn render_demand(
        &self,
        key: TileKey,
        core_rect: PixelRect,
        render_rect: PixelRect,
        color_mode: ColorMode,
        priority: DemandPriority,
        intent: DemandIntent,
    ) -> Result<RenderDemand, DemandError> {
        validate_raster(key.raster)?;
        validate_region(key.raster, core_rect)?;
        validate_region(key.raster, render_rect)?;
        if !contains_rect(render_rect, core_rect) {
            return Err(DemandError::CoreOutsideRenderRegion);
        }
        Ok(RenderDemand {
            stamp: self.stamp(priority, intent)?,
            key,
            core_rect,
            render_rect,
            color_mode,
        })
    }

    pub fn text_demand(
        &self,
        page: usize,
        purpose: TextDemandPurpose,
        priority: DemandPriority,
        intent: DemandIntent,
    ) -> Result<TextDemand, DemandError> {
        Ok(TextDemand {
            stamp: self.stamp(priority, intent)?,
            page,
            purpose,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn preview_demand(
        &self,
        page: usize,
        raster: RasterSize,
        output: RasterSize,
        center_x: f32,
        center_y: f32,
        color_mode: ColorMode,
        priority: DemandPriority,
    ) -> Result<PreviewDemand, DemandError> {
        validate_raster(raster)?;
        validate_raster(output)?;
        if !center_x.is_finite() || !center_y.is_finite() {
            return Err(DemandError::NonFiniteCoordinate);
        }
        let width = output.width.min(raster.width);
        let height = output.height.min(raster.height);
        let center_x = (center_x.clamp(0.0, 1.0) * raster.width as f32).round() as u32;
        let center_y = (center_y.clamp(0.0, 1.0) * raster.height as f32).round() as u32;
        let region = PixelRect {
            x: center_x.saturating_sub(width / 2).min(raster.width - width),
            y: center_y
                .saturating_sub(height / 2)
                .min(raster.height - height),
            width,
            height,
        };
        Ok(PreviewDemand {
            stamp: self.stamp(priority, DemandIntent::Interactive)?,
            page,
            raster,
            region,
            color_mode,
        })
    }

    fn stamp(
        &self,
        priority: DemandPriority,
        intent: DemandIntent,
    ) -> Result<DemandStamp, DemandError> {
        Ok(DemandStamp {
            generation: self.generation(),
            request: self.next_request_id()?,
            priority,
            intent,
        })
    }
}

fn validate_raster(raster: RasterSize) -> Result<(), DemandError> {
    if raster.width == 0 || raster.height == 0 {
        Err(DemandError::EmptyRaster)
    } else {
        Ok(())
    }
}

fn validate_region(raster: RasterSize, region: PixelRect) -> Result<(), DemandError> {
    if region.width == 0 || region.height == 0 {
        return Err(DemandError::EmptyRegion);
    }
    let Some(right) = region.x.checked_add(region.width) else {
        return Err(DemandError::RegionOutsideRaster);
    };
    let Some(bottom) = region.y.checked_add(region.height) else {
        return Err(DemandError::RegionOutsideRaster);
    };
    if right > raster.width || bottom > raster.height {
        Err(DemandError::RegionOutsideRaster)
    } else {
        Ok(())
    }
}

fn contains_rect(outer: PixelRect, inner: PixelRect) -> bool {
    let outer_right = outer.x.saturating_add(outer.width);
    let outer_bottom = outer.y.saturating_add(outer.height);
    let inner_right = inner.x.saturating_add(inner.width);
    let inner_bottom = inner.y.saturating_add(inner.height);
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner_right <= outer_right
        && inner_bottom <= outer_bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentSessionManager;

    fn session() -> DocumentSession {
        DocumentSessionManager::new().begin().unwrap()
    }

    #[test]
    fn render_demands_validate_regions_before_allocating_work() {
        let session = session();
        let key = TileKey {
            page: 2,
            raster: RasterSize {
                width: 1_000,
                height: 1_500,
            },
            column: 0,
            row: 0,
        };
        let core = PixelRect {
            x: 100,
            y: 200,
            width: 300,
            height: 400,
        };
        let render = PixelRect {
            x: 90,
            y: 190,
            width: 320,
            height: 420,
        };
        let demand = session
            .render_demand(
                key,
                core,
                render,
                ColorMode::Original,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        assert_eq!(demand.page(), 2);
        assert_eq!(demand.core_rect(), core);

        let invalid = PixelRect { x: 950, ..core };
        assert_eq!(
            session.render_demand(
                key,
                invalid,
                render,
                ColorMode::Original,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            ),
            Err(DemandError::RegionOutsideRaster)
        );
    }

    #[test]
    fn preview_crop_is_bounded_and_clamped_at_edges() {
        let demand = session()
            .preview_demand(
                1,
                RasterSize {
                    width: 1_000,
                    height: 700,
                },
                RasterSize {
                    width: 360,
                    height: 204,
                },
                1.0,
                0.0,
                ColorMode::Original,
                DemandPriority::INTERACTIVE,
            )
            .unwrap();
        assert_eq!(demand.region().x, 640);
        assert_eq!(demand.region().y, 0);
        assert_eq!(demand.region().width, 360);
        assert_eq!(demand.region().height, 204);
    }
}
