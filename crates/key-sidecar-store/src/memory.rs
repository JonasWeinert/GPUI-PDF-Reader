use crate::{
    AnnotationStore, DocumentIdentity, DocumentKey, SaveReceipt, StoreConflict, StoreError,
};
use key_pdf_core::AnnotationSet;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Deterministic in-memory implementation for application and extension tests.
///
/// Entries are keyed by source locator while retaining the document identity,
/// so replacing a document at the same path produces the same conflict shape
/// as the JSON sidecar store.
#[derive(Default)]
pub struct MemoryAnnotationStore {
    entries: Mutex<HashMap<PathBuf, MemoryEntry>>,
}

#[derive(Clone)]
struct MemoryEntry {
    identity: DocumentIdentity,
    annotations: AnnotationSet,
}

impl MemoryAnnotationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remove(&self, source_path: &std::path::Path) -> Result<bool, StoreError> {
        self.entries
            .lock()
            .map_err(|_| StoreError::LockPoisoned)
            .map(|mut entries| entries.remove(source_path).is_some())
    }
}

impl AnnotationStore for MemoryAnnotationStore {
    fn load(&self, document: &DocumentKey) -> Result<AnnotationSet, StoreError> {
        let entries = self.entries.lock().map_err(|_| StoreError::LockPoisoned)?;
        let Some(entry) = entries.get(document.source_path()) else {
            return Ok(AnnotationSet::new(document.identity().page_count()));
        };
        if &entry.identity != document.identity() {
            return Err(StoreError::Conflict(
                StoreConflict::DocumentIdentityMismatch {
                    expected: document.identity().clone(),
                    found: entry.identity.clone(),
                },
            ));
        }
        Ok(entry.annotations.clone())
    }

    fn compare_and_save(
        &self,
        document: &DocumentKey,
        expected_revision: u64,
        annotations: &AnnotationSet,
    ) -> Result<SaveReceipt, StoreError> {
        if annotations.page_count() != document.identity().page_count() {
            return Err(StoreError::PageCountMismatch {
                identity: document.identity().page_count(),
                annotations: annotations.page_count(),
            });
        }
        annotations.validate()?;

        let mut entries = self.entries.lock().map_err(|_| StoreError::LockPoisoned)?;
        if let Some(entry) = entries.get(document.source_path())
            && &entry.identity != document.identity()
        {
            return Err(StoreError::Conflict(
                StoreConflict::DocumentIdentityMismatch {
                    expected: document.identity().clone(),
                    found: entry.identity.clone(),
                },
            ));
        }
        let current = entries
            .get(document.source_path())
            .map_or(0, |entry| entry.annotations.revision());
        if current != expected_revision {
            return Err(StoreError::Conflict(StoreConflict::RevisionMismatch {
                expected: expected_revision,
                found: current,
            }));
        }
        if annotations.revision() < current {
            return Err(StoreError::RevisionRegression {
                current,
                attempted: annotations.revision(),
            });
        }
        if annotations.revision() == current
            && entries
                .get(document.source_path())
                .is_some_and(|entry| entry.annotations != *annotations)
        {
            return Err(StoreError::RevisionNotAdvanced {
                current,
                attempted: annotations.revision(),
            });
        }

        entries.insert(
            document.source_path().to_path_buf(),
            MemoryEntry {
                identity: document.identity().clone(),
                annotations: annotations.clone(),
            },
        );
        Ok(SaveReceipt {
            previous_revision: current,
            saved_revision: annotations.revision(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use key_pdf_core::{HighlightColor, TextPosition, TextRange};

    fn document(path: &str, byte_len: u64) -> DocumentKey {
        DocumentKey::new(
            PathBuf::from(path),
            DocumentIdentity::new(byte_len, 1_700_000_000, 12, 2).unwrap(),
        )
    }

    fn revision_one() -> AnnotationSet {
        let mut annotations = AnnotationSet::new(2);
        annotations
            .add(
                TextRange::new(
                    TextPosition { page: 0, index: 1 },
                    TextPosition { page: 0, index: 4 },
                ),
                Some(HighlightColor::Yellow),
                None,
            )
            .unwrap();
        annotations
    }

    #[test]
    fn missing_load_is_empty_and_compare_save_round_trips() {
        let store = MemoryAnnotationStore::new();
        let document = document("paper.pdf", 42);
        assert_eq!(store.load(&document).unwrap(), AnnotationSet::new(2));

        let annotations = revision_one();
        assert_eq!(
            store.compare_and_save(&document, 0, &annotations).unwrap(),
            SaveReceipt {
                previous_revision: 0,
                saved_revision: 1,
            }
        );
        assert_eq!(store.load(&document).unwrap(), annotations);
    }

    #[test]
    fn stale_writers_and_replaced_documents_are_structured_conflicts() {
        let store = MemoryAnnotationStore::new();
        let original = document("paper.pdf", 42);
        store
            .compare_and_save(&original, 0, &revision_one())
            .unwrap();

        assert!(matches!(
            store.compare_and_save(&original, 0, &revision_one()),
            Err(StoreError::Conflict(StoreConflict::RevisionMismatch {
                expected: 0,
                found: 1,
            }))
        ));

        let replacement = document("paper.pdf", 43);
        assert!(matches!(
            store.load(&replacement),
            Err(StoreError::Conflict(
                StoreConflict::DocumentIdentityMismatch { .. }
            ))
        ));
    }

    #[test]
    fn same_revision_cannot_smuggle_different_content() {
        let store = MemoryAnnotationStore::new();
        let document = document("paper.pdf", 42);
        let first = revision_one();
        store.compare_and_save(&document, 0, &first).unwrap();

        let alternate = AnnotationSet::restore(
            2,
            1,
            vec![key_pdf_core::RestoredAnnotation {
                id: key_pdf_core::AnnotationId(1),
                range: TextRange::new(
                    TextPosition { page: 1, index: 2 },
                    TextPosition { page: 1, index: 3 },
                ),
                highlight: Some(HighlightColor::Purple),
                comment_markdown: None,
                created_revision: 1,
                updated_revision: 1,
            }],
        )
        .unwrap();
        assert!(matches!(
            store.compare_and_save(&document, 1, &alternate),
            Err(StoreError::RevisionNotAdvanced {
                current: 1,
                attempted: 1,
            })
        ));
        assert_eq!(store.load(&document).unwrap(), first);
    }
}
