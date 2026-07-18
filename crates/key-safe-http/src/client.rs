use crate::network::{is_localhost_name, is_public_ip};
use crate::policy::canonical_host;
use crate::{
    CancellationToken, HttpError, HttpMethod, HttpPolicy, HttpRequest, HttpResponse, HttpTransport,
    RequestHeader, ResolveRequest, ResolvedEndpoint, Resolver, ResponseHeader, SystemResolver,
    TransportHeader, TransportRequest,
};
use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::{Host, Url};

const STREAM_CHUNK_BYTES: usize = 16 * 1024;

/// Monotonic time source used by the shared sliding-window rate limiter.
pub trait MonotonicClock: Send + Sync {
    /// Duration since an arbitrary stable epoch.
    fn now(&self) -> Duration;
}

/// Process-local monotonic clock.
#[derive(Debug)]
pub struct SystemClock {
    started: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemClock {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Cloneable bounded HTTP capability.
///
/// Clones share concurrency and rate limits. Every redirect is independently
/// checked against the scheme, exact-host, DNS, address, timeout, and rate
/// policy before a trusted transport sees it.
#[derive(Clone)]
pub struct SafeHttpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    policy: HttpPolicy,
    resolver: Arc<dyn Resolver>,
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn MonotonicClock>,
    active: AtomicUsize,
    rate_history: Mutex<VecDeque<Duration>>,
}

impl SafeHttpClient {
    /// Creates a client using the OS resolver and the address-pinning reqwest
    /// transport.
    #[must_use]
    pub fn new(policy: HttpPolicy) -> Self {
        Self::with_components(
            policy,
            Arc::new(SystemResolver),
            Arc::new(crate::ReqwestTransport),
            Arc::new(SystemClock::default()),
        )
    }

    /// Creates a client with explicit trusted boundaries.
    ///
    /// This constructor supports deterministic tests and alternative transports
    /// that can truly interrupt DNS/connect operations. The transport must obey
    /// [`HttpTransport`]'s fixed-address invariant.
    #[must_use]
    pub fn with_components(
        policy: HttpPolicy,
        resolver: Arc<dyn Resolver>,
        transport: Arc<dyn HttpTransport>,
        clock: Arc<dyn MonotonicClock>,
    ) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                policy,
                resolver,
                transport,
                clock,
                active: AtomicUsize::new(0),
                rate_history: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// Executes a request, buffering no more than the configured response cap.
    pub fn execute(
        &self,
        request: HttpRequest,
        cancellation: &CancellationToken,
    ) -> Result<HttpResponse, HttpError> {
        check_cancelled(cancellation)?;
        let _active = ActiveRequest::acquire(&self.inner)?;
        let (mut method, mut url, mut headers, mut body) = request.into_parts();
        let mut visited = HashSet::new();

        for redirect_count in 0..=self.inner.policy.limits().max_redirects {
            check_cancelled(cancellation)?;
            validate_request_size(
                method,
                &url,
                &headers,
                &body,
                self.inner.policy.limits().max_request_bytes,
            )?;
            let loop_key = redirect_loop_key(&url);
            if !visited.insert(loop_key) {
                return Err(HttpError::Redirect("redirect loop detected".to_owned()));
            }
            self.inner.acquire_rate_slot()?;
            let endpoint = self.resolve_endpoint(&url, cancellation)?;
            let transport_request = TransportRequest::new(
                method,
                url.clone(),
                headers.clone(),
                body.clone(),
                endpoint,
                self.inner.policy.limits().connect_timeout,
                self.inner.policy.limits().request_timeout,
                cancellation.clone(),
            );
            let response = self.inner.transport.execute(transport_request)?;
            check_cancelled(cancellation)?;
            let validated_headers = validate_response_headers(
                response.headers,
                self.inner.policy.limits().max_response_header_bytes,
            )?;

            if is_redirect(response.status) {
                if redirect_count == self.inner.policy.limits().max_redirects {
                    return Err(HttpError::Redirect(format!(
                        "more than {} hops",
                        self.inner.policy.limits().max_redirects
                    )));
                }
                let location = unique_utf8_header(&validated_headers, "location")?
                    .ok_or_else(|| HttpError::Redirect("missing Location header".to_owned()))?;
                let next = url
                    .join(location)
                    .map_err(|error| HttpError::Redirect(error.to_string()))?;
                if url.scheme() == "https"
                    && next.scheme() == "http"
                    && !self.inner.policy.allows_downgrade()
                {
                    return Err(HttpError::Redirect(
                        "HTTPS-to-HTTP downgrade is disabled".to_owned(),
                    ));
                }
                if !same_origin(&url, &next) {
                    headers.retain(|header| !header.is_sensitive());
                }
                if response.status == 303
                    || (matches!(response.status, 301 | 302) && method == HttpMethod::Post)
                {
                    method = HttpMethod::Get;
                    body.clear();
                    headers.retain(|header| {
                        !matches!(
                            header.name(),
                            "content-type" | "content-language" | "content-location"
                        )
                    });
                }
                url = next;
                continue;
            }

            validate_content_encoding(&validated_headers)?;
            if let Some(length) = content_length(&validated_headers)?
                && length > self.inner.policy.limits().max_response_bytes as u64
            {
                return Err(HttpError::ResponseTooLarge {
                    limit: self.inner.policy.limits().max_response_bytes,
                });
            }
            let content_type = self
                .inner
                .policy
                .content_types()
                .validate(unique_utf8_header(&validated_headers, "content-type")?)?;
            let response_body = if method == HttpMethod::Head {
                Vec::new()
            } else {
                read_bounded(
                    response.body,
                    self.inner.policy.limits().max_response_bytes,
                    cancellation,
                )?
            };
            let headers = validated_headers
                .into_iter()
                .map(|header| ResponseHeader::new(header.name, header.value))
                .collect();
            return Ok(HttpResponse::new(
                response.status,
                url,
                content_type,
                headers,
                response_body,
            ));
        }

        Err(HttpError::Redirect(
            "redirect state was exhausted".to_owned(),
        ))
    }

    fn resolve_endpoint(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<ResolvedEndpoint, HttpError> {
        let scheme = url.scheme();
        if !self.inner.policy.schemes().allows(scheme) {
            return Err(HttpError::SchemeDenied(scheme.to_owned()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(HttpError::InvalidUrl(
                "embedded credentials are not allowed".to_owned(),
            ));
        }
        let host = url
            .host()
            .ok_or_else(|| HttpError::InvalidUrl("URL has no host".to_owned()))?;
        let transport_host = url
            .host_str()
            .expect("a parsed host has a host string")
            .to_owned();
        let canonical = canonical_url_host(&host)?;
        if is_localhost_name(&canonical) {
            return Err(HttpError::LocalhostDenied(canonical));
        }
        if !self.inner.policy.hosts().allows(&canonical) {
            return Err(HttpError::HostDenied(canonical));
        }
        let port = url
            .port_or_known_default()
            .filter(|port| *port != 0)
            .ok_or_else(|| HttpError::InvalidUrl("URL has no usable port".to_owned()))?;

        let mut addresses = match host {
            Host::Domain(_) => self.inner.resolver.resolve(ResolveRequest::new(
                canonical.clone(),
                port,
                self.inner.policy.limits().resolve_timeout,
                cancellation.clone(),
            ))?,
            Host::Ipv4(address) => vec![IpAddr::V4(address)],
            Host::Ipv6(address) => vec![IpAddr::V6(address)],
        };
        check_cancelled(cancellation)?;
        if addresses.is_empty() {
            return Err(HttpError::Resolve(
                "host resolved to no addresses".to_owned(),
            ));
        }
        addresses.sort_unstable();
        addresses.dedup();
        for address in &addresses {
            if !is_public_ip(*address) {
                return Err(HttpError::AddressDenied(*address));
            }
        }
        Ok(ResolvedEndpoint::new(
            transport_host,
            port,
            addresses
                .into_iter()
                .map(|address| SocketAddr::new(address, port))
                .collect(),
        ))
    }
}

impl ClientInner {
    fn acquire_rate_slot(&self) -> Result<(), HttpError> {
        let now = self.clock.now();
        let rate = self.policy.limits().rate;
        let mut history = self
            .rate_history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while history
            .front()
            .is_some_and(|then| now.saturating_sub(*then) >= rate.window)
        {
            history.pop_front();
        }
        if history.len() >= rate.requests {
            let retry_after = history
                .front()
                .map(|then| rate.window.saturating_sub(now.saturating_sub(*then)))
                .unwrap_or(rate.window);
            return Err(HttpError::RateLimited { retry_after });
        }
        history.push_back(now);
        Ok(())
    }
}

struct ActiveRequest<'a> {
    active: &'a AtomicUsize,
}

impl<'a> ActiveRequest<'a> {
    fn acquire(inner: &'a ClientInner) -> Result<Self, HttpError> {
        let limit = inner.policy.limits().max_concurrent;
        inner
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < limit).then_some(active + 1)
            })
            .map_err(|_| HttpError::ConcurrencyLimit { limit })?;
        Ok(Self {
            active: &inner.active,
        })
    }
}

impl Drop for ActiveRequest<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn validate_request_size(
    method: HttpMethod,
    url: &Url,
    headers: &[RequestHeader],
    body: &[u8],
    limit: usize,
) -> Result<(), HttpError> {
    let method_bytes: usize = match method {
        HttpMethod::Get => 3,
        HttpMethod::Head | HttpMethod::Post => 4,
    };
    let mut bytes = method_bytes
        .checked_add(url.as_str().len())
        .and_then(|value| value.checked_add(body.len()))
        .ok_or(HttpError::RequestTooLarge { limit })?;
    for header in headers {
        bytes = bytes
            .checked_add(header.name().len())
            .and_then(|value| value.checked_add(header.value().len()))
            .and_then(|value| value.checked_add(4))
            .ok_or(HttpError::RequestTooLarge { limit })?;
    }
    if bytes > limit {
        return Err(HttpError::RequestTooLarge { limit });
    }
    Ok(())
}

fn validate_response_headers(
    headers: Vec<TransportHeader>,
    limit: usize,
) -> Result<Vec<TransportHeader>, HttpError> {
    let mut bytes = 0_usize;
    for header in &headers {
        if header.name.is_empty()
            || !header
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(HttpError::Transport(
                "response contains an invalid header name".to_owned(),
            ));
        }
        bytes = bytes
            .checked_add(header.name.len())
            .and_then(|value| value.checked_add(header.value.len()))
            .and_then(|value| value.checked_add(4))
            .ok_or(HttpError::ResponseHeadersTooLarge { limit })?;
        if bytes > limit {
            return Err(HttpError::ResponseHeadersTooLarge { limit });
        }
    }
    Ok(headers)
}

fn unique_utf8_header<'a>(
    headers: &'a [TransportHeader],
    name: &str,
) -> Result<Option<&'a str>, HttpError> {
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name));
    let Some(header) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(HttpError::Transport(format!(
            "response contains repeated `{name}` headers"
        )));
    }
    std::str::from_utf8(&header.value)
        .map(Some)
        .map_err(|_| HttpError::Transport(format!("response `{name}` header is not UTF-8")))
}

fn content_length(headers: &[TransportHeader]) -> Result<Option<u64>, HttpError> {
    unique_utf8_header(headers, "content-length")?
        .map(str::trim)
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| HttpError::Transport("invalid Content-Length header".to_owned()))
}

fn validate_content_encoding(headers: &[TransportHeader]) -> Result<(), HttpError> {
    if let Some(value) = unique_utf8_header(headers, "content-encoding")? {
        let value = value.trim();
        if !value.is_empty() && !value.eq_ignore_ascii_case("identity") {
            return Err(HttpError::ContentEncodingDenied(value.to_owned()));
        }
    }
    Ok(())
}

fn read_bounded(
    mut body: Box<dyn Read + Send>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, HttpError> {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; STREAM_CHUNK_BYTES];
    loop {
        check_cancelled(cancellation)?;
        let read = body
            .read(&mut chunk)
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > limit {
            return Err(HttpError::ResponseTooLarge { limit });
        }
        output.extend_from_slice(&chunk[..read]);
    }
    check_cancelled(cancellation)?;
    Ok(output)
}

fn canonical_url_host(host: &Host<&str>) -> Result<String, HttpError> {
    match host {
        Host::Domain(domain) => canonical_host(domain),
        Host::Ipv4(address) => Ok(address.to_string()),
        Host::Ipv6(address) => Ok(address.to_string()),
    }
}

fn redirect_loop_key(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), HttpError> {
    if cancellation.is_cancelled() {
        Err(HttpError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CancellationSource, ContentTypePolicy, ContentTypeRule, ExactHostAllowlist, HttpLimits,
        RateLimit, SchemePolicy, TransportResponse,
    };
    use std::collections::{HashMap, VecDeque};
    use std::io::{Cursor, Read};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::thread;

    const PUBLIC_ONE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34));
    const PUBLIC_TWO: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1));

    #[derive(Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn advance(&self, duration: Duration) {
            self.0.fetch_add(
                u64::try_from(duration.as_millis()).unwrap(),
                Ordering::AcqRel,
            );
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::Acquire))
        }
    }

    #[derive(Default)]
    struct FakeResolver {
        answers: Mutex<HashMap<String, Vec<IpAddr>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeResolver {
        fn answer(&self, host: &str, addresses: Vec<IpAddr>) {
            self.answers
                .lock()
                .unwrap()
                .insert(host.to_owned(), addresses);
        }
    }

    impl Resolver for FakeResolver {
        fn resolve(&self, request: ResolveRequest) -> Result<Vec<IpAddr>, HttpError> {
            if request.cancellation().is_cancelled() {
                return Err(HttpError::Cancelled);
            }
            self.calls.lock().unwrap().push(request.host().to_owned());
            self.answers
                .lock()
                .unwrap()
                .get(request.host())
                .cloned()
                .ok_or_else(|| HttpError::Resolve("no fake answer".to_owned()))
        }
    }

    struct ResponseSpec {
        status: u16,
        headers: Vec<TransportHeader>,
        body: Box<dyn Read + Send>,
    }

    impl ResponseSpec {
        fn json(status: u16, body: &[u8]) -> Self {
            Self {
                status,
                headers: vec![TransportHeader {
                    name: "content-type".to_owned(),
                    value: b"application/json".to_vec(),
                }],
                body: Box::new(Cursor::new(body.to_vec())),
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                status: 302,
                headers: vec![TransportHeader {
                    name: "location".to_owned(),
                    value: location.as_bytes().to_vec(),
                }],
                body: Box::new(Cursor::new(Vec::new())),
            }
        }
    }

    #[derive(Default)]
    struct FakeTransport {
        responses: Mutex<VecDeque<ResponseSpec>>,
        requests: Mutex<Vec<TransportRequest>>,
    }

    impl FakeTransport {
        fn push(&self, response: ResponseSpec) {
            self.responses.lock().unwrap().push_back(response);
        }
    }

    impl HttpTransport for FakeTransport {
        fn execute(&self, request: TransportRequest) -> Result<TransportResponse, HttpError> {
            self.requests.lock().unwrap().push(request);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| HttpError::Transport("no fake response".to_owned()))?;
            Ok(TransportResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            })
        }
    }

    fn policy(hosts: &[&str]) -> HttpPolicy {
        HttpPolicy::new(
            ExactHostAllowlist::new(hosts.iter().copied()).unwrap(),
            ContentTypePolicy::json(),
        )
        .with_schemes(SchemePolicy::HttpAndHttps)
    }

    fn make_client(
        policy: HttpPolicy,
        resolver: Arc<FakeResolver>,
        transport: Arc<FakeTransport>,
        clock: Arc<FakeClock>,
    ) -> SafeHttpClient {
        SafeHttpClient::with_components(policy, resolver, transport, clock)
    }

    #[test]
    fn passes_only_resolved_public_addresses_to_transport() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("api.example.com", vec![PUBLIC_ONE, PUBLIC_TWO, PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::json(200, br#"{"ok":true}"#));
        let client = make_client(
            policy(&["api.example.com"]),
            Arc::clone(&resolver),
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );

        let response = client
            .execute(
                HttpRequest::get("https://api.example.com/work").unwrap(),
                &CancellationToken::active(),
            )
            .unwrap();

        assert_eq!(response.body(), br#"{"ok":true}"#);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .endpoint()
                .addresses()
                .iter()
                .map(|address| address.ip())
                .collect::<Vec<_>>(),
            vec![PUBLIC_TWO, PUBLIC_ONE]
        );
    }

    #[test]
    fn trailing_dot_permission_normalization_still_pins_the_exact_url_host() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("api.example.com", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::json(200, b"{}"));
        let client = make_client(
            policy(&["api.example.com"]),
            resolver,
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );

        client
            .execute(
                HttpRequest::get("https://api.example.com./work").unwrap(),
                &CancellationToken::active(),
            )
            .unwrap();

        assert_eq!(
            transport.requests.lock().unwrap()[0].endpoint().host(),
            "api.example.com."
        );
    }

    #[test]
    fn rejects_mixed_public_private_dns_answer_before_transport() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer(
            "api.example.com",
            vec![PUBLIC_ONE, "127.0.0.1".parse().unwrap()],
        );
        let transport = Arc::new(FakeTransport::default());
        let client = make_client(
            policy(&["api.example.com"]),
            resolver,
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );

        assert!(matches!(
            client.execute(
                HttpRequest::get("https://api.example.com").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::AddressDenied(_))
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn public_ip_literal_is_validated_without_dns_and_remains_pinned() {
        let resolver = Arc::new(FakeResolver::default());
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::json(200, b"{}"));
        let address = "2606:4700:4700::1111".parse::<IpAddr>().unwrap();
        let client = make_client(
            policy(&["[2606:4700:4700::1111]"]),
            Arc::clone(&resolver),
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );

        client
            .execute(
                HttpRequest::get("https://[2606:4700:4700::1111]/dns-query").unwrap(),
                &CancellationToken::active(),
            )
            .unwrap();

        assert!(resolver.calls.lock().unwrap().is_empty());
        assert_eq!(
            transport.requests.lock().unwrap()[0].endpoint().addresses()[0].ip(),
            address
        );
    }

    #[test]
    fn redirect_revalidates_host_and_new_dns_answer() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        resolver.answer("two.example", vec![PUBLIC_TWO]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::redirect("https://two.example/final"));
        transport.push(ResponseSpec::json(200, b"{}"));
        let client = make_client(
            policy(&["one.example", "two.example"]),
            Arc::clone(&resolver),
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );

        let response = client
            .execute(
                HttpRequest::get("https://one.example/start").unwrap(),
                &CancellationToken::active(),
            )
            .unwrap();

        assert_eq!(response.final_url().as_str(), "https://two.example/final");
        assert_eq!(
            *resolver.calls.lock().unwrap(),
            vec!["one.example", "two.example"]
        );
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].endpoint().addresses()[0].ip(), PUBLIC_ONE);
        assert_eq!(requests[1].endpoint().addresses()[0].ip(), PUBLIC_TWO);
    }

    #[test]
    fn cross_origin_redirect_drops_sensitive_request_headers() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        resolver.answer("two.example", vec![PUBLIC_TWO]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::redirect("https://two.example/final"));
        transport.push(ResponseSpec::json(200, b"{}"));
        let client = make_client(
            policy(&["one.example", "two.example"]),
            resolver,
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );
        let request = HttpRequest::get("https://one.example/start")
            .unwrap()
            .with_header("authorization", "Bearer secret")
            .unwrap()
            .with_header("x-trace", "kept")
            .unwrap();

        client
            .execute(request, &CancellationToken::active())
            .unwrap();

        let requests = transport.requests.lock().unwrap();
        assert!(
            requests[0]
                .headers()
                .iter()
                .any(|header| header.name() == "authorization")
        );
        assert!(
            requests[1]
                .headers()
                .iter()
                .all(|header| header.name() != "authorization")
        );
        assert!(
            requests[1]
                .headers()
                .iter()
                .any(|header| header.name() == "x-trace")
        );
    }

    #[test]
    fn redirect_cannot_escape_exact_allowlist_or_reach_private_literal() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::redirect("https://evil.example/final"));
        let client = make_client(
            policy(&["one.example"]),
            Arc::clone(&resolver),
            Arc::clone(&transport),
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::HostDenied(_))
        ));

        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::redirect("http://127.0.0.1/private"));
        let client = make_client(
            policy(&["one.example"]),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(
            client
                .execute(
                    HttpRequest::get("http://one.example").unwrap(),
                    &CancellationToken::active(),
                )
                .is_err()
        );
    }

    #[test]
    fn https_downgrade_requires_a_separate_grant() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::redirect("http://one.example/final"));
        let client = make_client(
            policy(&["one.example"]),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::Redirect(_))
        ));
    }

    #[test]
    fn response_body_header_and_media_type_limits_are_enforced() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let limits = HttpLimits {
            max_response_bytes: 3,
            max_response_header_bytes: 64,
            ..HttpLimits::default()
        };
        let bounded_policy = policy(&["one.example"]).with_limits(limits).unwrap();
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::json(200, b"four"));
        let client = make_client(
            bounded_policy,
            Arc::clone(&resolver),
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ResponseTooLarge { limit: 3 })
        ));

        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![TransportHeader {
                name: "content-type".to_owned(),
                value: b"text/html".to_vec(),
            }],
            body: Box::new(Cursor::new(Vec::new())),
        });
        let client = make_client(
            policy(&["one.example"]),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ContentTypeDenied(_))
        ));
    }

    #[test]
    fn declared_length_header_budget_and_encoding_are_rejected_before_body_use() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);

        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![
                TransportHeader {
                    name: "content-type".to_owned(),
                    value: b"application/json".to_vec(),
                },
                TransportHeader {
                    name: "content-length".to_owned(),
                    value: b"999".to_vec(),
                },
            ],
            body: Box::new(Cursor::new(Vec::new())),
        });
        let limits = HttpLimits {
            max_response_bytes: 8,
            ..HttpLimits::default()
        };
        let client = make_client(
            policy(&["one.example"]).with_limits(limits).unwrap(),
            Arc::clone(&resolver),
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ResponseTooLarge { limit: 8 })
        ));

        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![TransportHeader {
                name: "x-oversized".to_owned(),
                value: vec![b'x'; 80],
            }],
            body: Box::new(Cursor::new(Vec::new())),
        });
        let limits = HttpLimits {
            max_response_header_bytes: 32,
            ..HttpLimits::default()
        };
        let client = make_client(
            policy(&["one.example"]).with_limits(limits).unwrap(),
            Arc::clone(&resolver),
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ResponseHeadersTooLarge { limit: 32 })
        ));

        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![
                TransportHeader {
                    name: "content-type".to_owned(),
                    value: b"application/json".to_vec(),
                },
                TransportHeader {
                    name: "content-encoding".to_owned(),
                    value: b"gzip".to_vec(),
                },
            ],
            body: Box::new(Cursor::new(Vec::new())),
        });
        let client = make_client(
            policy(&["one.example"]),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        assert_eq!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ContentEncodingDenied("gzip".to_owned()))
        );
    }

    struct CancelOnFirstRead {
        source: CancellationSource,
        returned: bool,
    }

    impl Read for CancelOnFirstRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.returned {
                return Ok(0);
            }
            self.returned = true;
            buffer[0] = b'x';
            self.source.cancel();
            Ok(1)
        }
    }

    #[test]
    fn cancellation_interrupts_response_streaming() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        let source = CancellationSource::new();
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![TransportHeader {
                name: "content-type".to_owned(),
                value: b"application/json".to_vec(),
            }],
            body: Box::new(CancelOnFirstRead {
                source: source.clone(),
                returned: false,
            }),
        });
        let client = make_client(
            policy(&["one.example"]),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        assert_eq!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &source.token(),
            ),
            Err(HttpError::Cancelled)
        );
    }

    #[test]
    fn rate_limit_is_shared_and_recovers_after_window() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec::json(200, b"{}"));
        transport.push(ResponseSpec::json(200, b"{}"));
        let clock = Arc::new(FakeClock::default());
        let limits = HttpLimits {
            rate: RateLimit::new(1, Duration::from_secs(10)).unwrap(),
            ..HttpLimits::default()
        };
        let client = make_client(
            policy(&["one.example"]).with_limits(limits).unwrap(),
            resolver,
            transport,
            Arc::clone(&clock),
        );
        let request = || HttpRequest::get("https://one.example").unwrap();
        client
            .execute(request(), &CancellationToken::active())
            .unwrap();
        assert!(matches!(
            client
                .clone()
                .execute(request(), &CancellationToken::active()),
            Err(HttpError::RateLimited { .. })
        ));
        clock.advance(Duration::from_secs(10));
        client
            .execute(request(), &CancellationToken::active())
            .unwrap();
    }

    struct BlockingTransport {
        entered: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl HttpTransport for BlockingTransport {
        fn execute(&self, _request: TransportRequest) -> Result<TransportResponse, HttpError> {
            let (entered_lock, entered_signal) = &*self.entered;
            *entered_lock.lock().unwrap() = true;
            entered_signal.notify_one();
            let (release_lock, release_signal) = &*self.release;
            let mut released = release_lock.lock().unwrap();
            while !*released {
                released = release_signal.wait(released).unwrap();
            }
            Ok(TransportResponse {
                status: 200,
                headers: vec![TransportHeader {
                    name: "content-type".to_owned(),
                    value: b"application/json".to_vec(),
                }],
                body: Box::new(Cursor::new(b"{}".to_vec())),
            })
        }
    }

    #[test]
    fn concurrency_limit_is_shared_by_client_clones() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let transport = Arc::new(BlockingTransport {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let limits = HttpLimits {
            max_concurrent: 1,
            ..HttpLimits::default()
        };
        let client = SafeHttpClient::with_components(
            policy(&["one.example"]).with_limits(limits).unwrap(),
            resolver,
            transport,
            Arc::new(FakeClock::default()),
        );
        let worker_client = client.clone();
        let worker = thread::spawn(move || {
            worker_client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            )
        });
        let (entered_lock, entered_signal) = &*entered;
        let mut did_enter = entered_lock.lock().unwrap();
        while !*did_enter {
            did_enter = entered_signal.wait(did_enter).unwrap();
        }
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::ConcurrencyLimit { limit: 1 })
        ));
        let (release_lock, release_signal) = &*release;
        *release_lock.lock().unwrap() = true;
        release_signal.notify_one();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn structured_content_type_and_request_cap_have_independent_rules() {
        let resolver = Arc::new(FakeResolver::default());
        resolver.answer("one.example", vec![PUBLIC_ONE]);
        let transport = Arc::new(FakeTransport::default());
        transport.push(ResponseSpec {
            status: 200,
            headers: vec![TransportHeader {
                name: "content-type".to_owned(),
                value: b"application/problem+json; charset=utf-8".to_vec(),
            }],
            body: Box::new(Cursor::new(b"{}".to_vec())),
        });
        let client = make_client(
            policy(&["one.example"]),
            Arc::clone(&resolver),
            transport,
            Arc::new(FakeClock::default()),
        );
        assert!(
            client
                .execute(
                    HttpRequest::get("https://one.example").unwrap(),
                    &CancellationToken::active(),
                )
                .is_ok()
        );

        let transport = Arc::new(FakeTransport::default());
        let limits = HttpLimits {
            max_request_bytes: 32,
            ..HttpLimits::default()
        };
        let custom_content =
            ContentTypePolicy::new([ContentTypeRule::exact("application/json").unwrap()], false)
                .unwrap();
        let limited = HttpPolicy::new(
            ExactHostAllowlist::new(["one.example"]).unwrap(),
            custom_content,
        )
        .with_limits(limits)
        .unwrap();
        let client = make_client(limited, resolver, transport, Arc::new(FakeClock::default()));
        assert!(matches!(
            client.execute(
                HttpRequest::get("https://one.example/a/very/long/path").unwrap(),
                &CancellationToken::active(),
            ),
            Err(HttpError::RequestTooLarge { limit: 32 })
        ));
    }
}
