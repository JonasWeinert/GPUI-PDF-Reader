use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString, StyledText,
    TextLayout, TextRun, UTF16Selection, UnderlineStyle, Window, actions, canvas, div, font, point,
    prelude::*, px, quad, size,
};
use std::ops::Range;
use std::{error::Error, fmt};

use key_ui_gpui::{DesignStyled as _, RadiusRole};

use crate::theme::ReaderPalette;
use crate::{EditCopy, EditCut, EditPaste, EditSelectAll};

actions!(
    text_field,
    [
        FieldBackspace,
        FieldDelete,
        FieldLeft,
        FieldRight,
        FieldSelectLeft,
        FieldSelectRight,
        FieldSelectAll,
        FieldHome,
        FieldEnd,
        FieldCopy,
        FieldCut,
        FieldPaste,
        FieldSubmit,
        FieldCancel,
    ]
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextFieldEvent {
    Changed(String),
    Rejected(TextFieldRejection),
    Submit,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextFieldRejection {
    TooLong {
        limit_bytes: usize,
        attempted_bytes: usize,
    },
}

impl fmt::Display for TextFieldRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong {
                limit_bytes,
                attempted_bytes,
            } => write!(
                formatter,
                "Search text would be {attempted_bytes} bytes; the {limit_bytes}-byte limit was reached and the edit was ignored"
            ),
        }
    }
}

impl Error for TextFieldRejection {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TextFieldBuffer {
    text: String,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
}

impl TextFieldBuffer {
    fn new(text: &str) -> Self {
        let text = single_line(text);
        let end = text.len();
        Self {
            text,
            selection: end..end,
            reversed: false,
            marked: None,
        }
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn move_to(&mut self, offset: usize) {
        let offset = floor_boundary(&self.text, offset.min(self.text.len()));
        self.selection = offset..offset;
        self.reversed = false;
    }

    fn select_to(&mut self, offset: usize) {
        let offset = floor_boundary(&self.text, offset.min(self.text.len()));
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        if self.selection.is_empty() {
            self.reversed = false;
        }
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
        max_bytes: usize,
    ) -> Result<Range<usize>, TextFieldRejection> {
        let start = floor_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_boundary(&self.text, range.end.min(self.text.len())).max(start);
        let retained_bytes = self.text.len() - (end - start);
        let Some(attempted_bytes) = retained_bytes.checked_add(text.len()) else {
            return Err(TextFieldRejection::TooLong {
                limit_bytes: max_bytes,
                attempted_bytes: usize::MAX,
            });
        };
        if attempted_bytes > max_bytes {
            return Err(TextFieldRejection::TooLong {
                limit_bytes: max_bytes,
                attempted_bytes,
            });
        }

        // CR and LF are one-byte scalars, just like the spaces that replace
        // them, so the validated byte count is also the final byte count.
        let inserted = single_line(text);
        debug_assert_eq!(inserted.len(), text.len());
        self.text.replace_range(start..end, &inserted);
        let inserted_range = start..start + inserted.len();
        self.move_to(inserted_range.end);
        self.marked = None;
        Ok(inserted_range)
    }

    fn replace_selection(
        &mut self,
        text: &str,
        max_bytes: usize,
    ) -> Result<Range<usize>, TextFieldRejection> {
        self.replace(self.selection.clone(), text, max_bytes)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..floor_boundary(&self.text, offset.min(self.text.len()))]
            .encode_utf16()
            .count()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        for (index, value) in self.text.char_indices() {
            if units >= offset {
                return index;
            }
            let next = units + value.len_utf16();
            if next > offset {
                return index;
            }
            units = next;
        }
        self.text.len()
    }

    fn range_to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

pub struct TextField {
    focus_handle: FocusHandle,
    buffer: TextFieldBuffer,
    placeholder: SharedString,
    layout: TextLayout,
    selecting: bool,
    max_bytes: usize,
    borderless: bool,
}

impl TextField {
    pub fn new(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        max_bytes: usize,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: TextFieldBuffer::new(""),
            placeholder: placeholder.into(),
            layout: TextLayout::default(),
            selecting: false,
            max_bytes,
            borderless: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.buffer.text
    }

    pub fn set_text(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), TextFieldRejection> {
        match self
            .buffer
            .replace(0..self.buffer.text.len(), text, self.max_bytes)
        {
            Ok(_) => {
                self.changed(cx);
                Ok(())
            }
            Err(rejection) => {
                self.rejected(rejection, cx);
                Err(rejection)
            }
        }
    }

    pub fn set_borderless(&mut self, borderless: bool, cx: &mut Context<Self>) {
        if self.borderless != borderless {
            self.borderless = borderless;
            cx.notify();
        }
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Changed(self.buffer.text.clone()));
        cx.notify();
    }

    fn rejected(&self, rejection: TextFieldRejection, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Rejected(rejection));
        cx.notify();
    }

    fn backspace(&mut self, _: &FieldBackspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.selection.is_empty() {
            let previous = previous_boundary(&self.buffer.text, self.buffer.cursor());
            self.buffer.select_to(previous);
        }
        match self.buffer.replace_selection("", self.max_bytes) {
            Ok(_) => self.changed(cx),
            Err(rejection) => self.rejected(rejection, cx),
        }
        cx.stop_propagation();
    }

    fn delete(&mut self, _: &FieldDelete, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.selection.is_empty() {
            let next = next_boundary(&self.buffer.text, self.buffer.cursor());
            self.buffer.select_to(next);
        }
        match self.buffer.replace_selection("", self.max_bytes) {
            Ok(_) => self.changed(cx),
            Err(rejection) => self.rejected(rejection, cx),
        }
        cx.stop_propagation();
    }

    fn left(&mut self, _: &FieldLeft, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.buffer.selection.is_empty() {
            previous_boundary(&self.buffer.text, self.buffer.cursor())
        } else {
            self.buffer.selection.start
        };
        self.buffer.move_to(offset);
        cx.notify();
        cx.stop_propagation();
    }

    fn right(&mut self, _: &FieldRight, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.buffer.selection.is_empty() {
            next_boundary(&self.buffer.text, self.buffer.cursor())
        } else {
            self.buffer.selection.end
        };
        self.buffer.move_to(offset);
        cx.notify();
        cx.stop_propagation();
    }

    fn select_left(&mut self, _: &FieldSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let offset = previous_boundary(&self.buffer.text, self.buffer.cursor());
        self.buffer.select_to(offset);
        cx.notify();
        cx.stop_propagation();
    }

    fn select_right(&mut self, _: &FieldSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let offset = next_boundary(&self.buffer.text, self.buffer.cursor());
        self.buffer.select_to(offset);
        cx.notify();
        cx.stop_propagation();
    }

    fn select_all(&mut self, _: &FieldSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.selection = 0..self.buffer.text.len();
        self.buffer.reversed = false;
        cx.notify();
        cx.stop_propagation();
    }

    fn edit_select_all(&mut self, _: &EditSelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(&FieldSelectAll, window, cx);
    }

    fn home(&mut self, _: &FieldHome, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to(0);
        cx.notify();
        cx.stop_propagation();
    }

    fn end(&mut self, _: &FieldEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to(self.buffer.text.len());
        cx.notify();
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &FieldCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text[self.buffer.selection.clone()].to_owned(),
            ));
        }
        cx.stop_propagation();
    }

    fn edit_copy(&mut self, _: &EditCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&FieldCopy, window, cx);
    }

    fn cut(&mut self, _: &FieldCut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.text[self.buffer.selection.clone()].to_owned(),
            ));
            match self.buffer.replace_selection("", self.max_bytes) {
                Ok(_) => self.changed(cx),
                Err(rejection) => self.rejected(rejection, cx),
            }
        }
        cx.stop_propagation();
    }

    fn edit_cut(&mut self, _: &EditCut, window: &mut Window, cx: &mut Context<Self>) {
        self.cut(&FieldCut, window, cx);
    }

    fn paste(&mut self, _: &FieldPaste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            match self.buffer.replace_selection(&text, self.max_bytes) {
                Ok(_) => self.changed(cx),
                Err(rejection) => self.rejected(rejection, cx),
            }
        }
        cx.stop_propagation();
    }

    fn edit_paste(&mut self, _: &EditPaste, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(&FieldPaste, window, cx);
    }

    fn submit(&mut self, _: &FieldSubmit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Submit);
        cx.stop_propagation();
    }

    fn cancel(&mut self, _: &FieldCancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Cancel);
        cx.stop_propagation();
    }

    fn offset_for_position(&self, position: Point<Pixels>) -> usize {
        self.layout
            .index_for_position(position)
            .unwrap_or_else(|closest| closest)
            .min(self.buffer.text.len())
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let offset = self.offset_for_position(event.position);
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
                .select_to(self.offset_for_position(event.position));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        cx.notify();
    }
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(range.clone()));
        Some(self.buffer.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(self.buffer.selection.clone()),
            reversed: self.buffer.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .clone()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.buffer.range_from_utf16(range))
            .or_else(|| self.buffer.marked.clone())
            .unwrap_or_else(|| self.buffer.selection.clone());
        match self.buffer.replace(range, text, self.max_bytes) {
            Ok(_) => self.changed(cx),
            Err(rejection) => self.rejected(rejection, cx),
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.buffer.range_from_utf16(range))
            .or_else(|| self.buffer.marked.clone())
            .unwrap_or_else(|| self.buffer.selection.clone());
        let inserted = match self.buffer.replace(range, text, self.max_bytes) {
            Ok(inserted) => inserted,
            Err(rejection) => {
                self.rejected(rejection, cx);
                return;
            }
        };
        if !inserted.is_empty() {
            self.buffer.marked = Some(inserted.clone());
        }
        if let Some(selected) = selected_utf16 {
            let inserted_text = &self.buffer.text[inserted.clone()];
            let relative = TextFieldBuffer::new(inserted_text).range_from_utf16(selected);
            self.buffer.selection = inserted.start + relative.start..inserted.start + relative.end;
        }
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let offset = self.buffer.offset_from_utf16(range_utf16.end);
        let position = self
            .layout
            .position_for_index(offset)
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
                .offset_to_utf16(self.offset_for_position(position)),
        )
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = ReaderPalette::from_app(cx);
        let focused = self.focus_handle.is_focused(window);
        let display: SharedString = if self.buffer.text.is_empty() {
            " ".into()
        } else {
            self.buffer.text.clone().into()
        };
        let runs = field_runs(&self.buffer, display.len(), palette);
        let styled = StyledText::new(display).with_runs(runs);
        self.layout = styled.layout().clone();

        let entity: Entity<Self> = cx.entity();
        let focus = self.focus_handle.clone();
        let caret = palette.accent;
        let overlay = canvas(
            |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                let field = entity.read(cx);
                if focus.is_focused(window) {
                    let position = field
                        .layout
                        .position_for_index(field.buffer.cursor())
                        .unwrap_or(bounds.origin + point(px(1.0), px(1.0)));
                    window.paint_quad(quad(
                        Bounds::new(position, size(px(1.5), field.layout.line_height())),
                        px(0.0),
                        caret,
                        px(0.0),
                        gpui::transparent_black(),
                        Default::default(),
                    ));
                }
            },
        )
        .absolute()
        .size_full();

        div()
            .id("search-text-field")
            .key_context("TextField")
            .track_focus(&self.focus_handle)
            .relative()
            .h(px(36.0))
            .w_full()
            .overflow_hidden()
            .px(if self.borderless { px(0.0) } else { px(12.0) })
            .flex()
            .items_center()
            .when(!self.borderless, |field| {
                field
                    .design_radius(RadiusRole::Medium, &palette.ui)
                    .border_1()
                    .border_color(if focused {
                        palette.accent
                    } else {
                        palette.separator
                    })
                    .bg(palette.surface_subtle)
            })
            .text_color(palette.text)
            .text_sm()
            .line_height(px(22.0))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
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
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when(self.buffer.text.is_empty(), |element| {
                element.child(
                    div()
                        .absolute()
                        .left(if self.borderless { px(0.0) } else { px(12.0) })
                        .text_color(palette.text_tertiary)
                        .child(self.placeholder.clone()),
                )
            })
            .child(styled)
            .child(overlay)
    }
}

fn field_runs(
    buffer: &TextFieldBuffer,
    display_len: usize,
    palette: ReaderPalette,
) -> Vec<TextRun> {
    if buffer.text.is_empty() {
        return vec![TextRun {
            len: display_len,
            font: font(".SystemUIFont"),
            color: palette.surface.opacity(0.0),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    }
    let mut boundaries = vec![
        0,
        buffer.text.len(),
        buffer.selection.start,
        buffer.selection.end,
    ];
    if let Some(marked) = &buffer.marked {
        boundaries.extend([marked.start, marked.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .windows(2)
        .filter_map(|pair| {
            let range = pair[0]..pair[1];
            (!range.is_empty()).then(|| TextRun {
                len: range.len(),
                font: font(".SystemUIFont"),
                color: palette.text,
                background_color: (!buffer.selection.is_empty()
                    && range.start < buffer.selection.end
                    && range.end > buffer.selection.start)
                    .then_some(palette.selection),
                underline: buffer
                    .marked
                    .as_ref()
                    .is_some_and(|marked| range.start < marked.end && range.end > marked.start)
                    .then_some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(palette.accent),
                        wavy: false,
                    }),
                strikethrough: None,
            })
        })
        .collect()
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|value| {
            if value == '\r' || value == '\n' {
                ' '
            } else {
                value
            }
        })
        .collect()
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn previous_boundary(text: &str, offset: usize) -> usize {
    text[..floor_boundary(text, offset.min(text.len()))]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    let offset = floor_boundary(text, offset.min(text.len()));
    text[offset..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(index, _)| offset + index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_is_utf8_safe_and_forces_single_line_text() {
        let mut buffer = TextFieldBuffer::new("a😀b");
        buffer.selection = 1..5;
        buffer.replace_selection("Ω\n你", usize::MAX).unwrap();
        assert_eq!(buffer.text, "aΩ 你b");
        assert_eq!(buffer.cursor(), "aΩ 你".len());
    }

    #[test]
    fn utf16_offsets_round_trip_at_scalar_boundaries() {
        let buffer = TextFieldBuffer::new("a😀Ω");
        for offset in [0, 1, 5, 7] {
            assert_eq!(
                buffer.offset_from_utf16(buffer.offset_to_utf16(offset)),
                offset
            );
        }
        assert_eq!(buffer.offset_from_utf16(2), 1);
    }

    #[test]
    fn reverse_selection_and_character_navigation_are_stable() {
        let mut buffer = TextFieldBuffer::new("a😀b");
        buffer.move_to(buffer.text.len());
        buffer.select_to(previous_boundary(&buffer.text, buffer.cursor()));
        buffer.select_to(previous_boundary(&buffer.text, buffer.cursor()));
        assert_eq!(&buffer.text[buffer.selection.clone()], "😀b");
        assert!(buffer.reversed);
        buffer.select_to(buffer.text.len());
        assert!(buffer.selection.is_empty());
        assert!(!buffer.reversed);
    }

    #[test]
    fn marked_replacement_tracks_inserted_range() {
        let mut buffer = TextFieldBuffer::new("abc");
        let inserted = buffer.replace(1..2, "😀", usize::MAX).unwrap();
        buffer.marked = Some(inserted.clone());
        assert_eq!(&buffer.text[inserted], "😀");
        assert_eq!(buffer.text, "a😀c");
    }

    #[test]
    fn byte_limit_accepts_exact_multibyte_boundary_and_rejects_the_next_scalar() {
        let mut buffer = TextFieldBuffer::new("");
        let inserted = buffer.replace(0..0, "😀你", 7).unwrap();
        assert_eq!(inserted, 0..7);
        assert_eq!(buffer.text, "😀你");

        let before = buffer.clone();
        assert_eq!(
            buffer.replace(7..7, "é", 8),
            Err(TextFieldRejection::TooLong {
                limit_bytes: 8,
                attempted_bytes: 9,
            })
        );
        assert_eq!(buffer, before);
    }

    #[test]
    fn ignored_search_scalars_still_count_toward_the_raw_byte_limit() {
        let ignored = "\0\u{00ad}\0";
        assert_eq!(ignored.len(), 4);

        let mut buffer = TextFieldBuffer::new("");
        buffer.replace(0..0, ignored, 4).unwrap();
        let before = buffer.clone();
        assert_eq!(
            buffer.replace(4..4, "x", 4),
            Err(TextFieldRejection::TooLong {
                limit_bytes: 4,
                attempted_bytes: 5,
            })
        );
        assert_eq!(buffer, before);
    }

    #[test]
    fn replacement_accounts_for_removed_bytes_before_enforcing_the_limit() {
        let mut buffer = TextFieldBuffer::new("12345678");
        buffer.selection = 0..4;
        let inserted = buffer.replace_selection("😀", 8).unwrap();
        assert_eq!(inserted, 0..4);
        assert_eq!(buffer.text, "😀5678");
        assert_eq!(buffer.cursor(), 4);
    }

    #[test]
    fn rejected_replacement_preserves_selection_direction_and_marked_text() {
        let mut buffer = TextFieldBuffer::new("a😀b");
        buffer.selection = 1..5;
        buffer.reversed = true;
        buffer.marked = Some(1..5);
        let before = buffer.clone();

        let rejection = buffer.replace(1..5, "😀😀", 6).unwrap_err();
        assert_eq!(
            rejection,
            TextFieldRejection::TooLong {
                limit_bytes: 6,
                attempted_bytes: 10,
            }
        );
        assert_eq!(buffer, before);
        assert!(rejection.to_string().contains("edit was ignored"));
    }
}
