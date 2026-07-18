//! PDFium implementation of the engine-independent Key PDF runtime.
//!
//! PDFium bindings and documents never cross this crate boundary. The engine
//! is deliberately `!Send`: construct and use it on the single worker thread
//! that owns PDFium work.

mod ops;

use key_pdf_core::TextLayer;
use key_pdf_runtime::{
    CancellationToken, DocumentDescriptor, EngineCapabilities, EngineDocument, PdfEngine,
    PixelFormat, PreviewDemand, RasterImage, RenderDemand, TextDemand,
};
use pdfium_render::prelude::{PdfDocument, PdfDocumentMetadataTagType, Pdfium, PdfiumError};
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    rc::Rc,
    thread::{self, ThreadId},
};

pub use ops::{MAX_PAGE_TEXT_CHARS, MAX_RASTER_DIMENSION, MAX_TILE_DIMENSION};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfiumLibraryConfig {
    candidates: Vec<PathBuf>,
    system_fallback: bool,
}

impl PdfiumLibraryConfig {
    /// Creates an injected candidate list. A directory candidate is resolved
    /// against PDFium's platform library filename at load time.
    pub fn new(candidates: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
            system_fallback: false,
        }
    }

    pub fn with_system_fallback(mut self, enabled: bool) -> Self {
        self.system_fallback = enabled;
        self
    }

    pub fn push_candidate(&mut self, candidate: PathBuf) {
        self.candidates.push(candidate);
    }

    pub fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }

    pub fn uses_system_fallback(&self) -> bool {
        self.system_fallback
    }

    fn resolved_candidates(&self) -> Vec<PathBuf> {
        let library_name = Pdfium::pdfium_platform_library_name();
        self.candidates
            .iter()
            .map(|candidate| {
                if candidate.is_dir() {
                    candidate.join(&library_name)
                } else {
                    candidate.clone()
                }
            })
            .collect()
    }
}

impl Default for PdfiumLibraryConfig {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            system_fallback: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PdfiumDocumentSource {
    File {
        path: PathBuf,
        password: Option<String>,
    },
    Bytes {
        bytes: Vec<u8>,
        password: Option<String>,
    },
}

impl PdfiumDocumentSource {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File {
            path: path.into(),
            password: None,
        }
    }

    pub fn bytes(bytes: Vec<u8>) -> Self {
        Self::Bytes {
            bytes,
            password: None,
        }
    }

    pub fn with_password(self, password: impl Into<String>) -> Self {
        let password = Some(password.into());
        match self {
            Self::File { path, .. } => Self::File { path, password },
            Self::Bytes { bytes, .. } => Self::Bytes { bytes, password },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PdfiumEngineError {
    WrongThread,
    Cancelled,
    LibraryLoad(String),
    Open(String),
    Operation(String),
}

impl fmt::Display for PdfiumEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongThread => formatter.write_str("PDFium engine used from a non-owner thread"),
            Self::Cancelled => formatter.write_str("PDFium operation cancelled"),
            Self::LibraryLoad(message) => write!(formatter, "could not load PDFium: {message}"),
            Self::Open(message) => write!(formatter, "could not open PDF: {message}"),
            Self::Operation(message) => write!(formatter, "PDFium operation failed: {message}"),
        }
    }
}

impl std::error::Error for PdfiumEngineError {}

pub struct PdfiumEngine {
    config: PdfiumLibraryConfig,
    pdfium: Option<&'static Pdfium>,
    owner: ThreadId,
    not_send: Rc<()>,
}

impl PdfiumEngine {
    pub fn new(config: PdfiumLibraryConfig) -> Self {
        Self {
            config,
            pdfium: None,
            owner: thread::current().id(),
            not_send: Rc::new(()),
        }
    }

    pub fn config(&self) -> &PdfiumLibraryConfig {
        &self.config
    }

    pub fn is_initialized(&self) -> bool {
        self.pdfium.is_some()
    }

    fn ensure_owner(&self) -> Result<(), PdfiumEngineError> {
        if thread::current().id() == self.owner {
            Ok(())
        } else {
            Err(PdfiumEngineError::WrongThread)
        }
    }

    fn pdfium(&mut self) -> Result<&'static Pdfium, PdfiumEngineError> {
        self.ensure_owner()?;
        if let Some(pdfium) = self.pdfium {
            return Ok(pdfium);
        }

        let mut failures = Vec::new();
        for candidate in self.config.resolved_candidates() {
            if !candidate.exists() {
                failures.push(format!("{} (not found)", candidate.display()));
                continue;
            }
            match Pdfium::bind_to_library(&candidate) {
                Ok(bindings) => {
                    let pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
                    self.pdfium = Some(pdfium);
                    return Ok(pdfium);
                }
                Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                    let pdfium = Box::leak(Box::new(Pdfium::default()));
                    self.pdfium = Some(pdfium);
                    return Ok(pdfium);
                }
                Err(error) => failures.push(format!("{} ({error})", candidate.display())),
            }
        }

        if self.config.system_fallback {
            match Pdfium::bind_to_system_library() {
                Ok(bindings) => {
                    let pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
                    self.pdfium = Some(pdfium);
                    return Ok(pdfium);
                }
                Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                    let pdfium = Box::leak(Box::new(Pdfium::default()));
                    self.pdfium = Some(pdfium);
                    return Ok(pdfium);
                }
                Err(error) => failures.push(format!("system lookup ({error})")),
            }
        }

        let detail = if failures.is_empty() {
            "no library candidates were configured".to_owned()
        } else {
            failures.join(", ")
        };
        Err(PdfiumEngineError::LibraryLoad(detail))
    }
}

impl fmt::Debug for PdfiumEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PdfiumEngine")
            .field("config", &self.config)
            .field("initialized", &self.is_initialized())
            .field("owner", &self.owner)
            .field("thread_marker_refs", &Rc::strong_count(&self.not_send))
            .finish()
    }
}

pub struct PdfiumEngineDocument {
    document: PdfDocument<'static>,
    descriptor: DocumentDescriptor,
    partial_text: HashMap<usize, Vec<key_pdf_core::TextChar>>,
    owner: ThreadId,
    not_send: Rc<()>,
}

impl PdfiumEngineDocument {
    fn ensure_owner(&self) -> Result<(), PdfiumEngineError> {
        if thread::current().id() == self.owner {
            Ok(())
        } else {
            Err(PdfiumEngineError::WrongThread)
        }
    }
}

impl PdfEngine for PdfiumEngine {
    type Source = PdfiumDocumentSource;
    type Document = PdfiumEngineDocument;
    type Error = PdfiumEngineError;

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            text: true,
            previews: true,
            outlines: true,
            links: true,
            forced_colors: true,
        }
    }

    fn open(
        &mut self,
        source: Self::Source,
        cancellation: &CancellationToken,
    ) -> Result<Self::Document, Self::Error> {
        self.ensure_owner()?;
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let pdfium = self.pdfium()?;
        let document: PdfDocument<'static> = match source {
            PdfiumDocumentSource::File { path, password } => pdfium
                .load_pdf_from_file(&path, password.as_deref())
                .map_err(|error| PdfiumEngineError::Open(error.to_string()))?,
            PdfiumDocumentSource::Bytes { bytes, password } => pdfium
                .load_pdf_from_byte_vec(bytes, password.as_deref())
                .map_err(|error| PdfiumEngineError::Open(error.to_string()))?,
        };
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let pages = ops::page_sizes(&document).map_err(PdfiumEngineError::Operation)?;
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let table_of_contents = ops::extract_table_of_contents(&document, &pages);
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let links = ops::extract_document_links(&document, &pages);
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let title = document
            .metadata()
            .get(PdfDocumentMetadataTagType::Title)
            .map(|tag| tag.value().trim().to_owned())
            .filter(|title| !title.is_empty());
        Ok(PdfiumEngineDocument {
            document,
            descriptor: DocumentDescriptor::new(pages, table_of_contents, links).with_title(title),
            partial_text: HashMap::new(),
            owner: self.owner,
            not_send: self.not_send.clone(),
        })
    }
}

impl EngineDocument for PdfiumEngineDocument {
    type Error = PdfiumEngineError;

    fn descriptor(&self) -> &DocumentDescriptor {
        &self.descriptor
    }

    fn render(
        &mut self,
        demand: &RenderDemand,
        cancellation: &CancellationToken,
    ) -> Result<RasterImage, Self::Error> {
        self.ensure_owner()?;
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let tile = ops::TileRequest {
            key: demand.key(),
            core_rect: demand.core_rect(),
            render_rect: demand.render_rect(),
        };
        let (width, height, pixels) =
            match ops::render_tile(&self.document, tile, demand.color_mode(), || {
                cancellation.is_cancelled()
            })
            .map_err(PdfiumEngineError::Operation)?
            {
                ops::RenderExtraction::Complete(output) => output,
                ops::RenderExtraction::Cancelled => return Err(PdfiumEngineError::Cancelled),
            };
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        raster_image(width, height, pixels)
    }

    fn extract_text(
        &mut self,
        demand: &TextDemand,
        cancellation: &CancellationToken,
    ) -> Result<TextLayer, Self::Error> {
        self.ensure_owner()?;
        let partial = self.partial_text.remove(&demand.page()).unwrap_or_default();
        let extraction = ops::extract_page_text(&self.document, demand.page(), partial, || {
            cancellation.is_cancelled()
        })
        .map_err(PdfiumEngineError::Operation)?;
        match extraction {
            ops::TextExtraction::Complete(characters) => Ok(TextLayer::new(characters)),
            ops::TextExtraction::Cancelled(partial) => {
                self.partial_text.insert(demand.page(), partial);
                Err(PdfiumEngineError::Cancelled)
            }
        }
    }

    fn render_preview(
        &mut self,
        demand: &PreviewDemand,
        cancellation: &CancellationToken,
    ) -> Result<RasterImage, Self::Error> {
        self.ensure_owner()?;
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        let region = demand.region();
        let tile = ops::TileRequest {
            key: key_pdf_core::TileKey {
                page: demand.page(),
                raster: demand.raster(),
                column: region.x,
                row: region.y,
            },
            core_rect: region,
            render_rect: region,
        };
        let (width, height, pixels) =
            match ops::render_tile(&self.document, tile, demand.color_mode(), || {
                cancellation.is_cancelled()
            })
            .map_err(PdfiumEngineError::Operation)?
            {
                ops::RenderExtraction::Complete(output) => output,
                ops::RenderExtraction::Cancelled => return Err(PdfiumEngineError::Cancelled),
            };
        cancellation
            .checkpoint()
            .map_err(|_| PdfiumEngineError::Cancelled)?;
        raster_image(width, height, pixels)
    }
}

impl fmt::Debug for PdfiumEngineDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PdfiumEngineDocument")
            .field("descriptor", &self.descriptor)
            .field("owner", &self.owner)
            .field("thread_marker_refs", &Rc::strong_count(&self.not_send))
            .finish()
    }
}

fn raster_image(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
) -> Result<RasterImage, PdfiumEngineError> {
    let stride = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| PdfiumEngineError::Operation("raster stride overflowed".to_owned()))?;
    RasterImage::new(
        width,
        height,
        stride,
        PixelFormat::Bgra8Premultiplied,
        pixels,
    )
    .map_err(|error| PdfiumEngineError::Operation(error.to_string()))
}

/// Resolves a test or application-supplied candidate path without assuming a
/// repository layout in production code.
pub fn library_candidate(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use key_pdf_core::{PixelRect, RasterSize, TileKey};
    use key_pdf_runtime::{
        CachePolicy, CancellationSource, ColorMode, DemandIntent, DemandPriority, DocumentEvent,
        DocumentSessionManager, PdfRuntime, PreviewEvent, RenderEvent, TextDemandPurpose,
        TextEvent,
    };
    use std::{env, sync::Mutex};

    static PDFIUM_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist")
    }

    fn test_library() -> PathBuf {
        env::var_os("KEY_PDFIUM_TEST_LIBRARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| repository_root().join("vendor/pdfium/lib"))
    }

    fn test_fixture() -> PathBuf {
        env::var_os("KEY_PDFIUM_TEST_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| repository_root().join("tests/fixtures/interaction.pdf"))
    }

    #[test]
    fn injected_directories_resolve_to_the_platform_library_name() {
        let directory = test_library();
        let config = PdfiumLibraryConfig::new([directory.clone()]);
        let resolved = config.resolved_candidates();
        assert_eq!(resolved.len(), 1);
        if directory.is_dir() {
            assert_eq!(
                resolved[0].file_name(),
                Some(Pdfium::pdfium_platform_library_name().as_ref())
            );
        }
        assert!(!config.uses_system_fallback());
    }

    #[test]
    fn bounds_and_url_validation_reject_malformed_engine_input() {
        assert_eq!(
            ops::validate_text_character_count(-1),
            Err("PDFium returned a negative character count".to_owned())
        );
        assert!(ops::validate_text_character_count(MAX_PAGE_TEXT_CHARS as i32 + 1).is_err());
        assert_eq!(ops::validated_link_url("javascript:alert(1)"), None);
        assert_eq!(
            ops::validated_link_url("https://example.com/a b"),
            Some("https://example.com/a%20b".to_owned())
        );
        assert_eq!(
            ops::normalized_text_bounds([(0, 0), (10, 0), (0, 20), (10, 20)], 100, 200),
            Some(key_pdf_core::TextBounds {
                left: 0.0,
                top: 0.0,
                right: 0.1,
                bottom: 0.1,
            })
        );
    }

    #[test]
    fn tile_validation_enforces_raster_tile_and_bleed_limits() {
        let valid = ops::TileRequest {
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
        };
        assert_eq!(ops::validate_tile_request(valid), Ok(()));

        let oversized_raster = ops::TileRequest {
            key: TileKey {
                raster: RasterSize {
                    width: MAX_RASTER_DIMENSION + 1,
                    height: 100,
                },
                ..valid.key
            },
            ..valid
        };
        assert!(ops::validate_tile_request(oversized_raster).is_err());

        let oversized_tile = ops::TileRequest {
            render_rect: PixelRect {
                width: MAX_TILE_DIMENSION + 1,
                ..valid.render_rect
            },
            ..valid
        };
        assert!(ops::validate_tile_request(oversized_tile).is_err());

        let missing_bleed = ops::TileRequest {
            render_rect: PixelRect {
                x: valid.core_rect.x + 1,
                ..valid.render_rect
            },
            ..valid
        };
        assert!(ops::validate_tile_request(missing_bleed).is_err());
    }

    #[test]
    fn precision_raster_is_bounded_and_preserves_aspect_ratio() {
        let (width, height) = ops::precision_text_raster(612.0, 792.0);
        assert_eq!(height, MAX_RASTER_DIMENSION as i32);
        assert!((width as f32 / height as f32 - 612.0 / 792.0).abs() < 0.0001);
        assert_eq!(ops::precision_text_raster(0.0, 0.0), (1, 1));
    }

    #[test]
    fn runtime_contract_opens_renders_extracts_and_previews_fixture() {
        let _guard = PDFIUM_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = test_fixture();
        assert!(
            fixture.is_file(),
            "fixture missing at {}",
            fixture.display()
        );
        let config = PdfiumLibraryConfig::new([test_library()]).with_system_fallback(true);
        let mut runtime = PdfRuntime::new(PdfiumEngine::new(config), CachePolicy::default());
        let opened = runtime
            .open(PdfiumDocumentSource::file(&fixture))
            .expect("generation should allocate");
        let descriptor = match opened {
            DocumentEvent::Opened { descriptor, .. } => descriptor,
            DocumentEvent::Failed { error, .. } => panic!("fixture open failed: {error}"),
            _ => panic!("fixture open unexpectedly cancelled"),
        };
        assert_eq!(descriptor.page_count(), 3);
        assert_eq!(descriptor.table_of_contents().len(), 4);
        assert_eq!(descriptor.links().len(), 2);
        assert_eq!(
            descriptor.pages(),
            [
                key_pdf_core::PageSize {
                    width: 612.0,
                    height: 792.0,
                },
                key_pdf_core::PageSize {
                    width: 612.0,
                    height: 792.0,
                },
                key_pdf_core::PageSize {
                    width: 648.0,
                    height: 360.0,
                },
            ]
        );
        assert_eq!(descriptor.table_of_contents()[0].title, "Getting Started");
        assert!(matches!(
            &descriptor.links()[0].target,
            key_pdf_core::PdfLinkTarget::External { url }
                if url == "https://example.com/research?source=pdf"
        ));
        assert!(matches!(
            descriptor.links()[1].target,
            key_pdf_core::PdfLinkTarget::Internal { page: 2, .. }
        ));
        let session = runtime.session().unwrap();

        let raster = RasterSize {
            width: 612,
            height: 792,
        };
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 612,
            height: 792,
        };
        let render = session
            .render_demand(
                TileKey {
                    page: 0,
                    raster,
                    column: 0,
                    row: 0,
                },
                rect,
                rect,
                ColorMode::Original,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        assert!(matches!(
            runtime.render(render),
            RenderEvent::Ready { tile, .. }
                if tile.image.width() == 612
                    && tile.image.height() == 792
                    && tile.image.byte_len() == 612 * 792 * 4
        ));

        let text = session
            .text_demand(
                0,
                TextDemandPurpose::Copy,
                DemandPriority::INTERACTIVE,
                DemandIntent::Explicit,
            )
            .unwrap();
        match runtime.extract_text(text) {
            TextEvent::Ready { text, .. } => {
                let extracted: String = text
                    .layer
                    .iter()
                    .filter_map(|character| (character.value != '\0').then_some(character.value))
                    .collect();
                assert!(extracted.contains("GPUI PDF Reader © Ω 你好—"));
                assert!(
                    text.layer
                        .iter()
                        .filter_map(|character| character.bounds)
                        .count()
                        > 20
                );
            }
            TextEvent::Failed { error, .. } => panic!("text extraction failed: {error}"),
            _ => panic!("text extraction unexpectedly cancelled or discarded"),
        }

        let preview = session
            .preview_demand(
                0,
                raster,
                RasterSize {
                    width: 360,
                    height: 204,
                },
                0.5,
                0.5,
                ColorMode::Forced {
                    background: key_pdf_runtime::PixelColor {
                        red: 20,
                        green: 20,
                        blue: 20,
                        alpha: 255,
                    },
                    foreground: key_pdf_runtime::PixelColor {
                        red: 230,
                        green: 230,
                        blue: 230,
                        alpha: 255,
                    },
                },
                DemandPriority::INTERACTIVE,
            )
            .unwrap();
        assert!(matches!(
            runtime.render_preview(preview),
            PreviewEvent::Ready { preview, .. }
                if preview.image.width() == 360 && preview.image.height() == 204
        ));
        assert!(runtime.close().is_some());

        let bytes = std::fs::read(&fixture).expect("fixture bytes should read");
        assert!(matches!(
            runtime
                .open(PdfiumDocumentSource::bytes(bytes))
                .expect("second generation should allocate"),
            DocumentEvent::Opened { descriptor, .. } if descriptor.page_count() == 3
        ));
        drop(runtime);

        // Partial character walks are an engine implementation detail. The
        // adapter retains them across cancellation so the app/runtime does not
        // need a PDFium-specific partial-text channel.
        let mut engine = PdfiumEngine::new(
            PdfiumLibraryConfig::new([test_library()]).with_system_fallback(true),
        );
        let source = CancellationSource::new();
        let mut document = engine
            .open(PdfiumDocumentSource::file(&fixture), &source.token())
            .expect("direct engine fixture open should work");
        let session = DocumentSessionManager::new().begin().unwrap();
        let demand = session
            .text_demand(
                0,
                TextDemandPurpose::VisibleLayer,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        let cancelled = CancellationSource::new();
        cancelled.cancel();
        assert!(matches!(
            document.extract_text(&demand, &cancelled.token()),
            Err(PdfiumEngineError::Cancelled)
        ));
        assert!(document.partial_text.contains_key(&0));
        let completed = document
            .extract_text(&demand, &CancellationSource::new().token())
            .expect("a later request should resume the retained page walk");
        assert!(!completed.is_empty());
        assert!(!document.partial_text.contains_key(&0));
    }
}
