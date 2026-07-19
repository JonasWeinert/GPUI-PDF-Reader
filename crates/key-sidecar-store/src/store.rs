use key_pdf_core::{AnnotationError, AnnotationSet};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Immutable identity of the PDF revision to which annotations are anchored.
///
/// This deliberately preserves the reader's existing on-disk identity scheme
/// so old sidecars remain valid. The source path is kept separately in
/// [`DocumentKey`] because it is a storage locator, not document identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DocumentIdentity {
    byte_len: u64,
    modified_unix_seconds: i64,
    modified_nanos: u32,
    page_count: usize,
    content_sha256: Option<[u8; 32]>,
}

impl DocumentIdentity {
    pub fn new(
        byte_len: u64,
        modified_unix_seconds: i64,
        modified_nanos: u32,
        page_count: usize,
    ) -> Result<Self, StoreError> {
        if modified_nanos >= 1_000_000_000 {
            return Err(StoreError::InvalidDocumentIdentity);
        }
        Ok(Self {
            byte_len,
            modified_unix_seconds,
            modified_nanos,
            page_count,
            content_sha256: None,
        })
    }

    pub fn new_with_content_sha256(
        byte_len: u64,
        modified_unix_seconds: i64,
        modified_nanos: u32,
        page_count: usize,
        content_sha256: [u8; 32],
    ) -> Result<Self, StoreError> {
        let mut identity = Self::new(byte_len, modified_unix_seconds, modified_nanos, page_count)?;
        identity.content_sha256 = Some(content_sha256);
        Ok(identity)
    }

    pub fn from_pdf(path: &Path, page_count: usize) -> Result<Self, StoreError> {
        let file = File::open(path).map_err(|source| StoreError::Io {
            operation: "open PDF for identity",
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_file(&file, path, page_count)
    }

    pub(crate) fn from_file(
        file: &File,
        path: &Path,
        page_count: usize,
    ) -> Result<Self, StoreError> {
        let before = file.metadata().map_err(|source| StoreError::Io {
            operation: "read locked PDF metadata",
            path: path.to_path_buf(),
            source,
        })?;
        let content_sha256 = hash_file(file, path)?;
        let after = file.metadata().map_err(|source| StoreError::Io {
            operation: "re-read locked PDF metadata",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata_revision(&before)? != metadata_revision(&after)? {
            return Err(StoreError::InvalidDocumentIdentity);
        }
        Self::from_metadata(&after, page_count, Some(content_sha256))
    }

    fn from_metadata(
        metadata: &Metadata,
        page_count: usize,
        content_sha256: Option<[u8; 32]>,
    ) -> Result<Self, StoreError> {
        let modified = metadata
            .modified()
            .map_err(|_| StoreError::InvalidDocumentIdentity)?;
        let (seconds, nanos) = system_time_parts(modified)?;
        let mut identity = Self::new(metadata.len(), seconds, nanos, page_count)?;
        identity.content_sha256 = content_sha256;
        Ok(identity)
    }

    pub(crate) fn from_file_metadata(
        file: &File,
        path: &Path,
        page_count: usize,
    ) -> Result<Self, StoreError> {
        let metadata = file.metadata().map_err(|source| StoreError::Io {
            operation: "read PDF metadata",
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_metadata(&metadata, page_count, None)
    }

    pub fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn modified_unix_seconds(&self) -> i64 {
        self.modified_unix_seconds
    }

    pub fn modified_nanos(&self) -> u32 {
        self.modified_nanos
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn content_sha256(&self) -> Option<&[u8; 32]> {
        self.content_sha256.as_ref()
    }

    /// Compares the immutable PDF content when both identities carry a
    /// digest. Legacy identities fall back to the exact metadata tuple that
    /// schema 1 stored, preserving its conservative safety behavior.
    pub fn same_revision(&self, other: &Self) -> bool {
        match (self.content_sha256, other.content_sha256) {
            (Some(left), Some(right)) => {
                self.byte_len == other.byte_len
                    && self.page_count == other.page_count
                    && left == right
            }
            _ => {
                self.byte_len == other.byte_len
                    && self.modified_unix_seconds == other.modified_unix_seconds
                    && self.modified_nanos == other.modified_nanos
                    && self.page_count == other.page_count
            }
        }
    }

    pub(crate) fn same_metadata_revision(&self, other: &Self) -> bool {
        self.byte_len == other.byte_len
            && self.modified_unix_seconds == other.modified_unix_seconds
            && self.modified_nanos == other.modified_nanos
            && self.page_count == other.page_count
    }
}

fn hash_file(file: &File, path: &Path) -> Result<[u8; 32], StoreError> {
    let mut reader = file.try_clone().map_err(|source| StoreError::Io {
        operation: "clone PDF for identity",
        path: path.to_path_buf(),
        source,
    })?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| StoreError::Io {
            operation: "seek PDF for identity",
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|source| StoreError::Io {
            operation: "hash PDF identity",
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

fn metadata_revision(metadata: &Metadata) -> Result<(u64, i64, u32), StoreError> {
    let modified = metadata
        .modified()
        .map_err(|_| StoreError::InvalidDocumentIdentity)?;
    let (seconds, nanos) = system_time_parts(modified)?;
    Ok((metadata.len(), seconds, nanos))
}

fn system_time_parts(time: SystemTime) -> Result<(i64, u32), StoreError> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Ok((
            i64::try_from(duration.as_secs()).map_err(|_| StoreError::InvalidDocumentIdentity)?,
            duration.subsec_nanos(),
        )),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| StoreError::InvalidDocumentIdentity)?;
            if duration.subsec_nanos() == 0 {
                Ok((-seconds, 0))
            } else {
                let seconds = seconds
                    .checked_add(1)
                    .and_then(|seconds| seconds.checked_neg())
                    .ok_or(StoreError::InvalidDocumentIdentity)?;
                Ok((seconds, 1_000_000_000 - duration.subsec_nanos()))
            }
        }
    }
}

/// Storage locator paired with the exact document revision it is allowed to
/// read or update.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DocumentKey {
    source_path: PathBuf,
    identity: DocumentIdentity,
}

impl DocumentKey {
    pub fn new(source_path: PathBuf, identity: DocumentIdentity) -> Self {
        Self {
            source_path,
            identity,
        }
    }

    pub fn from_pdf(path: impl Into<PathBuf>, page_count: usize) -> Result<Self, StoreError> {
        let path = path.into();
        let identity = DocumentIdentity::from_pdf(&path, page_count)?;
        Ok(Self::new(path, identity))
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn identity(&self) -> &DocumentIdentity {
        &self.identity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveReceipt {
    pub previous_revision: u64,
    pub saved_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreConflict {
    DocumentIdentityMismatch {
        expected: DocumentIdentity,
        found: DocumentIdentity,
    },
    RevisionMismatch {
        expected: u64,
        found: u64,
    },
}

/// Synchronous persistence boundary. Callers may place this behind their own
/// worker, task, or actor without coupling the storage crate to a UI runtime.
pub trait AnnotationStore: Send + Sync {
    /// Loads a validated snapshot. A missing sidecar is an empty revision-zero
    /// set for the key's page count.
    fn load(&self, document: &DocumentKey) -> Result<AnnotationSet, StoreError>;

    /// Atomically writes `annotations` only if the document identity and
    /// on-disk annotation revision still match the caller's observations.
    fn compare_and_save(
        &self,
        document: &DocumentKey,
        expected_revision: u64,
        annotations: &AnnotationSet,
    ) -> Result<SaveReceipt, StoreError>;
}

#[derive(Debug)]
pub enum StoreError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    Annotation(AnnotationError),
    Conflict(StoreConflict),
    MissingSourceFileName,
    SidecarTooLarge {
        bytes: u64,
        limit: usize,
    },
    UnsupportedSchemaVersion {
        found: u32,
        supported: u32,
    },
    InvalidDocumentIdentity,
    InvalidStoredTextPosition,
    PageCountMismatch {
        identity: usize,
        annotations: usize,
    },
    RevisionRegression {
        current: u64,
        attempted: u64,
    },
    RevisionNotAdvanced {
        current: u64,
        attempted: u64,
    },
    LockPoisoned,
}

impl Display for StoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} at {}: {source}",
                path.display()
            ),
            Self::Json { path, source } => write!(
                formatter,
                "annotation sidecar JSON at {} is invalid: {source}",
                path.display()
            ),
            Self::Annotation(error) => Display::fmt(error, formatter),
            Self::Conflict(StoreConflict::DocumentIdentityMismatch { expected, found }) => write!(
                formatter,
                "annotation sidecar belongs to a different PDF revision (expected {} bytes, timestamp {}.{:09}, {} pages; found {} bytes, timestamp {}.{:09}, {} pages)",
                expected.byte_len,
                expected.modified_unix_seconds,
                expected.modified_nanos,
                expected.page_count,
                found.byte_len,
                found.modified_unix_seconds,
                found.modified_nanos,
                found.page_count,
            ),
            Self::Conflict(StoreConflict::RevisionMismatch { expected, found }) => write!(
                formatter,
                "annotation sidecar changed on disk (expected revision {expected}, found revision {found}); reload the PDF before saving"
            ),
            Self::MissingSourceFileName => write!(formatter, "the PDF path has no file name"),
            Self::SidecarTooLarge { bytes, limit } => write!(
                formatter,
                "annotation sidecar is {bytes} bytes; the limit is {limit}"
            ),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                formatter,
                "annotation schema {found} is unsupported; this build supports {supported}"
            ),
            Self::InvalidDocumentIdentity => write!(formatter, "document identity is invalid"),
            Self::InvalidStoredTextPosition => {
                write!(formatter, "stored text position is invalid")
            }
            Self::PageCountMismatch {
                identity,
                annotations,
            } => write!(
                formatter,
                "document identity has {identity} pages but annotations expect {annotations}"
            ),
            Self::RevisionRegression { current, attempted } => write!(
                formatter,
                "annotation revision cannot move backwards from {current} to {attempted}"
            ),
            Self::RevisionNotAdvanced { current, attempted } => write!(
                formatter,
                "annotation content changed without advancing revision {current} (attempted {attempted})"
            ),
            Self::LockPoisoned => write!(formatter, "annotation store lock was poisoned"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Annotation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<AnnotationError> for StoreError {
    fn from(error: AnnotationError) -> Self {
        Self::Annotation(error)
    }
}
