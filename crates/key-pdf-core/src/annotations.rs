//! Engine- and UI-independent PDF annotation domain types.
//!
//! Text anchors use PDFium character order rather than screen coordinates, so
//! they remain stable across zoom and layout changes. Persistence belongs to a
//! store adapter; this module only owns validated annotation state and its
//! revision rules.

use crate::{TextPosition, TextSelection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::ops::RangeInclusive;

pub const MAX_COMMENT_BYTES: usize = 1024 * 1024;

// This mirrors the hard per-page extraction limit used by the PDF runtime.
// Persisted anchors always come from an extracted character, so a larger index
// can only be stale or malformed data.
pub const MAX_TEXT_CHARACTER_INDEX: usize = 99_999;

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

/// An inclusive text range in document character order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    start: TextPosition,
    end: TextPosition,
}

impl TextRange {
    /// Creates a normalized range with the earlier position first.
    pub fn new(first: TextPosition, second: TextPosition) -> Self {
        let (start, end) = if first <= second {
            (first, second)
        } else {
            (second, first)
        };
        Self { start, end }
    }

    /// Restores an already-ordered persisted range without silently repairing
    /// corrupt data.
    pub fn restore(start: TextPosition, end: TextPosition) -> Result<Self, AnnotationError> {
        if start > end {
            return Err(AnnotationError::ReversedTextRange);
        }
        Ok(Self { start, end })
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

    pub fn created_revision(&self) -> u64 {
        self.created_revision
    }

    pub fn updated_revision(&self) -> u64 {
        self.updated_revision
    }
}

/// Persistence-safe input for restoring an annotation.
///
/// The record itself is intentionally allowed to hold untrusted values.
/// [`AnnotationSet::restore`] validates every record against the document as
/// one atomic operation before producing usable domain state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoredAnnotation {
    pub id: AnnotationId,
    pub range: TextRange,
    pub highlight: Option<HighlightColor>,
    pub comment_markdown: Option<String>,
    pub created_revision: u64,
    pub updated_revision: u64,
}

impl From<&Annotation> for RestoredAnnotation {
    fn from(annotation: &Annotation) -> Self {
        Self {
            id: annotation.id,
            range: annotation.range,
            highlight: annotation.highlight,
            comment_markdown: annotation.comment_markdown.clone(),
            created_revision: annotation.created_revision,
            updated_revision: annotation.updated_revision,
        }
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

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

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
        self.records.push(Annotation {
            id,
            range,
            highlight,
            comment_markdown,
            created_revision: revision,
            updated_revision: revision,
        });
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

    /// Deletes an annotation and advances the document revision only when the
    /// ID exists.
    pub fn delete(&mut self, id: AnnotationId) -> Result<bool, AnnotationError> {
        let Some(index) = self.records.iter().position(|record| record.id == id) else {
            return Ok(false);
        };
        let revision = self.next_revision()?;
        self.records.remove(index);
        self.revision = revision;
        Ok(true)
    }

    /// Returns annotations intersecting a page in deterministic start/ID
    /// order.
    pub fn on_page(&self, page: usize) -> impl Iterator<Item = &Annotation> {
        self.records
            .iter()
            .take_while(move |record| record.range.start.page <= page)
            .filter(move |record| record.range.end.page >= page)
    }

    /// Returns annotations containing a character position in deterministic
    /// start/ID order.
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
    pub fn topmost_at(&self, position: TextPosition) -> Option<&Annotation> {
        self.at(position)
            .max_by_key(|record| (record.updated_revision, record.id))
    }

    /// Restores and validates an entire persisted snapshot atomically.
    pub fn restore(
        page_count: usize,
        revision: u64,
        records: Vec<RestoredAnnotation>,
    ) -> Result<Self, AnnotationError> {
        let mut annotations = Vec::with_capacity(records.len());
        let mut ids = HashSet::with_capacity(records.len());
        let mut maximum_id = 0_u64;
        for record in records {
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
            annotations.push(Annotation {
                id: record.id,
                range: record.range,
                highlight: record.highlight,
                comment_markdown: record.comment_markdown,
                created_revision: record.created_revision,
                updated_revision: record.updated_revision,
            });
        }
        annotations.sort_by(annotation_order);
        Ok(Self {
            page_count,
            records: annotations,
            next_id: maximum_id.checked_add(1).map(AnnotationId),
            revision,
        })
    }

    /// Revalidates a snapshot before crossing a persistence boundary.
    pub fn validate(&self) -> Result<(), AnnotationError> {
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

    fn next_revision(&self) -> Result<u64, AnnotationError> {
        self.revision
            .checked_add(1)
            .ok_or(AnnotationError::RevisionExhausted)
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
pub enum AnnotationError {
    CommentTooLarge(usize),
    EmptyComment,
    EmptyAnnotation,
    PageOutOfRange {
        page: usize,
        page_count: usize,
    },
    CharacterIndexOutOfRange(usize),
    ReversedTextRange,
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
}

impl Display for AnnotationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
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
        }
    }
}

impl std::error::Error for AnnotationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(page: usize, index: usize) -> TextPosition {
        TextPosition { page, index }
    }

    fn range(start_page: usize, start: usize, end_page: usize, end: usize) -> TextRange {
        TextRange::new(position(start_page, start), position(end_page, end))
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
    fn mutation_order_and_revisions_match_the_original_reader_contract() {
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

        assert!(
            !annotations
                .update(
                    early,
                    range(0, 9, 1, 2),
                    Some(HighlightColor::Yellow),
                    Some("**important**".into()),
                )
                .unwrap()
        );
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
        assert!(annotations.delete(early).unwrap());
        assert_eq!(annotations.revision(), 4);
    }

    #[test]
    fn queries_are_deterministic_for_pages_positions_and_overlaps() {
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
    fn invalid_input_is_rejected_without_mutation() {
        let mut annotations = AnnotationSet::new(2);
        assert!(matches!(
            annotations.add(range(2, 0, 2, 1), Some(HighlightColor::Blue), None),
            Err(AnnotationError::PageOutOfRange { .. })
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
    fn restore_rejects_duplicate_ids_and_invalid_revisions_atomically() {
        let valid = RestoredAnnotation {
            id: AnnotationId(7),
            range: range(0, 1, 0, 2),
            highlight: Some(HighlightColor::Blue),
            comment_markdown: None,
            created_revision: 1,
            updated_revision: 1,
        };
        assert!(matches!(
            AnnotationSet::restore(1, 2, vec![valid.clone(), valid.clone()]),
            Err(AnnotationError::DuplicateAnnotationId(AnnotationId(7)))
        ));
        let mut invalid_revision = valid;
        invalid_revision.updated_revision = 3;
        assert!(matches!(
            AnnotationSet::restore(1, 2, vec![invalid_revision]),
            Err(AnnotationError::InvalidAnnotationRevision { .. })
        ));
    }

    #[test]
    fn restoring_maximum_id_preserves_exhaustion_without_wrapping() {
        let annotations = AnnotationSet::restore(
            1,
            1,
            vec![RestoredAnnotation {
                id: AnnotationId(u64::MAX),
                range: range(0, 0, 0, 0),
                highlight: Some(HighlightColor::Yellow),
                comment_markdown: None,
                created_revision: 1,
                updated_revision: 1,
            }],
        )
        .unwrap();
        let mut annotations = annotations;
        assert!(matches!(
            annotations.add(range(0, 1, 0, 1), Some(HighlightColor::Green), None),
            Err(AnnotationError::AnnotationIdExhausted)
        ));
    }
}
