//! Product-agnostic workspace contracts shared by Key applications.
//!
//! This crate intentionally knows nothing about GPUI, PDFium, or a particular
//! application. It describes identity, placement, scheduling, and resource
//! policy so hosts and feature crates can agree without depending on one
//! another's implementations.

mod control_bar;
mod ids;
mod layout;
mod model;
mod resource_registry;
mod resources;
mod scheduler;

pub use control_bar::*;
pub use ids::*;
pub use layout::*;
pub use model::*;
pub use resource_registry::*;
pub use resources::*;
pub use scheduler::*;
