use std::fmt;

const MAX_RENDER_CACHE_ENTRIES: usize = 4_096;
const MAX_RENDER_CACHE_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_TEXT_CACHE_PAGES: usize = 16_384;
const MAX_TEXT_CACHE_CHARACTERS: usize = 100_000_000;
const MAX_PREVIEW_CACHE_ENTRIES: usize = 512;
const MAX_PREVIEW_CACHE_BYTES: usize = 512 * 1024 * 1024;

/// Explicit memory/count budgets. Caches are always document-scoped and must
/// be purged when their generation closes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CachePolicy {
    render_entries: usize,
    render_bytes: usize,
    text_pages: usize,
    text_characters: usize,
    preview_entries: usize,
    preview_bytes: usize,
}

impl CachePolicy {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        render_entries: usize,
        render_bytes: usize,
        text_pages: usize,
        text_characters: usize,
        preview_entries: usize,
        preview_bytes: usize,
    ) -> Result<Self, CachePolicyError> {
        validate_pair(
            CacheKind::Render,
            render_entries,
            render_bytes,
            MAX_RENDER_CACHE_ENTRIES,
            MAX_RENDER_CACHE_BYTES,
        )?;
        validate_pair(
            CacheKind::Text,
            text_pages,
            text_characters,
            MAX_TEXT_CACHE_PAGES,
            MAX_TEXT_CACHE_CHARACTERS,
        )?;
        validate_pair(
            CacheKind::Preview,
            preview_entries,
            preview_bytes,
            MAX_PREVIEW_CACHE_ENTRIES,
            MAX_PREVIEW_CACHE_BYTES,
        )?;
        Ok(Self {
            render_entries,
            render_bytes,
            text_pages,
            text_characters,
            preview_entries,
            preview_bytes,
        })
    }

    pub fn render_entries(self) -> usize {
        self.render_entries
    }

    pub fn render_bytes(self) -> usize {
        self.render_bytes
    }

    pub fn text_pages(self) -> usize {
        self.text_pages
    }

    pub fn text_characters(self) -> usize {
        self.text_characters
    }

    pub fn preview_entries(self) -> usize {
        self.preview_entries
    }

    pub fn preview_bytes(self) -> usize {
        self.preview_bytes
    }

    pub fn render_cache_enabled(self) -> bool {
        self.render_entries != 0
    }

    pub fn text_cache_enabled(self) -> bool {
        self.text_pages != 0
    }

    pub fn preview_cache_enabled(self) -> bool {
        self.preview_entries != 0
    }
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            render_entries: 48,
            render_bytes: 128 * 1024 * 1024,
            text_pages: 16,
            text_characters: 1_600_000,
            preview_entries: 16,
            preview_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheKind {
    Render,
    Text,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePolicyError {
    InconsistentDisabledBudget(&'static str),
    EntryLimitExceeded(&'static str),
    SizeLimitExceeded(&'static str),
}

impl fmt::Display for CachePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InconsistentDisabledBudget(kind) => {
                write!(
                    formatter,
                    "{kind} cache count and size must both be zero or non-zero"
                )
            }
            Self::EntryLimitExceeded(kind) => write!(formatter, "{kind} cache count is too large"),
            Self::SizeLimitExceeded(kind) => write!(formatter, "{kind} cache budget is too large"),
        }
    }
}

impl std::error::Error for CachePolicyError {}

fn validate_pair(
    kind: CacheKind,
    entries: usize,
    size: usize,
    max_entries: usize,
    max_size: usize,
) -> Result<(), CachePolicyError> {
    let label = match kind {
        CacheKind::Render => "render",
        CacheKind::Text => "text",
        CacheKind::Preview => "preview",
    };
    if (entries == 0) != (size == 0) {
        return Err(CachePolicyError::InconsistentDisabledBudget(label));
    }
    if entries > max_entries {
        return Err(CachePolicyError::EntryLimitExceeded(label));
    }
    if size > max_size {
        return Err(CachePolicyError::SizeLimitExceeded(label));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_current_reader_budgets() {
        let policy = CachePolicy::default();
        assert_eq!(policy.render_entries(), 48);
        assert_eq!(policy.render_bytes(), 128 * 1024 * 1024);
        assert_eq!(policy.text_pages(), 16);
        assert!(policy.render_cache_enabled());
    }

    #[test]
    fn caches_can_be_disabled_but_not_partially_configured() {
        let disabled = CachePolicy::try_new(0, 0, 0, 0, 0, 0).unwrap();
        assert!(!disabled.render_cache_enabled());
        assert!(!disabled.text_cache_enabled());
        assert!(!disabled.preview_cache_enabled());
        assert_eq!(
            CachePolicy::try_new(1, 0, 0, 0, 0, 0),
            Err(CachePolicyError::InconsistentDisabledBudget("render"))
        );
    }

    #[test]
    fn pathological_budgets_are_rejected() {
        assert_eq!(
            CachePolicy::try_new(MAX_RENDER_CACHE_ENTRIES + 1, 1, 0, 0, 0, 0),
            Err(CachePolicyError::EntryLimitExceeded("render"))
        );
    }
}
