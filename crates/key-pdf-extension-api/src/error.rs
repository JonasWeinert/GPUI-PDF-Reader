use std::{error::Error, fmt};

use key_extension_api::GenerationId;
use serde::{Deserialize, Serialize};

use crate::PdfCapability;

/// Structured failures shared by native providers and runtime adapters.
///
/// Messages in `Internal` and `Unsupported` are diagnostic text, never an
/// executable payload. Adapters should preserve the typed variants across
/// their transport instead of reducing failures to strings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PdfExtensionError {
    /// The requested capability was not linked or granted.
    CapabilityUnavailable {
        /// Capability that could not be used.
        capability: PdfCapability,
    },
    /// The user or application policy denied an otherwise available action.
    PermissionDenied {
        /// Capability whose use was denied.
        capability: PdfCapability,
    },
    /// No document is currently open in the provider's scope.
    NoActiveDocument,
    /// A resource belongs to a document generation that is no longer current.
    StaleGeneration {
        /// Generation currently owned by the host.
        expected: GenerationId,
        /// Generation carried by the resource.
        actual: GenerationId,
    },
    /// A generic resource token had the wrong semantic kind.
    InvalidHandleKind {
        /// Required kind name.
        expected: String,
        /// Received kind name.
        actual: String,
    },
    /// A resource token was well formed but is unknown to the host.
    ResourceNotFound {
        /// Semantic resource kind.
        kind: String,
        /// Opaque host-issued resource identity.
        id: u64,
    },
    /// A page index does not exist in the addressed document.
    PageOutOfBounds {
        /// Requested zero-based page index.
        page: u32,
        /// Number of pages in the document.
        page_count: u32,
    },
    /// A field was malformed or semantically invalid.
    InvalidInput {
        /// Stable dotted path to the invalid field.
        field: String,
        /// Machine-readable reason.
        reason: InputErrorKind,
    },
    /// A bounded contract input exceeded its negotiated host limit.
    LimitExceeded {
        /// Stable dotted path to the oversized value.
        field: String,
        /// Maximum accepted units.
        limit: u64,
        /// Received units.
        actual: u64,
    },
    /// Work was cancelled before a result could be published.
    Cancelled,
    /// The service is temporarily unable to accept another operation.
    Busy,
    /// The host does not implement a requested optional semantic behavior.
    Unsupported {
        /// Short feature identifier suitable for diagnostics.
        feature: String,
    },
    /// A trusted provider failed without exposing implementation details.
    Internal {
        /// Sanitized diagnostic message.
        message: String,
    },
}

/// Machine-readable reasons for invalid contract input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputErrorKind {
    /// A value was not finite.
    NotFinite,
    /// A normalized coordinate was outside zero through one.
    OutsideNormalizedRange,
    /// Rectangle edges were reversed or had no area.
    EmptyOrReversedRect,
    /// A required string was empty.
    Empty,
    /// A text range was reversed.
    ReversedRange,
    /// An identifier used a reserved value.
    InvalidIdentifier,
    /// A Unicode scalar value was invalid.
    InvalidUnicodeScalar,
    /// A revision did not advance monotonically.
    StaleRevision,
}

/// Result alias used by all PDF capability providers.
pub type PdfExtensionResult<T> = Result<T, PdfExtensionError>;

impl fmt::Display for PdfExtensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityUnavailable { capability } => {
                write!(
                    formatter,
                    "capability {} is unavailable",
                    capability.as_str()
                )
            }
            Self::PermissionDenied { capability } => {
                write!(formatter, "permission denied for {}", capability.as_str())
            }
            Self::NoActiveDocument => formatter.write_str("no PDF document is active"),
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "stale PDF generation {} (current generation is {})",
                actual.0, expected.0
            ),
            Self::InvalidHandleKind { expected, actual } => {
                write!(formatter, "expected a {expected} handle, received {actual}")
            }
            Self::ResourceNotFound { kind, id } => {
                write!(formatter, "unknown {kind} resource {id}")
            }
            Self::PageOutOfBounds { page, page_count } => {
                write!(formatter, "page {page} is outside {page_count} pages")
            }
            Self::InvalidInput { field, reason } => {
                write!(formatter, "invalid {field}: {reason:?}")
            }
            Self::LimitExceeded {
                field,
                limit,
                actual,
            } => write!(
                formatter,
                "{field} exceeds its limit of {limit} units (received {actual})"
            ),
            Self::Cancelled => formatter.write_str("PDF capability operation was cancelled"),
            Self::Busy => formatter.write_str("PDF capability service is busy"),
            Self::Unsupported { feature } => write!(formatter, "unsupported feature: {feature}"),
            Self::Internal { message } => write!(formatter, "PDF provider failed: {message}"),
        }
    }
}

impl Error for PdfExtensionError {}
