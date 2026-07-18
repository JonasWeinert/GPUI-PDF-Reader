use gpui::{AnyElement, App, IntoElement, RenderOnce, Window, div, prelude::*, px};

use crate::ThemeTokens;

pub const CONTEXT_BAR_HEIGHT: f32 = 44.0;

/// Window-level row whose contents are supplied by the command-active view.
///
/// The shell deliberately knows nothing about URLs, PDFs, media, or editors.
/// It establishes stable geometry and theme treatment for navigation,
/// location, split controls, and future extension-contributed actions.
#[derive(IntoElement)]
pub struct WorkspaceContextBar {
    tokens: ThemeTokens,
    leading: Vec<AnyElement>,
    center: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
}

impl WorkspaceContextBar {
    #[must_use]
    pub fn new(tokens: ThemeTokens) -> Self {
        Self {
            tokens,
            leading: Vec::new(),
            center: Vec::new(),
            trailing: Vec::new(),
        }
    }

    #[must_use]
    pub fn leading(mut self, item: impl IntoElement) -> Self {
        self.leading.push(item.into_any_element());
        self
    }

    #[must_use]
    pub fn center(mut self, item: impl IntoElement) -> Self {
        self.center.push(item.into_any_element());
        self
    }

    #[must_use]
    pub fn trailing(mut self, item: impl IntoElement) -> Self {
        self.trailing.push(item.into_any_element());
        self
    }
}

impl RenderOnce for WorkspaceContextBar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .h(px(CONTEXT_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(self.tokens.surface.border.opacity(0.72))
            .bg(self.tokens.surface.background)
            .text_color(self.tokens.content.primary)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(self.leading),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .children(self.center),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(self.trailing),
            )
    }
}
