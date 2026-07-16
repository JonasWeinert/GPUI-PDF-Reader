use crate::model::{PdfLink, PdfLinkTarget, TextBounds, TextLayer};
use crate::search::{SearchPageOutcome, SearchQuery, search_page};
use std::collections::BTreeMap;

const MAX_REFERENCE_ENTRIES: usize = 1_000;
const MAX_SYNTHETIC_LINKS: usize = 10_000;
const MAX_REFERENCE_TEXT_CHARACTERS: usize = 2_000;
const MAX_CITATION_SOURCE_CHARACTERS: usize = 24;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScientificSignals {
    pub reference_entries: usize,
    pub doi_entries: usize,
    pub bracket_citations: usize,
    pub superscript_citations: usize,
    pub concentrated_internal_links: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScientificAnalysis {
    pub is_scientific: bool,
    pub synthetic_links: Vec<PdfLink>,
    pub signals: ScientificSignals,
}

#[derive(Clone, Debug)]
struct ReferenceEntry {
    number: u32,
    page: usize,
    x_fraction: Option<f32>,
    y_fraction: Option<f32>,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CitationKind {
    Bracket,
    Superscript,
}

#[derive(Clone, Debug)]
struct CitationCandidate {
    page: usize,
    bounds: TextBounds,
    number: u32,
    kind: CitationKind,
}

pub struct ScientificAnalyzer {
    page_count: usize,
    tail_start: usize,
    page_order: Vec<usize>,
    reference_heading: Option<(usize, usize)>,
    references: BTreeMap<u32, ReferenceEntry>,
    provisional_references: BTreeMap<u32, ReferenceEntry>,
    citations: Vec<CitationCandidate>,
    existing_links: Vec<PdfLink>,
    concentrated_internal_links: usize,
    doi_link_count: usize,
}

impl ScientificAnalyzer {
    pub fn new(page_count: usize, existing_links: &[PdfLink]) -> Self {
        let tail_pages = page_count.div_ceil(3).clamp(3, 32).min(page_count);
        let tail_start = page_count.saturating_sub(tail_pages);
        let page_order = (tail_start..page_count)
            .chain(0..tail_start)
            .collect::<Vec<_>>();
        let internal_targets = existing_links
            .iter()
            .filter_map(|link| match link.target {
                PdfLinkTarget::Internal { page, .. } => Some(page),
                PdfLinkTarget::External { .. } => None,
            })
            .collect::<Vec<_>>();
        let concentrated_internal_links = internal_targets
            .iter()
            .filter(|page| **page >= tail_start)
            .count();
        let doi_link_count = existing_links
            .iter()
            .filter(|link| {
                matches!(
                    &link.target,
                    PdfLinkTarget::External { url }
                        if url.to_ascii_lowercase().contains("doi.org/10.")
                )
            })
            .count();
        Self {
            page_count,
            tail_start,
            page_order,
            reference_heading: None,
            references: BTreeMap::new(),
            provisional_references: BTreeMap::new(),
            citations: Vec::new(),
            existing_links: existing_links.to_vec(),
            concentrated_internal_links,
            doi_link_count,
        }
    }

    pub fn page_order(&self) -> &[usize] {
        &self.page_order
    }

    pub fn ingest_page(&mut self, page: usize, text: &TextLayer) {
        let heading = find_reference_heading(page, text);
        if let Some(heading_start) = heading {
            if self.reference_heading.is_none() {
                self.reference_heading = Some((page, heading_start));
                self.references.clear();
            }
            self.collect_citations(page, text, 0, heading_start);
            self.collect_references(page, text, heading_start, false);
            return;
        }

        if self
            .reference_heading
            .is_some_and(|(heading_page, _)| page > heading_page)
        {
            self.collect_references(page, text, 0, false);
        } else {
            self.collect_citations(page, text, 0, text.len());
            if page >= self.tail_start {
                self.collect_references(page, text, 0, true);
            }
        }
    }

    pub fn finish(mut self) -> ScientificAnalysis {
        if self.references.len() < 5
            && sequential_reference_count(&self.provisional_references) >= 5
        {
            self.references = std::mem::take(&mut self.provisional_references);
        }
        let doi_entries = self
            .references
            .values()
            .filter(|entry| detect_doi(&entry.text).is_some())
            .count()
            .saturating_add(self.doi_link_count);
        let bracket_citations = self
            .citations
            .iter()
            .filter(|citation| citation.kind == CitationKind::Bracket)
            .count();
        let superscript_citations = self
            .citations
            .iter()
            .filter(|citation| citation.kind == CitationKind::Superscript)
            .count();
        let signals = ScientificSignals {
            reference_entries: self.references.len(),
            doi_entries,
            bracket_citations,
            superscript_citations,
            concentrated_internal_links: self.concentrated_internal_links,
        };
        let reference_evidence = self.reference_heading.is_some() || signals.reference_entries >= 5;
        let citation_evidence = signals.bracket_citations >= 4
            || signals.superscript_citations >= 4
            || signals.concentrated_internal_links >= 6;
        let bibliographic_evidence = signals.reference_entries >= 8 || signals.doi_entries >= 2;
        let is_scientific = self.page_count >= 2
            && reference_evidence
            && (citation_evidence || bibliographic_evidence);
        let synthetic_links = if is_scientific {
            self.synthetic_links(signals.superscript_citations >= 3)
        } else {
            Vec::new()
        };
        ScientificAnalysis {
            is_scientific,
            synthetic_links,
            signals,
        }
    }

    fn collect_references(
        &mut self,
        page: usize,
        text: &TextLayer,
        start: usize,
        provisional: bool,
    ) {
        let entries = parse_reference_entries(page, text, start);
        let destination = if provisional {
            &mut self.provisional_references
        } else {
            &mut self.references
        };
        for entry in entries {
            if destination.len() >= MAX_REFERENCE_ENTRIES {
                break;
            }
            destination.entry(entry.number).or_insert(entry);
        }
    }

    fn collect_citations(&mut self, page: usize, text: &TextLayer, start: usize, end: usize) {
        if self.citations.len() >= MAX_SYNTHETIC_LINKS {
            return;
        }
        self.citations.extend(
            bracket_citations(page, text, start, end)
                .into_iter()
                .take(MAX_SYNTHETIC_LINKS.saturating_sub(self.citations.len())),
        );
        if self.citations.len() >= MAX_SYNTHETIC_LINKS {
            return;
        }
        self.citations.extend(
            superscript_citations(page, text, start, end)
                .into_iter()
                .take(MAX_SYNTHETIC_LINKS.saturating_sub(self.citations.len())),
        );
    }

    fn synthetic_links(&self, include_superscripts: bool) -> Vec<PdfLink> {
        let mut links = Vec::new();
        for citation in &self.citations {
            if links.len() >= MAX_SYNTHETIC_LINKS {
                break;
            }
            if citation.kind == CitationKind::Superscript && !include_superscripts {
                continue;
            }
            let Some(reference) = self.references.get(&citation.number) else {
                continue;
            };
            if self.existing_links.iter().any(|link| {
                link.page == citation.page && bounds_overlap(link.bounds, citation.bounds)
            }) || links.iter().any(|link: &PdfLink| {
                link.page == citation.page && bounds_overlap(link.bounds, citation.bounds)
            }) {
                continue;
            }
            links.push(PdfLink {
                id: self.existing_links.len() + links.len(),
                page: citation.page,
                bounds: citation.bounds,
                target: PdfLinkTarget::Internal {
                    page: reference.page,
                    x_fraction: reference.x_fraction,
                    y_fraction: reference.y_fraction,
                },
            });
        }
        links
    }
}

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

fn find_reference_heading(page: usize, text: &TextLayer) -> Option<usize> {
    for heading in ["references", "bibliography"] {
        let query = SearchQuery::new(heading).ok()?;
        let SearchPageOutcome::Complete(results) =
            search_page(page, text.as_slice(), &query, 4, || false)
        else {
            continue;
        };
        if let Some(result) = results.matches.into_iter().find(|result| {
            let before = &text[..result.id.start];
            before
                .iter()
                .rev()
                .take(8)
                .all(|character| character.value.is_whitespace())
                || result
                    .highlight_runs
                    .first()
                    .is_some_and(|run| run.left < 0.35)
        }) {
            return Some(result.id.end.saturating_add(1));
        }
    }
    None
}

fn parse_reference_entries(page: usize, text: &TextLayer, start: usize) -> Vec<ReferenceEntry> {
    let mut markers = Vec::new();
    let mut index = start.min(text.len());
    let mut line_start = index == 0
        || text
            .get(index.saturating_sub(1))
            .is_some_and(|character| matches!(character.value, '\n' | '\r'));
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
        if let Some((number, marker_end)) = parse_reference_marker(text, index) {
            markers.push((index, marker_end, number));
        }
        line_start = false;
        index += 1;
    }

    let mut entries = Vec::new();
    for (position, (entry_start, marker_end, number)) in markers.iter().copied().enumerate() {
        let entry_end = markers
            .get(position + 1)
            .map_or(text.len(), |(next, _, _)| *next)
            .saturating_sub(1);
        let entry_text =
            normalized_text(text, entry_start, entry_end, MAX_REFERENCE_TEXT_CHARACTERS);
        if entry_text
            .chars()
            .filter(|value| value.is_alphabetic())
            .count()
            < 12
        {
            continue;
        }
        let bounds = text[entry_start..marker_end.min(text.len())]
            .iter()
            .filter_map(|character| character.bounds)
            .reduce(union_bounds)
            .or_else(|| {
                text[entry_start..=entry_end.min(text.len().saturating_sub(1))]
                    .iter()
                    .filter_map(|character| character.bounds)
                    .next()
            });
        entries.push(ReferenceEntry {
            number,
            page,
            x_fraction: bounds.map(|bounds| (bounds.left + bounds.right) * 0.5),
            y_fraction: bounds.map(|bounds| bounds.top),
            text: entry_text,
        });
    }
    entries
}

fn parse_reference_marker(text: &TextLayer, start: usize) -> Option<(u32, usize)> {
    let mut index = start;
    let opening = text
        .get(index)
        .map(|character| character.value)
        .filter(|value| matches!(value, '[' | '('));
    if opening.is_some() {
        index += 1;
    }
    let digit_start = index;
    while index < text.len() && text[index].value.is_ascii_digit() && index - digit_start <= 4 {
        index += 1;
    }
    if digit_start == index || index - digit_start > 4 {
        return None;
    }
    let number = text[digit_start..index]
        .iter()
        .map(|character| character.value)
        .collect::<String>()
        .parse::<u32>()
        .ok()?;
    if number == 0 || number > MAX_REFERENCE_ENTRIES as u32 {
        return None;
    }
    let terminator = text.get(index).map(|character| character.value);
    match opening {
        Some('[') if terminator == Some(']') => index += 1,
        Some('(') if terminator == Some(')') => index += 1,
        Some(_) => return None,
        None if terminator.is_some_and(|value| matches!(value, '.' | ':')) => index += 1,
        None if terminator.is_some_and(char::is_whitespace) => {}
        None => return None,
    }
    Some((number, index))
}

fn bracket_citations(
    page: usize,
    text: &TextLayer,
    start: usize,
    end: usize,
) -> Vec<CitationCandidate> {
    let mut result = Vec::new();
    let mut index = start.min(text.len());
    let end = end.min(text.len());
    while index < end {
        if text[index].value != '[' {
            index += 1;
            continue;
        }
        let source_start = index;
        index += 1;
        let digit_start = index;
        while index < end && text[index].value.is_ascii_digit() {
            index += 1;
        }
        if digit_start == index {
            continue;
        }
        let number = text[digit_start..index]
            .iter()
            .map(|character| character.value)
            .collect::<String>()
            .parse::<u32>()
            .ok();
        while index < end
            && (text[index].value.is_ascii_digit()
                || matches!(text[index].value, ',' | '-' | '–' | '—')
                || text[index].value.is_whitespace())
            && index - source_start <= MAX_CITATION_SOURCE_CHARACTERS
        {
            index += 1;
        }
        if text.get(index).map(|character| character.value) != Some(']') {
            continue;
        }
        let source_end = index;
        index += 1;
        if let (Some(number), Some(bounds)) = (number, range_bounds(text, source_start, source_end))
        {
            result.push(CitationCandidate {
                page,
                bounds,
                number,
                kind: CitationKind::Bracket,
            });
        }
    }
    result
}

fn superscript_citations(
    page: usize,
    text: &TextLayer,
    start: usize,
    end: usize,
) -> Vec<CitationCandidate> {
    let mut body_heights = text[start.min(text.len())..end.min(text.len())]
        .iter()
        .filter(|character| character.value.is_alphabetic())
        .filter_map(|character| character.bounds)
        .map(|bounds| (bounds.bottom - bounds.top).abs())
        .filter(|height| height.is_finite() && *height > 0.0001)
        .collect::<Vec<_>>();
    if body_heights.len() < 16 {
        return Vec::new();
    }
    body_heights.sort_by(f32::total_cmp);
    let median_height = body_heights[body_heights.len() / 2];
    let mut result = Vec::new();
    let mut index = start.min(text.len());
    let end = end.min(text.len());
    while index < end {
        if !text[index].value.is_ascii_digit() {
            index += 1;
            continue;
        }
        let source_start = index;
        while index < end
            && (text[index].value.is_ascii_digit()
                || matches!(text[index].value, ',' | '-' | '–' | '—'))
            && index - source_start < MAX_CITATION_SOURCE_CHARACTERS
        {
            index += 1;
        }
        let source_end = index.saturating_sub(1);
        let Some(bounds) = range_bounds(text, source_start, source_end) else {
            continue;
        };
        let run_height = (bounds.bottom - bounds.top).abs();
        if run_height >= median_height * 0.84 {
            continue;
        }
        let neighbor = nearby_body_bounds(text, source_start, source_end, median_height);
        let Some(neighbor) = neighbor else {
            continue;
        };
        let neighbor_height = (neighbor.bottom - neighbor.top).abs();
        let raised = bounds.top + run_height * 0.45 < neighbor.top + neighbor_height * 0.3
            || bounds.bottom < neighbor.bottom - neighbor_height * 0.08;
        if !raised {
            continue;
        }
        let number = text[source_start..=source_end]
            .iter()
            .take_while(|character| character.value.is_ascii_digit())
            .map(|character| character.value)
            .collect::<String>()
            .parse::<u32>()
            .ok();
        if let Some(number) = number {
            result.push(CitationCandidate {
                page,
                bounds,
                number,
                kind: CitationKind::Superscript,
            });
        }
    }
    result
}

fn nearby_body_bounds(
    text: &TextLayer,
    source_start: usize,
    source_end: usize,
    median_height: f32,
) -> Option<TextBounds> {
    (source_end.saturating_add(1)..(source_end + 9).min(text.len()))
        .chain(source_start.saturating_sub(8)..source_start)
        .filter_map(|index| text.get(index))
        .filter(|character| character.value.is_alphabetic())
        .filter_map(|character| character.bounds)
        .find(|bounds| {
            let height = (bounds.bottom - bounds.top).abs();
            height >= median_height * 0.9
        })
}

fn sequential_reference_count(entries: &BTreeMap<u32, ReferenceEntry>) -> usize {
    let mut longest = 0;
    let mut current = 0;
    let mut previous = None;
    for number in entries.keys().copied() {
        if previous.is_some_and(|previous| number == previous + 1) {
            current += 1;
        } else {
            current = 1;
        }
        longest = longest.max(current);
        previous = Some(number);
    }
    longest
}

fn normalized_text(text: &TextLayer, start: usize, end: usize, maximum: usize) -> String {
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

fn range_bounds(text: &TextLayer, start: usize, end: usize) -> Option<TextBounds> {
    text.get(start..=end)?
        .iter()
        .filter_map(|character| character.bounds)
        .reduce(union_bounds)
}

fn union_bounds(left: TextBounds, right: TextBounds) -> TextBounds {
    TextBounds {
        left: left.left.min(right.left),
        top: left.top.min(right.top),
        right: left.right.max(right.right),
        bottom: left.bottom.max(right.bottom),
    }
}

fn bounds_overlap(left: TextBounds, right: TextBounds) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TextChar;

    fn text_layer(lines: &[(&str, f32, f32, f32)]) -> TextLayer {
        let mut characters = Vec::new();
        for (line_index, (text, left, top, height)) in lines.iter().enumerate() {
            let mut x = *left;
            for value in text.chars() {
                characters.push(TextChar {
                    value,
                    bounds: (!value.is_whitespace()).then_some(TextBounds {
                        left: x,
                        top: *top,
                        right: x + height * 0.55,
                        bottom: top + height,
                    }),
                });
                x += height * 0.58;
            }
            if line_index + 1 != lines.len() {
                characters.push(TextChar {
                    value: '\n',
                    bounds: None,
                });
            }
        }
        TextLayer::new(characters)
    }

    #[test]
    fn doi_detection_trims_sentence_punctuation_and_normalizes_case() {
        assert_eq!(
            detect_doi("Available at DOI:10.1001/JAMA.2016.17216).").as_deref(),
            Some("10.1001/jama.2016.17216")
        );
        assert_eq!(detect_doi("not a doi 10.12/no"), None);
    }

    #[test]
    fn reference_parser_handles_bracketed_and_period_markers() {
        let text = text_layer(&[
            ("References", 0.05, 0.05, 0.02),
            ("[1] First paper title. doi:10.1000/one", 0.05, 0.10, 0.02),
            ("2. Second paper title and journal.", 0.05, 0.15, 0.02),
            ("3", 0.05, 0.95, 0.02),
        ]);
        let entries = parse_reference_entries(4, &text, 0);
        assert_eq!(
            entries.iter().map(|entry| entry.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(detect_doi(&entries[0].text).as_deref(), Some("10.1000/one"));
    }

    #[test]
    fn analyzer_classifies_and_links_unannotated_superscript_citations() {
        let references = text_layer(&[
            ("REFERENCES", 0.05, 0.05, 0.024),
            (
                "1. First reference title. doi:10.1000/one",
                0.05,
                0.12,
                0.018,
            ),
            (
                "2. Second reference title. doi:10.1000/two",
                0.05,
                0.17,
                0.018,
            ),
            ("3. Third reference title.", 0.05, 0.22, 0.018),
            ("4. Fourth reference title.", 0.05, 0.27, 0.018),
            ("5. Fifth reference title.", 0.05, 0.32, 0.018),
            ("6. Sixth reference title.", 0.05, 0.37, 0.018),
            ("7. Seventh reference title.", 0.05, 0.42, 0.018),
            ("8. Eighth reference title.", 0.05, 0.47, 0.018),
        ]);
        let mut body = text_layer(&[
            ("Prior work supports this claim", 0.05, 0.10, 0.022),
            ("Further evidence also exists", 0.05, 0.18, 0.022),
            ("Another finding was reported", 0.05, 0.26, 0.022),
            ("A final result is available", 0.05, 0.34, 0.022),
        ])
        .as_slice()
        .to_vec();
        for (line, number) in [(0, '1'), (1, '2'), (2, '3'), (3, '4')] {
            let insertion = body
                .iter()
                .enumerate()
                .filter(|(_, character)| character.value == '\n')
                .nth(line)
                .map_or(body.len(), |(index, _)| index);
            let top = 0.10 + line as f32 * 0.08 - 0.008;
            body.insert(
                insertion,
                TextChar {
                    value: number,
                    bounds: Some(TextBounds {
                        left: 0.38,
                        top,
                        right: 0.389,
                        bottom: top + 0.012,
                    }),
                },
            );
        }
        let body = TextLayer::new(body);

        let mut analyzer = ScientificAnalyzer::new(3, &[]);
        analyzer.ingest_page(2, &references);
        analyzer.ingest_page(0, &body);
        analyzer.ingest_page(1, &TextLayer::empty());
        let analysis = analyzer.finish();
        assert!(analysis.is_scientific);
        assert_eq!(analysis.signals.reference_entries, 8);
        assert!(analysis.signals.superscript_citations >= 4);
        assert!(analysis.synthetic_links.len() >= 4);
        assert!(
            analysis
                .synthetic_links
                .iter()
                .all(|link| { matches!(link.target, PdfLinkTarget::Internal { page: 2, .. }) })
        );
    }

    #[test]
    fn ordinary_document_with_numbered_lists_is_not_scientific() {
        let text = text_layer(&[
            ("Shopping list", 0.05, 0.05, 0.02),
            ("1. Apples and oranges", 0.05, 0.10, 0.02),
            ("2. Bread and coffee", 0.05, 0.15, 0.02),
        ]);
        let mut analyzer = ScientificAnalyzer::new(2, &[]);
        analyzer.ingest_page(0, &text);
        analyzer.ingest_page(1, &TextLayer::empty());
        assert!(!analyzer.finish().is_scientific);
    }
}
