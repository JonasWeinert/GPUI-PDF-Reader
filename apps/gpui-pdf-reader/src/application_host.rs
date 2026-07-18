//! Process-global application services and workspace ownership.
//!
//! The first migration step moves heavyweight extension lifecycle state out of
//! individual PDF views. Window/item/view registries and the shared PDF engine
//! supervisor are added here in subsequent, independently testable steps.

use crate::app_extensions::ReaderExtensions;
use std::{cell::RefCell, rc::Rc};

/// Long-lived application owner. This is a GPUI entity without a rendered
/// surface so views can reach shared services without globals or OS handles.
pub(crate) struct ApplicationHost {
    extensions: Rc<RefCell<ReaderExtensions>>,
}

impl ApplicationHost {
    pub(crate) fn new(extensions: ReaderExtensions) -> Self {
        Self {
            extensions: Rc::new(RefCell::new(extensions)),
        }
    }

    pub(crate) fn extensions(&self) -> Rc<RefCell<ReaderExtensions>> {
        self.extensions.clone()
    }
}
