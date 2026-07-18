use key_pdf_core::{AnnotationError, AnnotationSet};
use std::fmt::{Display, Formatter};
use std::fs::{self, File, Metadata};
use std::io;
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
        })
    }

    pub fn from_pdf(path: &Path, page_count: usize) -> Result<Self, StoreError> {
        let metadata = fs::metadata(path).map_err(|source| StoreError::Io {
            operation: "read PDF metadata",
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_metadata(&metadata, page_count)
    }

    pub(crate) fn from_file(
        file: &File,
        path: &Path,
        page_count: usize,
    ) -> Result<Self, StoreError> {
        let metadata = file.metadata().map_err(|source| StoreError::Io {
            operation: "read locked PDF metadata",
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_metadata(&metadata, page_count)
    }

    fn from_metadata(metadata: &Metadata, page_count: usize) -> Result<Self, StoreError> {
        let modified = metadata
            .modified()
            .map_err(|_| StoreError::InvalidDocumentIdentity)?;
        let (seconds, nanos) = system_time_parts(modified)?;
        Self::new(metadata.len(), seconds, nanos, page_count)
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
