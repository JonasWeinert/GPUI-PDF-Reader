use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

/// Stage at which a bounded operation timed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutStage {
    /// Hostname resolution exceeded its budget.
    Resolve,
    /// Connecting to the validated endpoint exceeded its budget.
    Connect,
    /// The complete HTTP exchange exceeded its budget.
    Request,
}

/// A policy, validation, resource, or transport failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpError {
    /// A host policy or media-type rule was malformed.
    InvalidPolicy(String),
    /// The request itself was malformed.
    InvalidRequest(String),
    /// The URL could not be parsed as an absolute URL.
    InvalidUrl(String),
    /// The URL scheme was not granted by policy.
    SchemeDenied(String),
    /// The exact hostname was not granted by policy.
    HostDenied(String),
    /// The host is reserved for local resolution.
    LocalhostDenied(String),
    /// DNS or a URL literal produced a non-public address.
    AddressDenied(IpAddr),
    /// Name resolution failed or produced no addresses.
    Resolve(String),
    /// A configured deadline expired.
    Timeout(TimeoutStage),
    /// The caller cancelled the operation.
    Cancelled,
    /// The request exceeded its byte budget.
    RequestTooLarge {
        /// Configured upper bound.
        limit: usize,
    },
    /// The response headers exceeded their byte budget.
    ResponseHeadersTooLarge {
        /// Configured upper bound.
        limit: usize,
    },
    /// The response body exceeded its byte budget.
    ResponseTooLarge {
        /// Configured upper bound.
        limit: usize,
    },
    /// The response omitted a required media type.
    ContentTypeMissing,
    /// The response media type was not allowed.
    ContentTypeDenied(String),
    /// Encoded responses are disabled so byte limits apply to what callers see.
    ContentEncodingDenied(String),
    /// A redirect was malformed, repeated, downgraded, or exceeded its limit.
    Redirect(String),
    /// The client's in-flight request limit was reached.
    ConcurrencyLimit {
        /// Configured upper bound.
        limit: usize,
    },
    /// The sliding-window request rate was exhausted.
    RateLimited {
        /// Best-effort duration until a slot becomes available.
        retry_after: Duration,
    },
    /// The trusted transport failed.
    Transport(String),
    /// A document cache key or value was invalid.
    Cache(String),
    /// A cache quota would have been exceeded.
    CacheQuotaExceeded {
        /// Name of the exhausted quota.
        quota: &'static str,
        /// Configured upper bound.
        limit: usize,
    },
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(message) => write!(formatter, "invalid HTTP policy: {message}"),
            Self::InvalidRequest(message) => write!(formatter, "invalid HTTP request: {message}"),
            Self::InvalidUrl(message) => write!(formatter, "invalid HTTP URL: {message}"),
            Self::SchemeDenied(scheme) => write!(formatter, "URL scheme `{scheme}` is not allowed"),
            Self::HostDenied(host) => write!(formatter, "host `{host}` is not allowed"),
            Self::LocalhostDenied(host) => write!(formatter, "local host `{host}` is not allowed"),
            Self::AddressDenied(address) => {
                write!(formatter, "address `{address}` is not publicly routable")
            }
            Self::Resolve(message) => write!(formatter, "host resolution failed: {message}"),
            Self::Timeout(stage) => write!(formatter, "HTTP {stage:?} timed out"),
            Self::Cancelled => formatter.write_str("HTTP operation was cancelled"),
            Self::RequestTooLarge { limit } => {
                write!(formatter, "HTTP request exceeds its {limit}-byte limit")
            }
            Self::ResponseHeadersTooLarge { limit } => {
                write!(
                    formatter,
                    "HTTP response headers exceed their {limit}-byte limit"
                )
            }
            Self::ResponseTooLarge { limit } => {
                write!(formatter, "HTTP response exceeds its {limit}-byte limit")
            }
            Self::ContentTypeMissing => formatter.write_str("HTTP response has no content type"),
            Self::ContentTypeDenied(value) => {
                write!(
                    formatter,
                    "HTTP response content type `{value}` is not allowed"
                )
            }
            Self::ContentEncodingDenied(value) => {
                write!(formatter, "HTTP response encoding `{value}` is not allowed")
            }
            Self::Redirect(message) => write!(formatter, "HTTP redirect rejected: {message}"),
            Self::ConcurrencyLimit { limit } => {
                write!(formatter, "HTTP concurrency limit of {limit} was reached")
            }
            Self::RateLimited { retry_after } => {
                write!(formatter, "HTTP request rate limited for {retry_after:?}")
            }
            Self::Transport(message) => write!(formatter, "HTTP transport failed: {message}"),
            Self::Cache(message) => write!(formatter, "document cache error: {message}"),
            Self::CacheQuotaExceeded { quota, limit } => {
                write!(
                    formatter,
                    "document cache {quota} quota of {limit} was exceeded"
                )
            }
        }
    }
}

impl std::error::Error for HttpError {}
