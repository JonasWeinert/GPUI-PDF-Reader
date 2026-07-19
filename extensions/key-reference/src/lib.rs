//! Optional first-party reference enrichment for Key applications.
//!
//! The crate owns bounded website previews, per-document ephemeral image
//! caching, and scholarly-provider orchestration. It is independent of GPUI,
//! PDFium, and application state: a host opts into the crate at compile time,
//! then owns its fetchers and per-document sessions.
//!
//! A multi-window host should own one [`ReferenceExecutor`], then create one
//! [`ReferenceDocumentScope`] per open document and pass that same scope to
//! both adapters:
//!
//! ```
//! use key_reference::{LinkPreviewFetcher, ReferenceExecutor, ScholarlyFetcher};
//!
//! let executor = ReferenceExecutor::global();
//! let document = executor.document_scope();
//! let (links, _link_events) = LinkPreviewFetcher::with_scope(document.clone());
//! let (scholarly, _scholarly_events) = ScholarlyFetcher::with_scope(document);
//! links.begin_document(12);
//! scholarly.begin_document(12);
//! ```

#![forbid(unsafe_code)]

mod executor;
mod link_preview;
mod registry_preview;
mod scholarly;

pub use executor::{
    ReferenceDocumentScope, ReferenceExecutor, ReferenceExecutorConfig, ReferenceExecutorSnapshot,
};
pub use link_preview::{
    LinkPreviewEvent, LinkPreviewFetcher, LinkPreviewKind, LinkPreviewSession, WebsitePreview,
    WebsitePreviewProvider, WebsitePreviewState,
};
pub use scholarly::{
    MatchCertainty, ScholarlyEvent, ScholarlyFetcher, ScholarlyMetadata, ScholarlyMetadataProvider,
    ScholarlyMetadataState, ScholarlySession, ScholarlySource,
};

/// Extracts and normalizes a DOI embedded in human-readable citation text.
///
/// DOI matching deliberately remains syntax-only. Provider lookup is the
/// authority for whether the identifier resolves to a work.
#[must_use]
pub fn detect_doi(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("10.") {
        let start = cursor + relative;
        let mut index = start + 3;
        let digit_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() && index - digit_start < 9 {
            index += 1;
        }
        let digit_count = index - digit_start;
        if !(4..=9).contains(&digit_count) || bytes.get(index) != Some(&b'/') {
            cursor = start + 3;
            continue;
        }
        index += 1;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'<' | b'>' | b'"' | b'\'')
        {
            index += 1;
        }
        let doi = lower[start..index]
            .trim_end_matches(|value: char| {
                matches!(value, '.' | ',' | ';' | ':' | ')' | ']' | '}')
            })
            .to_owned();
        if doi.len() > digit_count + 4 {
            return Some(doi);
        }
        cursor = start + 3;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::detect_doi;

    #[test]
    fn doi_detection_normalizes_and_trims_citation_punctuation() {
        assert_eq!(
            detect_doi("See https://doi.org/10.1234/Example.Work)."),
            Some("10.1234/example.work".to_owned())
        );
        assert_eq!(detect_doi("10.12/not-a-doi"), None);
    }
}
