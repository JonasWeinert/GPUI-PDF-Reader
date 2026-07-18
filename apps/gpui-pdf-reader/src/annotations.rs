//! Standalone-reader composition of the reusable annotation domain and store.

pub use key_pdf_core::{
    AnnotationId, AnnotationSet, HighlightColor, MAX_TEXT_CHARACTER_INDEX, TextRange,
};
#[cfg(test)]
pub use key_sidecar_store::sidecar_path;
pub use key_sidecar_store::{AnnotationStore, DocumentIdentity, DocumentKey, JsonSidecarStore};
