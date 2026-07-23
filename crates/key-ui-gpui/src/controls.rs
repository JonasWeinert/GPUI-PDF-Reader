use gpui::{App, ClickEvent, ElementId, IntoElement, Window, div, prelude::*, px};
use gpui_component::{Icon, IconName};

use crate::{
    DesignStyled, ElevationRole, InteractionState, ThemeTokens, TypographyRole, semantic_icon,
};

/// Visual treatments shared by compact toolbar and panel controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeButtonStyle {
    /// Blends into title-bar chrome until interaction.
    Ghost,
    /// Blends into a floating surface until interaction.
    Floating,
    /// Shows a persistent accent selection.
    Selected,
    /// Indicates an active chrome mode without the stronger outlined treatment
    /// used for selected rows and primary segmented controls.
    SubtleSelected,
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
            tokens.materials.chrome.background,
            tokens.materials.chrome.background,
            tokens.content.secondary,
            tokens.action.control_hover,
            tokens.action.control_pressed,
        ),
        ChromeButtonStyle::Floating => (
            tokens.materials.floating.background,
            tokens.materials.floating.border,
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
        ChromeButtonStyle::SubtleSelected => (
            tokens.action.accent_soft,
            tokens.action.accent_soft,
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
    let hovered_state = InteractionState {
        hovered: true,
        ..InteractionState::default()
    };
    let pressed_state = InteractionState {
        pressed: true,
        ..InteractionState::default()
    };
    let (hover, pressed) = if matches!(
        style,
        ChromeButtonStyle::Ghost | ChromeButtonStyle::Floating
    ) {
        (
            hover.opacity(*tokens.interaction.surface_opacity.resolve(hovered_state)),
            pressed.opacity(*tokens.interaction.surface_opacity.resolve(pressed_state)),
        )
    } else {
        (hover, pressed)
    };
    let border = if matches!(
        style,
        ChromeButtonStyle::Selected | ChromeButtonStyle::SubtleSelected
    ) {
        border.opacity(
            *tokens.interaction.border_opacity.resolve(InteractionState {
                selected: true,
                ..InteractionState::default()
            }),
        )
    } else {
        border
    };

    div()
        .id(id)
        .h(px(tokens.geometry.control_height))
        .min_w(px(tokens.geometry.control_height))
        .px(px(tokens.geometry.space_unit * 3.0))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .design_corners(tokens.components.corners.button)
        .border_1()
        .border_color(border)
        .bg(background)
        .text_color(text)
        .design_typography(TypographyRole::Label, &tokens)
        .design_elevation(ElevationRole::Surface, &tokens)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(hover))
                .active(move |button| button.bg(pressed))
                .on_click(handler)
        })
        .when(!enabled, |button| {
            button.design_interaction(
                InteractionState {
                    disabled: true,
                    ..InteractionState::default()
                },
                &tokens,
            )
        })
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
        Icon::new(icon).size(px(tokens.geometry.icon_size)),
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
        semantic_icon(tokens.icons.close),
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
