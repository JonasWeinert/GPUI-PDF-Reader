use crate::model::{
    PageSize, PdfLink as DocumentLink, PixelRect, RasterSize, TextLayer, TileKey, TocEntry,
};
use crate::scientific::ScientificAnalysis;
use crate::search::{SearchPageResults, SearchQuery};
use key_pdf_runtime::{CancellationSource, ColorMode, PixelColor};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
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

impl RenderAppearance {
    pub(super) fn color_mode(self) -> ColorMode {
        match self {
            Self::Normal => ColorMode::Original,
            Self::ForcedColors {
                background,
                foreground,
            } => ColorMode::Forced {
                background: runtime_color(background),
                foreground: runtime_color(foreground),
            },
        }
    }
}

fn runtime_color(color: RenderColor) -> PixelColor {
    PixelColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: u8::MAX,
    }
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
    Resumed {
        generation: u64,
    },
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

#[derive(Debug)]
pub(super) enum WorkerCommand {
    Open {
        generation: u64,
        path: PathBuf,
        cancellation: CancellationSource,
    },
    SetBackgroundEnabled {
        generation: u64,
        enabled: bool,
    },
    Hibernate {
        generation: u64,
    },
    Resume {
        generation: u64,
    },
    RenderViewport {
        generation: u64,
        requests: Vec<RenderRequest>,
        text_pages: Vec<usize>,
        cancellation: CancellationSource,
    },
    ExtractText {
        generation: u64,
        page: usize,
        cancellation: CancellationSource,
    },
    EnsureTextPages {
        generation: u64,
        pages: Vec<usize>,
        cancellation: CancellationSource,
    },
    CancelExplicitText {
        generation: u64,
    },
    Search {
        generation: u64,
        revision: u64,
        query: SearchQuery,
        cancellation: CancellationSource,
    },
    CancelSearch {
        generation: u64,
        next_revision: u64,
    },
    RenderPreview {
        generation: u64,
        revision: u64,
        appearance: RenderAppearance,
        spec: PreviewSpec,
        cancellation: CancellationSource,
    },
}

#[derive(Clone, Debug)]
pub(super) struct RenderRequest {
    pub(super) generation: u64,
    pub(super) appearance: RenderAppearance,
    pub(super) tile: TileRequest,
    pub(super) priority: usize,
    pub(super) prefetch: bool,
}
