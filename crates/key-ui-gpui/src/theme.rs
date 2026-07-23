use gpui::{App, Global, Hsla, Rgba};
use gpui_component::{IconName, Theme};
use key_ui_core::{
    AccessibilityPreferences, BackdropEffect, ComponentConfig, DesignSystemConfig, IconRoleConfig,
    InteractionConfig, ReaderUiConfig, ResolvedDesignSystem, ResolvedGeometry, ResolvedMotion,
    ResponsiveConfig, RgbaColor, SemanticIcon, TypographyConfig,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct DesignSystemState(Arc<ResolvedDesignSystem>);

impl Global for DesignSystemState {}

pub fn install_design_system(
    cx: &mut App,
    config: &DesignSystemConfig,
    accessibility: AccessibilityPreferences,
) {
    cx.set_global(DesignSystemState(Arc::new(config.resolve(accessibility))));
    cx.refresh_windows();
}

#[must_use]
pub fn resolved_design_system(cx: &App) -> Arc<ResolvedDesignSystem> {
    if cx.has_global::<DesignSystemState>() {
        cx.global::<DesignSystemState>().0.clone()
    } else {
        Arc::new(ResolvedDesignSystem::default())
    }
}

/// Surfaces and boundaries used to establish UI elevation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceTokens {
    pub chrome: Hsla,
    pub background: Hsla,
    pub muted: Hsla,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub split_gutter: Hsla,
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
    pub geometry: ResolvedGeometry,
    pub materials: MaterialTokens,
    pub motion: ResolvedMotion,
    pub responsive: ResponsiveConfig,
    pub typography: TypographyConfig,
    pub interaction: InteractionConfig,
    pub components: ComponentConfig,
    pub reader: ReaderUiConfig,
    pub icons: IconRoleConfig,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialToken {
    pub background: Hsla,
    pub border: Hsla,
    pub highlight: Hsla,
    pub shadow_opacity: f32,
    pub backdrop: BackdropEffect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialTokens {
    pub window: MaterialToken,
    pub chrome: MaterialToken,
    pub surface: MaterialToken,
    pub floating: MaterialToken,
    pub control: MaterialToken,
}

impl ThemeTokens {
    /// Resolves the stable semantic token set from the active component theme.
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        Self::from_theme_and_system(theme, &ResolvedDesignSystem::default())
    }

    #[must_use]
    pub fn from_app(cx: &App) -> Self {
        let system = resolved_design_system(cx);
        Self::from_theme_and_system(Theme::global(cx), &system)
    }

    fn from_theme_and_system(theme: &Theme, system: &ResolvedDesignSystem) -> Self {
        let colors = system.appearance.colors;
        let chrome = configured_color(colors.surface_chrome).unwrap_or_else(|| {
            layered_surface(
                theme.title_bar,
                theme.foreground,
                system.palette.chrome_foreground_mix,
            )
        });
        let canvas = configured_color(colors.surface_canvas).unwrap_or_else(|| {
            layered_surface(
                theme.background,
                theme.foreground,
                system.palette.canvas_foreground_mix,
            )
        });
        let sidebar = configured_color(colors.surface_sidebar).unwrap_or_else(|| {
            layered_surface(
                theme.background,
                theme.foreground,
                system.palette.sidebar_foreground_mix,
            )
        });
        let split_gutter = configured_color(colors.surface_split_gutter).unwrap_or_else(|| {
            layered_surface(
                theme.background,
                theme.foreground,
                system.palette.split_gutter_foreground_mix,
            )
        });
        let background = configured_color(colors.surface_background).unwrap_or(theme.background);
        let popover = configured_color(colors.surface_popover).unwrap_or(theme.popover);
        let border = configured_color(colors.surface_border).unwrap_or(theme.border);
        let primary = configured_color(colors.content_primary).unwrap_or(theme.foreground);
        let secondary =
            configured_color(colors.content_secondary).unwrap_or(theme.secondary_foreground);
        let tertiary = configured_color(colors.content_tertiary).unwrap_or(theme.muted_foreground);
        let accent = configured_color(colors.action_accent).unwrap_or(theme.primary);
        let material = |background: Hsla, value: key_ui_core::ResolvedMaterial| MaterialToken {
            background: background.opacity(value.opacity),
            border: border.opacity(value.border_opacity),
            highlight: primary.opacity(value.highlight_opacity),
            shadow_opacity: value.shadow_opacity,
            backdrop: value.backdrop,
        };
        Self {
            surface: SurfaceTokens {
                chrome,
                background,
                muted: theme.muted,
                canvas,
                sidebar,
                split_gutter,
                overlay: popover,
                border,
            },
            content: ContentTokens {
                primary,
                secondary,
                tertiary,
                on_accent: theme.primary_foreground,
            },
            action: ActionTokens {
                control: theme.secondary,
                control_hover: theme.secondary_hover,
                control_pressed: theme.secondary_active,
                accent,
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
            geometry: system.geometry,
            materials: MaterialTokens {
                window: material(theme.background, system.materials.window),
                chrome: material(chrome, system.materials.chrome),
                surface: material(theme.background, system.materials.surface),
                floating: material(theme.popover, system.materials.floating),
                control: material(theme.secondary, system.materials.control),
            },
            motion: system.motion,
            responsive: system.responsive,
            typography: system.typography,
            interaction: system.interaction,
            components: system.components,
            reader: system.reader,
            icons: system.appearance.icons,
        }
    }
}

#[must_use]
pub fn configured_color(color: Option<RgbaColor>) -> Option<Hsla> {
    color.map(|color| {
        Rgba {
            r: color.red,
            g: color.green,
            b: color.blue,
            a: color.alpha,
        }
        .into()
    })
}

#[must_use]
pub fn semantic_icon(icon: SemanticIcon) -> IconName {
    match icon {
        SemanticIcon::Add => IconName::Plus,
        SemanticIcon::Close => IconName::Close,
        SemanticIcon::Document => IconName::File,
        SemanticIcon::Settings => IconName::Settings,
        SemanticIcon::Search => IconName::Search,
        SemanticIcon::Sidebar => IconName::PanelLeft,
        SemanticIcon::TabList => IconName::ChevronDown,
        SemanticIcon::Split => IconName::Frame,
        SemanticIcon::Folder => IconName::Folder,
        SemanticIcon::Swap => IconName::Replace,
        SemanticIcon::Separate => IconName::GalleryVerticalEnd,
        SemanticIcon::CloseLeft => IconName::PanelLeftClose,
        SemanticIcon::CloseRight => IconName::PanelRightClose,
        SemanticIcon::Previous => IconName::ChevronLeft,
        SemanticIcon::Next => IconName::ChevronRight,
        SemanticIcon::Collapse => IconName::ChevronUp,
        SemanticIcon::Expand => IconName::ChevronDown,
        SemanticIcon::ExternalLink => IconName::ExternalLink,
        SemanticIcon::Book => IconName::BookOpen,
        SemanticIcon::Globe => IconName::Globe,
        SemanticIcon::User => IconName::CircleUser,
        SemanticIcon::Error => IconName::CircleX,
        SemanticIcon::Info => IconName::Info,
        SemanticIcon::Terminal => IconName::SquareTerminal,
        SemanticIcon::Dashboard => IconName::LayoutDashboard,
        SemanticIcon::Loader => IconName::LoaderCircle,
        SemanticIcon::ThemeLight => IconName::Sun,
        SemanticIcon::ThemeDark => IconName::Moon,
        SemanticIcon::ZoomOut => IconName::Minus,
        SemanticIcon::ZoomIn => IconName::Plus,
        SemanticIcon::FitWidth => IconName::Maximize,
        SemanticIcon::Comments => IconName::PanelRight,
        SemanticIcon::Highlight => IconName::Asterisk,
        SemanticIcon::Copy => IconName::Copy,
        SemanticIcon::Check => IconName::Check,
        SemanticIcon::Menu => IconName::Menu,
    }
}

fn layered_surface(base: Hsla, foreground: Hsla, opacity: f32) -> Hsla {
    let base = Rgba::from(base);
    let mut foreground = Rgba::from(foreground);
    foreground.a = opacity;
    let mut result = base.blend(foreground);
    result.a = 1.0;
    result.into()
}

#[cfg(test)]
mod tests {
    use super::ThemeTokens;
    use gpui_component::{Theme, ThemeColor, ThemeMode};
    use key_ui_core::{AccessibilityPreferences, DesignSystemConfig, RgbaColor};

    #[test]
    fn semantic_tokens_track_light_and_dark_component_themes() {
        let light = Theme::from(ThemeColor::light().as_ref());
        let mut dark = Theme::from(ThemeColor::dark().as_ref());
        dark.mode = ThemeMode::Dark;
        let light_tokens = ThemeTokens::from_theme(&light);
        let dark_tokens = ThemeTokens::from_theme(&dark);

        assert_eq!(light_tokens.surface.background, light.background);
        assert_eq!(light_tokens.surface.overlay, light.popover);
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

    #[test]
    fn typed_semantic_color_overrides_replace_theme_roles() {
        let theme = Theme::from(ThemeColor::light().as_ref());
        let mut config = DesignSystemConfig::default();
        let accent = RgbaColor {
            red: 0.12,
            green: 0.34,
            blue: 0.78,
            alpha: 0.66,
        };
        config.appearance.colors.action_accent = Some(accent);
        let system = config.resolve(AccessibilityPreferences::default());
        let tokens = ThemeTokens::from_theme_and_system(&theme, &system);
        assert_eq!(tokens.action.accent.a, 0.66);
        assert_ne!(tokens.action.accent, theme.primary);
    }
}
