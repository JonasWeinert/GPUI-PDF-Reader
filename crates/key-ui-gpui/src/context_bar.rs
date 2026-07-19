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
    bottom_border: bool,
    leading: Vec<AnyElement>,
    center: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
    auxiliary: Option<AnyElement>,
    auxiliary_height: f32,
}

impl WorkspaceContextBar {
    #[must_use]
    pub fn new(tokens: ThemeTokens) -> Self {
        Self {
            tokens,
            bottom_border: true,
            leading: Vec::new(),
            center: Vec::new(),
            trailing: Vec::new(),
            auxiliary: None,
            auxiliary_height: 0.0,
        }
    }

    /// Controls whether the shell paints its lower separator. Compound views
    /// can disable it and let their active child visually join the bar.
    #[must_use]
    pub fn bottom_border(mut self, visible: bool) -> Self {
        self.bottom_border = visible;
        self
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

    /// Adds a bounded second row supplied by the active view. Its explicit
    /// height allows the owner to animate layout without absolute overlays.
    #[must_use]
    pub fn auxiliary(mut self, height: f32, item: impl IntoElement) -> Self {
        self.auxiliary_height = height.max(0.0);
        self.auxiliary = Some(item.into_any_element());
        self
    }
}

impl RenderOnce for WorkspaceContextBar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let primary = div()
            .h(px(CONTEXT_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
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
            );
        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .when(self.bottom_border, |bar| {
                bar.border_b_1()
                    .border_color(self.tokens.surface.border.opacity(0.72))
            })
            .bg(self.tokens.surface.background)
            .child(primary)
            .when_some(self.auxiliary, |bar, auxiliary| {
                bar.child(
                    div()
                        .h(px(self.auxiliary_height))
                        .w_full()
                        .flex_none()
                        .overflow_hidden()
                        .child(auxiliary),
                )
            })
    }
}
