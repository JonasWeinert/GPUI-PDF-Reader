use gpui::{App, Hsla, Rgba, SharedString, Window, WindowBackgroundAppearance};
use gpui_component::{Theme, ThemeConfig, ThemeRegistry, ThemeSet};
use key_ui_gpui::{
    AccessibilityPreferences, BackdropEffect, DesignSystemConfig, ThemeSelection, ThemeTokens,
    configured_color, install_design_system, resolved_design_system,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{rc::Rc, sync::LazyLock};

const BUNDLED_THEMES_JSON: &str = include_str!("../../../assets/themes/gpui-component.json");
const BUNDLED_DESIGN_SYSTEM_JSON: &str = include_str!("../../../assets/ui/key-glass.json");
pub const DESIGN_SYSTEM_PATH_ENV: &str = "GPUI_PDF_READER_STYLE_PATH";

static BUNDLED_THEMES: LazyLock<ThemeSet> = LazyLock::new(|| {
    serde_json::from_str(BUNDLED_THEMES_JSON).expect("bundled gpui-component themes must be valid")
});

pub fn design_system_path() -> Option<PathBuf> {
    std::env::var_os(DESIGN_SYSTEM_PATH_ENV).map(PathBuf::from)
}

pub fn load_design_system(path: Option<&Path>) -> Result<DesignSystemConfig, String> {
    let source = match path {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?,
        None => BUNDLED_DESIGN_SYSTEM_JSON.to_owned(),
    };
    let config = DesignSystemConfig::from_json(&source).map_err(|error| error.to_string())?;
    if let ThemeSelection::Named { name } = &config.appearance.theme
        && !bundled_themes()
            .iter()
            .any(|theme| theme.name.as_ref() == name)
    {
        return Err(format!("theme {name:?} is not bundled"));
    }
    Ok(config)
}

pub fn install_initial_design_system(cx: &mut App) -> Option<PathBuf> {
    let path = design_system_path();
    let config = match load_design_system(path.as_deref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Design-system override rejected: {error}; using bundled defaults");
            load_design_system(None).expect("bundled design system must be valid")
        }
    };
    apply_design_system(&config, cx);
    path
}

pub fn apply_design_system(config: &DesignSystemConfig, cx: &mut App) {
    install_design_system(cx, config, accessibility_preferences());
    let background = window_background_appearance(cx);
    let selection = config.appearance.theme.clone();
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, cx| {
            let _ = apply_configured_theme(&selection, window, cx);
            window.set_background_appearance(background);
            window.refresh();
        });
    }
}

pub fn apply_configured_theme(
    selection: &ThemeSelection,
    window: &mut Window,
    cx: &mut App,
) -> Result<Option<SharedString>, String> {
    match selection {
        ThemeSelection::System => Ok(apply_selection("", window, cx)),
        ThemeSelection::Named { name } => apply_selection(name, window, cx)
            .map(Some)
            .ok_or_else(|| format!("configured theme {name:?} is not bundled")),
    }
}

pub fn apply_current_design_system_theme(
    window: &mut Window,
    cx: &mut App,
) -> Result<Option<SharedString>, String> {
    apply_configured_theme(&resolved_design_system(cx).appearance.theme, window, cx)
}

pub fn watch_design_system(path: PathBuf, cx: &mut App) {
    let mut last_modified = modified_at(&path);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let modified = modified_at(&path);
            if modified.is_none() || modified == last_modified {
                continue;
            }
            last_modified = modified;
            match load_design_system(Some(&path)) {
                Ok(config) => {
                    if cx.update(|cx| apply_design_system(&config, cx)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    eprintln!(
                        "Design-system reload rejected for {}: {error}; keeping last valid version",
                        path.display()
                    );
                }
            }
        }
    })
    .detach();
}

pub fn window_background_appearance(cx: &App) -> WindowBackgroundAppearance {
    if resolved_design_system(cx).materials.window.backdrop == BackdropEffect::WindowBlur {
        WindowBackgroundAppearance::Blurred
    } else if resolved_design_system(cx).policy.translucency_allowed {
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(target_os = "macos")]
fn accessibility_preferences() -> AccessibilityPreferences {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    AccessibilityPreferences {
        reduce_transparency: workspace.accessibilityDisplayShouldReduceTransparency(),
        increase_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
    }
}

#[cfg(not(target_os = "macos"))]
fn accessibility_preferences() -> AccessibilityPreferences {
    AccessibilityPreferences::default()
}

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
    pub ui: ThemeTokens,
    pub surface: Hsla,
    pub surface_subtle: Hsla,
    pub control_hover: Hsla,
    pub control_pressed: Hsla,
    pub separator: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
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
    pub fn from_app(cx: &App) -> Self {
        let mut palette = Self::from_theme_and_tokens(Theme::global(cx), ThemeTokens::from_app(cx));
        let colors = resolved_design_system(cx).appearance.colors;
        if let Some(paper) = configured_color(colors.document_paper) {
            palette.paper = paper;
        }
        if let Some(border) = configured_color(colors.document_paper_border) {
            palette.paper_border = border;
        }
        palette
    }

    #[cfg(test)]
    pub fn from_theme(theme: &Theme) -> Self {
        Self::from_theme_and_tokens(theme, ThemeTokens::from_theme(theme))
    }

    fn from_theme_and_tokens(theme: &Theme, ui: ThemeTokens) -> Self {
        Self {
            ui,
            surface: ui.materials.surface.background,
            surface_subtle: ui.materials.control.background,
            control_hover: ui.action.control_hover,
            control_pressed: ui.action.control_pressed,
            separator: ui.surface.border,
            text: ui.content.primary,
            text_secondary: ui.content.secondary,
            text_tertiary: ui.content.tertiary,
            accent: ui.action.accent,
            accent_soft: ui.action.accent_soft,
            accent_soft_hover: ui.action.accent_soft_hover,
            accent_border: ui.action.accent_border,
            accent_foreground: ui.content.on_accent,
            error: ui.status.danger,
            error_soft: ui.status.danger_soft,
            canvas: ui.surface.canvas,
            canvas_empty: ui.surface.sidebar,
            overlay: ui.materials.floating.background,
            selection: ui.selection,
            yellow: theme.yellow,
            green: theme.green,
            blue: theme.blue,
            pink: theme.magenta,
            purple: theme.chart_4,
            warning: ui.status.warning,
            paper: pdf_paper_color(theme, theme.is_dark()),
            paper_border: pdf_paper_border(theme, theme.is_dark()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::{ThemeColor, ThemeMode};

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

        assert_eq!(light_palette.surface, light.background.opacity(0.90));
        assert_eq!(dark_palette.surface, dark.background.opacity(0.90));
        assert_eq!(light_palette.accent, light.primary);
        assert_eq!(dark_palette.accent, dark.primary);
        assert_eq!(light_palette.paper, ThemeColor::light().background);
        assert_eq!(dark_palette.paper, pdf_paper_color(&dark, true));
        assert_ne!(dark_palette.paper, dark_palette.canvas);
        assert_ne!(light_palette.surface, dark_palette.surface);
        assert_ne!(light_palette.text, dark_palette.text);
    }

    #[test]
    fn bundled_design_system_and_variations_are_valid_and_materially_distinct() {
        let baseline = DesignSystemConfig::from_json(BUNDLED_DESIGN_SYSTEM_JSON).unwrap();
        let square = DesignSystemConfig::from_json(include_str!(
            "../../../assets/ui/variations/square-opaque.json"
        ))
        .unwrap();
        let clear = DesignSystemConfig::from_json(include_str!(
            "../../../assets/ui/variations/clear-glass.json"
        ))
        .unwrap();
        let safari = DesignSystemConfig::from_json(include_str!(
            "../../../assets/ui/variations/safari-chrome.json"
        ))
        .unwrap();
        let safari_glass = DesignSystemConfig::from_json(include_str!(
            "../../../assets/ui/variations/safari-glass.json"
        ))
        .unwrap();
        let accessibility = AccessibilityPreferences::default();
        let baseline = baseline.resolve(accessibility);
        let square = square.resolve(accessibility);
        let clear = clear.resolve(accessibility);
        let safari = safari.resolve(accessibility);
        let safari_glass = safari_glass.resolve(accessibility);

        assert!(baseline.policy.translucency_allowed);
        assert_eq!(square.geometry.radius_large, 0.0);
        assert_eq!(square.materials.window.opacity, 1.0);
        assert_eq!(square.materials.floating.shadow_opacity, 0.0);
        assert!(clear.materials.chrome.opacity < baseline.materials.chrome.opacity);
        assert!(clear.geometry.radius_large > baseline.geometry.radius_large);
        assert_ne!(
            safari.workspace.chrome.regular.row_order,
            baseline.workspace.chrome.regular.row_order
        );
        assert_eq!(
            safari_glass.workspace.chrome.regular.row_order,
            safari.workspace.chrome.regular.row_order
        );
        assert_eq!(
            safari_glass.materials.chrome.backdrop,
            BackdropEffect::WindowBlur
        );
    }
}
