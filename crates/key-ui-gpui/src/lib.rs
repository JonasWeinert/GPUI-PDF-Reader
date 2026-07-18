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

pub use transition::UnitTransition;

#[cfg(target_os = "macos")]
mod controls;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod tabs;
#[cfg(target_os = "macos")]
mod theme;

#[cfg(target_os = "macos")]
pub use controls::{ChromeButtonStyle, chrome_button, close_button, icon_button, icon_label};
#[cfg(target_os = "macos")]
pub use panel::{PanelHeader, PanelShell, PanelShellStyle};
#[cfg(target_os = "macos")]
pub use tabs::{
    TAB_BAR_HEIGHT, TabBarAction, TabHoverCard, TabIndexAction, TabPresentation, TabSearchPopover,
    TabStrip,
};
#[cfg(target_os = "macos")]
pub use theme::{ActionTokens, ContentTokens, StatusTokens, SurfaceTokens, ThemeTokens};
