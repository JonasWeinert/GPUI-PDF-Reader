use crate::theme::ReaderPalette;
use gpui::{AnyElement, App, IntoElement, RenderOnce, Window, div, prelude::*};
use key_ui_gpui::{DesignStyled as _, ElevationRole, RadiusRole};

/// Shared visual shell for transient, floating side panels.
///
/// The caller owns placement and reveal animation; this component owns the
/// clipping, surface, border, and safe inset shared by every floating panel.
#[derive(IntoElement)]
pub struct FloatingPanel {
    palette: ReaderPalette,
    content: AnyElement,
}

impl FloatingPanel {
    pub fn new(palette: ReaderPalette, content: impl IntoElement) -> Self {
        Self {
            palette,
            content: content.into_any_element(),
        }
    }
}

impl RenderOnce for FloatingPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .occlude()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .design_radius(RadiusRole::Large, &self.palette.ui)
            .border_1()
            .border_color(self.palette.text.opacity(0.13))
            .bg(self.palette.surface)
            .design_elevation(ElevationRole::Surface, &self.palette.ui)
            .child(self.content)
    }
}
