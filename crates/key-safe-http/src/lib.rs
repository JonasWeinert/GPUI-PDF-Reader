//! Capability-scoped, bounded HTTP services for Key applications.
//!
//! The crate is intentionally independent of PDF, UI, and extension-runtime
//! types. A host creates one [`SafeHttpClient`] per granted network capability
//! and one [`DocumentCache`] per open document generation. Exact hostname,
//! scheme, redirect, public-address, media-type, byte, timeout, rate, and
//! concurrency rules remain host-owned.
//!
//! DNS rebinding needs cooperation from the transport: after [`Resolver`]
//! returns, this crate validates every address and passes a [`ResolvedEndpoint`]
//! to [`HttpTransport`]. The bundled [`ReqwestTransport`] pins those addresses
//! and disables proxies and automatic redirects. A custom transport is trusted
//! to obey the same invariant; the type system cannot inspect its socket calls.

mod cache;
mod cancellation;
mod client;
mod error;
mod network;
mod policy;
mod request;
mod transport;

pub use cache::{DocumentCache, DocumentCacheEntry, DocumentCacheLimits, DocumentCacheUsage};
pub use cancellation::{CancellationSource, CancellationToken};
pub use client::{MonotonicClock, SafeHttpClient, SystemClock};
pub use error::{HttpError, TimeoutStage};
pub use network::is_public_ip;
pub use policy::{
    ContentTypePolicy, ContentTypeRule, ExactHostAllowlist, HttpLimits, HttpPolicy, RateLimit,
    SchemePolicy,
};
pub use request::{HttpMethod, HttpRequest, HttpResponse, RequestHeader, ResponseHeader};
pub use transport::{
    HttpTransport, ReqwestTransport, ResolveRequest, ResolvedEndpoint, Resolver, SystemResolver,
    TransportHeader, TransportRequest, TransportResponse,
};
