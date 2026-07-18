use crate::HttpError;
use crate::network::{is_localhost_name, is_public_ip};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;
use url::Host;

const HARD_MAX_REDIRECTS: usize = 10;
const HARD_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const HARD_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const HARD_MAX_HEADER_BYTES: usize = 128 * 1024;
const HARD_MAX_CONCURRENCY: usize = 32;
const HARD_MAX_REQUESTS_PER_WINDOW: usize = 10_000;
const HARD_MAX_TIMEOUT: Duration = Duration::from_secs(120);
const HARD_MAX_RATE_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// An immutable, exact hostname permission set.
///
/// Entries are normalized through the URL parser (including IDNA) and compared
/// exactly. Granting `example.org` does not grant `api.example.org` or
/// `example.org.attacker.test`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactHostAllowlist {
    hosts: BTreeSet<String>,
}

impl ExactHostAllowlist {
    /// Builds an allowlist from exact DNS names or public IP literals.
    pub fn new<I, S>(hosts: I) -> Result<Self, HttpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = BTreeSet::new();
        for host in hosts {
            let host = canonical_host(host.as_ref())?;
            if is_localhost_name(&host) {
                return Err(HttpError::InvalidPolicy(
                    "localhost cannot be granted network access".to_owned(),
                ));
            }
            if let Ok(address) = host.parse::<IpAddr>()
                && !is_public_ip(address)
            {
                return Err(HttpError::AddressDenied(address));
            }
            normalized.insert(host);
        }
        if normalized.is_empty() {
            return Err(HttpError::InvalidPolicy(
                "at least one exact host is required".to_owned(),
            ));
        }
        Ok(Self { hosts: normalized })
    }

    /// Reports whether this exact normalized host was granted.
    #[must_use]
    pub fn allows(&self, host: &str) -> bool {
        canonical_host(host)
            .ok()
            .is_some_and(|host| self.hosts.contains(&host))
    }

    /// Iterates over canonical host strings.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }
}

/// The URL schemes a capability permits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemePolicy {
    /// Permit HTTPS only.
    HttpsOnly,
    /// Permit both HTTP and HTTPS.
    HttpAndHttps,
}

impl SchemePolicy {
    pub(crate) fn allows(self, scheme: &str) -> bool {
        scheme == "https" || (self == Self::HttpAndHttps && scheme == "http")
    }
}

/// A single bounded media-type match rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentTypeRule {
    /// Match one complete media type, such as `application/json`.
    Exact(String),
    /// Match every subtype for one top-level type, such as `image/*`.
    Type(String),
    /// Match a structured suffix, such as `application/problem+json` for
    /// suffix `json`.
    StructuredSuffix(String),
}

impl ContentTypeRule {
    /// Creates an exact media-type rule.
    pub fn exact(value: impl Into<String>) -> Result<Self, HttpError> {
        let value = canonical_media_type(&value.into())?;
        Ok(Self::Exact(value))
    }

    /// Creates a top-level wildcard rule without accepting `*/*`.
    pub fn type_wildcard(value: impl Into<String>) -> Result<Self, HttpError> {
        let value = value.into().trim().to_ascii_lowercase();
        if !is_media_token(&value) {
            return Err(HttpError::InvalidPolicy(format!(
                "invalid content-type category `{value}`"
            )));
        }
        Ok(Self::Type(value))
    }

    /// Creates a structured-suffix rule (for example, `json`).
    pub fn structured_suffix(value: impl Into<String>) -> Result<Self, HttpError> {
        let value = value.into().trim().to_ascii_lowercase();
        if !is_media_token(&value) {
            return Err(HttpError::InvalidPolicy(format!(
                "invalid content-type suffix `{value}`"
            )));
        }
        Ok(Self::StructuredSuffix(value))
    }

    fn matches(&self, media_type: &str) -> bool {
        match self {
            Self::Exact(expected) => media_type == expected,
            Self::Type(expected) => media_type
                .split_once('/')
                .is_some_and(|(category, _)| category == expected),
            Self::StructuredSuffix(expected) => media_type
                .split_once('/')
                .is_some_and(|(_, subtype)| subtype.ends_with(&format!("+{expected}"))),
        }
    }
}

/// Media types accepted from a remote endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentTypePolicy {
    rules: Vec<ContentTypeRule>,
    allow_missing: bool,
}

impl ContentTypePolicy {
    /// Creates a non-empty set of media-type rules.
    pub fn new<I>(rules: I, allow_missing: bool) -> Result<Self, HttpError>
    where
        I: IntoIterator<Item = ContentTypeRule>,
    {
        let rules = rules.into_iter().collect::<Vec<_>>();
        if rules.is_empty() {
            return Err(HttpError::InvalidPolicy(
                "at least one content-type rule is required".to_owned(),
            ));
        }
        Ok(Self {
            rules,
            allow_missing,
        })
    }

    /// Returns a strict JSON policy, including structured `+json` responses.
    pub fn json() -> Self {
        Self {
            rules: vec![
                ContentTypeRule::Exact("application/json".to_owned()),
                ContentTypeRule::StructuredSuffix("json".to_owned()),
            ],
            allow_missing: false,
        }
    }

    pub(crate) fn validate(&self, value: Option<&str>) -> Result<Option<String>, HttpError> {
        let Some(value) = value else {
            return if self.allow_missing {
                Ok(None)
            } else {
                Err(HttpError::ContentTypeMissing)
            };
        };
        let media_type = canonical_media_type(
            value
                .split(';')
                .next()
                .expect("split always returns one item"),
        )?;
        if !self.rules.iter().any(|rule| rule.matches(&media_type)) {
            return Err(HttpError::ContentTypeDenied(media_type));
        }
        Ok(Some(media_type))
    }
}

/// Sliding-window request allowance shared by clones of a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    /// Maximum transport hops in the window.
    pub requests: usize,
    /// Sliding-window duration.
    pub window: Duration,
}

impl RateLimit {
    /// Creates and validates a rate limit.
    pub fn new(requests: usize, window: Duration) -> Result<Self, HttpError> {
        if requests == 0 || requests > HARD_MAX_REQUESTS_PER_WINDOW {
            return Err(HttpError::InvalidPolicy(format!(
                "rate limit must be between 1 and {HARD_MAX_REQUESTS_PER_WINDOW}"
            )));
        }
        if window.is_zero() || window > HARD_MAX_RATE_WINDOW {
            return Err(HttpError::InvalidPolicy(format!(
                "rate window must be greater than zero and at most {HARD_MAX_RATE_WINDOW:?}"
            )));
        }
        Ok(Self { requests, window })
    }
}

/// Byte, time, redirect, and parallelism budgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpLimits {
    /// Complete request URL, headers, and body budget.
    pub max_request_bytes: usize,
    /// Response header budget.
    pub max_response_header_bytes: usize,
    /// Decoded response body budget.
    pub max_response_bytes: usize,
    /// Maximum redirect hops.
    pub max_redirects: usize,
    /// Maximum simultaneous top-level calls.
    pub max_concurrent: usize,
    /// DNS resolution deadline.
    pub resolve_timeout: Duration,
    /// Transport connection deadline.
    pub connect_timeout: Duration,
    /// Complete transport exchange deadline per hop.
    pub request_timeout: Duration,
    /// Sliding-window transport-hop allowance.
    pub rate: RateLimit,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 256 * 1024,
            max_response_header_bytes: 32 * 1024,
            max_response_bytes: 4 * 1024 * 1024,
            max_redirects: 5,
            max_concurrent: 4,
            resolve_timeout: Duration::from_secs(4),
            connect_timeout: Duration::from_secs(4),
            request_timeout: Duration::from_secs(10),
            rate: RateLimit {
                requests: 30,
                window: Duration::from_secs(60),
            },
        }
    }
}

impl HttpLimits {
    fn validate(&self) -> Result<(), HttpError> {
        validate_size(
            "request byte limit",
            self.max_request_bytes,
            HARD_MAX_REQUEST_BYTES,
        )?;
        validate_size(
            "response header byte limit",
            self.max_response_header_bytes,
            HARD_MAX_HEADER_BYTES,
        )?;
        validate_size(
            "response byte limit",
            self.max_response_bytes,
            HARD_MAX_RESPONSE_BYTES,
        )?;
        if self.max_redirects > HARD_MAX_REDIRECTS {
            return Err(HttpError::InvalidPolicy(format!(
                "redirect limit cannot exceed {HARD_MAX_REDIRECTS}"
            )));
        }
        if self.max_concurrent == 0 || self.max_concurrent > HARD_MAX_CONCURRENCY {
            return Err(HttpError::InvalidPolicy(format!(
                "concurrency must be between 1 and {HARD_MAX_CONCURRENCY}"
            )));
        }
        validate_duration("resolve timeout", self.resolve_timeout)?;
        validate_duration("connect timeout", self.connect_timeout)?;
        validate_duration("request timeout", self.request_timeout)?;
        RateLimit::new(self.rate.requests, self.rate.window)?;
        Ok(())
    }
}

/// Immutable policy applied to every request and redirect hop.
#[derive(Clone, Debug)]
pub struct HttpPolicy {
    hosts: ExactHostAllowlist,
    schemes: SchemePolicy,
    content_types: ContentTypePolicy,
    limits: HttpLimits,
    allow_https_downgrade: bool,
}

impl HttpPolicy {
    /// Creates a strict policy with default resource limits.
    pub fn new(hosts: ExactHostAllowlist, content_types: ContentTypePolicy) -> Self {
        Self {
            hosts,
            schemes: SchemePolicy::HttpsOnly,
            content_types,
            limits: HttpLimits::default(),
            allow_https_downgrade: false,
        }
    }

    /// Chooses whether plain HTTP is permitted at all.
    #[must_use]
    pub fn with_schemes(mut self, schemes: SchemePolicy) -> Self {
        self.schemes = schemes;
        self
    }

    /// Applies explicit byte, time, redirect, rate, and concurrency limits.
    pub fn with_limits(mut self, limits: HttpLimits) -> Result<Self, HttpError> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }

    /// Allows an HTTPS request to redirect to HTTP.
    ///
    /// This remains off even when HTTP is otherwise allowed and should be
    /// enabled only for a narrowly reviewed capability.
    #[must_use]
    pub fn allowing_https_downgrade(mut self, allow: bool) -> Self {
        self.allow_https_downgrade = allow;
        self
    }

    /// Exact host permissions.
    #[must_use]
    pub fn hosts(&self) -> &ExactHostAllowlist {
        &self.hosts
    }

    /// Configured limits.
    #[must_use]
    pub fn limits(&self) -> &HttpLimits {
        &self.limits
    }

    pub(crate) fn schemes(&self) -> SchemePolicy {
        self.schemes
    }

    pub(crate) fn content_types(&self) -> &ContentTypePolicy {
        &self.content_types
    }

    pub(crate) fn allows_downgrade(&self) -> bool {
        self.allow_https_downgrade
    }
}

pub(crate) fn canonical_host(value: &str) -> Result<String, HttpError> {
    let value = value.trim();
    if value.is_empty()
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
        || value.contains(['/', '@', '?', '#'])
    {
        return Err(HttpError::InvalidPolicy(format!(
            "invalid exact host `{value}`"
        )));
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let host = Host::parse(value)
        .map_err(|error| HttpError::InvalidPolicy(format!("invalid exact host: {error}")))?;
    let canonical = match host {
        Host::Domain(domain) => domain.trim_end_matches('.').to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => address.to_string(),
    };
    if canonical.is_empty() {
        return Err(HttpError::InvalidPolicy("host cannot be empty".to_owned()));
    }
    Ok(canonical)
}

fn canonical_media_type(value: &str) -> Result<String, HttpError> {
    let value = value.trim().to_ascii_lowercase();
    let Some((category, subtype)) = value.split_once('/') else {
        return Err(HttpError::InvalidPolicy(format!(
            "invalid media type `{value}`"
        )));
    };
    if !is_media_token(category) || !is_media_token(subtype) || subtype.contains('/') {
        return Err(HttpError::InvalidPolicy(format!(
            "invalid media type `{value}`"
        )));
    }
    Ok(value)
}

fn is_media_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

fn validate_size(name: &str, value: usize, maximum: usize) -> Result<(), HttpError> {
    if value == 0 || value > maximum {
        return Err(HttpError::InvalidPolicy(format!(
            "{name} must be between 1 and {maximum}"
        )));
    }
    Ok(())
}

fn validate_duration(name: &str, value: Duration) -> Result<(), HttpError> {
    if value.is_zero() || value > HARD_MAX_TIMEOUT {
        return Err(HttpError::InvalidPolicy(format!(
            "{name} must be greater than zero and at most {HARD_MAX_TIMEOUT:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_allowlist_normalizes_without_granting_subdomains() {
        let hosts =
            ExactHostAllowlist::new(["B\u{dc}cher.example.", "[2606:4700:4700::1111]"]).unwrap();
        assert!(hosts.allows("xn--bcher-kva.example"));
        assert!(hosts.allows("b\u{dc}cher.example."));
        assert!(hosts.allows("[2606:4700:4700::1111]"));
        assert!(hosts.allows("2606:4700:4700::1111"));
        assert!(!hosts.allows("api.xn--bcher-kva.example"));
        assert!(!hosts.allows("xn--bcher-kva.example.attacker.test"));
    }

    #[test]
    fn allowlist_refuses_local_and_private_literals() {
        assert!(ExactHostAllowlist::new(["localhost"]).is_err());
        assert!(matches!(
            ExactHostAllowlist::new(["127.0.0.1"]),
            Err(HttpError::AddressDenied(_))
        ));
    }

    #[test]
    fn content_type_rules_are_parameter_insensitive_and_bounded() {
        let policy = ContentTypePolicy::new(
            [
                ContentTypeRule::exact("application/json").unwrap(),
                ContentTypeRule::type_wildcard("image").unwrap(),
                ContentTypeRule::structured_suffix("json").unwrap(),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            policy
                .validate(Some("Application/JSON; charset=utf-8"))
                .unwrap(),
            Some("application/json".to_owned())
        );
        assert!(policy.validate(Some("image/png")).is_ok());
        assert!(policy.validate(Some("application/problem+json")).is_ok());
        assert!(matches!(
            policy.validate(Some("text/html")),
            Err(HttpError::ContentTypeDenied(_))
        ));
        assert_eq!(policy.validate(None), Err(HttpError::ContentTypeMissing));
    }

    #[test]
    fn limits_reject_unbounded_or_zero_configuration() {
        let limits = HttpLimits {
            max_response_bytes: HARD_MAX_RESPONSE_BYTES + 1,
            ..HttpLimits::default()
        };
        let policy = HttpPolicy::new(
            ExactHostAllowlist::new(["example.com"]).unwrap(),
            ContentTypePolicy::json(),
        );
        assert!(policy.with_limits(limits).is_err());

        let limits = HttpLimits {
            max_concurrent: 0,
            ..HttpLimits::default()
        };
        assert!(
            HttpPolicy::new(
                ExactHostAllowlist::new(["example.com"]).unwrap(),
                ContentTypePolicy::json(),
            )
            .with_limits(limits)
            .is_err()
        );
    }
}
