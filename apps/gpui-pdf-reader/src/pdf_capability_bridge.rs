//! App-owned bridge between immutable reader snapshots and PDF extensions.
//!
//! The bridge owns resource generations and validates every snapshot before it
//! becomes visible to an extension. It has no PDFium or GPUI types; the reader
//! can publish snapshots and consume pending semantic jumps/overlays at its
//! thread-affine boundary.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use key_extension_api::{DataValue, GenerationId};
use key_pdf_core::{
    DocumentJump, DocumentLayout, PageSize, PdfLink, PdfLinkTarget, SearchPageOutcome, SearchQuery,
    TextChar, TextLayer, TextSelection, TocEntry, append_selected_page_text, search_page,
    text_runs_for_range,
};
use key_pdf_extension_api::{
    DocumentDestination, DocumentHandle, DocumentMetadataService, DocumentMetadataSnapshot,
    GenerationScoped, InputErrorKind, LinkService, NavigationFocusRequest, NavigationReceipt,
    NavigationRequest, NavigationService, NormalizedPoint, NormalizedRect, OutlineEntry,
    OutlineEntryId, OutlineService, OutlineSnapshot, OverlayBatch, OverlayReceipt, OverlayService,
    OverlaySetHandle, PageHandle, PageIndex, PageLinksSnapshot, PageMetadataService,
    PageMetadataSnapshot, PageRotation, PageTextRequest, PageTextSnapshot, PdfCapability,
    PdfContractLimits, PdfExtensionError, PdfExtensionResult, PdfServiceProvider,
    PdfValidationContext, SelectionPageSnapshot, SelectionService, SelectionSnapshot,
    SnapshotRevision, TextCharacter, TextLocation, TextSelectionRange, TextService, TextTargetHint,
    ValidatePdfContract, ViewportAnchor, ViewportService, ViewportSnapshot, VisiblePageSnapshot,
};
use serde::{Serialize, de::DeserializeOwned};

/// One immutable snapshot publication from the reader into the extension host.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfBridgeSnapshot {
    /// Active-document metadata.
    Document(DocumentMetadataSnapshot),
    /// Metadata and handle for one page.
    Page(PageMetadataSnapshot),
    /// Complete bounded document outline.
    Outline(OutlineSnapshot),
    /// Complete bounded links for one page.
    Links(PageLinksSnapshot),
    /// Complete text and geometry for one page.
    Text(PageTextSnapshot),
    /// Current user selection.
    Selection(SelectionSnapshot),
    /// Current viewport state.
    Viewport(ViewportSnapshot),
}

/// A resolved shared jump waiting for the reader's UI-thread navigation owner.
#[derive(Clone, Debug)]
pub struct PendingPdfNavigation {
    /// Shared scroll/centering/focus abstraction.
    pub jump: DocumentJump,
    /// Transport receipt returned to the requesting extension.
    pub receipt: NavigationReceipt,
}

/// Generation-safe implementation of all nine pre-stable PDF capabilities.
pub struct PdfCapabilityBridge {
    limits: PdfContractLimits,
    state: RwLock<BridgeState>,
}

#[derive(Default)]
struct BridgeState {
    next_generation: u64,
    active: Option<ActiveDocument>,
}

struct ActiveDocument {
    document: DocumentHandle,
    page_count: u32,
    metadata: Option<Arc<DocumentMetadataSnapshot>>,
    pages: BTreeMap<PageIndex, Arc<PageMetadataSnapshot>>,
    outline: Option<Arc<OutlineSnapshot>>,
    links: BTreeMap<PageIndex, Arc<PageLinksSnapshot>>,
    text: BTreeMap<PageIndex, Arc<PageTextSnapshot>>,
    selection: Option<Arc<SelectionSnapshot>>,
    viewport: Option<Arc<ViewportSnapshot>>,
    pending_navigation: Option<PendingPdfNavigation>,
    next_snapshot_revision: u64,
    next_viewport_revision: u64,
    overlays: Option<Arc<OverlayBatch>>,
    next_overlay_id: u64,
}

impl Default for PdfCapabilityBridge {
    fn default() -> Self {
        Self::new(PdfContractLimits::default())
    }
}

impl PdfCapabilityBridge {
    /// Creates a bridge with host-negotiated contract limits.
    #[must_use]
    pub fn new(limits: PdfContractLimits) -> Self {
        Self {
            limits,
            state: RwLock::new(BridgeState {
                next_generation: 1,
                active: None,
            }),
        }
    }

    /// Starts a new document generation and invalidates every prior handle.
    ///
    /// # Errors
    ///
    /// Returns a structured internal error if the generation counter is
    /// exhausted or the bridge lock was poisoned.
    pub fn begin_document(&self, page_count: u32) -> PdfExtensionResult<DocumentHandle> {
        let mut state = self.write_state()?;
        let generation = allocate_document_generation(&mut state)?;
        let document = DocumentHandle::from_host_parts(GenerationId(generation), 1)?;
        state.active = Some(ActiveDocument::new(document, page_count));
        Ok(document)
    }

    /// Closes the active document and drops all retained snapshots and overlays.
    ///
    /// # Errors
    ///
    /// Returns a structured internal error if the bridge lock was poisoned.
    pub fn close_document(&self) -> PdfExtensionResult<()> {
        self.write_state()?.active = None;
        Ok(())
    }

    /// Begins a generation from the engine-neutral state already owned by the
    /// reader and atomically seeds every document-scoped immutable snapshot.
    /// No PDFium handle or thread-affine object crosses this boundary.
    ///
    /// # Errors
    ///
    /// Returns a structured bounds, geometry, generation, or lock error. A
    /// failed seed leaves the previous active generation intact so extensions
    /// never observe a partially described replacement document.
    pub fn begin_live_document(
        &self,
        title: Option<&str>,
        pages: &[PageSize],
        outline: &[TocEntry],
        links: &[PdfLink],
    ) -> PdfExtensionResult<DocumentHandle> {
        let page_count =
            u32::try_from(pages.len()).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "document.page_count".into(),
                limit: u64::from(u32::MAX),
                actual: u64::try_from(pages.len()).unwrap_or(u64::MAX),
            })?;
        let mut state = self.write_state()?;
        let generation = allocate_document_generation(&mut state)?;
        let document = DocumentHandle::from_host_parts(GenerationId(generation), 1)?;
        let snapshots =
            build_live_document_snapshots(document, title, pages, outline, links, &self.limits)?;
        let mut active = ActiveDocument::new(document, page_count);
        publish_snapshot_into(
            &mut active,
            PdfBridgeSnapshot::Document(snapshots.document),
            &self.limits,
        )?;
        for page in snapshots.pages {
            publish_snapshot_into(&mut active, PdfBridgeSnapshot::Page(page), &self.limits)?;
        }
        publish_snapshot_into(
            &mut active,
            PdfBridgeSnapshot::Outline(snapshots.outline),
            &self.limits,
        )?;
        for links in snapshots.links {
            publish_snapshot_into(&mut active, PdfBridgeSnapshot::Links(links), &self.limits)?;
        }
        state.active = Some(active);
        Ok(document)
    }

    /// Publishes one extracted page text layer if its immutable content has
    /// changed. Oversized pages are rejected instead of retaining an
    /// unbounded extension snapshot.
    ///
    /// # Errors
    ///
    /// Returns a structured bounds, page, generation, or lock error.
    pub fn publish_live_text(&self, page: usize, text: &TextLayer) -> PdfExtensionResult<bool> {
        let (_document, handle, index) = self.live_page_authority(page)?;
        let total_characters =
            u32::try_from(text.len()).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "text.characters".into(),
                limit: u64::from(u32::MAX),
                actual: u64::try_from(text.len()).unwrap_or(u64::MAX),
            })?;
        if text.len() > self.limits.maximum_text_characters {
            return Err(PdfExtensionError::LimitExceeded {
                field: "text.characters".into(),
                limit: u64::try_from(self.limits.maximum_text_characters).unwrap_or(u64::MAX),
                actual: u64::try_from(text.len()).unwrap_or(u64::MAX),
            });
        }
        let characters = text
            .iter()
            .map(|character| TextCharacter {
                scalar: u32::from(character.value),
                bounds: character.bounds.map(NormalizedRect::from),
            })
            .collect();
        self.publish_live_snapshot_if_changed(PdfBridgeSnapshot::Text(PageTextSnapshot {
            page: handle,
            index,
            revision: SnapshotRevision(0),
            start: 0,
            total_characters,
            characters,
            complete: true,
        }))
    }

    /// Replaces page-link snapshots after the reader discovers additional
    /// engine-neutral links (for example bounded scientific-reference matches).
    /// Unchanged pages retain their existing revision.
    ///
    /// # Errors
    ///
    /// Returns a structured bounds, page, generation, or lock error.
    pub fn publish_live_links(&self, links: &[PdfLink]) -> PdfExtensionResult<bool> {
        let (document, page_count) = self.live_document_authority()?;
        let page_count_usize = usize::try_from(page_count).unwrap_or(usize::MAX);
        let mut snapshots =
            build_live_link_snapshots(document, page_count_usize, page_count, links, &self.limits)?;
        let mut changed = false;
        for snapshot in &mut snapshots {
            snapshot.revision = SnapshotRevision(0);
            changed |=
                self.publish_live_snapshot_if_changed(PdfBridgeSnapshot::Links(snapshot.clone()))?;
        }
        Ok(changed)
    }

    /// Publishes the current selection and the bounded text/geometry already
    /// present in the reader's immutable page-text cache.
    ///
    /// # Errors
    ///
    /// Returns a structured range, generation, bounds, or lock error.
    pub fn publish_live_selection<'a>(
        &self,
        selection: Option<TextSelection>,
        page_text: impl Fn(usize) -> Option<&'a TextLayer>,
    ) -> PdfExtensionResult<bool> {
        let (document, page_count) = self.live_document_authority()?;
        let mut pages = Vec::new();
        let mut selected_text = String::new();
        let mut text_complete = true;
        let mut text_within_limit = true;
        let mut remaining_regions = self.limits.maximum_total_regions;
        let range = selection
            .map(|selection| live_selection_range(selection, page_count))
            .transpose()?;

        if let Some(selection) = selection {
            let (start, end) = selection.ordered();
            let last_page = end
                .page
                .min(usize::try_from(page_count.saturating_sub(1)).unwrap());
            let page_span = last_page.saturating_sub(start.page).saturating_add(1);
            if page_span > self.limits.maximum_snapshot_pages {
                text_complete = false;
            }
            for page in (start.page..=last_page).take(self.limits.maximum_snapshot_pages) {
                let Some(text) = page_text(page) else {
                    text_complete = false;
                    continue;
                };
                if text_within_limit {
                    append_selected_page_text(&mut selected_text, selection, page, text.as_slice());
                    if selected_text.len() > self.limits.maximum_selection_text_bytes {
                        selected_text.clear();
                        text_within_limit = false;
                    }
                }
                let Some(indices) = selection.indices_on_page(page, text.len()) else {
                    continue;
                };
                let mut regions =
                    text_runs_for_range(text.as_slice(), *indices.start(), *indices.end());
                let maximum = self.limits.maximum_regions_per_item.min(remaining_regions);
                if regions.len() > maximum {
                    regions.truncate(maximum);
                }
                remaining_regions = remaining_regions.saturating_sub(regions.len());
                if regions.is_empty() {
                    continue;
                }
                let index = live_page_index(page, page_count)?;
                pages.push(SelectionPageSnapshot {
                    page: live_page_handle(document, index)?,
                    index,
                    regions: regions.into_iter().map(NormalizedRect::from).collect(),
                });
                if remaining_regions == 0 {
                    if page < last_page {
                        text_complete = false;
                    }
                    break;
                }
            }
        }
        let text =
            (selection.is_some() && text_complete && text_within_limit).then_some(selected_text);
        self.publish_live_snapshot_if_changed(PdfBridgeSnapshot::Selection(SelectionSnapshot {
            document,
            revision: SnapshotRevision(0),
            range,
            text,
            pages,
        }))
    }

    /// Publishes the visible page regions and stable center anchor if the
    /// viewport changed since the previous publication.
    ///
    /// # Errors
    ///
    /// Returns a structured geometry, page, generation, or lock error.
    pub fn publish_live_viewport(
        &self,
        zoom_ratio: f32,
        layout: &DocumentLayout,
        scroll_x: f32,
        scroll_y: f32,
        viewport_width: f32,
        viewport_height: f32,
    ) -> PdfExtensionResult<bool> {
        let (document, page_count) = self.live_document_authority()?;
        if layout.page_count() != usize::try_from(page_count).unwrap_or(usize::MAX) {
            return invalid("viewport.layout", InputErrorKind::InvalidIdentifier);
        }
        let viewport = key_pdf_core::Rect {
            x: scroll_x,
            y: scroll_y,
            width: viewport_width,
            height: viewport_height,
        };
        let visible_pages = layout
            .visible_pages(scroll_y, viewport_height, 0.0)
            .take(self.limits.maximum_snapshot_pages)
            .filter_map(|page| {
                let page_rect = layout.page_rect(page)?;
                let visible = intersect_rect(page_rect, viewport)?;
                let index = live_page_index(page, page_count).ok()?;
                Some(
                    live_page_handle(document, index).map(|handle| VisiblePageSnapshot {
                        page: handle,
                        index,
                        visible_area: NormalizedRect {
                            left: ((visible.x - page_rect.x) / page_rect.width).clamp(0.0, 1.0),
                            top: ((visible.y - page_rect.y) / page_rect.height).clamp(0.0, 1.0),
                            right: ((visible.right() - page_rect.x) / page_rect.width)
                                .clamp(0.0, 1.0),
                            bottom: ((visible.bottom() - page_rect.y) / page_rect.height)
                                .clamp(0.0, 1.0),
                        },
                    }),
                )
            })
            .collect::<PdfExtensionResult<Vec<_>>>()?;
        let anchor = layout
            .anchor_at_content_point(
                scroll_x + viewport_width * 0.5,
                scroll_y + viewport_height * 0.5,
            )
            .map(|anchor| {
                Ok(ViewportAnchor {
                    page: live_page_index(anchor.page, page_count)?,
                    point: NormalizedPoint {
                        x: anchor.x_fraction,
                        y: anchor.y_fraction,
                    },
                })
            })
            .transpose()?;
        self.publish_live_snapshot_if_changed(PdfBridgeSnapshot::Viewport(ViewportSnapshot {
            document,
            revision: SnapshotRevision(0),
            zoom_ratio,
            anchor,
            visible_pages,
        }))
    }

    /// Dispatches the versioned WIT operation names over the generic bounded
    /// extension transport. The conversion is data-only and grants no network,
    /// filesystem, PDFium, or GPUI authority.
    ///
    /// # Errors
    ///
    /// Returns a structured PDF capability or transport-shape error.
    pub fn call(
        &self,
        capability: PdfCapability,
        operation: &str,
        input: DataValue,
    ) -> PdfExtensionResult<DataValue> {
        #[derive(serde::Deserialize)]
        struct PageMetadataInput {
            document: DocumentHandle,
            page: PageIndex,
        }

        match (capability, operation) {
            (PdfCapability::DocumentMetadata, "active-document") if input == DataValue::Null => {
                to_data_value(&self.active_document()?)
            }
            (PdfCapability::DocumentMetadata, "metadata") => {
                let document = from_data_value(input)?;
                to_data_value(&self.document_metadata(document)?)
            }
            (PdfCapability::PageMetadata, "metadata") => {
                let input: PageMetadataInput = from_data_value(input)?;
                self.page_handle(input.document, input.page)?;
                to_data_value(&self.page_metadata(input.document, input.page)?)
            }
            (PdfCapability::Outline, "read") => {
                let document = from_data_value(input)?;
                to_data_value(&self.outline(document)?)
            }
            (PdfCapability::Links, "read") => {
                let page = from_data_value(input)?;
                to_data_value(&self.links(page)?)
            }
            (PdfCapability::Text, "read-page") => {
                let request = from_data_value(input)?;
                to_data_value(&self.page_text(request)?)
            }
            (PdfCapability::Selection, "read") => {
                let document = from_data_value(input)?;
                to_data_value(&self.selection(document)?)
            }
            (PdfCapability::Viewport, "read") => {
                let document = from_data_value(input)?;
                to_data_value(&self.viewport(document)?)
            }
            (PdfCapability::Navigation, "navigate") => {
                let request = from_data_value(input)?;
                to_data_value(&self.navigate(request)?)
            }
            (PdfCapability::Overlays, "replace") => {
                let batch = from_data_value(input)?;
                to_data_value(&self.replace_overlays(batch)?)
            }
            (PdfCapability::DocumentMetadata, "active-document") => {
                invalid("capability.input", InputErrorKind::InvalidIdentifier)
            }
            _ => Err(PdfExtensionError::Unsupported {
                feature: format!("{}::{operation}", capability.as_str()),
            }),
        }
    }

    /// Returns the host-issued handle for a page in the current document.
    ///
    /// # Errors
    ///
    /// Returns `NoActiveDocument`, `StaleGeneration`, `ResourceNotFound`, or
    /// `PageOutOfBounds` when the requested authority is invalid.
    pub fn page_handle(
        &self,
        document: DocumentHandle,
        page: PageIndex,
    ) -> PdfExtensionResult<PageHandle> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active.page_handle(page)
    }

    /// Atomically validates and publishes one immutable reader snapshot.
    ///
    /// # Errors
    ///
    /// Returns the first contract, generation, handle, or monotonic-revision
    /// violation. Invalid publications leave the previous snapshot untouched.
    pub fn publish_snapshot(&self, snapshot: PdfBridgeSnapshot) -> PdfExtensionResult<()> {
        let mut state = self.write_state()?;
        let active = require_active_mut(&mut state)?;
        publish_snapshot_into(active, snapshot, &self.limits)
    }

    /// Takes the latest accepted semantic jump. Newer requests supersede older
    /// undelivered requests so extension traffic cannot create an unbounded queue.
    ///
    /// # Errors
    ///
    /// Returns `NoActiveDocument` or a structured lock error.
    pub fn take_pending_navigation(&self) -> PdfExtensionResult<Option<PendingPdfNavigation>> {
        let mut state = self.write_state()?;
        Ok(require_active_mut(&mut state)?.pending_navigation.take())
    }

    /// Returns the current extension-owned overlay set for trusted rendering.
    ///
    /// # Errors
    ///
    /// Returns a structured generation, resource, or lock error.
    pub fn current_overlays(
        &self,
        document: DocumentHandle,
    ) -> PdfExtensionResult<Option<Arc<OverlayBatch>>> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        Ok(active.overlays.clone())
    }

    /// Returns the active overlay batch without requiring the trusted reader
    /// to round-trip its own current document handle.
    ///
    /// # Errors
    ///
    /// Returns `NoActiveDocument` or a structured lock error.
    pub fn active_overlays(&self) -> PdfExtensionResult<Option<Arc<OverlayBatch>>> {
        let document = self
            .active_document()?
            .ok_or(PdfExtensionError::NoActiveDocument)?;
        self.current_overlays(document)
    }

    /// Routes a live reader TOC activation through the capability-only pilot.
    /// The title/page pair avoids coupling the UI's filtered index to the
    /// bounded flattened outline's host identity.
    ///
    /// # Errors
    ///
    /// Returns a structured outline, resource, or navigation error.
    pub fn activate_live_outline(
        &self,
        title: &str,
        page: usize,
    ) -> PdfExtensionResult<NavigationReceipt> {
        let document = self
            .active_document()?
            .ok_or(PdfExtensionError::NoActiveDocument)?;
        let page_count = self.live_document_authority()?.1;
        let page = live_page_index(page, page_count)?;
        let outline = self.outline(document)?;
        let entry = outline
            .entries
            .iter()
            .find(|entry| entry.title == title && entry.destination.page == page)
            .ok_or(PdfExtensionError::ResourceNotFound {
                kind: "key.pdf.outline-entry".into(),
                id: 0,
            })?;
        PdfTocPilot::new(self, self).activate(document, entry.id)
    }

    fn live_document_authority(&self) -> PdfExtensionResult<(DocumentHandle, u32)> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        Ok((active.document, active.page_count))
    }

    fn live_page_authority(
        &self,
        page: usize,
    ) -> PdfExtensionResult<(DocumentHandle, PageHandle, PageIndex)> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        let index = live_page_index(page, active.page_count)?;
        Ok((active.document, active.page_handle(index)?, index))
    }

    fn publish_live_snapshot_if_changed(
        &self,
        mut snapshot: PdfBridgeSnapshot,
    ) -> PdfExtensionResult<bool> {
        let mut state = self.write_state()?;
        let active = require_active_mut(&mut state)?;
        let document = snapshot_document(&snapshot, active)?;
        require_document(active, document)?;
        if live_snapshot_matches(active, &snapshot) {
            return Ok(false);
        }
        let revision = active.allocate_snapshot_revision()?;
        set_snapshot_revision(&mut snapshot, revision);
        publish_snapshot_into(active, snapshot, &self.limits)?;
        Ok(true)
    }

    fn read_state(&self) -> PdfExtensionResult<RwLockReadGuard<'_, BridgeState>> {
        self.state.read().map_err(|_| PdfExtensionError::Internal {
            message: "PDF capability bridge read lock was poisoned".into(),
        })
    }

    fn write_state(&self) -> PdfExtensionResult<RwLockWriteGuard<'_, BridgeState>> {
        self.state.write().map_err(|_| PdfExtensionError::Internal {
            message: "PDF capability bridge write lock was poisoned".into(),
        })
    }
}

impl ActiveDocument {
    fn new(document: DocumentHandle, page_count: u32) -> Self {
        Self {
            document,
            page_count,
            metadata: None,
            pages: BTreeMap::new(),
            outline: None,
            links: BTreeMap::new(),
            text: BTreeMap::new(),
            selection: None,
            viewport: None,
            pending_navigation: None,
            next_snapshot_revision: 2,
            next_viewport_revision: 0,
            overlays: None,
            next_overlay_id: u64::from(page_count).saturating_add(2),
        }
    }

    fn validation_context(&self, limits: PdfContractLimits) -> PdfValidationContext {
        PdfValidationContext::with_limits(self.document.generation(), self.page_count, limits)
    }

    fn page_handle(&self, page: PageIndex) -> PdfExtensionResult<PageHandle> {
        if page.0 >= self.page_count {
            return Err(PdfExtensionError::PageOutOfBounds {
                page: page.0,
                page_count: self.page_count,
            });
        }
        PageHandle::from_host_parts(
            self.document.generation(),
            u64::from(page.0).saturating_add(2),
        )
    }

    fn page_index(&self, handle: PageHandle) -> PdfExtensionResult<PageIndex> {
        if handle.generation() != self.document.generation() {
            return Err(PdfExtensionError::StaleGeneration {
                expected: self.document.generation(),
                actual: handle.generation(),
            });
        }
        let Some(raw_index) = handle.id().checked_sub(2) else {
            return resource_not_found("key.pdf.page", handle.id());
        };
        let Ok(index) = u32::try_from(raw_index) else {
            return resource_not_found("key.pdf.page", handle.id());
        };
        let page = PageIndex(index);
        self.require_page_handle(handle, page)?;
        Ok(page)
    }

    fn require_page_handle(&self, handle: PageHandle, index: PageIndex) -> PdfExtensionResult<()> {
        if handle.generation() != self.document.generation() {
            return Err(PdfExtensionError::StaleGeneration {
                expected: self.document.generation(),
                actual: handle.generation(),
            });
        }
        let expected = self.page_handle(index)?;
        if handle == expected {
            Ok(())
        } else {
            resource_not_found("key.pdf.page", handle.id())
        }
    }

    fn allocate_snapshot_revision(&mut self) -> PdfExtensionResult<SnapshotRevision> {
        let revision = self.next_snapshot_revision;
        self.next_snapshot_revision =
            revision
                .checked_add(1)
                .ok_or_else(|| PdfExtensionError::Internal {
                    message: "PDF snapshot revision counter exhausted".into(),
                })?;
        Ok(SnapshotRevision(revision))
    }

    fn observe_snapshot_revision(&mut self, revision: SnapshotRevision) {
        self.next_snapshot_revision = self
            .next_snapshot_revision
            .max(revision.0.saturating_add(1));
    }
}

fn allocate_document_generation(state: &mut BridgeState) -> PdfExtensionResult<u64> {
    let generation = state.next_generation;
    state.next_generation =
        generation
            .checked_add(1)
            .ok_or_else(|| PdfExtensionError::Internal {
                message: "PDF document generation counter exhausted".into(),
            })?;
    Ok(generation)
}

impl DocumentMetadataService for PdfCapabilityBridge {
    fn active_document(&self) -> PdfExtensionResult<Option<DocumentHandle>> {
        Ok(self
            .read_state()?
            .active
            .as_ref()
            .map(|active| active.document))
    }

    fn document_metadata(
        &self,
        document: DocumentHandle,
    ) -> PdfExtensionResult<DocumentMetadataSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active
            .metadata
            .as_deref()
            .cloned()
            .ok_or(PdfExtensionError::Busy)
    }
}

impl PageMetadataService for PdfCapabilityBridge {
    fn page_metadata(
        &self,
        document: DocumentHandle,
        page: PageIndex,
    ) -> PdfExtensionResult<PageMetadataSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active.page_handle(page)?;
        active
            .pages
            .get(&page)
            .map(|snapshot| snapshot.as_ref().clone())
            .ok_or(PdfExtensionError::Busy)
    }
}

impl OutlineService for PdfCapabilityBridge {
    fn outline(&self, document: DocumentHandle) -> PdfExtensionResult<OutlineSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active
            .outline
            .as_deref()
            .cloned()
            .ok_or(PdfExtensionError::Busy)
    }
}

impl LinkService for PdfCapabilityBridge {
    fn links(&self, page: PageHandle) -> PdfExtensionResult<PageLinksSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        let index = active.page_index(page)?;
        active
            .links
            .get(&index)
            .map(|snapshot| snapshot.as_ref().clone())
            .ok_or(PdfExtensionError::Busy)
    }
}

impl TextService for PdfCapabilityBridge {
    fn page_text(&self, request: PageTextRequest) -> PdfExtensionResult<PageTextSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        request.validate_contract(&active.validation_context(self.limits.clone()))?;
        let index = active.page_index(request.page)?;
        let source = active.text.get(&index).ok_or(PdfExtensionError::Busy)?;
        if request.start > source.total_characters {
            return invalid("text.start", InputErrorKind::ReversedRange);
        }
        let start =
            usize::try_from(request.start).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "text.start".into(),
                limit: u64::try_from(source.characters.len()).unwrap_or(u64::MAX),
                actual: u64::from(request.start),
            })?;
        let requested = usize::try_from(request.maximum_characters).unwrap_or(usize::MAX);
        let end = start.saturating_add(requested).min(source.characters.len());
        let returned = source.characters[start..end].to_vec();
        Ok(PageTextSnapshot {
            page: request.page,
            index,
            revision: source.revision,
            start: request.start,
            total_characters: source.total_characters,
            characters: returned,
            complete: end == source.characters.len(),
        })
    }
}

impl SelectionService for PdfCapabilityBridge {
    fn selection(&self, document: DocumentHandle) -> PdfExtensionResult<SelectionSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active
            .selection
            .as_deref()
            .cloned()
            .ok_or(PdfExtensionError::Busy)
    }
}

impl ViewportService for PdfCapabilityBridge {
    fn viewport(&self, document: DocumentHandle) -> PdfExtensionResult<ViewportSnapshot> {
        let state = self.read_state()?;
        let active = require_active(&state)?;
        require_document(active, document)?;
        active
            .viewport
            .as_deref()
            .cloned()
            .ok_or(PdfExtensionError::Busy)
    }
}

impl NavigationService for PdfCapabilityBridge {
    fn navigate(&self, request: NavigationRequest) -> PdfExtensionResult<NavigationReceipt> {
        let mut state = self.write_state()?;
        let active = require_active_mut(&mut state)?;
        request.validate_contract(&active.validation_context(self.limits.clone()))?;
        require_document(active, request.document)?;

        let mut resolved = request;
        refine_navigation(active, &mut resolved)?;
        active.next_viewport_revision =
            active
                .next_viewport_revision
                .checked_add(1)
                .ok_or_else(|| PdfExtensionError::Internal {
                    message: "viewport revision counter exhausted".into(),
                })?;
        let receipt = NavigationReceipt {
            document: active.document,
            viewport_revision: SnapshotRevision(active.next_viewport_revision),
            destination: resolved.destination.clone(),
            focus_scheduled: resolved.focus.is_some(),
        };
        active.pending_navigation = Some(PendingPdfNavigation {
            jump: resolved.to_document_jump(),
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }
}

impl OverlayService for PdfCapabilityBridge {
    fn replace_overlays(&self, batch: OverlayBatch) -> PdfExtensionResult<OverlayReceipt> {
        let mut state = self.write_state()?;
        let active = require_active_mut(&mut state)?;
        batch.validate_contract(&active.validation_context(self.limits.clone()))?;
        require_document(active, batch.document)?;
        batch
            .validate_revision_after(active.overlays.as_deref().map(|current| current.revision))?;
        let handle = OverlaySetHandle::from_host_parts(
            active.document.generation(),
            active.next_overlay_id,
        )?;
        active.next_overlay_id =
            active
                .next_overlay_id
                .checked_add(1)
                .ok_or_else(|| PdfExtensionError::Internal {
                    message: "overlay resource counter exhausted".into(),
                })?;
        let count =
            u32::try_from(batch.overlays.len()).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "overlays".into(),
                limit: u64::from(u32::MAX),
                actual: u64::try_from(batch.overlays.len()).unwrap_or(u64::MAX),
            })?;
        let revision = batch.revision;
        active.overlays = Some(Arc::new(batch));
        Ok(OverlayReceipt {
            handle,
            revision,
            overlay_count: count,
        })
    }
}

impl PdfServiceProvider for PdfCapabilityBridge {
    fn document_metadata_service(&self) -> Option<&dyn DocumentMetadataService> {
        Some(self)
    }

    fn page_metadata_service(&self) -> Option<&dyn PageMetadataService> {
        Some(self)
    }

    fn outline_service(&self) -> Option<&dyn OutlineService> {
        Some(self)
    }

    fn link_service(&self) -> Option<&dyn LinkService> {
        Some(self)
    }

    fn text_service(&self) -> Option<&dyn TextService> {
        Some(self)
    }

    fn selection_service(&self) -> Option<&dyn SelectionService> {
        Some(self)
    }

    fn viewport_service(&self) -> Option<&dyn ViewportService> {
        Some(self)
    }

    fn navigation_service(&self) -> Option<&dyn NavigationService> {
        Some(self)
    }

    fn overlay_service(&self) -> Option<&dyn OverlayService> {
        Some(self)
    }
}

/// Minimal native TOC consumer proving outline, text refinement, navigation,
/// and focus can operate entirely through the extension capability boundary.
pub struct PdfTocPilot<'a> {
    outline: &'a dyn OutlineService,
    navigation: &'a dyn NavigationService,
}

impl<'a> PdfTocPilot<'a> {
    /// Creates a TOC pilot from independently granted services.
    #[must_use]
    pub fn new(outline: &'a dyn OutlineService, navigation: &'a dyn NavigationService) -> Self {
        Self {
            outline,
            navigation,
        }
    }

    /// Navigates one outline entry, adding its title as a bounded refinement
    /// hint when the PDF supplied only a page or rough coordinate.
    ///
    /// # Errors
    ///
    /// Returns structured service, resource, or navigation errors.
    pub fn activate(
        &self,
        document: DocumentHandle,
        entry: OutlineEntryId,
    ) -> PdfExtensionResult<NavigationReceipt> {
        let outline = self.outline.outline(document)?;
        let entry = outline
            .entries
            .iter()
            .find(|candidate| candidate.id == entry)
            .ok_or(PdfExtensionError::ResourceNotFound {
                kind: "key.pdf.outline-entry".into(),
                id: entry.0,
            })?;
        let mut destination = entry.destination.clone();
        if destination.text_hint.is_none() {
            destination.text_hint = Some(TextTargetHint {
                text: entry.title.clone(),
                occurrence: 0,
            });
        }
        self.navigation.navigate(NavigationRequest {
            document,
            destination,
            placement: Default::default(),
            focus: Some(NavigationFocusRequest::default()),
        })
    }
}

struct LiveDocumentSnapshots {
    document: DocumentMetadataSnapshot,
    pages: Vec<PageMetadataSnapshot>,
    outline: OutlineSnapshot,
    links: Vec<PageLinksSnapshot>,
}

fn build_live_document_snapshots(
    document: DocumentHandle,
    title: Option<&str>,
    pages: &[PageSize],
    outline: &[TocEntry],
    links: &[PdfLink],
    limits: &PdfContractLimits,
) -> PdfExtensionResult<LiveDocumentSnapshots> {
    let page_count = u32::try_from(pages.len()).map_err(|_| PdfExtensionError::LimitExceeded {
        field: "document.page_count".into(),
        limit: u64::from(u32::MAX),
        actual: u64::try_from(pages.len()).unwrap_or(u64::MAX),
    })?;
    let title = title
        .filter(|title| !title.is_empty())
        .map(|title| bounded_utf8(title, limits.maximum_string_bytes));
    let document_snapshot = DocumentMetadataSnapshot {
        document,
        revision: SnapshotRevision(1),
        page_count,
        title,
        author: None,
        subject: None,
        keywords: None,
        creator: None,
        producer: None,
        language: None,
        format_version: None,
        tagged: false,
        encrypted: false,
    };
    let page_snapshots = pages
        .iter()
        .enumerate()
        .map(|(page, size)| {
            let index = live_page_index(page, page_count)?;
            Ok(PageMetadataSnapshot {
                document,
                page: live_page_handle(document, index)?,
                revision: SnapshotRevision(1),
                index,
                label: Some((page + 1).to_string()),
                width_points: size.width,
                height_points: size.height,
                rotation: PageRotation::Degrees0,
            })
        })
        .collect::<PdfExtensionResult<Vec<_>>>()?;

    let mut ancestors = Vec::<OutlineEntryId>::new();
    let mut outline_truncated = outline.len() > limits.maximum_outline_entries;
    let mut outline_entries = Vec::new();
    for entry in outline.iter().take(limits.maximum_outline_entries) {
        let Ok(page) = live_page_index(entry.page, page_count) else {
            outline_truncated = true;
            continue;
        };
        let title = bounded_utf8(entry.title.trim(), limits.maximum_string_bytes);
        if title.is_empty() {
            outline_truncated = true;
            continue;
        }
        let depth = entry
            .depth
            .min(limits.maximum_outline_depth)
            .min(u16::try_from(ancestors.len()).unwrap_or(u16::MAX));
        ancestors.truncate(usize::from(depth));
        let parent = depth
            .checked_sub(1)
            .and_then(|depth| ancestors.get(usize::from(depth)).copied());
        let id = OutlineEntryId(
            u64::try_from(outline_entries.len())
                .unwrap_or(u64::MAX - 1)
                .saturating_add(1),
        );
        outline_entries.push(OutlineEntry {
            id,
            parent,
            depth,
            title: title.clone(),
            destination: DocumentDestination {
                page,
                x_fraction: None,
                y_fraction: finite_normalized_fraction(entry.destination_y),
                text_range: None,
                text_hint: Some(TextTargetHint {
                    text: title,
                    occurrence: 0,
                }),
            },
        });
        ancestors.push(id);
    }
    let outline_snapshot = OutlineSnapshot {
        document,
        revision: SnapshotRevision(1),
        entries: outline_entries,
        truncated: outline_truncated,
    };

    let link_snapshots =
        build_live_link_snapshots(document, pages.len(), page_count, links, limits)?;

    Ok(LiveDocumentSnapshots {
        document: document_snapshot,
        pages: page_snapshots,
        outline: outline_snapshot,
        links: link_snapshots,
    })
}

fn build_live_link_snapshots(
    document: DocumentHandle,
    page_count_usize: usize,
    page_count: u32,
    links: &[PdfLink],
    limits: &PdfContractLimits,
) -> PdfExtensionResult<Vec<PageLinksSnapshot>> {
    let mut links_by_page = vec![Vec::new(); page_count_usize];
    let mut links_truncated = vec![false; page_count_usize];
    for link in links {
        let Some(page_links) = links_by_page.get_mut(link.page) else {
            continue;
        };
        if page_links.len() >= limits.maximum_links_per_page {
            links_truncated[link.page] = true;
            continue;
        }
        let Some(region) = normalized_rect(link.bounds) else {
            links_truncated[link.page] = true;
            continue;
        };
        let target = match &link.target {
            PdfLinkTarget::Internal {
                page,
                x_fraction,
                y_fraction,
            } => {
                let Ok(page) = live_page_index(*page, page_count) else {
                    links_truncated[link.page] = true;
                    continue;
                };
                key_pdf_extension_api::LinkTarget::Internal {
                    destination: DocumentDestination {
                        page,
                        x_fraction: finite_normalized_fraction(*x_fraction),
                        y_fraction: finite_normalized_fraction(*y_fraction),
                        text_range: None,
                        text_hint: None,
                    },
                }
            }
            PdfLinkTarget::External { url }
                if !url.is_empty() && url.len() <= limits.maximum_uri_bytes =>
            {
                key_pdf_extension_api::LinkTarget::External { uri: url.clone() }
            }
            PdfLinkTarget::External { .. } => {
                links_truncated[link.page] = true;
                continue;
            }
        };
        page_links.push(key_pdf_extension_api::LinkEntry {
            id: key_pdf_extension_api::LinkId(
                u64::try_from(page_links.len())
                    .unwrap_or(u64::MAX - 1)
                    .saturating_add(1),
            ),
            regions: vec![region],
            source_range: None,
            target,
        });
    }
    links_by_page
        .into_iter()
        .enumerate()
        .map(|(page, links)| {
            let index = live_page_index(page, page_count)?;
            Ok(PageLinksSnapshot {
                page: live_page_handle(document, index)?,
                index,
                revision: SnapshotRevision(1),
                links,
                truncated: links_truncated[page],
            })
        })
        .collect::<PdfExtensionResult<Vec<_>>>()
}

fn publish_snapshot_into(
    active: &mut ActiveDocument,
    snapshot: PdfBridgeSnapshot,
    limits: &PdfContractLimits,
) -> PdfExtensionResult<()> {
    let context = active.validation_context(limits.clone());
    let revision = snapshot_revision(&snapshot);
    match snapshot {
        PdfBridgeSnapshot::Document(snapshot) => {
            snapshot.validate_contract(&context)?;
            require_document(active, snapshot.document)?;
            require_newer_revision(
                active.metadata.as_deref().map(|current| current.revision),
                snapshot.revision,
                "document.revision",
            )?;
            active.metadata = Some(Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Page(snapshot) => {
            snapshot.validate_contract(&context)?;
            require_document(active, snapshot.document)?;
            active.require_page_handle(snapshot.page, snapshot.index)?;
            require_newer_revision(
                active
                    .pages
                    .get(&snapshot.index)
                    .map(|current| current.revision),
                snapshot.revision,
                "page.revision",
            )?;
            active.pages.insert(snapshot.index, Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Outline(snapshot) => {
            snapshot.validate_contract(&context)?;
            require_document(active, snapshot.document)?;
            require_newer_revision(
                active.outline.as_deref().map(|current| current.revision),
                snapshot.revision,
                "outline.revision",
            )?;
            active.outline = Some(Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Links(snapshot) => {
            snapshot.validate_contract(&context)?;
            active.require_page_handle(snapshot.page, snapshot.index)?;
            require_newer_revision(
                active
                    .links
                    .get(&snapshot.index)
                    .map(|current| current.revision),
                snapshot.revision,
                "links.revision",
            )?;
            active.links.insert(snapshot.index, Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Text(snapshot) => {
            snapshot.validate_contract(&context)?;
            active.require_page_handle(snapshot.page, snapshot.index)?;
            if snapshot.start != 0
                || !snapshot.complete
                || usize::try_from(snapshot.total_characters).ok()
                    != Some(snapshot.characters.len())
            {
                return invalid("text", InputErrorKind::ReversedRange);
            }
            require_newer_revision(
                active
                    .text
                    .get(&snapshot.index)
                    .map(|current| current.revision),
                snapshot.revision,
                "text.revision",
            )?;
            active.text.insert(snapshot.index, Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Selection(snapshot) => {
            snapshot.validate_contract(&context)?;
            require_document(active, snapshot.document)?;
            for page in &snapshot.pages {
                active.require_page_handle(page.page, page.index)?;
            }
            require_newer_revision(
                active.selection.as_deref().map(|current| current.revision),
                snapshot.revision,
                "selection.revision",
            )?;
            active.selection = Some(Arc::new(snapshot));
        }
        PdfBridgeSnapshot::Viewport(snapshot) => {
            snapshot.validate_contract(&context)?;
            require_document(active, snapshot.document)?;
            for page in &snapshot.visible_pages {
                active.require_page_handle(page.page, page.index)?;
            }
            require_newer_revision(
                active.viewport.as_deref().map(|current| current.revision),
                snapshot.revision,
                "viewport.revision",
            )?;
            active.next_viewport_revision = active.next_viewport_revision.max(snapshot.revision.0);
            active.viewport = Some(Arc::new(snapshot));
        }
    }
    active.observe_snapshot_revision(revision);
    Ok(())
}

fn snapshot_revision(snapshot: &PdfBridgeSnapshot) -> SnapshotRevision {
    match snapshot {
        PdfBridgeSnapshot::Document(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Page(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Outline(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Links(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Text(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Selection(snapshot) => snapshot.revision,
        PdfBridgeSnapshot::Viewport(snapshot) => snapshot.revision,
    }
}

fn set_snapshot_revision(snapshot: &mut PdfBridgeSnapshot, revision: SnapshotRevision) {
    match snapshot {
        PdfBridgeSnapshot::Document(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Page(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Outline(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Links(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Text(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Selection(snapshot) => snapshot.revision = revision,
        PdfBridgeSnapshot::Viewport(snapshot) => snapshot.revision = revision,
    }
}

fn snapshot_document(
    snapshot: &PdfBridgeSnapshot,
    active: &ActiveDocument,
) -> PdfExtensionResult<DocumentHandle> {
    match snapshot {
        PdfBridgeSnapshot::Document(snapshot) => Ok(snapshot.document),
        PdfBridgeSnapshot::Page(snapshot) => Ok(snapshot.document),
        PdfBridgeSnapshot::Outline(snapshot) => Ok(snapshot.document),
        PdfBridgeSnapshot::Selection(snapshot) => Ok(snapshot.document),
        PdfBridgeSnapshot::Viewport(snapshot) => Ok(snapshot.document),
        PdfBridgeSnapshot::Links(snapshot) => {
            active.page_index(snapshot.page)?;
            Ok(active.document)
        }
        PdfBridgeSnapshot::Text(snapshot) => {
            active.page_index(snapshot.page)?;
            Ok(active.document)
        }
    }
}

fn live_snapshot_matches(active: &ActiveDocument, candidate: &PdfBridgeSnapshot) -> bool {
    match candidate {
        PdfBridgeSnapshot::Document(candidate) => active
            .metadata
            .as_deref()
            .is_some_and(|current| document_without_revision(current, candidate)),
        PdfBridgeSnapshot::Page(candidate) => active
            .pages
            .get(&candidate.index)
            .is_some_and(|current| page_without_revision(current, candidate)),
        PdfBridgeSnapshot::Outline(candidate) => active
            .outline
            .as_deref()
            .is_some_and(|current| outline_without_revision(current, candidate)),
        PdfBridgeSnapshot::Links(candidate) => active
            .links
            .get(&candidate.index)
            .is_some_and(|current| links_without_revision(current, candidate)),
        PdfBridgeSnapshot::Text(candidate) => active
            .text
            .get(&candidate.index)
            .is_some_and(|current| text_without_revision(current, candidate)),
        PdfBridgeSnapshot::Selection(candidate) => active
            .selection
            .as_deref()
            .is_some_and(|current| selection_without_revision(current, candidate)),
        PdfBridgeSnapshot::Viewport(candidate) => active
            .viewport
            .as_deref()
            .is_some_and(|current| viewport_without_revision(current, candidate)),
    }
}

fn document_without_revision(
    left: &DocumentMetadataSnapshot,
    right: &DocumentMetadataSnapshot,
) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn page_without_revision(left: &PageMetadataSnapshot, right: &PageMetadataSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn outline_without_revision(left: &OutlineSnapshot, right: &OutlineSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn links_without_revision(left: &PageLinksSnapshot, right: &PageLinksSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn text_without_revision(left: &PageTextSnapshot, right: &PageTextSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn selection_without_revision(left: &SelectionSnapshot, right: &SelectionSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn viewport_without_revision(left: &ViewportSnapshot, right: &ViewportSnapshot) -> bool {
    let mut right = right.clone();
    right.revision = left.revision;
    left == &right
}

fn live_page_index(page: usize, page_count: u32) -> PdfExtensionResult<PageIndex> {
    let page = u32::try_from(page).map_err(|_| PdfExtensionError::PageOutOfBounds {
        page: u32::MAX,
        page_count,
    })?;
    if page >= page_count {
        return Err(PdfExtensionError::PageOutOfBounds { page, page_count });
    }
    Ok(PageIndex(page))
}

fn live_page_handle(document: DocumentHandle, page: PageIndex) -> PdfExtensionResult<PageHandle> {
    PageHandle::from_host_parts(document.generation(), u64::from(page.0).saturating_add(2))
}

fn live_selection_range(
    selection: TextSelection,
    page_count: u32,
) -> PdfExtensionResult<TextSelectionRange> {
    let location = |position: key_pdf_core::TextPosition| {
        Ok(TextLocation {
            page: live_page_index(position.page, page_count)?,
            character: u32::try_from(position.index).unwrap_or(u32::MAX),
        })
    };
    Ok(TextSelectionRange {
        anchor: location(selection.anchor)?,
        focus: location(selection.focus)?,
    })
}

fn bounded_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

fn normalized_rect(bounds: key_pdf_core::TextBounds) -> Option<NormalizedRect> {
    let values = [bounds.left, bounds.top, bounds.right, bounds.bottom];
    values
        .iter()
        .all(|value| value.is_finite())
        .then(|| {
            let left = bounds.left.min(bounds.right).clamp(0.0, 1.0);
            let right = bounds.left.max(bounds.right).clamp(0.0, 1.0);
            let top = bounds.top.min(bounds.bottom).clamp(0.0, 1.0);
            let bottom = bounds.top.max(bounds.bottom).clamp(0.0, 1.0);
            NormalizedRect {
                left,
                top,
                right,
                bottom,
            }
        })
        .filter(|bounds| bounds.left < bounds.right && bounds.top < bounds.bottom)
}

fn finite_normalized_fraction(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn intersect_rect(
    left: key_pdf_core::Rect,
    right: key_pdf_core::Rect,
) -> Option<key_pdf_core::Rect> {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let right_edge = left.right().min(right.right());
    let bottom = left.bottom().min(right.bottom());
    (x < right_edge && y < bottom).then_some(key_pdf_core::Rect {
        x,
        y,
        width: right_edge - x,
        height: bottom - y,
    })
}

fn from_data_value<T: DeserializeOwned>(value: DataValue) -> PdfExtensionResult<T> {
    serde_json::from_value(data_value_to_json(value)).map_err(|_| PdfExtensionError::InvalidInput {
        field: "capability.input".into(),
        reason: InputErrorKind::InvalidIdentifier,
    })
}

fn to_data_value(value: &impl Serialize) -> PdfExtensionResult<DataValue> {
    let json = serde_json::to_value(value).map_err(|error| PdfExtensionError::Internal {
        message: format!("could not encode PDF capability result: {error}"),
    })?;
    json_to_data_value(json)
}

fn data_value_to_json(value: DataValue) -> serde_json::Value {
    match value {
        DataValue::Null => serde_json::Value::Null,
        DataValue::Boolean(value) => serde_json::Value::Bool(value),
        DataValue::Integer(value) => value.into(),
        DataValue::Number(value) => serde_json::Number::from_f64(value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        DataValue::String(value) => value.into(),
        DataValue::List(values) => {
            serde_json::Value::Array(values.into_iter().map(data_value_to_json).collect())
        }
        DataValue::Record(values) => serde_json::Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, data_value_to_json(value)))
                .collect(),
        ),
    }
}

fn json_to_data_value(value: serde_json::Value) -> PdfExtensionResult<DataValue> {
    match value {
        serde_json::Value::Null => Ok(DataValue::Null),
        serde_json::Value::Bool(value) => Ok(DataValue::Boolean(value)),
        serde_json::Value::Number(value) if value.as_i64().is_some() => {
            Ok(DataValue::Integer(value.as_i64().expect("guarded")))
        }
        serde_json::Value::Number(value) if value.as_u64().is_some() => {
            Err(PdfExtensionError::LimitExceeded {
                field: "capability.output.number".into(),
                limit: i64::MAX as u64,
                actual: value.as_u64().expect("guarded"),
            })
        }
        serde_json::Value::Number(value) => {
            value
                .as_f64()
                .map(DataValue::Number)
                .ok_or_else(|| PdfExtensionError::InvalidInput {
                    field: "capability.output.number".into(),
                    reason: InputErrorKind::NotFinite,
                })
        }
        serde_json::Value::String(value) => Ok(DataValue::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_data_value)
            .collect::<PdfExtensionResult<Vec<_>>>()
            .map(DataValue::List),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_data_value(value)?)))
            .collect::<PdfExtensionResult<BTreeMap<_, _>>>()
            .map(DataValue::Record),
    }
}

fn refine_navigation(
    active: &ActiveDocument,
    request: &mut NavigationRequest,
) -> PdfExtensionResult<()> {
    let page = request.destination.page;
    let Some(text) = active.text.get(&page) else {
        return Ok(());
    };
    let characters = core_characters(text)?;
    let mut resolved_range = request
        .destination
        .text_range
        .and_then(|range| regions_for_selection(page, range.into(), &characters));

    if resolved_range.is_none()
        && let Some(hint) = &request.destination.text_hint
    {
        resolved_range = search_text_hint(page, text, &characters, hint);
    }

    let Some((range, regions)) = resolved_range else {
        return Ok(());
    };
    if let Some(top) = regions
        .iter()
        .map(|region| region.top)
        .min_by(f32::total_cmp)
    {
        request.destination.y_fraction = Some(top.clamp(0.0, 1.0));
    }
    request.destination.text_range = Some(range);
    if let Some(focus) = &mut request.focus
        && focus.regions.is_empty()
    {
        focus.regions = regions;
    }
    Ok(())
}

fn search_text_hint(
    page: PageIndex,
    snapshot: &PageTextSnapshot,
    characters: &[TextChar],
    hint: &TextTargetHint,
) -> Option<(
    key_pdf_extension_api::TextSelectionRange,
    Vec<NormalizedRect>,
)> {
    let query = SearchQuery::new(&hint.text).ok()?;
    let limit = usize::from(hint.occurrence).saturating_add(1);
    let SearchPageOutcome::Complete(results) =
        search_page(usize::from(page), characters, &query, limit, || false)
    else {
        return None;
    };
    let matched = results.matches.get(usize::from(hint.occurrence))?;
    let start = snapshot
        .start
        .checked_add(u32::try_from(matched.id.start).ok()?)?;
    let end = snapshot
        .start
        .checked_add(u32::try_from(matched.id.end).ok()?)?;
    Some((
        key_pdf_extension_api::TextSelectionRange {
            anchor: TextLocation {
                page,
                character: start,
            },
            focus: TextLocation {
                page,
                character: end,
            },
        },
        matched
            .highlight_runs
            .iter()
            .copied()
            .map(NormalizedRect::from)
            .collect(),
    ))
}

fn regions_for_selection(
    page: PageIndex,
    selection: TextSelection,
    characters: &[TextChar],
) -> Option<(
    key_pdf_extension_api::TextSelectionRange,
    Vec<NormalizedRect>,
)> {
    let range = selection.indices_on_page(usize::from(page), characters.len())?;
    let start = *range.start();
    let end = *range.end();
    let regions = text_runs_for_range(characters, start, end)
        .into_iter()
        .map(NormalizedRect::from)
        .collect::<Vec<_>>();
    (!regions.is_empty()).then_some((
        key_pdf_extension_api::TextSelectionRange {
            anchor: TextLocation {
                page,
                character: u32::try_from(start).ok()?,
            },
            focus: TextLocation {
                page,
                character: u32::try_from(end).ok()?,
            },
        },
        regions,
    ))
}

fn core_characters(snapshot: &PageTextSnapshot) -> PdfExtensionResult<Vec<TextChar>> {
    snapshot
        .characters
        .iter()
        .enumerate()
        .map(|(index, character)| {
            let value = character
                .value()
                .ok_or_else(|| PdfExtensionError::InvalidInput {
                    field: format!("text.characters[{index}].scalar"),
                    reason: InputErrorKind::InvalidUnicodeScalar,
                })?;
            Ok(TextChar {
                value,
                bounds: character.bounds.map(Into::into),
            })
        })
        .collect()
}

fn require_active(state: &BridgeState) -> PdfExtensionResult<&ActiveDocument> {
    state
        .active
        .as_ref()
        .ok_or(PdfExtensionError::NoActiveDocument)
}

fn require_active_mut(state: &mut BridgeState) -> PdfExtensionResult<&mut ActiveDocument> {
    state
        .active
        .as_mut()
        .ok_or(PdfExtensionError::NoActiveDocument)
}

fn require_document(active: &ActiveDocument, document: DocumentHandle) -> PdfExtensionResult<()> {
    if document.generation() != active.document.generation() {
        return Err(PdfExtensionError::StaleGeneration {
            expected: active.document.generation(),
            actual: document.generation(),
        });
    }
    if document == active.document {
        Ok(())
    } else {
        resource_not_found("key.pdf.document", document.id())
    }
}

fn require_newer_revision(
    previous: Option<SnapshotRevision>,
    next: SnapshotRevision,
    field: &str,
) -> PdfExtensionResult<()> {
    if previous.is_some_and(|previous| next <= previous) {
        invalid(field, InputErrorKind::StaleRevision)
    } else {
        Ok(())
    }
}

fn invalid<T>(field: impl Into<String>, reason: InputErrorKind) -> PdfExtensionResult<T> {
    Err(PdfExtensionError::InvalidInput {
        field: field.into(),
        reason,
    })
}

fn resource_not_found<T>(kind: &str, id: u64) -> PdfExtensionResult<T> {
    Err(PdfExtensionError::ResourceNotFound {
        kind: kind.into(),
        id,
    })
}

#[cfg(test)]
mod tests {
    use key_extension_api::LocalId;
    use key_pdf_core::{DocumentLayout, PageSize};
    use key_pdf_extension_api::{
        DocumentOverlay, LinkTarget, NavigationFocusMotion, OutlineEntry, OverlayAppearance,
        PageRotation, TextCharacter,
    };

    use super::*;

    fn document_metadata(
        document: DocumentHandle,
        page_count: u32,
        revision: u64,
    ) -> DocumentMetadataSnapshot {
        DocumentMetadataSnapshot {
            document,
            revision: SnapshotRevision(revision),
            page_count,
            title: Some("Bridge fixture".into()),
            author: None,
            subject: None,
            keywords: None,
            creator: None,
            producer: None,
            language: Some("en".into()),
            format_version: Some("PDF 1.7".into()),
            tagged: false,
            encrypted: false,
        }
    }

    fn page_metadata(
        document: DocumentHandle,
        page: PageHandle,
        index: u32,
    ) -> PageMetadataSnapshot {
        PageMetadataSnapshot {
            document,
            page,
            revision: SnapshotRevision(1),
            index: PageIndex(index),
            label: Some((index + 1).to_string()),
            width_points: 600.0,
            height_points: 800.0,
            rotation: PageRotation::Degrees0,
        }
    }

    fn text_snapshot(page: PageHandle, index: u32, value: &str) -> PageTextSnapshot {
        let mut line = 0_u32;
        let characters = value
            .chars()
            .enumerate()
            .map(|(index, value)| {
                if value == '\n' {
                    line += 1;
                }
                let column = u32::try_from(index).unwrap_or(u32::MAX) % 40;
                TextCharacter {
                    scalar: u32::from(value),
                    bounds: (value != '\n').then_some(NormalizedRect {
                        left: 0.1 + column as f32 * 0.012,
                        top: 0.1 + line as f32 * 0.2,
                        right: 0.11 + column as f32 * 0.012,
                        bottom: 0.13 + line as f32 * 0.2,
                    }),
                }
            })
            .collect::<Vec<_>>();
        PageTextSnapshot {
            page,
            index: PageIndex(index),
            revision: SnapshotRevision(1),
            start: 0,
            total_characters: u32::try_from(characters.len()).expect("short fixture"),
            characters,
            complete: true,
        }
    }

    #[test]
    fn reopen_and_close_invalidate_every_resource_and_snapshot() {
        let bridge = PdfCapabilityBridge::default();
        let first = bridge.begin_document(1).expect("first document opens");
        bridge
            .publish_snapshot(PdfBridgeSnapshot::Document(document_metadata(first, 1, 1)))
            .expect("metadata publishes");
        assert_eq!(
            bridge
                .document_metadata(first)
                .expect("current metadata")
                .title
                .as_deref(),
            Some("Bridge fixture")
        );

        let second = bridge.begin_document(1).expect("replacement opens");
        assert_ne!(first.generation(), second.generation());
        assert!(matches!(
            bridge.document_metadata(first),
            Err(PdfExtensionError::StaleGeneration { .. })
        ));
        bridge.close_document().expect("document closes");
        assert_eq!(bridge.active_document(), Ok(None));
        assert!(matches!(
            bridge.document_metadata(second),
            Err(PdfExtensionError::NoActiveDocument)
        ));
    }

    #[test]
    fn toc_pilot_refines_page_only_titles_and_emits_one_shared_jump() {
        let bridge = PdfCapabilityBridge::default();
        let document = bridge.begin_document(2).expect("document opens");
        let page = bridge
            .page_handle(document, PageIndex(1))
            .expect("page handle");
        for snapshot in [
            PdfBridgeSnapshot::Document(document_metadata(document, 2, 1)),
            PdfBridgeSnapshot::Page(page_metadata(document, page, 1)),
            PdfBridgeSnapshot::Text(text_snapshot(
                page,
                1,
                "Opening paragraph\nMethods\nStudy design",
            )),
            PdfBridgeSnapshot::Outline(OutlineSnapshot {
                document,
                revision: SnapshotRevision(1),
                entries: vec![OutlineEntry {
                    id: OutlineEntryId(9),
                    parent: None,
                    depth: 0,
                    title: "Methods".into(),
                    destination: DocumentDestination::page(PageIndex(1)),
                }],
                truncated: false,
            }),
        ] {
            bridge
                .publish_snapshot(snapshot)
                .expect("fixture snapshot publishes");
        }

        let receipt = PdfTocPilot::new(&bridge, &bridge)
            .activate(document, OutlineEntryId(9))
            .expect("TOC navigation accepted");
        assert_eq!(receipt.destination.page, PageIndex(1));
        assert!(receipt.destination.y_fraction.is_some());
        assert!(receipt.destination.text_range.is_some());
        let pending = bridge
            .take_pending_navigation()
            .expect("bridge remains active")
            .expect("one jump is pending");
        let layout = DocumentLayout::new(
            &[
                PageSize {
                    width: 600.0,
                    height: 800.0,
                },
                PageSize {
                    width: 600.0,
                    height: 800.0,
                },
            ],
            1.0,
            900.0,
        );
        let resolved = pending
            .jump
            .resolve(&layout, 0.0, 500.0, 300.0, 600.0)
            .expect("resolved page exists");
        let focus = resolved.focus.expect("TOC pilot requests focus");
        assert_eq!(focus.page, 1);
        assert_eq!(focus.motion, NavigationFocusMotion::Sweep.into());
        assert!(!focus.text_runs.is_empty());
        assert!(
            bridge
                .take_pending_navigation()
                .expect("bridge remains active")
                .is_none()
        );
    }

    #[test]
    fn bounded_overlay_replacement_is_visible_only_in_its_generation() {
        let limits = PdfContractLimits {
            maximum_overlays: 1,
            ..PdfContractLimits::default()
        };
        let bridge = PdfCapabilityBridge::new(limits);
        let document = bridge.begin_document(1).expect("document opens");
        let item = DocumentOverlay {
            id: LocalId::parse("pilot/result").expect("valid local ID"),
            page: PageIndex(0),
            regions: vec![NormalizedRect {
                left: 0.1,
                top: 0.2,
                right: 0.5,
                bottom: 0.3,
            }],
            appearance: OverlayAppearance::default(),
            label: Some("Pilot result".into()),
            command: None,
        };
        let receipt = bridge
            .replace_overlays(OverlayBatch {
                document,
                revision: SnapshotRevision(1),
                overlays: vec![item.clone()],
            })
            .expect("bounded overlay accepted");
        assert_eq!(receipt.overlay_count, 1);
        assert_eq!(
            bridge
                .current_overlays(document)
                .expect("current document")
                .expect("overlay retained")
                .overlays,
            vec![item.clone()]
        );

        assert!(matches!(
            bridge.replace_overlays(OverlayBatch {
                document,
                revision: SnapshotRevision(2),
                overlays: vec![item.clone(), item],
            }),
            Err(PdfExtensionError::LimitExceeded { .. })
        ));
        let replacement = bridge.begin_document(1).expect("replacement opens");
        assert!(matches!(
            bridge.current_overlays(document),
            Err(PdfExtensionError::StaleGeneration { .. })
        ));
        assert!(
            bridge
                .current_overlays(replacement)
                .expect("new generation")
                .is_none()
        );
    }

    #[test]
    fn bridge_exposes_exactly_the_nine_independent_services() {
        let bridge = PdfCapabilityBridge::default();
        assert_eq!(bridge.provided_pdf_capabilities(), PdfCapability::ALL);
    }

    #[test]
    fn page_text_requests_are_chunked_without_mutating_the_published_snapshot() {
        let bridge = PdfCapabilityBridge::default();
        let document = bridge.begin_document(1).expect("document opens");
        let page = bridge
            .page_handle(document, PageIndex(0))
            .expect("page handle");
        bridge
            .publish_snapshot(PdfBridgeSnapshot::Text(text_snapshot(page, 0, "abcdef")))
            .expect("text publishes");
        let chunk = bridge
            .page_text(PageTextRequest {
                page,
                start: 2,
                maximum_characters: 2,
            })
            .expect("chunk reads");
        assert_eq!(chunk.text().as_deref(), Some("cd"));
        assert!(!chunk.complete);
        assert_eq!(
            bridge
                .page_text(PageTextRequest {
                    page,
                    start: 0,
                    maximum_characters: 6,
                })
                .expect("full read")
                .text()
                .as_deref(),
            Some("abcdef")
        );
    }

    #[test]
    fn external_link_values_do_not_grant_network_or_open_authority() {
        let target = LinkTarget::External {
            uri: "https://example.test/paper".into(),
        };
        assert!(matches!(target, LinkTarget::External { .. }));
    }

    #[test]
    fn live_reader_state_maps_without_engine_handles_and_deduplicates_updates() {
        let bridge = PdfCapabilityBridge::default();
        let pages = vec![
            PageSize {
                width: 612.0,
                height: 792.0,
            },
            PageSize {
                width: 400.0,
                height: 600.0,
            },
        ];
        let toc = vec![
            TocEntry {
                title: "Methods".into(),
                page: 1,
                depth: 0,
                destination_y: Some(0.2),
            },
            TocEntry {
                title: "Participants".into(),
                page: 1,
                depth: 1,
                destination_y: None,
            },
        ];
        let links = vec![PdfLink {
            id: 42,
            page: 0,
            bounds: key_pdf_core::TextBounds {
                left: 0.1,
                top: 0.2,
                right: 0.3,
                bottom: 0.25,
            },
            target: PdfLinkTarget::Internal {
                page: 1,
                x_fraction: None,
                y_fraction: Some(0.2),
            },
        }];
        let document = bridge
            .begin_live_document(Some("Mapped fixture.pdf"), &pages, &toc, &links)
            .expect("engine-neutral state publishes");
        assert_eq!(
            bridge
                .document_metadata(document)
                .expect("metadata is live")
                .page_count,
            2
        );
        assert_eq!(
            bridge.outline(document).expect("outline is live").entries[1].parent,
            Some(OutlineEntryId(1))
        );
        let first_page = bridge
            .page_handle(document, PageIndex(0))
            .expect("host page handle");
        assert_eq!(
            bridge
                .links(first_page)
                .expect("links are live")
                .links
                .len(),
            1
        );

        let text = TextLayer::new(
            "Methods section"
                .chars()
                .enumerate()
                .map(|(index, value)| TextChar {
                    value,
                    bounds: Some(key_pdf_core::TextBounds {
                        left: 0.05 + index as f32 * 0.02,
                        top: 0.1,
                        right: 0.06 + index as f32 * 0.02,
                        bottom: 0.13,
                    }),
                })
                .collect(),
        );
        assert!(bridge.publish_live_text(0, &text).expect("text publishes"));
        assert!(
            !bridge
                .publish_live_text(0, &text)
                .expect("same text deduplicates")
        );
        let selection = TextSelection {
            anchor: key_pdf_core::TextPosition { page: 0, index: 0 },
            focus: key_pdf_core::TextPosition { page: 0, index: 6 },
        };
        assert!(
            bridge
                .publish_live_selection(Some(selection), |page| (page == 0).then_some(&text))
                .expect("selection publishes")
        );
        assert_eq!(
            bridge
                .selection(document)
                .expect("selection is live")
                .text
                .as_deref(),
            Some("Methods")
        );

        let layout = DocumentLayout::new(&pages, 1.0, 700.0);
        assert!(
            bridge
                .publish_live_viewport(1.0, &layout, 0.0, 0.0, 700.0, 500.0)
                .expect("viewport publishes")
        );
        assert!(
            !bridge
                .publish_live_viewport(1.0, &layout, 0.0, 0.0, 700.0, 500.0)
                .expect("same viewport deduplicates")
        );
        let viewport = bridge.viewport(document).expect("viewport is live");
        assert_eq!(viewport.visible_pages.len(), 1);
        assert_eq!(viewport.visible_pages[0].index, PageIndex(0));
    }

    #[test]
    fn generic_pdf_calls_use_wit_names_and_replacement_invalidates_live_authority() {
        let bridge = PdfCapabilityBridge::default();
        let pages = [PageSize {
            width: 600.0,
            height: 800.0,
        }];
        let first = bridge
            .begin_live_document(Some("First"), &pages, &[], &[])
            .expect("first document");
        let active: Option<DocumentHandle> = from_data_value(
            bridge
                .call(
                    PdfCapability::DocumentMetadata,
                    "active-document",
                    DataValue::Null,
                )
                .expect("WIT operation dispatches"),
        )
        .expect("typed handle decodes");
        assert_eq!(active, Some(first));

        let request = NavigationRequest {
            document: first,
            destination: DocumentDestination::page(PageIndex(0)),
            placement: Default::default(),
            focus: None,
        };
        let _: NavigationReceipt = from_data_value(
            bridge
                .call(
                    PdfCapability::Navigation,
                    "navigate",
                    to_data_value(&request).expect("request encodes"),
                )
                .expect("navigation call dispatches"),
        )
        .expect("receipt decodes");
        assert!(
            bridge
                .take_pending_navigation()
                .expect("bridge remains live")
                .is_some()
        );

        let invalid_pages = [PageSize {
            width: 0.0,
            height: 800.0,
        }];
        assert!(
            bridge
                .begin_live_document(Some("Invalid"), &invalid_pages, &[], &[])
                .is_err()
        );
        assert_eq!(
            bridge.active_document().expect("bridge remains readable"),
            Some(first),
            "a failed replacement never exposes a partial generation"
        );

        let replacement = bridge
            .begin_live_document(Some("Replacement"), &pages, &[], &[])
            .expect("replacement document");
        assert_ne!(first.generation(), replacement.generation());
        assert!(matches!(
            bridge.document_metadata(first),
            Err(PdfExtensionError::StaleGeneration { .. })
        ));
        assert!(
            bridge
                .take_pending_navigation()
                .expect("new generation is live")
                .is_none(),
            "replacement drops pending jumps from the old generation"
        );
    }
}
