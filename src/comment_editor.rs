//! A small, dependency-free rich comment editor for GPUI.
//!
//! The editor keeps rich text as plain UTF-8 plus semantic inline and block
//! styles. Markdown is only a deterministic storage format; Markdown markers
//! are never exposed to the person editing the comment.

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString, StyledText,
    TextLayout, TextRun, UTF16Selection, UnderlineStyle, Window, actions, canvas, div, font, point,
    prelude::*, px, quad, size,
};
use gpui_component::{Icon, IconName, Theme};
use std::{collections::HashMap, fmt, ops::Range};

use crate::annotations::MAX_COMMENT_BYTES;
use crate::theme::ReaderPalette;
use crate::{EditCopy, EditCut, EditPaste, EditSelectAll};

const COMMENT_TOO_LONG_MESSAGE: &str = "Comment is too long (1 MiB Markdown limit).";

/// The stored Markdown is within the sidecar's raw byte limit, but parsing it
/// would produce a canonical representation that is too large to save again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommentMarkdownTooLong;

impl fmt::Display for CommentMarkdownTooLong {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "this comment cannot be edited because its canonical Markdown exceeds the 1 MiB limit",
        )
    }
}

impl std::error::Error for CommentMarkdownTooLong {}

actions!(
    comment_editor,
    [
        CommentBackspace,
        CommentDelete,
        CommentLeft,
        CommentRight,
        CommentUp,
        CommentDown,
        CommentSelectLeft,
        CommentSelectRight,
        CommentSelectUp,
        CommentSelectDown,
        CommentSelectAll,
        CommentHome,
        CommentEnd,
        CommentCopy,
        CommentCut,
        CommentPaste,
        CommentToggleBold,
        CommentToggleItalic,
        CommentToggleCode,
        CommentToggleBulletedList,
        CommentToggleNumberedList,
        CommentSave,
        CommentCancel,
    ]
);

/// Events emitted by [`CommentEditor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommentEditorEvent {
    Changed,
    Save(String),
    Cancel,
}

/// The block style of one logical line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Bulleted,
    Numbered,
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
struct StyleRun {
    range: Range<usize>,
    style: InlineStyle,
}

/// Monotonic access to normalized style runs. A run that crosses a line
/// boundary remains current and is visited once on each crossed line; every
/// other run is advanced past exactly once.
struct StyleRunCursor<'a> {
    runs: &'a [StyleRun],
    index: usize,
    operations: usize,
}

impl<'a> StyleRunCursor<'a> {
    fn new(runs: &'a [StyleRun]) -> Self {
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

    fn style_at(&mut self, offset: usize) -> InlineStyle {
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

    fn for_each_overlap(
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

    fn operations(&self) -> usize {
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
}

impl RichTextBuffer {
    /// Parses persisted Markdown while enforcing the canonical persistence
    /// limit. Production callers must use this path: raw input can fit the
    /// sidecar limit while canonical escaping makes it larger.
    pub fn try_from_markdown(markdown: &str) -> Result<Self, CommentMarkdownTooLong> {
        if markdown.len() > MAX_COMMENT_BYTES {
            return Err(CommentMarkdownTooLong);
        }
        let buffer = Self::parse_markdown(markdown);
        if markdown_fits(&buffer.text, &buffer.runs, &buffer.blocks) {
            Ok(buffer)
        } else {
            Err(CommentMarkdownTooLong)
        }
    }

    /// Convenience parser for Markdown whose canonical size is already a
    /// trusted invariant. It panics rather than silently truncating invalid
    /// input; production data-loading paths use [`Self::try_from_markdown`].
    #[cfg(test)]
    pub fn from_trusted_markdown(markdown: &str) -> Self {
        Self::try_from_markdown(markdown)
            .expect("trusted comment Markdown must fit the canonical 1 MiB limit")
    }

    #[cfg(test)]
    fn from_markdown(markdown: &str) -> Self {
        Self::from_trusted_markdown(markdown)
    }

    fn parse_markdown(markdown: &str) -> Self {
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
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn markdown(&self) -> String {
        self.markdown_with_run_operations().0
    }

    fn markdown_with_run_operations(&self) -> (String, usize) {
        let mut output = String::new();
        let mut cursor = StyleRunCursor::new(&self.runs);
        for (line_index, (start, end)) in line_ranges(&self.text).into_iter().enumerate() {
            if line_index > 0 {
                output.push('\n');
            }
            match self.blocks[line_index] {
                BlockKind::Paragraph => {}
                BlockKind::Bulleted => output.push_str("- "),
                BlockKind::Numbered => output.push_str("1. "),
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

    #[cfg(test)]
    fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
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
        if retained_len.checked_add(normalized_len)? > MAX_COMMENT_BYTES {
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
        if !markdown_fits(&new_text, &new_runs, &new_blocks) {
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
        if !markdown_fits(&self.text, &updated, &self.blocks) {
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
        if !markdown_fits(&self.text, &self.runs, &updated) {
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

/// GPUI entity that renders and edits [`RichTextBuffer`] without showing
/// Markdown markers.
pub struct CommentEditor {
    focus_handle: FocusHandle,
    buffer: RichTextBuffer,
    layout: TextLayout,
    projection: DisplayProjection,
    input_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    validation_message: Option<SharedString>,
}

impl CommentEditor {
    /// Builds an editor from a buffer produced by the fallible persisted-data
    /// parser. Buffer mutations preserve the same canonical size invariant.
    pub fn new(cx: &mut Context<Self>, buffer: RichTextBuffer) -> Self {
        debug_assert!(markdown_fits(&buffer.text, &buffer.runs, &buffer.blocks));
        let projection = DisplayProjection::new(&buffer);
        Self {
            focus_handle: cx.focus_handle(),
            buffer,
            layout: TextLayout::default(),
            projection,
            input_bounds: None,
            selecting: false,
            validation_message: None,
        }
    }

    pub fn markdown(&self) -> String {
        self.buffer.markdown()
    }

    pub fn is_blank(&self) -> bool {
        self.buffer.text().trim().is_empty()
    }

    /// Emits a save only while the canonical Markdown persistence invariant
    /// holds. Both the keyboard action and the reader's Save button use this
    /// single guarded path.
    pub fn request_save(&mut self, cx: &mut Context<Self>) -> bool {
        if !markdown_fits(&self.buffer.text, &self.buffer.runs, &self.buffer.blocks) {
            self.validation_message = Some(COMMENT_TOO_LONG_MESSAGE.into());
            cx.notify();
            return false;
        }
        cx.emit(CommentEditorEvent::Save(self.markdown()));
        true
    }

    #[cfg(debug_assertions)]
    pub fn qa_has_painted(&self) -> bool {
        self.input_bounds.is_some()
    }

    fn emit_changed(&self, cx: &mut Context<Self>) {
        cx.emit(CommentEditorEvent::Changed);
        cx.notify();
    }

    fn finish_mutation(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if accepted {
            self.validation_message = None;
            self.emit_changed(cx);
        } else {
            self.validation_message = Some(COMMENT_TOO_LONG_MESSAGE.into());
            cx.notify();
        }
    }

    fn backspace(&mut self, _: &CommentBackspace, _: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.backspace();
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn delete(&mut self, _: &CommentDelete, _: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.delete_forward();
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn move_left(&mut self, _: &CommentLeft, _: &mut Window, cx: &mut Context<Self>) {
        let destination = if self.buffer.selection.is_empty() {
            previous_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset())
        } else {
            self.buffer.selection.start
        };
        self.buffer.move_to(destination);
        cx.notify();
        cx.stop_propagation();
    }

    fn move_right(&mut self, _: &CommentRight, _: &mut Window, cx: &mut Context<Self>) {
        let destination = if self.buffer.selection.is_empty() {
            next_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset())
        } else {
            self.buffer.selection.end
        };
        self.buffer.move_to(destination);
        cx.notify();
        cx.stop_propagation();
    }

    fn select_left(&mut self, _: &CommentSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let destination =
            previous_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        cx.notify();
        cx.stop_propagation();
    }

    fn select_right(&mut self, _: &CommentSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let destination = next_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        cx.notify();
        cx.stop_propagation();
    }

    fn move_up(&mut self, _: &CommentUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(destination) = self.vertical_destination(-1.0) {
            self.buffer.move_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn move_down(&mut self, _: &CommentDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(destination) = self.vertical_destination(1.0) {
            self.buffer.move_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn select_up(&mut self, _: &CommentSelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(destination) = self.vertical_destination(-1.0) {
            self.buffer.select_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn select_down(&mut self, _: &CommentSelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(destination) = self.vertical_destination(1.0) {
            self.buffer.select_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn vertical_destination(&self, direction: f32) -> Option<usize> {
        // The layout is populated during the first paint. A programmatic key
        // dispatch before that frame should be a no-op, not a TextLayout panic.
        self.input_bounds?;
        let display_offset = self
            .projection
            .display_for_model(self.buffer.cursor_offset());
        let position = self.layout.position_for_index(display_offset)?;
        let target = point(
            position.x,
            position.y + self.layout.line_height() * direction,
        );
        let display = self
            .layout
            .index_for_position(target)
            .unwrap_or_else(|closest| closest);
        Some(self.projection.model_for_display(display))
    }

    fn select_all(&mut self, _: &CommentSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
        cx.stop_propagation();
    }

    fn edit_select_all(&mut self, _: &EditSelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(&CommentSelectAll, window, cx);
    }

    fn home(&mut self, _: &CommentHome, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.cursor_offset();
        let start = self.buffer.text()[..cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.buffer.move_to(start);
        cx.notify();
        cx.stop_propagation();
    }

    fn end(&mut self, _: &CommentEnd, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.cursor_offset();
        let end = self.buffer.text()[cursor..]
            .find('\n')
            .map_or(self.buffer.text().len(), |index| cursor + index);
        self.buffer.move_to(end);
        cx.notify();
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &CommentCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text()[self.buffer.selection.clone()].to_owned(),
            ));
        }
        cx.stop_propagation();
    }

    fn edit_copy(&mut self, _: &EditCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&CommentCopy, window, cx);
    }

    fn cut(&mut self, _: &CommentCut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text()[self.buffer.selection.clone()].to_owned(),
            ));
            let accepted = self.buffer.replace_selection("");
            self.finish_mutation(accepted, cx);
        }
        cx.stop_propagation();
    }

    fn edit_cut(&mut self, _: &EditCut, window: &mut Window, cx: &mut Context<Self>) {
        self.cut(&CommentCut, window, cx);
    }

    fn paste(&mut self, _: &CommentPaste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let accepted = self.buffer.replace_selection(&text);
            self.finish_mutation(accepted, cx);
        }
        cx.stop_propagation();
    }

    fn edit_paste(&mut self, _: &EditPaste, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(&CommentPaste, window, cx);
    }

    fn toggle_bold(&mut self, _: &CommentToggleBold, window: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.toggle_bold();
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn toggle_italic(
        &mut self,
        _: &CommentToggleItalic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted = self.buffer.toggle_italic();
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn toggle_code(&mut self, _: &CommentToggleCode, window: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.toggle_code();
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn toggle_bulleted_list(
        &mut self,
        _: &CommentToggleBulletedList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted = self.buffer.toggle_bulleted_list();
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn toggle_numbered_list(
        &mut self,
        _: &CommentToggleNumberedList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted = self.buffer.toggle_numbered_list();
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn save(&mut self, _: &CommentSave, _: &mut Window, cx: &mut Context<Self>) {
        self.request_save(cx);
        cx.stop_propagation();
    }

    fn cancel(&mut self, _: &CommentCancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(CommentEditorEvent::Cancel);
        cx.stop_propagation();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let offset = self.model_index_for_position(event.position);
        if event.modifiers.shift {
            self.buffer.select_to(offset);
        } else {
            self.buffer.move_to(offset);
        }
        self.selecting = true;
        cx.notify();
        cx.stop_propagation();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.buffer
                .select_to(self.model_index_for_position(event.position));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        cx.notify();
    }

    fn model_index_for_position(&self, position: Point<Pixels>) -> usize {
        let display = self
            .layout
            .index_for_position(position)
            .unwrap_or_else(|closest| closest);
        self.projection.model_for_display(display)
    }

    fn format_button(
        palette: ReaderPalette,
        id: &'static str,
        label: impl IntoElement,
        active: bool,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(30.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded_md()
            .bg(if active {
                palette.accent_soft
            } else {
                palette.surface
            })
            .text_color(if active {
                palette.accent
            } else {
                palette.text_secondary
            })
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(move |style| style.bg(palette.control_hover))
            .active(move |style| style.bg(palette.control_pressed))
            .on_click(handler)
            .child(label)
    }
}

impl EventEmitter<CommentEditorEvent> for CommentEditor {}

impl Focusable for CommentEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for CommentEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(range.clone()));
        Some(self.buffer.text()[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(self.buffer.selection.clone()),
            reversed: self.buffer.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked_range
            .clone()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted = self.buffer.replace_text_utf16(range_utf16, text);
        self.finish_mutation(accepted, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted =
            self.buffer
                .replace_and_mark_utf16(range_utf16, new_text, new_selected_range_utf16);
        self.finish_mutation(accepted, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let model = self.buffer.offset_from_utf16(range_utf16.end);
        let display = self.projection.display_for_model(model);
        let position = self
            .layout
            .position_for_index(display)
            .unwrap_or(element_bounds.origin);
        Some(Bounds::new(
            position,
            size(px(1.0), self.layout.line_height()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(
            self.buffer
                .offset_to_utf16(self.model_index_for_position(position)),
        )
    }
}

impl Render for CommentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let focused = self.focus_handle.is_focused(window);
        self.projection = DisplayProjection::new(&self.buffer);
        let runs = display_text_runs(&self.buffer, &self.projection, palette);
        let styled = StyledText::new(self.projection.text.clone()).with_runs(runs);
        self.layout = styled.layout().clone();

        let editor = cx.entity();
        let focus = self.focus_handle.clone();
        let caret = palette.accent;
        let overlay = canvas(
            |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, editor.clone()), cx);
                {
                    let state = editor.read(cx);
                    if focus.is_focused(window) {
                        let display = state
                            .projection
                            .display_for_model(state.buffer.cursor_offset());
                        let position = state
                            .layout
                            .position_for_index(display)
                            .unwrap_or(bounds.origin + point(px(1.0), px(1.0)));
                        window.paint_quad(quad(
                            Bounds::new(position, size(px(1.5), state.layout.line_height())),
                            px(0.0),
                            caret,
                            px(0.0),
                            gpui::transparent_black(),
                            Default::default(),
                        ));
                    }
                }
                editor.update(cx, |editor, _| editor.input_bounds = Some(bounds));
            },
        )
        .absolute()
        .size_full();

        let validation_message = self.validation_message.clone().map(|message| {
            div()
                .id("comment-validation-message")
                .mt_1()
                .text_xs()
                .text_color(palette.error)
                .child(message)
        });
        let text_area = div()
            .id("comment-editor-text")
            .relative()
            .mt(px(44.0))
            .flex_1()
            .min_h(px(116.0))
            .w_full()
            .overflow_y_scroll()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(if self.validation_message.is_some() {
                palette.error
            } else if focused {
                palette.accent
            } else {
                palette.separator
            })
            .bg(palette.surface)
            .text_color(palette.text)
            .text_sm()
            .line_height(px(22.0))
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when(self.buffer.text().is_empty(), |element| {
                element.child(
                    div()
                        .absolute()
                        .text_color(palette.text_tertiary)
                        .child("Write a comment…"),
                )
            })
            .child(styled)
            .child(overlay);

        let toolbar = div()
            .id("comment-format-pill")
            .absolute()
            .top_2()
            .left_2()
            .right_2()
            .flex()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .overflow_hidden()
                    .rounded_xl()
                    .border_1()
                    .border_color(palette.separator)
                    .bg(palette.surface)
                    .shadow_sm()
                    .child(Self::format_button(
                        palette,
                        "comment-bold",
                        "B",
                        self.buffer.bold_active(),
                        cx.listener(|editor, _, window, cx| {
                            editor.toggle_bold(&CommentToggleBold, window, cx)
                        }),
                    ))
                    .child(Self::format_button(
                        palette,
                        "comment-italic",
                        "I",
                        self.buffer.italic_active(),
                        cx.listener(|editor, _, window, cx| {
                            editor.toggle_italic(&CommentToggleItalic, window, cx)
                        }),
                    ))
                    .child(Self::format_button(
                        palette,
                        "comment-code",
                        Icon::new(IconName::SquareTerminal),
                        self.buffer.code_active(),
                        cx.listener(|editor, _, window, cx| {
                            editor.toggle_code(&CommentToggleCode, window, cx)
                        }),
                    ))
                    .child(div().mx_1().h(px(20.0)).w(px(1.0)).bg(palette.separator))
                    .child(Self::format_button(
                        palette,
                        "comment-bullets",
                        Icon::new(IconName::Menu),
                        self.buffer.bulleted_list_active(),
                        cx.listener(|editor, _, window, cx| {
                            editor.toggle_bulleted_list(&CommentToggleBulletedList, window, cx)
                        }),
                    ))
                    .child(Self::format_button(
                        palette,
                        "comment-numbers",
                        "1.",
                        self.buffer.numbered_list_active(),
                        cx.listener(|editor, _, window, cx| {
                            editor.toggle_numbered_list(&CommentToggleNumberedList, window, cx)
                        }),
                    )),
            );

        div()
            .id("comment-editor")
            .key_context("CommentEditor")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::edit_select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::edit_copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::edit_cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::edit_paste))
            .on_action(cx.listener(Self::toggle_bold))
            .on_action(cx.listener(Self::toggle_italic))
            .on_action(cx.listener(Self::toggle_code))
            .on_action(cx.listener(Self::toggle_bulleted_list))
            .on_action(cx.listener(Self::toggle_numbered_list))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::cancel))
            .child(text_area)
            .children(validation_message)
            .child(toolbar)
    }
}

#[derive(Clone, Debug)]
struct DisplayProjection {
    text: SharedString,
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
    fn new(buffer: &RichTextBuffer) -> Self {
        Self::new_with_run_operations(buffer).0
    }

    fn new_with_run_operations(buffer: &RichTextBuffer) -> (Self, usize) {
        let ranges = line_ranges(&buffer.text);
        let mut text = String::new();
        let mut lines = Vec::with_capacity(ranges.len());
        let mut spans = Vec::new();
        let mut numbered_index = 0usize;
        let mut cursor = StyleRunCursor::new(&buffer.runs);

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
            let prefix = match buffer.blocks[line_index] {
                BlockKind::Paragraph => {
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
            text.push_str(&buffer.text[model_start..model_end]);
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

    fn display_for_model(&self, model: usize) -> usize {
        let model = model.min(self.lines.last().map_or(0, |line| line.model_end));
        for line in &self.lines {
            if model <= line.model_end {
                return line.display_content_start + model.saturating_sub(line.model_start);
            }
        }
        self.text.len()
    }

    fn model_for_display(&self, display: usize) -> usize {
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

fn display_text_runs(
    buffer: &RichTextBuffer,
    projection: &DisplayProjection,
    palette: ReaderPalette,
) -> Vec<TextRun> {
    let mut result = Vec::new();
    for span in &projection.spans {
        if let Some(model) = &span.model {
            let mut boundaries = vec![model.start, model.end];
            for boundary in [
                buffer.selection.start,
                buffer.selection.end,
                buffer
                    .marked_range
                    .as_ref()
                    .map_or(usize::MAX, |range| range.start),
                buffer
                    .marked_range
                    .as_ref()
                    .map_or(usize::MAX, |range| range.end),
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
                let selected = !buffer.selection.is_empty()
                    && start < buffer.selection.end
                    && end > buffer.selection.start;
                let marked = buffer
                    .marked_range
                    .as_ref()
                    .is_some_and(|range| start < range.end && end > range.start);
                result.push(make_text_run(
                    end - start,
                    span.style,
                    selected,
                    marked,
                    palette,
                ));
            }
        } else {
            result.push(make_text_run(
                span.display.end - span.display.start,
                span.style,
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
    selected: bool,
    marked: bool,
    palette: ReaderPalette,
) -> TextRun {
    let mut selected_font = if style.code() {
        font(".ZedMono")
    } else {
        font(".SystemUIFont")
    };
    if style.bold() {
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

fn parse_block_prefix(line: &str) -> (BlockKind, &str) {
    if let Some(rest) = line.strip_prefix("- ") {
        (BlockKind::Bulleted, rest)
    } else if let Some(prefix_len) = looks_numbered_prefix(line) {
        (BlockKind::Numbered, &line[prefix_len..])
    } else {
        (BlockKind::Paragraph, line)
    }
}

fn looks_numbered_prefix(line: &str) -> Option<usize> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && line.get(digits..digits + 2) == Some(". ")).then_some(digits + 2)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InlineParseOperations {
    indexed_bytes: usize,
    parse_steps: usize,
    delimiter_queries: usize,
    delimiter_positions_advanced: usize,
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

fn parse_inline_with_operations(
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
fn markdown_fits(text: &str, runs: &[StyleRun], blocks: &[BlockKind]) -> bool {
    markdown_fits_with_run_operations(text, runs, blocks).0
}

fn markdown_fits_with_run_operations(
    text: &str,
    runs: &[StyleRun],
    blocks: &[BlockKind],
) -> (bool, usize) {
    let ranges = line_ranges(text);
    if ranges.len() != blocks.len() {
        return (false, 0);
    }
    let mut total = 0usize;
    let mut cursor = StyleRunCursor::new(runs);
    for (line_index, (start, end)) in ranges.into_iter().enumerate() {
        if line_index > 0 && !add_with_limit(&mut total, 1) {
            return (false, cursor.operations());
        }
        let prefix_len = match blocks[line_index] {
            BlockKind::Paragraph => 0,
            BlockKind::Bulleted => 2,
            BlockKind::Numbered => 3,
        };
        if !add_with_limit(&mut total, prefix_len) {
            return (false, cursor.operations());
        }
        let (first_style, first_run_end) = cursor.style_extent_at(start);
        if blocks[line_index] == BlockKind::Paragraph
            && paragraph_prefix_needs_escape(text, start, end, first_style, first_run_end)
            && !add_with_limit(&mut total, 1)
        {
            return (false, cursor.operations());
        }
        let mut fits = true;
        cursor.for_each_overlap(start, end, |overlap, style| {
            if !fits {
                return;
            }
            fits = encoded_inline_len(&text[overlap], style)
                .is_some_and(|encoded_len| add_with_limit(&mut total, encoded_len));
        });
        if !fits {
            return (false, cursor.operations());
        }
    }
    (true, cursor.operations())
}

fn add_with_limit(total: &mut usize, additional: usize) -> bool {
    let Some(updated) = total.checked_add(additional) else {
        return false;
    };
    if updated > MAX_COMMENT_BYTES {
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

fn line_ranges(text: &str) -> Vec<(usize, usize)> {
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

fn line_at_offset(text: &str, offset: usize) -> usize {
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

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
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

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
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
    fn fallible_parser_rejects_raw_valid_but_canonical_oversize_markdown() {
        let raw = format!("*{}", "x".repeat(MAX_COMMENT_BYTES - 1));
        assert_eq!(raw.len(), MAX_COMMENT_BYTES);

        let parsed = RichTextBuffer::parse_markdown(&raw);
        assert_eq!(parsed.markdown().len(), MAX_COMMENT_BYTES + 1);
        assert_eq!(
            RichTextBuffer::try_from_markdown(&raw),
            Err(CommentMarkdownTooLong)
        );
        assert_eq!(
            RichTextBuffer::try_from_markdown(&format!("{raw}x")),
            Err(CommentMarkdownTooLong)
        );
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
        assert_eq!(buffer.blocks, vec![BlockKind::Paragraph; 3]);
    }

    #[test]
    fn code_fence_grows_to_preserve_embedded_backticks() {
        let buffer = RichTextBuffer::from_markdown("``a`b``");
        assert_eq!(buffer.text(), "a`b");
        assert_eq!(buffer.markdown(), "``a`b``");
        assert_eq!(RichTextBuffer::from_markdown(&buffer.markdown()), buffer);
    }

    #[test]
    fn unmatched_backtick_runs_are_literal_and_parser_work_is_linear() {
        let mut source = String::new();
        let run_count = 512usize;
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

        // Exact delimiter runs are paired. A shorter trailing run is literal
        // rather than causing repeated suffix retries inside the opener.
        let literal = RichTextBuffer::from_markdown("```code``");
        assert_eq!(literal.text(), "```code``");
        assert_eq!(literal.runs.len(), 1);

        // Outside a code span the backslash branch consumes the escaped
        // opener, so it cannot later become a delimiter.
        let escaped = RichTextBuffer::from_markdown("\\`not code`");
        assert_eq!(escaped.text(), "`not code`");
        assert_eq!(escaped.runs.len(), 1);
    }

    #[test]
    fn selection_toggle_splits_and_rejoins_runs() {
        let mut buffer = RichTextBuffer::from_markdown("hello");
        buffer.set_selection(1..4, false);
        buffer.toggle_bold();
        assert_eq!(buffer.markdown(), "h**ell**o");
        assert_eq!(buffer.runs.len(), 3);
        buffer.toggle_bold();
        assert_eq!(buffer.markdown(), "hello");
        assert_eq!(buffer.runs.len(), 1);
    }

    #[test]
    fn caret_toggle_is_inherited_by_inserted_text() {
        let mut buffer = RichTextBuffer::from_markdown("ab");
        buffer.move_to(1);
        buffer.toggle_italic();
        buffer.replace_selection("λ");
        assert_eq!(buffer.text(), "aλb");
        assert_eq!(buffer.markdown(), "a*λ*b");
    }

    #[test]
    fn code_is_exclusive_with_bold_and_italic() {
        let mut buffer = RichTextBuffer::from_markdown("word");
        buffer.select_all();
        buffer.toggle_bold();
        buffer.toggle_italic();
        assert_eq!(buffer.markdown(), "***word***");
        buffer.toggle_code();
        assert_eq!(buffer.markdown(), "`word`");
        buffer.toggle_bold();
        assert_eq!(buffer.markdown(), "**word**");
    }

    #[test]
    fn replacing_across_runs_preserves_both_outer_fragments() {
        let mut buffer = RichTextBuffer::from_markdown("**ab**cd*ef*");
        buffer.set_selection(1..5, false);
        buffer.replace_selection("X");
        assert_eq!(buffer.text(), "aXf");
        assert!(buffer.runs.first().unwrap().style.bold());
        assert!(buffer.runs.last().unwrap().style.italic());
        assert_eq!(
            RichTextBuffer::from_markdown(&buffer.markdown()).text(),
            "aXf"
        );
    }

    #[test]
    fn list_toggle_covers_selected_lines_but_not_trailing_boundary() {
        let mut buffer = RichTextBuffer::from_markdown("one\ntwo\nthree");
        buffer.set_selection(0..4, false); // Includes `one\n`, not `two`.
        buffer.toggle_bulleted_list();
        assert_eq!(buffer.markdown(), "- one\ntwo\nthree");
        buffer.set_selection(0..buffer.text().len(), false);
        buffer.toggle_numbered_list();
        assert_eq!(buffer.markdown(), "1. one\n1. two\n1. three");
    }

    #[test]
    fn splitting_and_merging_lines_preserves_block_semantics() {
        let mut buffer = RichTextBuffer::from_markdown("- onetwo");
        buffer.move_to(3);
        buffer.replace_selection("\n");
        assert_eq!(buffer.markdown(), "- one\n- two");
        buffer.set_selection(3..4, false);
        buffer.replace_selection("");
        assert_eq!(buffer.markdown(), "- onetwo");
        assert_eq!(buffer.blocks, vec![BlockKind::Bulleted]);
    }

    #[test]
    fn utf16_conversion_handles_astral_and_combining_text() {
        let buffer = RichTextBuffer::from_markdown("A😀e\u{301}B");
        assert_eq!(buffer.offset_to_utf16(1), 1);
        assert_eq!(buffer.offset_to_utf16(5), 3);
        assert_eq!(buffer.offset_from_utf16(2), 1); // Inside 😀 rounds down safely.
        assert_eq!(buffer.offset_from_utf16(3), 5);
        assert_eq!(buffer.range_from_utf16(1..3), 1..5);
    }

    #[test]
    fn ime_replacement_tracks_marked_range_and_relative_selection() {
        let mut buffer = RichTextBuffer::from_markdown("A😀B");
        buffer.set_selection(1..5, false);
        assert!(buffer.replace_and_mark_utf16(None, "漢字", Some(1..2)));
        assert_eq!(buffer.text(), "A漢字B");
        assert_eq!(buffer.marked_range, Some(1..7));
        assert_eq!(buffer.selection, 4..7);
        assert!(buffer.replace_and_mark_utf16(None, "語", Some(1..1)));
        assert_eq!(buffer.text(), "A語B");
        assert_eq!(buffer.marked_range, Some(1..4));
    }

    #[test]
    fn paste_accepts_exact_multibyte_limit_and_overflow_is_atomic() {
        let exact = "é".repeat(MAX_COMMENT_BYTES / "é".len());
        assert_eq!(exact.len(), MAX_COMMENT_BYTES);

        let mut buffer = RichTextBuffer::from_markdown("");
        assert!(buffer.replace_selection(&exact));
        assert_eq!(buffer.markdown().len(), MAX_COMMENT_BYTES);

        // A caret style is part of editor state even though it has no Markdown
        // representation until text is inserted.
        buffer.move_to("é".len());
        assert!(buffer.toggle_bold());
        let before = buffer.clone();
        assert!(!buffer.replace_selection("é"));
        assert_eq!(buffer, before);
    }

    #[test]
    fn native_replacement_rejection_preserves_reversed_selection() {
        let exact = "é".repeat(MAX_COMMENT_BYTES / "é".len());
        let mut buffer = RichTextBuffer::from_markdown(&exact);
        buffer.set_selection(2..6, true);
        let before = buffer.clone();

        // The explicit zero-width native range must not replace the editor's
        // selection before the limit check has succeeded.
        assert!(!buffer.replace_text_utf16(Some(0..0), "漢"));
        assert_eq!(buffer, before);
    }

    #[test]
    fn ime_exact_boundary_and_rejected_update_preserve_composition() {
        let marker_bytes = 4; // The canonical `**...**` surrounding the run.
        let exact = "漢".repeat((MAX_COMMENT_BYTES - marker_bytes) / "漢".len());
        assert_eq!(exact.len() + marker_bytes, MAX_COMMENT_BYTES);

        let mut buffer = RichTextBuffer::from_markdown("**seed**");
        buffer.select_all();
        assert!(buffer.replace_and_mark_utf16(None, &exact, Some(1..2)));
        assert_eq!(buffer.markdown().len(), MAX_COMMENT_BYTES);
        assert_eq!(buffer.marked_range, Some(0..exact.len()));

        let before = buffer.clone();
        let overflow = format!("{exact}a");
        assert!(!buffer.replace_and_mark_utf16(None, &overflow, Some(0..0)));
        assert_eq!(buffer, before);
    }

    #[test]
    fn serialized_escaping_and_formatting_overhead_obey_same_limit() {
        let escaped_exact = "*".repeat(MAX_COMMENT_BYTES / 2);
        let mut escaped = RichTextBuffer::from_markdown("");
        assert!(escaped.replace_selection(&escaped_exact));
        assert_eq!(escaped.markdown().len(), MAX_COMMENT_BYTES);
        let before_paste = escaped.clone();
        assert!(!escaped.replace_selection("*"));
        assert_eq!(escaped, before_paste);

        let mut formatted = RichTextBuffer::from_markdown(&"x".repeat(MAX_COMMENT_BYTES));
        formatted.select_all();
        let before_formatting = formatted.clone();
        assert!(!formatted.toggle_bold());
        assert_eq!(formatted, before_formatting);
    }

    #[test]
    fn backspace_removes_combining_and_zwj_clusters_together() {
        let mut combining = RichTextBuffer::from_markdown("e\u{301}");
        combining.backspace();
        assert_eq!(combining.text(), "");

        let mut family = RichTextBuffer::from_markdown("👩\u{200d}💻");
        family.backspace();
        assert_eq!(family.text(), "");
    }

    #[test]
    fn reverse_selection_keeps_a_stable_anchor() {
        let mut buffer = RichTextBuffer::from_markdown("abcd");
        buffer.move_to(3);
        buffer.select_to(1);
        assert_eq!(buffer.selection, 1..3);
        assert!(buffer.selection_reversed);
        assert_eq!(buffer.cursor_offset(), 1);
        buffer.select_to(4);
        assert_eq!(buffer.selection, 3..4);
        assert!(!buffer.selection_reversed);
    }

    #[test]
    fn display_projection_hides_markers_and_maps_list_prefixes() {
        let buffer = RichTextBuffer::from_markdown("- one\n1. two\n1. three");
        let projection = DisplayProjection::new(&buffer);
        assert_eq!(projection.text.as_ref(), "• one\n1. two\n2. three");
        assert_eq!(projection.model_for_display(0), 0);
        assert_eq!(projection.model_for_display(2), 0);
        assert_eq!(projection.display_for_model(1), 5);
        for offset in buffer
            .text()
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([buffer.text().len()])
        {
            assert_eq!(
                projection.model_for_display(projection.display_for_model(offset)),
                offset
            );
        }
    }

    #[test]
    fn markdown_and_projection_traverse_many_lines_and_runs_linearly() {
        let line_count = 640usize;
        let markdown = (0..line_count)
            .map(|index| format!("**b{index}** *i{index}* plain"))
            .collect::<Vec<_>>()
            .join("\n");
        let buffer = RichTextBuffer::from_markdown(&markdown);
        let structural_size = buffer.blocks.len() + buffer.runs.len();

        let (serialized, markdown_operations) = buffer.markdown_with_run_operations();
        let (fits, fit_operations) =
            markdown_fits_with_run_operations(&buffer.text, &buffer.runs, &buffer.blocks);
        let (projection, projection_operations) =
            DisplayProjection::new_with_run_operations(&buffer);

        assert_eq!(serialized, markdown);
        assert!(fits);
        assert_eq!(projection.lines.len(), line_count);
        assert!(markdown_operations <= structural_size * 3);
        assert!(fit_operations <= structural_size * 3);
        assert!(projection_operations <= structural_size * 3);
    }

    #[test]
    fn crlf_and_cr_are_normalized_without_losing_block_count() {
        let buffer = RichTextBuffer::from_markdown("a\r\n- b\rc");
        assert_eq!(buffer.text(), "a\nb\nc");
        assert_eq!(
            buffer.blocks,
            vec![
                BlockKind::Paragraph,
                BlockKind::Bulleted,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(buffer.markdown(), "a\n- b\nc");
    }

    #[test]
    fn empty_and_trailing_empty_lines_round_trip() {
        for markdown in ["", "\n", "a\n", "\n\n"] {
            let buffer = RichTextBuffer::from_markdown(markdown);
            assert_eq!(buffer.markdown(), markdown);
            assert_eq!(
                buffer.blocks.len(),
                buffer.text().bytes().filter(|byte| *byte == b'\n').count() + 1
            );
        }
    }
}
