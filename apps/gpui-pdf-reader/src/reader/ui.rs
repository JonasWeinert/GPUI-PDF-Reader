use crate::theme::ReaderPalette;
use gpui::{App, ClickEvent, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px};
use gpui_component::{Icon, IconName};
pub(super) use key_ui_gpui::ChromeButtonStyle;

/// Shared button chrome used by classic, fluid, and floating reader controls.
pub(super) fn chrome_button(
    palette: ReaderPalette,
    id: &'static str,
    label: impl IntoElement,
    style: ChromeButtonStyle,
    enabled: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    key_ui_gpui::chrome_button(palette.ui, id, label, style, enabled, handler)
}

pub(super) fn icon_label(icon: IconName, label: impl IntoElement) -> gpui::AnyElement {
    key_ui_gpui::icon_label(icon, label).into_any_element()
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
