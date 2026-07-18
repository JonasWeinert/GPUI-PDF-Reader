//! Compile-time minimal-bundle replacement for optional scholarly providers.

use std::collections::HashMap;

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

impl ScholarlyEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Fetched { generation, .. } => *generation,
        }
    }
}

#[derive(Default)]
pub struct ScholarlyFetcher;

impl ScholarlyFetcher {
    pub fn new() -> (Self, flume::Receiver<ScholarlyEvent>) {
        let (_sender, receiver) = flume::unbounded();
        (Self, receiver)
    }

    pub fn begin_document(&self, _generation: u64) {}
}

#[derive(Default)]
pub struct ScholarlySession {
    entries: HashMap<String, ScholarlyMetadataState>,
}

impl ScholarlySession {
    pub fn state(&self, reference: &str) -> Option<&ScholarlyMetadataState> {
        self.entries.get(reference)
    }

    pub fn request(
        &mut self,
        _fetcher: &ScholarlyFetcher,
        _generation: u64,
        reference: &str,
    ) -> bool {
        self.entries.insert(
            reference.to_owned(),
            ScholarlyMetadataState::Failed(
                "Scholarly metadata is omitted from this build".to_owned(),
            ),
        );
        false
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
