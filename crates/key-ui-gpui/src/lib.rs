//! Shared, application-neutral GPUI presentation primitives for Key apps.
//!
//! The crate deliberately contains only low-level contracts that can be used
//! by the standalone PDF reader and by future applications. Product-specific
//! layout, document colors, actions, and state remain in their owning crates.
//!
//! [`UnitTransition`] is platform-independent. GPUI presentation modules are
//! currently exposed on macOS, matching the workspace's supported GPUI target.

#![forbid(unsafe_code)]

mod transition;

pub use key_ui_core::{
    AccessibilityPreferences, AppearanceConfig, BackdropEffect, ChromeLayoutConfig, ChromeRowOrder,
    ChromeUtilityControlsRow, CommonComponentConfig, ComponentConfig, ComponentCornerConfig,
    ConfigError as DesignSystemConfigError, ControlBarComponentConfig, CornerShape, CornerSpec,
    DesignSystemConfig, FeatureComponentConfig, FeaturePolicy, FontWeightToken, IconRoleConfig,
    InteractionConfig, InteractionState, PaletteContrastConfig, PopoverComponentConfig,
    ReaderUiConfig, ResolvedDesignSystem, RgbaColor, SemanticColorConfig, SemanticIcon,
    SplitLayoutConfig, StateValue, TextStyleConfig, ThemeSelection, TypographyConfig, WidthClass,
    WorkspaceLayoutConfig,
};
pub use transition::UnitTransition;

#[cfg(target_os = "macos")]
mod context_bar;
mod control_bar_layout;
#[cfg(target_os = "macos")]
mod controls;
#[cfg(target_os = "macos")]
mod hover_card;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod style;
#[cfg(target_os = "macos")]
mod tabs;
#[cfg(target_os = "macos")]
mod theme;

#[cfg(target_os = "macos")]
pub use context_bar::{CONTEXT_BAR_HEIGHT, WorkspaceContextBar};
pub use control_bar_layout::{
    ControlBarDisplayMode, ControlBarLayoutItem, solve_control_bar_layout,
};
#[cfg(target_os = "macos")]
pub use controls::{ChromeButtonStyle, chrome_button, close_button, icon_button, icon_label};
#[cfg(target_os = "macos")]
pub use hover_card::HoverCardShell;
#[cfg(target_os = "macos")]
pub use panel::{PanelHeader, PanelShell, PanelShellStyle};
#[cfg(target_os = "macos")]
pub use style::{DesignStyled, ElevationRole, RadiusRole, TypographyRole};
#[cfg(target_os = "macos")]
pub use tabs::{
    TabBarAction, TabDragPayload, TabDropAction, TabHoverAction, TabHoverCard, TabIndexAction,
    TabPresentation, TabSearchPopover, TabSegmentAction, TabSegmentPresentation, TabStrip,
    tab_hover_card_x, tab_hover_card_y, tab_search_popover_x, workspace_utility_controls,
};
#[cfg(target_os = "macos")]
pub use theme::{
    ActionTokens, ContentTokens, MaterialToken, MaterialTokens, StatusTokens, SurfaceTokens,
    ThemeTokens, configured_color, install_design_system, resolved_design_system, semantic_icon,
};
