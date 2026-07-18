use crate::model::{TextPosition, TextSelection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SIDECAR_SCHEMA_VERSION: u32 = 1;
pub const MAX_SIDECAR_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_COMMENT_BYTES: usize = 1024 * 1024;

// This mirrors the hard per-page extraction limit in `backend.rs`. Persisted
// anchors always come from an extracted character, so a larger index can only
// be stale or malformed sidecar data.
pub const MAX_TEXT_CHARACTER_INDEX: usize = 99_999;

const SIDECAR_SUFFIX: &str = ".gpui-pdf-reader.json";
const MAX_TEMP_FILE_ATTEMPTS: usize = 32;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnnotationId(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HighlightColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

impl HighlightColor {
    pub const ALL: [Self; 5] = [
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Pink,
        Self::Purple,
    ];
}

/// An inclusive text range in PDFium character order.
///
/// Construction always puts the earlier position first. Screen coordinates
/// are deliberately absent so the anchor remains valid across zoom and layout
/// changes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    start: TextPosition,
    end: TextPosition,
}

impl TextRange {
    pub fn new(first: TextPosition, second: TextPosition) -> Self {
        let (start, end) = if first <= second {
            (first, second)
        } else {
            (second, first)
        };
        Self { start, end }
    }

    pub fn from_selection(selection: TextSelection) -> Self {
        Self::new(selection.anchor, selection.focus)
    }

    pub fn start(self) -> TextPosition {
        self.start
    }

    pub fn end(self) -> TextPosition {
        self.end
    }

    pub fn as_selection(self) -> TextSelection {
        TextSelection {
            anchor: self.start,
            focus: self.end,
        }
    }

    pub fn contains(self, position: TextPosition) -> bool {
        self.start <= position && position <= self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    pub fn indices_on_page(
        self,
        page: usize,
        character_count: usize,
    ) -> Option<RangeInclusive<usize>> {
        self.as_selection().indices_on_page(page, character_count)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Annotation {
    id: AnnotationId,
    range: TextRange,
    highlight: Option<HighlightColor>,
    comment_markdown: Option<String>,
    created_revision: u64,
    updated_revision: u64,
}

impl Annotation {
    pub fn id(&self) -> AnnotationId {
        self.id
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    pub fn highlight(&self) -> Option<HighlightColor> {
        self.highlight
    }

    pub fn comment_markdown(&self) -> Option<&str> {
        self.comment_markdown.as_deref()
    }

    pub fn updated_revision(&self) -> u64 {
        self.updated_revision
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationSet {
    page_count: usize,
    records: Vec<Annotation>,
    next_id: Option<AnnotationId>,
    revision: u64,
}

impl AnnotationSet {
    pub fn new(page_count: usize) -> Self {
        Self {
            page_count,
            records: Vec::new(),
            next_id: Some(AnnotationId(1)),
            revision: 0,
        }
    }

    #[cfg(test)]
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Annotation> + DoubleEndedIterator {
        self.records.iter()
    }

    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        self.records.iter().find(|record| record.id == id)
    }

    pub fn add(
        &mut self,
        range: TextRange,
        highlight: Option<HighlightColor>,
        comment_markdown: Option<String>,
    ) -> Result<AnnotationId, AnnotationError> {
        validate_range(range, self.page_count)?;
        validate_content(highlight, comment_markdown.as_deref())?;
        let id = self.next_id.ok_or(AnnotationError::AnnotationIdExhausted)?;
        let revision = self.next_revision()?;
        let annotation = Annotation {
            id,
            range,
            highlight,
            comment_markdown,
            created_revision: revision,
            updated_revision: revision,
        };
        self.records.push(annotation);
        self.records.sort_by(annotation_order);
        self.revision = revision;
        self.next_id = id.0.checked_add(1).map(AnnotationId);
        Ok(id)
    }

    /// Replaces all user-editable fields. Returns `false` for a valid no-op.
    pub fn update(
        &mut self,
        id: AnnotationId,
        range: TextRange,
        highlight: Option<HighlightColor>,
        comment_markdown: Option<String>,
    ) -> Result<bool, AnnotationError> {
        validate_range(range, self.page_count)?;
        validate_content(highlight, comment_markdown.as_deref())?;
        let index = self
            .records
            .iter()
            .position(|record| record.id == id)
            .ok_or(AnnotationError::UnknownAnnotation(id))?;
        let current = &self.records[index];
        if current.range == range
            && current.highlight == highlight
            && current.comment_markdown == comment_markdown
        {
            return Ok(false);
        }
        let revision = self.next_revision()?;
        let record = &mut self.records[index];
        record.range = range;
        record.highlight = highlight;
        record.comment_markdown = comment_markdown;
        record.updated_revision = revision;
        self.records.sort_by(annotation_order);
        self.revision = revision;
        Ok(true)
    }

    #[cfg(test)]
    pub fn delete(&mut self, id: AnnotationId) -> Result<bool, AnnotationError> {
        let Some(index) = self.records.iter().position(|record| record.id == id) else {
            return Ok(false);
        };
        let revision = self.next_revision()?;
        self.records.remove(index);
        self.revision = revision;
        Ok(true)
    }

    /// Returns annotations intersecting a page in deterministic start/id order.
    pub fn on_page(&self, page: usize) -> impl Iterator<Item = &Annotation> {
        self.records
            .iter()
            .take_while(move |record| record.range.start.page <= page)
            .filter(move |record| record.range.end.page >= page)
    }

    /// Returns annotations containing a character position in deterministic
    /// start/id order.
    pub fn at(&self, position: TextPosition) -> impl Iterator<Item = &Annotation> {
        self.records
            .iter()
            .take_while(move |record| record.range.start <= position)
            .filter(move |record| record.range.contains(position))
    }

    pub fn overlapping(&self, range: TextRange) -> impl Iterator<Item = &Annotation> {
        self.records
            .iter()
            .take_while(move |record| record.range.start <= range.end)
            .filter(move |record| record.range.overlaps(range))
    }

    /// Picks the most recently edited annotation at a position; ID is a stable
    /// tie-breaker for externally produced sidecars.
    #[cfg(test)]
    pub fn topmost_at(&self, position: TextPosition) -> Option<&Annotation> {
        self.at(position)
            .max_by_key(|record| (record.updated_revision, record.id))
    }

    fn next_revision(&self) -> Result<u64, AnnotationError> {
        self.revision
            .checked_add(1)
            .ok_or(AnnotationError::RevisionExhausted)
    }

    fn from_records(
        page_count: usize,
        revision: u64,
        mut records: Vec<Annotation>,
    ) -> Result<Self, AnnotationError> {
        let mut ids = HashSet::with_capacity(records.len());
        let mut maximum_id = 0_u64;
        for record in &records {
            if record.id.0 == 0 {
                return Err(AnnotationError::InvalidAnnotationId(record.id));
            }
            if !ids.insert(record.id) {
                return Err(AnnotationError::DuplicateAnnotationId(record.id));
            }
            maximum_id = maximum_id.max(record.id.0);
            validate_range(record.range, page_count)?;
            validate_content(record.highlight, record.comment_markdown.as_deref())?;
            if record.created_revision == 0
                || record.updated_revision < record.created_revision
                || record.updated_revision > revision
            {
                return Err(AnnotationError::InvalidAnnotationRevision {
                    id: record.id,
                    created: record.created_revision,
                    updated: record.updated_revision,
                    document: revision,
                });
            }
        }
        records.sort_by(annotation_order);
        Ok(Self {
            page_count,
            records,
            next_id: maximum_id.checked_add(1).map(AnnotationId),
            revision,
        })
    }

    fn validate(&self) -> Result<(), AnnotationError> {
        let mut ids = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if record.id.0 == 0 {
                return Err(AnnotationError::InvalidAnnotationId(record.id));
            }
            if !ids.insert(record.id) {
                return Err(AnnotationError::DuplicateAnnotationId(record.id));
            }
            validate_range(record.range, self.page_count)?;
            validate_content(record.highlight, record.comment_markdown.as_deref())?;
            if record.created_revision == 0
                || record.updated_revision < record.created_revision
                || record.updated_revision > self.revision
            {
                return Err(AnnotationError::InvalidAnnotationRevision {
                    id: record.id,
                    created: record.created_revision,
                    updated: record.updated_revision,
                    document: self.revision,
                });
            }
        }
        Ok(())
    }
}

fn annotation_order(left: &Annotation, right: &Annotation) -> std::cmp::Ordering {
    (left.range.start, left.id).cmp(&(right.range.start, right.id))
}

fn validate_range(range: TextRange, page_count: usize) -> Result<(), AnnotationError> {
    for position in [range.start, range.end] {
        if position.page >= page_count {
            return Err(AnnotationError::PageOutOfRange {
                page: position.page,
                page_count,
            });
        }
        if position.index > MAX_TEXT_CHARACTER_INDEX {
            return Err(AnnotationError::CharacterIndexOutOfRange(position.index));
        }
    }
    if range.start > range.end {
        return Err(AnnotationError::ReversedTextRange);
    }
    Ok(())
}

fn validate_content(
    highlight: Option<HighlightColor>,
    comment_markdown: Option<&str>,
) -> Result<(), AnnotationError> {
    if let Some(comment) = comment_markdown {
        if comment.len() > MAX_COMMENT_BYTES {
            return Err(AnnotationError::CommentTooLarge(comment.len()));
        }
        if comment.trim().is_empty() {
            return Err(AnnotationError::EmptyComment);
        }
    }
    if highlight.is_none() && comment_markdown.is_none() {
        return Err(AnnotationError::EmptyAnnotation);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    ) -> Result<Self, AnnotationError> {
        if modified_nanos >= 1_000_000_000 {
            return Err(AnnotationError::InvalidDocumentIdentity);
        }
        Ok(Self {
            byte_len,
            modified_unix_seconds,
            modified_nanos,
            page_count,
        })
    }

    pub fn from_pdf(path: &Path, page_count: usize) -> Result<Self, AnnotationError> {
        let metadata = fs::metadata(path)?;
        let (seconds, nanos) = system_time_parts(metadata.modified()?)?;
        Self::new(metadata.len(), seconds, nanos, page_count)
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }
}

fn system_time_parts(time: SystemTime) -> Result<(i64, u32), AnnotationError> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Ok((
            i64::try_from(duration.as_secs())
                .map_err(|_| AnnotationError::InvalidDocumentIdentity)?,
            duration.subsec_nanos(),
        )),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| AnnotationError::InvalidDocumentIdentity)?;
            if duration.subsec_nanos() == 0 {
                Ok((-seconds, 0))
            } else {
                let seconds = seconds
                    .checked_add(1)
                    .and_then(|seconds| seconds.checked_neg())
                    .ok_or(AnnotationError::InvalidDocumentIdentity)?;
                Ok((seconds, 1_000_000_000 - duration.subsec_nanos()))
            }
        }
    }
}

pub fn sidecar_path(pdf_path: &Path) -> Result<PathBuf, AnnotationError> {
    let file_name = pdf_path
        .file_name()
        .ok_or(AnnotationError::MissingPdfFileName)?;
    let mut sidecar_name = OsString::from(file_name);
    sidecar_name.push(SIDECAR_SUFFIX);
    Ok(pdf_path.with_file_name(sidecar_name))
}

pub fn load_sidecar(
    pdf_path: &Path,
    expected_identity: &DocumentIdentity,
) -> Result<AnnotationSet, AnnotationError> {
    let path = sidecar_path(pdf_path)?;
    match load_sidecar_at(&path, expected_identity) {
        Err(AnnotationError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok(AnnotationSet::new(expected_identity.page_count))
        }
        result => result,
    }
}

pub fn load_sidecar_at(
    path: &Path,
    expected_identity: &DocumentIdentity,
) -> Result<AnnotationSet, AnnotationError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_SIDECAR_BYTES as u64 {
        return Err(AnnotationError::SidecarTooLarge(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take((MAX_SIDECAR_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(AnnotationError::SidecarTooLarge(bytes.len() as u64));
    }

    let probe: SchemaProbe = serde_json::from_slice(&bytes)?;
    if probe.schema_version != SIDECAR_SCHEMA_VERSION {
        return Err(AnnotationError::UnsupportedSchemaVersion(
            probe.schema_version,
        ));
    }
    let stored: StoredSidecar = serde_json::from_slice(&bytes)?;
    let identity = DocumentIdentity::try_from(stored.document)?;
    if identity != *expected_identity {
        return Err(AnnotationError::DocumentIdentityMismatch {
            expected: expected_identity.clone(),
            found: identity,
        });
    }

    let records = stored
        .annotations
        .into_iter()
        .map(Annotation::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    AnnotationSet::from_records(identity.page_count, stored.revision, records)
}

pub fn save_sidecar(
    pdf_path: &Path,
    identity: &DocumentIdentity,
    annotations: &AnnotationSet,
) -> Result<PathBuf, AnnotationError> {
    let path = sidecar_path(pdf_path)?;
    save_sidecar_at(&path, identity, annotations)?;
    Ok(path)
}

pub fn save_sidecar_at(
    path: &Path,
    identity: &DocumentIdentity,
    annotations: &AnnotationSet,
) -> Result<(), AnnotationError> {
    if identity.page_count != annotations.page_count {
        return Err(AnnotationError::PageCountMismatch {
            identity: identity.page_count,
            annotations: annotations.page_count,
        });
    }
    annotations.validate()?;
    let stored = StoredSidecar::from_parts(identity, annotations)?;
    let bytes = serde_json::to_vec_pretty(&stored)?;
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(AnnotationError::SidecarTooLarge(bytes.len() as u64));
    }
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AnnotationError> {
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(AnnotationError::SidecarTooLarge(bytes.len() as u64));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let mut last_collision = None;
    let (temp_path, mut file) = (0..MAX_TEMP_FILE_ATTEMPTS)
        .find_map(|_| {
            let candidate = temporary_path(path).ok()?;
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
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            AnnotationError::Io(last_collision.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not allocate a unique annotation sidecar temporary file",
                )
            }))
        })?;
    let mut guard = TemporaryFileGuard::new(temp_path);
    let write_result = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    if let Err(error) = write_result {
        // Close before the guard removes the temporary file; Windows does not
        // permit deleting a file while its handle is still open.
        drop(file);
        return Err(error.into());
    }
    drop(file);
    fs::rename(guard.path(), path)?;
    guard.disarm();

    // The file itself is durable before the rename. Syncing the directory is
    // supported on Unix; failure here is best-effort because the new target is
    // already visible and returning an error could prompt an unsafe retry.
    if let Some(parent) = parent
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn temporary_path(target: &Path) -> Result<PathBuf, AnnotationError> {
    let file_name = target
        .file_name()
        .ok_or(AnnotationError::MissingPdfFileName)?;
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
    ) -> Result<Self, AnnotationError> {
        Ok(Self {
            schema_version: SIDECAR_SCHEMA_VERSION,
            document: StoredDocumentIdentity::try_from(identity)?,
            revision: annotations.revision,
            annotations: annotations
                .records
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
    type Error = AnnotationError;

    fn try_from(identity: &DocumentIdentity) -> Result<Self, Self::Error> {
        Ok(Self {
            byte_len: identity.byte_len,
            modified_unix_seconds: identity.modified_unix_seconds,
            modified_nanos: identity.modified_nanos,
            page_count: u64::try_from(identity.page_count)
                .map_err(|_| AnnotationError::InvalidDocumentIdentity)?,
        })
    }
}

impl TryFrom<StoredDocumentIdentity> for DocumentIdentity {
    type Error = AnnotationError;

    fn try_from(stored: StoredDocumentIdentity) -> Result<Self, Self::Error> {
        Self::new(
            stored.byte_len,
            stored.modified_unix_seconds,
            stored.modified_nanos,
            usize::try_from(stored.page_count)
                .map_err(|_| AnnotationError::InvalidDocumentIdentity)?,
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
    type Error = AnnotationError;

    fn try_from(position: TextPosition) -> Result<Self, Self::Error> {
        Ok(Self {
            page: u64::try_from(position.page)
                .map_err(|_| AnnotationError::InvalidStoredTextPosition)?,
            index: u64::try_from(position.index)
                .map_err(|_| AnnotationError::InvalidStoredTextPosition)?,
        })
    }
}

impl TryFrom<StoredPosition> for TextPosition {
    type Error = AnnotationError;

    fn try_from(position: StoredPosition) -> Result<Self, Self::Error> {
        Ok(Self {
            page: usize::try_from(position.page)
                .map_err(|_| AnnotationError::InvalidStoredTextPosition)?,
            index: usize::try_from(position.index)
                .map_err(|_| AnnotationError::InvalidStoredTextPosition)?,
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
    type Error = AnnotationError;

    fn try_from(annotation: &Annotation) -> Result<Self, Self::Error> {
        Ok(Self {
            id: annotation.id.0,
            start: StoredPosition::try_from(annotation.range.start)?,
            end: StoredPosition::try_from(annotation.range.end)?,
            highlight: annotation.highlight,
            comment_markdown: annotation.comment_markdown.clone(),
            created_revision: annotation.created_revision,
            updated_revision: annotation.updated_revision,
        })
    }
}

impl TryFrom<StoredAnnotation> for Annotation {
    type Error = AnnotationError;

    fn try_from(stored: StoredAnnotation) -> Result<Self, Self::Error> {
        let start = TextPosition::try_from(stored.start)?;
        let end = TextPosition::try_from(stored.end)?;
        if start > end {
            return Err(AnnotationError::ReversedTextRange);
        }
        Ok(Self {
            id: AnnotationId(stored.id),
            range: TextRange { start, end },
            highlight: stored.highlight,
            comment_markdown: stored.comment_markdown,
            created_revision: stored.created_revision,
            updated_revision: stored.updated_revision,
        })
    }
}

#[derive(Debug)]
pub enum AnnotationError {
    Io(io::Error),
    Json(serde_json::Error),
    MissingPdfFileName,
    SidecarTooLarge(u64),
    CommentTooLarge(usize),
    EmptyComment,
    EmptyAnnotation,
    PageOutOfRange {
        page: usize,
        page_count: usize,
    },
    CharacterIndexOutOfRange(usize),
    ReversedTextRange,
    InvalidStoredTextPosition,
    UnknownAnnotation(AnnotationId),
    InvalidAnnotationId(AnnotationId),
    DuplicateAnnotationId(AnnotationId),
    InvalidAnnotationRevision {
        id: AnnotationId,
        created: u64,
        updated: u64,
        document: u64,
    },
    AnnotationIdExhausted,
    RevisionExhausted,
    UnsupportedSchemaVersion(u32),
    InvalidDocumentIdentity,
    DocumentIdentityMismatch {
        expected: DocumentIdentity,
        found: DocumentIdentity,
    },
    PageCountMismatch {
        identity: usize,
        annotations: usize,
    },
}

impl Display for AnnotationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "annotation sidecar I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "annotation sidecar JSON is invalid: {error}"),
            Self::MissingPdfFileName => write!(formatter, "the PDF path has no file name"),
            Self::SidecarTooLarge(bytes) => write!(
                formatter,
                "annotation sidecar is {bytes} bytes; the limit is {MAX_SIDECAR_BYTES}"
            ),
            Self::CommentTooLarge(bytes) => write!(
                formatter,
                "comment is {bytes} bytes; the limit is {MAX_COMMENT_BYTES}"
            ),
            Self::EmptyComment => write!(formatter, "a stored comment cannot be blank"),
            Self::EmptyAnnotation => {
                write!(formatter, "an annotation needs a highlight or a comment")
            }
            Self::PageOutOfRange { page, page_count } => write!(
                formatter,
                "annotation page {page} is outside the {page_count}-page document"
            ),
            Self::CharacterIndexOutOfRange(index) => {
                write!(
                    formatter,
                    "annotation character index {index} is unsupported"
                )
            }
            Self::ReversedTextRange => write!(formatter, "stored text range is reversed"),
            Self::InvalidStoredTextPosition => write!(formatter, "stored text position is invalid"),
            Self::UnknownAnnotation(id) => write!(formatter, "annotation {} does not exist", id.0),
            Self::InvalidAnnotationId(id) => write!(formatter, "annotation ID {} is invalid", id.0),
            Self::DuplicateAnnotationId(id) => {
                write!(formatter, "annotation ID {} occurs more than once", id.0)
            }
            Self::InvalidAnnotationRevision {
                id,
                created,
                updated,
                document,
            } => write!(
                formatter,
                "annotation {} has invalid revisions {created}/{updated} for document revision {document}",
                id.0
            ),
            Self::AnnotationIdExhausted => write!(formatter, "annotation IDs are exhausted"),
            Self::RevisionExhausted => write!(formatter, "annotation revisions are exhausted"),
            Self::UnsupportedSchemaVersion(version) => write!(
                formatter,
                "annotation schema {version} is unsupported; this build supports {SIDECAR_SCHEMA_VERSION}"
            ),
            Self::InvalidDocumentIdentity => write!(formatter, "document identity is invalid"),
            Self::DocumentIdentityMismatch { expected, found } => write!(
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
            Self::PageCountMismatch {
                identity,
                annotations,
            } => write!(
                formatter,
                "document identity has {identity} pages but annotations expect {annotations}"
            ),
        }
    }
}

impl std::error::Error for AnnotationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for AnnotationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for AnnotationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gpui-pdf-reader-{name}-{}-{sequence}",
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

    fn position(page: usize, index: usize) -> TextPosition {
        TextPosition { page, index }
    }

    fn range(start_page: usize, start: usize, end_page: usize, end: usize) -> TextRange {
        TextRange::new(position(start_page, start), position(end_page, end))
    }

    fn identity(page_count: usize) -> DocumentIdentity {
        DocumentIdentity::new(42, 1_700_000_000, 123, page_count).unwrap()
    }

    #[test]
    fn five_highlight_colors_have_stable_lowercase_wire_names() {
        let names = HighlightColor::ALL
            .into_iter()
            .map(|color| serde_json::to_string(&color).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "\"yellow\"",
                "\"green\"",
                "\"blue\"",
                "\"pink\"",
                "\"purple\""
            ]
        );
        for color in HighlightColor::ALL {
            let encoded = serde_json::to_vec(&color).unwrap();
            assert_eq!(
                serde_json::from_slice::<HighlightColor>(&encoded).unwrap(),
                color
            );
        }
    }

    #[test]
    fn text_ranges_normalize_and_keep_inclusive_multi_page_semantics() {
        let text_range = TextRange::new(position(3, 2), position(1, 4));
        assert_eq!(text_range.start(), position(1, 4));
        assert_eq!(text_range.end(), position(3, 2));
        assert_eq!(text_range.indices_on_page(0, 10), None);
        assert_eq!(text_range.indices_on_page(1, 10), Some(4..=9));
        assert_eq!(text_range.indices_on_page(2, 3), Some(0..=2));
        assert_eq!(text_range.indices_on_page(3, 10), Some(0..=2));
        assert!(text_range.contains(position(2, 500)));
        assert!(text_range.overlaps(range(3, 2, 4, 1)));
        assert!(!text_range.overlaps(range(3, 3, 4, 1)));
    }

    #[test]
    fn annotation_set_orders_mutates_and_revises_only_real_changes() {
        let mut annotations = AnnotationSet::new(5);
        let late = annotations
            .add(range(3, 1, 3, 4), Some(HighlightColor::Blue), None)
            .unwrap();
        let early = annotations
            .add(
                range(0, 9, 1, 2),
                Some(HighlightColor::Yellow),
                Some("**important**".into()),
            )
            .unwrap();
        assert_eq!(annotations.revision(), 2);
        assert_eq!(
            annotations.iter().map(Annotation::id).collect::<Vec<_>>(),
            [early, late]
        );

        let unchanged = annotations
            .update(
                early,
                range(0, 9, 1, 2),
                Some(HighlightColor::Yellow),
                Some("**important**".into()),
            )
            .unwrap();
        assert!(!unchanged);
        assert_eq!(annotations.revision(), 2);

        assert!(
            annotations
                .update(
                    late,
                    range(0, 1, 0, 3),
                    Some(HighlightColor::Purple),
                    Some("moved".into()),
                )
                .unwrap()
        );
        assert_eq!(annotations.revision(), 3);
        assert_eq!(annotations.iter().next().unwrap().id(), late);
        assert!(!annotations.delete(AnnotationId(999)).unwrap());
        assert_eq!(annotations.revision(), 3);
        assert!(annotations.delete(early).unwrap());
        assert_eq!(annotations.revision(), 4);
    }

    #[test]
    fn queries_are_deterministic_for_pages_ranges_and_overlaps() {
        let mut annotations = AnnotationSet::new(6);
        let spanning = annotations
            .add(range(1, 4, 3, 8), Some(HighlightColor::Green), None)
            .unwrap();
        let inner = annotations
            .add(range(2, 2, 2, 9), None, Some("note".into()))
            .unwrap();
        let outside = annotations
            .add(range(4, 0, 4, 1), Some(HighlightColor::Pink), None)
            .unwrap();

        assert_eq!(
            annotations
                .on_page(2)
                .map(Annotation::id)
                .collect::<Vec<_>>(),
            [spanning, inner]
        );
        assert_eq!(
            annotations
                .at(position(2, 5))
                .map(Annotation::id)
                .collect::<Vec<_>>(),
            [spanning, inner]
        );
        assert_eq!(annotations.topmost_at(position(2, 5)).unwrap().id(), inner);
        assert_eq!(
            annotations
                .overlapping(range(3, 8, 4, 0))
                .map(Annotation::id)
                .collect::<Vec<_>>(),
            [spanning, outside]
        );
    }

    #[test]
    fn invalid_ranges_and_comment_sizes_are_rejected_without_mutation() {
        let mut annotations = AnnotationSet::new(2);
        assert!(matches!(
            annotations.add(range(2, 0, 2, 1), Some(HighlightColor::Blue), None),
            Err(AnnotationError::PageOutOfRange { .. })
        ));
        assert!(matches!(
            annotations.add(
                range(
                    0,
                    MAX_TEXT_CHARACTER_INDEX + 1,
                    0,
                    MAX_TEXT_CHARACTER_INDEX + 1
                ),
                Some(HighlightColor::Blue),
                None
            ),
            Err(AnnotationError::CharacterIndexOutOfRange(_))
        ));
        assert!(matches!(
            annotations.add(range(0, 0, 0, 0), None, None),
            Err(AnnotationError::EmptyAnnotation)
        ));
        assert!(matches!(
            annotations.add(range(0, 0, 0, 0), None, Some(" \n ".into())),
            Err(AnnotationError::EmptyComment)
        ));
        assert!(
            annotations
                .add(range(0, 0, 0, 0), None, Some("x".repeat(MAX_COMMENT_BYTES)))
                .is_ok()
        );
        assert!(matches!(
            annotations.add(
                range(0, 0, 0, 0),
                None,
                Some("x".repeat(MAX_COMMENT_BYTES + 1))
            ),
            Err(AnnotationError::CommentTooLarge(_))
        ));
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations.revision(), 1);
    }

    #[test]
    fn unicode_sidecar_names_append_without_replacing_pdf_extensions() {
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
            Err(AnnotationError::MissingPdfFileName)
        ));
    }

    #[test]
    fn sidecar_round_trip_preserves_unicode_markdown_colors_and_revisions() {
        let directory = TestDirectory::new("round-trip");
        let pdf = directory.path().join("Résumé 日本語.pdf");
        fs::write(&pdf, b"pretend pdf bytes").unwrap();
        let identity = DocumentIdentity::from_pdf(&pdf, 3).unwrap();
        let mut annotations = AnnotationSet::new(3);
        let first = annotations
            .add(
                range(0, 2, 0, 8),
                Some(HighlightColor::Yellow),
                Some("**Café** — [資料](https://example.test/資料)\n\n- one\n- two".into()),
            )
            .unwrap();
        annotations
            .add(range(1, 0, 2, 3), Some(HighlightColor::Purple), None)
            .unwrap();

        let saved_path = save_sidecar(&pdf, &identity, &annotations).unwrap();
        assert_eq!(saved_path, sidecar_path(&pdf).unwrap());
        let loaded = load_sidecar(&pdf, &identity).unwrap();
        assert_eq!(loaded, annotations);
        assert_eq!(
            loaded.get(first).unwrap().comment_markdown(),
            annotations.get(first).unwrap().comment_markdown()
        );
        let json = fs::read_to_string(saved_path).unwrap();
        assert!(json.contains("\"yellow\""));
        assert!(json.contains("Café"));
        assert!(!json.contains("comment_markdown\": null"));
    }

    #[test]
    fn a_missing_sidecar_loads_as_an_empty_set() {
        let directory = TestDirectory::new("missing");
        let pdf = directory.path().join("document.pdf");
        fs::write(&pdf, b"pdf").unwrap();
        let identity = DocumentIdentity::from_pdf(&pdf, 17).unwrap();
        let loaded = load_sidecar(&pdf, &identity).unwrap();
        assert!(loaded.is_empty());
        assert_eq!(loaded.page_count(), 17);
        assert_eq!(loaded.revision(), 0);
    }

    #[test]
    fn stale_future_corrupt_and_oversize_sidecars_are_rejected() {
        let directory = TestDirectory::new("rejections");
        let path = directory.path().join("document.pdf.gpui-pdf-reader.json");
        let expected = identity(1);

        fs::write(
            &path,
            br#"{"schema_version":99,"document":{},"revision":0,"annotations":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &expected),
            Err(AnnotationError::UnsupportedSchemaVersion(99))
        ));

        fs::write(&path, b"{ definitely not json").unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &expected),
            Err(AnnotationError::Json(_))
        ));

        let mut stale = AnnotationSet::new(1);
        stale
            .add(range(0, 0, 0, 1), Some(HighlightColor::Blue), None)
            .unwrap();
        save_sidecar_at(&path, &identity(1), &stale).unwrap();
        let other_identity = DocumentIdentity::new(43, 1_700_000_000, 123, 1).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &other_identity),
            Err(AnnotationError::DocumentIdentityMismatch { .. })
        ));

        fs::write(&path, vec![b' '; MAX_SIDECAR_BYTES + 1]).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &expected),
            Err(AnnotationError::SidecarTooLarge(_))
        ));
    }

    #[test]
    fn duplicate_ids_reversed_ranges_invalid_revisions_and_unknown_fields_are_rejected() {
        let directory = TestDirectory::new("strict-json");
        let path = directory.path().join("sidecar.json");
        let identity = identity(2);
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
        fs::write(&path, duplicate).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &identity),
            Err(AnnotationError::DuplicateAnnotationId(AnnotationId(7)))
        ));

        let reversed = format!(
            r#"{{"schema_version":1,{document},"revision":1,"annotations":[{}]}}"#,
            record(1, 1, 0, 1, 1)
        );
        fs::write(&path, reversed).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &identity),
            Err(AnnotationError::ReversedTextRange)
        ));

        let invalid_revision = format!(
            r#"{{"schema_version":1,{document},"revision":1,"annotations":[{}]}}"#,
            record(1, 0, 0, 2, 1)
        );
        fs::write(&path, invalid_revision).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &identity),
            Err(AnnotationError::InvalidAnnotationRevision { .. })
        ));

        let unknown = format!(
            r#"{{"schema_version":1,{document},"revision":0,"annotations":[],"surprise":true}}"#
        );
        fs::write(&path, unknown).unwrap();
        assert!(matches!(
            load_sidecar_at(&path, &identity),
            Err(AnnotationError::Json(_))
        ));
    }

    #[test]
    fn failed_oversize_save_preserves_the_previous_valid_sidecar_and_cleans_temps() {
        let directory = TestDirectory::new("atomic-preservation");
        let path = directory.path().join("sidecar.json");
        let identity = identity(1);
        let mut original = AnnotationSet::new(1);
        original
            .add(range(0, 1, 0, 2), Some(HighlightColor::Green), None)
            .unwrap();
        save_sidecar_at(&path, &identity, &original).unwrap();
        let original_bytes = fs::read(&path).unwrap();

        let mut too_large = AnnotationSet::new(1);
        for index in 0..5 {
            too_large
                .add(
                    range(0, index, 0, index),
                    None,
                    Some("x".repeat(MAX_COMMENT_BYTES)),
                )
                .unwrap();
        }
        assert!(matches!(
            save_sidecar_at(&path, &identity, &too_large),
            Err(AnnotationError::SidecarTooLarge(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
        assert_eq!(load_sidecar_at(&path, &identity).unwrap(), original);
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }

    #[test]
    fn document_identity_validates_nanos_and_page_count_before_save() {
        assert!(matches!(
            DocumentIdentity::new(1, 0, 1_000_000_000, 1),
            Err(AnnotationError::InvalidDocumentIdentity)
        ));
        let directory = TestDirectory::new("identity");
        let path = directory.path().join("sidecar.json");
        assert!(matches!(
            save_sidecar_at(&path, &identity(2), &AnnotationSet::new(1)),
            Err(AnnotationError::PageCountMismatch { .. })
        ));
        assert!(!path.exists());
    }
}
