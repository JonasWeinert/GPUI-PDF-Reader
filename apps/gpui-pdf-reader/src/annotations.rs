//! Standalone-reader composition of the reusable annotation domain and store.

pub use key_pdf_core::{
    AnnotationId, AnnotationSet, HighlightColor, MAX_TEXT_CHARACTER_INDEX, TextRange,
};
pub use key_sidecar_store::DocumentIdentity;
#[cfg(test)]
pub use key_sidecar_store::sidecar_path;
#[cfg(test)]
pub use key_sidecar_store::{AnnotationStore, DocumentKey, JsonSidecarStore};
