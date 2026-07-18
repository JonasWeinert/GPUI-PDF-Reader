//! Consumer-facing contract and shared-navigation compatibility tests.

use key_extension_api::{GenerationId, LocalId};
use key_pdf_core::{DocumentLayout, NavigationFocusMotion as CoreMotion, PageSize};
use key_pdf_extension_api::{
    DocumentDestination, DocumentHandle, DocumentMetadataService, DocumentMetadataSnapshot,
    NavigationFocusMotion, NavigationFocusRequest, NavigationFocusTone, NavigationPlacement,
    NavigationRequest, NormalizedRect, PageIndex, PdfCapability, PdfExtensionResult,
    PdfServiceProvider, SnapshotRevision,
};

fn document(generation: u64, id: u64) -> DocumentHandle {
    DocumentHandle::from_host_parts(GenerationId(generation), id).expect("valid fixture handle")
}

struct MetadataOnlyProvider {
    document: DocumentHandle,
}

impl DocumentMetadataService for MetadataOnlyProvider {
    fn active_document(&self) -> PdfExtensionResult<Option<DocumentHandle>> {
        Ok(Some(self.document))
    }

    fn document_metadata(
        &self,
        document: DocumentHandle,
    ) -> PdfExtensionResult<DocumentMetadataSnapshot> {
        assert_eq!(document, self.document);
        Ok(DocumentMetadataSnapshot {
            document,
            revision: SnapshotRevision(3),
            page_count: 2,
            title: Some("A transport-neutral PDF".into()),
            author: None,
            subject: None,
            keywords: None,
            creator: None,
            producer: None,
            language: Some("en".into()),
            format_version: Some("PDF 1.7".into()),
            tagged: true,
            encrypted: false,
        })
    }
}

impl PdfServiceProvider for MetadataOnlyProvider {
    fn document_metadata_service(&self) -> Option<&dyn DocumentMetadataService> {
        Some(self)
    }
}

fn consumer_title(provider: &dyn PdfServiceProvider) -> String {
    let service = provider
        .document_metadata_service()
        .expect("consumer requested a negotiated capability");
    let document = service
        .active_document()
        .expect("provider call should succeed")
        .expect("fixture has a document");
    service
        .document_metadata(document)
        .expect("metadata should be readable")
        .title
        .expect("fixture has a title")
}

#[test]
fn an_external_consumer_can_use_a_narrow_service_without_runtime_types() {
    let provider = MetadataOnlyProvider {
        document: document(7, 42),
    };

    assert_eq!(consumer_title(&provider), "A transport-neutral PDF");
    assert_eq!(
        provider.provided_pdf_capabilities(),
        vec![PdfCapability::DocumentMetadata]
    );
}

#[test]
fn typed_handles_round_trip_through_the_generic_extension_protocol() {
    let original = document(9, 81);
    let generic = original.to_resource_handle();
    assert_eq!(generic.kind, "key.pdf.document");
    assert_eq!(
        DocumentHandle::try_from_resource_handle(generic),
        Ok(original)
    );

    let json = serde_json::to_string(&original).expect("handle is serializable");
    assert_eq!(
        serde_json::from_str::<DocumentHandle>(&json).expect("valid handle JSON"),
        original
    );
    assert!(serde_json::from_str::<DocumentHandle>(r#"{"generation":1,"id":0}"#).is_err());
}

#[test]
fn navigation_request_reuses_core_centering_and_focus_semantics() {
    let layout = DocumentLayout::new(
        &[PageSize {
            width: 600.0,
            height: 800.0,
        }],
        1.0,
        900.0,
    );
    let focus_rect = NormalizedRect {
        left: 0.2,
        top: 0.38,
        right: 0.7,
        bottom: 0.44,
    };
    let request = NavigationRequest {
        document: document(1, 1),
        destination: DocumentDestination::page(PageIndex(0)).resolved(Some(0.5), Some(0.4)),
        placement: NavigationPlacement::default(),
        focus: Some(NavigationFocusRequest {
            tone: NavigationFocusTone::SearchMatch,
            motion: NavigationFocusMotion::Pulse,
            regions: vec![focus_rect],
        }),
    };

    let resolved = request
        .to_document_jump()
        .resolve(&layout, 0.0, 300.0, 200.0, 600.0)
        .expect("page exists");
    let page = layout.page_rect(0).expect("page exists");
    assert!((resolved.x - (page.x + page.width * 0.5 - 150.0)).abs() < 0.001);
    assert!((resolved.y - (page.y + page.height * 0.4 - 100.0)).abs() < 0.001);
    let cue = resolved.focus.expect("focus cue should be retained");
    assert_eq!(cue.motion, CoreMotion::Pulse);
    assert_eq!(cue.text_runs, vec![focus_rect.into()]);
}

#[test]
fn extension_owned_ids_remain_generic_contract_ids() {
    let id = LocalId::parse("outline/current-section").expect("canonical local ID");
    assert_eq!(id.as_str(), "outline/current-section");
}
