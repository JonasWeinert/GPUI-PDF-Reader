//! UI-agnostic Markdown-backed rich-text editing.
//!
//! The crate owns the editor's document state, canonical Markdown storage,
//! selections, formatting, Unicode movement, and transactional size limits.
//! It deliberately has no GPUI, filesystem, networking, or PDF dependency;
//! presentation crates adapt it to native text input and rendering.

#![forbid(unsafe_code)]

use std::{collections::HashMap, fmt, ops::Range};

/// Default persistence limit used by the current editor configuration.
pub const DEFAULT_MAX_MARKDOWN_BYTES: usize = 1024 * 1024;

/// Markdown input or its canonical representation exceeds the configured
/// persistence limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLimitExceeded;

impl fmt::Display for MarkdownLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "this document cannot be edited because its canonical Markdown exceeds the configured storage limit",
        )
    }
}

impl std::error::Error for MarkdownLimitExceeded {}

/// The block style of one logical line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading1,
    Heading2,
    Heading3,
    Bulleted,
    Numbered,
    Quote,
}

/// Semantic inline formatting. Code is exclusive with bold and italic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InlineStyle(u8);

impl InlineStyle {
    const BOLD: u8 = 1 << 0;
    const ITALIC: u8 = 1 << 1;
    const CODE: u8 = 1 << 2;

    pub fn bold(self) -> bool {
        self.0 & Self::BOLD != 0
    }

    pub fn italic(self) -> bool {
        self.0 & Self::ITALIC != 0
    }

    pub fn code(self) -> bool {
        self.0 & Self::CODE != 0
    }

    /// Returns whether no inline formatting is active.
    pub fn is_plain(self) -> bool {
        self == Self::default()
    }

    fn with_flag(mut self, flag: u8, enabled: bool) -> Self {
        if enabled {
            if flag == Self::CODE {
                self.0 = Self::CODE;
            } else {
                self.0 &= !Self::CODE;
                self.0 |= flag;
            }
        } else {
            self.0 &= !flag;
        }
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StyleRun {
    range: Range<usize>,
    style: InlineStyle,
}

impl StyleRun {
    /// UTF-8 byte range covered by this normalized run.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub fn style(&self) -> InlineStyle {
        self.style
    }
}

/// Monotonic access to normalized style runs. A run that crosses a line
/// boundary remains current and is visited once on each crossed line; every
/// other run is advanced past exactly once.
pub struct StyleRunCursor<'a> {
    runs: &'a [StyleRun],
    index: usize,
    operations: usize,
}

impl<'a> StyleRunCursor<'a> {
    pub fn new(runs: &'a [StyleRun]) -> Self {
        Self {
            runs,
            index: 0,
            operations: 0,
        }
    }

    fn advance_to(&mut self, offset: usize) {
        while self
            .runs
            .get(self.index)
            .is_some_and(|run| run.range.end <= offset)
        {
            self.index += 1;
            self.operations += 1;
        }
    }

    pub fn style_at(&mut self, offset: usize) -> InlineStyle {
        self.style_extent_at(offset).0
    }

    fn style_extent_at(&mut self, offset: usize) -> (InlineStyle, usize) {
        self.advance_to(offset);
        let Some(run) = self.runs.get(self.index) else {
            return (InlineStyle::default(), offset);
        };
        self.operations += 1;
        if run.range.start <= offset && offset < run.range.end {
            (run.style, run.range.end)
        } else {
            (InlineStyle::default(), offset)
        }
    }

    pub fn for_each_overlap(
        &mut self,
        start: usize,
        end: usize,
        mut visit: impl FnMut(Range<usize>, InlineStyle),
    ) {
        if start >= end {
            return;
        }
        self.advance_to(start);
        let mut index = self.index;
        while let Some(run) = self.runs.get(index) {
            if run.range.start >= end {
                break;
            }
            self.operations += 1;
            let overlap = run.range.start.max(start)..run.range.end.min(end);
            if !overlap.is_empty() {
                visit(overlap, run.style);
            }
            if run.range.end <= end {
                index += 1;
                self.index = index;
            } else {
                break;
            }
        }
    }

    /// Number of run visits made so far. Intended for performance regression
    /// tests and diagnostics rather than application behavior.
    pub fn operations(&self) -> usize {
        self.operations
    }
}

/// Pure rich-text state. All ranges are UTF-8 byte ranges on character
/// boundaries; conversion to UTF-16 is contained here for native IME APIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RichTextBuffer {
    text: String,
    runs: Vec<StyleRun>,
    blocks: Vec<BlockKind>,
    selection: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    pending_style: Option<InlineStyle>,
    max_markdown_bytes: usize,
}

impl RichTextBuffer {
    /// Creates an empty document using the default persistence limit.
    pub fn new() -> Self {
        Self::parse_markdown("", DEFAULT_MAX_MARKDOWN_BYTES)
    }

    /// Creates an empty document with a caller-defined canonical Markdown
    /// byte limit.
    pub fn with_max_markdown_bytes(max_markdown_bytes: usize) -> Self {
        Self::parse_markdown("", max_markdown_bytes)
    }

    /// Parses persisted Markdown while enforcing the canonical persistence
    /// limit. Production callers must use this path: raw input can fit the
    /// sidecar limit while canonical escaping makes it larger.
    pub fn try_from_markdown(markdown: &str) -> Result<Self, MarkdownLimitExceeded> {
        Self::try_from_markdown_with_limit(markdown, DEFAULT_MAX_MARKDOWN_BYTES)
    }

    /// Parses persisted Markdown with a caller-supplied canonical byte limit.
    ///
    /// This keeps the rich-text model reusable outside PDF comments while the
    /// default constructor continues to enforce the reader's 1 MiB contract.
    pub fn try_from_markdown_with_limit(
        markdown: &str,
        max_markdown_bytes: usize,
    ) -> Result<Self, MarkdownLimitExceeded> {
        if markdown.len() > max_markdown_bytes {
            return Err(MarkdownLimitExceeded);
        }
        let buffer = Self::parse_markdown(markdown, max_markdown_bytes);
        if buffer.fits_persistence_limit() {
            Ok(buffer)
        } else {
            Err(MarkdownLimitExceeded)
        }
    }

    /// Convenience parser for Markdown whose canonical size is already a
    /// trusted invariant. It panics rather than silently truncating invalid
    /// input; production data-loading paths use [`Self::try_from_markdown`].
    pub fn from_trusted_markdown(markdown: &str) -> Self {
        Self::try_from_markdown(markdown)
            .expect("trusted comment Markdown must fit the canonical 1 MiB limit")
    }

    #[doc(hidden)]
    pub fn from_markdown(markdown: &str) -> Self {
        Self::from_trusted_markdown(markdown)
    }

    #[doc(hidden)]
    pub fn parse_markdown(markdown: &str, max_markdown_bytes: usize) -> Self {
        let markdown = normalize_newlines(markdown);
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut blocks = Vec::new();

        for (line_index, source_line) in markdown.split('\n').enumerate() {
            if line_index > 0 {
                let start = text.len();
                text.push('\n');
                push_style_run(&mut runs, start..start + 1, InlineStyle::default());
            }

            let (kind, line) = parse_block_prefix(source_line);
            blocks.push(kind);
            parse_inline(line, &mut text, &mut runs);
        }

        if blocks.is_empty() {
            blocks.push(BlockKind::Paragraph);
        }
        normalize_runs(&text, &mut runs);
        let cursor = text.len();
        Self {
            text,
            runs,
            blocks,
            selection: cursor..cursor,
            selection_reversed: false,
            marked_range: None,
            pending_style: None,
            max_markdown_bytes,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Normalized style runs covering the complete visible text.
    pub fn style_runs(&self) -> &[StyleRun] {
        &self.runs
    }

    /// One block kind for every logical line in [`Self::text`].
    pub fn block_kinds(&self) -> &[BlockKind] {
        &self.blocks
    }

    /// Current ordered UTF-8 byte selection.
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    /// Whether the selection cursor is at the start rather than the end.
    pub fn selection_is_reversed(&self) -> bool {
        self.selection_reversed
    }

    /// Current native IME composition range, expressed as UTF-8 bytes.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    /// Inline style that will be applied to text inserted at the caret.
    pub fn pending_inline_style(&self) -> Option<InlineStyle> {
        self.pending_style
    }

    pub fn max_markdown_bytes(&self) -> usize {
        self.max_markdown_bytes
    }

    pub fn markdown(&self) -> String {
        self.markdown_with_run_operations().0
    }

    pub fn fits_persistence_limit(&self) -> bool {
        markdown_fits_with_limit(
            &self.text,
            &self.runs,
            &self.blocks,
            self.max_markdown_bytes,
        )
    }

    #[doc(hidden)]
    pub fn markdown_with_run_operations(&self) -> (String, usize) {
        let mut output = String::new();
        let mut cursor = StyleRunCursor::new(&self.runs);
        for (line_index, (start, end)) in line_ranges(&self.text).into_iter().enumerate() {
            if line_index > 0 {
                output.push('\n');
            }
            match self.blocks[line_index] {
                BlockKind::Paragraph => {}
                BlockKind::Heading1 => output.push_str("# "),
                BlockKind::Heading2 => output.push_str("## "),
                BlockKind::Heading3 => output.push_str("### "),
                BlockKind::Bulleted => output.push_str("- "),
                BlockKind::Numbered => output.push_str("1. "),
                BlockKind::Quote => output.push_str("> "),
            }

            let (first_style, first_run_end) = cursor.style_extent_at(start);
            if self.blocks[line_index] == BlockKind::Paragraph
                && paragraph_prefix_needs_escape(&self.text, start, end, first_style, first_run_end)
            {
                output.push('\\');
            }
            cursor.for_each_overlap(start, end, |overlap, style| {
                encode_inline(&self.text[overlap], style, &mut output);
            });
        }
        (output, cursor.operations())
    }

    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// Sets an ordered UTF-8 byte selection. Non-boundary offsets are rounded
    /// down safely and out-of-range offsets are clamped.
    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        let start = floor_char_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_char_boundary(&self.text, range.end.min(self.text.len()));
        self.selection = start.min(end)..start.max(end);
        self.selection_reversed = reversed && !self.selection.is_empty();
        self.pending_style = None;
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = floor_char_boundary(&self.text, offset.min(self.text.len()));
        self.selection = offset..offset;
        self.selection_reversed = false;
        self.pending_style = None;
    }

    pub fn select_to(&mut self, offset: usize) {
        let offset = floor_char_boundary(&self.text, offset.min(self.text.len()));
        let anchor = if self.selection_reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        self.selection = anchor.min(offset)..anchor.max(offset);
        self.selection_reversed = offset < anchor;
        self.pending_style = None;
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.selection_reversed = false;
        self.pending_style = None;
    }

    /// Clears native IME composition without changing text or selection.
    pub fn clear_marked_range(&mut self) {
        self.marked_range = None;
    }

    /// Replaces the current selection if the resulting serialized Markdown
    /// stays within the persistence limit. A rejection leaves every field,
    /// including the selection, marked range, and pending style, untouched.
    pub fn replace_selection(&mut self, replacement: &str) -> bool {
        let range = self.selection.clone();
        if self.replace_range(range, replacement).is_none() {
            return false;
        }
        self.marked_range = None;
        true
    }

    pub fn backspace(&mut self) -> bool {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor_offset();
            let previous = previous_grapheme_boundary(&self.text, cursor);
            previous..cursor
        } else {
            self.selection.clone()
        };
        if self.replace_range(range, "").is_none() {
            return false;
        }
        self.marked_range = None;
        true
    }

    pub fn delete_forward(&mut self) -> bool {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor_offset();
            let next = next_grapheme_boundary(&self.text, cursor);
            cursor..next
        } else {
            self.selection.clone()
        };
        if self.replace_range(range, "").is_none() {
            return false;
        }
        self.marked_range = None;
        true
    }

    pub fn toggle_bold(&mut self) -> bool {
        self.toggle_inline(InlineStyle::BOLD)
    }

    pub fn toggle_italic(&mut self) -> bool {
        self.toggle_inline(InlineStyle::ITALIC)
    }

    pub fn toggle_code(&mut self) -> bool {
        self.toggle_inline(InlineStyle::CODE)
    }

    pub fn toggle_bulleted_list(&mut self) -> bool {
        self.toggle_block(BlockKind::Bulleted)
    }

    pub fn toggle_numbered_list(&mut self) -> bool {
        self.toggle_block(BlockKind::Numbered)
    }

    pub fn insert_newline(&mut self) -> bool {
        let cursor = self.cursor_offset();
        let line = line_at_offset(&self.text, cursor);
        let (line_start, line_end) = line_ranges(&self.text)[line];
        let kind = self.blocks[line];

        if self.selection.is_empty()
            && matches!(kind, BlockKind::Bulleted | BlockKind::Numbered)
            && self.text[line_start..line_end].trim().is_empty()
        {
            let mut blocks = self.blocks.clone();
            blocks[line] = BlockKind::Paragraph;
            if !markdown_fits_with_limit(&self.text, &self.runs, &blocks, self.max_markdown_bytes) {
                return false;
            }
            self.blocks = blocks;
            self.pending_style = Some(InlineStyle::default());
            return true;
        }

        let range = self.selection.clone();
        if self.replace_range(range, "\n").is_none() {
            return false;
        }
        if matches!(
            kind,
            BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3
        ) && cursor == line_end
            && let Some(next) = self.blocks.get_mut(line + 1)
        {
            *next = BlockKind::Paragraph;
        }
        self.marked_range = None;
        true
    }

    pub fn bold_active(&self) -> bool {
        self.inline_active(InlineStyle::BOLD)
    }

    pub fn italic_active(&self) -> bool {
        self.inline_active(InlineStyle::ITALIC)
    }

    pub fn code_active(&self) -> bool {
        self.inline_active(InlineStyle::CODE)
    }

    pub fn bulleted_list_active(&self) -> bool {
        self.block_active(BlockKind::Bulleted)
    }

    pub fn numbered_list_active(&self) -> bool {
        self.block_active(BlockKind::Numbered)
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16(&self.text, offset)
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(&self.text, offset)
    }

    pub fn range_to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// Applies a native text replacement expressed in UTF-16 units. This is
    /// also used by ordinary keyboard input, while paste uses the same bounded
    /// byte-range primitive through [`Self::replace_selection`].
    pub fn replace_text_utf16(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        new_text: &str,
    ) -> bool {
        let replacement = replacement_range_utf16
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        if self.replace_range(replacement, new_text).is_none() {
            return false;
        }
        self.marked_range = None;
        true
    }

    /// Applies an IME composition update. The optional selected range is
    /// relative to the newly inserted text and expressed in UTF-16 units.
    pub fn replace_and_mark_utf16(
        &mut self,
        replacement_range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selection_utf16: Option<Range<usize>>,
    ) -> bool {
        let replacement = replacement_range_utf16
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selection.clone());
        let Some(inserted_range) = self.replace_range(replacement, new_text) else {
            return false;
        };
        self.marked_range = (!inserted_range.is_empty()).then_some(inserted_range.clone());
        if let Some(selection) = new_selection_utf16 {
            let inserted_text = &self.text[inserted_range.clone()];
            let relative_start = offset_from_utf16(inserted_text, selection.start);
            let relative_end = offset_from_utf16(inserted_text, selection.end);
            self.selection =
                inserted_range.start + relative_start..inserted_range.start + relative_end;
        }
        true
    }

    /// Performs a transactional replacement and returns the inserted range.
    /// The cheap visible-text check happens before allocating normalized input;
    /// the exact Markdown check happens on local candidate state before commit.
    fn replace_range(&mut self, range: Range<usize>, replacement: &str) -> Option<Range<usize>> {
        let start = floor_char_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_char_boundary(&self.text, range.end.min(self.text.len())).max(start);
        let range = start..end;
        let removed_len = range.end - range.start;
        let retained_len = self.text.len().checked_sub(removed_len)?;
        let normalized_len = normalized_newline_len(replacement);
        if retained_len.checked_add(normalized_len)? > self.max_markdown_bytes {
            return None;
        }
        let replacement = normalize_newlines(replacement);
        debug_assert_eq!(replacement.len(), normalized_len);
        let insertion_style = self
            .pending_style
            .unwrap_or_else(|| self.style_for_insertion(range.start));

        let start_line = line_at_offset(&self.text, range.start);
        let end_line = line_at_offset(&self.text, range.end);
        let inherited_block = self.blocks[start_line];
        let replacement_lines = replacement.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let mut new_blocks = self.blocks.clone();
        new_blocks.splice(
            start_line..=end_line,
            std::iter::repeat_n(inherited_block, replacement_lines),
        );

        let inserted_len = replacement.len();
        let new_end = range.start + inserted_len;
        let mut new_runs = Vec::with_capacity(self.runs.len() + 2);
        for run in &self.runs {
            if run.range.end <= range.start {
                push_style_run(&mut new_runs, run.range.clone(), run.style);
            } else if run.range.start < range.start {
                push_style_run(&mut new_runs, run.range.start..range.start, run.style);
            }
        }
        if inserted_len > 0 {
            push_style_run(&mut new_runs, range.start..new_end, insertion_style);
        }
        for run in &self.runs {
            if run.range.start >= range.end {
                let shifted_start = run.range.start - removed_len + inserted_len;
                let shifted_end = run.range.end - removed_len + inserted_len;
                push_style_run(&mut new_runs, shifted_start..shifted_end, run.style);
            } else if run.range.end > range.end {
                push_style_run(
                    &mut new_runs,
                    new_end..new_end + (run.range.end - range.end),
                    run.style,
                );
            }
        }

        let mut new_text = String::with_capacity(retained_len + inserted_len);
        new_text.push_str(&self.text[..range.start]);
        new_text.push_str(&replacement);
        new_text.push_str(&self.text[range.end..]);
        normalize_runs(&new_text, &mut new_runs);
        if !markdown_fits_with_limit(&new_text, &new_runs, &new_blocks, self.max_markdown_bytes) {
            return None;
        }

        self.text = new_text;
        self.runs = new_runs;
        self.blocks = new_blocks;
        self.selection = new_end..new_end;
        self.selection_reversed = false;
        self.pending_style = Some(insertion_style);
        debug_assert_eq!(
            self.blocks.len(),
            self.text.bytes().filter(|b| *b == b'\n').count() + 1
        );
        Some(range.start..new_end)
    }

    fn toggle_inline(&mut self, flag: u8) -> bool {
        if self.selection.is_empty() {
            let current = self
                .pending_style
                .unwrap_or_else(|| self.style_for_insertion(self.cursor_offset()));
            self.pending_style = Some(current.with_flag(flag, !has_flag(current, flag)));
            return true;
        }

        let enabled = !self.inline_active(flag);
        let range = self.selection.clone();
        let mut updated = Vec::with_capacity(self.runs.len() + 2);
        for run in &self.runs {
            if run.range.end <= range.start || run.range.start >= range.end {
                push_style_run(&mut updated, run.range.clone(), run.style);
                continue;
            }
            if run.range.start < range.start {
                push_style_run(&mut updated, run.range.start..range.start, run.style);
            }
            let middle_start = run.range.start.max(range.start);
            let middle_end = run.range.end.min(range.end);
            push_style_run(
                &mut updated,
                middle_start..middle_end,
                run.style.with_flag(flag, enabled),
            );
            if run.range.end > range.end {
                push_style_run(&mut updated, range.end..run.range.end, run.style);
            }
        }
        normalize_runs(&self.text, &mut updated);
        if !markdown_fits_with_limit(&self.text, &updated, &self.blocks, self.max_markdown_bytes) {
            return false;
        }
        self.runs = updated;
        self.pending_style = None;
        true
    }

    fn toggle_block(&mut self, target: BlockKind) -> bool {
        let lines = self.selected_lines();
        let enabled = !self.blocks[lines.clone()]
            .iter()
            .all(|kind| *kind == target);
        let mut updated = self.blocks.clone();
        for kind in &mut updated[lines] {
            *kind = if enabled {
                target
            } else {
                BlockKind::Paragraph
            };
        }
        if !markdown_fits_with_limit(&self.text, &self.runs, &updated, self.max_markdown_bytes) {
            return false;
        }
        self.blocks = updated;
        true
    }

    /// Applies one block kind to every line touched by the selection.
    pub fn set_block(&mut self, target: BlockKind) -> bool {
        let lines = self.selected_lines();
        let mut updated = self.blocks.clone();
        for kind in &mut updated[lines] {
            *kind = target;
        }
        if !markdown_fits_with_limit(&self.text, &self.runs, &updated, self.max_markdown_bytes) {
            return false;
        }
        self.blocks = updated;
        true
    }

    fn inline_active(&self, flag: u8) -> bool {
        if self.selection.is_empty() {
            return has_flag(
                self.pending_style
                    .unwrap_or_else(|| self.style_for_insertion(self.cursor_offset())),
                flag,
            );
        }
        let mut saw_text = false;
        for run in &self.runs {
            if run.range.start < self.selection.end && run.range.end > self.selection.start {
                saw_text = true;
                if !has_flag(run.style, flag) {
                    return false;
                }
            }
        }
        saw_text
    }

    fn block_active(&self, target: BlockKind) -> bool {
        self.blocks[self.selected_lines()]
            .iter()
            .all(|kind| *kind == target)
    }

    fn selected_lines(&self) -> Range<usize> {
        let first = line_at_offset(&self.text, self.selection.start);
        let last_offset = if self.selection.end > self.selection.start {
            previous_char_boundary(&self.text, self.selection.end)
        } else {
            self.selection.end
        };
        let last = line_at_offset(&self.text, last_offset);
        first..last + 1
    }

    fn style_for_insertion(&self, offset: usize) -> InlineStyle {
        if self.text.is_empty() {
            return InlineStyle::default();
        }
        let probe = if offset == self.text.len() {
            previous_char_boundary(&self.text, offset)
        } else {
            offset
        };
        self.runs
            .iter()
            .find(|run| run.range.contains(&probe))
            .map(|run| run.style)
            .unwrap_or_default()
    }
}

impl Default for RichTextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<&str> for RichTextBuffer {
    type Error = MarkdownLimitExceeded;

    fn try_from(markdown: &str) -> Result<Self, Self::Error> {
        Self::try_from_markdown(markdown)
    }
}

fn parse_block_prefix(line: &str) -> (BlockKind, &str) {
    if let Some(rest) = line.strip_prefix("### ") {
        (BlockKind::Heading3, rest)
    } else if let Some(rest) = line.strip_prefix("## ") {
        (BlockKind::Heading2, rest)
    } else if let Some(rest) = line.strip_prefix("# ") {
        (BlockKind::Heading1, rest)
    } else if let Some(rest) = line.strip_prefix("- ") {
        (BlockKind::Bulleted, rest)
    } else if let Some(prefix_len) = looks_numbered_prefix(line) {
        (BlockKind::Numbered, &line[prefix_len..])
    } else if let Some(rest) = line.strip_prefix("> ") {
        (BlockKind::Quote, rest)
    } else {
        (BlockKind::Paragraph, line)
    }
}

fn looks_numbered_prefix(line: &str) -> Option<usize> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && line.get(digits..digits + 2) == Some(". ")).then_some(digits + 2)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InlineParseOperations {
    pub indexed_bytes: usize,
    pub parse_steps: usize,
    pub delimiter_queries: usize,
    pub delimiter_positions_advanced: usize,
}

struct BacktickRunIndex {
    positions: HashMap<usize, Vec<usize>>,
    cursors: HashMap<usize, usize>,
    indexed_bytes: usize,
    positions_advanced: usize,
}

impl BacktickRunIndex {
    fn new(source: &str) -> Self {
        let mut positions = HashMap::<usize, Vec<usize>>::new();
        let mut index = 0usize;
        while index < source.len() {
            if source.as_bytes()[index] == b'`' {
                let start = index;
                while index < source.len() && source.as_bytes()[index] == b'`' {
                    index += 1;
                }
                positions.entry(index - start).or_default().push(start);
            } else {
                index += source[index..].chars().next().unwrap().len_utf8();
            }
        }
        Self {
            positions,
            cursors: HashMap::new(),
            indexed_bytes: source.len(),
            positions_advanced: 0,
        }
    }

    fn next_run(&mut self, length: usize, minimum_start: usize) -> Option<usize> {
        let positions = self.positions.get(&length)?;
        let cursor = self.cursors.entry(length).or_default();
        while positions
            .get(*cursor)
            .is_some_and(|position| *position < minimum_start)
        {
            *cursor += 1;
            self.positions_advanced += 1;
        }
        positions.get(*cursor).copied()
    }
}

fn parse_inline(source: &str, output: &mut String, runs: &mut Vec<StyleRun>) {
    let _ = parse_inline_with_operations(source, output, runs);
}

#[doc(hidden)]
pub fn parse_inline_with_operations(
    source: &str,
    output: &mut String,
    runs: &mut Vec<StyleRun>,
) -> InlineParseOperations {
    let mut backticks = BacktickRunIndex::new(source);
    let mut index = 0;
    let mut parse_steps = 0usize;
    let mut delimiter_queries = 0usize;
    while index < source.len() {
        parse_steps += 1;
        if source.as_bytes()[index] == b'\\' {
            let next = index + 1;
            if next < source.len() {
                let end = next + source[next..].chars().next().unwrap().len_utf8();
                append_piece(&source[next..end], InlineStyle::default(), output, runs);
                index = end;
            } else {
                append_piece("\\", InlineStyle::default(), output, runs);
                index += 1;
            }
            continue;
        }

        if source.as_bytes()[index] == b'`' {
            let fence_len = source.as_bytes()[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            delimiter_queries += 1;
            if let Some(content_end) = backticks.next_run(fence_len, index + fence_len) {
                let content_start = index + fence_len;
                append_piece(
                    &source[content_start..content_end],
                    InlineStyle(InlineStyle::CODE),
                    output,
                    runs,
                );
                index = content_end + fence_len;
                continue;
            }
            append_piece(
                &source[index..index + fence_len],
                InlineStyle::default(),
                output,
                runs,
            );
            index += fence_len;
            continue;
        }

        let marker = if source[index..].starts_with("***") {
            Some(("***", InlineStyle(InlineStyle::BOLD | InlineStyle::ITALIC)))
        } else if source[index..].starts_with("**") {
            Some(("**", InlineStyle(InlineStyle::BOLD)))
        } else if source[index..].starts_with('*') {
            Some(("*", InlineStyle(InlineStyle::ITALIC)))
        } else {
            None
        };
        if let Some((marker, style)) = marker
            && let Some(end) = find_unescaped_marker(source, index + marker.len(), marker)
        {
            let decoded = unescape_inline(&source[index + marker.len()..end]);
            append_piece(&decoded, style, output, runs);
            index = end + marker.len();
            continue;
        }

        let end = index + source[index..].chars().next().unwrap().len_utf8();
        append_piece(&source[index..end], InlineStyle::default(), output, runs);
        index = end;
    }
    InlineParseOperations {
        indexed_bytes: backticks.indexed_bytes,
        parse_steps,
        delimiter_queries,
        delimiter_positions_advanced: backticks.positions_advanced,
    }
}

fn find_unescaped_marker(source: &str, mut index: usize, marker: &str) -> Option<usize> {
    while index < source.len() {
        if source.as_bytes()[index] == b'\\' {
            index += 1;
            if index < source.len() {
                index += source[index..].chars().next().unwrap().len_utf8();
            }
        } else if source[index..].starts_with(marker) {
            return Some(index);
        } else {
            index += source[index..].chars().next().unwrap().len_utf8();
        }
    }
    None
}

fn unescape_inline(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(next) = chars.next() {
                output.push(next);
            } else {
                output.push(character);
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn encode_inline(source: &str, style: InlineStyle, output: &mut String) {
    if source.is_empty() {
        return;
    }
    if style.code() {
        let fence_len = longest_backtick_run(source) + 1;
        let fence = "`".repeat(fence_len);
        output.push_str(&fence);
        output.push_str(source);
        output.push_str(&fence);
        return;
    }
    let marker = match (style.bold(), style.italic()) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        (false, false) => "",
    };
    output.push_str(marker);
    escape_inline(source, output);
    output.push_str(marker);
}

fn encoded_inline_len(source: &str, style: InlineStyle) -> Option<usize> {
    if source.is_empty() {
        return Some(0);
    }
    if style.code() {
        let fence_len = longest_backtick_run(source).checked_add(1)?;
        return source.len().checked_add(fence_len.checked_mul(2)?);
    }
    let marker_len = match (style.bold(), style.italic()) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    };
    let escaped = source
        .bytes()
        .filter(|byte| matches!(*byte, b'\\' | b'*' | b'`'))
        .count();
    source
        .len()
        .checked_add(escaped)?
        .checked_add(marker_len * 2)
}

/// Mirrors `RichTextBuffer::markdown` without allocating the serialized
/// comment. This makes the editor's transactional guard exactly match the
/// persistence contract, including list prefixes, escapes, and style markers.
#[doc(hidden)]
pub fn markdown_fits_with_run_operations(
    text: &str,
    runs: &[StyleRun],
    blocks: &[BlockKind],
) -> (bool, usize) {
    markdown_fits_with_limit_and_run_operations(text, runs, blocks, DEFAULT_MAX_MARKDOWN_BYTES)
}

fn markdown_fits_with_limit(
    text: &str,
    runs: &[StyleRun],
    blocks: &[BlockKind],
    max_markdown_bytes: usize,
) -> bool {
    markdown_fits_with_limit_and_run_operations(text, runs, blocks, max_markdown_bytes).0
}

fn markdown_fits_with_limit_and_run_operations(
    text: &str,
    runs: &[StyleRun],
    blocks: &[BlockKind],
    max_markdown_bytes: usize,
) -> (bool, usize) {
    let ranges = line_ranges(text);
    if ranges.len() != blocks.len() {
        return (false, 0);
    }
    let mut total = 0usize;
    let mut cursor = StyleRunCursor::new(runs);
    for (line_index, (start, end)) in ranges.into_iter().enumerate() {
        if line_index > 0 && !add_with_limit(&mut total, 1, max_markdown_bytes) {
            return (false, cursor.operations());
        }
        let prefix_len = match blocks[line_index] {
            BlockKind::Paragraph => 0,
            BlockKind::Heading1 => 2,
            BlockKind::Heading2 => 3,
            BlockKind::Heading3 => 4,
            BlockKind::Bulleted => 2,
            BlockKind::Numbered => 3,
            BlockKind::Quote => 2,
        };
        if !add_with_limit(&mut total, prefix_len, max_markdown_bytes) {
            return (false, cursor.operations());
        }
        let (first_style, first_run_end) = cursor.style_extent_at(start);
        if blocks[line_index] == BlockKind::Paragraph
            && paragraph_prefix_needs_escape(text, start, end, first_style, first_run_end)
            && !add_with_limit(&mut total, 1, max_markdown_bytes)
        {
            return (false, cursor.operations());
        }
        let mut fits = true;
        cursor.for_each_overlap(start, end, |overlap, style| {
            if !fits {
                return;
            }
            fits = encoded_inline_len(&text[overlap], style).is_some_and(|encoded_len| {
                add_with_limit(&mut total, encoded_len, max_markdown_bytes)
            });
        });
        if !fits {
            return (false, cursor.operations());
        }
    }
    (true, cursor.operations())
}

fn add_with_limit(total: &mut usize, additional: usize, max_markdown_bytes: usize) -> bool {
    let Some(updated) = total.checked_add(additional) else {
        return false;
    };
    if updated > max_markdown_bytes {
        return false;
    }
    *total = updated;
    true
}

fn paragraph_prefix_needs_escape(
    text: &str,
    start: usize,
    end: usize,
    first_style: InlineStyle,
    first_run_end: usize,
) -> bool {
    if start == end || first_style != InlineStyle::default() {
        return false;
    }
    let prefix = &text[start..first_run_end.min(end)];
    prefix.starts_with("- ") || looks_numbered_prefix(prefix).is_some()
}

fn escape_inline(source: &str, output: &mut String) {
    for character in source.chars() {
        if matches!(character, '\\' | '*' | '`') {
            output.push('\\');
        }
        output.push(character);
    }
}

fn longest_backtick_run(source: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for byte in source.bytes() {
        if byte == b'`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn append_piece(piece: &str, style: InlineStyle, output: &mut String, runs: &mut Vec<StyleRun>) {
    if piece.is_empty() {
        return;
    }
    let start = output.len();
    output.push_str(piece);
    push_style_run(runs, start..output.len(), style);
}

fn push_style_run(runs: &mut Vec<StyleRun>, range: Range<usize>, style: InlineStyle) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.range.end == range.start
        && last.style == style
    {
        last.range.end = range.end;
    } else {
        runs.push(StyleRun { range, style });
    }
}

fn normalize_runs(text: &str, runs: &mut Vec<StyleRun>) {
    let old = std::mem::take(runs);
    let mut cursor = 0;
    for run in old {
        let start = run.range.start.max(cursor).min(text.len());
        let end = run.range.end.max(start).min(text.len());
        if cursor < start {
            push_style_run(runs, cursor..start, InlineStyle::default());
        }
        push_style_run(runs, start..end, run.style);
        cursor = end;
    }
    if cursor < text.len() {
        push_style_run(runs, cursor..text.len(), InlineStyle::default());
    }
}

fn has_flag(style: InlineStyle, flag: u8) -> bool {
    style.0 & flag != 0
}

/// Returns the UTF-8 byte range of every logical line, including a trailing
/// empty line when the text ends in a newline.
pub fn line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            result.push((start, index));
            start = index + 1;
        }
    }
    result.push((start, text.len()));
    result
}

/// Returns the zero-based logical line containing a UTF-8 byte offset.
pub fn line_at_offset(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn normalized_newline_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut source = 0usize;
    let mut normalized = 0usize;
    while source < bytes.len() {
        if bytes[source] == b'\r' && bytes.get(source + 1) == Some(&b'\n') {
            source += 2;
        } else {
            source += 1;
        }
        normalized += 1;
    }
    normalized
}

fn offset_to_utf16(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset.min(text.len()));
    text[..offset].encode_utf16().count()
}

fn offset_from_utf16(text: &str, target: usize) -> usize {
    let mut utf16 = 0;
    for (offset, character) in text.char_indices() {
        if utf16 >= target {
            return offset;
        }
        let next = utf16 + character.len_utf16();
        if target < next {
            return offset;
        }
        utf16 = next;
    }
    text.len()
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn previous_char_boundary(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text[..offset]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

/// Finds the previous user-perceived character boundary using the compact
/// grapheme rules required by the native editor.
pub fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    if offset == 0 {
        return 0;
    }
    let mut previous = 0;
    let mut current = 0;
    while current < offset {
        previous = current;
        current = next_grapheme_boundary(text, current);
        if current >= offset {
            return previous;
        }
    }
    previous
}

/// Finds the next user-perceived character boundary using the compact
/// grapheme rules required by the native editor.
pub fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    let start = floor_char_boundary(text, offset);
    if start >= text.len() {
        return text.len();
    }
    let first = text[start..].chars().next().unwrap();
    let mut end = start + first.len_utf8();
    if first == '\r' && text[end..].starts_with('\n') {
        return end + 1;
    }
    if is_regional_indicator(first)
        && let Some(next) = text[end..].chars().next()
        && is_regional_indicator(next)
    {
        end += next.len_utf8();
    }
    loop {
        while let Some(character) = text[end..].chars().next() {
            if is_grapheme_extend(character) {
                end += character.len_utf8();
            } else {
                break;
            }
        }
        if !text[end..].starts_with('\u{200d}') {
            break;
        }
        end += '\u{200d}'.len_utf8();
        if let Some(character) = text[end..].chars().next() {
            end += character.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn is_grapheme_extend(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0x1f3fb..=0x1f3ff
            | 0xe0100..=0xe01ef
    )
}

fn is_regional_indicator(character: char) -> bool {
    matches!(character as u32, 0x1f1e6..=0x1f1ff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_subset_parses_and_serializes_canonically() {
        let input = "- **bold** *italic* ***both*** `code`\n7. second\nplain";
        let buffer = RichTextBuffer::from_markdown(input);
        assert_eq!(
            buffer.markdown(),
            "- **bold** *italic* ***both*** `code`\n1. second\nplain"
        );
        assert_eq!(
            RichTextBuffer::from_markdown(&buffer.markdown()).markdown(),
            buffer.markdown()
        );
    }

    #[test]
    fn canonical_expansion_is_included_in_the_input_limit() {
        let raw = format!("*{}", "x".repeat(DEFAULT_MAX_MARKDOWN_BYTES - 1));
        assert_eq!(raw.len(), DEFAULT_MAX_MARKDOWN_BYTES);

        let parsed = RichTextBuffer::parse_markdown(&raw, DEFAULT_MAX_MARKDOWN_BYTES);
        assert_eq!(parsed.markdown().len(), DEFAULT_MAX_MARKDOWN_BYTES + 1);
        assert_eq!(
            RichTextBuffer::try_from_markdown(&raw),
            Err(MarkdownLimitExceeded)
        );
    }

    #[test]
    fn caller_limit_is_preserved_transactionally() {
        assert_eq!(
            RichTextBuffer::try_from_markdown_with_limit("*", 1),
            Err(MarkdownLimitExceeded)
        );
        let mut buffer = RichTextBuffer::try_from_markdown_with_limit("hello", 5).unwrap();
        let before = buffer.clone();
        assert!(!buffer.replace_selection("!"));
        assert_eq!(buffer, before);
        assert_eq!(buffer.max_markdown_bytes(), 5);
    }

    #[test]
    fn markdown_escapes_literals_and_ambiguous_paragraph_prefixes() {
        let buffer = RichTextBuffer::from_markdown(
            "\\- paragraph, not a list\n\\12. still a paragraph\nslashes \\\\ and \\*stars\\* and \\`ticks\\`",
        );
        assert_eq!(
            buffer.text(),
            "- paragraph, not a list\n12. still a paragraph\nslashes \\ and *stars* and `ticks`"
        );
        assert_eq!(
            RichTextBuffer::from_markdown(&buffer.markdown()).text(),
            buffer.text()
        );
        assert_eq!(buffer.block_kinds(), &[BlockKind::Paragraph; 3]);
    }

    #[test]
    fn code_fence_grows_to_preserve_embedded_backticks() {
        let buffer = RichTextBuffer::from_markdown("``a`b``");
        assert_eq!(buffer.text(), "a`b");
        assert_eq!(buffer.markdown(), "``a`b``");
        assert_eq!(RichTextBuffer::from_markdown(&buffer.markdown()), buffer);
    }

    #[test]
    fn unmatched_backtick_parser_work_is_linear() {
        let run_count = 512usize;
        let mut source = String::new();
        for length in (1..=run_count).rev() {
            source.push_str(&"`".repeat(length));
            source.push('x');
        }
        let mut text = String::new();
        let mut runs = Vec::new();
        let operations = parse_inline_with_operations(&source, &mut text, &mut runs);

        assert_eq!(text, source);
        assert_eq!(operations.indexed_bytes, source.len());
        assert_eq!(operations.delimiter_queries, run_count);
        assert!(operations.delimiter_positions_advanced <= run_count);
        assert!(
            operations
                .parse_steps
                .saturating_add(operations.delimiter_positions_advanced)
                <= source.len() + run_count
        );
    }

    #[test]
    fn selection_formatting_splits_and_rejoins_runs() {
        let mut buffer = RichTextBuffer::from_markdown("hello");
        buffer.set_selection(1..4, false);
        assert!(buffer.toggle_bold());
        assert_eq!(buffer.markdown(), "h**ell**o");
        assert_eq!(buffer.style_runs().len(), 3);
        assert!(buffer.toggle_bold());
        assert_eq!(buffer.markdown(), "hello");
        assert_eq!(buffer.style_runs().len(), 1);
    }

    #[test]
    fn caret_formatting_is_inherited_and_code_is_exclusive() {
        let mut buffer = RichTextBuffer::from_markdown("ab");
        buffer.move_to(1);
        assert!(buffer.toggle_italic());
        assert!(buffer.replace_selection("λ"));
        assert_eq!(buffer.markdown(), "a*λ*b");

        buffer.select_all();
        assert!(buffer.toggle_bold());
        assert!(buffer.toggle_code());
        assert!(buffer.code_active());
        assert!(!buffer.bold_active());
        assert!(!buffer.italic_active());
    }

    #[test]
    fn replacement_across_runs_preserves_outer_fragments() {
        let mut buffer = RichTextBuffer::from_markdown("**ab**cd*ef*");
        buffer.set_selection(1..5, false);
        assert!(buffer.replace_selection("X"));
        assert_eq!(buffer.text(), "aXf");
        assert!(buffer.style_runs().first().unwrap().style().bold());
        assert!(buffer.style_runs().last().unwrap().style().italic());
    }

    #[test]
    fn block_operations_cover_touched_lines_only() {
        let mut buffer = RichTextBuffer::from_markdown("one\ntwo\nthree");
        buffer.set_selection(0..4, false); // Includes `one\n`, not `two`.
        assert!(buffer.toggle_bulleted_list());
        assert_eq!(buffer.markdown(), "- one\ntwo\nthree");

        buffer.set_selection(4..7, false);
        assert!(buffer.set_block(BlockKind::Quote));
        assert_eq!(buffer.markdown(), "- one\n> two\nthree");

        buffer.select_all();
        assert!(buffer.toggle_numbered_list());
        assert_eq!(buffer.markdown(), "1. one\n1. two\n1. three");
    }

    #[test]
    fn newline_continues_lists_exits_empty_items_and_ends_headings() {
        let mut list = RichTextBuffer::from_markdown("- first");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n- ");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n");
        assert_eq!(
            list.block_kinds(),
            &[BlockKind::Bulleted, BlockKind::Paragraph]
        );

        let mut heading = RichTextBuffer::from_markdown("# Title");
        assert!(heading.insert_newline());
        assert_eq!(heading.markdown(), "# Title\n");
        assert_eq!(
            heading.block_kinds(),
            &[BlockKind::Heading1, BlockKind::Paragraph]
        );
    }

    #[test]
    fn utf16_conversion_and_ime_composition_are_unicode_safe() {
        let buffer = RichTextBuffer::from_markdown("A😀e\u{301}B");
        assert_eq!(buffer.offset_to_utf16(1), 1);
        assert_eq!(buffer.offset_to_utf16(5), 3);
        assert_eq!(buffer.offset_from_utf16(2), 1);
        assert_eq!(buffer.range_from_utf16(1..3), 1..5);

        let mut buffer = RichTextBuffer::from_markdown("A😀B");
        buffer.set_selection(1..5, false);
        assert!(buffer.replace_and_mark_utf16(None, "漢字", Some(1..2)));
        assert_eq!(buffer.text(), "A漢字B");
        assert_eq!(buffer.marked_range(), Some(1..7));
        assert_eq!(buffer.selection(), 4..7);
        buffer.clear_marked_range();
        assert_eq!(buffer.marked_range(), None);
    }

    #[test]
    fn exact_multibyte_limit_is_accepted_and_overflow_is_atomic() {
        let exact = "é".repeat(DEFAULT_MAX_MARKDOWN_BYTES / "é".len());
        let mut buffer = RichTextBuffer::new();
        assert!(buffer.replace_selection(&exact));
        assert_eq!(buffer.markdown().len(), DEFAULT_MAX_MARKDOWN_BYTES);

        buffer.move_to("é".len());
        assert!(buffer.toggle_bold());
        let before = buffer.clone();
        assert!(!buffer.replace_selection("é"));
        assert_eq!(buffer, before);
    }

    #[test]
    fn rejected_native_replacement_preserves_reverse_selection() {
        let exact = "é".repeat(DEFAULT_MAX_MARKDOWN_BYTES / "é".len());
        let mut buffer = RichTextBuffer::from_markdown(&exact);
        buffer.set_selection(2..6, true);
        let before = buffer.clone();

        assert!(!buffer.replace_text_utf16(Some(0..0), "漢"));
        assert_eq!(buffer, before);
    }

    #[test]
    fn formatting_and_escaping_overhead_obey_the_same_limit() {
        let escaped_exact = "*".repeat(DEFAULT_MAX_MARKDOWN_BYTES / 2);
        let mut escaped = RichTextBuffer::new();
        assert!(escaped.replace_selection(&escaped_exact));
        assert_eq!(escaped.markdown().len(), DEFAULT_MAX_MARKDOWN_BYTES);
        let before_paste = escaped.clone();
        assert!(!escaped.replace_selection("*"));
        assert_eq!(escaped, before_paste);

        let mut formatted = RichTextBuffer::from_markdown(&"x".repeat(DEFAULT_MAX_MARKDOWN_BYTES));
        formatted.select_all();
        let before_formatting = formatted.clone();
        assert!(!formatted.toggle_bold());
        assert_eq!(formatted, before_formatting);
    }

    #[test]
    fn backspace_removes_combining_zwj_and_flag_clusters_together() {
        for cluster in ["e\u{301}", "👩\u{200d}💻", "🇬🇧"] {
            let mut buffer = RichTextBuffer::from_markdown(cluster);
            assert!(buffer.backspace());
            assert_eq!(buffer.text(), "");
        }
    }

    #[test]
    fn reverse_selection_keeps_a_stable_anchor() {
        let mut buffer = RichTextBuffer::from_markdown("abcd");
        buffer.move_to(3);
        buffer.select_to(1);
        assert_eq!(buffer.selection(), 1..3);
        assert!(buffer.selection_is_reversed());
        assert_eq!(buffer.cursor_offset(), 1);
        buffer.select_to(4);
        assert_eq!(buffer.selection(), 3..4);
        assert!(!buffer.selection_is_reversed());
    }

    #[test]
    fn serialization_traverses_many_lines_and_runs_linearly() {
        let line_count = 640usize;
        let markdown = (0..line_count)
            .map(|index| format!("**b{index}** *i{index}* plain"))
            .collect::<Vec<_>>()
            .join("\n");
        let buffer = RichTextBuffer::from_markdown(&markdown);
        let structural_size = buffer.block_kinds().len() + buffer.style_runs().len();

        let (serialized, markdown_operations) = buffer.markdown_with_run_operations();
        let (fits, fit_operations) = markdown_fits_with_run_operations(
            buffer.text(),
            buffer.style_runs(),
            buffer.block_kinds(),
        );
        assert_eq!(serialized, markdown);
        assert!(fits);
        assert!(markdown_operations <= structural_size * 3);
        assert!(fit_operations <= structural_size * 3);
    }

    #[test]
    fn newline_normalization_preserves_empty_lines_and_block_count() {
        let buffer = RichTextBuffer::from_markdown("a\r\n- b\rc");
        assert_eq!(buffer.text(), "a\nb\nc");
        assert_eq!(
            buffer.block_kinds(),
            &[
                BlockKind::Paragraph,
                BlockKind::Bulleted,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(buffer.markdown(), "a\n- b\nc");

        for markdown in ["", "\n", "a\n", "\n\n"] {
            let buffer = RichTextBuffer::from_markdown(markdown);
            assert_eq!(buffer.markdown(), markdown);
            assert_eq!(
                buffer.block_kinds().len(),
                buffer.text().bytes().filter(|byte| *byte == b'\n').count() + 1
            );
        }
    }

    #[test]
    fn public_projection_views_are_normalized_and_read_only() {
        let buffer = RichTextBuffer::try_from("**a**\n- b").unwrap();
        assert_eq!(
            buffer.block_kinds(),
            &[BlockKind::Paragraph, BlockKind::Bulleted]
        );
        assert_eq!(line_ranges(buffer.text()), vec![(0, 1), (2, 3)]);
        assert_eq!(line_at_offset(buffer.text(), 2), 1);
        assert_eq!(buffer.style_runs().first().unwrap().range(), 0..1);
        assert!(buffer.style_runs().first().unwrap().style().bold());

        let mut cursor = StyleRunCursor::new(buffer.style_runs());
        assert!(cursor.style_at(0).bold());
        assert!(cursor.style_at(1).is_plain());
    }
}
