use image::{ImageFormat, ImageReader, codecs::webp::WebPDecoder};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use url::Url;

const MAX_HTML_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_REDIRECTS: usize = 5;
const MAX_CONCURRENT_FETCHES: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
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

impl LinkPreviewEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::WebsiteFetched { generation, .. } => *generation,
        }
    }
}

pub struct LinkPreviewFetcher {
    events: mpsc::Sender<LinkPreviewEvent>,
    generation: Arc<AtomicU64>,
    active_fetches: Arc<AtomicUsize>,
}

impl LinkPreviewFetcher {
    pub fn new() -> (Self, mpsc::Receiver<LinkPreviewEvent>) {
        let (events, receiver) = mpsc::channel();
        (
            Self {
                events,
                generation: Arc::new(AtomicU64::new(0)),
                active_fetches: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    pub fn begin_document(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }

    fn fetch(&self, generation: u64, url: String, cache_dir: PathBuf) -> bool {
        if self.generation.load(Ordering::Acquire) != generation {
            return false;
        }
        if self
            .active_fetches
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_FETCHES).then_some(active + 1)
            })
            .is_err()
        {
            return false;
        }

        let events = self.events.clone();
        let active_generation = self.generation.clone();
        let active_fetches = self.active_fetches.clone();
        let spawned = thread::Builder::new()
            .name("link-preview-fetch".to_owned())
            .spawn(move || {
                let _guard = ActiveFetchGuard(active_fetches);
                if active_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let result = fetch_website_preview(&url, &cache_dir, NetworkPolicy::PublicOnly)
                    .map_err(|error| concise_error(&error));
                if active_generation.load(Ordering::Acquire) == generation {
                    let _ = events.send(LinkPreviewEvent::WebsiteFetched {
                        generation,
                        url,
                        result,
                    });
                }
            });
        if spawned.is_err() {
            self.active_fetches.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }
}

struct ActiveFetchGuard(Arc<AtomicUsize>);

impl Drop for ActiveFetchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct LinkPreviewSession {
    cache_dir: TempDir,
    websites: HashMap<String, WebsitePreviewState>,
}

impl LinkPreviewSession {
    pub fn new() -> Result<Self, String> {
        tempfile::Builder::new()
            .prefix("gpui-pdf-link-preview-")
            .tempdir()
            .map(|cache_dir| Self {
                cache_dir,
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
        if !fetcher.fetch(
            generation,
            url.to_owned(),
            self.cache_dir.path().to_path_buf(),
        ) {
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

    #[cfg(test)]
    fn path(&self) -> &Path {
        self.cache_dir.path()
    }
}

#[derive(Clone, Copy)]
enum NetworkPolicy {
    PublicOnly,
    #[cfg(test)]
    AllowPrivate,
}

#[derive(Default)]
struct WebsiteMetadata {
    title: Option<String>,
    site_name: Option<String>,
    image_url: Option<Url>,
}

fn fetch_website_preview(
    url: &str,
    cache_dir: &Path,
    network_policy: NetworkPolicy,
) -> Result<WebsitePreview, String> {
    let initial = validated_http_url(url)?;
    let (resolved, response) = bounded_get(
        initial,
        "text/html,application/xhtml+xml;q=0.9",
        MAX_HTML_BYTES,
        network_policy,
    )?;
    validate_html_response(&response)?;
    let html = String::from_utf8_lossy(&response.body);
    let metadata = parse_website_metadata(&html, &resolved);
    let image_path = metadata
        .image_url
        .and_then(|image_url| fetch_and_cache_image(image_url, cache_dir, network_policy).ok());
    Ok(WebsitePreview {
        title: metadata.title,
        site_name: metadata.site_name,
        resolved_url: resolved.to_string(),
        image_path,
    })
}

fn fetch_and_cache_image(
    image_url: Url,
    cache_dir: &Path,
    network_policy: NetworkPolicy,
) -> Result<PathBuf, String> {
    let (resolved, response) = bounded_get(
        image_url,
        "image/avif,image/webp,image/png,image/jpeg;q=0.9",
        MAX_IMAGE_BYTES,
        network_policy,
    )?;
    validate_image_response(&response)?;
    let reader = ImageReader::new(Cursor::new(&response.body))
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
        && WebPDecoder::new(Cursor::new(&response.body))
            .map_err(|error| format!("Could not inspect the WebP preview: {error}"))?
            .has_animation()
    {
        return Err("Animated website preview images are unsupported".to_owned());
    }

    let extension = match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::WebP => "webp",
        ImageFormat::Avif => "avif",
        _ => unreachable!(),
    };
    write_cached_image(cache_dir, resolved.as_str(), extension, &response.body)
}

struct BoundedResponse {
    content_type: Option<String>,
    body: Vec<u8>,
}

fn bounded_get(
    mut url: Url,
    accept: &str,
    maximum_bytes: usize,
    network_policy: NetworkPolicy,
) -> Result<(Url, BoundedResponse), String> {
    for redirect_count in 0..=MAX_REDIRECTS {
        let (host, addresses) = resolve_url(&url, network_policy)?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .resolve_to_addrs(&host, &addresses)
            .build()
            .map_err(|error| format!("Could not configure the preview request: {error}"))?;
        let mut response = client
            .get(url.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, accept)
            .send()
            .map_err(|error| format!("Could not fetch the link preview: {error}"))?;

        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err("The link preview redirected too many times".to_owned());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "The link preview returned an invalid redirect".to_owned())?;
            url = validated_http_url(
                url.join(location)
                    .map_err(|error| format!("The link preview redirect is invalid: {error}"))?
                    .as_str(),
            )?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "The link preview returned HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > maximum_bytes as u64)
        {
            return Err("The link preview response is too large".to_owned());
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = read_bounded(&mut response, maximum_bytes)?;
        return Ok((url, BoundedResponse { content_type, body }));
    }
    Err("The link preview redirected too many times".to_owned())
}

pub(crate) fn fetch_public_json(url: &str, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    let url = validated_http_url(url)?;
    let (_, response) = bounded_get(
        url,
        "application/json",
        maximum_bytes,
        NetworkPolicy::PublicOnly,
    )
    .map_err(|error| error.replace("link preview", "metadata request"))?;
    if response
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            let content_type = content_type
                .split(';')
                .next()
                .unwrap_or(content_type)
                .trim()
                .to_ascii_lowercase();
            !content_type.starts_with("application/json")
                && !content_type.ends_with("+json")
                && !content_type.starts_with("text/json")
        })
    {
        return Err("The metadata service did not return JSON".to_owned());
    }
    Ok(response.body)
}

fn resolve_url(
    url: &Url,
    network_policy: NetworkPolicy,
) -> Result<(String, Vec<SocketAddr>), String> {
    let host = url
        .host_str()
        .ok_or_else(|| "The link preview URL has no host".to_owned())?
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "The link preview URL has no usable port".to_owned())?;
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("Could not resolve the link preview host: {error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("The link preview host resolved to no addresses".to_owned());
    }
    if matches!(network_policy, NetworkPolicy::PublicOnly)
        && addresses.iter().any(|address| !is_public_ip(address.ip()))
    {
        return Err("The link preview host resolves to a private address".to_owned());
    }
    Ok((host, addresses))
}

fn read_bounded(response: &mut Response, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    let limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut body = Vec::with_capacity(maximum_bytes.min(64 * 1024));
    response
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|error| format!("Could not read the link preview response: {error}"))?;
    if body.len() > maximum_bytes {
        return Err("The link preview response is too large".to_owned());
    }
    Ok(body)
}

fn validate_html_response(response: &BoundedResponse) -> Result<(), String> {
    if response
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            let content_type = content_type.to_ascii_lowercase();
            !content_type.starts_with("text/html")
                && !content_type.starts_with("application/xhtml+xml")
        })
    {
        return Err("The link target is not an HTML page".to_owned());
    }
    Ok(())
}

fn validate_image_response(response: &BoundedResponse) -> Result<(), String> {
    if response
        .content_type
        .as_deref()
        .is_some_and(|content_type| !content_type.to_ascii_lowercase().starts_with("image/"))
    {
        return Err("The website share image is not an image".to_owned());
    }
    Ok(())
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

fn write_cached_image(
    cache_dir: &Path,
    image_url: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    if !cache_dir.is_dir() {
        return Err("The link preview cache is no longer available".to_owned());
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image_url.hash(&mut hasher);
    let hash = hasher.finish();
    for attempt in 0..16_u8 {
        let path = cache_dir.join(format!("preview-{hash:016x}-{attempt}.{extension}"));
        let mut file = match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create the preview image cache: {error}")),
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(format!("Could not cache the preview image: {error}"));
        }
        return Ok(path);
    }
    Err("Could not allocate a unique preview image cache path".to_owned())
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

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (a == 100 && (64..=127).contains(&b))
        || a >= 224)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

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
    fn private_and_reserved_networks_are_rejected() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.0.1",
            "100.64.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
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
    fn bounded_fetch_follows_relative_redirect_and_caches_share_image() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let mut stream = stream;
                match path {
                    "/" => stream
                        .write_all(b"HTTP/1.1 302 Found\r\nLocation: /paper\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .unwrap(),
                    "/paper" => {
                        let body = format!(
                            "<meta property=\"og:title\" content=\"Fetched paper\"><meta property=\"og:image\" content=\"http://{address}/share.png\">"
                        );
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .unwrap();
                    }
                    "/share.png" => {
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            PNG_1X1.len()
                        )
                        .unwrap();
                        stream.write_all(PNG_1X1).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
        });

        let cache = tempfile::tempdir().unwrap();
        let preview = fetch_website_preview(
            &format!("http://{address}/"),
            cache.path(),
            NetworkPolicy::AllowPrivate,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(preview.title.as_deref(), Some("Fetched paper"));
        assert_eq!(preview.resolved_url, format!("http://{address}/paper"));
        let image_path = preview.image_path.unwrap();
        assert!(image_path.starts_with(cache.path()));
        assert_eq!(fs::read(image_path).unwrap(), PNG_1X1);
    }
}
