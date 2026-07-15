//! A deliberately small, independently written compatibility surface.
//!
//! GPUI's Blade renderer does not call its C header generator, but GPUI 0.2.2
//! still type-checks that dormant function. Keeping these types local avoids
//! pulling an MPL-licensed build tool into an otherwise permissively licensed
//! application. `Builder::generate()` returns an error if it is ever reached.

use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub enum Language {
    #[default]
    C,
}

#[derive(Clone, Debug, Default)]
pub struct ExportConfig {
    pub include: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EnumConfig {
    pub prefix_with_name: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub include_guard: Option<String>,
    pub language: Language,
    pub no_includes: bool,
    pub export: ExportConfig,
    pub enumeration: EnumConfig,
}

#[derive(Debug, Default)]
pub struct Builder;

impl Builder {
    pub fn new() -> Self {
        Self
    }

    pub fn with_src(self, _path: impl AsRef<Path>) -> Self {
        self
    }

    pub fn with_config(self, _config: Config) -> Self {
        self
    }

    pub fn generate(self) -> Result<Bindings, Error> {
        Err(Error)
    }
}

#[derive(Debug)]
pub struct Bindings;

impl Bindings {
    pub fn write_to_file(&self, _path: impl AsRef<Path>) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Error;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disabled GPUI C header generator was unexpectedly invoked")
    }
}

impl std::error::Error for Error {}
