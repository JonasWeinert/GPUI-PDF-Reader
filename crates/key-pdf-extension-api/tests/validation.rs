//! Adversarial bounds and stale-resource contract tests.

use key_extension_api::{GenerationId, LocalId};
use key_pdf_extension_api::{
    DocumentHandle, DocumentOverlay, GenerationScope, GenerationScoped, InputErrorKind,
    NormalizedRect, OverlayAppearance, OverlayBatch, PageHandle, PageIndex, PageTextRequest,
    PdfContractLimits, PdfExtensionError, PdfValidationContext, SnapshotRevision,
    ValidatePdfContract,
};

fn document(generation: u64) -> DocumentHandle {
    DocumentHandle::from_host_parts(GenerationId(generation), 1).expect("valid document")
}

fn page(generation: u64) -> PageHandle {
    PageHandle::from_host_parts(GenerationId(generation), 2).expect("valid page")
}

fn overlay(id: &str) -> DocumentOverlay {
    DocumentOverlay {
        id: LocalId::parse(id).expect("canonical fixture ID"),
        page: PageIndex(0),
        regions: vec![NormalizedRect {
            left: 0.1,
            top: 0.2,
            right: 0.5,
            bottom: 0.3,
        }],
        appearance: OverlayAppearance::default(),
        label: Some("Current result".into()),
        command: None,
    }
}

#[test]
fn closed_document_handles_are_rejected_before_provider_lookup() {
    let stale = document(11);
    let scope = GenerationScope::new(GenerationId(12));

    assert_eq!(stale.generation(), GenerationId(11));
    assert_eq!(
        scope.validate(&stale),
        Err(PdfExtensionError::StaleGeneration {
            expected: GenerationId(12),
            actual: GenerationId(11),
        })
    );
}

#[test]
fn overlay_batches_enforce_item_region_geometry_and_generation_bounds() {
    let limits = PdfContractLimits {
        maximum_overlays: 1,
        ..PdfContractLimits::default()
    };
    let context = PdfValidationContext::with_limits(GenerationId(4), 3, limits);
    let mut batch = OverlayBatch {
        document: document(4),
        revision: SnapshotRevision(1),
        overlays: vec![overlay("first")],
    };
    assert_eq!(batch.validate_contract(&context), Ok(()));

    batch.overlays.push(overlay("second"));
    assert!(matches!(
        batch.validate_contract(&context),
        Err(PdfExtensionError::LimitExceeded {
            ref field,
            limit: 1,
            actual: 2,
        }) if field == "overlays"
    ));

    batch.overlays.truncate(1);
    batch.overlays[0].regions[0].right = f32::NAN;
    assert!(matches!(
        batch.validate_contract(&context),
        Err(PdfExtensionError::InvalidInput {
            reason: InputErrorKind::NotFinite,
            ..
        })
    ));

    batch.document = document(3);
    assert!(matches!(
        batch.validate_contract(&context),
        Err(PdfExtensionError::StaleGeneration { .. })
    ));
}

#[test]
fn text_requests_are_bounded_independently_of_page_extraction() {
    let limits = PdfContractLimits {
        maximum_text_characters: 32,
        ..PdfContractLimits::default()
    };
    let context = PdfValidationContext::with_limits(GenerationId(8), 1, limits);
    let request = PageTextRequest {
        page: page(8),
        start: 0,
        maximum_characters: 33,
    };
    assert!(matches!(
        request.validate_contract(&context),
        Err(PdfExtensionError::LimitExceeded {
            ref field,
            limit: 32,
            actual: 33,
        }) if field == "text.maximum_characters"
    ));
}

#[test]
fn duplicate_overlay_ids_and_out_of_document_pages_are_rejected() {
    let context = PdfValidationContext::new(GenerationId(2), 1);
    let mut second = overlay("same");
    second.page = PageIndex(0);
    let batch = OverlayBatch {
        document: document(2),
        revision: SnapshotRevision(1),
        overlays: vec![overlay("same"), second],
    };
    assert!(matches!(
        batch.validate_contract(&context),
        Err(PdfExtensionError::InvalidInput {
            reason: InputErrorKind::InvalidIdentifier,
            ..
        })
    ));

    let mut outside = overlay("outside");
    outside.page = PageIndex(1);
    let batch = OverlayBatch {
        document: document(2),
        revision: SnapshotRevision(2),
        overlays: vec![outside],
    };
    assert_eq!(
        batch.validate_contract(&context),
        Err(PdfExtensionError::PageOutOfBounds {
            page: 1,
            page_count: 1,
        })
    );
}

#[test]
fn overlay_revisions_must_advance_before_atomic_replacement() {
    let batch = OverlayBatch {
        document: document(2),
        revision: SnapshotRevision(7),
        overlays: Vec::new(),
    };
    assert_eq!(batch.validate_revision_after(None), Ok(()));
    assert_eq!(
        batch.validate_revision_after(Some(SnapshotRevision(7))),
        Err(PdfExtensionError::InvalidInput {
            field: "overlays.revision".into(),
            reason: InputErrorKind::StaleRevision,
        })
    );
    assert_eq!(
        batch.validate_revision_after(Some(SnapshotRevision(8))),
        Err(PdfExtensionError::InvalidInput {
            field: "overlays.revision".into(),
            reason: InputErrorKind::StaleRevision,
        })
    );
}
