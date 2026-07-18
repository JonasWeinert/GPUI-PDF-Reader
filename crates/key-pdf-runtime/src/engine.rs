use crate::{
    AllocationError, CachePolicy, CancellationToken, DemandError, DemandStamp, DocumentGeneration,
    DocumentSession, DocumentSessionManager, PreviewDemand, RenderDemand, ResourceHandle,
    ScheduledDemand, SessionError, TextDemand,
};
use key_pdf_core::{PageSize, PdfLink, TextLayer, TocEntry};
use std::{fmt, sync::Arc};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineCapabilities {
    pub text: bool,
    pub previews: bool,
    pub outlines: bool,
    pub links: bool,
    pub forced_colors: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentDescriptor {
    pages: Arc<[PageSize]>,
    table_of_contents: Arc<[TocEntry]>,
    links: Arc<[PdfLink]>,
}

impl DocumentDescriptor {
    pub fn new(
        pages: Vec<PageSize>,
        table_of_contents: Vec<TocEntry>,
        links: Vec<PdfLink>,
    ) -> Self {
        Self {
            pages: pages.into(),
            table_of_contents: table_of_contents.into(),
            links: links.into(),
        }
    }

    pub fn pages(&self) -> &[PageSize] {
        &self.pages
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn table_of_contents(&self) -> &[TocEntry] {
        &self.table_of_contents
    }

    pub fn links(&self) -> &[PdfLink] {
        &self.links
    }
}

/// Error bound shared by engine implementations without erasing their
/// concrete diagnostic type.
pub trait EngineFailure: std::error::Error + Send + Sync + 'static {}

impl<T> EngineFailure for T where T: std::error::Error + Send + Sync + 'static {}

pub trait EngineDocument {
    type Error: EngineFailure;

    fn descriptor(&self) -> &DocumentDescriptor;

    fn render(
        &mut self,
        demand: &RenderDemand,
        cancellation: &CancellationToken,
    ) -> Result<RasterImage, Self::Error>;

    fn extract_text(
        &mut self,
        demand: &TextDemand,
        cancellation: &CancellationToken,
    ) -> Result<TextLayer, Self::Error>;

    fn render_preview(
        &mut self,
        demand: &PreviewDemand,
        cancellation: &CancellationToken,
    ) -> Result<RasterImage, Self::Error>;
}

pub trait PdfEngine {
    type Source;
    type Document: EngineDocument<Error = Self::Error>;
    type Error: EngineFailure;

    fn capabilities(&self) -> EngineCapabilities;

    fn open(
        &mut self,
        source: Self::Source,
        cancellation: &CancellationToken,
    ) -> Result<Self::Document, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Bgra8Premultiplied,
    Rgba8Premultiplied,
    Rgba8Straight,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        4
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RasterImageError {
    Empty,
    StrideTooSmall,
    SizeOverflow,
    BufferLengthMismatch,
}

impl fmt::Display for RasterImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("raster image must be non-empty"),
            Self::StrideTooSmall => formatter.write_str("raster stride is smaller than one row"),
            Self::SizeOverflow => formatter.write_str("raster image byte size overflowed"),
            Self::BufferLengthMismatch => {
                formatter.write_str("raster buffer length does not match dimensions and stride")
            }
        }
    }
}

impl std::error::Error for RasterImageError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RasterImage {
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    pixels: Arc<[u8]>,
}

impl RasterImage {
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<Self, RasterImageError> {
        if width == 0 || height == 0 {
            return Err(RasterImageError::Empty);
        }
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(format.bytes_per_pixel()))
            .ok_or(RasterImageError::SizeOverflow)?;
        if stride < row_bytes {
            return Err(RasterImageError::StrideTooSmall);
        }
        let expected = stride
            .checked_mul(usize::try_from(height).map_err(|_| RasterImageError::SizeOverflow)?)
            .ok_or(RasterImageError::SizeOverflow)?;
        let pixels = pixels.into();
        if pixels.len() != expected {
            return Err(RasterImageError::BufferLengthMismatch);
        }
        Ok(Self {
            width,
            height,
            stride,
            format,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }
}

#[derive(Debug)]
pub enum DocumentResource {}
#[derive(Debug)]
pub enum RenderResource {}
#[derive(Debug)]
pub enum TextResource {}
#[derive(Debug)]
pub enum PreviewResource {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedTile {
    pub resource: ResourceHandle<RenderResource>,
    pub image: RasterImage,
}

#[derive(Clone, Debug)]
pub struct TextPage {
    pub resource: ResourceHandle<TextResource>,
    pub page: usize,
    pub layer: Arc<TextLayer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPreview {
    pub resource: ResourceHandle<PreviewResource>,
    pub page: usize,
    pub image: RasterImage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineOutputError {
    UnexpectedDimensions {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
}

impl fmt::Display for EngineOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedDimensions {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                formatter,
                "engine returned {actual_width}x{actual_height}, expected {expected_width}x{expected_height}"
            ),
        }
    }
}

impl std::error::Error for EngineOutputError {}

#[derive(Debug)]
pub enum RuntimeFailure<E> {
    Engine(E),
    Demand(DemandError),
    Allocation(AllocationError),
    NoDocument,
    StaleGeneration,
    InvalidPage { page: usize, page_count: usize },
    InvalidEngineOutput(EngineOutputError),
}

impl<E: fmt::Display> fmt::Display for RuntimeFailure<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(error) => error.fmt(formatter),
            Self::Demand(error) => error.fmt(formatter),
            Self::Allocation(error) => error.fmt(formatter),
            Self::NoDocument => formatter.write_str("no PDF document is open"),
            Self::StaleGeneration => formatter.write_str("demand belongs to a stale document"),
            Self::InvalidPage { page, page_count } => {
                write!(
                    formatter,
                    "page {page} lies outside a {page_count}-page document"
                )
            }
            Self::InvalidEngineOutput(error) => error.fmt(formatter),
        }
    }
}

impl<E: EngineFailure> std::error::Error for RuntimeFailure<E> {}

#[derive(Debug)]
pub enum DocumentEvent<E> {
    Opened {
        generation: DocumentGeneration,
        document: ResourceHandle<DocumentResource>,
        descriptor: DocumentDescriptor,
    },
    Cancelled {
        generation: DocumentGeneration,
    },
    Failed {
        generation: DocumentGeneration,
        error: RuntimeFailure<E>,
    },
    Closed {
        generation: DocumentGeneration,
    },
}

#[derive(Debug)]
pub enum RenderEvent<E> {
    Ready {
        demand: RenderDemand,
        tile: RenderedTile,
    },
    Cancelled {
        stamp: DemandStamp,
    },
    Discarded {
        stamp: DemandStamp,
    },
    Failed {
        stamp: DemandStamp,
        error: RuntimeFailure<E>,
    },
}

#[derive(Debug)]
pub enum TextEvent<E> {
    Ready {
        demand: TextDemand,
        text: TextPage,
    },
    Cancelled {
        stamp: DemandStamp,
    },
    Discarded {
        stamp: DemandStamp,
    },
    Failed {
        stamp: DemandStamp,
        error: RuntimeFailure<E>,
    },
}

#[derive(Debug)]
pub enum PreviewEvent<E> {
    Ready {
        demand: PreviewDemand,
        preview: RenderedPreview,
    },
    Cancelled {
        stamp: DemandStamp,
    },
    Discarded {
        stamp: DemandStamp,
    },
    Failed {
        stamp: DemandStamp,
        error: RuntimeFailure<E>,
    },
}

pub struct PdfRuntime<E: PdfEngine> {
    engine: E,
    sessions: DocumentSessionManager,
    document: Option<E::Document>,
    document_handle: Option<ResourceHandle<DocumentResource>>,
    cache_policy: CachePolicy,
}

impl<E: PdfEngine> PdfRuntime<E> {
    pub fn new(engine: E, cache_policy: CachePolicy) -> Self {
        Self {
            engine,
            sessions: DocumentSessionManager::new(),
            document: None,
            document_handle: None,
            cache_policy,
        }
    }

    pub fn capabilities(&self) -> EngineCapabilities {
        self.engine.capabilities()
    }

    pub fn cache_policy(&self) -> CachePolicy {
        self.cache_policy
    }

    pub fn session(&self) -> Option<DocumentSession> {
        self.sessions.current().cloned()
    }

    pub fn document_handle(&self) -> Option<ResourceHandle<DocumentResource>> {
        self.document_handle
    }

    pub fn descriptor(&self) -> Option<&DocumentDescriptor> {
        self.document.as_ref().map(EngineDocument::descriptor)
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    pub fn into_engine(self) -> E {
        self.engine
    }

    pub fn open(&mut self, source: E::Source) -> Result<DocumentEvent<E::Error>, SessionError> {
        self.open_inner(source, None)
    }

    /// Opens a document with caller-owned cancellation in addition to the
    /// runtime's session cancellation. This lets a UI supersede a synchronous
    /// open before the engine-owner thread can drain its command queue.
    pub fn open_with_cancellation(
        &mut self,
        source: E::Source,
        operation_cancellation: &CancellationToken,
    ) -> Result<DocumentEvent<E::Error>, SessionError> {
        self.open_inner(source, Some(operation_cancellation))
    }

    fn open_inner(
        &mut self,
        source: E::Source,
        operation_cancellation: Option<&CancellationToken>,
    ) -> Result<DocumentEvent<E::Error>, SessionError> {
        self.document = None;
        self.document_handle = None;
        let session = self.sessions.begin()?;
        let generation = session.generation();
        let cancellation = composed_cancellation(&session, operation_cancellation);
        match self.engine.open(source, &cancellation) {
            Ok(document) if cancellation.is_cancelled() => {
                drop(document);
                Ok(DocumentEvent::Cancelled { generation })
            }
            Ok(document) => {
                let descriptor = document.descriptor().clone();
                match session.allocate_resource::<DocumentResource>() {
                    Ok(handle) => {
                        self.document = Some(document);
                        self.document_handle = Some(handle);
                        Ok(DocumentEvent::Opened {
                            generation,
                            document: handle,
                            descriptor,
                        })
                    }
                    Err(error) => Ok(DocumentEvent::Failed {
                        generation,
                        error: RuntimeFailure::Allocation(error),
                    }),
                }
            }
            Err(_) if cancellation.is_cancelled() => Ok(DocumentEvent::Cancelled { generation }),
            Err(error) => Ok(DocumentEvent::Failed {
                generation,
                error: RuntimeFailure::Engine(error),
            }),
        }
    }

    pub fn close(&mut self) -> Option<DocumentEvent<E::Error>> {
        self.document = None;
        self.document_handle = None;
        self.sessions
            .close()
            .map(|generation| DocumentEvent::Closed { generation })
    }

    pub fn render(&mut self, demand: RenderDemand) -> RenderEvent<E::Error> {
        self.render_inner(demand, None)
    }

    /// Executes a render with an operation-specific cancellation token. The
    /// engine receives a token composed with the owning document session, so
    /// either source cooperatively cancels the work.
    pub fn render_with_cancellation(
        &mut self,
        demand: RenderDemand,
        operation_cancellation: &CancellationToken,
    ) -> RenderEvent<E::Error> {
        self.render_inner(demand, Some(operation_cancellation))
    }

    /// Convenience bridge from the bounded latest-wins scheduler.
    pub fn render_scheduled<K>(
        &mut self,
        scheduled: &ScheduledDemand<K, RenderDemand>,
    ) -> RenderEvent<E::Error> {
        self.render_with_cancellation(scheduled.value().clone(), &scheduled.cancellation())
    }

    fn render_inner(
        &mut self,
        demand: RenderDemand,
        operation_cancellation: Option<&CancellationToken>,
    ) -> RenderEvent<E::Error> {
        let stamp = demand.stamp();
        let session = match self.session_for(stamp) {
            SessionState::Current(session) => session,
            SessionState::Cancelled => return RenderEvent::Cancelled { stamp },
            SessionState::Stale => return RenderEvent::Discarded { stamp },
        };
        let cancellation = composed_cancellation(&session, operation_cancellation);
        if cancellation.is_cancelled() {
            return RenderEvent::Cancelled { stamp };
        }
        if let Err(error) = self.validate_page(demand.page()) {
            return RenderEvent::Failed { stamp, error };
        }
        let result = match self.document.as_mut() {
            Some(document) => document.render(&demand, &cancellation),
            None => {
                return RenderEvent::Failed {
                    stamp,
                    error: RuntimeFailure::NoDocument,
                };
            }
        };
        if cancellation.is_cancelled() {
            return RenderEvent::Cancelled { stamp };
        }
        match result {
            Ok(image) => {
                let expected = demand.render_rect();
                if image.width() != expected.width || image.height() != expected.height {
                    return RenderEvent::Failed {
                        stamp,
                        error: RuntimeFailure::InvalidEngineOutput(
                            EngineOutputError::UnexpectedDimensions {
                                expected_width: expected.width,
                                expected_height: expected.height,
                                actual_width: image.width(),
                                actual_height: image.height(),
                            },
                        ),
                    };
                }
                match session.allocate_resource::<RenderResource>() {
                    Ok(resource) => RenderEvent::Ready {
                        demand,
                        tile: RenderedTile { resource, image },
                    },
                    Err(error) => RenderEvent::Failed {
                        stamp,
                        error: RuntimeFailure::Allocation(error),
                    },
                }
            }
            Err(error) => RenderEvent::Failed {
                stamp,
                error: RuntimeFailure::Engine(error),
            },
        }
    }

    pub fn extract_text(&mut self, demand: TextDemand) -> TextEvent<E::Error> {
        self.extract_text_inner(demand, None)
    }

    /// Executes text extraction with operation and session cancellation
    /// composed into the token observed by the engine.
    pub fn extract_text_with_cancellation(
        &mut self,
        demand: TextDemand,
        operation_cancellation: &CancellationToken,
    ) -> TextEvent<E::Error> {
        self.extract_text_inner(demand, Some(operation_cancellation))
    }

    pub fn extract_text_scheduled<K>(
        &mut self,
        scheduled: &ScheduledDemand<K, TextDemand>,
    ) -> TextEvent<E::Error> {
        self.extract_text_with_cancellation(scheduled.value().clone(), &scheduled.cancellation())
    }

    fn extract_text_inner(
        &mut self,
        demand: TextDemand,
        operation_cancellation: Option<&CancellationToken>,
    ) -> TextEvent<E::Error> {
        let stamp = demand.stamp();
        let session = match self.session_for(stamp) {
            SessionState::Current(session) => session,
            SessionState::Cancelled => return TextEvent::Cancelled { stamp },
            SessionState::Stale => return TextEvent::Discarded { stamp },
        };
        let cancellation = composed_cancellation(&session, operation_cancellation);
        if cancellation.is_cancelled() {
            return TextEvent::Cancelled { stamp };
        }
        if let Err(error) = self.validate_page(demand.page()) {
            return TextEvent::Failed { stamp, error };
        }
        let result = match self.document.as_mut() {
            Some(document) => document.extract_text(&demand, &cancellation),
            None => {
                return TextEvent::Failed {
                    stamp,
                    error: RuntimeFailure::NoDocument,
                };
            }
        };
        if cancellation.is_cancelled() {
            return TextEvent::Cancelled { stamp };
        }
        match result {
            Ok(layer) => match session.allocate_resource::<TextResource>() {
                Ok(resource) => TextEvent::Ready {
                    text: TextPage {
                        resource,
                        page: demand.page(),
                        layer: Arc::new(layer),
                    },
                    demand,
                },
                Err(error) => TextEvent::Failed {
                    stamp,
                    error: RuntimeFailure::Allocation(error),
                },
            },
            Err(error) => TextEvent::Failed {
                stamp,
                error: RuntimeFailure::Engine(error),
            },
        }
    }

    pub fn render_preview(&mut self, demand: PreviewDemand) -> PreviewEvent<E::Error> {
        self.render_preview_inner(demand, None)
    }

    /// Executes preview rendering with operation and session cancellation
    /// composed into the token observed by the engine.
    pub fn render_preview_with_cancellation(
        &mut self,
        demand: PreviewDemand,
        operation_cancellation: &CancellationToken,
    ) -> PreviewEvent<E::Error> {
        self.render_preview_inner(demand, Some(operation_cancellation))
    }

    pub fn render_preview_scheduled<K>(
        &mut self,
        scheduled: &ScheduledDemand<K, PreviewDemand>,
    ) -> PreviewEvent<E::Error> {
        self.render_preview_with_cancellation(scheduled.value().clone(), &scheduled.cancellation())
    }

    fn render_preview_inner(
        &mut self,
        demand: PreviewDemand,
        operation_cancellation: Option<&CancellationToken>,
    ) -> PreviewEvent<E::Error> {
        let stamp = demand.stamp();
        let session = match self.session_for(stamp) {
            SessionState::Current(session) => session,
            SessionState::Cancelled => return PreviewEvent::Cancelled { stamp },
            SessionState::Stale => return PreviewEvent::Discarded { stamp },
        };
        let cancellation = composed_cancellation(&session, operation_cancellation);
        if cancellation.is_cancelled() {
            return PreviewEvent::Cancelled { stamp };
        }
        if let Err(error) = self.validate_page(demand.page()) {
            return PreviewEvent::Failed { stamp, error };
        }
        let result = match self.document.as_mut() {
            Some(document) => document.render_preview(&demand, &cancellation),
            None => {
                return PreviewEvent::Failed {
                    stamp,
                    error: RuntimeFailure::NoDocument,
                };
            }
        };
        if cancellation.is_cancelled() {
            return PreviewEvent::Cancelled { stamp };
        }
        match result {
            Ok(image) => {
                let expected = demand.region();
                if image.width() != expected.width || image.height() != expected.height {
                    return PreviewEvent::Failed {
                        stamp,
                        error: RuntimeFailure::InvalidEngineOutput(
                            EngineOutputError::UnexpectedDimensions {
                                expected_width: expected.width,
                                expected_height: expected.height,
                                actual_width: image.width(),
                                actual_height: image.height(),
                            },
                        ),
                    };
                }
                match session.allocate_resource::<PreviewResource>() {
                    Ok(resource) => PreviewEvent::Ready {
                        preview: RenderedPreview {
                            resource,
                            page: demand.page(),
                            image,
                        },
                        demand,
                    },
                    Err(error) => PreviewEvent::Failed {
                        stamp,
                        error: RuntimeFailure::Allocation(error),
                    },
                }
            }
            Err(error) => PreviewEvent::Failed {
                stamp,
                error: RuntimeFailure::Engine(error),
            },
        }
    }

    fn session_for(&self, stamp: DemandStamp) -> SessionState {
        let Some(session) = self.sessions.current() else {
            return SessionState::Stale;
        };
        if session.generation() != stamp.generation() {
            SessionState::Stale
        } else if session.is_cancelled() {
            SessionState::Cancelled
        } else {
            SessionState::Current(session.clone())
        }
    }

    fn validate_page(&self, page: usize) -> Result<(), RuntimeFailure<E::Error>> {
        let Some(descriptor) = self.descriptor() else {
            return Err(RuntimeFailure::NoDocument);
        };
        if page >= descriptor.page_count() {
            Err(RuntimeFailure::InvalidPage {
                page,
                page_count: descriptor.page_count(),
            })
        } else {
            Ok(())
        }
    }
}

fn composed_cancellation(
    session: &DocumentSession,
    operation: Option<&CancellationToken>,
) -> CancellationToken {
    let session = session.cancellation();
    operation.map_or(session.clone(), |operation| session.combined(operation))
}

enum SessionState {
    Current(DocumentSession),
    Cancelled,
    Stale,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ColorMode, CompletionDisposition, DemandIntent, DemandPriority, LatestWinsQueue,
        TextDemandPurpose,
    };
    use key_pdf_core::{PixelRect, RasterSize, TextBounds, TextChar, TileKey};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockError;

    impl fmt::Display for MockError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("mock engine failure")
        }
    }

    impl std::error::Error for MockError {}

    #[derive(Default)]
    struct MockEngine {
        opens: usize,
        wrong_render_dimensions: bool,
    }

    struct MockDocument {
        descriptor: DocumentDescriptor,
        renders: usize,
        text_extractions: usize,
        previews: usize,
        wrong_render_dimensions: bool,
    }

    impl PdfEngine for MockEngine {
        type Source = usize;
        type Document = MockDocument;
        type Error = MockError;

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
            page_count: Self::Source,
            cancellation: &CancellationToken,
        ) -> Result<Self::Document, Self::Error> {
            cancellation.checkpoint().map_err(|_| MockError)?;
            self.opens += 1;
            Ok(MockDocument {
                descriptor: DocumentDescriptor::new(
                    vec![
                        PageSize {
                            width: 600.0,
                            height: 800.0
                        };
                        page_count
                    ],
                    Vec::new(),
                    Vec::new(),
                ),
                renders: 0,
                text_extractions: 0,
                previews: 0,
                wrong_render_dimensions: self.wrong_render_dimensions,
            })
        }
    }

    impl EngineDocument for MockDocument {
        type Error = MockError;

        fn descriptor(&self) -> &DocumentDescriptor {
            &self.descriptor
        }

        fn render(
            &mut self,
            demand: &RenderDemand,
            cancellation: &CancellationToken,
        ) -> Result<RasterImage, Self::Error> {
            cancellation.checkpoint().map_err(|_| MockError)?;
            self.renders += 1;
            let width = demand
                .render_rect()
                .width
                .saturating_add(u32::from(self.wrong_render_dimensions));
            image(width, demand.render_rect().height)
        }

        fn extract_text(
            &mut self,
            demand: &TextDemand,
            cancellation: &CancellationToken,
        ) -> Result<TextLayer, Self::Error> {
            cancellation.checkpoint().map_err(|_| MockError)?;
            self.text_extractions += 1;
            Ok(TextLayer::new(vec![TextChar {
                value: char::from_digit(demand.page() as u32, 10).unwrap_or('?'),
                bounds: Some(TextBounds {
                    left: 0.1,
                    top: 0.1,
                    right: 0.2,
                    bottom: 0.2,
                }),
            }]))
        }

        fn render_preview(
            &mut self,
            demand: &PreviewDemand,
            cancellation: &CancellationToken,
        ) -> Result<RasterImage, Self::Error> {
            cancellation.checkpoint().map_err(|_| MockError)?;
            self.previews += 1;
            image(demand.region().width, demand.region().height)
        }
    }

    fn image(width: u32, height: u32) -> Result<RasterImage, MockError> {
        let stride = width as usize * 4;
        RasterImage::new(
            width,
            height,
            stride,
            PixelFormat::Bgra8Premultiplied,
            vec![0; stride * height as usize],
        )
        .map_err(|_| MockError)
    }

    fn opened(runtime: &mut PdfRuntime<MockEngine>) -> DocumentSession {
        assert!(matches!(
            runtime.open(2).unwrap(),
            DocumentEvent::Opened { descriptor, .. } if descriptor.page_count() == 2
        ));
        runtime.session().unwrap()
    }

    #[test]
    fn mock_engine_contract_covers_open_render_text_and_preview() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let session = opened(&mut runtime);
        assert!(runtime.capabilities().forced_colors);

        let raster = RasterSize {
            width: 100,
            height: 120,
        };
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        let render = session
            .render_demand(
                TileKey {
                    page: 1,
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
                if tile.image.width() == 40 && tile.resource.belongs_to(session.generation())
        ));

        let text = session
            .text_demand(
                1,
                TextDemandPurpose::Copy,
                DemandPriority::INTERACTIVE,
                DemandIntent::Explicit,
            )
            .unwrap();
        assert!(matches!(
            runtime.extract_text(text),
            TextEvent::Ready { text, .. }
                if text.layer[0].value == '1' && text.resource.belongs_to(session.generation())
        ));

        let preview = session
            .preview_demand(
                0,
                raster,
                RasterSize {
                    width: 20,
                    height: 10,
                },
                0.5,
                0.5,
                ColorMode::Original,
                DemandPriority::INTERACTIVE,
            )
            .unwrap();
        assert!(matches!(
            runtime.render_preview(preview),
            PreviewEvent::Ready { preview, .. }
                if preview.image.width() == 20 && preview.image.height() == 10
        ));
        let document = runtime.document.as_ref().unwrap();
        assert_eq!(
            (
                document.renders,
                document.text_extractions,
                document.previews
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn reopening_cancels_old_session_and_discards_old_demands() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let old = opened(&mut runtime);
        let stale = old
            .text_demand(
                0,
                TextDemandPurpose::VisibleLayer,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        let new = opened(&mut runtime);
        assert!(old.is_cancelled());
        assert_ne!(old.generation(), new.generation());
        assert!(matches!(
            runtime.extract_text(stale),
            TextEvent::Discarded { .. }
        ));
        assert_eq!(runtime.document.as_ref().unwrap().text_extractions, 0);
    }

    #[test]
    fn caller_can_cancel_document_open_before_engine_work_starts() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let cancellation = crate::CancellationSource::new();
        cancellation.cancel();

        assert!(matches!(
            runtime
                .open_with_cancellation(2, &cancellation.token())
                .unwrap(),
            DocumentEvent::Cancelled { .. }
        ));
        assert_eq!(runtime.engine().opens, 0);
        assert!(runtime.document.is_none());
    }

    #[test]
    fn cancelled_current_session_never_enters_the_engine() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let session = opened(&mut runtime);
        let demand = session
            .text_demand(
                0,
                TextDemandPurpose::Search,
                DemandPriority::VISIBLE,
                DemandIntent::Explicit,
            )
            .unwrap();
        session.cancel();
        assert!(matches!(
            runtime.extract_text(demand),
            TextEvent::Cancelled { .. }
        ));
        assert_eq!(runtime.document.as_ref().unwrap().text_extractions, 0);
    }

    #[test]
    fn latest_wins_replacement_cancels_engine_work_not_only_publication() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let session = opened(&mut runtime);
        let old_demand = session
            .text_demand(
                0,
                TextDemandPurpose::VisibleLayer,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        let new_demand = session
            .text_demand(
                0,
                TextDemandPurpose::VisibleLayer,
                DemandPriority::VISIBLE,
                DemandIntent::Visible,
            )
            .unwrap();
        let mut queue = LatestWinsQueue::new(2);
        queue.schedule(0, DemandPriority::VISIBLE, old_demand);
        let old = queue.pop_next().unwrap();

        queue.schedule(0, DemandPriority::VISIBLE, new_demand);
        assert!(old.is_cancelled());
        assert!(matches!(
            runtime.extract_text_scheduled(&old),
            TextEvent::Cancelled { .. }
        ));
        assert_eq!(runtime.document.as_ref().unwrap().text_extractions, 0);
        assert_eq!(queue.finish(&old), CompletionDisposition::Stale);

        let new = queue.pop_next().unwrap();
        assert!(!new.is_cancelled());
        assert!(matches!(
            runtime.extract_text_scheduled(&new),
            TextEvent::Ready { .. }
        ));
        assert_eq!(queue.finish(&new), CompletionDisposition::Publish);
        assert_eq!(runtime.document.as_ref().unwrap().text_extractions, 1);
    }

    #[test]
    fn scheduled_operation_token_is_also_cancelled_by_its_document_session() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let session = opened(&mut runtime);
        let demand = session
            .text_demand(
                0,
                TextDemandPurpose::Search,
                DemandPriority::BACKGROUND,
                DemandIntent::Background,
            )
            .unwrap();
        let mut queue = LatestWinsQueue::new(1);
        queue.schedule(0, DemandPriority::BACKGROUND, demand);
        let scheduled = queue.pop_next().unwrap();
        let composed = session.cancellation().combined(&scheduled.cancellation());

        session.cancel();
        assert!(composed.is_cancelled());
        assert!(!scheduled.is_cancelled());
        assert!(matches!(
            runtime.extract_text_scheduled(&scheduled),
            TextEvent::Cancelled { .. }
        ));
        assert_eq!(runtime.document.as_ref().unwrap().text_extractions, 0);
    }

    #[test]
    fn page_bounds_and_engine_output_dimensions_are_enforced() {
        let mut runtime = PdfRuntime::new(MockEngine::default(), CachePolicy::default());
        let session = opened(&mut runtime);
        let invalid_page = session
            .text_demand(
                9,
                TextDemandPurpose::Copy,
                DemandPriority::INTERACTIVE,
                DemandIntent::Explicit,
            )
            .unwrap();
        assert!(matches!(
            runtime.extract_text(invalid_page),
            TextEvent::Failed {
                error: RuntimeFailure::InvalidPage {
                    page: 9,
                    page_count: 2
                },
                ..
            }
        ));

        let mut runtime = PdfRuntime::new(
            MockEngine {
                wrong_render_dimensions: true,
                ..MockEngine::default()
            },
            CachePolicy::default(),
        );
        let session = opened(&mut runtime);
        let raster = RasterSize {
            width: 100,
            height: 100,
        };
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 20,
            height: 20,
        };
        let demand = session
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
            runtime.render(demand),
            RenderEvent::Failed {
                error: RuntimeFailure::InvalidEngineOutput(
                    EngineOutputError::UnexpectedDimensions { .. }
                ),
                ..
            }
        ));
    }

    #[test]
    fn raster_constructor_rejects_malformed_engine_buffers() {
        assert_eq!(
            RasterImage::new(2, 2, 8, PixelFormat::Rgba8Straight, vec![0; 15]),
            Err(RasterImageError::BufferLengthMismatch)
        );
        assert_eq!(
            RasterImage::new(2, 2, 7, PixelFormat::Rgba8Straight, vec![0; 14]),
            Err(RasterImageError::StrideTooSmall)
        );
    }
}
