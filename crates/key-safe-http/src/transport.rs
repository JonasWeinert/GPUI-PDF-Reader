use crate::{CancellationToken, HttpError, HttpMethod, RequestHeader, TimeoutStage};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use url::{Host, Url};

/// Input to a trusted hostname resolver.
#[derive(Clone, Debug)]
pub struct ResolveRequest {
    host: String,
    port: u16,
    timeout: Duration,
    cancellation: CancellationToken,
}

impl ResolveRequest {
    pub(crate) fn new(
        host: String,
        port: u16,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            host,
            port,
            timeout,
            cancellation,
        }
    }

    /// Canonical DNS hostname (never a URL literal).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Effective URL port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Maximum resolution time.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Cancellation signal for this operation.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

/// Resolver boundary used to make DNS validation deterministic and testable.
pub trait Resolver: Send + Sync {
    /// Resolves a hostname. The client subsequently rejects any non-public
    /// result and supplies only the approved addresses to its transport.
    fn resolve(&self, request: ResolveRequest) -> Result<Vec<IpAddr>, HttpError>;
}

/// OS resolver with a caller-visible timeout and cooperative cancellation.
///
/// The platform resolver itself is blocking and cannot always be interrupted;
/// a timed-out lookup thread may finish in the background. It cannot issue an
/// HTTP request and its result is discarded.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, request: ResolveRequest) -> Result<Vec<IpAddr>, HttpError> {
        if request.cancellation.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let host = request.host;
        let port = request.port;
        let timeout = request.timeout;
        let cancellation = request.cancellation;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("key-safe-http-resolve".to_owned())
            .spawn(move || {
                let result = (host.as_str(), port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.map(|address| address.ip()).collect::<Vec<_>>())
                    .map_err(|error| HttpError::Resolve(error.to_string()));
                let _ = sender.send(result);
            })
            .map_err(|error| HttpError::Resolve(error.to_string()))?;

        let started = Instant::now();
        loop {
            if cancellation.is_cancelled() {
                return Err(HttpError::Cancelled);
            }
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return Err(HttpError::Timeout(TimeoutStage::Resolve));
            };
            match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(HttpError::Resolve(
                        "platform resolver ended without a result".to_owned(),
                    ));
                }
            }
        }
    }
}

/// A host and its already validated, fixed socket addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl ResolvedEndpoint {
    pub(crate) fn new(host: String, port: u16, addresses: Vec<SocketAddr>) -> Self {
        Self {
            host,
            port,
            addresses,
        }
    }

    /// URL host spelling used by the transport's fixed-address override.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Effective URL port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Public addresses approved for this one hop.
    #[must_use]
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
}

/// One validated request hop passed to a trusted transport.
#[derive(Clone, Debug)]
pub struct TransportRequest {
    method: HttpMethod,
    url: Url,
    headers: Vec<RequestHeader>,
    body: Vec<u8>,
    endpoint: ResolvedEndpoint,
    connect_timeout: Duration,
    request_timeout: Duration,
    cancellation: CancellationToken,
}

impl TransportRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        method: HttpMethod,
        url: Url,
        headers: Vec<RequestHeader>,
        body: Vec<u8>,
        endpoint: ResolvedEndpoint,
        connect_timeout: Duration,
        request_timeout: Duration,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            method,
            url,
            headers,
            body,
            endpoint,
            connect_timeout,
            request_timeout,
            cancellation,
        }
    }

    /// Request method.
    #[must_use]
    pub fn method(&self) -> HttpMethod {
        self.method
    }

    /// Fully validated URL for this hop.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Caller headers after redirect sanitization.
    #[must_use]
    pub fn headers(&self) -> &[RequestHeader] {
        &self.headers
    }

    /// Bounded request body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Endpoint whose addresses have passed public-address validation.
    #[must_use]
    pub fn endpoint(&self) -> &ResolvedEndpoint {
        &self.endpoint
    }

    /// Connection timeout.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// Complete per-hop timeout.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Cancellation signal.
    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

/// Raw response header returned by a trusted transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportHeader {
    /// Lowercase or original header name. The client compares names
    /// case-insensitively and counts all bytes.
    pub name: String,
    /// Untrusted raw value bytes.
    pub value: Vec<u8>,
}

/// A transport response whose body is streamed and bounded by the client.
pub struct TransportResponse {
    /// Numeric HTTP status.
    pub status: u16,
    /// Untrusted response headers.
    pub headers: Vec<TransportHeader>,
    /// Streaming response body.
    pub body: Box<dyn Read + Send>,
}

/// HTTP transport boundary.
///
/// Security requirement: implementations must connect only to
/// `request.endpoint().addresses()` while preserving the original URL host for
/// HTTP Host and TLS SNI. Resolving the hostname again would reopen a DNS
/// rebinding window. This invariant is enforced by the bundled reqwest
/// transport and observable by deterministic fake transports, but Rust traits
/// cannot prove it for arbitrary third-party implementations.
pub trait HttpTransport: Send + Sync {
    /// Executes exactly one hop. Redirect following belongs to the safe client.
    fn execute(&self, request: TransportRequest) -> Result<TransportResponse, HttpError>;
}

/// Direct reqwest transport with redirects and environment proxies disabled.
///
/// A fresh client pins the validated socket addresses for each hop. The
/// blocking reqwest send cannot be interrupted on every platform, so
/// cancellation latency while connecting is bounded by the configured
/// timeouts. Response streaming remains cooperatively cancellable.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestTransport;

impl HttpTransport for ReqwestTransport {
    fn execute(&self, request: TransportRequest) -> Result<TransportResponse, HttpError> {
        if request.cancellation.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let mut client_builder = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(request.connect_timeout)
            .timeout(request.request_timeout);
        if matches!(request.url.host(), Some(Host::Domain(_))) {
            client_builder = client_builder
                .resolve_to_addrs(request.endpoint.host(), request.endpoint.addresses());
        }
        let client = client_builder
            .build()
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Head => reqwest::Method::HEAD,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = client
            .request(method, request.url.clone())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        for header in &request.headers {
            let name = HeaderName::from_bytes(header.name().as_bytes())
                .map_err(|error| HttpError::InvalidRequest(error.to_string()))?;
            let value = HeaderValue::from_str(header.value())
                .map_err(|error| HttpError::InvalidRequest(error.to_string()))?;
            builder = builder.header(name, value);
        }
        if !request.body.is_empty() {
            builder = builder.body(request.body);
        }
        let response = builder.send().map_err(|error| {
            if error.is_timeout() {
                HttpError::Timeout(TimeoutStage::Request)
            } else {
                HttpError::Transport(error.to_string())
            }
        })?;
        if request.cancellation.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| TransportHeader {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect();
        Ok(TransportResponse {
            status,
            headers,
            body: Box::new(response),
        })
    }
}
