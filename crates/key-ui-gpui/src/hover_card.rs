use gpui::{AnyElement, App, IntoElement, Pixels, RenderOnce, Window, div, prelude::*, px};

use crate::ThemeTokens;

/// Reusable elevated hover surface with arbitrary, separated content slots.
///
/// Feature code owns the meaning and layout of every section. The shell only
/// provides elevation, clipping, theme colors, and consistent separators, so
/// it can host document metadata, media previews, note state, or any future
/// tab-specific presentation without importing those domain concepts here.
#[derive(IntoElement)]
pub struct HoverCardShell {
    tokens: ThemeTokens,
    width: Pixels,
    sections: Vec<AnyElement>,
}

impl HoverCardShell {
    #[must_use]
    pub fn new(tokens: ThemeTokens, width: Pixels) -> Self {
        Self {
            tokens,
            width,
            sections: Vec::new(),
        }
    }

    #[must_use]
    pub fn section(mut self, content: impl IntoElement) -> Self {
        self.sections.push(content.into_any_element());
        self
    }
}

impl RenderOnce for HoverCardShell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let mut children = Vec::with_capacity(self.sections.len().saturating_mul(2));
        for (index, section) in self.sections.into_iter().enumerate() {
            if index > 0 {
                children.push(
                    div()
                        .h(px(1.0))
                        .mx_3()
                        .bg(self.tokens.surface.border.opacity(0.7))
                        .into_any_element(),
                );
            }
            children.push(section);
        }

        div()
            .occlude()
            .w(self.width)
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(self.tokens.content.primary.opacity(0.12))
            .shadow_lg()
            .bg(self.tokens.surface.background)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .children(children)
    }
}
