use crate::link_preview::fetch_public_json;
use crate::scientific::detect_doi;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
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
    pub authors: Vec<String>,
    pub year: Option<u32>,
    pub journal: Option<String>,
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
    Ready(ScholarlyMetadata),
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
}

impl ScholarlyFetcher {
    pub fn new() -> (Self, mpsc::Receiver<ScholarlyEvent>) {
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
        let spawned = thread::Builder::new()
            .name("scholarly-metadata-fetch".into())
            .spawn(move || {
                let _guard = ActiveFetchGuard(active_fetches);
                if active_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let result =
                    fetch_scholarly_metadata(&reference).map_err(|error| concise_error(&error));
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
                        Ok(metadata) => ScholarlyMetadataState::Ready(metadata),
                        Err(error) => ScholarlyMetadataState::Failed(error),
                    },
                );
                Some(generation)
            }
        }
    }
}

fn fetch_scholarly_metadata(reference: &str) -> Result<ScholarlyMetadata, String> {
    if let Some(doi) = detect_doi(reference) {
        fetch_openalex(&doi)
    } else {
        fetch_semantic_scholar(reference)
    }
}

fn fetch_openalex(doi: &str) -> Result<ScholarlyMetadata, String> {
    let mut url = Url::parse(&format!(
        "https://api.openalex.org/works/https://doi.org/{doi}"
    ))
    .map_err(|error| format!("Could not build the OpenAlex request: {error}"))?;
    url.query_pairs_mut().append_pair(
        "select",
        "id,doi,title,display_name,publication_year,authorships,primary_location,locations,best_oa_location,open_access,abstract_inverted_index",
    );
    let body = fetch_public_json(url.as_str(), MAX_METADATA_BYTES)
        .map_err(|error| format!("OpenAlex: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("OpenAlex returned invalid metadata: {error}"))?;
    parse_openalex(&value).ok_or_else(|| "OpenAlex returned no usable work metadata".to_owned())
}

fn fetch_semantic_scholar(reference: &str) -> Result<ScholarlyMetadata, String> {
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
            "title,abstract,authors,year,venue,openAccessPdf,url,externalIds",
        );
    let body = fetch_public_json(url.as_str(), MAX_METADATA_BYTES)
        .map_err(|error| format!("Semantic Scholar: {error}"))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("Semantic Scholar returned invalid metadata: {error}"))?;
    parse_semantic_scholar(&value, &query)
        .ok_or_else(|| "Semantic Scholar found no reliable title match".to_owned())
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
        authors,
        year: value
            .get("publication_year")
            .and_then(Value::as_u64)
            .and_then(|year| u32::try_from(year).ok()),
        journal,
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
        assert!(parse_semantic_scholar(&value, "Completely unrelated book title").is_none());
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
