use crate::theme::ReaderPalette;
use gpui::{App, ClickEvent, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px};
use gpui_component::{Icon, IconName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChromeButtonStyle {
    Ghost,
    Floating,
    Selected,
    Primary,
}

/// Shared button chrome used by classic, fluid, and floating reader controls.
pub(super) fn chrome_button(
    palette: ReaderPalette,
    id: &'static str,
    label: impl IntoElement,
    style: ChromeButtonStyle,
    enabled: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (background, border, text, hover, pressed) = match style {
        ChromeButtonStyle::Ghost => (
            palette.chrome,
            palette.chrome,
            palette.text_secondary,
            palette.control_hover,
            palette.control_pressed,
        ),
        ChromeButtonStyle::Floating => (
            palette.surface,
            palette.surface,
            palette.text_secondary,
            palette.control_hover,
            palette.control_pressed,
        ),
        ChromeButtonStyle::Selected => (
            palette.accent_soft,
            palette.accent_border,
            palette.accent,
            palette.accent_soft_hover,
            palette.control_pressed,
        ),
        ChromeButtonStyle::Primary => (
            palette.accent,
            palette.accent,
            palette.accent_foreground,
            palette.accent_hover,
            palette.accent_active,
        ),
    };
    div()
        .id(id)
        .h(px(32.0))
        .min_w(px(32.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(background)
        .text_color(text)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(hover))
                .active(move |button| button.bg(pressed))
                .on_click(handler)
        })
        .when(!enabled, |button| button.opacity(0.42))
        .child(label)
}

pub(super) fn icon_label(icon: IconName, label: impl IntoElement) -> gpui::AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(Icon::new(icon))
        .child(label)
        .into_any_element()
}

/// Consistent centered empty state for side panels.
pub(super) fn empty_state(
    palette: ReaderPalette,
    icon: IconName,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> gpui::AnyElement {
    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .text_center()
        .child(
            div()
                .size(px(38.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(palette.accent_soft)
                .text_color(palette.accent)
                .text_lg()
                .child(Icon::new(icon)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.text)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(248.0))
                .text_xs()
                .line_height(px(18.0))
                .text_color(palette.text_secondary)
                .child(detail.into()),
        )
        .into_any_element()
}

pub(super) fn error_banner(palette: ReaderPalette, message: SharedString) -> gpui::AnyElement {
    div()
        .mx_4()
        .mt_3()
        .p_3()
        .rounded_md()
        .bg(palette.error_soft)
        .text_xs()
        .line_height(px(18.0))
        .text_color(palette.error)
        .child(message)
        .into_any_element()
}
