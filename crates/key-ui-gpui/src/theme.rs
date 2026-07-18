use gpui::Hsla;
use gpui_component::Theme;

/// Surfaces and boundaries used to establish UI elevation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceTokens {
    pub chrome: Hsla,
    pub background: Hsla,
    pub muted: Hsla,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub overlay: Hsla,
    pub border: Hsla,
}

/// Foreground colors for content at different emphasis levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentTokens {
    pub primary: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub on_accent: Hsla,
}

/// Interactive-control colors, including their hover and pressed states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionTokens {
    pub control: Hsla,
    pub control_hover: Hsla,
    pub control_pressed: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_pressed: Hsla,
    pub accent_soft: Hsla,
    pub accent_soft_hover: Hsla,
    pub accent_border: Hsla,
}

/// Semantic feedback colors. Soft variants are suitable for banner surfaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusTokens {
    pub danger: Hsla,
    pub danger_soft: Hsla,
    pub warning: Hsla,
    pub success: Hsla,
    pub info: Hsla,
}

/// Application-neutral semantic tokens derived entirely from a
/// `gpui-component` [`Theme`].
///
/// Feature crates can compose these with domain colors (for example PDF paper
/// or annotation colors) without duplicating theme-to-semantic mappings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeTokens {
    pub surface: SurfaceTokens,
    pub content: ContentTokens,
    pub action: ActionTokens,
    pub status: StatusTokens,
    pub selection: Hsla,
}

impl ThemeTokens {
    /// Resolves the stable semantic token set from the active component theme.
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            surface: SurfaceTokens {
                chrome: theme.title_bar,
                background: theme.background,
                muted: theme.muted,
                canvas: theme.tiles,
                sidebar: theme.sidebar,
                overlay: theme.overlay,
                border: theme.border,
            },
            content: ContentTokens {
                primary: theme.foreground,
                secondary: theme.secondary_foreground,
                tertiary: theme.muted_foreground,
                on_accent: theme.primary_foreground,
            },
            action: ActionTokens {
                control: theme.secondary,
                control_hover: theme.secondary_hover,
                control_pressed: theme.secondary_active,
                accent: theme.primary,
                accent_hover: theme.primary_hover,
                accent_pressed: theme.primary_active,
                accent_soft: theme.accent,
                accent_soft_hover: theme.list_hover,
                accent_border: theme.list_active_border,
            },
            status: StatusTokens {
                danger: theme.danger,
                danger_soft: theme.danger.opacity(0.12),
                warning: theme.warning,
                success: theme.green,
                info: theme.blue,
            },
            selection: theme.selection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ThemeTokens;
    use gpui_component::{Theme, ThemeColor, ThemeMode};

    #[test]
    fn semantic_tokens_track_light_and_dark_component_themes() {
        let light = Theme::from(ThemeColor::light().as_ref());
        let mut dark = Theme::from(ThemeColor::dark().as_ref());
        dark.mode = ThemeMode::Dark;
        let light_tokens = ThemeTokens::from_theme(&light);
        let dark_tokens = ThemeTokens::from_theme(&dark);

        assert_eq!(light_tokens.surface.background, light.background);
        assert_eq!(dark_tokens.surface.background, dark.background);
        assert_eq!(light_tokens.content.primary, light.foreground);
        assert_eq!(dark_tokens.action.accent, dark.primary);
        assert_eq!(light_tokens.status.danger, light.danger);
        assert_ne!(
            light_tokens.surface.background,
            dark_tokens.surface.background
        );
        assert_ne!(light_tokens.content.primary, dark_tokens.content.primary);
    }

    #[test]
    fn required_visual_tokens_are_not_fully_transparent() {
        for theme in [
            Theme::from(ThemeColor::light().as_ref()),
            Theme::from(ThemeColor::dark().as_ref()),
        ] {
            let tokens = ThemeTokens::from_theme(&theme);
            for token in [
                tokens.surface.background,
                tokens.surface.border,
                tokens.content.primary,
                tokens.action.accent,
                tokens.status.danger,
            ] {
                assert!(token.a > 0.0);
            }
        }
    }
}
