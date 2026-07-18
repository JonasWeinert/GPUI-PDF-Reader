use crate::model::{TextBounds, TextChar};
use std::fmt;
use std::ops::RangeInclusive;

pub const MAX_SEARCH_RESULTS: usize = 20_000;
pub const MAX_NORMALIZED_QUERY_CHARS: usize = 256;
/// Raw search-field input is bounded independently of normalization. Four
/// bytes per normalized scalar preserves the full UTF-8 range at the semantic
/// query limit while also bounding ignored and collapsed input.
pub const MAX_SEARCH_QUERY_BYTES: usize = MAX_NORMALIZED_QUERY_CHARS * 4;

const CANCEL_POLL_INTERVAL: usize = 64;
const PREVIEW_CONTEXT_CHARS: usize = 28;

/// A search result's stable identity within one opened document.
///
/// The offsets are inclusive indices into PDFium's original page text layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchMatchId {
    pub page: usize,
    pub start: usize,
    pub end: usize,
}

impl SearchMatchId {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn range(self) -> RangeInclusive<usize> {
        self.start..=self.end
    }
}

/// One page-local result, including list text and zoom-independent paint data.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchMatch {
    pub id: SearchMatchId,
    pub preview: String,
    /// UTF-8 byte range of the matched text within `preview`.
    pub preview_match: std::ops::Range<usize>,
    pub highlight_runs: Vec<TextBounds>,
}

/// The complete bounded result of searching a single page.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchPageResults {
    pub page: usize,
    pub matches: Vec<SearchMatch>,
    /// True when at least one further match existed beyond `matches`.
    pub truncated: bool,
}

/// A validated, reusable query. Its representation intentionally remains
/// private so page text and queries cannot accidentally use different rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    normalized: Vec<char>,
    failure: Vec<usize>,
}

impl SearchQuery {
    pub fn new(query: &str) -> Result<Self, SearchQueryError> {
        let mut normalized = Vec::with_capacity(query.len().min(MAX_NORMALIZED_QUERY_CHARS));
        append_normalized_query(query, &mut normalized)?;
        if normalized.is_empty() {
            return Err(SearchQueryError::Empty);
        }

        let mut failure = vec![0; normalized.len()];
        let mut prefix = 0;
        for index in 1..normalized.len() {
            while prefix > 0 && normalized[index] != normalized[prefix] {
                prefix = failure[prefix - 1];
            }
            if normalized[index] == normalized[prefix] {
                prefix += 1;
            }
            failure[index] = prefix;
        }

        Ok(Self {
            normalized,
            failure,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchQueryError {
    Empty,
    TooLong,
}

impl fmt::Display for SearchQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the search query is empty"),
            Self::TooLong => write!(
                formatter,
                "the normalized search query exceeds {MAX_NORMALIZED_QUERY_CHARS} characters"
            ),
        }
    }
}

impl std::error::Error for SearchQueryError {}

#[derive(Clone, Debug, PartialEq)]
pub enum SearchPageOutcome {
    Complete(SearchPageResults),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceAtom {
    value: char,
    source_start: usize,
    source_end: usize,
}

/// Searches a page without crossing its boundary.
///
/// `result_limit` is clamped to [`MAX_SEARCH_RESULTS`]. Passing the remaining
/// document-wide capacity lets a caller enforce the same bound across pages.
/// Cancellation discards partial results, so stale query revisions are never
/// accidentally published.
pub fn search_page(
    page: usize,
    characters: &[TextChar],
    query: &SearchQuery,
    result_limit: usize,
    mut should_cancel: impl FnMut() -> bool,
) -> SearchPageOutcome {
    if should_cancel() {
        return SearchPageOutcome::Cancelled;
    }

    let Some(atoms) = normalize_page(characters, &mut should_cancel) else {
        return SearchPageOutcome::Cancelled;
    };
    if should_cancel() {
        return SearchPageOutcome::Cancelled;
    }

    let limit = result_limit.min(MAX_SEARCH_RESULTS);
    let mut matches = Vec::with_capacity(limit.min(64));
    let mut matched_prefix = 0;
    let mut truncated = false;

    for (index, atom) in atoms.iter().enumerate() {
        if index != 0 && index.is_multiple_of(CANCEL_POLL_INTERVAL) && should_cancel() {
            return SearchPageOutcome::Cancelled;
        }

        while matched_prefix > 0 && atom.value != query.normalized[matched_prefix] {
            matched_prefix = query.failure[matched_prefix - 1];
        }
        if atom.value == query.normalized[matched_prefix] {
            matched_prefix += 1;
        }
        if matched_prefix != query.normalized.len() {
            continue;
        }

        let first_atom = index + 1 - query.normalized.len();
        let start = atoms[first_atom].source_start;
        let end = atom.source_end;
        if matches.len() == limit {
            truncated = true;
            break;
        }

        let Some(highlight_runs) = highlight_runs(characters, start, end, &mut should_cancel)
        else {
            return SearchPageOutcome::Cancelled;
        };
        let id = SearchMatchId { page, start, end };
        let (preview, preview_match) = preview(characters, start, end);
        matches.push(SearchMatch {
            id,
            preview,
            preview_match,
            highlight_runs,
        });

        // PDF reader search traditionally returns non-overlapping results.
        // Resetting instead of following the KMP suffix link makes that rule
        // explicit for self-overlapping queries such as "aa" in "aaaa".
        matched_prefix = 0;
    }

    if should_cancel() {
        SearchPageOutcome::Cancelled
    } else {
        SearchPageOutcome::Complete(SearchPageResults {
            page,
            matches,
            truncated,
        })
    }
}

fn append_normalized_query(
    query: &str,
    normalized: &mut Vec<char>,
) -> Result<(), SearchQueryError> {
    for value in query.chars() {
        append_normalized_value(value, 0, normalized, |output, value, _, _| {
            output.push(value)
        });
        if normalized.len() > MAX_NORMALIZED_QUERY_CHARS {
            return Err(SearchQueryError::TooLong);
        }
    }
    Ok(())
}

fn normalize_page(
    characters: &[TextChar],
    should_cancel: &mut impl FnMut() -> bool,
) -> Option<Vec<SourceAtom>> {
    let mut normalized = Vec::with_capacity(characters.len());
    for (index, character) in characters.iter().enumerate() {
        if index != 0 && index.is_multiple_of(CANCEL_POLL_INTERVAL) && should_cancel() {
            return None;
        }
        append_normalized_value(
            character.value,
            index,
            &mut normalized,
            |output, value, source_start, source_end| {
                output.push(SourceAtom {
                    value,
                    source_start,
                    source_end,
                });
            },
        );
    }
    Some(normalized)
}

fn append_normalized_value<T>(
    value: char,
    source: usize,
    output: &mut Vec<T>,
    mut push: impl FnMut(&mut Vec<T>, char, usize, usize),
) where
    T: NormalizedAtom,
{
    if value == '\0' || value == '\u{00ad}' {
        return;
    }
    if value.is_whitespace() {
        if let Some(last) = output.last_mut()
            && last.normalized_value() == ' '
        {
            last.extend_source_to(source);
        } else {
            push(output, ' ', source, source);
        }
        return;
    }
    for lowercase in value.to_lowercase() {
        push(output, lowercase, source, source);
    }
}

trait NormalizedAtom {
    fn normalized_value(&self) -> char;
    fn extend_source_to(&mut self, source: usize);
}

impl NormalizedAtom for char {
    fn normalized_value(&self) -> char {
        *self
    }

    fn extend_source_to(&mut self, _source: usize) {}
}

impl NormalizedAtom for SourceAtom {
    fn normalized_value(&self) -> char {
        self.value
    }

    fn extend_source_to(&mut self, source: usize) {
        self.source_end = source;
    }
}

fn preview(characters: &[TextChar], start: usize, end: usize) -> (String, std::ops::Range<usize>) {
    let window_start = start.saturating_sub(PREVIEW_CONTEXT_CHARS);
    let window_end = end
        .saturating_add(PREVIEW_CONTEXT_CHARS)
        .saturating_add(1)
        .min(characters.len());
    let mut result = String::new();
    let mut previous_whitespace = true;
    let mut match_start = None;
    let mut match_end = None;

    if window_start > 0 {
        result.push('\u{2026}');
    }
    for (source_index, character) in characters[window_start..window_end].iter().enumerate() {
        let source_index = window_start + source_index;
        if source_index == start {
            match_start = Some(result.len());
        }
        let value = character.value;
        if value == '\0' || value == '\u{00ad}' {
            continue;
        }
        if value.is_whitespace() {
            if !previous_whitespace {
                result.push(' ');
                previous_whitespace = true;
            }
        } else {
            result.push(value);
            previous_whitespace = false;
        }
        if source_index == end {
            match_end = Some(result.len());
        }
    }
    while result.ends_with(' ') {
        result.pop();
    }
    if window_end < characters.len() {
        result.push('\u{2026}');
    }
    let match_start = match_start.unwrap_or(0).min(result.len());
    let match_end = match_end
        .unwrap_or(match_start)
        .clamp(match_start, result.len());
    (result, match_start..match_end)
}

fn highlight_runs(
    characters: &[TextChar],
    start: usize,
    end: usize,
    should_cancel: &mut impl FnMut() -> bool,
) -> Option<Vec<TextBounds>> {
    let Some(selected) = characters.get(start..=end) else {
        return Some(Vec::new());
    };
    let mut runs = Vec::new();
    for (offset, character) in selected.iter().enumerate() {
        if offset != 0 && offset.is_multiple_of(CANCEL_POLL_INTERVAL) && should_cancel() {
            return None;
        }
        let Some(bounds) = character.bounds.and_then(sanitize_bounds) else {
            continue;
        };
        if let Some(current) = runs.last_mut()
            && can_merge_runs(*current, bounds)
        {
            *current = union_bounds(*current, bounds);
        } else {
            runs.push(bounds);
        }
    }
    Some(runs)
}

/// Produces sanitized, coalesced highlight geometry for an inclusive range of
/// original page-text offsets.
pub fn text_runs_for_range(characters: &[TextChar], start: usize, end: usize) -> Vec<TextBounds> {
    highlight_runs(characters, start, end, &mut || false).unwrap_or_default()
}

fn sanitize_bounds(bounds: TextBounds) -> Option<TextBounds> {
    if ![bounds.left, bounds.top, bounds.right, bounds.bottom]
        .into_iter()
        .all(f32::is_finite)
    {
        return None;
    }
    let left = bounds.left.min(bounds.right).clamp(0.0, 1.0);
    let right = bounds.left.max(bounds.right).clamp(0.0, 1.0);
    let top = bounds.top.min(bounds.bottom).clamp(0.0, 1.0);
    let bottom = bounds.top.max(bounds.bottom).clamp(0.0, 1.0);
    Some(TextBounds {
        left,
        top,
        right,
        bottom,
    })
}

fn can_merge_runs(left: TextBounds, right: TextBounds) -> bool {
    let left_height = (left.bottom - left.top).max(0.0);
    let right_height = (right.bottom - right.top).max(0.0);
    let smaller_height = left_height.min(right_height);
    let larger_height = left_height.max(right_height);
    let vertical_overlap = left.bottom.min(right.bottom) - left.top.max(right.top);
    let center_distance = ((left.top + left.bottom) - (right.top + right.bottom)).abs() * 0.5;
    let same_line = vertical_overlap >= smaller_height * 0.5
        || center_distance <= (larger_height * 0.3).max(0.002);
    if !same_line {
        return false;
    }

    let horizontal_gap = if right.left > left.right {
        right.left - left.right
    } else if left.left > right.right {
        left.left - right.right
    } else {
        0.0
    };
    horizontal_gap <= (larger_height * 0.75).max(0.003)
}

fn union_bounds(left: TextBounds, right: TextBounds) -> TextBounds {
    TextBounds {
        left: left.left.min(right.left),
        top: left.top.min(right.top),
        right: left.right.max(right.right),
        bottom: left.bottom.max(right.bottom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<TextChar> {
        text.chars()
            .map(|value| TextChar {
                value,
                bounds: None,
            })
            .collect()
    }

    fn bounded(value: char, left: f32, top: f32, right: f32, bottom: f32) -> TextChar {
        TextChar {
            value,
            bounds: Some(TextBounds {
                left,
                top,
                right,
                bottom,
            }),
        }
    }

    fn complete(outcome: SearchPageOutcome) -> SearchPageResults {
        match outcome {
            SearchPageOutcome::Complete(results) => results,
            SearchPageOutcome::Cancelled => panic!("search unexpectedly cancelled"),
        }
    }

    fn find(page: usize, text: &[TextChar], query: &str) -> SearchPageResults {
        let query = SearchQuery::new(query).unwrap();
        complete(search_page(page, text, &query, MAX_SEARCH_RESULTS, || {
            false
        }))
    }

    #[test]
    fn ascii_search_is_case_insensitive_and_document_order_is_stable() {
        let results = find(4, &chars("Reader reader READER"), "rEaDeR");
        assert_eq!(
            results
                .matches
                .iter()
                .map(|result| result.id)
                .collect::<Vec<_>>(),
            vec![
                SearchMatchId {
                    page: 4,
                    start: 0,
                    end: 5,
                },
                SearchMatchId {
                    page: 4,
                    start: 7,
                    end: 12,
                },
                SearchMatchId {
                    page: 4,
                    start: 14,
                    end: 19,
                },
            ]
        );
        assert!(!results.truncated);
    }

    #[test]
    fn unicode_lowercase_expansion_maps_back_to_one_source_character() {
        // U+0130 lowercases to two Unicode scalar values: i + combining dot.
        let results = find(2, &chars("İX"), "i\u{307}x");
        assert_eq!(
            results.matches[0].id,
            SearchMatchId {
                page: 2,
                start: 0,
                end: 1,
            }
        );
        let one_character = find(2, &chars("İ"), "i\u{307}");
        assert_eq!(one_character.matches[0].id.start, 0);
        assert_eq!(one_character.matches[0].id.end, 0);
        let preview = &one_character.matches[0].preview;
        assert_eq!(
            &preview[one_character.matches[0].preview_match.clone()],
            "İ"
        );
    }

    #[test]
    fn whitespace_runs_collapse_and_preserve_the_full_source_range() {
        let text = chars("A\r\n\t B");
        let phrase = find(0, &text, "a b");
        assert_eq!(phrase.matches[0].id.range(), 0..=5);

        let whitespace = find(0, &text, " \n\t");
        assert_eq!(whitespace.matches[0].id.range(), 1..=4);
        assert_eq!(phrase.matches[0].preview, "A B");
        assert_eq!(
            &phrase.matches[0].preview[phrase.matches[0].preview_match.clone()],
            "A B"
        );
    }

    #[test]
    fn nul_and_soft_hyphen_are_ignored_without_losing_source_mapping() {
        let text = vec![
            TextChar {
                value: 'A',
                bounds: None,
            },
            TextChar {
                value: '\0',
                bounds: None,
            },
            TextChar {
                value: '\u{00ad}',
                bounds: None,
            },
            TextChar {
                value: 'b',
                bounds: None,
            },
        ];
        let results = find(7, &text, "ab");
        assert_eq!(results.matches[0].id.range(), 0..=3);
        assert_eq!(results.matches[0].preview, "Ab");
        assert_eq!(SearchQuery::new("\0\u{00ad}"), Err(SearchQueryError::Empty));
    }

    #[test]
    fn self_overlapping_matches_are_reported_non_overlapping() {
        let results = find(0, &chars("aaaaa"), "aa");
        let ranges = results
            .matches
            .iter()
            .map(|result| result.id.range())
            .collect::<Vec<_>>();
        assert_eq!(ranges, vec![0..=1, 2..=3]);
    }

    #[test]
    fn query_and_result_limits_are_enforced_and_truncation_is_proven() {
        assert!(SearchQuery::new(&"x".repeat(MAX_NORMALIZED_QUERY_CHARS)).is_ok());
        assert_eq!(
            SearchQuery::new(&"x".repeat(MAX_NORMALIZED_QUERY_CHARS + 1)),
            Err(SearchQueryError::TooLong)
        );

        let query = SearchQuery::new("a").unwrap();
        let results = complete(search_page(0, &chars("aaaa"), &query, 2, || false));
        assert_eq!(results.matches.len(), 2);
        assert!(results.truncated);

        let no_match_beyond_limit = complete(search_page(0, &chars("aa"), &query, 2, || false));
        assert_eq!(no_match_beyond_limit.matches.len(), 2);
        assert!(!no_match_beyond_limit.truncated);

        let hard_cap = complete(search_page(
            0,
            &chars(&"a".repeat(MAX_SEARCH_RESULTS + 1)),
            &query,
            usize::MAX,
            || false,
        ));
        assert_eq!(hard_cap.matches.len(), MAX_SEARCH_RESULTS);
        assert!(hard_cap.truncated);
    }

    #[test]
    fn cancellation_discards_partial_results_during_page_normalization() {
        let query = SearchQuery::new("a").unwrap();
        let text = chars(&"a".repeat(10_000));
        let mut polls = 0;
        let outcome = search_page(0, &text, &query, MAX_SEARCH_RESULTS, || {
            polls += 1;
            polls >= 3
        });
        assert_eq!(outcome, SearchPageOutcome::Cancelled);
        assert!(polls >= 3);
    }

    #[test]
    fn cancellation_is_polled_while_materializing_a_wide_source_match() {
        let query = SearchQuery::new("ab").unwrap();
        let mut text = chars("a");
        text.extend(chars(&"\u{00ad}".repeat(128)));
        text.extend(chars("b"));
        let mut polls = 0;
        let outcome = search_page(0, &text, &query, MAX_SEARCH_RESULTS, || {
            polls += 1;
            // Start, two normalization polls, and the post-normalization poll
            // succeed. The next poll happens inside highlight materialization.
            polls >= 5
        });
        assert_eq!(outcome, SearchPageOutcome::Cancelled);
        assert_eq!(polls, 5);
    }

    #[test]
    fn malformed_geometry_is_ignored_and_remaining_bounds_are_clamped() {
        let text = vec![
            bounded('a', f32::NAN, 0.1, 0.2, 0.2),
            bounded('b', -0.4, 0.1, 1.4, 0.2),
            bounded('c', 0.8, 0.4, 0.7, 0.3),
        ];
        let results = find(0, &text, "abc");
        let runs = &results.matches[0].highlight_runs;
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0],
            TextBounds {
                left: 0.0,
                top: 0.1,
                right: 1.0,
                bottom: 0.2,
            }
        );
        assert_eq!(
            runs[1],
            TextBounds {
                left: 0.7,
                top: 0.3,
                right: 0.8,
                bottom: 0.4,
            }
        );
        assert!(runs.iter().all(|run| {
            [run.left, run.top, run.right, run.bottom]
                .into_iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        }));
    }

    #[test]
    fn adjacent_glyphs_merge_on_a_line_but_never_across_lines() {
        let text = vec![
            bounded('a', 0.10, 0.10, 0.14, 0.20),
            bounded('b', 0.15, 0.11, 0.19, 0.20),
            bounded('c', 0.10, 0.30, 0.14, 0.40),
        ];
        let results = find(0, &text, "abc");
        assert_eq!(
            results.matches[0].highlight_runs,
            vec![
                TextBounds {
                    left: 0.10,
                    top: 0.10,
                    right: 0.19,
                    bottom: 0.20,
                },
                TextBounds {
                    left: 0.10,
                    top: 0.30,
                    right: 0.14,
                    bottom: 0.40,
                },
            ]
        );
    }

    #[test]
    fn previews_are_concise_deterministic_and_normalize_whitespace() {
        let prefix = "p".repeat(PREVIEW_CONTEXT_CHARS + 10);
        let suffix = "s".repeat(PREVIEW_CONTEXT_CHARS + 10);
        let text = chars(&format!("{prefix}\nMatch\t{suffix}"));
        let results = find(0, &text, "match");
        let preview = &results.matches[0].preview;
        assert!(preview.starts_with('\u{2026}'));
        assert!(preview.ends_with('\u{2026}'));
        assert!(preview.contains(" Match "));
        assert!(preview.chars().count() <= PREVIEW_CONTEXT_CHARS * 2 + 7);
        let range = results.matches[0].preview_match.clone();
        assert_eq!(&preview[range], "Match");
    }
}
