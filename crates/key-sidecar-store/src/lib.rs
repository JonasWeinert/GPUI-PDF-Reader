//! Reusable storage boundaries for PDF annotations.
//!
//! [`AnnotationStore`] is intentionally synchronous and narrow: applications
//! choose their own worker/thread model while stores own atomic compare-and-save
//! semantics. [`JsonSidecarStore`] keeps the GPUI PDF Reader v1 sidecar format.

#![forbid(unsafe_code)]

mod extension;
mod json;
mod memory;
mod store;

pub use extension::{JsonExtensionStorage, extension_document_namespace};
pub use json::{JsonSidecarStore, MAX_SIDECAR_BYTES, SIDECAR_SCHEMA_VERSION, sidecar_path};
pub use memory::MemoryAnnotationStore;
pub use store::{
    AnnotationStore, DocumentIdentity, DocumentKey, SaveReceipt, StoreConflict, StoreError,
};
