//! Pre-stable, engine- and UI-neutral PDF contracts for Key extensions.
//!
//! This crate is a semantic boundary between extension runtimes and a trusted
//! PDF host. It intentionally exposes no `PDFium`, `GPUI`, filesystem, network, or
//! WebAssembly-runtime types. All resource handles are opaque and scoped to one
//! document generation; every collection and string accepted from an extension
//! has an explicit validation limit.

#![forbid(unsafe_code)]

mod capability;
mod error;
mod handle;
mod navigation;
mod overlay;
mod service;
mod snapshot;
mod validation;

pub use capability::*;
pub use error::*;
pub use handle::*;
pub use navigation::*;
pub use overlay::*;
pub use service::*;
pub use snapshot::*;
pub use validation::*;
