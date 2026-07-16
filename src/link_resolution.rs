use crate::model::{TextBounds, TextLayer};
use crate::search::{
    MAX_SEARCH_RESULTS, SearchMatch, SearchPageOutcome, SearchQuery, search_page,
    text_runs_for_range,
};

const MAX_LINK_QUERY_MATCHES: usize = 64;
const MAX_RESOLVED_ENTRY_CHARACTERS: usize = 1_200;
const MAX_RESOLVED_PREVIEW_CHARACTERS: usize = 520;

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedInternalLink {
    pub x_fraction: Option<f32>,
    pub y_fraction: Option<f32>,
    pub text_runs: Vec<TextBounds>,
    pub preview: String,
    pub matched_source: bool,
}

pub fn resolve_internal_link(
    source_text: &TextLayer,
    source_bounds: TextBounds,
    target_text: &TextLayer,
    page: usize,
    rough_x: Option<f32>,
    rough_y: Option<f32>,
) -> ResolvedInternalLink {
    let source = link_source_text(source_text, source_bounds);
    let candidates = link_query_candidates(&source);
    let matched = best_target_match(target_text, page, rough_y, &candidates);
    if let Some(matched) = matched {
        let numeric_marker = source_numeric_marker(&source);
        let (start, end) = meaningful_entry_range(
            target_text,
            matched.id.start,
            matched.id.end,
            numeric_marker,
        );
        let text_runs = text_runs_for_range(target_text.as_slice(), start, end);
        let first = text_runs.first().copied();
        return ResolvedInternalLink {
            x_fraction: first
                .map(|bounds| (bounds.left + bounds.right) * 0.5)
                .or(rough_x),
            y_fraction: first.map(|bounds| bounds.top).or(rough_y),
            preview: range_preview(target_text, start, end),
            text_runs,
            matched_source: true,
        };
    }

    ResolvedInternalLink {
        x_fraction: rough_x,
        y_fraction: rough_y,
        text_runs: Vec::new(),
        preview: rough_position_preview(target_text, rough_y),
        matched_source: false,
    }
}

pub fn link_source_text(text: &TextLayer, link_bounds: TextBounds) -> String {
    let horizontal_padding = 0.004;
    let vertical_padding = 0.004;
    let left = link_bounds.left.min(link_bounds.right) - horizontal_padding;
    let right = link_bounds.left.max(link_bounds.right) + horizontal_padding;
    let top = link_bounds.top.min(link_bounds.bottom) - vertical_padding;
    let bottom = link_bounds.top.max(link_bounds.bottom) + vertical_padding;
    let mut first = None;
    let mut last = None;
    for (index, character) in text.iter().enumerate() {
        let Some(bounds) = character.bounds else {
            continue;
        };
        let center_x = (bounds.left + bounds.right) * 0.5;
        let center_y = (bounds.top + bounds.bottom) * 0.5;
        if (left..=right).contains(&center_x) && (top..=bottom).contains(&center_y) {
            first.get_or_insert(index);
            last = Some(index);
        }
    }
    let Some((first, last)) = first.zip(last) else {
        return String::new();
    };
    normalized_range_text(text, first, last, 256)
}

fn link_query_candidates(source: &str) -> Vec<String> {
    let source = source.trim();
    if source.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    push_candidate(&mut candidates, source);
    let stripped = source
        .trim_matches(|value: char| {
            value.is_whitespace()
                || matches!(
                    value,
                    '[' | ']' | '(' | ')' | '{' | '}' | ',' | ';' | ':' | '.' | '†' | '*'
                )
        })
        .trim();
    push_candidate(&mut candidates, stripped);

    if let Some(number) = source_numeric_marker(source) {
        push_candidate(&mut candidates, &number.to_string());
    } else {
        let distinctive = source
            .split(|value: char| !value.is_alphanumeric())
            .filter(|token| token.chars().count() >= 4)
            .max_by_key(|token| token.chars().count());
        if let Some(distinctive) = distinctive {
            push_candidate(&mut candidates, distinctive);
        }
    }
    candidates
}

fn push_candidate(candidates: &mut Vec<String>, candidate: &str) {
    let candidate = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    if !candidate.is_empty()
        && candidate.chars().count() <= crate::search::MAX_NORMALIZED_QUERY_CHARS
        && !candidates.iter().any(|existing| existing == &candidate)
    {
        candidates.push(candidate);
    }
}

fn best_target_match(
    target: &TextLayer,
    page: usize,
    rough_y: Option<f32>,
    candidates: &[String],
) -> Option<SearchMatch> {
    let target_y = rough_y.unwrap_or(0.0).clamp(0.0, 1.0);
    let mut best: Option<(f32, SearchMatch)> = None;
    for (priority, candidate) in candidates.iter().enumerate() {
        let Ok(query) = SearchQuery::new(candidate) else {
            continue;
        };
        let SearchPageOutcome::Complete(results) = search_page(
            page,
            target.as_slice(),
            &query,
            MAX_LINK_QUERY_MATCHES.min(MAX_SEARCH_RESULTS),
            || false,
        ) else {
            continue;
        };
        for result in results.matches {
            let result_y = result
                .highlight_runs
                .first()
                .map_or(target_y, |run| (run.top + run.bottom) * 0.5);
            let marker_bonus = if numeric_marker_context(
                target,
                result.id.start,
                result.id.end,
                source_numeric_marker(candidate),
            ) {
                0.16
            } else {
                0.0
            };
            let specificity = candidate.chars().count().min(80) as f32 * 0.001;
            let score =
                (result_y - target_y).abs() + priority as f32 * 0.07 - marker_bonus - specificity;
            if best.as_ref().is_none_or(|(best_score, best_match)| {
                score < *best_score - 0.0001
                    || ((score - *best_score).abs() <= 0.0001
                        && result.id.start < best_match.id.start)
            }) {
                best = Some((score, result));
            }
        }
    }
    best.map(|(_, result)| result)
}

fn numeric_marker_context(
    text: &TextLayer,
    start: usize,
    end: usize,
    expected: Option<u32>,
) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let value = normalized_range_text(text, start, end, 24);
    if source_numeric_marker(&value) != Some(expected) {
        return false;
    }
    let line_start = line_start_index(text, start);
    text[line_start..start].iter().all(|character| {
        character.value.is_whitespace() || matches!(character.value, '[' | '(' | '{')
    })
}

fn meaningful_entry_range(
    text: &TextLayer,
    matched_start: usize,
    matched_end: usize,
    numeric_marker: Option<u32>,
) -> (usize, usize) {
    let start = line_start_index(text, matched_start);
    let hard_end = start
        .saturating_add(MAX_RESOLVED_ENTRY_CHARACTERS)
        .min(text.len())
        .max(matched_end.saturating_add(1));
    let mut end = numeric_marker
        .and_then(|marker| {
            next_numeric_entry_start(text, matched_end.saturating_add(1), marker + 1)
        })
        .or_else(|| next_geometry_entry_start(text, start, matched_end.saturating_add(1), hard_end))
        .or_else(|| next_paragraph_start(text, matched_end.saturating_add(1)))
        .unwrap_or(hard_end)
        .min(hard_end)
        .saturating_sub(1);
    while end > matched_end
        && text
            .get(end)
            .is_some_and(|character| character.value.is_whitespace())
    {
        end -= 1;
    }
    (start.min(matched_start), end.max(matched_end))
}

#[derive(Clone, Copy)]
struct VisualLine {
    start: usize,
    left: f32,
    top: f32,
    height: f32,
}

fn next_geometry_entry_start(
    text: &TextLayer,
    entry_start: usize,
    from: usize,
    hard_end: usize,
) -> Option<usize> {
    let lines = visual_lines(text, entry_start, hard_end);
    if lines.len() < 3 {
        return None;
    }
    let base = lines[0];
    let hanging_indent = lines
        .get(1)
        .is_some_and(|line| line.left > base.left + 0.014);
    let mut gaps = lines
        .windows(2)
        .filter_map(|pair| {
            let gap = pair[1].top - pair[0].top;
            (gap.is_finite() && gap > 0.001).then_some(gap)
        })
        .collect::<Vec<_>>();
    gaps.sort_by(f32::total_cmp);
    let typical_gap = gaps
        .get(gaps.len() / 2)
        .copied()
        .unwrap_or(base.height.max(0.01));

    for (index, line) in lines.iter().enumerate().skip(1) {
        if line.start < from {
            continue;
        }
        if hanging_indent && line.left <= base.left + 0.008 {
            return Some(line.start);
        }
        if index >= 2 {
            let previous = lines[index - 1];
            let gap = line.top - previous.top;
            if gap > typical_gap * 1.5 && gap > previous.height * 1.15 {
                return Some(line.start);
            }
        }
    }
    None
}

fn visual_lines(text: &TextLayer, start: usize, end: usize) -> Vec<VisualLine> {
    let mut lines: Vec<VisualLine> = Vec::new();
    let mut force_new_line = true;
    for (index, character) in text
        .iter()
        .enumerate()
        .take(end.min(text.len()))
        .skip(start)
    {
        if matches!(character.value, '\n' | '\r') {
            force_new_line = true;
            continue;
        }
        if character.value.is_whitespace() {
            continue;
        }
        let Some(bounds) = character.bounds else {
            continue;
        };
        let top = bounds.top.min(bounds.bottom);
        let bottom = bounds.top.max(bounds.bottom);
        let height = (bottom - top).max(0.001);
        let same_line = !force_new_line
            && lines.last().is_some_and(|line| {
                let overlap = (line.top + line.height).min(bottom) - line.top.max(top);
                let center_distance = ((line.top + line.height * 0.5) - (top + height * 0.5)).abs();
                overlap >= line.height.min(height) * 0.35
                    || center_distance <= line.height.max(height) * 0.4
            });
        if same_line {
            if let Some(line) = lines.last_mut() {
                line.left = line.left.min(bounds.left.min(bounds.right));
                let new_top = line.top.min(top);
                let new_bottom = (line.top + line.height).max(bottom);
                line.top = new_top;
                line.height = new_bottom - new_top;
            }
        } else {
            lines.push(VisualLine {
                start: index,
                left: bounds.left.min(bounds.right),
                top,
                height,
            });
        }
        force_new_line = false;
    }
    lines
}

fn next_numeric_entry_start(text: &TextLayer, from: usize, expected: u32) -> Option<usize> {
    let mut line_start = from == 0
        || text
            .get(from.saturating_sub(1))
            .is_some_and(|character| matches!(character.value, '\n' | '\r'));
    let mut index = from;
    while index < text.len() {
        let value = text[index].value;
        if matches!(value, '\n' | '\r') {
            line_start = true;
            index += 1;
            continue;
        }
        if !line_start {
            index += 1;
            continue;
        }
        if value.is_whitespace() {
            index += 1;
            continue;
        }
        let marker_start = index;
        if matches!(value, '[' | '(' | '{') {
            index += 1;
        }
        let digit_start = index;
        while index < text.len() && text[index].value.is_ascii_digit() {
            index += 1;
        }
        if digit_start != index {
            let number = text[digit_start..index]
                .iter()
                .map(|character| character.value)
                .collect::<String>()
                .parse::<u32>()
                .ok();
            let terminator = text.get(index).map(|character| character.value);
            if number == Some(expected)
                && terminator.is_some_and(|value| {
                    value.is_whitespace() || matches!(value, ']' | ')' | '}' | '.' | ':')
                })
            {
                return Some(marker_start);
            }
        }
        line_start = false;
        index += 1;
    }
    None
}

fn next_paragraph_start(text: &TextLayer, from: usize) -> Option<usize> {
    let mut newline_count = 0;
    for (index, character) in text.iter().enumerate().skip(from) {
        match character.value {
            '\n' => {
                newline_count += 1;
                if newline_count >= 2 {
                    return Some(index + 1);
                }
            }
            '\r' => {}
            value if value.is_whitespace() => {}
            _ => newline_count = 0,
        }
    }
    None
}

fn line_start_index(text: &TextLayer, index: usize) -> usize {
    text[..index.min(text.len())]
        .iter()
        .rposition(|character| matches!(character.value, '\n' | '\r'))
        .map_or(0, |line_break| line_break + 1)
}

fn source_numeric_marker(source: &str) -> Option<u32> {
    let source = source.trim();
    let mut values = source.char_indices().peekable();
    let opening = values
        .peek()
        .and_then(|(_, value)| matches!(value, '[' | '(' | '{').then_some(*value));
    if opening.is_some() {
        values.next();
    }
    let mut digits = String::new();
    let mut digit_end = 0;
    while let Some((index, value)) = values.peek().copied() {
        if !value.is_ascii_digit() {
            break;
        }
        digits.push(value);
        digit_end = index + value.len_utf8();
        values.next();
    }
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let remainder = &source[digit_end..];
    let valid_terminator = if let Some(opening) = opening {
        let closing = match opening {
            '[' => ']',
            '(' => ')',
            '{' => '}',
            _ => unreachable!(),
        };
        remainder.trim_start().starts_with(closing)
    } else {
        remainder.is_empty()
            || remainder.chars().next().is_some_and(|value| {
                value.is_whitespace() || matches!(value, '.' | ':' | ')' | ']')
            })
    };
    valid_terminator
        .then(|| digits.parse::<u32>().ok())
        .flatten()
}

fn rough_position_preview(text: &TextLayer, rough_y: Option<f32>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let target_y = rough_y.unwrap_or(0.15).clamp(0.0, 1.0);
    let target = text
        .iter()
        .enumerate()
        .filter_map(|(index, character)| {
            let bounds = character.bounds?;
            let center = (bounds.top + bounds.bottom) * 0.5;
            center
                .is_finite()
                .then_some((index, (center - target_y).abs()))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map_or(0, |(index, _)| index);
    let start = line_start_index(text, target);
    let end = next_paragraph_start(text, target)
        .unwrap_or_else(|| (start + MAX_RESOLVED_PREVIEW_CHARACTERS).min(text.len()))
        .saturating_sub(1)
        .max(target);
    range_preview(text, start, end)
}

fn range_preview(text: &TextLayer, start: usize, end: usize) -> String {
    normalized_range_text(text, start, end, MAX_RESOLVED_PREVIEW_CHARACTERS)
}

fn normalized_range_text(text: &TextLayer, start: usize, end: usize, maximum: usize) -> String {
    let Some(selected) = text.get(start..=end.min(text.len().saturating_sub(1))) else {
        return String::new();
    };
    let mut result = String::new();
    let mut pending_space = false;
    let mut count = 0;
    for value in selected.iter().map(|character| character.value) {
        if value == '\0' || value == '\u{00ad}' {
            continue;
        }
        if value.is_whitespace() {
            pending_space = !result.is_empty();
            continue;
        }
        if pending_space {
            result.push(' ');
            count += 1;
            pending_space = false;
        }
        result.push(value);
        count += 1;
        if count >= maximum {
            result.push('…');
            break;
        }
    }
    result.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TextChar;

    fn laid_out(text: &str, line_height: f32) -> TextLayer {
        let mut line = 0;
        let mut column = 0;
        TextLayer::new(
            text.chars()
                .map(|value| {
                    if matches!(value, '\n' | '\r') {
                        line += 1;
                        column = 0;
                        return TextChar {
                            value,
                            bounds: None,
                        };
                    }
                    let left = 0.05 + column as f32 * 0.012;
                    let top = 0.05 + line as f32 * line_height;
                    column += 1;
                    TextChar {
                        value,
                        bounds: Some(TextBounds {
                            left,
                            top,
                            right: left + 0.01,
                            bottom: top + line_height * 0.7,
                        }),
                    }
                })
                .collect(),
        )
    }

    #[test]
    fn source_text_uses_annotation_geometry_and_keeps_inner_punctuation() {
        let text = laid_out("See [12] and [13]", 0.04);
        let source = link_source_text(
            &text,
            TextBounds {
                left: 0.095,
                top: 0.045,
                right: 0.155,
                bottom: 0.085,
            },
        );
        assert_eq!(source, "[12]");
        assert_eq!(source_numeric_marker(&source), Some(12));
    }

    #[test]
    fn numeric_citation_resolves_near_rough_target_and_expands_full_entry() {
        let source = laid_out("Prior work [12] is useful.", 0.04);
        let target = laid_out(
            "References\n[11] Earlier paper. Journal.\n[12] Chosen paper title. Authors.\nContinued journal and DOI text.\n[13] Following paper.\n",
            0.06,
        );
        let resolved = resolve_internal_link(
            &source,
            TextBounds {
                left: 0.178,
                top: 0.045,
                right: 0.232,
                bottom: 0.085,
            },
            &target,
            3,
            None,
            Some(0.18),
        );
        assert!(resolved.matched_source);
        assert!(resolved.preview.starts_with("[12] Chosen paper title."));
        assert!(resolved.preview.contains("Continued journal and DOI text."));
        assert!(!resolved.preview.contains("[13]"));
        assert!(resolved.y_fraction.is_some_and(|y| y > 0.1));
        assert!(resolved.text_runs.len() >= 2);
    }

    #[test]
    fn unmatched_source_retains_pdf_destination_and_rough_preview() {
        let source = laid_out("Jump elsewhere", 0.04);
        let target = laid_out("Destination paragraph text.", 0.05);
        let resolved = resolve_internal_link(
            &source,
            TextBounds {
                left: 0.04,
                top: 0.04,
                right: 0.24,
                bottom: 0.09,
            },
            &target,
            1,
            Some(0.4),
            Some(0.06),
        );
        assert!(!resolved.matched_source);
        assert_eq!(resolved.x_fraction, Some(0.4));
        assert_eq!(resolved.y_fraction, Some(0.06));
        assert_eq!(resolved.preview, "Destination paragraph text.");
    }

    #[test]
    fn author_year_entry_uses_hanging_indent_to_stop_before_the_next_reference() {
        let mut target = laid_out(
            "Smith, A. (2020). First paper title.\nJournal continuation text.\nJones, B. (2021). Next paper title.\n",
            0.06,
        )
        .as_slice()
        .to_vec();
        let first_break = target
            .iter()
            .position(|character| character.value == '\n')
            .unwrap();
        let second_break = target[first_break + 1..]
            .iter()
            .position(|character| character.value == '\n')
            .map(|offset| first_break + 1 + offset)
            .unwrap();
        for character in &mut target[first_break + 1..second_break] {
            if let Some(bounds) = character.bounds.as_mut() {
                bounds.left += 0.04;
                bounds.right += 0.04;
            }
        }
        let target = TextLayer::new(target);
        let (start, end) = meaningful_entry_range(&target, 0, 4, None);
        let preview = range_preview(&target, start, end);
        assert!(preview.contains("Journal continuation text."));
        assert!(!preview.contains("Jones"));
    }
}
