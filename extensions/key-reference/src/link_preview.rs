//! Bounded website metadata and share-image previews.

use crate::{ReferenceDocumentScope, ReferenceExecutor};
use image::{ImageFormat, ImageReader, codecs::webp::WebPDecoder};
use key_safe_http::{
    CancellationToken, ContentTypePolicy, ContentTypeRule, DocumentCache, DocumentCacheEntry,
    DocumentCacheLimits, ExactHostAllowlist, HttpLimits, HttpPolicy, HttpRequest, RateLimit,
    SafeHttpClient, SchemePolicy,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use url::Url;

const MAX_HTML_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(4);
const USER_AGENT_VALUE: &str = "GPUI-PDF-Reader/0.1 link-preview";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebsitePreview {
    pub title: Option<String>,
    pub site_name: Option<String>,
    pub resolved_url: String,
    pub image_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebsitePreviewState {
    Loading,
    Ready(WebsitePreview),
    Failed(String),
}

#[derive(Debug)]
pub enum LinkPreviewEvent {
    WebsiteFetched {
        generation: u64,
        url: String,
        result: Result<WebsitePreview, String>,
    },
}

/// Injectable website-preview boundary used by the asynchronous orchestrator.
///
/// Implementations must cooperate with cancellation and place any persistent
/// preview assets in the supplied document-owned cache.
pub trait WebsitePreviewProvider: Send + Sync {
    fn fetch(
        &self,
        url: &str,
        cache: &DocumentCache,
        cancellation: &CancellationToken,
    ) -> Result<WebsitePreview, String>;
}

#[derive(Clone, Copy, Debug, Default)]
struct NetworkWebsitePreviewProvider;

impl WebsitePreviewProvider for NetworkWebsitePreviewProvider {
    fn fetch(
        &self,
        url: &str,
        cache: &DocumentCache,
        cancellation: &CancellationToken,
    ) -> Result<WebsitePreview, String> {
        fetch_website_preview(url, cache, cancellation)
    }
}

impl LinkPreviewEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::WebsiteFetched { generation, .. } => *generation,
        }
    }
}

pub struct LinkPreviewFetcher {
    events: mpsc::Sender<LinkPreviewEvent>,
    scope: ReferenceDocumentScope,
    provider: Arc<dyn WebsitePreviewProvider>,
}

impl LinkPreviewFetcher {
    pub fn new() -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        Self::with_executor(ReferenceExecutor::global())
    }

    /// Uses a host-owned process service while creating an independent
    /// document scope for this compatibility adapter.
    pub fn with_executor(executor: ReferenceExecutor) -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        Self::with_provider_and_scope(
            Arc::new(NetworkWebsitePreviewProvider),
            executor.document_scope(),
        )
    }

    /// Uses a scope shared with other document services such as scholarly
    /// metadata, giving the host one cancellation lifetime per open PDF.
    pub fn with_scope(scope: ReferenceDocumentScope) -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        Self::with_provider_and_scope(Arc::new(NetworkWebsitePreviewProvider), scope)
    }

    /// Creates an orchestrator around a deterministic or host-supplied
    /// provider. This is useful for tests and alternative network adapters.
    pub fn with_provider(
        provider: Arc<dyn WebsitePreviewProvider>,
    ) -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        Self::with_provider_and_scope(provider, ReferenceExecutor::global().document_scope())
    }

    pub fn with_provider_and_scope(
        provider: Arc<dyn WebsitePreviewProvider>,
        scope: ReferenceDocumentScope,
    ) -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        let (events, receiver) = mpsc::channel();
        (
            Self {
                events,
                scope,
                provider,
            },
            receiver,
        )
    }

    pub fn begin_document(&self, generation: u64) {
        self.scope.begin_generation(generation);
    }

    fn fetch(&self, generation: u64, url: String, cache: Arc<DocumentCache>) -> bool {
        if !self.scope.is_current(generation) {
            return false;
        }
        self.scope.register_cache(&cache);
        let events = self.events.clone();
        let provider = Arc::clone(&self.provider);
        self.scope.execute(generation, move |cancellation| {
            let result = provider
                .fetch(&url, &cache, &cancellation)
                .map_err(|error| concise_error(&error));
            if !cancellation.is_cancelled() {
                let _ = events.send(LinkPreviewEvent::WebsiteFetched {
                    generation,
                    url,
                    result,
                });
            }
        })
    }
}

pub struct LinkPreviewSession {
    cache: Arc<DocumentCache>,
    websites: HashMap<String, WebsitePreviewState>,
}

impl LinkPreviewSession {
    pub fn new() -> Result<Self, String> {
        DocumentCache::new(DocumentCacheLimits {
            memory_bytes: 1,
            file_bytes: 32 * 1024 * 1024,
            entry_bytes: MAX_IMAGE_BYTES,
            entries: 32,
        })
        .map(|cache| Self {
            cache: Arc::new(cache),
            websites: HashMap::new(),
        })
        .map_err(|error| format!("Could not create the link preview cache: {error}"))
    }

    pub fn website(&self, url: &str) -> Option<&WebsitePreviewState> {
        self.websites.get(url)
    }

    pub fn request_website(
        &mut self,
        fetcher: &LinkPreviewFetcher,
        generation: u64,
        url: &str,
    ) -> bool {
        if self.websites.contains_key(url) {
            return false;
        }
        if !fetcher.fetch(generation, url.to_owned(), Arc::clone(&self.cache)) {
            return false;
        }
        self.websites
            .insert(url.to_owned(), WebsitePreviewState::Loading);
        true
    }

    pub fn apply(&mut self, event: LinkPreviewEvent) -> Option<u64> {
        match event {
            LinkPreviewEvent::WebsiteFetched {
                generation,
                url,
                result,
            } => {
                let state = match result {
                    Ok(preview) => WebsitePreviewState::Ready(preview),
                    Err(error) => WebsitePreviewState::Failed(error),
                };
                self.websites.insert(url, state);
                Some(generation)
            }
        }
    }

    /// Clears in-memory states and removes every ephemeral preview file now.
    pub fn purge(&mut self) {
        self.websites.clear();
        self.cache.purge();
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        self.cache.directory()
    }
}

#[derive(Default)]
struct WebsiteMetadata {
    title: Option<String>,
    site_name: Option<String>,
    image_url: Option<Url>,
}

fn fetch_website_preview(
    url: &str,
    cache: &DocumentCache,
    cancellation: &CancellationToken,
) -> Result<WebsitePreview, String> {
    let response = safe_get(
        url,
        "text/html,application/xhtml+xml;q=0.9",
        MAX_HTML_BYTES,
        ContentTypePolicy::new(
            [
                ContentTypeRule::exact("text/html").map_err(|error| error.to_string())?,
                ContentTypeRule::exact("application/xhtml+xml")
                    .map_err(|error| error.to_string())?,
            ],
            true,
        )
        .map_err(|error| error.to_string())?,
        cancellation,
    )?;
    let resolved = response.final_url().clone();
    let html = String::from_utf8_lossy(response.body());
    let metadata = parse_website_metadata(&html, &resolved);
    let image_path = metadata
        .image_url
        .and_then(|image_url| fetch_and_cache_image(image_url, cache, cancellation).ok());
    Ok(WebsitePreview {
        title: metadata.title,
        site_name: metadata.site_name,
        resolved_url: resolved.to_string(),
        image_path,
    })
}

fn fetch_and_cache_image(
    image_url: Url,
    cache: &DocumentCache,
    cancellation: &CancellationToken,
) -> Result<PathBuf, String> {
    let response = safe_get(
        image_url.as_str(),
        "image/avif,image/webp,image/png,image/jpeg;q=0.9",
        MAX_IMAGE_BYTES,
        ContentTypePolicy::new(
            [ContentTypeRule::type_wildcard("image").map_err(|error| error.to_string())?],
            true,
        )
        .map_err(|error| error.to_string())?,
        cancellation,
    )?;
    validate_and_cache_image(response.final_url(), response.body(), cache, cancellation)
}

fn validate_and_cache_image(
    resolved: &Url,
    body: &[u8],
    cache: &DocumentCache,
    cancellation: &CancellationToken,
) -> Result<PathBuf, String> {
    let reader = ImageReader::new(Cursor::new(body))
        .with_guessed_format()
        .map_err(|error| format!("Could not inspect the preview image: {error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "The preview image format is unknown".to_owned())?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Avif
    ) {
        return Err("The preview image format is unsupported".to_owned());
    }
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("Could not read the preview image dimensions: {error}"))?;
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err("The preview image dimensions are unsafe".to_owned());
    }
    if format == ImageFormat::WebP
        && WebPDecoder::new(Cursor::new(body))
            .map_err(|error| format!("Could not inspect the WebP preview: {error}"))?
            .has_animation()
    {
        return Err("Animated website preview images are unsupported".to_owned());
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    resolved.as_str().hash(&mut hasher);
    let key = format!("preview-image-{:016x}", hasher.finish());
    cache
        .insert_file(&key, Cursor::new(body), cancellation)
        .map_err(|error| format!("Could not cache the preview image: {error}"))?;
    match cache.get(&key) {
        Some(DocumentCacheEntry::File { path, .. }) => Ok(path),
        Some(DocumentCacheEntry::Memory(_)) => {
            Err("The preview image cache used the wrong storage tier".to_owned())
        }
        None => Err("The preview image cache discarded the new entry".to_owned()),
    }
}

fn safe_get(
    url: &str,
    accept: &str,
    maximum_bytes: usize,
    content_types: ContentTypePolicy,
    cancellation: &CancellationToken,
) -> Result<key_safe_http::HttpResponse, String> {
    let url = validated_http_url(url)?;
    let hosts = redirect_hosts(&url)?;
    let limits = HttpLimits {
        max_response_bytes: maximum_bytes,
        max_redirects: 5,
        max_concurrent: 1,
        resolve_timeout: RESOLVE_TIMEOUT,
        connect_timeout: CONNECT_TIMEOUT,
        request_timeout: REQUEST_TIMEOUT,
        rate: RateLimit::new(8, Duration::from_secs(60)).map_err(|error| error.to_string())?,
        ..HttpLimits::default()
    };
    let policy = HttpPolicy::new(
        ExactHostAllowlist::new(hosts).map_err(|error| error.to_string())?,
        content_types,
    )
    .with_schemes(SchemePolicy::HttpAndHttps)
    .with_limits(limits)
    .map_err(|error| error.to_string())?;
    let request = HttpRequest::get(url.as_str())
        .and_then(|request| request.with_header("accept", accept))
        .and_then(|request| request.with_header("user-agent", USER_AGENT_VALUE))
        .map_err(|error| error.to_string())?;
    let response = SafeHttpClient::new(policy)
        .execute(request, cancellation)
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&response.status()) {
        return Err(format!(
            "The remote server returned HTTP {}",
            response.status()
        ));
    }
    Ok(response)
}

pub(crate) fn fetch_public_json(
    url: &str,
    maximum_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let response = safe_get(
        url,
        "application/json",
        maximum_bytes,
        ContentTypePolicy::json(),
        cancellation,
    )
    .map_err(|error| error.replace("link preview", "metadata request"))?;
    Ok(response.body().to_vec())
}

fn validated_http_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("The link URL is invalid: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS link previews are supported".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Authenticated link preview URLs are not supported".to_owned());
    }
    Ok(url)
}

fn redirect_hosts(url: &Url) -> Result<Vec<String>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "The link preview URL has no host".to_owned())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let mut hosts = vec![host.clone()];
    if host.parse::<std::net::IpAddr>().is_err() {
        if let Some(base) = host.strip_prefix("www.") {
            hosts.push(base.to_owned());
        } else {
            hosts.push(format!("www.{host}"));
        }
    }
    Ok(hosts)
}

fn parse_website_metadata(html: &str, base_url: &Url) -> WebsiteMetadata {
    let lower = html.to_ascii_lowercase();
    let mut metadata = WebsiteMetadata::default();
    let mut twitter_title = None;
    let mut twitter_image = None;
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("<meta") {
        let start = cursor + relative;
        if lower
            .as_bytes()
            .get(start + 5)
            .is_some_and(|value| !value.is_ascii_whitespace() && !matches!(value, b'/' | b'>'))
        {
            cursor = start + 5;
            continue;
        }
        let Some(end) = html_tag_end(html, start) else {
            break;
        };
        if end - start <= 8_192 {
            let attributes = parse_html_attributes(&html[start + 5..end - 1]);
            let key = attributes
                .get("property")
                .or_else(|| attributes.get("name"))
                .map(|value| value.to_ascii_lowercase());
            let content = attributes
                .get("content")
                .map(|value| normalized_metadata_text(value, 512));
            if let (Some(key), Some(content)) = (key, content)
                && !content.is_empty()
            {
                match key.as_str() {
                    "og:title" => metadata.title = Some(content),
                    "og:site_name" => metadata.site_name = Some(content),
                    "og:image" | "og:image:secure_url" if metadata.image_url.is_none() => {
                        metadata.image_url = resolve_metadata_url(base_url, &content);
                    }
                    "twitter:title" if twitter_title.is_none() => twitter_title = Some(content),
                    "twitter:image" | "twitter:image:src" if twitter_image.is_none() => {
                        twitter_image = resolve_metadata_url(base_url, &content);
                    }
                    _ => {}
                }
            }
        }
        cursor = end;
    }

    if metadata.title.is_none() {
        metadata.title = twitter_title.or_else(|| html_title(html, &lower));
    }
    if metadata.image_url.is_none() {
        metadata.image_url = twitter_image;
    }
    metadata
}

fn html_tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, value) in html.as_bytes().get(start..)?.iter().copied().enumerate() {
        if let Some(active) = quote {
            if value == active {
                quote = None;
            }
        } else if matches!(value, b'"' | b'\'') {
            quote = Some(value);
        } else if value == b'>' {
            return Some(start + offset + 1);
        }
    }
    None
}

fn parse_html_attributes(input: &str) -> HashMap<String, String> {
    let bytes = input.as_bytes();
    let mut attributes = HashMap::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        let key_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric()
                || matches!(bytes[cursor], b'-' | b'_' | b':'))
        {
            cursor += 1;
        }
        if key_start == cursor {
            cursor += 1;
            continue;
        }
        let key = input[key_start..cursor].to_ascii_lowercase();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            attributes.entry(key).or_default();
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let (value_start, value_end) =
            if cursor < bytes.len() && matches!(bytes[cursor], b'"' | b'\'') {
                let quote = bytes[cursor];
                cursor += 1;
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor] != quote {
                    cursor += 1;
                }
                let end = cursor;
                cursor = (cursor + 1).min(bytes.len());
                (start, end)
            } else {
                let start = cursor;
                while cursor < bytes.len()
                    && !bytes[cursor].is_ascii_whitespace()
                    && bytes[cursor] != b'>'
                {
                    cursor += 1;
                }
                (start, cursor)
            };
        attributes.insert(key, decode_html_entities(&input[value_start..value_end]));
    }
    attributes
}

fn html_title(html: &str, lower: &str) -> Option<String> {
    let start = lower.find("<title")?;
    let content_start = start + lower[start..].find('>')? + 1;
    let content_end = content_start + lower[content_start..].find("</title>")?;
    let title = normalized_metadata_text(&html[content_start..content_end], 512);
    (!title.is_empty()).then_some(title)
}

fn resolve_metadata_url(base_url: &Url, value: &str) -> Option<Url> {
    let url = base_url.join(value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

fn normalized_metadata_text(value: &str, maximum_chars: usize) -> String {
    decode_html_entities(value)
        .split_whitespace()
        .flat_map(|word| [word, " "])
        .flat_map(str::chars)
        .take(maximum_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn decode_html_entities(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(index) = remaining.find('&') {
        result.push_str(&remaining[..index]);
        remaining = &remaining[index..];
        let Some(end) = remaining.find(';').filter(|end| *end <= 12) else {
            result.push('&');
            remaining = &remaining[1..];
            continue;
        };
        let entity = &remaining[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "apos" | "#39" => Some('\''),
            "gt" => Some('>'),
            "lt" => Some('<'),
            "nbsp" => Some(' '),
            "quot" => Some('"'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            result.push(decoded);
        } else {
            result.push_str(&remaining[..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    result.push_str(remaining);
    result
}

fn concise_error(error: &str) -> String {
    let normalized = error.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    struct FakeWebsiteProvider {
        calls: Arc<AtomicUsize>,
    }

    impl WebsitePreviewProvider for FakeWebsiteProvider {
        fn fetch(
            &self,
            url: &str,
            _cache: &DocumentCache,
            cancellation: &CancellationToken,
        ) -> Result<WebsitePreview, String> {
            assert!(!cancellation.is_cancelled());
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(WebsitePreview {
                title: Some("Injected preview".to_owned()),
                site_name: None,
                resolved_url: url.to_owned(),
                image_path: None,
            })
        }
    }

    #[test]
    fn injected_provider_is_generation_scoped_and_updates_the_session() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (fetcher, events) = LinkPreviewFetcher::with_provider(Arc::new(FakeWebsiteProvider {
            calls: Arc::clone(&calls),
        }));
        fetcher.begin_document(7);
        let mut session = LinkPreviewSession::new().unwrap();
        assert!(!session.request_website(&fetcher, 6, "https://stale.example"));
        assert!(session.request_website(&fetcher, 7, "https://current.example"));

        let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.generation(), 7);
        assert_eq!(session.apply(event), Some(7));
        assert!(matches!(
            session.website("https://current.example"),
            Some(WebsitePreviewState::Ready(WebsitePreview { title, .. }))
                if title.as_deref() == Some("Injected preview")
        ));
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn metadata_parser_handles_attribute_order_entities_and_relative_images() {
        let base = Url::parse("https://example.com/papers/index.html").unwrap();
        let metadata = parse_website_metadata(
            r#"
                <html><head>
                <meta content="/share.png" property="og:image">
                <meta content="Research &amp; Results > Methods" property="og:title">
                <metadata content="must be ignored">
                <meta name='og:site_name' content='Example Journal'>
                <title>Fallback title</title>
                </head></html>
            "#,
            &base,
        );
        assert_eq!(
            metadata.title.as_deref(),
            Some("Research & Results > Methods")
        );
        assert_eq!(metadata.site_name.as_deref(), Some("Example Journal"));
        assert_eq!(
            metadata.image_url.as_ref().map(Url::as_str),
            Some("https://example.com/share.png")
        );
    }

    #[test]
    fn preview_redirect_scope_is_exact_except_for_the_conventional_www_alias() {
        let hosts =
            redirect_hosts(&Url::parse("https://papers.example.org/article").unwrap()).unwrap();
        assert_eq!(hosts, vec!["papers.example.org", "www.papers.example.org"]);
        assert!(!hosts.iter().any(|host| host == "cdn.example.org"));
    }

    #[test]
    fn session_temp_cache_is_removed_on_drop() {
        let path = {
            let session = LinkPreviewSession::new().unwrap();
            let path = session.path().to_path_buf();
            fs::write(path.join("preview.bin"), b"cached").unwrap();
            assert!(path.exists());
            path
        };
        assert!(!path.exists());
    }

    #[test]
    fn validated_share_image_is_cached_in_the_document_owned_file_tier() {
        let session = LinkPreviewSession::new().unwrap();
        let path = validate_and_cache_image(
            &Url::parse("https://example.org/share.png").unwrap(),
            PNG_1X1,
            &session.cache,
            &CancellationToken::active(),
        )
        .unwrap();
        assert!(path.starts_with(session.path()));
        assert_eq!(fs::read(path).unwrap(), PNG_1X1);
    }
}
