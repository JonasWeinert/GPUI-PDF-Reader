//! Native input, interaction, command handling, and rendering.

use crate::projection::{DisplayProjection, display_text_runs};
use crate::{
    BlockKind, MARKDOWN_EDITOR_KEY_CONTEXT, MarkdownBackspace, MarkdownCancel, MarkdownCopy,
    MarkdownCut, MarkdownDelete, MarkdownDown, MarkdownEditorCommand, MarkdownEditorConfig,
    MarkdownEditorStyle, MarkdownEnd, MarkdownHome, MarkdownLeft, MarkdownLimitExceeded,
    MarkdownNewline, MarkdownPaste, MarkdownRight, MarkdownSave, MarkdownSelectAll,
    MarkdownSelectDown, MarkdownSelectLeft, MarkdownSelectRight, MarkdownSelectUp,
    MarkdownToggleBold, MarkdownToggleBulletedList, MarkdownToggleCode, MarkdownToggleItalic,
    MarkdownToggleNumberedList, MarkdownUp, RichTextBuffer,
};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, StyledText, TextLayout, UTF16Selection,
    Window, canvas, div, point, prelude::*, px, quad, size,
};
use gpui_component::Icon;
use key_editor_core::{line_at_offset, next_grapheme_boundary, previous_grapheme_boundary};
use key_ui_gpui::{DesignStyled as _, ElevationRole, RadiusRole, ThemeTokens, TypographyRole};
use std::{fmt, ops::Range};

/// Events emitted by [`MarkdownEditor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEditorEvent {
    /// The document changed; callers can query [`MarkdownEditor::markdown`].
    Changed,
    /// The user explicitly requested persistence through the submit binding.
    Save(String),
    /// The user requested dismissal. Escape closes an open slash menu first.
    Cancel,
}

/// A pre-built core buffer must use the same limit as its UI configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownEditorConfigError {
    pub buffer_max_markdown_bytes: usize,
    pub configured_max_markdown_bytes: usize,
}

impl fmt::Display for MarkdownEditorConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "editor buffer limit ({}) does not match configured limit ({})",
            self.buffer_max_markdown_bytes, self.configured_max_markdown_bytes
        )
    }
}

impl std::error::Error for MarkdownEditorConfigError {}

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
    config: MarkdownEditorConfig,
    layout: TextLayout,
    projection: DisplayProjection,
    input_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    validation_message: Option<SharedString>,
    slash_menu: Option<SlashMenuState>,
}

impl MarkdownEditor {
    /// Builds an editor around a core buffer. The limits must match so every
    /// mutation and every UI validation message describe the same invariant.
    pub fn new(
        cx: &mut Context<Self>,
        buffer: RichTextBuffer,
        mut config: MarkdownEditorConfig,
    ) -> Result<Self, MarkdownEditorConfigError> {
        if buffer.max_markdown_bytes() != config.max_markdown_bytes {
            return Err(MarkdownEditorConfigError {
                buffer_max_markdown_bytes: buffer.max_markdown_bytes(),
                configured_max_markdown_bytes: config.max_markdown_bytes,
            });
        }
        debug_assert!(buffer.fits_persistence_limit());
        config.normalize();
        let projection = DisplayProjection::new(&buffer);
        Ok(Self {
            focus_handle: cx.focus_handle(),
            buffer,
            config,
            layout: TextLayout::default(),
            projection,
            input_bounds: None,
            selecting: false,
            validation_message: None,
            slash_menu: None,
        })
    }

    /// Parses persisted Markdown and builds a configured editor in one step.
    pub fn from_markdown(
        cx: &mut Context<Self>,
        markdown: &str,
        mut config: MarkdownEditorConfig,
    ) -> Result<Self, MarkdownLimitExceeded> {
        config.normalize();
        let buffer = config.parse_markdown(markdown)?;
        Ok(Self::build(cx, buffer, config))
    }

    /// Preserves the original zero-configuration construction path.
    pub fn with_default_config(cx: &mut Context<Self>, buffer: RichTextBuffer) -> Self {
        let config = MarkdownEditorConfig {
            max_markdown_bytes: buffer.max_markdown_bytes(),
            ..MarkdownEditorConfig::default()
        };
        Self::build(cx, buffer, config)
    }

    fn build(cx: &mut Context<Self>, buffer: RichTextBuffer, config: MarkdownEditorConfig) -> Self {
        debug_assert_eq!(buffer.max_markdown_bytes(), config.max_markdown_bytes);
        debug_assert!(buffer.fits_persistence_limit());
        let projection = DisplayProjection::new(&buffer);
        Self {
            focus_handle: cx.focus_handle(),
            buffer,
            config,
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

    pub fn config(&self) -> &MarkdownEditorConfig {
        &self.config
    }

    pub fn buffer(&self) -> &RichTextBuffer {
        &self.buffer
    }

    /// Emits a save only while the canonical Markdown persistence invariant
    /// holds. Keyboard and host controls should share this guarded path.
    pub fn request_save(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.buffer.fits_persistence_limit() {
            self.validation_message = Some(self.config.limit_message.clone());
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
            self.validation_message = Some(self.config.limit_message.clone());
            cx.notify();
        }
    }

    fn slash_query(&self) -> Option<(Range<usize>, String)> {
        slash_query(&self.buffer)
    }

    fn filtered_slash_commands(&self) -> Vec<MarkdownEditorCommand> {
        let query = self
            .slash_query()
            .map_or_else(String::new, |(_, query)| query);
        filter_commands(&self.config.slash_commands, &query)
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

    fn apply_slash_command(&mut self, command: MarkdownEditorCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.slash_menu.take() else {
            return;
        };
        let formatted = apply_slash_command(&mut self.buffer, menu.range, command);
        self.finish_mutation(formatted, cx);
        self.slash_menu = None;
    }

    fn newline(&mut self, _: &MarkdownNewline, _: &mut Window, cx: &mut Context<Self>) {
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

    fn backspace(&mut self, _: &MarkdownBackspace, _: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.backspace();
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn delete(&mut self, _: &MarkdownDelete, _: &mut Window, cx: &mut Context<Self>) {
        let accepted = self.buffer.delete_forward();
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn move_left(&mut self, _: &MarkdownLeft, _: &mut Window, cx: &mut Context<Self>) {
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

    fn move_right(&mut self, _: &MarkdownRight, _: &mut Window, cx: &mut Context<Self>) {
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

    fn select_left(&mut self, _: &MarkdownSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let destination =
            previous_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn select_right(&mut self, _: &MarkdownSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let destination = next_grapheme_boundary(self.buffer.text(), self.buffer.cursor_offset());
        self.buffer.select_to(destination);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn move_up(&mut self, _: &MarkdownUp, _: &mut Window, cx: &mut Context<Self>) {
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

    fn move_down(&mut self, _: &MarkdownDown, _: &mut Window, cx: &mut Context<Self>) {
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

    fn select_up(&mut self, _: &MarkdownSelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(destination) = self.vertical_destination(-1.0) {
            self.buffer.select_to(destination);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn select_down(&mut self, _: &MarkdownSelectDown, _: &mut Window, cx: &mut Context<Self>) {
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

    fn select_all(&mut self, _: &MarkdownSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
        cx.stop_propagation();
    }

    fn home(&mut self, _: &MarkdownHome, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.cursor_offset();
        let start = self.buffer.text()[..cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.buffer.move_to(start);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn end(&mut self, _: &MarkdownEnd, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.buffer.cursor_offset();
        let end = self.buffer.text()[cursor..]
            .find('\n')
            .map_or(self.buffer.text().len(), |index| cursor + index);
        self.buffer.move_to(end);
        self.refresh_slash_menu();
        cx.notify();
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &MarkdownCopy, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.buffer.selection();
        if !selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text()[selection].to_owned(),
            ));
        }
        cx.stop_propagation();
    }

    fn cut(&mut self, _: &MarkdownCut, _: &mut Window, cx: &mut Context<Self>) {
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

    fn paste(&mut self, _: &MarkdownPaste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let accepted = self.buffer.replace_selection(&text);
            self.finish_mutation(accepted, cx);
        }
        cx.stop_propagation();
    }

    fn toggle_bold(&mut self, _: &MarkdownToggleBold, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format_command(MarkdownEditorCommand::Bold, window, cx);
    }

    fn toggle_italic(
        &mut self,
        _: &MarkdownToggleItalic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_format_command(MarkdownEditorCommand::Italic, window, cx);
    }

    fn toggle_code(&mut self, _: &MarkdownToggleCode, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format_command(MarkdownEditorCommand::InlineCode, window, cx);
    }

    fn toggle_bulleted_list(
        &mut self,
        _: &MarkdownToggleBulletedList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_format_command(MarkdownEditorCommand::BulletedList, window, cx);
    }

    fn toggle_numbered_list(
        &mut self,
        _: &MarkdownToggleNumberedList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_format_command(MarkdownEditorCommand::NumberedList, window, cx);
    }

    fn apply_format_command(
        &mut self,
        command: MarkdownEditorCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.config.format_commands.contains(&command) {
            cx.stop_propagation();
            return;
        }
        let accepted = apply_command(&mut self.buffer, command);
        window.focus(&self.focus_handle);
        self.finish_mutation(accepted, cx);
        cx.stop_propagation();
    }

    fn save(&mut self, _: &MarkdownSave, _: &mut Window, cx: &mut Context<Self>) {
        self.request_save(cx);
        cx.stop_propagation();
    }

    fn cancel(&mut self, _: &MarkdownCancel, _: &mut Window, cx: &mut Context<Self>) {
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

    fn command_active(&self, command: MarkdownEditorCommand) -> bool {
        match command {
            MarkdownEditorCommand::Bold => self.buffer.bold_active(),
            MarkdownEditorCommand::Italic => self.buffer.italic_active(),
            MarkdownEditorCommand::InlineCode => self.buffer.code_active(),
            MarkdownEditorCommand::Paragraph => block_active(&self.buffer, BlockKind::Paragraph),
            MarkdownEditorCommand::Heading1 => block_active(&self.buffer, BlockKind::Heading1),
            MarkdownEditorCommand::Heading2 => block_active(&self.buffer, BlockKind::Heading2),
            MarkdownEditorCommand::Heading3 => block_active(&self.buffer, BlockKind::Heading3),
            MarkdownEditorCommand::BulletedList => self.buffer.bulleted_list_active(),
            MarkdownEditorCommand::NumberedList => self.buffer.numbered_list_active(),
            MarkdownEditorCommand::Quote => block_active(&self.buffer, BlockKind::Quote),
        }
    }

    fn render_slash_menu(
        &self,
        palette: MarkdownEditorStyle,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.slash_menu.as_ref()?;
        let tokens = ThemeTokens::from_app(cx);
        let metrics = tokens.components.editor;
        let commands = self.filtered_slash_commands();
        let body = if commands.is_empty() {
            div()
                .px_3()
                .py_4()
                .design_typography(TypographyRole::Body, &tokens)
                .text_color(palette.text_secondary)
                .child("No matching Markdown style")
                .into_any_element()
        } else {
            div()
                .id("markdown-slash-menu-items")
                .max_h(px(metrics.row_height * 6.0 + metrics.section_gap * 5.0))
                .overflow_y_scroll()
                .p_1()
                .children(commands.into_iter().enumerate().map(|(index, command)| {
                    let selected = index == menu.selected;
                    div()
                        .id(("markdown-command", index))
                        .h(px(metrics.row_height))
                        .w_full()
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .design_radius(RadiusRole::Large, &tokens)
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
                                .size(px(metrics.button_height))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .design_radius(RadiusRole::Medium, &tokens)
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
                                .child(Icon::new(command.icon()).size(px(metrics.icon_size))),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .design_typography(TypographyRole::Label, &tokens)
                                        .text_color(palette.text)
                                        .child(command.label()),
                                )
                                .child(
                                    div()
                                        .design_typography(TypographyRole::Caption, &tokens)
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
                .top(px(metrics.header_height + metrics.button_height))
                .left(px(metrics.card_padding))
                .right(px(metrics.card_padding))
                .max_w(px(tokens.components.popover.tab_search_width - 20.0))
                .overflow_hidden()
                .design_radius(RadiusRole::Large, &tokens)
                .border_1()
                .border_color(palette.separator)
                .bg(palette.surface)
                .design_elevation(ElevationRole::Floating, &tokens)
                .child(
                    div()
                        .h(px(tokens.components.common.control_medium))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(palette.separator)
                        .design_typography(TypographyRole::Heading, &tokens)
                        .text_color(palette.text_secondary)
                        .child("INSERT MARKDOWN")
                        .child("↑↓  Return"),
                )
                .child(body)
                .into_any_element(),
        )
    }

    fn format_button(
        palette: MarkdownEditorStyle,
        tokens: ThemeTokens,
        id: &'static str,
        label: impl IntoElement,
        active: bool,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(tokens.components.editor.button_height))
            .min_w(px(tokens.components.editor.button_height))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .design_radius(RadiusRole::Medium, &tokens)
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
            .design_typography(TypographyRole::Label, &tokens)
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
        let palette = self.config.style_policy.resolve(cx);
        let tokens = ThemeTokens::from_app(cx);
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
                .id("markdown-validation-message")
                .mt_1()
                .design_typography(TypographyRole::Caption, &tokens)
                .text_color(palette.error)
                .child(message)
        });
        let placeholder = self.config.placeholder.clone();
        let has_toolbar = !self.config.format_commands.is_empty();
        let text_area = div()
            .id("markdown-editor-text")
            .relative()
            .mt(px(if has_toolbar { 44.0 } else { 0.0 }))
            .flex_1()
            .min_h(px(tokens.components.editor.row_height * 3.0))
            .w_full()
            .overflow_y_scroll()
            .p_4()
            .design_radius(RadiusRole::Large, &tokens)
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
            .design_typography(TypographyRole::Body, &tokens)
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
                        .child(placeholder),
                )
            })
            .child(styled)
            .child(overlay);

        let toolbar = has_toolbar.then(|| {
            div()
                .id("markdown-format-pill")
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
                        .design_radius(RadiusRole::Large, &tokens)
                        .border_1()
                        .border_color(palette.separator)
                        .bg(palette.surface)
                        .design_elevation(ElevationRole::Surface, &tokens)
                        .children(self.config.format_commands.iter().copied().map(|command| {
                            Self::format_button(
                                palette,
                                tokens,
                                command.element_id(),
                                command.toolbar_label(),
                                self.command_active(command),
                                cx.listener(move |editor, _, window, cx| {
                                    editor.apply_format_command(command, window, cx);
                                }),
                            )
                        })),
                )
        });

        div()
            .id("markdown-editor")
            .key_context(MARKDOWN_EDITOR_KEY_CONTEXT)
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
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
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
            .children(toolbar)
            .children(slash_menu)
    }
}

fn slash_query(buffer: &RichTextBuffer) -> Option<(Range<usize>, String)> {
    if !buffer.selection().is_empty() {
        return None;
    }
    let cursor = buffer.cursor_offset();
    let line_start = buffer.text()[..cursor]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let typed = &buffer.text()[line_start..cursor];
    let query = typed.strip_prefix('/')?;
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some((line_start..cursor, query.to_owned()))
}

fn filter_commands(commands: &[MarkdownEditorCommand], query: &str) -> Vec<MarkdownEditorCommand> {
    commands
        .iter()
        .copied()
        .filter(|command| command.matches(query))
        .collect()
}

fn apply_slash_command(
    buffer: &mut RichTextBuffer,
    range: Range<usize>,
    command: MarkdownEditorCommand,
) -> bool {
    let original = buffer.clone();
    buffer.set_selection(range, false);
    let accepted = buffer.replace_selection("") && apply_command(buffer, command);
    if !accepted {
        *buffer = original;
    }
    accepted
}

fn apply_command(buffer: &mut RichTextBuffer, command: MarkdownEditorCommand) -> bool {
    match command {
        MarkdownEditorCommand::Paragraph => buffer.set_block(BlockKind::Paragraph),
        MarkdownEditorCommand::Heading1 => buffer.set_block(BlockKind::Heading1),
        MarkdownEditorCommand::Heading2 => buffer.set_block(BlockKind::Heading2),
        MarkdownEditorCommand::Heading3 => buffer.set_block(BlockKind::Heading3),
        MarkdownEditorCommand::BulletedList => buffer.set_block(BlockKind::Bulleted),
        MarkdownEditorCommand::NumberedList => buffer.set_block(BlockKind::Numbered),
        MarkdownEditorCommand::Quote => buffer.set_block(BlockKind::Quote),
        MarkdownEditorCommand::Bold => buffer.toggle_bold(),
        MarkdownEditorCommand::Italic => buffer.toggle_italic(),
        MarkdownEditorCommand::InlineCode => buffer.toggle_code(),
    }
}

fn block_active(buffer: &RichTextBuffer, target: BlockKind) -> bool {
    let selection = buffer.selection();
    let first = line_at_offset(buffer.text(), selection.start);
    let last_offset = if selection.end > selection.start {
        previous_grapheme_boundary(buffer.text(), selection.end)
    } else {
        selection.end
    };
    let last = line_at_offset(buffer.text(), last_offset);
    buffer.block_kinds()[first..=last]
        .iter()
        .all(|kind| *kind == target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_MAX_MARKDOWN_BYTES;

    #[test]
    fn default_configuration_exposes_the_complete_slash_catalog() {
        let config = MarkdownEditorConfig::default();

        assert_eq!(config.max_markdown_bytes, DEFAULT_MAX_MARKDOWN_BYTES);
        assert_eq!(config.slash_commands, MarkdownEditorCommand::ALL);
        assert_eq!(
            config.format_commands,
            MarkdownEditorCommand::DEFAULT_TOOLBAR
        );
    }

    #[test]
    fn configuration_limit_is_used_when_parsing_persisted_markdown() {
        let config = MarkdownEditorConfig {
            max_markdown_bytes: 5,
            ..MarkdownEditorConfig::default()
        };

        assert_eq!(config.parse_markdown("hello").unwrap().markdown(), "hello");
        assert_eq!(config.parse_markdown("hello!"), Err(MarkdownLimitExceeded));
    }

    #[test]
    fn configuration_normalization_preserves_first_command_order() {
        let mut config = MarkdownEditorConfig {
            slash_commands: vec![
                MarkdownEditorCommand::Italic,
                MarkdownEditorCommand::Bold,
                MarkdownEditorCommand::Italic,
                MarkdownEditorCommand::Bold,
                MarkdownEditorCommand::Quote,
            ],
            format_commands: vec![
                MarkdownEditorCommand::InlineCode,
                MarkdownEditorCommand::InlineCode,
            ],
            ..MarkdownEditorConfig::default()
        };
        config.normalize();

        assert_eq!(
            config.slash_commands,
            vec![
                MarkdownEditorCommand::Italic,
                MarkdownEditorCommand::Bold,
                MarkdownEditorCommand::Quote
            ]
        );
        assert_eq!(
            config.format_commands,
            vec![MarkdownEditorCommand::InlineCode]
        );
    }

    #[test]
    fn configured_command_filter_only_returns_available_matches() {
        let available = [
            MarkdownEditorCommand::Heading2,
            MarkdownEditorCommand::BulletedList,
            MarkdownEditorCommand::InlineCode,
        ];

        assert_eq!(
            filter_commands(&available, "head"),
            vec![MarkdownEditorCommand::Heading2]
        );
        assert_eq!(
            filter_commands(&available, "unordered"),
            vec![MarkdownEditorCommand::BulletedList]
        );
        assert_eq!(
            filter_commands(&available, "mono"),
            vec![MarkdownEditorCommand::InlineCode]
        );
        assert!(filter_commands(&available, "quote").is_empty());
    }

    #[test]
    fn slash_query_is_scoped_to_the_current_line_and_plain_token() {
        let buffer = RichTextBuffer::from_trusted_markdown("first\n/head");

        assert_eq!(slash_query(&buffer), Some((6..11, "head".to_owned())));

        let spaced = RichTextBuffer::from_trusted_markdown("/heading one");
        assert_eq!(slash_query(&spaced), None);

        let mut selected = RichTextBuffer::from_trusted_markdown("/head");
        selected.select_all();
        assert_eq!(slash_query(&selected), None);
    }

    #[test]
    fn applying_a_slash_command_removes_query_and_sets_block_semantics() {
        let mut buffer = RichTextBuffer::from_trusted_markdown("before\n/head");

        assert!(apply_slash_command(
            &mut buffer,
            7..12,
            MarkdownEditorCommand::Heading2
        ));
        assert_eq!(buffer.markdown(), "before\n## ");
        assert_eq!(
            buffer.block_kinds(),
            &[BlockKind::Paragraph, BlockKind::Heading2]
        );
    }

    #[test]
    fn failed_slash_command_is_transactional_at_the_byte_limit() {
        let limit = 1;
        let mut buffer = RichTextBuffer::try_from_markdown_with_limit("/", limit).unwrap();
        let original = buffer.clone();

        // A heading prefix would make the canonical Markdown exceed the limit.
        assert!(!apply_slash_command(
            &mut buffer,
            0..1,
            MarkdownEditorCommand::Heading1
        ));
        assert_eq!(buffer, original);
    }

    #[test]
    fn toolbar_commands_use_the_core_for_inline_and_list_mutations() {
        let mut buffer = RichTextBuffer::from_trusted_markdown("one\ntwo");
        buffer.select_all();

        assert!(apply_command(&mut buffer, MarkdownEditorCommand::Bold));
        assert_eq!(buffer.markdown(), "**one**\n**two**");
        assert!(apply_command(
            &mut buffer,
            MarkdownEditorCommand::BulletedList
        ));
        assert_eq!(buffer.markdown(), "- **one**\n- **two**");
    }

    #[test]
    fn block_activity_respects_a_selection_ending_at_a_line_boundary() {
        let mut buffer = RichTextBuffer::from_trusted_markdown("- one\nplain");
        buffer.set_selection(0..4, false);

        assert!(block_active(&buffer, BlockKind::Bulleted));
        assert!(!block_active(&buffer, BlockKind::Paragraph));
    }

    #[test]
    fn display_projection_hides_storage_markers_and_maps_generated_prefixes() {
        let buffer = RichTextBuffer::from_trusted_markdown("- one\n1. two\n1. three");
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
    fn native_ime_and_newline_paths_retain_core_semantics() {
        let mut ime = RichTextBuffer::from_trusted_markdown("A😀B");
        ime.set_selection(1..5, false);
        assert!(ime.replace_and_mark_utf16(None, "漢字", Some(1..2)));
        assert_eq!(ime.text(), "A漢字B");
        assert_eq!(ime.marked_range(), Some(1..7));

        let mut list = RichTextBuffer::from_trusted_markdown("- first");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n- ");
        assert!(list.insert_newline());
        assert_eq!(list.markdown(), "- first\n");
    }

    #[test]
    fn mismatched_limit_error_is_explicit() {
        let error = MarkdownEditorConfigError {
            buffer_max_markdown_bytes: 10,
            configured_max_markdown_bytes: 20,
        };

        assert!(error.to_string().contains("10"));
        assert!(error.to_string().contains("20"));
    }
}
