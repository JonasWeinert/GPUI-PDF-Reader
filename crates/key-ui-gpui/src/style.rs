use gpui::{BoxShadow, FontWeight, Styled, black, point, px, rems};

use crate::{
    CornerShape, CornerSpec, FontWeightToken, InteractionState, TextStyleConfig, ThemeTokens,
};

/// Semantic corner sizes. Feature views select intent; the resolved design
/// system supplies the actual value and can globally remove curvature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadiusRole {
    Small,
    Medium,
    Large,
    Pill,
}

/// Semantic elevation levels. A root shadow policy can suppress all of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElevationRole {
    Surface,
    Floating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypographyRole {
    Caption,
    Label,
    Body,
    Heading,
    Title,
    Display,
}

/// Parameterized styling operations shared by every GPUI feature view.
pub trait DesignStyled: Styled + Sized {
    fn design_radius(self, role: RadiusRole, tokens: &ThemeTokens) -> Self {
        let radius = match role {
            RadiusRole::Small => tokens.geometry.radius_small,
            RadiusRole::Medium => tokens.geometry.radius_medium,
            RadiusRole::Large => tokens.geometry.radius_large,
            RadiusRole::Pill => tokens.geometry.radius_pill,
        };
        self.rounded(px(radius))
    }

    fn design_elevation(self, role: ElevationRole, tokens: &ThemeTokens) -> Self {
        self.design_elevation_with_strength(role, 1.0, tokens)
    }

    fn design_elevation_with_strength(
        self,
        role: ElevationRole,
        strength: f32,
        tokens: &ThemeTokens,
    ) -> Self {
        let (opacity, y, blur, spread) = match role {
            ElevationRole::Surface => (
                tokens.materials.surface.shadow_opacity,
                tokens.components.common.shadow_surface_y,
                tokens.components.common.shadow_surface_blur,
                tokens.components.common.shadow_surface_spread,
            ),
            ElevationRole::Floating => (
                tokens.materials.floating.shadow_opacity,
                tokens.components.common.shadow_floating_y,
                tokens.components.common.shadow_floating_blur,
                tokens.components.common.shadow_floating_spread,
            ),
        };
        let opacity = opacity * strength.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            self
        } else {
            self.shadow(vec![BoxShadow {
                color: black().opacity(opacity),
                offset: point(px(0.0), px(y)),
                blur_radius: px(blur),
                spread_radius: px(spread),
            }])
        }
    }

    /// Applies independently configured corners after root policy resolution.
    /// The vendored GPUI renderer interprets negative radii as concave cutouts.
    fn design_corners(self, corners: CornerSpec) -> Self {
        self.rounded_tl(px(signed_radius(corners.top_left)))
            .rounded_tr(px(signed_radius(corners.top_right)))
            .rounded_br(px(signed_radius(corners.bottom_right)))
            .rounded_bl(px(signed_radius(corners.bottom_left)))
    }

    fn design_typography(self, role: TypographyRole, tokens: &ThemeTokens) -> Self {
        let style = match role {
            TypographyRole::Caption => tokens.typography.caption,
            TypographyRole::Label => tokens.typography.label,
            TypographyRole::Body => tokens.typography.body,
            TypographyRole::Heading => tokens.typography.heading,
            TypographyRole::Title => tokens.typography.title,
            TypographyRole::Display => tokens.typography.display,
        };
        apply_text_style(self, style)
    }

    fn design_interaction(self, state: InteractionState, tokens: &ThemeTokens) -> Self {
        let opacity = *tokens.interaction.opacity.resolve(state);
        let surface_opacity = *tokens.interaction.surface_opacity.resolve(state);
        self.opacity(opacity)
            .bg(tokens.action.control.opacity(surface_opacity))
    }
}

fn signed_radius(shape: CornerShape) -> f32 {
    match shape {
        CornerShape::Convex { radius } => radius,
        CornerShape::Concave { radius } => -radius,
        CornerShape::Square => 0.0,
    }
}

impl<T> DesignStyled for T where T: Styled {}

fn apply_text_style<T: Styled + Sized>(element: T, style: TextStyleConfig) -> T {
    let weight = match style.weight {
        FontWeightToken::Normal => FontWeight::NORMAL,
        FontWeightToken::Medium => FontWeight::MEDIUM,
        FontWeightToken::Semibold => FontWeight::SEMIBOLD,
        FontWeightToken::Bold => FontWeight::BOLD,
    };
    element
        .text_size(rems(style.size_rem))
        .line_height(rems(style.size_rem * style.line_height))
        .font_weight(weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpui_corner_encoding_preserves_convex_square_and_concave_intent() {
        assert_eq!(signed_radius(CornerShape::Convex { radius: 12.0 }), 12.0);
        assert_eq!(signed_radius(CornerShape::Square), 0.0);
        assert_eq!(signed_radius(CornerShape::Concave { radius: 8.0 }), -8.0);
    }
}
