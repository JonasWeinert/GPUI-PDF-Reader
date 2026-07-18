//! Trusted GPUI presentation for validated extension contribution data.
//!
//! Extensions never receive GPUI handles or rendering callbacks. This crate
//! resolves their bounded data and maps it to host-owned components, themes,
//! focus behavior, actions, and native menu structures.

#![forbid(unsafe_code)]

mod state;

pub use state::*;

#[cfg(target_os = "macos")]
mod icon;
#[cfg(target_os = "macos")]
mod menu;
#[cfg(target_os = "macos")]
mod view;

#[cfg(target_os = "macos")]
pub use icon::*;
#[cfg(target_os = "macos")]
pub use menu::*;
#[cfg(target_os = "macos")]
pub use view::*;
