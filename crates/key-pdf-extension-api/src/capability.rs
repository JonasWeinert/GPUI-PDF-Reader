use key_extension_api::{CapabilityId, DocumentAccess, Permission};
use serde::{Deserialize, Serialize};

/// Semantic version of the pre-stable PDF extension contract.
pub const PDF_EXTENSION_API_VERSION: &str = "0.1.0";

/// A separately negotiable PDF host capability.
///
/// The enum is deliberately exhaustive for this contract version. Extensions
/// request only the capabilities they use through `key-extension-api`; hosts
/// must not infer one grant from another.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfCapability {
    /// Read active-document metadata and obtain a document handle.
    DocumentMetadata,
    /// Read metadata for individual pages and obtain page handles.
    PageMetadata,
    /// Read the flattened document outline.
    Outline,
    /// Read page link annotations and their semantic targets.
    Links,
    /// Read bounded page-text chunks and geometry.
    Text,
    /// Read the current document selection.
    Selection,
    /// Read the current zoom, anchor, and visible-page snapshot.
    Viewport,
    /// Request semantic document navigation and transient focus cues.
    Navigation,
    /// Replace a bounded, extension-owned set of visual overlays.
    Overlays,
}

impl PdfCapability {
    /// Every capability in deterministic contract order.
    pub const ALL: [Self; 9] = [
        Self::DocumentMetadata,
        Self::PageMetadata,
        Self::Outline,
        Self::Links,
        Self::Text,
        Self::Selection,
        Self::Viewport,
        Self::Navigation,
        Self::Overlays,
    ];

    /// Returns the canonical semantic capability identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentMetadata => "key:pdf/document-metadata",
            Self::PageMetadata => "key:pdf/page-metadata",
            Self::Outline => "key:pdf/outline",
            Self::Links => "key:pdf/links",
            Self::Text => "key:pdf/text",
            Self::Selection => "key:pdf/selection",
            Self::Viewport => "key:pdf/viewport",
            Self::Navigation => "key:pdf/navigation",
            Self::Overlays => "key:pdf/overlays",
        }
    }

    /// Returns the corresponding case in the versioned WIT `capability` enum.
    #[must_use]
    pub const fn wit_case(self) -> &'static str {
        match self {
            Self::DocumentMetadata => "document-metadata",
            Self::PageMetadata => "page-metadata",
            Self::Outline => "outline",
            Self::Links => "links",
            Self::Text => "text",
            Self::Selection => "selection",
            Self::Viewport => "viewport",
            Self::Navigation => "navigation",
            Self::Overlays => "overlays",
        }
    }

    /// Converts the semantic name to the generic extension capability type.
    ///
    /// # Panics
    ///
    /// Panics only if a capability constant declared in this crate stops being
    /// canonical according to `key-extension-api`, which is a build-time bug.
    #[must_use]
    pub fn capability_id(self) -> CapabilityId {
        CapabilityId::parse(self.as_str())
            .expect("built-in PDF capability identifiers are canonical")
    }

    /// Returns the user-sensitive authority required to invoke this
    /// capability. Capability negotiation only proves protocol availability;
    /// the host must independently require this permission for every call.
    #[must_use]
    pub const fn required_permission(self) -> Permission {
        match self {
            Self::DocumentMetadata
            | Self::PageMetadata
            | Self::Outline
            | Self::Links
            | Self::Viewport => Permission::ReadDocumentMetadata,
            Self::Text => Permission::ReadDocumentText(DocumentAccess::ActiveDocument),
            Self::Selection => Permission::ReadSelection,
            Self::Navigation => Permission::NavigateDocument,
            Self::Overlays => Permission::AddDocumentOverlays,
        }
    }

    /// Finds a PDF capability by its canonical semantic identifier.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.as_str() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pdf_capability_has_explicit_user_authority() {
        let permissions = PdfCapability::ALL
            .into_iter()
            .map(PdfCapability::required_permission)
            .collect::<Vec<_>>();

        assert_eq!(permissions.len(), PdfCapability::ALL.len());
        assert_eq!(
            PdfCapability::Text.required_permission(),
            Permission::ReadDocumentText(DocumentAccess::ActiveDocument)
        );
        assert_eq!(
            PdfCapability::Navigation.required_permission(),
            Permission::NavigateDocument
        );
        assert_eq!(
            PdfCapability::Overlays.required_permission(),
            Permission::AddDocumentOverlays
        );
    }
}
