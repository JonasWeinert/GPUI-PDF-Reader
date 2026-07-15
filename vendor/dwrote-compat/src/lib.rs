//! This package is intentionally empty.
//!
//! `zed-font-kit` declares `dwrote` only for Windows. GPUI PDF Reader currently
//! supports macOS, so Cargo resolves this package but never compiles it.
compile_error!("GPUI PDF Reader's dwrote compatibility package must not be built on this target");
