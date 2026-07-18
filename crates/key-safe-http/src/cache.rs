use crate::{CancellationToken, HttpError};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::{NamedTempFile, TempDir};

const HARD_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const HARD_MAX_FILE_BYTES: usize = 512 * 1024 * 1024;
const HARD_MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const HARD_MAX_ENTRIES: usize = 1_024;
const HARD_MAX_KEY_BYTES: usize = 512;

/// Independent memory, file, entry-size, and entry-count cache quotas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentCacheLimits {
    /// Maximum memory-backed bytes retained by the cache.
    pub memory_bytes: usize,
    /// Maximum file-backed bytes retained by the cache.
    pub file_bytes: usize,
    /// Maximum size of one value in either tier.
    pub entry_bytes: usize,
    /// Maximum number of retained entries.
    pub entries: usize,
}

impl Default for DocumentCacheLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 8 * 1024 * 1024,
            file_bytes: 64 * 1024 * 1024,
            entry_bytes: 16 * 1024 * 1024,
            entries: 256,
        }
    }
}

impl DocumentCacheLimits {
    fn validate(self) -> Result<Self, HttpError> {
        if self.memory_bytes > HARD_MAX_MEMORY_BYTES {
            return Err(HttpError::CacheQuotaExceeded {
                quota: "memory",
                limit: HARD_MAX_MEMORY_BYTES,
            });
        }
        if self.file_bytes > HARD_MAX_FILE_BYTES {
            return Err(HttpError::CacheQuotaExceeded {
                quota: "file",
                limit: HARD_MAX_FILE_BYTES,
            });
        }
        if self.entry_bytes == 0 || self.entry_bytes > HARD_MAX_ENTRY_BYTES {
            return Err(HttpError::CacheQuotaExceeded {
                quota: "single-entry",
                limit: HARD_MAX_ENTRY_BYTES,
            });
        }
        if self.entries == 0 || self.entries > HARD_MAX_ENTRIES {
            return Err(HttpError::CacheQuotaExceeded {
                quota: "entry-count",
                limit: HARD_MAX_ENTRIES,
            });
        }
        if self.memory_bytes == 0 && self.file_bytes == 0 {
            return Err(HttpError::Cache(
                "at least one cache tier must be enabled".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// Snapshot of current cache consumption.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentCacheUsage {
    /// Number of retained entries.
    pub entries: usize,
    /// Memory-backed bytes.
    pub memory_bytes: usize,
    /// File-backed bytes.
    pub file_bytes: usize,
}

/// A cache lookup result.
#[derive(Clone, Debug)]
pub enum DocumentCacheEntry {
    /// Immutable memory-backed bytes.
    Memory(Arc<[u8]>),
    /// A file that remains valid until its key is removed or the cache is
    /// purged/dropped.
    File {
        /// Ephemeral path owned by the cache.
        path: PathBuf,
        /// Stored byte length.
        len: usize,
    },
}

impl DocumentCacheEntry {
    /// Stored byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::File { len, .. } => *len,
        }
    }

    /// Whether the cached value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

enum StoredEntry {
    Memory(Arc<[u8]>),
    File { file: NamedTempFile, len: usize },
}

impl StoredEntry {
    fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::File { len, .. } => *len,
        }
    }

    fn is_memory(&self) -> bool {
        matches!(self, Self::Memory(_))
    }

    fn snapshot(&self) -> DocumentCacheEntry {
        match self {
            Self::Memory(bytes) => DocumentCacheEntry::Memory(Arc::clone(bytes)),
            Self::File { file, len } => DocumentCacheEntry::File {
                path: file.path().to_path_buf(),
                len: *len,
            },
        }
    }
}

struct CacheState {
    entries: HashMap<String, StoredEntry>,
    memory_bytes: usize,
    file_bytes: usize,
}

/// Per-document ephemeral cache with explicit purge semantics.
///
/// File values live in a private temporary directory and are removed when
/// their key is replaced, [`Self::purge`] is called, or this cache is dropped.
/// This type intentionally has no `Clone`: the document session should own and
/// purge one cache generation.
pub struct DocumentCache {
    directory: TempDir,
    limits: DocumentCacheLimits,
    state: Mutex<CacheState>,
}

impl DocumentCache {
    /// Creates a new private cache directory for one document generation.
    pub fn new(limits: DocumentCacheLimits) -> Result<Self, HttpError> {
        let limits = limits.validate()?;
        let directory = tempfile::Builder::new()
            .prefix("key-document-http-cache-")
            .tempdir()
            .map_err(|error| HttpError::Cache(error.to_string()))?;
        Ok(Self {
            directory,
            limits,
            state: Mutex::new(CacheState {
                entries: HashMap::new(),
                memory_bytes: 0,
                file_bytes: 0,
            }),
        })
    }

    /// Returns a snapshot of an entry if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<DocumentCacheEntry> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(key)
            .map(StoredEntry::snapshot)
    }

    /// Stores a memory-backed value, replacing the same key atomically.
    pub fn insert_memory(
        &self,
        key: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<(), HttpError> {
        let key = validated_key(key.into())?;
        let bytes = bytes.into();
        self.validate_entry_len(bytes.len())?;
        let entry = StoredEntry::Memory(Arc::from(bytes));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_replacement_quota(&state, &key, &entry, self.limits)?;
        replace_entry(&mut state, key, entry);
        Ok(())
    }

    /// Streams a value into the file tier without retaining it all in memory.
    ///
    /// The temporary file is not made visible in the cache until the complete
    /// input has passed cancellation and quota checks.
    pub fn insert_file<R: Read>(
        &self,
        key: impl Into<String>,
        mut reader: R,
        cancellation: &CancellationToken,
    ) -> Result<(), HttpError> {
        let key = validated_key(key.into())?;
        let mut file = NamedTempFile::new_in(self.directory.path())
            .map_err(|error| HttpError::Cache(error.to_string()))?;
        let mut length = 0_usize;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            if cancellation.is_cancelled() {
                return Err(HttpError::Cancelled);
            }
            let read = reader
                .read(&mut buffer)
                .map_err(|error| HttpError::Cache(error.to_string()))?;
            if read == 0 {
                break;
            }
            length = length
                .checked_add(read)
                .ok_or(HttpError::CacheQuotaExceeded {
                    quota: "single-entry",
                    limit: self.limits.entry_bytes,
                })?;
            self.validate_entry_len(length)?;
            file.as_file_mut()
                .write_all(&buffer[..read])
                .map_err(|error| HttpError::Cache(error.to_string()))?;
        }
        if cancellation.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        file.as_file_mut()
            .flush()
            .map_err(|error| HttpError::Cache(error.to_string()))?;
        let entry = StoredEntry::File { file, len: length };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_replacement_quota(&state, &key, &entry, self.limits)?;
        replace_entry(&mut state, key, entry);
        Ok(())
    }

    /// Removes one key and its owned file, if present.
    pub fn remove(&self, key: &str) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = state.entries.remove(key) else {
            return false;
        };
        subtract_usage(&mut state, &entry);
        true
    }

    /// Immediately drops every retained memory value and removes every file.
    pub fn purge(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.entries.clear();
        state.memory_bytes = 0;
        state.file_bytes = 0;
    }

    /// Returns current retained usage.
    #[must_use]
    pub fn usage(&self) -> DocumentCacheUsage {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        DocumentCacheUsage {
            entries: state.entries.len(),
            memory_bytes: state.memory_bytes,
            file_bytes: state.file_bytes,
        }
    }

    /// Private directory path for trusted host diagnostics.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    fn validate_entry_len(&self, length: usize) -> Result<(), HttpError> {
        if length > self.limits.entry_bytes {
            return Err(HttpError::CacheQuotaExceeded {
                quota: "single-entry",
                limit: self.limits.entry_bytes,
            });
        }
        Ok(())
    }
}

fn validated_key(key: String) -> Result<String, HttpError> {
    if key.is_empty() || key.len() > HARD_MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(HttpError::Cache(format!(
            "cache key must contain 1..={HARD_MAX_KEY_BYTES} non-control bytes"
        )));
    }
    Ok(key)
}

fn validate_replacement_quota(
    state: &CacheState,
    key: &str,
    replacement: &StoredEntry,
    limits: DocumentCacheLimits,
) -> Result<(), HttpError> {
    let existing = state.entries.get(key);
    let resulting_entries = state.entries.len() + usize::from(existing.is_none());
    if resulting_entries > limits.entries {
        return Err(HttpError::CacheQuotaExceeded {
            quota: "entry-count",
            limit: limits.entries,
        });
    }
    let old_memory = existing
        .filter(|entry| entry.is_memory())
        .map_or(0, StoredEntry::len);
    let old_file = existing
        .filter(|entry| !entry.is_memory())
        .map_or(0, StoredEntry::len);
    let (new_memory, new_file) = if replacement.is_memory() {
        (replacement.len(), 0)
    } else {
        (0, replacement.len())
    };
    let memory = state
        .memory_bytes
        .saturating_sub(old_memory)
        .saturating_add(new_memory);
    if memory > limits.memory_bytes {
        return Err(HttpError::CacheQuotaExceeded {
            quota: "memory",
            limit: limits.memory_bytes,
        });
    }
    let file = state
        .file_bytes
        .saturating_sub(old_file)
        .saturating_add(new_file);
    if file > limits.file_bytes {
        return Err(HttpError::CacheQuotaExceeded {
            quota: "file",
            limit: limits.file_bytes,
        });
    }
    Ok(())
}

fn replace_entry(state: &mut CacheState, key: String, replacement: StoredEntry) {
    if let Some(previous) = state.entries.remove(&key) {
        subtract_usage(state, &previous);
    }
    if replacement.is_memory() {
        state.memory_bytes += replacement.len();
    } else {
        state.file_bytes += replacement.len();
    }
    state.entries.insert(key, replacement);
}

fn subtract_usage(state: &mut CacheState, entry: &StoredEntry) {
    if entry.is_memory() {
        state.memory_bytes = state.memory_bytes.saturating_sub(entry.len());
    } else {
        state.file_bytes = state.file_bytes.saturating_sub(entry.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    fn small_cache() -> DocumentCache {
        DocumentCache::new(DocumentCacheLimits {
            memory_bytes: 8,
            file_bytes: 8,
            entry_bytes: 8,
            entries: 2,
        })
        .unwrap()
    }

    #[test]
    fn memory_and_file_tiers_have_independent_bounded_accounting() {
        let cache = small_cache();
        cache.insert_memory("metadata", b"1234".to_vec()).unwrap();
        cache
            .insert_file(
                "image",
                Cursor::new(b"12345678"),
                &CancellationToken::active(),
            )
            .unwrap();
        assert_eq!(
            cache.usage(),
            DocumentCacheUsage {
                entries: 2,
                memory_bytes: 4,
                file_bytes: 8,
            }
        );
        assert!(matches!(
            cache.insert_memory("third", b"x".to_vec()),
            Err(HttpError::CacheQuotaExceeded {
                quota: "entry-count",
                ..
            })
        ));
        assert!(matches!(
            cache.insert_memory("metadata", b"123456789".to_vec()),
            Err(HttpError::CacheQuotaExceeded {
                quota: "single-entry",
                ..
            })
        ));
    }

    #[test]
    fn replacing_between_tiers_updates_usage_and_removes_old_file() {
        let cache = small_cache();
        cache
            .insert_file("same", Cursor::new(b"file"), &CancellationToken::active())
            .unwrap();
        let old_path = match cache.get("same").unwrap() {
            DocumentCacheEntry::File { path, .. } => path,
            DocumentCacheEntry::Memory(_) => panic!("expected file"),
        };
        assert!(old_path.exists());

        cache.insert_memory("same", b"memory".to_vec()).unwrap();

        assert!(!old_path.exists());
        assert_eq!(cache.usage().file_bytes, 0);
        assert_eq!(cache.usage().memory_bytes, 6);
    }

    #[test]
    fn purge_removes_files_and_resets_all_usage() {
        let cache = small_cache();
        cache.insert_memory("memory", b"1234".to_vec()).unwrap();
        cache
            .insert_file("file", Cursor::new(b"1234"), &CancellationToken::active())
            .unwrap();
        let path = match cache.get("file").unwrap() {
            DocumentCacheEntry::File { path, .. } => path,
            DocumentCacheEntry::Memory(_) => panic!("expected file"),
        };

        cache.purge();

        assert!(!path.exists());
        assert_eq!(cache.usage(), DocumentCacheUsage::default());
        assert!(cache.get("memory").is_none());
    }

    struct CancelAfterChunk {
        source: crate::CancellationSource,
        read_once: bool,
    }

    impl Read for CancelAfterChunk {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.read_once {
                return Ok(0);
            }
            self.read_once = true;
            buffer[..4].copy_from_slice(b"data");
            self.source.cancel();
            Ok(4)
        }
    }

    #[test]
    fn cancelled_file_insert_never_becomes_visible() {
        let cache = small_cache();
        let source = crate::CancellationSource::new();
        let result = cache.insert_file(
            "file",
            CancelAfterChunk {
                source: source.clone(),
                read_once: false,
            },
            &source.token(),
        );
        assert_eq!(result, Err(HttpError::Cancelled));
        assert!(cache.get("file").is_none());
        assert_eq!(cache.usage(), DocumentCacheUsage::default());
    }
}
