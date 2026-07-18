//! Projection from storage-neutral rich text to GPUI display text.

use crate::MarkdownEditorStyle;
use gpui::{FontStyle, FontWeight, SharedString, TextRun, UnderlineStyle, font, px};
use key_editor_core::{
    BlockKind, InlineStyle, RichTextBuffer, StyleRunCursor, line_at_offset, line_ranges,
};
use std::ops::Range;

#[derive(Clone, Debug)]
pub(crate) struct DisplayProjection {
    pub(crate) text: SharedString,
    lines: Vec<ProjectedLine>,
    spans: Vec<ProjectedSpan>,
}

#[derive(Clone, Debug)]
struct ProjectedLine {
    model_start: usize,
    model_end: usize,
    display_content_start: usize,
    display_end: usize,
}

#[derive(Clone, Debug)]
struct ProjectedSpan {
    display: Range<usize>,
    model: Option<Range<usize>>,
    style: InlineStyle,
}

impl DisplayProjection {
    pub(crate) fn new(buffer: &RichTextBuffer) -> Self {
        Self::new_with_run_operations(buffer).0
    }

    fn new_with_run_operations(buffer: &RichTextBuffer) -> (Self, usize) {
        let text_value = buffer.text();
        let block_kinds = buffer.block_kinds();
        let ranges = line_ranges(text_value);
        let mut text = String::new();
        let mut lines = Vec::with_capacity(ranges.len());
        let mut spans = Vec::new();
        let mut numbered_index = 0usize;
        let mut cursor = StyleRunCursor::new(buffer.style_runs());

        for (line_index, (model_start, model_end)) in ranges.into_iter().enumerate() {
            if line_index > 0 {
                let newline_offset = model_start - 1;
                let display_start = text.len();
                text.push('\n');
                push_projected_span(
                    &mut spans,
                    display_start..display_start + 1,
                    Some(newline_offset..model_start),
                    cursor.style_at(newline_offset),
                );
            }
            let display_start = text.len();
            let prefix = match block_kinds[line_index] {
                BlockKind::Paragraph => {
                    numbered_index = 0;
                    String::new()
                }
                BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
                    numbered_index = 0;
                    String::new()
                }
                BlockKind::Bulleted => {
                    numbered_index = 0;
                    "• ".to_owned()
                }
                BlockKind::Numbered => {
                    numbered_index += 1;
                    format!("{numbered_index}. ")
                }
                BlockKind::Quote => {
                    numbered_index = 0;
                    "› ".to_owned()
                }
            };
            text.push_str(&prefix);
            if !prefix.is_empty() {
                push_projected_span(
                    &mut spans,
                    display_start..text.len(),
                    None,
                    InlineStyle::default(),
                );
            }
            let display_content_start = text.len();
            text.push_str(&text_value[model_start..model_end]);
            cursor.for_each_overlap(model_start, model_end, |overlap, style| {
                let display_range = display_content_start + (overlap.start - model_start)
                    ..display_content_start + (overlap.end - model_start);
                push_projected_span(&mut spans, display_range, Some(overlap), style);
            });
            lines.push(ProjectedLine {
                model_start,
                model_end,
                display_content_start,
                display_end: text.len(),
            });
        }
        (
            Self {
                text: text.into(),
                lines,
                spans,
            },
            cursor.operations(),
        )
    }

    pub(crate) fn display_for_model(&self, model: usize) -> usize {
        let model = model.min(self.lines.last().map_or(0, |line| line.model_end));
        for line in &self.lines {
            if model <= line.model_end {
                return line.display_content_start + model.saturating_sub(line.model_start);
            }
        }
        self.text.len()
    }

    pub(crate) fn model_for_display(&self, display: usize) -> usize {
        let display = display.min(self.text.len());
        for line in &self.lines {
            if display <= line.display_end {
                if display <= line.display_content_start {
                    return line.model_start;
                }
                return (line.model_start + display - line.display_content_start)
                    .min(line.model_end);
            }
        }
        self.lines.last().map_or(0, |line| line.model_end)
    }
}

pub(crate) fn display_text_runs(
    buffer: &RichTextBuffer,
    projection: &DisplayProjection,
    palette: MarkdownEditorStyle,
) -> Vec<TextRun> {
    let mut result = Vec::new();
    let selection = buffer.selection();
    let marked_range = buffer.marked_range();
    let text = buffer.text();
    let block_kinds = buffer.block_kinds();
    for span in &projection.spans {
        if let Some(model) = &span.model {
            let mut boundaries = vec![model.start, model.end];
            for boundary in [
                selection.start,
                selection.end,
                marked_range
                    .as_ref()
                    .map_or(usize::MAX, |range| range.start),
                marked_range.as_ref().map_or(usize::MAX, |range| range.end),
            ] {
                if boundary > model.start && boundary < model.end {
                    boundaries.push(boundary);
                }
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            for pair in boundaries.windows(2) {
                let start = pair[0];
                let end = pair[1];
                let selected =
                    !selection.is_empty() && start < selection.end && end > selection.start;
                let marked = marked_range
                    .as_ref()
                    .is_some_and(|range| start < range.end && end > range.start);
                result.push(make_text_run(
                    end - start,
                    span.style,
                    block_kinds[line_at_offset(text, start)],
                    selected,
                    marked,
                    palette,
                ));
            }
        } else {
            result.push(make_text_run(
                span.display.end - span.display.start,
                span.style,
                BlockKind::Paragraph,
                false,
                false,
                palette,
            ));
        }
    }
    result
}

fn make_text_run(
    len: usize,
    style: InlineStyle,
    block: BlockKind,
    selected: bool,
    marked: bool,
    palette: MarkdownEditorStyle,
) -> TextRun {
    let mut selected_font = if style.code() {
        font(".ZedMono")
    } else {
        font(".SystemUIFont")
    };
    if style.bold()
        || matches!(
            block,
            BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3
        )
    {
        selected_font.weight = FontWeight::BOLD;
    }
    if style.italic() {
        selected_font.style = FontStyle::Italic;
    }
    TextRun {
        len,
        font: selected_font,
        color: palette.text,
        background_color: if selected {
            Some(palette.selection)
        } else if style.code() {
            Some(palette.surface_subtle)
        } else {
            None
        },
        underline: marked.then_some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(palette.accent),
            wavy: false,
        }),
        strikethrough: None,
    }
}

fn push_projected_span(
    spans: &mut Vec<ProjectedSpan>,
    display: Range<usize>,
    model: Option<Range<usize>>,
    style: InlineStyle,
) {
    if !display.is_empty() {
        spans.push(ProjectedSpan {
            display,
            model,
            style,
        });
    }
}
