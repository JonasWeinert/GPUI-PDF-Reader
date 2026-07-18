//! Engine- and UI-agnostic PDF document behavior.
//!
//! This crate owns normalized geometry, document layout, selectable text,
//! bounded search, internal-link refinement, scientific-document detection,
//! and document-jump/focus semantics. PDFium extraction, GPUI presentation,
//! networking, and persistence are intentionally adapter responsibilities.

#![forbid(unsafe_code)]

pub mod annotations;
pub mod document_jump;
pub mod link_resolution;
pub mod model;
pub mod navigation_focus;
pub mod scientific;
pub mod search;

pub use annotations::{
    Annotation, AnnotationError, AnnotationId, AnnotationSet, HighlightColor, MAX_COMMENT_BYTES,
    MAX_TEXT_CHARACTER_INDEX, RestoredAnnotation, TextRange,
};
pub use document_jump::{DocumentJump, ResolvedDocumentJump};
pub use link_resolution::{ResolvedInternalLink, link_source_text, resolve_internal_link};
pub use model::{
    DocumentLayout, PAGE_GAP, PAGE_MARGIN, PDF_POINTS_TO_LOGICAL_PIXELS, PageAnchor, PageSize,
    PdfLink, PdfLinkTarget, PixelRect, RasterSize, Rect, TextBounds, TextChar, TextLayer,
    TextPosition, TextSelection, TextSpatialIndex, TileKey, TocEntry, append_selected_page_text,
    selected_text,
};
pub use navigation_focus::{
    NAVIGATION_FOCUS_DURATION, NAVIGATION_FOCUS_PULSE_DURATION, NavigationFocusEffect,
    NavigationFocusFrame, NavigationFocusMotion, NavigationFocusTarget, NavigationFocusTone,
};
pub use scientific::{
    ScientificAnalysis, ScientificAnalyzer, ScientificReference, ScientificSignals, detect_doi,
    grouped_citation_numbers,
};
pub use search::{
    MAX_NORMALIZED_QUERY_CHARS, MAX_SEARCH_QUERY_BYTES, MAX_SEARCH_RESULTS, SearchMatch,
    SearchMatchId, SearchPageOutcome, SearchPageResults, SearchQuery, SearchQueryError,
    search_page, text_runs_for_range,
};
