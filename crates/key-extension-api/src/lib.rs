//! Runtime-neutral contracts shared by Key extension hosts and adapters.
//!
//! This crate intentionally contains no GPUI, PDFium, Wasmtime, filesystem,
//! socket, or operating-system types. Extensions describe semantic intent;
//! trusted hosts decide whether and how to execute it.

#![forbid(unsafe_code)]

mod capability;
mod id;
mod manifest;
mod protocol;
mod ui;
mod validation;
mod value;

pub use capability::*;
pub use id::*;
pub use manifest::*;
pub use protocol::*;
pub use ui::*;
pub use validation::*;
pub use value::*;
