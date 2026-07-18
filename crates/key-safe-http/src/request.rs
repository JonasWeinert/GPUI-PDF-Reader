use crate::HttpError;
use url::Url;

/// HTTP methods supported by the bounded capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    /// Retrieve a representation.
    Get,
    /// Retrieve response metadata without a body.
    Head,
    /// Submit a bounded body.
    Post,
}

/// A validated request header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHeader {
    name: String,
    value: String,
}

impl RequestHeader {
    /// Creates a request header, rejecting framing and proxy controls reserved
    /// to the trusted transport.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Result<Self, HttpError> {
        let name = name.into().trim().to_ascii_lowercase();
        let value = value.into();
        if !is_header_name(&name) {
            return Err(HttpError::InvalidRequest(format!(
                "invalid header name `{name}`"
            )));
        }
        if value
            .bytes()
            .any(|byte| (!byte.is_ascii() || byte < b' ') && byte != b'\t' || byte == 0x7f)
        {
            return Err(HttpError::InvalidRequest(format!(
                "header `{name}` contains a forbidden control character"
            )));
        }
        if is_transport_header(&name) {
            return Err(HttpError::InvalidRequest(format!(
                "header `{name}` is controlled by the HTTP host"
            )));
        }
        Ok(Self { name, value })
    }

    /// Lowercase header name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Header value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn is_sensitive(&self) -> bool {
        matches!(self.name.as_str(), "authorization" | "cookie")
    }
}

/// A bounded HTTP request. Final byte limits are applied by the client policy.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    method: HttpMethod,
    url: Url,
    headers: Vec<RequestHeader>,
    body: Vec<u8>,
}

impl HttpRequest {
    /// Creates an absolute request from a URL string.
    pub fn new(method: HttpMethod, url: &str) -> Result<Self, HttpError> {
        let url = Url::parse(url).map_err(|error| HttpError::InvalidUrl(error.to_string()))?;
        Ok(Self {
            method,
            url,
            headers: Vec::new(),
            body: Vec::new(),
        })
    }

    /// Convenience constructor for a GET request.
    pub fn get(url: &str) -> Result<Self, HttpError> {
        Self::new(HttpMethod::Get, url)
    }

    /// Adds one validated header. Repeated headers remain repeated.
    pub fn with_header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, HttpError> {
        self.headers.push(RequestHeader::new(name, value)?);
        Ok(self)
    }

    /// Replaces the request body.
    ///
    /// GET and HEAD requests intentionally cannot carry a body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Result<Self, HttpError> {
        if self.method != HttpMethod::Post {
            return Err(HttpError::InvalidRequest(
                "only POST requests may include a body".to_owned(),
            ));
        }
        self.body = body.into();
        Ok(self)
    }

    /// Request method.
    #[must_use]
    pub fn method(&self) -> HttpMethod {
        self.method
    }

    /// Initial URL before validated redirects.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Validated request headers.
    #[must_use]
    pub fn headers(&self) -> &[RequestHeader] {
        &self.headers
    }

    /// Request body bytes.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub(crate) fn into_parts(self) -> (HttpMethod, Url, Vec<RequestHeader>, Vec<u8>) {
        (self.method, self.url, self.headers, self.body)
    }
}

/// A bounded response header. Non-UTF-8 values remain available as bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseHeader {
    name: String,
    value: Vec<u8>,
}

impl ResponseHeader {
    pub(crate) fn new(name: String, value: Vec<u8>) -> Self {
        Self { name, value }
    }

    /// Lowercase header name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raw header bytes.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// UTF-8 header value when representable.
    #[must_use]
    pub fn value_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }
}

/// A fully buffered response that has passed policy and size validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    status: u16,
    final_url: Url,
    content_type: Option<String>,
    headers: Vec<ResponseHeader>,
    body: Vec<u8>,
}

impl HttpResponse {
    pub(crate) fn new(
        status: u16,
        final_url: Url,
        content_type: Option<String>,
        headers: Vec<ResponseHeader>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            final_url,
            content_type,
            headers,
            body,
        }
    }

    /// Numeric HTTP status.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Final URL after validated redirects.
    #[must_use]
    pub fn final_url(&self) -> &Url {
        &self.final_url
    }

    /// Canonical media type without parameters.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Response headers within the configured aggregate byte limit.
    #[must_use]
    pub fn headers(&self) -> &[ResponseHeader] {
        &self.headers
    }

    /// Fully buffered response body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Finds the first response header with this case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&ResponseHeader> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
    }
}

fn is_header_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_transport_header(name: &str) -> bool {
    matches!(
        name,
        "accept-encoding"
            | "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    ) || name.starts_with("proxy-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_headers_reject_framing_and_injection() {
        assert!(RequestHeader::new("Host", "attacker.test").is_err());
        assert!(RequestHeader::new("X-Test", "ok\r\nInjected: yes").is_err());
        assert!(RequestHeader::new("X-Test", "non-ascii: ☃").is_err());
        assert!(RequestHeader::new("x-safe", "yes").is_ok());
    }

    #[test]
    fn bodies_are_limited_to_post() {
        assert!(
            HttpRequest::get("https://example.com")
                .unwrap()
                .with_body(b"unexpected".to_vec())
                .is_err()
        );
        assert!(
            HttpRequest::new(HttpMethod::Post, "https://example.com")
                .unwrap()
                .with_body(b"accepted".to_vec())
                .is_ok()
        );
    }
}
