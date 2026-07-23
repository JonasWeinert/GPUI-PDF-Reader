//! Renderer-independent design-system schema, validation, and resolution.
//!
//! Serialized configuration is deliberately converted into validated domain
//! types before any renderer sees it. Product code consumes semantic roles;
//! it does not interpret loosely typed style maps.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::fmt;

pub const CURRENT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesignSystemConfig {
    pub schema_version: u16,
    pub appearance: AppearanceConfig,
    pub policy: GlobalPolicy,
    pub geometry: GeometryConfig,
    pub materials: MaterialConfig,
    pub palette: PaletteContrastConfig,
    pub workspace: WorkspaceLayoutConfig,
    pub typography: TypographyConfig,
    pub interaction: InteractionConfig,
    pub components: ComponentConfig,
    pub reader: ReaderUiConfig,
    pub motion: MotionConfig,
    pub responsive: ResponsiveConfig,
}

impl Default for DesignSystemConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            appearance: AppearanceConfig::default(),
            policy: GlobalPolicy::default(),
            geometry: GeometryConfig::default(),
            materials: MaterialConfig::default(),
            palette: PaletteContrastConfig::default(),
            workspace: WorkspaceLayoutConfig::default(),
            typography: TypographyConfig::default(),
            interaction: InteractionConfig::default(),
            components: ComponentConfig::default(),
            reader: ReaderUiConfig::default(),
            motion: MotionConfig::default(),
            responsive: ResponsiveConfig::default(),
        }
    }
}

impl DesignSystemConfig {
    pub fn from_json(source: &str) -> Result<Self, ConfigError> {
        let config: Self =
            serde_json::from_str(source).map_err(|error| ConfigError::Parse(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: self.schema_version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        self.appearance.validate()?;
        validate_number(
            "policy.maximum_corner_radius",
            self.policy.maximum_corner_radius,
            true,
            2_048.0,
        )?;
        self.geometry.validate()?;
        self.materials.validate()?;
        self.palette.validate()?;
        self.workspace.validate()?;
        self.typography.validate()?;
        self.interaction.validate()?;
        self.components.validate()?;
        self.reader.validate()?;
        self.motion.validate()?;
        self.responsive.validate()?;
        Ok(())
    }

    #[must_use]
    pub fn resolve(&self, accessibility: AccessibilityPreferences) -> ResolvedDesignSystem {
        let curvature_allowed = self.policy.curvature != FeaturePolicy::Disabled;
        let shadows_allowed = self.policy.shadows != FeaturePolicy::Disabled;
        let translucency_allowed = self.policy.translucency != FeaturePolicy::Disabled
            && !accessibility.reduce_transparency;
        let motion_allowed =
            self.policy.motion != FeaturePolicy::Disabled && !accessibility.reduce_motion;

        let radius = |requested: f32| {
            if curvature_allowed {
                requested.min(self.policy.maximum_corner_radius)
            } else {
                0.0
            }
        };
        let material = |value: MaterialStyle| {
            value.resolve(
                translucency_allowed,
                shadows_allowed,
                accessibility.increase_contrast,
            )
        };

        ResolvedDesignSystem {
            appearance: self.appearance.clone(),
            policy: ResolvedPolicy {
                curvature_allowed,
                concave_corners_allowed: curvature_allowed
                    && self.policy.concave_corners != FeaturePolicy::Disabled,
                shadows_allowed,
                translucency_allowed,
                motion_allowed,
            },
            geometry: ResolvedGeometry {
                radius_small: radius(self.geometry.radius_small),
                radius_medium: radius(self.geometry.radius_medium),
                radius_large: radius(self.geometry.radius_large),
                radius_pill: radius(self.geometry.radius_pill),
                control_height: self.geometry.control_height,
                compact_control_height: self.geometry.compact_control_height,
                icon_size: self.geometry.icon_size,
                panel_header_height: self.geometry.panel_header_height,
                space_unit: self.geometry.space_unit,
                border_width: self.geometry.border_width,
            },
            materials: ResolvedMaterials {
                window: material(self.materials.window),
                chrome: material(self.materials.chrome),
                surface: material(self.materials.surface),
                floating: material(self.materials.floating),
                control: material(self.materials.control),
            },
            palette: self.palette,
            workspace: self.workspace,
            typography: self.typography,
            interaction: self.interaction.resolve(),
            components: self.components.resolve(
                ResolvedPolicy {
                    curvature_allowed,
                    concave_corners_allowed: curvature_allowed
                        && self.policy.concave_corners != FeaturePolicy::Disabled,
                    shadows_allowed,
                    translucency_allowed,
                    motion_allowed,
                },
                self.policy.maximum_corner_radius,
            ),
            reader: self.reader,
            motion: ResolvedMotion {
                fast_response: motion_allowed.then_some(self.motion.fast_response),
                standard_response: motion_allowed.then_some(self.motion.standard_response),
                gentle_response: motion_allowed.then_some(self.motion.gentle_response),
                short_duration_ms: if motion_allowed {
                    self.motion.short_duration_ms
                } else {
                    0
                },
                medium_duration_ms: if motion_allowed {
                    self.motion.medium_duration_ms
                } else {
                    0
                },
                tab_move_duration_ms: motion_allowed.then_some(self.motion.tab_move_duration_ms),
                spinner_duration_ms: motion_allowed.then_some(self.motion.spinner_duration_ms),
                hover_handoff_delay_ms: self.motion.hover_handoff_delay_ms,
                hover_close_delay_ms: self.motion.hover_close_delay_ms,
                toc_leave_delay_ms: self.motion.toc_leave_delay_ms,
                feedback_duration_ms: self.motion.feedback_duration_ms,
                marquee_points_per_second: motion_allowed
                    .then_some(self.motion.marquee_points_per_second),
                marquee_min_duration_ms: motion_allowed
                    .then_some(self.motion.marquee_min_duration_ms),
            },
            responsive: self.responsive,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ThemeSelection {
    #[default]
    System,
    Named {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RgbaColor {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    #[serde(default = "opaque_alpha")]
    pub alpha: f32,
}

const fn opaque_alpha() -> f32 {
    1.0
}

impl RgbaColor {
    fn validate(self, name: &str) -> Result<(), ConfigError> {
        for (channel, value) in [
            ("red", self.red),
            ("green", self.green),
            ("blue", self.blue),
            ("alpha", self.alpha),
        ] {
            validate_number(
                &format!("appearance.colors.{name}.{channel}"),
                value,
                true,
                1.0,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticColorConfig {
    pub surface_background: Option<RgbaColor>,
    pub surface_chrome: Option<RgbaColor>,
    pub surface_canvas: Option<RgbaColor>,
    pub surface_split_gutter: Option<RgbaColor>,
    pub surface_sidebar: Option<RgbaColor>,
    pub surface_popover: Option<RgbaColor>,
    pub surface_border: Option<RgbaColor>,
    pub content_primary: Option<RgbaColor>,
    pub content_secondary: Option<RgbaColor>,
    pub content_tertiary: Option<RgbaColor>,
    pub action_accent: Option<RgbaColor>,
    pub document_paper: Option<RgbaColor>,
    pub document_paper_border: Option<RgbaColor>,
}

impl SemanticColorConfig {
    fn validate(self) -> Result<(), ConfigError> {
        for (name, color) in [
            ("surface_background", self.surface_background),
            ("surface_chrome", self.surface_chrome),
            ("surface_canvas", self.surface_canvas),
            ("surface_split_gutter", self.surface_split_gutter),
            ("surface_sidebar", self.surface_sidebar),
            ("surface_popover", self.surface_popover),
            ("surface_border", self.surface_border),
            ("content_primary", self.content_primary),
            ("content_secondary", self.content_secondary),
            ("content_tertiary", self.content_tertiary),
            ("action_accent", self.action_accent),
            ("document_paper", self.document_paper),
            ("document_paper_border", self.document_paper_border),
        ] {
            if let Some(color) = color {
                color.validate(name)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIcon {
    Add,
    Close,
    Document,
    Settings,
    Search,
    Sidebar,
    TabList,
    Split,
    Folder,
    Swap,
    Separate,
    CloseLeft,
    CloseRight,
    Previous,
    Next,
    Collapse,
    Expand,
    ExternalLink,
    Book,
    Globe,
    User,
    Error,
    Info,
    Terminal,
    Dashboard,
    Loader,
    ThemeLight,
    ThemeDark,
    ZoomOut,
    ZoomIn,
    FitWidth,
    Comments,
    Highlight,
    Copy,
    Check,
    Menu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IconRoleConfig {
    pub new_tab: SemanticIcon,
    pub close: SemanticIcon,
    pub document: SemanticIcon,
    pub settings: SemanticIcon,
    pub search: SemanticIcon,
    pub sidebar: SemanticIcon,
    pub tab_list: SemanticIcon,
    pub split: SemanticIcon,
    pub folder: SemanticIcon,
    pub swap: SemanticIcon,
    pub separate: SemanticIcon,
    pub close_left: SemanticIcon,
    pub close_right: SemanticIcon,
    pub previous: SemanticIcon,
    pub next: SemanticIcon,
    pub collapse: SemanticIcon,
    pub expand: SemanticIcon,
    pub external_link: SemanticIcon,
    pub book: SemanticIcon,
    pub globe: SemanticIcon,
    pub user: SemanticIcon,
    pub error: SemanticIcon,
    pub info: SemanticIcon,
    pub terminal: SemanticIcon,
    pub dashboard: SemanticIcon,
    pub loader: SemanticIcon,
    pub theme_light: SemanticIcon,
    pub theme_dark: SemanticIcon,
    pub zoom_out: SemanticIcon,
    pub zoom_in: SemanticIcon,
    pub fit_width: SemanticIcon,
    pub comments: SemanticIcon,
    pub highlight: SemanticIcon,
    pub copy: SemanticIcon,
    pub check: SemanticIcon,
    pub menu: SemanticIcon,
}

impl Default for IconRoleConfig {
    fn default() -> Self {
        Self {
            new_tab: SemanticIcon::Add,
            close: SemanticIcon::Close,
            document: SemanticIcon::Document,
            settings: SemanticIcon::Settings,
            search: SemanticIcon::Search,
            sidebar: SemanticIcon::Sidebar,
            tab_list: SemanticIcon::TabList,
            split: SemanticIcon::Split,
            folder: SemanticIcon::Folder,
            swap: SemanticIcon::Swap,
            separate: SemanticIcon::Separate,
            close_left: SemanticIcon::CloseLeft,
            close_right: SemanticIcon::CloseRight,
            previous: SemanticIcon::Previous,
            next: SemanticIcon::Next,
            collapse: SemanticIcon::Collapse,
            expand: SemanticIcon::Expand,
            external_link: SemanticIcon::ExternalLink,
            book: SemanticIcon::Book,
            globe: SemanticIcon::Globe,
            user: SemanticIcon::User,
            error: SemanticIcon::Error,
            info: SemanticIcon::Info,
            terminal: SemanticIcon::Terminal,
            dashboard: SemanticIcon::Dashboard,
            loader: SemanticIcon::Loader,
            theme_light: SemanticIcon::ThemeLight,
            theme_dark: SemanticIcon::ThemeDark,
            zoom_out: SemanticIcon::ZoomOut,
            zoom_in: SemanticIcon::ZoomIn,
            fit_width: SemanticIcon::FitWidth,
            comments: SemanticIcon::Comments,
            highlight: SemanticIcon::Highlight,
            copy: SemanticIcon::Copy,
            check: SemanticIcon::Check,
            menu: SemanticIcon::Menu,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceConfig {
    pub theme: ThemeSelection,
    pub colors: SemanticColorConfig,
    pub icons: IconRoleConfig,
}

impl AppearanceConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if let ThemeSelection::Named { name } = &self.theme
            && name.trim().is_empty()
        {
            return Err(ConfigError::InvalidValue(
                "appearance.theme named selection requires a non-empty name".into(),
            ));
        }
        self.colors.validate()
    }
}

/// Theme-relative tonal separation between structural application regions.
/// Values are foreground-mix fractions, keeping contrast adaptive in light,
/// dark, and custom themes without hard-coding RGB colors in feature views.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaletteContrastConfig {
    pub chrome_foreground_mix: f32,
    pub canvas_foreground_mix: f32,
    pub sidebar_foreground_mix: f32,
    pub split_gutter_foreground_mix: f32,
}

impl Default for PaletteContrastConfig {
    fn default() -> Self {
        Self {
            chrome_foreground_mix: 0.018,
            canvas_foreground_mix: 0.075,
            sidebar_foreground_mix: 0.035,
            split_gutter_foreground_mix: 0.135,
        }
    }
}

impl PaletteContrastConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("palette.chrome_foreground_mix", self.chrome_foreground_mix),
            ("palette.canvas_foreground_mix", self.canvas_foreground_mix),
            (
                "palette.sidebar_foreground_mix",
                self.sidebar_foreground_mix,
            ),
            (
                "palette.split_gutter_foreground_mix",
                self.split_gutter_foreground_mix,
            ),
        ] {
            validate_number(name, value, true, 1.0)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeRowOrder {
    TabsThenControls,
    ControlsThenTabs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeUtilityControlsRow {
    Top,
    Tab,
    Control,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChromeLayoutConfig {
    pub row_order: ChromeRowOrder,
    pub utility_controls_row: ChromeUtilityControlsRow,
    pub utility_controls_leading_inset: f32,
    pub tab_bar_height: f32,
    pub tab_height: f32,
    pub split_segment_height: f32,
    pub tab_popover_gap: f32,
    pub tab_leading_inset: f32,
    pub control_leading_inset: f32,
    pub tab_width: f32,
    pub split_tab_width: f32,
    pub new_tab_button_size: f32,
    pub tab_min_width_ratio: f32,
    pub title_fade_width: f32,
    pub utility_cluster_width: f32,
    pub trailing_reserved_width: f32,
    pub tab_horizontal_padding: f32,
    pub split_horizontal_padding: f32,
}

impl Default for ChromeLayoutConfig {
    fn default() -> Self {
        Self {
            row_order: ChromeRowOrder::TabsThenControls,
            utility_controls_row: ChromeUtilityControlsRow::Top,
            utility_controls_leading_inset: 104.0,
            tab_bar_height: 52.0,
            tab_height: 34.0,
            split_segment_height: 32.0,
            tab_popover_gap: 4.0,
            tab_leading_inset: 104.0,
            control_leading_inset: 0.0,
            tab_width: 220.0,
            split_tab_width: 440.0,
            new_tab_button_size: 28.0,
            tab_min_width_ratio: 0.67,
            title_fade_width: 42.0,
            utility_cluster_width: 96.0,
            trailing_reserved_width: 52.0,
            tab_horizontal_padding: 12.0,
            split_horizontal_padding: 4.0,
        }
    }
}

impl ChromeLayoutConfig {
    fn validate(&self, prefix: &str) -> Result<(), ConfigError> {
        for (name, value, allow_zero) in [
            (
                "utility_controls_leading_inset",
                self.utility_controls_leading_inset,
                true,
            ),
            ("tab_bar_height", self.tab_bar_height, false),
            ("tab_height", self.tab_height, false),
            ("split_segment_height", self.split_segment_height, false),
            ("tab_popover_gap", self.tab_popover_gap, true),
            ("tab_leading_inset", self.tab_leading_inset, true),
            ("control_leading_inset", self.control_leading_inset, true),
            ("tab_width", self.tab_width, false),
            ("split_tab_width", self.split_tab_width, false),
            ("new_tab_button_size", self.new_tab_button_size, false),
            ("title_fade_width", self.title_fade_width, true),
            ("utility_cluster_width", self.utility_cluster_width, true),
            (
                "trailing_reserved_width",
                self.trailing_reserved_width,
                true,
            ),
            ("tab_horizontal_padding", self.tab_horizontal_padding, true),
            (
                "split_horizontal_padding",
                self.split_horizontal_padding,
                true,
            ),
        ] {
            validate_number(&format!("{prefix}.{name}"), value, allow_zero, 2_048.0)?;
        }
        validate_number(
            &format!("{prefix}.tab_min_width_ratio"),
            self.tab_min_width_ratio,
            false,
            1.0,
        )?;
        if self.tab_height > self.tab_bar_height {
            return Err(ConfigError::InvalidValue(format!(
                "{prefix}.tab_height must not exceed tab_bar_height"
            )));
        }
        if self.split_segment_height > self.tab_height {
            return Err(ConfigError::InvalidValue(format!(
                "{prefix}.split_segment_height must not exceed tab_height"
            )));
        }
        if self.split_tab_width < self.tab_width {
            return Err(ConfigError::InvalidValue(format!(
                "{prefix}.split_tab_width must be at least tab_width"
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn utilities_in_tab_row(&self) -> bool {
        match self.utility_controls_row {
            ChromeUtilityControlsRow::Top => self.row_order == ChromeRowOrder::TabsThenControls,
            ChromeUtilityControlsRow::Tab => true,
            ChromeUtilityControlsRow::Control => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SplitLayoutConfig {
    pub outer_padding: f32,
    pub pane_gap: f32,
    pub divider_line_width: f32,
}

impl Default for SplitLayoutConfig {
    fn default() -> Self {
        Self {
            outer_padding: 10.0,
            pane_gap: 14.0,
            divider_line_width: 2.0,
        }
    }
}

impl SplitLayoutConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("workspace.split.outer_padding", self.outer_padding),
            ("workspace.split.pane_gap", self.pane_gap),
            (
                "workspace.split.divider_line_width",
                self.divider_line_width,
            ),
        ] {
            validate_number(name, value, true, 256.0)?;
        }
        if self.divider_line_width > self.pane_gap {
            return Err(ConfigError::InvalidValue(
                "workspace.split.divider_line_width must not exceed pane_gap".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceLayoutConfig {
    pub chrome: ResponsiveValue<ChromeLayoutConfig>,
    pub split: SplitLayoutConfig,
}

impl Default for WorkspaceLayoutConfig {
    fn default() -> Self {
        Self {
            chrome: ResponsiveValue {
                regular: ChromeLayoutConfig::default(),
                compact: None,
                comfortable: None,
            },
            split: SplitLayoutConfig::default(),
        }
    }
}

impl WorkspaceLayoutConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        self.chrome.regular.validate("workspace.chrome.regular")?;
        if let Some(compact) = self.chrome.compact {
            compact.validate("workspace.chrome.compact")?;
        }
        if let Some(comfortable) = self.chrome.comfortable {
            comfortable.validate("workspace.chrome.comfortable")?;
        }
        self.split.validate()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontWeightToken {
    Normal,
    Medium,
    Semibold,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyleConfig {
    pub size_rem: f32,
    pub line_height: f32,
    pub weight: FontWeightToken,
}

impl TextStyleConfig {
    fn validate(self, name: &str) -> Result<(), ConfigError> {
        validate_number(
            &format!("typography.{name}.size_rem"),
            self.size_rem,
            false,
            8.0,
        )?;
        validate_number(
            &format!("typography.{name}.line_height"),
            self.line_height,
            false,
            4.0,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypographyConfig {
    pub caption: TextStyleConfig,
    pub label: TextStyleConfig,
    pub body: TextStyleConfig,
    pub heading: TextStyleConfig,
    pub title: TextStyleConfig,
    pub display: TextStyleConfig,
}

impl Default for TypographyConfig {
    fn default() -> Self {
        Self {
            caption: TextStyleConfig {
                size_rem: 0.75,
                line_height: 1.25,
                weight: FontWeightToken::Normal,
            },
            label: TextStyleConfig {
                size_rem: 0.875,
                line_height: 1.3,
                weight: FontWeightToken::Medium,
            },
            body: TextStyleConfig {
                size_rem: 0.875,
                line_height: 1.45,
                weight: FontWeightToken::Normal,
            },
            heading: TextStyleConfig {
                size_rem: 1.0,
                line_height: 1.35,
                weight: FontWeightToken::Semibold,
            },
            title: TextStyleConfig {
                size_rem: 1.25,
                line_height: 1.25,
                weight: FontWeightToken::Semibold,
            },
            display: TextStyleConfig {
                size_rem: 1.5,
                line_height: 1.2,
                weight: FontWeightToken::Bold,
            },
        }
    }
}

impl TypographyConfig {
    fn validate(self) -> Result<(), ConfigError> {
        for (name, style) in [
            ("caption", self.caption),
            ("label", self.label),
            ("body", self.body),
            ("heading", self.heading),
            ("title", self.title),
            ("display", self.display),
        ] {
            style.validate(name)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InteractionConfig {
    pub opacity: StateValue<f32>,
    pub surface_opacity: StateValue<f32>,
    pub border_opacity: StateValue<f32>,
}

impl Default for InteractionConfig {
    fn default() -> Self {
        Self {
            opacity: StateValue {
                base: 1.0,
                hovered: None,
                pressed: None,
                focused: None,
                disabled: Some(0.42),
                selected: None,
                expanded: None,
                dragging: Some(0.72),
            },
            surface_opacity: StateValue {
                base: 0.0,
                hovered: Some(0.08),
                pressed: Some(0.14),
                focused: Some(0.06),
                disabled: Some(0.0),
                selected: Some(0.12),
                expanded: Some(0.12),
                dragging: Some(0.16),
            },
            border_opacity: StateValue {
                base: 0.0,
                hovered: Some(0.42),
                pressed: Some(0.58),
                focused: Some(0.82),
                disabled: Some(0.18),
                selected: Some(0.72),
                expanded: Some(0.72),
                dragging: Some(0.88),
            },
        }
    }
}

impl InteractionConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_state_number("interaction.opacity", &self.opacity, 1.0)?;
        validate_state_number("interaction.surface_opacity", &self.surface_opacity, 1.0)?;
        validate_state_number("interaction.border_opacity", &self.border_opacity, 1.0)
    }

    fn resolve(&self) -> Self {
        *self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommonComponentConfig {
    pub icon_xsmall: f32,
    pub icon_small: f32,
    pub icon_medium: f32,
    pub icon_large: f32,
    pub control_small: f32,
    pub control_medium: f32,
    pub control_large: f32,
    pub row_compact: f32,
    pub row_standard: f32,
    pub row_comfortable: f32,
    pub separator_width: f32,
    pub separator_length: f32,
    pub content_max_width: f32,
    pub shadow_surface_y: f32,
    pub shadow_surface_blur: f32,
    pub shadow_surface_spread: f32,
    pub shadow_floating_y: f32,
    pub shadow_floating_blur: f32,
    pub shadow_floating_spread: f32,
}

impl Default for CommonComponentConfig {
    fn default() -> Self {
        Self {
            icon_xsmall: 11.0,
            icon_small: 13.0,
            icon_medium: 15.0,
            icon_large: 17.0,
            control_small: 28.0,
            control_medium: 34.0,
            control_large: 38.0,
            row_compact: 28.0,
            row_standard: 38.0,
            row_comfortable: 48.0,
            separator_width: 1.0,
            separator_length: 24.0,
            content_max_width: 720.0,
            shadow_surface_y: 2.0,
            shadow_surface_blur: 7.0,
            shadow_surface_spread: -3.0,
            shadow_floating_y: 12.0,
            shadow_floating_blur: 34.0,
            shadow_floating_spread: -12.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PopoverComponentConfig {
    pub tab_hover_width: f32,
    pub tab_search_width: f32,
    pub tab_search_max_height: f32,
    pub split_menu_width: f32,
    pub edge_margin: f32,
    pub row_height: f32,
    pub drag_preview_width: f32,
    pub drag_preview_height: f32,
    pub marquee_start_characters: u16,
    pub average_character_width: f32,
}

impl Default for PopoverComponentConfig {
    fn default() -> Self {
        Self {
            tab_hover_width: 282.0,
            tab_search_width: 380.0,
            tab_search_max_height: 460.0,
            split_menu_width: 268.0,
            edge_margin: 8.0,
            row_height: 48.0,
            drag_preview_width: 220.0,
            drag_preview_height: 40.0,
            marquee_start_characters: 31,
            average_character_width: 7.2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlBarComponentConfig {
    pub primary_height: f32,
    pub item_gap: f32,
    pub host_reserved_width: f32,
    pub auxiliary_height: f32,
    pub title_auxiliary_height: f32,
    pub search_height: f32,
    pub search_close_fade_width: f32,
    pub result_label_width: f32,
    pub result_card_height: f32,
    pub result_card_width: f32,
}

impl Default for ControlBarComponentConfig {
    fn default() -> Self {
        Self {
            primary_height: 44.0,
            item_gap: 4.0,
            host_reserved_width: 72.0,
            auxiliary_height: 94.0,
            title_auxiliary_height: 168.0,
            search_height: 34.0,
            search_close_fade_width: 58.0,
            result_label_width: 116.0,
            result_card_height: 66.0,
            result_card_width: 270.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureComponentConfig {
    pub header_height: f32,
    pub row_height: f32,
    pub compact_row_height: f32,
    pub button_height: f32,
    pub card_padding: f32,
    pub card_gap: f32,
    pub section_gap: f32,
    pub badge_height: f32,
    pub icon_size: f32,
}

impl Default for FeatureComponentConfig {
    fn default() -> Self {
        Self {
            header_height: 54.0,
            row_height: 38.0,
            compact_row_height: 28.0,
            button_height: 30.0,
            card_padding: 12.0,
            card_gap: 8.0,
            section_gap: 12.0,
            badge_height: 22.0,
            icon_size: 15.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentConfig {
    pub common: CommonComponentConfig,
    pub corners: ComponentCornerConfig,
    pub popover: PopoverComponentConfig,
    pub control_bar: ControlBarComponentConfig,
    pub references: FeatureComponentConfig,
    pub extensions: FeatureComponentConfig,
    pub comments: FeatureComponentConfig,
    pub search: FeatureComponentConfig,
    pub settings: FeatureComponentConfig,
    pub editor: FeatureComponentConfig,
}

impl ComponentConfig {
    fn validate(self) -> Result<(), ConfigError> {
        for (name, corners) in [
            ("button", self.corners.button),
            ("panel", self.corners.panel),
            ("floating", self.corners.floating),
            ("tab", self.corners.tab),
            ("split_segment", self.corners.split_segment),
            ("popover", self.corners.popover),
            ("card", self.corners.card),
            ("context_pill", self.corners.context_pill),
        ] {
            corners.validate(&format!("components.corners.{name}"))?;
        }
        validate_positive_fields(
            "components.common",
            &[
                ("icon_xsmall", self.common.icon_xsmall),
                ("icon_small", self.common.icon_small),
                ("icon_medium", self.common.icon_medium),
                ("icon_large", self.common.icon_large),
                ("control_small", self.common.control_small),
                ("control_medium", self.common.control_medium),
                ("control_large", self.common.control_large),
                ("row_compact", self.common.row_compact),
                ("row_standard", self.common.row_standard),
                ("row_comfortable", self.common.row_comfortable),
                ("separator_width", self.common.separator_width),
                ("separator_length", self.common.separator_length),
                ("content_max_width", self.common.content_max_width),
                ("shadow_surface_blur", self.common.shadow_surface_blur),
                ("shadow_floating_blur", self.common.shadow_floating_blur),
            ],
        )?;
        for (name, value) in [
            ("shadow_surface_y", self.common.shadow_surface_y),
            ("shadow_surface_spread", self.common.shadow_surface_spread),
            ("shadow_floating_y", self.common.shadow_floating_y),
            ("shadow_floating_spread", self.common.shadow_floating_spread),
        ] {
            validate_number(
                &format!("components.common.{name}"),
                value.abs(),
                true,
                2_048.0,
            )?;
        }
        validate_positive_fields(
            "components.popover",
            &[
                ("tab_hover_width", self.popover.tab_hover_width),
                ("tab_search_width", self.popover.tab_search_width),
                ("tab_search_max_height", self.popover.tab_search_max_height),
                ("split_menu_width", self.popover.split_menu_width),
                ("edge_margin", self.popover.edge_margin),
                ("row_height", self.popover.row_height),
                ("drag_preview_width", self.popover.drag_preview_width),
                ("drag_preview_height", self.popover.drag_preview_height),
                (
                    "average_character_width",
                    self.popover.average_character_width,
                ),
            ],
        )?;
        if self.popover.marquee_start_characters == 0 {
            return Err(ConfigError::InvalidValue(
                "components.popover.marquee_start_characters must be positive".into(),
            ));
        }
        validate_positive_fields(
            "components.control_bar",
            &[
                ("primary_height", self.control_bar.primary_height),
                ("item_gap", self.control_bar.item_gap),
                ("host_reserved_width", self.control_bar.host_reserved_width),
                ("auxiliary_height", self.control_bar.auxiliary_height),
                (
                    "title_auxiliary_height",
                    self.control_bar.title_auxiliary_height,
                ),
                ("search_height", self.control_bar.search_height),
                (
                    "search_close_fade_width",
                    self.control_bar.search_close_fade_width,
                ),
                ("result_label_width", self.control_bar.result_label_width),
                ("result_card_height", self.control_bar.result_card_height),
                ("result_card_width", self.control_bar.result_card_width),
            ],
        )?;
        for (name, feature) in [
            ("references", self.references),
            ("extensions", self.extensions),
            ("comments", self.comments),
            ("search", self.search),
            ("settings", self.settings),
            ("editor", self.editor),
        ] {
            validate_positive_fields(
                &format!("components.{name}"),
                &[
                    ("header_height", feature.header_height),
                    ("row_height", feature.row_height),
                    ("compact_row_height", feature.compact_row_height),
                    ("button_height", feature.button_height),
                    ("card_padding", feature.card_padding),
                    ("card_gap", feature.card_gap),
                    ("section_gap", feature.section_gap),
                    ("badge_height", feature.badge_height),
                    ("icon_size", feature.icon_size),
                ],
            )?;
        }
        Ok(())
    }

    fn resolve(self, policy: ResolvedPolicy, maximum_radius: f32) -> Self {
        Self {
            corners: self.corners.resolve(policy, maximum_radius),
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentCornerConfig {
    pub button: CornerSpec,
    pub panel: CornerSpec,
    pub floating: CornerSpec,
    pub tab: CornerSpec,
    pub split_segment: CornerSpec,
    pub popover: CornerSpec,
    pub card: CornerSpec,
    pub context_pill: CornerSpec,
}

impl Default for ComponentCornerConfig {
    fn default() -> Self {
        Self {
            button: CornerSpec::uniform_convex(10.0),
            panel: CornerSpec::uniform_convex(0.0),
            floating: CornerSpec::uniform_convex(18.0),
            tab: CornerSpec::uniform_convex(10.0),
            split_segment: CornerSpec::uniform_convex(10.0),
            popover: CornerSpec::uniform_convex(18.0),
            card: CornerSpec::uniform_convex(18.0),
            context_pill: CornerSpec::uniform_convex(999.0),
        }
    }
}

impl ComponentCornerConfig {
    fn resolve(self, policy: ResolvedPolicy, maximum_radius: f32) -> Self {
        Self {
            button: self.button.resolve(policy, maximum_radius),
            panel: self.panel.resolve(policy, maximum_radius),
            floating: self.floating.resolve(policy, maximum_radius),
            tab: self.tab.resolve(policy, maximum_radius),
            split_segment: self.split_segment.resolve(policy, maximum_radius),
            popover: self.popover.resolve(policy, maximum_radius),
            card: self.card.resolve(policy, maximum_radius),
            context_pill: self.context_pill.resolve(policy, maximum_radius),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReaderUiConfig {
    pub error_bar_height: f32,
    pub sidebar_width: f32,
    pub reference_panel_min_width: f32,
    pub reference_panel_max_width: f32,
    pub minimum_document_viewport_width: f32,
    pub panel_horizontal_margin: f32,
    pub panel_vertical_margin: f32,
    pub context_pill_width: f32,
    pub context_pill_height: f32,
    pub link_card_width: f32,
    pub link_card_margin: f32,
    pub link_card_gap: f32,
    pub toc_rail_width: f32,
    pub toc_marker_left: f32,
    pub toc_stack_margin: f32,
    pub toc_stack_spacing: f32,
    pub toc_cascade_radius: f32,
    pub toc_card_min_height: f32,
}

impl Default for ReaderUiConfig {
    fn default() -> Self {
        Self {
            error_bar_height: 34.0,
            sidebar_width: 344.0,
            reference_panel_min_width: 372.0,
            reference_panel_max_width: 468.0,
            minimum_document_viewport_width: 300.0,
            panel_horizontal_margin: 12.0,
            panel_vertical_margin: 18.0,
            context_pill_width: 214.0,
            context_pill_height: 40.0,
            link_card_width: 340.0,
            link_card_margin: 12.0,
            link_card_gap: 8.0,
            toc_rail_width: 54.0,
            toc_marker_left: 8.0,
            toc_stack_margin: 22.0,
            toc_stack_spacing: 12.0,
            toc_cascade_radius: 5.0,
            toc_card_min_height: 82.0,
        }
    }
}

impl ReaderUiConfig {
    fn validate(self) -> Result<(), ConfigError> {
        validate_positive_fields(
            "reader",
            &[
                ("error_bar_height", self.error_bar_height),
                ("sidebar_width", self.sidebar_width),
                ("reference_panel_min_width", self.reference_panel_min_width),
                ("reference_panel_max_width", self.reference_panel_max_width),
                (
                    "minimum_document_viewport_width",
                    self.minimum_document_viewport_width,
                ),
                ("panel_horizontal_margin", self.panel_horizontal_margin),
                ("panel_vertical_margin", self.panel_vertical_margin),
                ("context_pill_width", self.context_pill_width),
                ("context_pill_height", self.context_pill_height),
                ("link_card_width", self.link_card_width),
                ("link_card_margin", self.link_card_margin),
                ("link_card_gap", self.link_card_gap),
                ("toc_rail_width", self.toc_rail_width),
                ("toc_marker_left", self.toc_marker_left),
                ("toc_stack_margin", self.toc_stack_margin),
                ("toc_stack_spacing", self.toc_stack_spacing),
                ("toc_cascade_radius", self.toc_cascade_radius),
                ("toc_card_min_height", self.toc_card_min_height),
            ],
        )?;
        if self.reference_panel_min_width > self.reference_panel_max_width {
            return Err(ConfigError::InvalidValue(
                "reader.reference_panel_min_width must not exceed reference_panel_max_width".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeaturePolicy {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalPolicy {
    pub curvature: FeaturePolicy,
    pub concave_corners: FeaturePolicy,
    pub shadows: FeaturePolicy,
    pub translucency: FeaturePolicy,
    pub motion: FeaturePolicy,
    pub maximum_corner_radius: f32,
}

impl Default for GlobalPolicy {
    fn default() -> Self {
        Self {
            curvature: FeaturePolicy::Enabled,
            concave_corners: FeaturePolicy::Enabled,
            shadows: FeaturePolicy::Enabled,
            translucency: FeaturePolicy::Enabled,
            motion: FeaturePolicy::Enabled,
            maximum_corner_radius: 999.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeometryConfig {
    pub radius_small: f32,
    pub radius_medium: f32,
    pub radius_large: f32,
    pub radius_pill: f32,
    pub control_height: f32,
    pub compact_control_height: f32,
    pub icon_size: f32,
    pub panel_header_height: f32,
    pub space_unit: f32,
    pub border_width: f32,
}

impl Default for GeometryConfig {
    fn default() -> Self {
        Self {
            radius_small: 6.0,
            radius_medium: 10.0,
            radius_large: 18.0,
            radius_pill: 999.0,
            control_height: 34.0,
            compact_control_height: 30.0,
            icon_size: 16.0,
            panel_header_height: 52.0,
            space_unit: 4.0,
            border_width: 1.0,
        }
    }
}

impl GeometryConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        for (name, value, allow_zero) in [
            ("geometry.radius_small", self.radius_small, true),
            ("geometry.radius_medium", self.radius_medium, true),
            ("geometry.radius_large", self.radius_large, true),
            ("geometry.radius_pill", self.radius_pill, true),
            ("geometry.control_height", self.control_height, false),
            (
                "geometry.compact_control_height",
                self.compact_control_height,
                false,
            ),
            ("geometry.icon_size", self.icon_size, false),
            (
                "geometry.panel_header_height",
                self.panel_header_height,
                false,
            ),
            ("geometry.space_unit", self.space_unit, false),
            ("geometry.border_width", self.border_width, true),
        ] {
            validate_number(name, value, allow_zero, 2_048.0)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackdropEffect {
    None,
    WindowBlur,
    ElementBlur,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaterialStyle {
    pub opacity: f32,
    pub border_opacity: f32,
    pub highlight_opacity: f32,
    pub shadow_opacity: f32,
    pub backdrop: BackdropEffect,
}

impl MaterialStyle {
    fn resolve(
        self,
        translucency_allowed: bool,
        shadows_allowed: bool,
        increase_contrast: bool,
    ) -> ResolvedMaterial {
        let opacity = if translucency_allowed {
            self.opacity
        } else {
            1.0
        };
        ResolvedMaterial {
            opacity: if increase_contrast {
                (opacity + 0.12).min(1.0)
            } else {
                opacity
            },
            border_opacity: if increase_contrast {
                (self.border_opacity + 0.2).min(1.0)
            } else {
                self.border_opacity
            },
            highlight_opacity: if translucency_allowed {
                self.highlight_opacity
            } else {
                0.0
            },
            shadow_opacity: if shadows_allowed {
                self.shadow_opacity
            } else {
                0.0
            },
            backdrop: if translucency_allowed {
                self.backdrop
            } else {
                BackdropEffect::None
            },
        }
    }

    fn validate(&self, prefix: &str) -> Result<(), ConfigError> {
        for (name, value) in [
            ("opacity", self.opacity),
            ("border_opacity", self.border_opacity),
            ("highlight_opacity", self.highlight_opacity),
            ("shadow_opacity", self.shadow_opacity),
        ] {
            validate_number(&format!("materials.{prefix}.{name}"), value, true, 1.0)?;
        }
        Ok(())
    }
}

impl Default for MaterialStyle {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            border_opacity: 0.7,
            highlight_opacity: 0.0,
            shadow_opacity: 0.12,
            backdrop: BackdropEffect::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaterialConfig {
    pub window: MaterialStyle,
    pub chrome: MaterialStyle,
    pub surface: MaterialStyle,
    pub floating: MaterialStyle,
    pub control: MaterialStyle,
}

impl Default for MaterialConfig {
    fn default() -> Self {
        Self {
            window: MaterialStyle {
                opacity: 0.92,
                backdrop: BackdropEffect::WindowBlur,
                ..MaterialStyle::default()
            },
            chrome: MaterialStyle {
                opacity: 0.78,
                border_opacity: 0.55,
                highlight_opacity: 0.18,
                shadow_opacity: 0.08,
                backdrop: BackdropEffect::WindowBlur,
            },
            surface: MaterialStyle {
                opacity: 0.90,
                border_opacity: 0.62,
                highlight_opacity: 0.10,
                shadow_opacity: 0.08,
                backdrop: BackdropEffect::WindowBlur,
            },
            floating: MaterialStyle {
                opacity: 0.82,
                border_opacity: 0.72,
                highlight_opacity: 0.24,
                shadow_opacity: 0.18,
                backdrop: BackdropEffect::ElementBlur,
            },
            control: MaterialStyle {
                opacity: 0.68,
                border_opacity: 0.48,
                highlight_opacity: 0.16,
                shadow_opacity: 0.06,
                backdrop: BackdropEffect::ElementBlur,
            },
        }
    }
}

impl MaterialConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        self.window.validate("window")?;
        self.chrome.validate("chrome")?;
        self.surface.validate("surface")?;
        self.floating.validate("floating")?;
        self.control.validate("control")?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MotionConfig {
    pub fast_response: f32,
    pub standard_response: f32,
    pub gentle_response: f32,
    pub short_duration_ms: u32,
    pub medium_duration_ms: u32,
    pub tab_move_duration_ms: u32,
    pub spinner_duration_ms: u32,
    pub hover_handoff_delay_ms: u32,
    pub hover_close_delay_ms: u32,
    pub toc_leave_delay_ms: u32,
    pub feedback_duration_ms: u32,
    pub marquee_points_per_second: f32,
    pub marquee_min_duration_ms: u32,
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            fast_response: 28.0,
            standard_response: 22.0,
            gentle_response: 16.0,
            short_duration_ms: 180,
            medium_duration_ms: 320,
            tab_move_duration_ms: 260,
            spinner_duration_ms: 800,
            hover_handoff_delay_ms: 180,
            hover_close_delay_ms: 320,
            toc_leave_delay_ms: 120,
            feedback_duration_ms: 1_100,
            marquee_points_per_second: 22.0,
            marquee_min_duration_ms: 4_500,
        }
    }
}

impl MotionConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_number("motion.fast_response", self.fast_response, false, 200.0)?;
        validate_number(
            "motion.standard_response",
            self.standard_response,
            false,
            200.0,
        )?;
        validate_number("motion.gentle_response", self.gentle_response, false, 200.0)?;
        for (name, value) in [
            ("short_duration_ms", self.short_duration_ms),
            ("medium_duration_ms", self.medium_duration_ms),
            ("tab_move_duration_ms", self.tab_move_duration_ms),
            ("spinner_duration_ms", self.spinner_duration_ms),
            ("hover_handoff_delay_ms", self.hover_handoff_delay_ms),
            ("hover_close_delay_ms", self.hover_close_delay_ms),
            ("toc_leave_delay_ms", self.toc_leave_delay_ms),
            ("feedback_duration_ms", self.feedback_duration_ms),
            ("marquee_min_duration_ms", self.marquee_min_duration_ms),
        ] {
            if value > 10_000 {
                return Err(ConfigError::InvalidValue(format!(
                    "motion.{name} must be at most 10000ms"
                )));
            }
        }
        validate_number(
            "motion.marquee_points_per_second",
            self.marquee_points_per_second,
            false,
            1_000.0,
        )?;
        if self.short_duration_ms > self.medium_duration_ms {
            return Err(ConfigError::InvalidValue(
                "motion.short_duration_ms must not exceed medium_duration_ms".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResponsiveConfig {
    pub compact_max_width: f32,
    pub comfortable_min_width: f32,
}

impl Default for ResponsiveConfig {
    fn default() -> Self {
        Self {
            compact_max_width: 760.0,
            comfortable_min_width: 1_080.0,
        }
    }
}

impl ResponsiveConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_number(
            "responsive.compact_max_width",
            self.compact_max_width,
            false,
            100_000.0,
        )?;
        validate_number(
            "responsive.comfortable_min_width",
            self.comfortable_min_width,
            false,
            100_000.0,
        )?;
        if self.compact_max_width >= self.comfortable_min_width {
            return Err(ConfigError::InvalidValue(
                "responsive.compact_max_width must be below comfortable_min_width".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn classify(self, container_width: f32) -> WidthClass {
        if container_width <= self.compact_max_width {
            WidthClass::Compact
        } else if container_width >= self.comfortable_min_width {
            WidthClass::Comfortable
        } else {
            WidthClass::Regular
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WidthClass {
    Compact,
    Regular,
    Comfortable,
}

/// A typed value selected from the width of the component's own container.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsiveValue<T> {
    pub regular: T,
    #[serde(default)]
    pub compact: Option<T>,
    #[serde(default)]
    pub comfortable: Option<T>,
}

impl<T> ResponsiveValue<T> {
    #[must_use]
    pub fn resolve(&self, width: WidthClass) -> &T {
        match width {
            WidthClass::Compact => self.compact.as_ref().unwrap_or(&self.regular),
            WidthClass::Regular => &self.regular,
            WidthClass::Comfortable => self.comfortable.as_ref().unwrap_or(&self.regular),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InteractionState {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub disabled: bool,
    pub selected: bool,
    pub expanded: bool,
    pub dragging: bool,
}

/// Typed state overrides. Resolution order is deliberate: disabled, pressed,
/// dragging, selected, expanded, hover, focus, then the base value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateValue<T> {
    pub base: T,
    #[serde(default)]
    pub hovered: Option<T>,
    #[serde(default)]
    pub pressed: Option<T>,
    #[serde(default)]
    pub focused: Option<T>,
    #[serde(default)]
    pub disabled: Option<T>,
    #[serde(default)]
    pub selected: Option<T>,
    #[serde(default)]
    pub expanded: Option<T>,
    #[serde(default)]
    pub dragging: Option<T>,
}

impl<T> StateValue<T> {
    #[must_use]
    pub fn resolve(&self, state: InteractionState) -> &T {
        if state.disabled {
            self.disabled.as_ref().unwrap_or(&self.base)
        } else if state.pressed {
            self.pressed.as_ref().unwrap_or(&self.base)
        } else if state.dragging {
            self.dragging.as_ref().unwrap_or(&self.base)
        } else if state.selected {
            self.selected.as_ref().unwrap_or(&self.base)
        } else if state.expanded {
            self.expanded.as_ref().unwrap_or(&self.base)
        } else if state.hovered {
            self.hovered.as_ref().unwrap_or(&self.base)
        } else if state.focused {
            self.focused.as_ref().unwrap_or(&self.base)
        } else {
            &self.base
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum CornerShape {
    Square,
    Convex { radius: f32 },
    Concave { radius: f32 },
}

impl CornerShape {
    #[must_use]
    pub fn resolve(self, policy: ResolvedPolicy, maximum_radius: f32) -> Self {
        match self {
            Self::Square => Self::Square,
            Self::Convex { radius } if policy.curvature_allowed => Self::Convex {
                radius: radius.clamp(0.0, maximum_radius),
            },
            Self::Concave { radius } if policy.concave_corners_allowed => Self::Concave {
                radius: radius.clamp(0.0, maximum_radius),
            },
            Self::Convex { .. } | Self::Concave { .. } => Self::Square,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CornerSpec {
    pub top_left: CornerShape,
    pub top_right: CornerShape,
    pub bottom_right: CornerShape,
    pub bottom_left: CornerShape,
}

impl CornerSpec {
    #[must_use]
    pub const fn uniform_convex(radius: f32) -> Self {
        let corner = CornerShape::Convex { radius };
        Self {
            top_left: corner,
            top_right: corner,
            bottom_right: corner,
            bottom_left: corner,
        }
    }

    fn validate(self, prefix: &str) -> Result<(), ConfigError> {
        for (name, corner) in [
            ("top_left", self.top_left),
            ("top_right", self.top_right),
            ("bottom_right", self.bottom_right),
            ("bottom_left", self.bottom_left),
        ] {
            if let CornerShape::Convex { radius } | CornerShape::Concave { radius } = corner {
                validate_number(&format!("{prefix}.{name}.radius"), radius, true, 2_048.0)?;
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn resolve(self, policy: ResolvedPolicy, maximum_radius: f32) -> Self {
        Self {
            top_left: self.top_left.resolve(policy, maximum_radius),
            top_right: self.top_right.resolve(policy, maximum_radius),
            bottom_right: self.bottom_right.resolve(policy, maximum_radius),
            bottom_left: self.bottom_left.resolve(policy, maximum_radius),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AccessibilityPreferences {
    pub reduce_transparency: bool,
    pub increase_contrast: bool,
    pub reduce_motion: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDesignSystem {
    pub appearance: AppearanceConfig,
    pub policy: ResolvedPolicy,
    pub geometry: ResolvedGeometry,
    pub materials: ResolvedMaterials,
    pub palette: PaletteContrastConfig,
    pub workspace: WorkspaceLayoutConfig,
    pub typography: TypographyConfig,
    pub interaction: InteractionConfig,
    pub components: ComponentConfig,
    pub reader: ReaderUiConfig,
    pub motion: ResolvedMotion,
    pub responsive: ResponsiveConfig,
}

impl Default for ResolvedDesignSystem {
    fn default() -> Self {
        DesignSystemConfig::default().resolve(AccessibilityPreferences::default())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedPolicy {
    pub curvature_allowed: bool,
    pub concave_corners_allowed: bool,
    pub shadows_allowed: bool,
    pub translucency_allowed: bool,
    pub motion_allowed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedGeometry {
    pub radius_small: f32,
    pub radius_medium: f32,
    pub radius_large: f32,
    pub radius_pill: f32,
    pub control_height: f32,
    pub compact_control_height: f32,
    pub icon_size: f32,
    pub panel_header_height: f32,
    pub space_unit: f32,
    pub border_width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMaterials {
    pub window: ResolvedMaterial,
    pub chrome: ResolvedMaterial,
    pub surface: ResolvedMaterial,
    pub floating: ResolvedMaterial,
    pub control: ResolvedMaterial,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMaterial {
    pub opacity: f32,
    pub border_opacity: f32,
    pub highlight_opacity: f32,
    pub shadow_opacity: f32,
    pub backdrop: BackdropEffect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMotion {
    pub fast_response: Option<f32>,
    pub standard_response: Option<f32>,
    pub gentle_response: Option<f32>,
    pub short_duration_ms: u32,
    pub medium_duration_ms: u32,
    pub tab_move_duration_ms: Option<u32>,
    pub spinner_duration_ms: Option<u32>,
    pub hover_handoff_delay_ms: u32,
    pub hover_close_delay_ms: u32,
    pub toc_leave_delay_ms: u32,
    pub feedback_duration_ms: u32,
    pub marquee_points_per_second: Option<f32>,
    pub marquee_min_duration_ms: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    Parse(String),
    UnsupportedVersion { found: u16, supported: u16 },
    InvalidValue(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(formatter, "invalid design-system JSON: {error}"),
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "unsupported design-system schema version {found}; expected {supported}"
            ),
            Self::InvalidValue(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ConfigError {}

fn validate_number(
    name: &str,
    value: f32,
    allow_zero: bool,
    maximum: f32,
) -> Result<(), ConfigError> {
    if !value.is_finite() || value < 0.0 || (!allow_zero && value == 0.0) || value > maximum {
        return Err(ConfigError::InvalidValue(format!(
            "{name} must be {} and at most {maximum}",
            if allow_zero {
                "non-negative"
            } else {
                "positive"
            }
        )));
    }
    Ok(())
}

fn validate_positive_fields(prefix: &str, fields: &[(&str, f32)]) -> Result<(), ConfigError> {
    for (name, value) in fields {
        validate_number(&format!("{prefix}.{name}"), *value, false, 4_096.0)?;
    }
    Ok(())
}

fn validate_state_number(
    prefix: &str,
    value: &StateValue<f32>,
    maximum: f32,
) -> Result<(), ConfigError> {
    validate_number(&format!("{prefix}.base"), value.base, true, maximum)?;
    for (name, candidate) in [
        ("hovered", value.hovered),
        ("pressed", value.pressed),
        ("focused", value.focused),
        ("disabled", value.disabled),
        ("selected", value.selected),
        ("expanded", value.expanded),
        ("dragging", value.dragging),
    ] {
        if let Some(candidate) = candidate {
            validate_number(&format!("{prefix}.{name}"), candidate, true, maximum)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_GLASS: &str = include_str!("../../../assets/ui/key-glass.json");
    const CLEAR_GLASS: &str = include_str!("../../../assets/ui/variations/clear-glass.json");
    const SQUARE_OPAQUE: &str = include_str!("../../../assets/ui/variations/square-opaque.json");
    const SAFARI_CHROME: &str = include_str!("../../../assets/ui/variations/safari-chrome.json");
    const SAFARI_GLASS: &str = include_str!("../../../assets/ui/variations/safari-glass.json");
    const ADVENTURE_SAFARI: &str =
        include_str!("../../../assets/ui/variations/adventure-safari.json");

    #[test]
    fn default_configuration_round_trips_and_validates() {
        let source = serde_json::to_string_pretty(&DesignSystemConfig::default()).unwrap();
        let parsed = DesignSystemConfig::from_json(&source).unwrap();
        assert_eq!(parsed, DesignSystemConfig::default());
    }

    #[test]
    fn shipped_configurations_parse_and_exercise_distinct_policies() {
        let baseline = DesignSystemConfig::from_json(KEY_GLASS).unwrap();
        let clear = DesignSystemConfig::from_json(CLEAR_GLASS).unwrap();
        let square = DesignSystemConfig::from_json(SQUARE_OPAQUE).unwrap();
        let safari = DesignSystemConfig::from_json(SAFARI_CHROME).unwrap();
        let safari_glass = DesignSystemConfig::from_json(SAFARI_GLASS).unwrap();
        let adventure = DesignSystemConfig::from_json(ADVENTURE_SAFARI).unwrap();

        let baseline = baseline.resolve(AccessibilityPreferences::default());
        let clear = clear.resolve(AccessibilityPreferences::default());
        let square = square.resolve(AccessibilityPreferences::default());
        let safari = safari.resolve(AccessibilityPreferences::default());
        let safari_glass = safari_glass.resolve(AccessibilityPreferences::default());

        assert!(clear.materials.chrome.opacity < baseline.materials.chrome.opacity);
        assert!(clear.geometry.radius_large > baseline.geometry.radius_large);
        assert_eq!(square.geometry.radius_large, 0.0);
        assert_eq!(square.materials.chrome.opacity, 1.0);
        assert_eq!(square.materials.chrome.backdrop, BackdropEffect::None);
        assert_eq!(square.motion.standard_response, None);
        assert_eq!(
            safari.workspace.chrome.regular.row_order,
            ChromeRowOrder::ControlsThenTabs
        );
        assert_eq!(
            safari_glass.workspace.chrome.regular.row_order,
            ChromeRowOrder::ControlsThenTabs
        );
        for config in [&baseline, &clear, &square, &safari, &safari_glass] {
            assert_eq!(
                config.workspace.chrome.regular.utility_controls_row,
                ChromeUtilityControlsRow::Top
            );
        }
        assert!(!safari_glass.workspace.chrome.regular.utilities_in_tab_row());
        assert!(baseline.workspace.chrome.regular.utilities_in_tab_row());
        assert!(
            safari_glass.workspace.chrome.regular.split_segment_height
                < safari_glass.workspace.chrome.regular.tab_height
        );
        assert_eq!(
            safari_glass.materials.chrome.backdrop,
            BackdropEffect::WindowBlur
        );
        assert!(safari_glass.policy.translucency_allowed);
        assert_eq!(
            baseline.workspace.chrome.regular.row_order,
            ChromeRowOrder::TabsThenControls
        );
        assert_eq!(
            adventure.appearance.theme,
            ThemeSelection::Named {
                name: "Adventure".to_owned()
            }
        );
        assert_eq!(
            adventure.workspace.chrome.regular.utility_controls_row,
            ChromeUtilityControlsRow::Top
        );
        assert_eq!(
            adventure.workspace.chrome.regular.split_segment_height,
            24.0
        );
        assert_eq!(adventure.appearance.icons.tab_list, SemanticIcon::TabList);
    }

    #[test]
    fn root_policy_has_final_authority_over_component_values() {
        let mut config = DesignSystemConfig::default();
        config.policy.curvature = FeaturePolicy::Disabled;
        config.policy.shadows = FeaturePolicy::Disabled;
        config.policy.translucency = FeaturePolicy::Disabled;
        config.policy.motion = FeaturePolicy::Disabled;
        let resolved = config.resolve(AccessibilityPreferences::default());

        assert_eq!(resolved.geometry.radius_large, 0.0);
        assert_eq!(resolved.materials.floating.opacity, 1.0);
        assert_eq!(resolved.materials.floating.shadow_opacity, 0.0);
        assert_eq!(resolved.materials.floating.backdrop, BackdropEffect::None);
        assert_eq!(resolved.motion.standard_response, None);
        assert_eq!(resolved.motion.medium_duration_ms, 0);
        assert_eq!(resolved.motion.tab_move_duration_ms, None);
        assert_eq!(resolved.motion.spinner_duration_ms, None);
    }

    #[test]
    fn accessibility_preferences_strengthen_or_remove_effects() {
        let config = DesignSystemConfig::default();
        let resolved = config.resolve(AccessibilityPreferences {
            reduce_transparency: true,
            increase_contrast: true,
            reduce_motion: true,
        });
        assert_eq!(resolved.materials.chrome.opacity, 1.0);
        assert_eq!(resolved.materials.chrome.backdrop, BackdropEffect::None);
        assert!(resolved.materials.chrome.border_opacity > config.materials.chrome.border_opacity);
        assert_eq!(resolved.motion.fast_response, None);
    }

    #[test]
    fn malformed_and_unknown_configuration_is_rejected() {
        assert!(DesignSystemConfig::from_json("{\"schema_version\":99}").is_err());
        assert!(DesignSystemConfig::from_json("{\"unknown\":true}").is_err());
        let mut config = DesignSystemConfig::default();
        config.materials.floating.opacity = 1.5;
        assert!(config.validate().is_err());

        let mut config = DesignSystemConfig::default();
        config.palette.canvas_foreground_mix = 1.5;
        assert!(config.validate().is_err());

        let mut config = DesignSystemConfig::default();
        config.workspace.chrome.regular.tab_height = 80.0;
        assert!(config.validate().is_err());

        let mut config = DesignSystemConfig::default();
        config.workspace.chrome.regular.split_segment_height = 40.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn width_classes_are_stable_at_boundaries() {
        let responsive = ResponsiveConfig::default();
        assert_eq!(responsive.classify(760.0), WidthClass::Compact);
        assert_eq!(responsive.classify(900.0), WidthClass::Regular);
        assert_eq!(responsive.classify(1_080.0), WidthClass::Comfortable);
    }

    #[test]
    fn state_and_responsive_values_resolve_without_stringly_typed_selectors() {
        let responsive = ResponsiveValue {
            regular: 10,
            compact: Some(8),
            comfortable: Some(12),
        };
        assert_eq!(*responsive.resolve(WidthClass::Compact), 8);
        let state = StateValue {
            base: "base",
            hovered: Some("hover"),
            pressed: Some("pressed"),
            focused: None,
            disabled: Some("disabled"),
            selected: None,
            expanded: None,
            dragging: None,
        };
        assert_eq!(
            *state.resolve(InteractionState {
                hovered: true,
                pressed: true,
                ..InteractionState::default()
            }),
            "pressed"
        );
    }

    #[test]
    fn appearance_theme_colors_and_icon_roles_round_trip_as_typed_values() {
        let mut config = DesignSystemConfig::default();
        config.appearance.theme = ThemeSelection::Named {
            name: "Adventure".to_owned(),
        };
        config.appearance.colors.action_accent = Some(RgbaColor {
            red: 0.2,
            green: 0.4,
            blue: 0.8,
            alpha: 0.75,
        });
        config.appearance.icons.document = SemanticIcon::Settings;

        let json = serde_json::to_string(&config).unwrap();
        let parsed = DesignSystemConfig::from_json(&json).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(
            parsed.appearance.theme,
            ThemeSelection::Named {
                name: "Adventure".to_owned()
            }
        );
        assert_eq!(parsed.appearance.icons.document, SemanticIcon::Settings);
    }

    #[test]
    fn corner_policy_can_prohibit_component_level_convex_and_concave_shapes() {
        let mut config = DesignSystemConfig::default();
        config.components.corners.tab.top_right = CornerShape::Concave { radius: 8.0 };
        config.policy.curvature = FeaturePolicy::Disabled;
        let resolved = config.resolve(AccessibilityPreferences::default());
        assert_eq!(
            resolved.components.corners.tab.top_right,
            CornerShape::Square
        );
        let corners = CornerSpec {
            top_left: CornerShape::Convex { radius: 12.0 },
            top_right: CornerShape::Concave { radius: 8.0 },
            bottom_right: CornerShape::Square,
            bottom_left: CornerShape::Convex { radius: 4.0 },
        }
        .resolve(resolved.policy, config.policy.maximum_corner_radius);
        assert_eq!(corners.top_left, CornerShape::Square);
        assert_eq!(corners.top_right, CornerShape::Square);
        assert_eq!(corners.bottom_left, CornerShape::Square);
    }
}
