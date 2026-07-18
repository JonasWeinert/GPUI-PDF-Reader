//! A small WYSIWYG Markdown editor for GPUI.
//!
//! The editor keeps rich text as plain UTF-8 plus semantic inline and block
//! styles. Markdown is only a deterministic storage format; Markdown markers
//! are never exposed to the person editing the document.

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString, StyledText,
    TextLayout, TextRun, UTF16Selection, UnderlineStyle, Window, actions, canvas, div, font, point,
    prelude::*, px, quad, size,
};
use gpui_component::{Icon, IconName, Theme};
use std::ops::Range;

use crate::theme::ReaderPalette;
use crate::{EditCopy, EditCut, EditPaste, EditSelectAll};

pub use key_editor_core::{BlockKind, InlineStyle, RichTextBuffer};
// Compatibility name for existing PDF-comment callers. New reusable code
// should use the storage-neutral `MarkdownLimitExceeded` name.
#[cfg(test)]
use key_editor_core::{
    DEFAULT_MAX_MARKDOWN_BYTES as MAX_COMMENT_BYTES, markdown_fits_with_run_operations,
    parse_inline_with_operations,
};
#[allow(unused_imports)]
pub use key_editor_core::{MarkdownLimitExceeded, MarkdownLimitExceeded as CommentMarkdownTooLong};
use key_editor_core::{
    StyleRunCursor, line_at_offset, line_ranges, next_grapheme_boundary, previous_grapheme_boundary,
};

const COMMENT_TOO_LONG_MESSAGE: &str = "Markdown exceeds the configured storage limit.";

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
        CommentNewline,
        CommentSave,
        CommentCancel,
    ]
);

/// Events emitted by [`MarkdownEditor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEditorEvent {
    Changed,
    Save(String),
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlashCommand {
    Paragraph,
    Heading1,
    Heading2,
    Heading3,
    Bulleted,
    Numbered,
    Quote,
    Bold,
    Italic,
    InlineCode,
}

impl SlashCommand {
    const ALL: [Self; 10] = [
        Self::Paragraph,
        Self::Heading1,
        Self::Heading2,
        Self::Heading3,
        Self::Bulleted,
        Self::Numbered,
        Self::Quote,
        Self::Bold,
        Self::Italic,
        Self::InlineCode,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Paragraph => "Text",
            Self::Heading1 => "Heading 1",
            Self::Heading2 => "Heading 2",
            Self::Heading3 => "Heading 3",
            Self::Bulleted => "Bulleted list",
            Self::Numbered => "Numbered list",
            Self::Quote => "Quote",
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::InlineCode => "Inline code",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Paragraph => "Plain paragraph",
            Self::Heading1 => "Large section heading",
            Self::Heading2 => "Medium section heading",
            Self::Heading3 => "Small section heading",
            Self::Bulleted => "Create an unordered list",
            Self::Numbered => "Create an ordered list",
            Self::Quote => "Emphasize a quotation",
            Self::Bold => "Strong emphasis",
            Self::Italic => "Light emphasis",
            Self::InlineCode => "Monospaced inline text",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Paragraph | Self::Heading1 | Self::Heading2 | Self::Heading3 => {
                IconName::ALargeSmall
            }
            Self::Bulleted | Self::Numbered => IconName::Menu,
            Self::Quote => IconName::Asterisk,
            Self::Bold | Self::Italic => IconName::CaseSensitive,
            Self::InlineCode => IconName::SquareTerminal,
        }
    }

    fn matches(self, query: &str) -> bool {
        let query = query.to_ascii_lowercase();
        query.is_empty()
            || self.label().to_ascii_lowercase().contains(&query)
            || (query.len() >= 2 && self.detail().to_ascii_lowercase().contains(&query))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SlashMenuState {
    range: Range<usize>,
    selected: usize,
}

/// GPUI entity that renders and edits [`RichTextBuffer`] without showing
/// Markdown markers.
pub struct MarkdownEditor {
    focus_handle: FocusHandle,
    buffer: RichTextBuffer,
    layout: TextLayout,
    projection: DisplayProjection,
    input_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    validation_message: Option<SharedString>,
    slash_menu: Option<SlashMenuState>,
}

impl MarkdownEditor {
    /// Builds an editor from a buffer produced by the fallible persisted-data
    /// parser. Buffer mutations preserve the same canonical size invariant.
    pub fn new(cx: &mut Context<Self>, buffer: RichTextBuffer) -> Self {
        debug_assert!(buffer.fits_persistence_limit());
        let projection = DisplayProjection::new(&buffer);
        Self {
            focus_handle: cx.focus_handle(),
            buffer,
            layout: TextLayout::default(),
            projection,
            input_bounds: None,
            selecting: false,
            validation_message: None,
            slash_menu: None,
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
        if !self.buffer.fits_persistence_limit() {
            self.validation_message = Some(COMMENT_TOO_LONG_MESSAGE.into());
            cx.notify();
            return false;
        }
        cx.emit(MarkdownEditorEvent::Save(self.markdown()));
        true
    }

    #[cfg(debug_assertions)]
    pub fn qa_has_painted(&self) -> bool {
        self.input_bounds.is_some()
    }

    fn emit_changed(&self, cx: &mut Context<Self>) {
        cx.emit(MarkdownEditorEvent::Changed);
        cx.notify();
    }

    fn finish_mutation(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if accepted {
            self.validation_message = None;
            self.refresh_slash_menu();
            self.emit_changed(cx);
        } else {
            self.validation_message = Some(COMMENT_TOO_LONG_MESSAGE.into());
            cx.notify();
        }
    }

    fn slash_query(&self) -> Option<(Range<usize>, String)> {
        if !self.buffer.selection().is_empty() {
            return None;
        }
        let cursor = self.buffer.cursor_offset();
        let line_start = self.buffer.text()[..cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let typed = &self.buffer.text()[line_start..cursor];
        let query = typed.strip_prefix('/')?;
        if query.chars().any(char::is_whitespace) {
            return None;
        }
        Some((line_start..cursor, query.to_owned()))
    }

    fn filtered_slash_commands(&self) -> Vec<SlashCommand> {
        let query = self
            .slash_query()
            .map_or_else(String::new, |(_, query)| query);
        SlashCommand::ALL
            .into_iter()
            .filter(|command| command.matches(&query))
            .collect()
    }

    fn refresh_slash_menu(&mut self) {
        let Some((range, _)) = self.slash_query() else {
            self.slash_menu = None;
            return;
        };
        let count = self.filtered_slash_commands().len();
        let selected = self
            .slash_menu
            .as_ref()
            .map_or(0, |menu| menu.selected.min(count.saturating_sub(1)));
        self.slash_menu = Some(SlashMenuState { range, selected });
    }

    fn apply_slash_command(&mut self, command: SlashCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.slash_menu.take() else {
            return;
        };
        let original = self.buffer.clone();
        self.buffer.set_selection(menu.range, false);
        let removed = self.buffer.replace_selection("");
        let formatted = removed
            && match command {
                SlashCommand::Paragraph => self.buffer.set_block(BlockKind::Paragraph),
                SlashCommand::Heading1 => self.buffer.set_block(BlockKind::Heading1),
                SlashCommand::Heading2 => self.buffer.set_block(BlockKind::Heading2),
                SlashCommand::Heading3 => self.buffer.set_block(BlockKind::Heading3),
                SlashCommand::Bulleted => self.buffer.set_block(BlockKind::Bulleted),
                SlashCommand::Numbered => self.buffer.set_block(BlockKind::Numbered),
                SlashCommand::Quote => self.buffer.set_block(BlockKind::Quote),
                SlashCommand::Bold => self.buffer.toggle_bold(),
                SlashCommand::Italic => self.buffer.toggle_italic(),
                SlashCommand::InlineCode => self.buffer.toggle_code(),
            };
        if !formatted {
            self.buffer = original;
        }
        self.finish_mutation(formatted, cx);
        self.slash_menu = None;
    }

    fn newline(&mut self, _: &CommentNewline, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.slash_menu.as_ref() {
            let commands = self.filtered_slash_commands();
            if let Some(command) = commands.get(menu.selected).copied() {
                self.apply_slash_command(command, cx);
                cx.stop_propagation();
                return;
            }
        }
        let accepted = self.buffer.insert_newline();
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
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
        let selection = self.buffer.selection();
        let destination = if selection.is_empty() {
            previous_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset())
        } else {
            selection.start
        };
        self.buffer.move_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn move_right(&mut self, _: &CommentRight, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        let destination = if selection.is_empty() {
            next_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset())
        } else {
            selection.end
        };
        self.buffer.move_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn select_left(&mut self, _: &CommentSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let destination =
            previous_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn select_right(&mut self, _: &CommentSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let destination = next_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn move_up(&mut self, _: &CommentUp, _: &mut Window, cx: &mut Context<Self>) {
        let slash_command_count = self.filtered_slash_commands().len();
        if let Some(menu) = self.slash_menu.as_mut() {
            let count = slash_command_count;
            menu.selected = menu
                .selected
                .checked_sub(1)
                .unwrap_or(count.saturating_sub(1));
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if let Some(destination) = self.vertical_destination(-1.0) {
            self.buffer.move_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn move_down(&mut self, _: &CommentDown, _: &mut Window, cx: &mut Context<Self>) {
        let slash_command_count = self.filtered_slash_commands().len();
        if let Some(menu) = self.slash_menu.as_mut() {
            let count = slash_command_count;
            menu.selected = if count == 0 {
                0
            } else {
                (menu.selected + 1) % count
            };
            cx.notify();
            cx.stop_propagation();
            return;
        }
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
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn end(&mut self, _: &CommentEnd, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.cursor_offset();
        let end = self.buffer.text()[cursor..]
            .find('\n')
            .map_or(self.buffer.text().len(), |index| cursor + index);
        self.buffer.move_to(end);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &CommentCopy, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        if !selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text()[selection].to_owned(),
            ));
        }
        cx.stop_propagation();
    }

    fn edit_copy(&mut self, _: &EditCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&CommentCopy, window, cx);
    }

    fn cut(&mut self, _: &CommentCut, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        if !selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text()[selection].to_owned(),
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
        if self.slash_menu.take().is_none() {
            cx.emit(MarkdownEditorEvent::Cancel);
        } else {
            cx.notify();
        }
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
        self.refresh_slash_menu();
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

    fn render_slash_menu(
        &self,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.slash_menu.as_ref()?;
        let commands = self.filtered_slash_commands();
        let body = if commands.is_empty() {
            div()
                .px_3()
                .py_4()
                .text_sm()
                .text_color(palette.text_secondary)
                .child("No matching Markdown style")
                .into_any_element()
        } else {
            div()
                .id("markdown-slash-menu-items")
                .max_h(px(292.0))
                .overflow_y_scroll()
                .p_1()
                .children(commands.into_iter().enumerate().map(|(index, command)| {
                    let selected = index == menu.selected;
                    div()
                        .id(("markdown-command", index))
                        .h(px(48.0))
                        .w_full()
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded_lg()
                        .cursor_pointer()
                        .bg(if selected {
                            palette.accent_soft
                        } else {
                            palette.surface
                        })
                        .hover(move |row| row.bg(palette.control_hover))
                        .on_click(cx.listener(move |editor, _, window, cx| {
                            editor.apply_slash_command(command, cx);
                            window.focus(&editor.focus_handle);
                        }))
                        .child(
                            div()
                                .size(px(30.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(if selected {
                                    palette.accent.opacity(0.14)
                                } else {
                                    palette.surface_subtle
                                })
                                .text_color(if selected {
                                    palette.accent
                                } else {
                                    palette.text_secondary
                                })
                                .child(Icon::new(command.icon()).size(px(15.0))),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(palette.text)
                                        .child(command.label()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(palette.text_secondary)
                                        .child(command.detail()),
                                ),
                        )
                }))
                .into_any_element()
        };
        Some(
            div()
                .id("markdown-slash-menu")
                .absolute()
                .top(px(86.0))
                .left(px(12.0))
                .right(px(12.0))
                .max_w(px(360.0))
                .overflow_hidden()
                .rounded_xl()
                .border_1()
                .border_color(palette.separator)
                .bg(palette.surface)
                .shadow_lg()
                .child(
                    div()
                        .h(px(34.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(palette.separator)
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.text_secondary)
                        .child("INSERT MARKDOWN")
                        .child("↑↓  Return"),
                )
                .child(body)
                .into_any_element(),
        )
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

impl EventEmitter<MarkdownEditorEvent> for MarkdownEditor {}

impl Focusable for MarkdownEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for MarkdownEditor {
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
            range: self.buffer.range_to_utf16(self.buffer.selection()),
            reversed: self.buffer.selection_is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked_range()
            .clone()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.clear_marked_range();
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

impl Render for MarkdownEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let focused = self.focus_handle.is_focused(window);
        self.projection = DisplayProjection::new(&self.buffer);
        let runs = display_text_runs(&self.buffer, &self.projection, palette);
        let styled = StyledText::new(self.projection.text.clone()).with_runs(runs);
        self.layout = styled.layout().clone();

        let editor = cx.entity();
        let slash_menu = self.render_slash_menu(palette, cx);
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
            .key_context("MarkdownEditor")
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
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::cancel))
            .child(text_area)
            .children(validation_message)
            .child(toolbar)
            .children(slash_menu)
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
    palette: ReaderPalette,
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

        let parsed = RichTextBuffer::parse_markdown(&raw, MAX_COMMENT_BYTES);
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
    fn caller_supplied_markdown_limit_is_preserved_by_mutations() {
        assert_eq!(
            RichTextBuffer::try_from_markdown_with_limit("*", 1),
            Err(MarkdownLimitExceeded)
        );
        assert!(COMMENT_TOO_LONG_MESSAGE.contains("configured storage limit"));
        assert!(!COMMENT_TOO_LONG_MESSAGE.contains("1 MiB"));

        let mut buffer = RichTextBuffer::try_from_markdown_with_limit("hello", 5).unwrap();
        let before = buffer.clone();
        assert!(!buffer.replace_selection("!"));
        assert_eq!(buffer, before);
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
        assert_eq!(buffer.block_kinds(), vec![BlockKind::Paragraph; 3]);
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
        assert_eq!(literal.style_runs().len(), 1);

        // Outside a code span the backslash branch consumes the escaped
        // opener, so it cannot later become a delimiter.
        let escaped = RichTextBuffer::from_markdown("\\`not code`");
        assert_eq!(escaped.text(), "`not code`");
        assert_eq!(escaped.style_runs().len(), 1);
    }

    #[test]
    fn selection_toggle_splits_and_rejoins_runs() {
        let mut buffer = RichTextBuffer::from_markdown("hello");
        buffer.set_selection(1..4, false);
        buffer.toggle_bold();
        assert_eq!(buffer.markdown(), "h**ell**o");
        assert_eq!(buffer.style_runs().len(), 3);
        buffer.toggle_bold();
        assert_eq!(buffer.markdown(), "hello");
        assert_eq!(buffer.style_runs().len(), 1);
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
        assert!(buffer.style_runs().first().unwrap().style().bold());
        assert!(buffer.style_runs().last().unwrap().style().italic());
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
        assert_eq!(buffer.block_kinds(), vec![BlockKind::Bulleted]);
    }

    #[test]
    fn return_inserts_lines_continues_lists_and_exits_empty_items() {
        let mut paragraph = RichTextBuffer::from_markdown("firstsecond");
        paragraph.move_to(5);
        assert!(paragraph.insert_newline());
        assert_eq!(paragraph.markdown(), "first\nsecond");

        let mut list = RichTextBuffer::from_markdown("- first");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n- ");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n");
        assert_eq!(
            list.block_kinds(),
            vec![BlockKind::Bulleted, BlockKind::Paragraph]
        );
    }

    #[test]
    fn headings_and_quotes_round_trip_and_heading_return_starts_body_text() {
        let markdown = "# Title\n## Section\n### Detail\n> Quoted";
        let mut buffer = RichTextBuffer::from_markdown(markdown);
        assert_eq!(buffer.markdown(), markdown);
        buffer.move_to("Title".len());
        assert!(buffer.insert_newline());
        assert_eq!(
            buffer.markdown(),
            "# Title\n\n## Section\n### Detail\n> Quoted"
        );
        assert_eq!(buffer.block_kinds()[1], BlockKind::Paragraph);
    }

    #[test]
    fn slash_commands_filter_by_names_and_descriptions() {
        assert!(SlashCommand::Heading2.matches("head"));
        assert!(SlashCommand::Bulleted.matches("unordered"));
        assert!(SlashCommand::InlineCode.matches("mono"));
        assert!(!SlashCommand::Quote.matches("number"));
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
        assert_eq!(buffer.marked_range(), Some(1..7));
        assert_eq!(buffer.selection(), 4..7);
        assert!(buffer.replace_and_mark_utf16(None, "語", Some(1..1)));
        assert_eq!(buffer.text(), "A語B");
        assert_eq!(buffer.marked_range(), Some(1..4));
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
        assert_eq!(buffer.marked_range(), Some(0..exact.len()));

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
        assert_eq!(buffer.selection(), 1..3);
        assert!(buffer.selection_is_reversed());
        assert_eq!(buffer.cursor_offset(), 1);
        buffer.select_to(4);
        assert_eq!(buffer.selection(), 3..4);
        assert!(!buffer.selection_is_reversed());
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
        let structural_size = buffer.block_kinds().len() + buffer.style_runs().len();

        let (serialized, markdown_operations) = buffer.markdown_with_run_operations();
        let (fits, fit_operations) = markdown_fits_with_run_operations(
            buffer.text(),
            buffer.style_runs(),
            buffer.block_kinds(),
        );
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
            buffer.block_kinds(),
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
                buffer.block_kinds().len(),
                buffer.text().bytes().filter(|byte| *byte == b'\n').count() + 1
            );
        }
    }
}
