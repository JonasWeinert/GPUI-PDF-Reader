//! Compile-time minimal-bundle replacement for optional website previews.

use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebsitePreview {
    pub title: Option<String>,
    pub site_name: Option<String>,
    pub details: Vec<String>,
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

#[derive(Default)]
pub struct LinkPreviewFetcher;

impl LinkPreviewFetcher {
    pub fn new() -> (Self, flume::Receiver<LinkPreviewEvent>) {
        let (_sender, receiver) = flume::unbounded();
        (Self, receiver)
    }

    pub fn begin_document(&self, _generation: u64) {}
}

#[derive(Default)]
pub struct LinkPreviewSession {
    websites: HashMap<String, WebsitePreviewState>,
}

impl LinkPreviewSession {
    pub fn new() -> Result<Self, String> {
        Ok(Self::default())
    }

    pub fn website(&self, url: &str) -> Option<&WebsitePreviewState> {
        self.websites.get(url)
    }

    pub fn request_website(
        &mut self,
        _fetcher: &LinkPreviewFetcher,
        _generation: u64,
        url: &str,
    ) -> bool {
        self.websites.insert(
            url.to_owned(),
            WebsitePreviewState::Failed("Website previews are omitted from this build".into()),
        );
        false
    }

    pub fn apply(&mut self, event: LinkPreviewEvent) -> Option<u64> {
        match event {
            LinkPreviewEvent::WebsiteFetched {
                generation,
                url,
                result,
            } => {
                self.websites.insert(
                    url,
                    match result {
                        Ok(preview) => WebsitePreviewState::Ready(preview),
                        Err(error) => WebsitePreviewState::Failed(error),
                    },
                );
                Some(generation)
            }
        }
    }
}
