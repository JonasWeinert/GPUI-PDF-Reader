//! Bounded, immutable loading of installable `.keyext` archives and unpacked
//! development packages.
//!
//! Package contents are never extracted to disk. Archive and directory inputs
//! are normalized into the same logical representation and canonical SHA-256
//! content hash before signature policy is applied.

#![forbid(unsafe_code)]

mod digest;
mod loader;
mod package;
mod signature;

pub use digest::*;
pub use loader::*;
pub use package::*;
pub use signature::*;

#[cfg(test)]
mod tests;
