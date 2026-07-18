//! Optional sandboxed execution for Key WebAssembly components.
//!
//! Compile without the `wasmtime-runtime` feature to keep Wasmtime and its JIT
//! compiler completely outside a minimal application bundle.

#![forbid(unsafe_code)]

mod config;
mod diagnostic;

#[cfg(feature = "wasmtime-runtime")]
mod host_adapter;
#[cfg(feature = "wasmtime-runtime")]
mod runtime;
#[cfg(feature = "wasmtime-runtime")]
mod worker;

pub use config::*;
pub use diagnostic::*;

#[cfg(feature = "wasmtime-runtime")]
pub use host_adapter::*;
#[cfg(feature = "wasmtime-runtime")]
pub use runtime::*;

#[must_use]
pub const fn runtime_available() -> bool {
    cfg!(feature = "wasmtime-runtime")
}
