use gpui::{
    AnyElement, App, FontWeight, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px,
};

use crate::ThemeTokens;

/// Surface treatment for a panel container.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PanelShellStyle {
    /// A rectangular panel managed by the surrounding layout.
    #[default]
    Docked,
    /// A clipped, rounded, lightly elevated floating panel.
    Floating,
}

/// Application-neutral shell for docked and floating panels.
///
/// Placement and reveal animation remain with the caller; the shell owns
/// surface color, clipping, and elevation so these details do not drift among
/// search, comment, reference, or future extension panels.
#[derive(IntoElement)]
pub struct PanelShell {
    tokens: ThemeTokens,
    style: PanelShellStyle,
    content: AnyElement,
}

impl PanelShell {
    #[must_use]
    pub fn new(tokens: ThemeTokens, content: impl IntoElement) -> Self {
        Self {
            tokens,
            style: PanelShellStyle::Docked,
            content: content.into_any_element(),
        }
    }

    #[must_use]
    pub fn style(mut self, style: PanelShellStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn floating(self) -> Self {
        self.style(PanelShellStyle::Floating)
    }
}

impl RenderOnce for PanelShell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .occlude()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(self.tokens.surface.background)
            .when(self.style == PanelShellStyle::Floating, |shell| {
                shell
                    .rounded_xl()
                    .border_1()
                    .border_color(self.tokens.content.primary.opacity(0.13))
                    .shadow_sm()
            })
            .child(self.content)
    }
}

/// Consistent panel header with optional leading and trailing controls.
#[derive(IntoElement)]
pub struct PanelHeader {
    tokens: ThemeTokens,
    title: SharedString,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
}

impl PanelHeader {
    #[must_use]
    pub fn new(tokens: ThemeTokens, title: impl Into<SharedString>) -> Self {
        Self {
            tokens,
            title: title.into(),
            leading: None,
            trailing: None,
        }
    }

    #[must_use]
    pub fn leading(mut self, control: impl IntoElement) -> Self {
        self.leading = Some(control.into_any_element());
        self
    }

    #[must_use]
    pub fn trailing(mut self, control: impl IntoElement) -> Self {
        self.trailing = Some(control.into_any_element());
        self
    }
}

impl RenderOnce for PanelHeader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .h(px(54.0))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(self.tokens.surface.border)
            .text_color(self.tokens.content.primary)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.leading)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title),
                    ),
            )
            .children(self.trailing)
    }
}
