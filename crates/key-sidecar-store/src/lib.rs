//! Reusable storage boundaries for PDF annotations.
//!
//! [`AnnotationStore`] is intentionally synchronous and narrow while
//! [`AnnotationService`] supplies the reusable process-level, multi-document
//! writer actor. [`JsonSidecarStore`] keeps the GPUI PDF Reader v1 sidecar
//! format.

#![forbid(unsafe_code)]

mod extension;
mod json;
mod memory;
mod service;
mod store;

pub use extension::{
    JsonExtensionStorage, JsonExtensionStorageScope, extension_document_namespace,
};
pub use json::{JsonSidecarStore, MAX_SIDECAR_BYTES, SIDECAR_SCHEMA_VERSION, sidecar_path};
pub use memory::MemoryAnnotationStore;
pub use service::{
    AnnotationClientId, AnnotationDocumentId, AnnotationService, AnnotationServiceClient,
    AnnotationServiceDiagnostics, AnnotationServiceEvent, AnnotationServiceEventKind,
    AnnotationServiceOperation, AnnotationServiceStartError,
};
pub use store::{
    AnnotationStore, DocumentIdentity, DocumentKey, SaveReceipt, StoreConflict, StoreError,
};
