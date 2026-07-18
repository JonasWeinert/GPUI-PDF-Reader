//! Runtime-neutral orchestration for native and declarative Key extensions.
//!
//! The host owns validation, activation, permissions, capability negotiation,
//! event dispatch, effect arbitration, contributions, diagnostics, safe mode,
//! and failure isolation. Rendering engines and platform UI deliberately live
//! outside this crate.

#![forbid(unsafe_code)]

mod diagnostic;
mod host;
mod native;
mod registry;

pub use diagnostic::*;
pub use host::*;
pub use native::*;
pub use registry::*;
