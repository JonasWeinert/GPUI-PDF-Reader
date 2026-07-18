//! OpenAlex and Semantic Scholar metadata lookup and enrichment.

use crate::detect_doi;
use crate::link_preview::fetch_public_json;
use key_safe_http::{CancellationSource, CancellationToken};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use url::Url;

const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONCURRENT_FETCHES: usize = 3;
const MAX_REFERENCE_CHARS: usize = 2_000;
const MAX_ABSTRACT_CHARS: usize = 8_000;
const MAX_AUTHORS: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScholarlySource {
    OpenAlex,
    SemanticScholar,
}

impl ScholarlySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAlex => "OpenAlex",
            Self::SemanticScholar => "Semantic Scholar",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchCertainty {
    High,
    Medium,
    Low,
}

impl MatchCertainty {
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "High-confidence match",
            Self::Medium => "Likely match",
            Self::Low => "Possible match",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScholarlyMetadata {
    pub source: ScholarlySource,
    pub title: String,
    pub abstract_text: Option<String>,
    pub tldr_text: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<u32>,
    pub journal: Option<String>,
    pub journal_short: Option<String>,
    pub journal_url: Option<String>,
    pub doi: Option<String>,
    pub open_access: Option<bool>,
    pub full_text_url: Option<String>,
    pub landing_url: Option<String>,
    pub certainty: Option<MatchCertainty>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScholarlyMetadataState {
    Loading,
    Ready(Box<ScholarlyMetadata>),
    Failed(String),
}

#[derive(Debug)]
pub enum ScholarlyEvent {
    Fetched {
        generation: u64,
        key: String,
        result: Result<ScholarlyMetadata, String>,
    },
}

/// Injectable metadata-provider boundary used by the asynchronous fetcher.
pub trait ScholarlyMetadataProvider: Send + Sync {
    fn fetch(
        &self,
        reference: &str,
        cancellation: &CancellationToken,
    ) -> Result<ScholarlyMetadata, String>;
}

#[derive(Clone, Copy, Debug, Default)]
struct NetworkScholarlyMetadataProvider;

impl ScholarlyMetadataProvider for NetworkScholarlyMetadataProvider {
    fn fetch(
        &self,
        reference: &str,
        cancellation: &CancellationToken,
    ) -> Result<ScholarlyMetadata, String> {
        fetch_scholarly_metadata(reference, cancellation)
    }
}

impl ScholarlyEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Fetched { generation, .. } => *generation,
        }
    }
}

pub struct ScholarlyFetcher {
    events: mpsc::Sender<ScholarlyEvent>,
    generation: Arc<AtomicU64>,
    active_fetches: Arc<AtomicUsize>,
    cancellation: Arc<Mutex<CancellationSource>>,
    provider: Arc<dyn ScholarlyMetadataProvider>,
}

impl ScholarlyFetcher {
    pub fn new() -> (Self, mpsc::Receiver<ScholarlyEvent>) {
        Self::with_provider(Arc::new(NetworkScholarlyMetadataProvider))
    }

    /// Creates a fetcher around a deterministic or host-supplied provider.
    pub fn with_provider(
        provider: Arc<dyn ScholarlyMetadataProvider>,
    ) -> (Self, mpsc::Receiver<ScholarlyEvent>) {
        let (events, receiver) = mpsc::channel();
        (
            Self {
                events,
                generation: Arc::new(AtomicU64::new(0)),
                active_fetches: Arc::new(AtomicUsize::new(0)),
                cancellation: Arc::new(Mutex::new(CancellationSource::new())),
                provider,
            },
            receiver,
        )
    }

    pub fn begin_document(&self, generation: u64) {
        let mut cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cancellation.cancel();
        *cancellation = CancellationSource::new();
        self.generation.store(generation, Ordering::Release);
    }

    fn fetch(&self, generation: u64, key: String, reference: String) -> bool {
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
        let cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token();
        let provider = Arc::clone(&self.provider);
        let spawned = thread::Builder::new()
            .name("scholarly-metadata-fetch".into())
            .spawn(move || {
                let _guard = ActiveFetchGuard(active_fetches);
                if active_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let result = provider
                    .fetch(&reference, &cancellation)
                    .map_err(|error| concise_error(&error));
                if active_generation.load(Ordering::Acquire) == generation {
                    let _ = events.send(ScholarlyEvent::Fetched {
                        generation,
                        key,
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

#[derive(Default)]
pub struct ScholarlySession {
    entries: HashMap<String, ScholarlyMetadataState>,
}

impl ScholarlySession {
    pub fn state(&self, reference: &str) -> Option<&ScholarlyMetadataState> {
        self.entries.get(&reference_key(reference))
    }

    pub fn request(
        &mut self,
        fetcher: &ScholarlyFetcher,
        generation: u64,
        reference: &str,
    ) -> bool {
        let reference = normalize_reference(reference);
        if reference.is_empty() {
            return false;
        }
        let key = reference_key(&reference);
        if self.entries.contains_key(&key) {
            return false;
        }
        if !fetcher.fetch(generation, key.clone(), reference) {
            self.entries.insert(
                key,
                ScholarlyMetadataState::Failed("lookup unavailable".to_owned()),
            );
            return false;
        }
        self.entries.insert(key, ScholarlyMetadataState::Loading);
        true
    }

    pub fn apply(&mut self, event: ScholarlyEvent) -> Option<u64> {
        match event {
            ScholarlyEvent::Fetched {
                generation,
                key,
                result,
            } => {
                self.entries.insert(
                    key,
                    match result {
                        Ok(metadata) => ScholarlyMetadataState::Ready(Box::new(metadata)),
                        Err(error) => ScholarlyMetadataState::Failed(error),
                    },
                );
                Some(generation)
            }
        }
    }
}

fn fetch_scholarly_metadata(
    reference: &str,
    cancellation: &CancellationToken,
) -> Result<ScholarlyMetadata, String> {
    if let Some(doi) = detect_doi(reference) {
        fetch_openalex(&doi, cancellation)
    } else {
        fetch_semantic_scholar(reference, cancellation)
    }
}

fn fetch_openalex(
    doi: &str,
    cancellation: &CancellationToken,
) -> Result<ScholarlyMetadata, String> {
    let mut url = Url::parse(&format!(
        "https://api.openalex.org/works/https://doi.org/{doi}"
    ))
    .map_err(|error| format!("Could not build the OpenAlex request: {error}"))?;
    url.query_pairs_mut().append_pair(
        "select",
        "id,doi,title,display_name,publication_year,authorships,primary_location,locations,best_oa_location,open_access,abstract_inverted_index",
    );
    let body = fetch_public_json(url.as_str(), MAX_METADATA_BYTES, cancellation)
        .map_err(|error| format!("OpenAlex: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("OpenAlex returned invalid metadata: {error}"))?;
    parse_openalex(&value).ok_or_else(|| "OpenAlex returned no usable work metadata".to_owned())
}

fn fetch_semantic_scholar(
    reference: &str,
    cancellation: &CancellationToken,
) -> Result<ScholarlyMetadata, String> {
    let query = probable_title(reference);
    if query.split_whitespace().count() < 3 {
        return Err("The reference does not contain enough title text to match".to_owned());
    }
    let mut url = Url::parse("https://api.semanticscholar.org/graph/v1/paper/search/match")
        .expect("the Semantic Scholar endpoint is a valid URL");
    url.query_pairs_mut()
        .append_pair("query", &query)
        .append_pair(
            "fields",
            "title,abstract,tldr,authors,year,venue,openAccessPdf,url,externalIds",
        );
    let body = fetch_public_json(url.as_str(), MAX_METADATA_BYTES, cancellation)
        .map_err(|error| format!("Semantic Scholar: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("Semantic Scholar returned invalid metadata: {error}"))?;
    let metadata = parse_semantic_scholar(&value, &query)
        .ok_or_else(|| "Semantic Scholar found no reliable title match".to_owned())?;
    if let Some(doi) = metadata.doi.clone()
        && semantic_metadata_needs_openalex(&metadata)
        && let Ok(openalex) = fetch_openalex(&doi, cancellation)
    {
        return Ok(merge_semantic_with_openalex(metadata, openalex));
    }
    Ok(metadata)
}

fn parse_openalex(value: &Value) -> Option<ScholarlyMetadata> {
    let title = value
        .get("title")
        .or_else(|| value.get("display_name"))
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|title| !title.is_empty())?;
    let authors = value
        .get("authorships")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|authorship| {
            authorship
                .pointer("/author/display_name")
                .and_then(Value::as_str)
                .map(normalize_space)
                .filter(|name| !name.is_empty())
        })
        .take(MAX_AUTHORS)
        .collect();
    let journal = value
        .pointer("/primary_location/source/display_name")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|journal| !journal.is_empty());
    let journal_short = value
        .pointer("/primary_location/source/display_name")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|journal| !journal.is_empty());
    let open_access = value.pointer("/open_access/is_oa").and_then(Value::as_bool);
    let full_text_url = http_string(value.pointer("/best_oa_location/pdf_url")).or_else(|| {
        value
            .get("locations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|location| http_string(location.get("pdf_url")))
    });
    let journal_url = http_string(value.pointer("/primary_location/landing_page_url"));
    let landing_url = http_string(value.get("id"));
    Some(ScholarlyMetadata {
        source: ScholarlySource::OpenAlex,
        title,
        abstract_text: value
            .get("abstract_inverted_index")
            .and_then(reconstruct_abstract),
        tldr_text: None,
        authors,
        year: value
            .get("publication_year")
            .and_then(Value::as_u64)
            .and_then(|year| u32::try_from(year).ok()),
        journal,
        journal_short,
        journal_url,
        doi: value
            .get("doi")
            .and_then(Value::as_str)
            .and_then(detect_doi),
        open_access,
        full_text_url,
        landing_url,
        certainty: None,
    })
}

fn parse_semantic_scholar(value: &Value, query: &str) -> Option<ScholarlyMetadata> {
    let work = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|works| works.first())?;
    let title = work
        .get("title")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|title| !title.is_empty())?;
    let coverage = title_token_coverage(query, &title);
    if coverage < 0.28 {
        return None;
    }
    let certainty = if coverage >= 0.72 {
        MatchCertainty::High
    } else if coverage >= 0.48 {
        MatchCertainty::Medium
    } else {
        MatchCertainty::Low
    };
    let authors = work
        .get("authors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|author| author.get("name").and_then(Value::as_str))
        .map(normalize_space)
        .filter(|name| !name.is_empty())
        .take(MAX_AUTHORS)
        .collect();
    let full_text_url = work
        .pointer("/openAccessPdf/url")
        .and_then(Value::as_str)
        .and_then(valid_http_string);
    let landing_url = work
        .get("url")
        .and_then(Value::as_str)
        .and_then(valid_http_string);
    Some(ScholarlyMetadata {
        source: ScholarlySource::SemanticScholar,
        title,
        abstract_text: work
            .get("abstract")
            .and_then(Value::as_str)
            .map(|text| truncate_text(&normalize_space(text), MAX_ABSTRACT_CHARS))
            .filter(|text| !text.is_empty()),
        tldr_text: work
            .pointer("/tldr/text")
            .and_then(Value::as_str)
            .map(|text| truncate_text(&normalize_space(text), MAX_ABSTRACT_CHARS))
            .filter(|text| !text.is_empty()),
        authors,
        year: work
            .get("year")
            .and_then(Value::as_u64)
            .and_then(|year| u32::try_from(year).ok()),
        journal: work
            .get("venue")
            .and_then(Value::as_str)
            .map(normalize_space)
            .filter(|venue| !venue.is_empty()),
        journal_short: None,
        journal_url: None,
        doi: work
            .pointer("/externalIds/DOI")
            .and_then(Value::as_str)
            .and_then(detect_doi),
        open_access: Some(full_text_url.is_some()),
        full_text_url,
        landing_url,
        certainty: Some(certainty),
    })
}

fn semantic_metadata_needs_openalex(metadata: &ScholarlyMetadata) -> bool {
    metadata.abstract_text.is_none()
        || metadata.authors.is_empty()
        || metadata.year.is_none()
        || metadata.journal.is_none()
        || metadata.journal_short.is_none()
        || metadata.journal_url.is_none()
        || metadata.full_text_url.is_none()
        || metadata.open_access != Some(true)
}

fn merge_semantic_with_openalex(
    mut semantic: ScholarlyMetadata,
    openalex: ScholarlyMetadata,
) -> ScholarlyMetadata {
    if semantic.abstract_text.is_none() {
        semantic.abstract_text = openalex.abstract_text;
    }
    if semantic.authors.is_empty() {
        semantic.authors = openalex.authors;
    }
    semantic.year = semantic.year.or(openalex.year);
    semantic.journal = semantic.journal.or(openalex.journal);
    semantic.journal_short = semantic.journal_short.or(openalex.journal_short);
    semantic.journal_url = semantic.journal_url.or(openalex.journal_url);
    semantic.doi = semantic.doi.or(openalex.doi);
    semantic.full_text_url = semantic.full_text_url.or(openalex.full_text_url);
    if semantic.open_access != Some(true) {
        semantic.open_access = openalex.open_access.or(semantic.open_access);
    }
    semantic.landing_url = semantic.landing_url.or(openalex.landing_url);
    semantic
}

fn reconstruct_abstract(value: &Value) -> Option<String> {
    let words = value.as_object()?;
    let mut ordered = BTreeMap::<usize, &str>::new();
    for (word, positions) in words {
        for position in positions.as_array().into_iter().flatten() {
            if let Some(position) = position
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
            {
                ordered.entry(position).or_insert(word);
            }
        }
    }
    let abstract_text = ordered.into_values().collect::<Vec<_>>().join(" ");
    (!abstract_text.is_empty()).then(|| truncate_text(&abstract_text, MAX_ABSTRACT_CHARS))
}

fn probable_title(reference: &str) -> String {
    let reference = strip_reference_marker(&normalize_reference(reference));
    let without_doi = detect_doi(&reference).map_or(reference.clone(), |doi| {
        reference
            .to_ascii_lowercase()
            .find(&doi)
            .map_or(reference.clone(), |start| reference[..start].to_owned())
    });
    without_doi
        .split(['.', ';'])
        .map(normalize_space)
        .filter(|segment| {
            let words = segment
                .split_whitespace()
                .filter(|word| word.chars().any(char::is_alphabetic))
                .count();
            (3..=30).contains(&words)
                && !segment.to_ascii_lowercase().starts_with("http")
                && !segment.chars().all(|character| {
                    character.is_ascii_digit()
                        || character.is_whitespace()
                        || matches!(character, '(' | ')' | ':' | '-')
                })
        })
        .max_by_key(|segment| {
            let words = segment.split_whitespace().count();
            let lowercase_words = segment
                .split_whitespace()
                .filter(|word| word.chars().next().is_some_and(char::is_lowercase))
                .count();
            words.saturating_mul(2).saturating_add(lowercase_words)
        })
        .unwrap_or_else(|| truncate_text(&without_doi, 300))
}

fn title_token_coverage(query: &str, title: &str) -> f32 {
    let query = title_tokens(query);
    if query.is_empty() {
        return 0.0;
    }
    let title = title_tokens(title);
    query.intersection(&title).count() as f32 / query.len() as f32
}

fn title_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| word.len() >= 3)
        .filter(|word| {
            !matches!(
                word.as_str(),
                "and" | "the" | "for" | "with" | "from" | "that" | "this" | "doi"
            )
        })
        .collect()
}

fn reference_key(reference: &str) -> String {
    detect_doi(reference)
        .map(|doi| format!("doi:{doi}"))
        .unwrap_or_else(|| format!("title:{}", probable_title(reference).to_ascii_lowercase()))
}

fn normalize_reference(value: &str) -> String {
    truncate_text(&normalize_space(value), MAX_REFERENCE_CHARS)
}

fn strip_reference_marker(value: &str) -> String {
    value
        .trim_start()
        .trim_start_matches('[')
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .trim_start_matches([']', '.', ')', ':', ' '])
        .to_owned()
}

fn normalize_space(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_text(value: &str, maximum_chars: usize) -> String {
    value.chars().take(maximum_chars).collect()
}

fn concise_error(error: &str) -> String {
    let normalized = normalize_space(error);
    let mut concise = normalized.chars().take(240).collect::<String>();
    if normalized.chars().count() > 240 {
        concise.push('…');
    }
    concise
}

fn http_string(value: Option<&Value>) -> Option<String> {
    value.as_ref()?.as_str().and_then(valid_http_string)
}

fn valid_http_string(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    struct FakeScholarlyProvider;

    impl ScholarlyMetadataProvider for FakeScholarlyProvider {
        fn fetch(
            &self,
            reference: &str,
            cancellation: &CancellationToken,
        ) -> Result<ScholarlyMetadata, String> {
            assert!(!cancellation.is_cancelled());
            Ok(ScholarlyMetadata {
                source: ScholarlySource::OpenAlex,
                title: reference.to_owned(),
                abstract_text: None,
                tldr_text: None,
                authors: Vec::new(),
                year: None,
                journal: None,
                journal_short: None,
                journal_url: None,
                doi: None,
                open_access: None,
                full_text_url: None,
                landing_url: None,
                certainty: None,
            })
        }
    }

    #[test]
    fn injected_provider_is_generation_scoped_and_updates_the_session() {
        let (fetcher, events) = ScholarlyFetcher::with_provider(Arc::new(FakeScholarlyProvider));
        fetcher.begin_document(4);
        let mut session = ScholarlySession::default();
        assert!(!session.request(&fetcher, 3, "A stale scholarly reference title"));
        assert!(session.request(&fetcher, 4, "A current scholarly reference title"));

        let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.generation(), 4);
        assert_eq!(session.apply(event), Some(4));
        assert!(matches!(
            session.state("A current scholarly reference title"),
            Some(ScholarlyMetadataState::Ready(metadata))
                if metadata.title == "A current scholarly reference title"
        ));
    }

    #[test]
    fn reconstructs_openalex_abstract_and_access_metadata() {
        let value = json!({
            "id": "https://openalex.org/W1",
            "doi": "https://doi.org/10.1000/Example",
            "title": "A useful paper",
            "publication_year": 2024,
            "authorships": [
                {"author": {"display_name": "Ada Author"}},
                {"author": {"display_name": "Ben Writer"}}
            ],
            "primary_location": {
                "landing_page_url": "https://journal.example/article",
                "source": {"display_name": "Good Journal"}
            },
            "best_oa_location": {
                "pdf_url": "https://example.org/paper.pdf",
                "landing_page_url": "https://example.org/paper"
            },
            "open_access": {"is_oa": true},
            "abstract_inverted_index": {
                "second": [1],
                "First": [0],
                "sentence": [2]
            }
        });
        let metadata = parse_openalex(&value).unwrap();
        assert_eq!(metadata.title, "A useful paper");
        assert_eq!(
            metadata.abstract_text.as_deref(),
            Some("First second sentence")
        );
        assert_eq!(metadata.authors, vec!["Ada Author", "Ben Writer"]);
        assert_eq!(metadata.journal_short.as_deref(), Some("Good Journal"));
        assert_eq!(metadata.doi.as_deref(), Some("10.1000/example"));
        assert_eq!(metadata.open_access, Some(true));
        assert_eq!(
            metadata.full_text_url.as_deref(),
            Some("https://example.org/paper.pdf")
        );
        assert_eq!(
            metadata.journal_url.as_deref(),
            Some("https://journal.example/article")
        );
        assert_eq!(
            metadata.landing_url.as_deref(),
            Some("https://openalex.org/W1")
        );
    }

    #[test]
    fn openalex_never_labels_a_landing_page_as_a_pdf() {
        let value = json!({
            "id": "https://openalex.org/W2",
            "title": "Landing pages are not documents",
            "primary_location": {
                "landing_page_url": "https://publisher.example/article",
                "source": {"display_name": "Example Journal"}
            },
            "best_oa_location": {
                "landing_page_url": "https://repository.example/item"
            },
            "locations": [{
                "landing_page_url": "https://another.example/work",
                "pdf_url": null
            }]
        });

        let metadata = parse_openalex(&value).unwrap();
        assert_eq!(metadata.full_text_url, None);
        assert_eq!(
            metadata.journal_url.as_deref(),
            Some("https://publisher.example/article")
        );
    }

    #[test]
    fn semantic_scholar_match_requires_title_overlap_and_labels_certainty() {
        let value = json!({
            "data": [{
                "title": "Diagnostic efficacy of an artificial intelligence platform",
                "year": 2019,
                "venue": "EClinicalMedicine",
                "authors": [{"name": "H Lin"}],
                "abstract": "An abstract.",
                "tldr": {"text": "A short machine-generated summary."},
                "openAccessPdf": {"url": "https://example.org/full.pdf"},
                "url": "https://semanticscholar.org/paper/1",
                "externalIds": {"DOI": "10.1000/test"}
            }]
        });
        let metadata = parse_semantic_scholar(
            &value,
            "Diagnostic efficacy of an artificial intelligence platform",
        )
        .unwrap();
        assert_eq!(metadata.certainty, Some(MatchCertainty::High));
        assert_eq!(metadata.open_access, Some(true));
        assert_eq!(
            metadata.tldr_text.as_deref(),
            Some("A short machine-generated summary.")
        );
        assert!(parse_semantic_scholar(&value, "Completely unrelated book title").is_none());
    }

    #[test]
    fn openalex_enrichment_fills_semantic_gaps_without_replacing_match_identity() {
        let semantic_value = json!({
            "data": [{
                "title": "A carefully matched paper title",
                "authors": [],
                "venue": "Journal of Enrichment",
                "tldr": {"text": "Semantic summary"},
                "externalIds": {"DOI": "10.1000/enrich"},
                "url": "https://semanticscholar.org/paper/matched"
            }]
        });
        let openalex_value = json!({
            "id": "https://openalex.org/W99",
            "doi": "https://doi.org/10.1000/enrich",
            "title": "A differently normalized title",
            "publication_year": 2025,
            "authorships": [{"author": {"display_name": "Ada Author"}}],
            "primary_location": {
                "landing_page_url": "https://journal.example/work",
                "source": {"display_name": "J Enrichment"}
            },
            "best_oa_location": {"pdf_url": "https://repository.example/work.pdf"},
            "open_access": {"is_oa": true},
            "abstract_inverted_index": {"OpenAlex": [0], "abstract": [1]}
        });
        let semantic =
            parse_semantic_scholar(&semantic_value, "A carefully matched paper title").unwrap();
        assert!(semantic_metadata_needs_openalex(&semantic));
        let enriched =
            merge_semantic_with_openalex(semantic, parse_openalex(&openalex_value).unwrap());
        assert_eq!(enriched.source, ScholarlySource::SemanticScholar);
        assert_eq!(enriched.title, "A carefully matched paper title");
        assert_eq!(enriched.tldr_text.as_deref(), Some("Semantic summary"));
        assert_eq!(enriched.abstract_text.as_deref(), Some("OpenAlex abstract"));
        assert_eq!(enriched.authors, vec!["Ada Author"]);
        assert_eq!(enriched.year, Some(2025));
        assert_eq!(enriched.journal.as_deref(), Some("Journal of Enrichment"));
        assert_eq!(enriched.journal_short.as_deref(), Some("J Enrichment"));
        assert_eq!(enriched.open_access, Some(true));
        assert_eq!(
            enriched.landing_url.as_deref(),
            Some("https://semanticscholar.org/paper/matched")
        );
    }

    #[test]
    fn probable_title_prefers_the_descriptive_segment() {
        let reference = "[12] Lin H, Li R, Liu Z. Diagnostic efficacy and therapeutic decision-making capacity of an artificial intelligence platform. EClinicalMedicine. 2019;9:52-59.";
        assert_eq!(
            probable_title(reference),
            "Diagnostic efficacy and therapeutic decision-making capacity of an artificial intelligence platform"
        );
    }

    #[test]
    fn session_keys_doi_variants_together() {
        assert_eq!(
            reference_key("doi:10.1000/ABC."),
            reference_key("https://doi.org/10.1000/abc")
        );
    }

    #[test]
    fn unavailable_fetcher_becomes_terminal_instead_of_loading_forever() {
        let (fetcher, _events) = ScholarlyFetcher::new();
        let mut session = ScholarlySession::default();
        assert!(!session.request(&fetcher, 99, "A reference that cannot be queued"));
        assert!(matches!(
            session.state("A reference that cannot be queued"),
            Some(ScholarlyMetadataState::Failed(_))
        ));
    }
}
