use gpui::{App, ClickEvent, ElementId, FontWeight, IntoElement, Window, div, prelude::*, px};
use gpui_component::{Icon, IconName};

use crate::ThemeTokens;

/// Visual treatments shared by compact toolbar and panel controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeButtonStyle {
    /// Blends into title-bar chrome until interaction.
    Ghost,
    /// Blends into a floating surface until interaction.
    Floating,
    /// Shows a persistent accent selection.
    Selected,
    /// Uses the primary accent as a filled call to action.
    Primary,
}

/// Builds a compact semantic button while leaving its content and action to
/// the owning feature.
pub fn chrome_button(
    tokens: ThemeTokens,
    id: impl Into<ElementId>,
    label: impl IntoElement,
    style: ChromeButtonStyle,
    enabled: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (background, border, text, hover, pressed) = match style {
        ChromeButtonStyle::Ghost => (
            tokens.surface.chrome,
            tokens.surface.chrome,
            tokens.content.secondary,
            tokens.action.control_hover,
            tokens.action.control_pressed,
        ),
        ChromeButtonStyle::Floating => (
            tokens.surface.background,
            tokens.surface.background,
            tokens.content.secondary,
            tokens.action.control_hover,
            tokens.action.control_pressed,
        ),
        ChromeButtonStyle::Selected => (
            tokens.action.accent_soft,
            tokens.action.accent_border,
            tokens.action.accent,
            tokens.action.accent_soft_hover,
            tokens.action.control_pressed,
        ),
        ChromeButtonStyle::Primary => (
            tokens.action.accent,
            tokens.action.accent,
            tokens.content.on_accent,
            tokens.action.accent_hover,
            tokens.action.accent_pressed,
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

/// Builds an icon-only compact button with a consistent 16-pixel icon.
pub fn icon_button(
    tokens: ThemeTokens,
    id: impl Into<ElementId>,
    icon: IconName,
    style: ChromeButtonStyle,
    enabled: bool,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    chrome_button(
        tokens,
        id,
        Icon::new(icon).size(px(16.0)),
        style,
        enabled,
        handler,
    )
}

/// Builds the standard close affordance used by panel headers.
pub fn close_button(
    tokens: ThemeTokens,
    id: impl Into<ElementId>,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    icon_button(
        tokens,
        id,
        IconName::Close,
        ChromeButtonStyle::Ghost,
        true,
        handler,
    )
}

/// Pairs an icon and text with the spacing used by compact buttons.
pub fn icon_label(icon: IconName, label: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(Icon::new(icon))
        .child(label)
}
