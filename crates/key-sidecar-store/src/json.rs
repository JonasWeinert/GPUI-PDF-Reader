use crate::{
    AnnotationStore, DocumentIdentity, DocumentKey, SaveReceipt, StoreConflict, StoreError,
};
use key_pdf_core::{
    Annotation, AnnotationId, AnnotationSet, HighlightColor, RestoredAnnotation, TextPosition,
    TextRange,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SIDECAR_SCHEMA_VERSION: u32 = 1;
pub const MAX_SIDECAR_BYTES: usize = 4 * 1024 * 1024;

const SIDECAR_SUFFIX: &str = ".gpui-pdf-reader.json";
const MAX_TEMP_FILE_ATTEMPTS: usize = 32;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Adjacent versioned JSON sidecars compatible with GPUI PDF Reader schema 1.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonSidecarStore;

impl JsonSidecarStore {
    pub fn new() -> Self {
        Self
    }

    fn load_at(
        &self,
        path: &Path,
        expected_identity: &DocumentIdentity,
    ) -> Result<AnnotationSet, StoreError> {
        let metadata = fs::metadata(path).map_err(|source| StoreError::Io {
            operation: "read annotation sidecar metadata",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.len() > MAX_SIDECAR_BYTES as u64 {
            return Err(StoreError::SidecarTooLarge {
                bytes: metadata.len(),
                limit: MAX_SIDECAR_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)
            .map_err(|source| StoreError::Io {
                operation: "open annotation sidecar",
                path: path.to_path_buf(),
                source,
            })?
            .take((MAX_SIDECAR_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| StoreError::Io {
                operation: "read annotation sidecar",
                path: path.to_path_buf(),
                source,
            })?;
        if bytes.len() > MAX_SIDECAR_BYTES {
            return Err(StoreError::SidecarTooLarge {
                bytes: bytes.len() as u64,
                limit: MAX_SIDECAR_BYTES,
            });
        }
        decode_sidecar(path, &bytes, expected_identity)
    }

    fn load_or_empty(
        &self,
        path: &Path,
        expected_identity: &DocumentIdentity,
    ) -> Result<AnnotationSet, StoreError> {
        match self.load_at(path, expected_identity) {
            Err(StoreError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(AnnotationSet::new(expected_identity.page_count()))
            }
            result => result,
        }
    }
}

impl AnnotationStore for JsonSidecarStore {
    fn load(&self, document: &DocumentKey) -> Result<AnnotationSet, StoreError> {
        revalidate_document_path(document)?;
        let path = sidecar_path(document.source_path())?;
        self.load_or_empty(&path, document.identity())
    }

    fn compare_and_save(
        &self,
        document: &DocumentKey,
        expected_revision: u64,
        annotations: &AnnotationSet,
    ) -> Result<SaveReceipt, StoreError> {
        validate_snapshot(document.identity(), annotations)?;
        let sidecar = sidecar_path(document.source_path())?;

        // Lock the PDF rather than a lock sidecar. The advisory lock disappears
        // with the file descriptor after a crash and leaves no stale lock file.
        let pdf = File::open(document.source_path()).map_err(|source| StoreError::Io {
            operation: "open PDF for annotation lock",
            path: document.source_path().to_path_buf(),
            source,
        })?;
        pdf.lock().map_err(|source| StoreError::Io {
            operation: "lock PDF for annotation save",
            path: document.source_path().to_path_buf(),
            source,
        })?;
        let _lock = FileLockGuard { file: &pdf };
        revalidate_locked_document(&pdf, document)?;

        let current = self.load_or_empty(&sidecar, document.identity())?;
        if current.revision() != expected_revision {
            return Err(StoreError::Conflict(StoreConflict::RevisionMismatch {
                expected: expected_revision,
                found: current.revision(),
            }));
        }
        if annotations.revision() < current.revision() {
            return Err(StoreError::RevisionRegression {
                current: current.revision(),
                attempted: annotations.revision(),
            });
        }
        if annotations.revision() == current.revision() && *annotations != current {
            return Err(StoreError::RevisionNotAdvanced {
                current: current.revision(),
                attempted: annotations.revision(),
            });
        }

        let bytes = encode_sidecar(document.identity(), annotations)?;
        // Keep the last identity check adjacent to the replacement. This also
        // catches an in-place edit that occurred while the sidecar was read and
        // encoded, even if another process ignored the advisory lock.
        revalidate_locked_document(&pdf, document)?;
        atomic_write(&sidecar, &bytes)?;
        Ok(SaveReceipt {
            previous_revision: current.revision(),
            saved_revision: annotations.revision(),
        })
    }
}

pub fn sidecar_path(pdf_path: &Path) -> Result<PathBuf, StoreError> {
    let file_name = pdf_path
        .file_name()
        .ok_or(StoreError::MissingSourceFileName)?;
    let mut sidecar_name = OsString::from(file_name);
    sidecar_name.push(SIDECAR_SUFFIX);
    Ok(pdf_path.with_file_name(sidecar_name))
}

fn validate_snapshot(
    identity: &DocumentIdentity,
    annotations: &AnnotationSet,
) -> Result<(), StoreError> {
    if identity.page_count() != annotations.page_count() {
        return Err(StoreError::PageCountMismatch {
            identity: identity.page_count(),
            annotations: annotations.page_count(),
        });
    }
    annotations.validate()?;
    Ok(())
}

fn revalidate_document_path(document: &DocumentKey) -> Result<(), StoreError> {
    let found =
        DocumentIdentity::from_pdf(document.source_path(), document.identity().page_count())?;
    ensure_identity(document.identity(), found)
}

fn revalidate_locked_document(file: &File, document: &DocumentKey) -> Result<(), StoreError> {
    let locked = DocumentIdentity::from_file(
        file,
        document.source_path(),
        document.identity().page_count(),
    )?;
    ensure_identity(document.identity(), locked)?;

    // A path can be replaced after its previous inode was opened. Checking the
    // current path as well ensures we never write a sidecar for that replacement.
    revalidate_document_path(document)
}

fn ensure_identity(expected: &DocumentIdentity, found: DocumentIdentity) -> Result<(), StoreError> {
    if &found == expected {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            StoreConflict::DocumentIdentityMismatch {
                expected: expected.clone(),
                found,
            },
        ))
    }
}

struct FileLockGuard<'a> {
    file: &'a File,
}

impl Drop for FileLockGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn encode_sidecar(
    identity: &DocumentIdentity,
    annotations: &AnnotationSet,
) -> Result<Vec<u8>, StoreError> {
    validate_snapshot(identity, annotations)?;
    let stored = StoredSidecar::from_parts(identity, annotations)?;
    let bytes = serde_json::to_vec_pretty(&stored).map_err(|source| StoreError::Json {
        path: PathBuf::from("<memory>"),
        source,
    })?;
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(StoreError::SidecarTooLarge {
            bytes: bytes.len() as u64,
            limit: MAX_SIDECAR_BYTES,
        });
    }
    Ok(bytes)
}

fn decode_sidecar(
    path: &Path,
    bytes: &[u8],
    expected_identity: &DocumentIdentity,
) -> Result<AnnotationSet, StoreError> {
    let probe: SchemaProbe = serde_json::from_slice(bytes).map_err(|source| StoreError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    if probe.schema_version != SIDECAR_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            found: probe.schema_version,
            supported: SIDECAR_SCHEMA_VERSION,
        });
    }
    let stored: StoredSidecar =
        serde_json::from_slice(bytes).map_err(|source| StoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    let identity = DocumentIdentity::try_from(stored.document)?;
    ensure_identity(expected_identity, identity.clone())?;
    let records = stored
        .annotations
        .into_iter()
        .map(RestoredAnnotation::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AnnotationSet::restore(
        identity.page_count(),
        stored.revision,
        records,
    )?)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(StoreError::SidecarTooLarge {
            bytes: bytes.len() as u64,
            limit: MAX_SIDECAR_BYTES,
        });
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let mut last_collision = None;
    let (temp_path, mut file) = (0..MAX_TEMP_FILE_ATTEMPTS)
        .find_map(|_| {
            let candidate = match temporary_path(path) {
                Ok(candidate) => candidate,
                Err(error) => return Some(Err(error)),
            };
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_collision = Some(error);
                    None
                }
                Err(source) => Some(Err(StoreError::Io {
                    operation: "create annotation sidecar temporary file",
                    path: candidate,
                    source,
                })),
            }
        })
        .transpose()?
        .ok_or_else(|| StoreError::Io {
            operation: "allocate annotation sidecar temporary file",
            path: path.to_path_buf(),
            source: last_collision.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not allocate a unique annotation sidecar temporary file",
                )
            }),
        })?;

    let mut guard = TemporaryFileGuard::new(temp_path);
    let write_result = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    if let Err(source) = write_result {
        drop(file);
        return Err(StoreError::Io {
            operation: "write annotation sidecar temporary file",
            path: guard.path().to_path_buf(),
            source,
        });
    }
    drop(file);
    fs::rename(guard.path(), path).map_err(|source| StoreError::Io {
        operation: "atomically replace annotation sidecar",
        path: path.to_path_buf(),
        source,
    })?;
    guard.disarm();

    // The file is durable before rename. Directory sync is best-effort because
    // the new target is already visible and reporting failure invites unsafe
    // retries after a successful replace.
    if let Some(parent) = parent
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn temporary_path(target: &Path) -> Result<PathBuf, StoreError> {
    let file_name = target
        .file_name()
        .ok_or(StoreError::MissingSourceFileName)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(file_name);
    name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    Ok(target.with_file_name(name))
}

struct TemporaryFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSidecar {
    schema_version: u32,
    document: StoredDocumentIdentity,
    revision: u64,
    annotations: Vec<StoredAnnotation>,
}

impl StoredSidecar {
    fn from_parts(
        identity: &DocumentIdentity,
        annotations: &AnnotationSet,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            schema_version: SIDECAR_SCHEMA_VERSION,
            document: StoredDocumentIdentity::try_from(identity)?,
            revision: annotations.revision(),
            annotations: annotations
                .iter()
                .map(StoredAnnotation::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredDocumentIdentity {
    byte_len: u64,
    modified_unix_seconds: i64,
    modified_nanos: u32,
    page_count: u64,
}

impl TryFrom<&DocumentIdentity> for StoredDocumentIdentity {
    type Error = StoreError;

    fn try_from(identity: &DocumentIdentity) -> Result<Self, Self::Error> {
        Ok(Self {
            byte_len: identity.byte_len(),
            modified_unix_seconds: identity.modified_unix_seconds(),
            modified_nanos: identity.modified_nanos(),
            page_count: u64::try_from(identity.page_count())
                .map_err(|_| StoreError::InvalidDocumentIdentity)?,
        })
    }
}

impl TryFrom<StoredDocumentIdentity> for DocumentIdentity {
    type Error = StoreError;

    fn try_from(stored: StoredDocumentIdentity) -> Result<Self, Self::Error> {
        Self::new(
            stored.byte_len,
            stored.modified_unix_seconds,
            stored.modified_nanos,
            usize::try_from(stored.page_count).map_err(|_| StoreError::InvalidDocumentIdentity)?,
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPosition {
    page: u64,
    index: u64,
}

impl TryFrom<TextPosition> for StoredPosition {
    type Error = StoreError;

    fn try_from(position: TextPosition) -> Result<Self, Self::Error> {
        Ok(Self {
            page: u64::try_from(position.page)
                .map_err(|_| StoreError::InvalidStoredTextPosition)?,
            index: u64::try_from(position.index)
                .map_err(|_| StoreError::InvalidStoredTextPosition)?,
        })
    }
}

impl TryFrom<StoredPosition> for TextPosition {
    type Error = StoreError;

    fn try_from(position: StoredPosition) -> Result<Self, Self::Error> {
        Ok(Self {
            page: usize::try_from(position.page)
                .map_err(|_| StoreError::InvalidStoredTextPosition)?,
            index: usize::try_from(position.index)
                .map_err(|_| StoreError::InvalidStoredTextPosition)?,
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredAnnotation {
    id: u64,
    start: StoredPosition,
    end: StoredPosition,
    #[serde(skip_serializing_if = "Option::is_none")]
    highlight: Option<HighlightColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment_markdown: Option<String>,
    created_revision: u64,
    updated_revision: u64,
}

impl TryFrom<&Annotation> for StoredAnnotation {
    type Error = StoreError;

    fn try_from(annotation: &Annotation) -> Result<Self, Self::Error> {
        Ok(Self {
            id: annotation.id().0,
            start: StoredPosition::try_from(annotation.range().start())?,
            end: StoredPosition::try_from(annotation.range().end())?,
            highlight: annotation.highlight(),
            comment_markdown: annotation.comment_markdown().map(ToOwned::to_owned),
            created_revision: annotation.created_revision(),
            updated_revision: annotation.updated_revision(),
        })
    }
}

impl TryFrom<StoredAnnotation> for RestoredAnnotation {
    type Error = StoreError;

    fn try_from(stored: StoredAnnotation) -> Result<Self, Self::Error> {
        let start = TextPosition::try_from(stored.start)?;
        let end = TextPosition::try_from(stored.end)?;
        Ok(Self {
            id: AnnotationId(stored.id),
            range: TextRange::restore(start, end)?,
            highlight: stored.highlight,
            comment_markdown: stored.comment_markdown,
            created_revision: stored.created_revision,
            updated_revision: stored.updated_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "key-sidecar-store-{name}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixed_identity(page_count: usize) -> DocumentIdentity {
        DocumentIdentity::new(42, 1_700_000_000, 123, page_count).unwrap()
    }

    fn range(start_page: usize, start: usize, end_page: usize, end: usize) -> TextRange {
        TextRange::new(
            TextPosition {
                page: start_page,
                index: start,
            },
            TextPosition {
                page: end_page,
                index: end,
            },
        )
    }

    fn revision_one(page_count: usize, color: HighlightColor) -> AnnotationSet {
        let mut annotations = AnnotationSet::new(page_count);
        annotations
            .add(range(0, 2, 0, 8), Some(color), None)
            .unwrap();
        annotations
    }

    fn pdf_document(directory: &TestDirectory, page_count: usize) -> DocumentKey {
        let pdf = directory.path().join("document.pdf");
        fs::write(&pdf, b"stable pretend PDF bytes").unwrap();
        DocumentKey::from_pdf(pdf, page_count).unwrap()
    }

    #[test]
    fn sidecar_names_append_without_replacing_unicode_extensions() {
        assert_eq!(
            sidecar_path(Path::new("/tmp/Überblick 📚.pdf")).unwrap(),
            Path::new("/tmp/Überblick 📚.pdf.gpui-pdf-reader.json")
        );
        assert_eq!(
            sidecar_path(Path::new("report")).unwrap(),
            Path::new("report.gpui-pdf-reader.json")
        );
        assert!(matches!(
            sidecar_path(Path::new("/")),
            Err(StoreError::MissingSourceFileName)
        ));
    }

    #[test]
    fn schema_one_encoding_is_byte_for_byte_compatible_with_the_reader() {
        let mut annotations = AnnotationSet::new(3);
        annotations
            .add(
                range(0, 2, 0, 8),
                Some(HighlightColor::Yellow),
                Some("**Café** — 日本語".into()),
            )
            .unwrap();
        annotations
            .add(range(1, 0, 2, 3), Some(HighlightColor::Purple), None)
            .unwrap();
        let actual =
            String::from_utf8(encode_sidecar(&fixed_identity(3), &annotations).unwrap()).unwrap();
        let expected = r#"{
  "schema_version": 1,
  "document": {
    "byte_len": 42,
    "modified_unix_seconds": 1700000000,
    "modified_nanos": 123,
    "page_count": 3
  },
  "revision": 2,
  "annotations": [
    {
      "id": 1,
      "start": {
        "page": 0,
        "index": 2
      },
      "end": {
        "page": 0,
        "index": 8
      },
      "highlight": "yellow",
      "comment_markdown": "**Café** — 日本語",
      "created_revision": 1,
      "updated_revision": 1
    },
    {
      "id": 2,
      "start": {
        "page": 1,
        "index": 0
      },
      "end": {
        "page": 2,
        "index": 3
      },
      "highlight": "purple",
      "created_revision": 2,
      "updated_revision": 2
    }
  ]
}"#;
        assert_eq!(actual, expected);
    }

    #[test]
    fn an_existing_reader_sidecar_decodes_semantically() {
        let bytes = br#"{
  "schema_version": 1,
  "document": {
    "byte_len": 42,
    "modified_unix_seconds": 1700000000,
    "modified_nanos": 123,
    "page_count": 2
  },
  "revision": 1,
  "annotations": [
    {
      "id": 9,
      "start": { "page": 0, "index": 4 },
      "end": { "page": 1, "index": 2 },
      "highlight": "green",
      "comment_markdown": "legacy **markdown**",
      "created_revision": 1,
      "updated_revision": 1
    }
  ]
}"#;
        let loaded = decode_sidecar(Path::new("legacy.json"), bytes, &fixed_identity(2)).unwrap();
        assert_eq!(loaded.page_count(), 2);
        assert_eq!(loaded.revision(), 1);
        let annotation = loaded.iter().next().unwrap();
        assert_eq!(annotation.id(), AnnotationId(9));
        assert_eq!(annotation.highlight(), Some(HighlightColor::Green));
        assert_eq!(annotation.comment_markdown(), Some("legacy **markdown**"));
        assert_eq!(annotation.range(), range(0, 4, 1, 2));
    }

    #[test]
    fn missing_sidecar_loads_empty_and_round_trip_preserves_unicode() {
        let directory = TestDirectory::new("round-trip");
        let document = pdf_document(&directory, 3);
        let store = JsonSidecarStore::new();
        assert_eq!(store.load(&document).unwrap(), AnnotationSet::new(3));

        let mut annotations = AnnotationSet::new(3);
        annotations
            .add(
                range(0, 2, 0, 8),
                Some(HighlightColor::Yellow),
                Some("**Café** — [資料](https://example.test/資料)".into()),
            )
            .unwrap();
        store.compare_and_save(&document, 0, &annotations).unwrap();
        assert_eq!(store.load(&document).unwrap(), annotations);
        let json = fs::read_to_string(sidecar_path(document.source_path()).unwrap()).unwrap();
        assert!(json.contains("Café"));
        assert!(!json.contains("highlight\": null"));
    }

    #[test]
    fn stale_schema_identity_corruption_unknown_fields_and_oversize_are_rejected() {
        let directory = TestDirectory::new("invalid-input");
        let document = pdf_document(&directory, 1);
        let sidecar = sidecar_path(document.source_path()).unwrap();
        let store = JsonSidecarStore::new();

        fs::write(
            &sidecar,
            br#"{"schema_version":99,"document":{},"revision":0,"annotations":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            store.load(&document),
            Err(StoreError::UnsupportedSchemaVersion { found: 99, .. })
        ));

        fs::write(&sidecar, b"{ definitely not json").unwrap();
        assert!(matches!(
            store.load(&document),
            Err(StoreError::Json { .. })
        ));

        let wrong = encode_sidecar(
            &DocumentIdentity::new(
                document.identity().byte_len() + 1,
                document.identity().modified_unix_seconds(),
                document.identity().modified_nanos(),
                1,
            )
            .unwrap(),
            &AnnotationSet::new(1),
        )
        .unwrap();
        fs::write(&sidecar, wrong).unwrap();
        assert!(matches!(
            store.load(&document),
            Err(StoreError::Conflict(
                StoreConflict::DocumentIdentityMismatch { .. }
            ))
        ));

        let mut valid: serde_json::Value = serde_json::from_slice(
            &encode_sidecar(document.identity(), &AnnotationSet::new(1)).unwrap(),
        )
        .unwrap();
        valid["surprise"] = serde_json::Value::Bool(true);
        fs::write(&sidecar, serde_json::to_vec(&valid).unwrap()).unwrap();
        assert!(matches!(
            store.load(&document),
            Err(StoreError::Json { .. })
        ));

        fs::write(&sidecar, vec![b' '; MAX_SIDECAR_BYTES + 1]).unwrap();
        assert!(matches!(
            store.load(&document),
            Err(StoreError::SidecarTooLarge { .. })
        ));
    }

    #[test]
    fn reversed_ranges_duplicate_ids_and_invalid_revisions_are_rejected() {
        let identity = fixed_identity(2);
        let document = r#""document":{"byte_len":42,"modified_unix_seconds":1700000000,"modified_nanos":123,"page_count":2}"#;
        let record = |id: u64, start_page: u64, end_page: u64, created: u64, updated: u64| {
            format!(
                r#"{{"id":{id},"start":{{"page":{start_page},"index":1}},"end":{{"page":{end_page},"index":2}},"highlight":"blue","created_revision":{created},"updated_revision":{updated}}}"#
            )
        };

        let duplicate = format!(
            r#"{{"schema_version":1,{document},"revision":2,"annotations":[{},{}]}}"#,
            record(7, 0, 0, 1, 1),
            record(7, 1, 1, 2, 2)
        );
        assert!(matches!(
            decode_sidecar(Path::new("test"), duplicate.as_bytes(), &identity),
            Err(StoreError::Annotation(
                key_pdf_core::AnnotationError::DuplicateAnnotationId(AnnotationId(7))
            ))
        ));

        let reversed = format!(
            r#"{{"schema_version":1,{document},"revision":1,"annotations":[{}]}}"#,
            record(1, 1, 0, 1, 1)
        );
        assert!(matches!(
            decode_sidecar(Path::new("test"), reversed.as_bytes(), &identity),
            Err(StoreError::Annotation(
                key_pdf_core::AnnotationError::ReversedTextRange
            ))
        ));

        let invalid_revision = format!(
            r#"{{"schema_version":1,{document},"revision":1,"annotations":[{}]}}"#,
            record(1, 0, 0, 2, 1)
        );
        assert!(matches!(
            decode_sidecar(Path::new("test"), invalid_revision.as_bytes(), &identity),
            Err(StoreError::Annotation(
                key_pdf_core::AnnotationError::InvalidAnnotationRevision { .. }
            ))
        ));
    }

    #[test]
    fn save_revalidates_pdf_identity_before_touching_the_sidecar() {
        let directory = TestDirectory::new("identity-recheck");
        let document = pdf_document(&directory, 1);
        let sidecar = sidecar_path(document.source_path()).unwrap();
        fs::write(
            document.source_path(),
            b"changed PDF bytes that invalidate identity",
        )
        .unwrap();

        assert!(matches!(
            JsonSidecarStore.compare_and_save(
                &document,
                0,
                &revision_one(1, HighlightColor::Yellow)
            ),
            Err(StoreError::Conflict(
                StoreConflict::DocumentIdentityMismatch { .. }
            ))
        ));
        assert!(!sidecar.exists());
    }

    #[test]
    fn stale_writer_never_overwrites_the_newer_snapshot() {
        let directory = TestDirectory::new("stale-writer");
        let document = pdf_document(&directory, 1);
        let store = JsonSidecarStore;
        let first = revision_one(1, HighlightColor::Green);
        store.compare_and_save(&document, 0, &first).unwrap();

        let mut latest = first.clone();
        latest
            .add(range(0, 10, 0, 12), None, Some("latest".into()))
            .unwrap();
        store.compare_and_save(&document, 1, &latest).unwrap();

        assert!(matches!(
            store.compare_and_save(&document, 0, &revision_one(1, HighlightColor::Purple)),
            Err(StoreError::Conflict(StoreConflict::RevisionMismatch {
                expected: 0,
                found: 2,
            }))
        ));
        assert_eq!(store.load(&document).unwrap(), latest);
    }

    #[test]
    fn concurrent_first_writers_are_serialized_and_exactly_one_wins() {
        let directory = TestDirectory::new("concurrent-writers");
        let document = Arc::new(pdf_document(&directory, 1));
        let barrier = Arc::new(Barrier::new(3));
        let handles = [HighlightColor::Blue, HighlightColor::Pink].map(|color| {
            let document = Arc::clone(&document);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                JsonSidecarStore.compare_and_save(&document, 0, &revision_one(1, color))
            })
        });
        barrier.wait();
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(StoreError::Conflict(StoreConflict::RevisionMismatch {
                        expected: 0,
                        found: 1,
                    }))
                ))
                .count(),
            1
        );
        assert_eq!(JsonSidecarStore.load(&document).unwrap().revision(), 1);
    }

    #[test]
    fn failed_oversize_save_preserves_target_and_leaves_no_temps() {
        let directory = TestDirectory::new("atomic-preservation");
        let document = pdf_document(&directory, 1);
        let store = JsonSidecarStore;
        let original = revision_one(1, HighlightColor::Green);
        store.compare_and_save(&document, 0, &original).unwrap();
        let sidecar = sidecar_path(document.source_path()).unwrap();
        let original_bytes = fs::read(&sidecar).unwrap();

        let mut too_large = AnnotationSet::new(1);
        for index in 0..5 {
            too_large
                .add(
                    range(0, index, 0, index),
                    None,
                    Some("x".repeat(key_pdf_core::MAX_COMMENT_BYTES)),
                )
                .unwrap();
        }
        assert!(matches!(
            store.compare_and_save(&document, 1, &too_large),
            Err(StoreError::SidecarTooLarge { .. })
        ));
        assert_eq!(fs::read(&sidecar).unwrap(), original_bytes);
        assert_eq!(store.load(&document).unwrap(), original);
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }

    #[test]
    fn a_failed_atomic_rename_cleans_its_temporary_file() {
        let directory = TestDirectory::new("rename-cleanup");
        let target_directory = directory.path().join("target.json");
        fs::create_dir(&target_directory).unwrap();
        assert!(matches!(
            atomic_write(&target_directory, b"valid"),
            Err(StoreError::Io { .. })
        ));
        assert!(target_directory.is_dir());
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }

    #[test]
    fn file_lock_serializes_the_compare_and_replace_section() {
        let directory = TestDirectory::new("file-lock");
        let document = pdf_document(&directory, 1);
        let first = File::open(document.source_path()).unwrap();
        let second = File::open(document.source_path()).unwrap();
        first.lock().unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        first.unlock().unwrap();
        second.lock().unwrap();
        second.unlock().unwrap();
    }
}
