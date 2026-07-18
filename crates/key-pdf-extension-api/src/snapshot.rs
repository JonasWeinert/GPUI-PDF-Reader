use key_pdf_core::{TextBounds, TextPosition, TextSelection};
use serde::{Deserialize, Serialize};

use crate::{DocumentDestination, DocumentHandle, PageHandle, PdfExtensionError};

/// Monotonic identity of a published snapshot within one document generation.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SnapshotRevision(pub u64);

/// A zero-based page index, independent of any PDF engine index type.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PageIndex(pub u32);

impl From<PageIndex> for usize {
    fn from(value: PageIndex) -> Self {
        value.0 as Self
    }
}

/// A point in normalized page coordinates with a top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizedPoint {
    /// Horizontal fraction from the page's left edge.
    pub x: f32,
    /// Vertical fraction from the page's top edge.
    pub y: f32,
}

/// A rectangle in normalized page coordinates with a top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    /// Left edge as a page-width fraction.
    pub left: f32,
    /// Top edge as a page-height fraction.
    pub top: f32,
    /// Right edge as a page-width fraction.
    pub right: f32,
    /// Bottom edge as a page-height fraction.
    pub bottom: f32,
}

impl From<TextBounds> for NormalizedRect {
    fn from(bounds: TextBounds) -> Self {
        Self {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        }
    }
}

impl From<NormalizedRect> for TextBounds {
    fn from(bounds: NormalizedRect) -> Self {
        Self {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        }
    }
}

/// One character position in document reading order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct TextLocation {
    /// Zero-based page index.
    pub page: PageIndex,
    /// Zero-based character index in the host's stable page text order.
    pub character: u32,
}

impl TryFrom<TextPosition> for TextLocation {
    type Error = PdfExtensionError;

    fn try_from(position: TextPosition) -> Result<Self, Self::Error> {
        let page = u32::try_from(position.page).map_err(|_| PdfExtensionError::LimitExceeded {
            field: "text_location.page".into(),
            limit: u64::from(u32::MAX),
            actual: u64::try_from(position.page).unwrap_or(u64::MAX),
        })?;
        let character =
            u32::try_from(position.index).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "text_location.character".into(),
                limit: u64::from(u32::MAX),
                actual: u64::try_from(position.index).unwrap_or(u64::MAX),
            })?;
        Ok(Self {
            page: PageIndex(page),
            character,
        })
    }
}

impl From<TextLocation> for TextPosition {
    fn from(location: TextLocation) -> Self {
        Self {
            page: location.page.into(),
            index: location.character as usize,
        }
    }
}

/// An oriented selection range; anchor and focus preserve selection direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextSelectionRange {
    /// Position where selection began.
    pub anchor: TextLocation,
    /// Current selection focus.
    pub focus: TextLocation,
}

impl TextSelectionRange {
    /// Returns the range endpoints in document order.
    #[must_use]
    pub fn ordered(self) -> (TextLocation, TextLocation) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

impl TryFrom<TextSelection> for TextSelectionRange {
    type Error = PdfExtensionError;

    fn try_from(selection: TextSelection) -> Result<Self, Self::Error> {
        Ok(Self {
            anchor: selection.anchor.try_into()?,
            focus: selection.focus.try_into()?,
        })
    }
}

impl From<TextSelectionRange> for TextSelection {
    fn from(selection: TextSelectionRange) -> Self {
        Self {
            anchor: selection.anchor.into(),
            focus: selection.focus.into(),
        }
    }
}

/// Read-only metadata for the active PDF document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadataSnapshot {
    /// Host-issued document authority.
    pub document: DocumentHandle,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Number of pages in the document.
    pub page_count: u32,
    /// Document information-dictionary title.
    pub title: Option<String>,
    /// Document information-dictionary author.
    pub author: Option<String>,
    /// Document information-dictionary subject.
    pub subject: Option<String>,
    /// Document information-dictionary keywords.
    pub keywords: Option<String>,
    /// Application that created the original document.
    pub creator: Option<String>,
    /// Application that produced the PDF bytes.
    pub producer: Option<String>,
    /// Declared natural-language tag when available.
    pub language: Option<String>,
    /// Human-readable PDF format version when available.
    pub format_version: Option<String>,
    /// Whether the document advertises structural tags.
    pub tagged: bool,
    /// Whether the source PDF is encrypted, regardless of current access.
    pub encrypted: bool,
}

/// Normalized clockwise page rotation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageRotation {
    /// No rotation.
    #[default]
    Degrees0,
    /// 90 degrees clockwise.
    Degrees90,
    /// 180 degrees clockwise.
    Degrees180,
    /// 270 degrees clockwise.
    Degrees270,
}

/// Read-only geometry and label for one page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageMetadataSnapshot {
    /// Document that owns the page.
    pub document: DocumentHandle,
    /// Host-issued page authority.
    pub page: PageHandle,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Zero-based page index.
    pub index: PageIndex,
    /// Optional logical page label such as `iv` or `A-3`.
    pub label: Option<String>,
    /// Media-box width in PDF points after normalized rotation.
    pub width_points: f32,
    /// Media-box height in PDF points after normalized rotation.
    pub height_points: f32,
    /// Clockwise page rotation.
    pub rotation: PageRotation,
}

/// Stable outline identity within a document generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutlineEntryId(pub u64);

/// One entry in a flattened document outline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineEntry {
    /// Entry identity, unique within the snapshot generation.
    pub id: OutlineEntryId,
    /// Parent identity, or `None` for a root entry.
    pub parent: Option<OutlineEntryId>,
    /// Zero-based hierarchy depth, redundant by design for streaming adapters.
    pub depth: u16,
    /// Human-readable outline title.
    pub title: String,
    /// Semantic destination, including optional text refinement.
    pub destination: DocumentDestination,
}

/// Complete bounded outline published for one document generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineSnapshot {
    /// Document that owns the outline.
    pub document: DocumentHandle,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Preorder, flattened outline entries.
    pub entries: Vec<OutlineEntry>,
    /// Whether the host omitted entries at its negotiated bound.
    pub truncated: bool,
}

/// Stable link identity within one page snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkId(pub u64);

/// Read-only target of a PDF link annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkTarget {
    /// Destination in the same document.
    Internal {
        /// Semantic destination.
        destination: DocumentDestination,
    },
    /// External URI exactly as declared by the document.
    ///
    /// Reading this value grants no authority to fetch or open it.
    External {
        /// Bounded URI string; no particular URL library is part of the API.
        uri: String,
    },
}

/// One page link and its source geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkEntry {
    /// Link identity unique within the document generation.
    pub id: LinkId,
    /// One or more clickable source regions in normalized page coordinates.
    pub regions: Vec<NormalizedRect>,
    /// Optional text range represented by the clickable source.
    pub source_range: Option<TextSelectionRange>,
    /// Link destination.
    pub target: LinkTarget,
}

/// Complete bounded link snapshot for one page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageLinksSnapshot {
    /// Page that owns the links.
    pub page: PageHandle,
    /// Zero-based page index for convenient batching.
    pub index: PageIndex,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Links in stable source order.
    pub links: Vec<LinkEntry>,
    /// Whether the host omitted links at its negotiated bound.
    pub truncated: bool,
}

/// Request for a bounded contiguous page-text chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageTextRequest {
    /// Page to read.
    pub page: PageHandle,
    /// Zero-based first character in stable host text order.
    pub start: u32,
    /// Maximum characters to return.
    pub maximum_characters: u32,
}

/// One Unicode scalar and optional page-normalized geometry.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextCharacter {
    /// Unicode scalar value encoded as an unsigned integer for WIT parity.
    pub scalar: u32,
    /// Character bounds when the PDF supplies usable geometry.
    pub bounds: Option<NormalizedRect>,
}

impl TextCharacter {
    /// Returns this scalar as a Rust character when it is valid Unicode.
    #[must_use]
    pub fn value(self) -> Option<char> {
        char::from_u32(self.scalar)
    }
}

/// Bounded text and geometry chunk for one page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageTextSnapshot {
    /// Page that owns the text.
    pub page: PageHandle,
    /// Zero-based page index.
    pub index: PageIndex,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// First returned character index.
    pub start: u32,
    /// Total known page character count.
    pub total_characters: u32,
    /// Character values and optional bounds in original extraction order.
    pub characters: Vec<TextCharacter>,
    /// Whether this chunk reaches the end of the page text.
    pub complete: bool,
}

impl PageTextSnapshot {
    /// Reconstructs the chunk text, replacing no values and failing on invalid
    /// Unicode should only occur before contract validation.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        self.characters
            .iter()
            .map(|character| character.value())
            .collect()
    }
}

/// Selection geometry for one participating page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionPageSnapshot {
    /// Host-issued page authority.
    pub page: PageHandle,
    /// Zero-based page index.
    pub index: PageIndex,
    /// Coalesced selected text regions.
    pub regions: Vec<NormalizedRect>,
}

/// Current user selection in one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    /// Document that owns the selection.
    pub document: DocumentHandle,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Oriented text range, or `None` when nothing is selected.
    pub range: Option<TextSelectionRange>,
    /// Selected text when policy allows it.
    pub text: Option<String>,
    /// Bounded geometry grouped by page.
    pub pages: Vec<SelectionPageSnapshot>,
}

/// Visible portion of one page in the current viewport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisiblePageSnapshot {
    /// Host-issued page authority.
    pub page: PageHandle,
    /// Zero-based page index.
    pub index: PageIndex,
    /// Visible page region in normalized page coordinates.
    pub visible_area: NormalizedRect,
}

/// Page-normalized point used to preserve the viewport position.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportAnchor {
    /// Zero-based anchor page.
    pub page: PageIndex,
    /// Normalized point within the page.
    pub point: NormalizedPoint,
}

/// Engine- and UI-neutral current PDF viewport state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportSnapshot {
    /// Document shown in the viewport.
    pub document: DocumentHandle,
    /// Publication revision within this generation.
    pub revision: SnapshotRevision,
    /// Zoom as a ratio where `1.0` is the document's nominal scale.
    pub zoom_ratio: f32,
    /// Page-normalized point currently used to preserve scroll position.
    pub anchor: Option<ViewportAnchor>,
    /// Visible pages in ascending page order.
    pub visible_pages: Vec<VisiblePageSnapshot>,
}
