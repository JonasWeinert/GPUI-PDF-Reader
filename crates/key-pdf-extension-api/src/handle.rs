use key_extension_api::{GenerationId, ResourceHandle};
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{InputErrorKind, PdfExtensionError, PdfExtensionResult};

const DOCUMENT_KIND: &str = "key.pdf.document";
const PAGE_KIND: &str = "key.pdf.page";
const OVERLAY_KIND: &str = "key.pdf.overlay-set";

/// Common behavior of resources tied to one open-document generation.
pub trait GenerationScoped {
    /// Generation that issued the resource.
    fn generation(&self) -> GenerationId;
}

macro_rules! opaque_handle {
    ($(#[$meta:meta])* $name:ident, $kind:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            generation: GenerationId,
            id: u64,
        }

        impl $name {
            /// Creates a host-issued resource token.
            ///
            /// An ID is opaque and conveys no authority by itself. Providers
            /// must still verify that it exists in the current generation.
            ///
            /// # Errors
            ///
            /// Returns `InvalidInput` when `id` is the reserved zero value.
            pub fn from_host_parts(
                generation: GenerationId,
                id: u64,
            ) -> PdfExtensionResult<Self> {
                if id == 0 {
                    return Err(PdfExtensionError::InvalidInput {
                        field: concat!(stringify!($name), ".id").into(),
                        reason: InputErrorKind::InvalidIdentifier,
                    });
                }
                Ok(Self { generation, id })
            }

            /// Returns the opaque host identity for adapter bookkeeping.
            #[must_use]
            pub const fn id(self) -> u64 {
                self.id
            }

            /// Converts this typed token to the generic extension resource form.
            #[must_use]
            pub fn to_resource_handle(self) -> ResourceHandle {
                ResourceHandle {
                    kind: $kind.into(),
                    generation: self.generation,
                    id: self.id,
                }
            }

            /// Validates and converts a generic extension resource token.
            ///
            /// # Errors
            ///
            /// Returns `InvalidHandleKind` for a different resource type or
            /// `InvalidInput` for the reserved zero identity.
            pub fn try_from_resource_handle(raw: ResourceHandle) -> PdfExtensionResult<Self> {
                if raw.kind != $kind {
                    return Err(PdfExtensionError::InvalidHandleKind {
                        expected: $kind.into(),
                        actual: raw.kind,
                    });
                }
                Self::from_host_parts(raw.generation, raw.id)
            }
        }

        impl GenerationScoped for $name {
            fn generation(&self) -> GenerationId {
                self.generation
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct WireHandle {
                    generation: GenerationId,
                    id: u64,
                }

                let wire = WireHandle::deserialize(deserializer)?;
                Self::from_host_parts(wire.generation, wire.id).map_err(de::Error::custom)
            }
        }
    };
}

opaque_handle!(
    /// Opaque authority for one opened PDF document.
    DocumentHandle,
    DOCUMENT_KIND
);
opaque_handle!(
    /// Opaque authority for one page in an opened PDF document.
    PageHandle,
    PAGE_KIND
);
opaque_handle!(
    /// Opaque identity for one accepted extension-owned overlay set.
    OverlaySetHandle,
    OVERLAY_KIND
);

/// Validator for rejecting resources from closed or replaced documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationScope {
    current: GenerationId,
}

impl GenerationScope {
    /// Creates a scope for the host's current document generation.
    #[must_use]
    pub const fn new(current: GenerationId) -> Self {
        Self { current }
    }

    /// Returns the generation accepted by this scope.
    #[must_use]
    pub const fn current(self) -> GenerationId {
        self.current
    }

    /// Rejects a resource issued by an older or unrelated generation.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` when the resource does not belong to this
    /// scope's active document generation.
    pub fn validate<T: GenerationScoped>(&self, resource: &T) -> PdfExtensionResult<()> {
        let actual = resource.generation();
        if actual == self.current {
            Ok(())
        } else {
            Err(PdfExtensionError::StaleGeneration {
                expected: self.current,
                actual,
            })
        }
    }
}
