//! Narrow service traits for native implementations and transport adapters.
//!
//! Methods are intentionally synchronous semantic request boundaries. A host
//! may dispatch work before invoking them or return `Busy`; the API does not
//! select an async executor, thread model, or WebAssembly runtime.

use crate::{
    DocumentHandle, DocumentMetadataSnapshot, NavigationReceipt, NavigationRequest,
    OutlineSnapshot, OverlayBatch, OverlayReceipt, PageIndex, PageLinksSnapshot,
    PageMetadataSnapshot, PageTextRequest, PageTextSnapshot, PdfCapability, PdfExtensionResult,
    SelectionSnapshot, ViewportSnapshot,
};

/// Provider for `key:pdf/document-metadata`.
pub trait DocumentMetadataService {
    /// Returns the active scoped document, if one exists.
    ///
    /// # Errors
    ///
    /// Returns a structured provider, policy, or availability error.
    fn active_document(&self) -> PdfExtensionResult<Option<DocumentHandle>>;

    /// Reads metadata for a host-issued document handle.
    ///
    /// # Errors
    ///
    /// Returns a structured error for stale/unknown handles or provider failure.
    fn document_metadata(
        &self,
        document: DocumentHandle,
    ) -> PdfExtensionResult<DocumentMetadataSnapshot>;
}

/// Provider for `key:pdf/page-metadata`.
pub trait PageMetadataService {
    /// Reads metadata and obtains a page handle by zero-based index.
    ///
    /// # Errors
    ///
    /// Returns a structured error for stale handles, invalid pages, or provider failure.
    fn page_metadata(
        &self,
        document: DocumentHandle,
        page: PageIndex,
    ) -> PdfExtensionResult<PageMetadataSnapshot>;
}

/// Provider for `key:pdf/outline`.
pub trait OutlineService {
    /// Reads the bounded flattened outline for a document.
    ///
    /// # Errors
    ///
    /// Returns a structured error for a stale handle or provider failure.
    fn outline(&self, document: DocumentHandle) -> PdfExtensionResult<OutlineSnapshot>;
}

/// Provider for `key:pdf/links`.
pub trait LinkService {
    /// Reads bounded links for a page obtained through page metadata.
    ///
    /// # Errors
    ///
    /// Returns a structured error for a stale/unknown page or provider failure.
    fn links(&self, page: crate::PageHandle) -> PdfExtensionResult<PageLinksSnapshot>;
}

/// Provider for `key:pdf/text`.
pub trait TextService {
    /// Reads a bounded contiguous page-text chunk.
    ///
    /// # Errors
    ///
    /// Returns a structured error for invalid bounds, stale pages, or provider failure.
    fn page_text(&self, request: PageTextRequest) -> PdfExtensionResult<PageTextSnapshot>;
}

/// Provider for `key:pdf/selection`.
pub trait SelectionService {
    /// Reads the current selection for a document.
    ///
    /// # Errors
    ///
    /// Returns a structured error for a stale handle, denied policy, or provider failure.
    fn selection(&self, document: DocumentHandle) -> PdfExtensionResult<SelectionSnapshot>;
}

/// Provider for `key:pdf/viewport`.
pub trait ViewportService {
    /// Reads the current viewport for a document.
    ///
    /// # Errors
    ///
    /// Returns a structured error for a stale handle or unavailable viewport.
    fn viewport(&self, document: DocumentHandle) -> PdfExtensionResult<ViewportSnapshot>;
}

/// Provider for `key:pdf/navigation`.
pub trait NavigationService {
    /// Resolves and schedules one semantic navigation request.
    ///
    /// # Errors
    ///
    /// Returns a structured error for invalid input, denied policy, or provider failure.
    fn navigate(&self, request: NavigationRequest) -> PdfExtensionResult<NavigationReceipt>;
}

/// Provider for `key:pdf/overlays`.
pub trait OverlayService {
    /// Atomically replaces all overlays owned by the calling extension.
    ///
    /// # Errors
    ///
    /// Returns a structured error for invalid input, stale revisions, or denied policy.
    fn replace_overlays(&self, batch: OverlayBatch) -> PdfExtensionResult<OverlayReceipt>;
}

/// Optional service locator used by hosts that compose capabilities dynamically.
///
/// Returning `None` is distinct from a present service denying a particular
/// call. All methods default to `None`, so small providers implement only the
/// capabilities they actually expose.
pub trait PdfServiceProvider {
    /// Resolves the document-metadata service.
    fn document_metadata_service(&self) -> Option<&dyn DocumentMetadataService> {
        None
    }

    /// Resolves the page-metadata service.
    fn page_metadata_service(&self) -> Option<&dyn PageMetadataService> {
        None
    }

    /// Resolves the outline service.
    fn outline_service(&self) -> Option<&dyn OutlineService> {
        None
    }

    /// Resolves the link service.
    fn link_service(&self) -> Option<&dyn LinkService> {
        None
    }

    /// Resolves the text service.
    fn text_service(&self) -> Option<&dyn TextService> {
        None
    }

    /// Resolves the selection service.
    fn selection_service(&self) -> Option<&dyn SelectionService> {
        None
    }

    /// Resolves the viewport service.
    fn viewport_service(&self) -> Option<&dyn ViewportService> {
        None
    }

    /// Resolves the navigation service.
    fn navigation_service(&self) -> Option<&dyn NavigationService> {
        None
    }

    /// Resolves the overlay service.
    fn overlay_service(&self) -> Option<&dyn OverlayService> {
        None
    }

    /// Returns exactly the capabilities currently backed by a service.
    #[must_use]
    fn provided_pdf_capabilities(&self) -> Vec<PdfCapability> {
        let mut result = Vec::with_capacity(PdfCapability::ALL.len());
        if self.document_metadata_service().is_some() {
            result.push(PdfCapability::DocumentMetadata);
        }
        if self.page_metadata_service().is_some() {
            result.push(PdfCapability::PageMetadata);
        }
        if self.outline_service().is_some() {
            result.push(PdfCapability::Outline);
        }
        if self.link_service().is_some() {
            result.push(PdfCapability::Links);
        }
        if self.text_service().is_some() {
            result.push(PdfCapability::Text);
        }
        if self.selection_service().is_some() {
            result.push(PdfCapability::Selection);
        }
        if self.viewport_service().is_some() {
            result.push(PdfCapability::Viewport);
        }
        if self.navigation_service().is_some() {
            result.push(PdfCapability::Navigation);
        }
        if self.overlay_service().is_some() {
            result.push(PdfCapability::Overlays);
        }
        result
    }
}
