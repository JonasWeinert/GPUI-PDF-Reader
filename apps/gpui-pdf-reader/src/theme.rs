use crate::SelectTheme;
use gpui::{App, Hsla, Menu, MenuItem, Rgba, SharedString, Window};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, ThemeSet};
use std::{rc::Rc, sync::LazyLock};

const BUNDLED_THEMES_JSON: &str = include_str!("../../../assets/themes/gpui-component.json");

static BUNDLED_THEMES: LazyLock<ThemeSet> = LazyLock::new(|| {
    serde_json::from_str(BUNDLED_THEMES_JSON).expect("bundled gpui-component themes must be valid")
});

/// The reader's theme choice. `System` follows the active window appearance;
/// the explicit choices remain stable when macOS changes appearance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemePreference {
    #[default]
    System,
    Named,
}

impl ThemePreference {
    #[cfg(debug_assertions)]
    pub fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Named => "named",
        }
    }
}

pub fn bundled_themes() -> &'static [ThemeConfig] {
    &BUNDLED_THEMES.themes
}

pub fn theme_menu() -> MenuItem {
    let mode_menu = |name: &'static str, mode: ThemeMode| {
        MenuItem::submenu(Menu {
            name: name.into(),
            items: bundled_themes()
                .iter()
                .filter(|theme| theme.mode == mode)
                .map(|theme| {
                    MenuItem::action(
                        theme.name.clone(),
                        SelectTheme {
                            name: theme.name.clone(),
                        },
                    )
                })
                .collect(),
        })
    };

    MenuItem::submenu(Menu {
        name: "Theme".into(),
        items: vec![
            MenuItem::action(
                "Follow System (Default)",
                SelectTheme {
                    name: SharedString::default(),
                },
            ),
            MenuItem::separator(),
            mode_menu("Light", ThemeMode::Light),
            mode_menu("Dark", ThemeMode::Dark),
        ],
    })
}

pub fn apply_selection(name: &str, window: &mut Window, cx: &mut App) -> Option<SharedString> {
    if name.is_empty() {
        let light = ThemeRegistry::global(cx).default_light_theme().clone();
        let dark = ThemeRegistry::global(cx).default_dark_theme().clone();
        let theme = Theme::global_mut(cx);
        theme.light_theme = light;
        theme.dark_theme = dark;
        Theme::sync_system_appearance(Some(window), cx);
        return None;
    }

    let config = bundled_themes()
        .iter()
        .find(|theme| theme.name.as_ref() == name)
        .cloned()
        .map(Rc::new)?;
    let mode = config.mode;
    Theme::global_mut(cx).apply_config(&config);
    Theme::change(mode, Some(window), cx);
    Some(config.name.clone())
}

/// Returns the page backing used by both GPUI and PDFium. Dark paper is a
/// subtle, opaque lift toward the theme foreground, keeping the page visibly
/// separate from the workspace without introducing an unrelated color.
pub fn pdf_paper_color(theme: &Theme, forced_dark: bool) -> Hsla {
    if !forced_dark || !theme.is_dark() {
        return gpui_component::ThemeColor::light().background;
    }

    let background = Rgba::from(theme.background);
    let mut foreground = Rgba::from(theme.foreground);
    foreground.a = 0.06;
    let mut paper = background.blend(foreground);
    paper.a = 1.0;
    paper.into()
}

pub fn pdf_paper_border(theme: &Theme, forced_dark: bool) -> Hsla {
    if forced_dark && theme.is_dark() {
        theme.border
    } else {
        gpui_component::ThemeColor::light().border
    }
}

/// Semantic colors used by the PDF workspace. Every value is sourced from the
/// active gpui-component theme; alpha changes preserve that theme's hue.
#[derive(Clone, Copy, Debug)]
pub struct ReaderPalette {
    pub chrome: Hsla,
    pub surface: Hsla,
    pub surface_subtle: Hsla,
    pub control: Hsla,
    pub control_hover: Hsla,
    pub control_pressed: Hsla,
    pub separator: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_active: Hsla,
    pub accent_soft: Hsla,
    pub accent_soft_hover: Hsla,
    pub accent_border: Hsla,
    pub accent_foreground: Hsla,
    pub error: Hsla,
    pub error_soft: Hsla,
    pub canvas: Hsla,
    pub canvas_empty: Hsla,
    pub overlay: Hsla,
    pub selection: Hsla,
    pub yellow: Hsla,
    pub green: Hsla,
    pub blue: Hsla,
    pub pink: Hsla,
    pub purple: Hsla,
    pub warning: Hsla,
    pub paper: Hsla,
    pub paper_border: Hsla,
}

impl ReaderPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            chrome: theme.title_bar,
            surface: theme.background,
            surface_subtle: theme.muted,
            control: theme.secondary,
            control_hover: theme.secondary_hover,
            control_pressed: theme.secondary_active,
            separator: theme.border,
            text: theme.foreground,
            text_secondary: theme.secondary_foreground,
            text_tertiary: theme.muted_foreground,
            accent: theme.primary,
            accent_hover: theme.primary_hover,
            accent_active: theme.primary_active,
            accent_soft: theme.accent,
            accent_soft_hover: theme.list_hover,
            accent_border: theme.list_active_border,
            accent_foreground: theme.primary_foreground,
            error: theme.danger,
            error_soft: theme.danger.opacity(0.12),
            canvas: theme.tiles,
            canvas_empty: theme.sidebar,
            overlay: theme.overlay,
            selection: theme.selection,
            yellow: theme.yellow,
            green: theme.green,
            blue: theme.blue,
            pink: theme.magenta,
            purple: theme.chart_4,
            warning: theme.warning,
            paper: pdf_paper_color(theme, theme.is_dark()),
            paper_border: pdf_paper_border(theme, theme.is_dark()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ThemeColor;

    #[test]
    fn preferences_have_stable_qa_names() {
        assert_eq!(ThemePreference::System.name(), "system");
        assert_eq!(ThemePreference::Named.name(), "named");
    }

    #[test]
    fn all_bundled_themes_are_named_and_menu_modes_are_covered() {
        assert_eq!(bundled_themes().len(), 37);
        assert!(bundled_themes().iter().all(|theme| !theme.name.is_empty()));
        assert!(
            bundled_themes()
                .iter()
                .any(|theme| theme.mode == ThemeMode::Light)
        );
        assert!(
            bundled_themes()
                .iter()
                .any(|theme| theme.mode == ThemeMode::Dark)
        );
        let unique = bundled_themes()
            .iter()
            .map(|theme| theme.name.as_ref())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), bundled_themes().len());
    }

    #[test]
    fn every_bundled_theme_resolves_all_reader_utility_tokens() {
        for config in bundled_themes() {
            let colors = if config.mode.is_dark() {
                ThemeColor::dark()
            } else {
                ThemeColor::light()
            };
            let mut theme = Theme::from(colors.as_ref());
            theme.apply_config(&Rc::new(config.clone()));
            theme.mode = config.mode;
            let palette = ReaderPalette::from_theme(&theme);

            assert_eq!(theme.theme_name(), &config.name, "{}", config.name);
            assert_eq!(theme.mode, config.mode, "{}", config.name);
            for (token, color) in [
                ("surface", palette.surface),
                ("text", palette.text),
                ("accent", palette.accent),
                ("separator", palette.separator),
                ("canvas", palette.canvas),
            ] {
                assert!(
                    color.a > 0.0,
                    "theme {} produced a transparent {token} token",
                    config.name
                );
            }
        }
    }

    #[test]
    fn dark_pdf_paper_is_opaque_and_distinct_from_every_bundled_workspace() {
        for config in bundled_themes()
            .iter()
            .filter(|config| config.mode.is_dark())
        {
            let mut theme = Theme::from(ThemeColor::dark().as_ref());
            theme.apply_config(&Rc::new(config.clone()));
            theme.mode = config.mode;
            let paper = Rgba::from(pdf_paper_color(&theme, true));
            let pane = Rgba::from(theme.tiles);
            let distance =
                (paper.r - pane.r).abs() + (paper.g - pane.g).abs() + (paper.b - pane.b).abs();

            assert_eq!(paper.a, 1.0, "{}", config.name);
            assert!(
                distance >= 3.0 / 255.0,
                "theme {} produced indistinguishable PDF paper and pane colors: {paper:?} vs {pane:?}",
                config.name
            );
        }
    }

    #[test]
    fn palette_tracks_both_component_theme_palettes() {
        let light = Theme::from(ThemeColor::light().as_ref());
        let mut dark = Theme::from(ThemeColor::dark().as_ref());
        dark.mode = ThemeMode::Dark;
        let light_palette = ReaderPalette::from_theme(&light);
        let dark_palette = ReaderPalette::from_theme(&dark);

        assert_eq!(light_palette.surface, light.background);
        assert_eq!(dark_palette.surface, dark.background);
        assert_eq!(light_palette.accent, light.primary);
        assert_eq!(dark_palette.accent, dark.primary);
        assert_eq!(light_palette.paper, ThemeColor::light().background);
        assert_eq!(dark_palette.paper, pdf_paper_color(&dark, true));
        assert_ne!(dark_palette.paper, dark_palette.canvas);
        assert_ne!(light_palette.surface, dark_palette.surface);
        assert_ne!(light_palette.text, dark_palette.text);
    }
}
