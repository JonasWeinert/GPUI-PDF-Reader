//! Atomic, app-data-backed storage for extension settings and document state.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use key_extension_api::{DataValue, ExtensionError, ExtensionErrorCode, ExtensionId, StorageArea};
use key_extension_host::ExtensionStorage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::DocumentKey;

const SCHEMA_VERSION: u16 = 1;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEMP_FILE_ATTEMPTS: usize = 32;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct ExtensionStorageState {
    document: Option<String>,
}

#[derive(Default)]
struct SharedExtensionStorageState {
    ephemeral: BTreeMap<(ExtensionId, String), DataValue>,
}

struct SharedExtensionStorage {
    root: PathBuf,
    state: Mutex<SharedExtensionStorageState>,
    io_lock: Mutex<()>,
}

/// Persistent namespaced extension storage selected by the standalone app.
/// Settings live in app data, document values are keyed by immutable document
/// identity, and ephemeral values never touch disk.
pub struct JsonExtensionStorage {
    shared: Arc<SharedExtensionStorage>,
    state: Mutex<ExtensionStorageState>,
}

/// Immutable storage context for one document.
///
/// Unlike [`JsonExtensionStorage::select_document`], a scope cannot be retargeted
/// by another window while an extension storage operation is in flight. Clones
/// share settings, ephemeral values and serialized disk I/O, while document
/// values remain pinned to this scope's namespace.
#[derive(Clone)]
pub struct JsonExtensionStorageScope {
    shared: Arc<SharedExtensionStorage>,
    document: Option<String>,
}

impl JsonExtensionStorage {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            shared: Arc::new(SharedExtensionStorage {
                root,
                state: Mutex::new(SharedExtensionStorageState::default()),
                io_lock: Mutex::new(()),
            }),
            state: Mutex::new(ExtensionStorageState::default()),
        }
    }

    /// Returns a storage context permanently bound to `document`.
    ///
    /// Multi-window hosts should give each document service router its own
    /// scope instead of mutating the legacy selected-document cursor.
    pub fn document_scope(
        &self,
        document: impl Into<String>,
    ) -> Result<JsonExtensionStorageScope, ExtensionError> {
        let document = document.into();
        validate_document_namespace(&document)?;
        Ok(JsonExtensionStorageScope {
            shared: self.shared.clone(),
            document: Some(document),
        })
    }

    /// Returns an application-level context. Settings and ephemeral cache are
    /// available; document storage fails closed until a document scope is used.
    #[must_use]
    pub fn application_scope(&self) -> JsonExtensionStorageScope {
        JsonExtensionStorageScope {
            shared: self.shared.clone(),
            document: None,
        }
    }

    /// Selects the document namespace used by subsequent document-area calls.
    /// `None` makes document storage unavailable until another PDF opens.
    pub fn select_document(&self, document: Option<String>) -> Result<(), ExtensionError> {
        if let Some(document) = document.as_deref() {
            validate_document_namespace(document)?;
        }
        self.state
            .lock()
            .map_err(|_| internal("extension storage lock is poisoned"))?
            .document = document;
        Ok(())
    }

    fn selected_document(&self) -> Result<Option<String>, ExtensionError> {
        self.state
            .lock()
            .map_err(|_| internal("extension storage lock is poisoned"))
            .map(|state| state.document.clone())
    }
}

impl JsonExtensionStorageScope {
    #[must_use]
    pub fn document_namespace(&self) -> Option<&str> {
        self.document.as_deref()
    }
}

impl SharedExtensionStorage {
    fn path(
        &self,
        document: Option<&str>,
        extension: &ExtensionId,
        area: StorageArea,
    ) -> Result<PathBuf, ExtensionError> {
        let owner = extension.as_str();
        match area {
            StorageArea::Settings => Ok(self.root.join("settings").join(format!("{owner}.json"))),
            StorageArea::Document => {
                let document = document.ok_or_else(|| ExtensionError {
                    code: ExtensionErrorCode::StaleResource,
                    message: "no active document storage namespace".into(),
                    retryable: false,
                })?;
                Ok(self
                    .root
                    .join("documents")
                    .join(document)
                    .join(format!("{owner}.json")))
            }
            StorageArea::EphemeralCache => Err(invalid("ephemeral storage has no disk path")),
        }
    }

    fn load(&self, path: &Path) -> Result<BTreeMap<String, DataValue>, ExtensionError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(io_error("read metadata", path, error)),
        };
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return Err(quota("stored extension data exceeds the host file limit"));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)
            .map_err(|error| io_error("open", path, error))?
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("read", path, error))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(quota("stored extension data exceeds the host file limit"));
        }
        let stored: StoredValues = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("stored extension data is invalid: {error}")))?;
        if stored.schema_version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported extension storage schema {}",
                stored.schema_version
            )));
        }
        Ok(stored.values)
    }

    fn save(
        &self,
        path: &Path,
        values: BTreeMap<String, DataValue>,
        quota_bytes: u64,
    ) -> Result<(), ExtensionError> {
        let bytes = serde_json::to_vec_pretty(&StoredValues {
            schema_version: SCHEMA_VERSION,
            values,
        })
        .map_err(|error| internal(format!("could not encode extension storage: {error}")))?;
        if bytes.len() > MAX_FILE_BYTES
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > quota_bytes
        {
            return Err(quota(format!(
                "encoded extension storage uses {} bytes; quota is {quota_bytes}",
                bytes.len()
            )));
        }
        atomic_write(path, &bytes)
    }
}

fn validate_document_namespace(document: &str) -> Result<(), ExtensionError> {
    if document.is_empty()
        || document.len() > 128
        || !document
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(invalid("document storage namespace is invalid"))
    } else {
        Ok(())
    }
}

impl ExtensionStorage for JsonExtensionStorage {
    fn get(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        self.shared
            .get(self.selected_document()?.as_deref(), extension, area, key)
    }

    fn put(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
        value: DataValue,
        quota_bytes: u64,
    ) -> Result<DataValue, ExtensionError> {
        self.shared.put(
            self.selected_document()?.as_deref(),
            extension,
            area,
            key,
            value,
            quota_bytes,
        )
    }

    fn delete(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        self.shared
            .delete(self.selected_document()?.as_deref(), extension, area, key)
    }

    fn clear_area(&self, extension: &ExtensionId, area: StorageArea) {
        let document = self.selected_document().ok().flatten();
        self.shared.clear_area(document.as_deref(), extension, area);
    }

    fn clear_all(&self, area: StorageArea) {
        self.shared.clear_all(area);
    }
}

impl ExtensionStorage for JsonExtensionStorageScope {
    fn get(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        self.shared
            .get(self.document.as_deref(), extension, area, key)
    }

    fn put(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
        value: DataValue,
        quota_bytes: u64,
    ) -> Result<DataValue, ExtensionError> {
        self.shared.put(
            self.document.as_deref(),
            extension,
            area,
            key,
            value,
            quota_bytes,
        )
    }

    fn delete(
        &self,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        self.shared
            .delete(self.document.as_deref(), extension, area, key)
    }

    fn clear_area(&self, extension: &ExtensionId, area: StorageArea) {
        self.shared
            .clear_area(self.document.as_deref(), extension, area);
    }

    fn clear_all(&self, area: StorageArea) {
        self.shared.clear_all(area);
    }
}

impl SharedExtensionStorage {
    fn get(
        &self,
        document: Option<&str>,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        validate_key(key)?;
        if area == StorageArea::EphemeralCache {
            return Ok(self
                .state
                .lock()
                .map_err(|_| internal("extension storage lock is poisoned"))?
                .ephemeral
                .get(&(extension.clone(), key.to_owned()))
                .cloned()
                .unwrap_or(DataValue::Null));
        }
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| internal("extension storage I/O lock is poisoned"))?;
        Ok(self
            .load(&self.path(document, extension, area)?)?
            .get(key)
            .cloned()
            .unwrap_or(DataValue::Null))
    }

    fn put(
        &self,
        document: Option<&str>,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
        value: DataValue,
        quota_bytes: u64,
    ) -> Result<DataValue, ExtensionError> {
        validate_key(key)?;
        if area == StorageArea::EphemeralCache {
            let mut state = self
                .state
                .lock()
                .map_err(|_| internal("extension storage lock is poisoned"))?;
            state
                .ephemeral
                .insert((extension.clone(), key.to_owned()), value);
            let used = state
                .ephemeral
                .iter()
                .filter(|((owner, _), _)| owner == extension)
                .map(|((_, key), value)| key.len().saturating_add(value_size(value)))
                .sum::<usize>();
            if u64::try_from(used).unwrap_or(u64::MAX) > quota_bytes {
                state.ephemeral.remove(&(extension.clone(), key.to_owned()));
                return Err(quota("ephemeral extension storage quota exceeded"));
            }
            return Ok(DataValue::Null);
        }
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| internal("extension storage I/O lock is poisoned"))?;
        let path = self.path(document, extension, area)?;
        let mut values = self.load(&path)?;
        values.insert(key.to_owned(), value);
        self.save(&path, values, quota_bytes)?;
        Ok(DataValue::Null)
    }

    fn delete(
        &self,
        document: Option<&str>,
        extension: &ExtensionId,
        area: StorageArea,
        key: &str,
    ) -> Result<DataValue, ExtensionError> {
        validate_key(key)?;
        if area == StorageArea::EphemeralCache {
            self.state
                .lock()
                .map_err(|_| internal("extension storage lock is poisoned"))?
                .ephemeral
                .remove(&(extension.clone(), key.to_owned()));
            return Ok(DataValue::Null);
        }
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| internal("extension storage I/O lock is poisoned"))?;
        let path = self.path(document, extension, area)?;
        let mut values = self.load(&path)?;
        values.remove(key);
        // Deletion can only reduce authority; keep the existing file limit as
        // its write ceiling rather than requiring the caller's quota again.
        self.save(&path, values, MAX_FILE_BYTES as u64)?;
        Ok(DataValue::Null)
    }

    fn clear_area(&self, document: Option<&str>, extension: &ExtensionId, area: StorageArea) {
        if area == StorageArea::EphemeralCache {
            if let Ok(mut state) = self.state.lock() {
                state.ephemeral.retain(|(owner, _), _| owner != extension);
            }
            return;
        }
        if let Ok(path) = self.path(document, extension, area) {
            let _guard = self.io_lock.lock();
            let _ = fs::remove_file(path);
        }
    }

    fn clear_all(&self, area: StorageArea) {
        if area == StorageArea::EphemeralCache
            && let Ok(mut state) = self.state.lock()
        {
            state.ephemeral.clear();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredValues {
    schema_version: u16,
    values: BTreeMap<String, DataValue>,
}

/// Stable document namespace derived from canonical path and exact revision.
#[must_use]
pub fn extension_document_namespace(document: &DocumentKey) -> String {
    let identity = document.identity();
    let mut digest = Sha256::new();
    let path = document.source_path().as_os_str().as_encoded_bytes();
    digest.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(path);
    digest.update(identity.byte_len().to_le_bytes());
    digest.update(identity.modified_unix_seconds().to_le_bytes());
    digest.update(identity.modified_nanos().to_le_bytes());
    digest.update(
        u64::try_from(identity.page_count())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut encoded, byte| {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
            encoded
        })
}

fn validate_key(key: &str) -> Result<(), ExtensionError> {
    if key.is_empty()
        || key.len() > 256
        || !key.is_ascii()
        || key.starts_with('.')
        || key.contains("..")
        || key.contains(['/', '\\'])
    {
        Err(invalid("storage key is not a bounded logical key"))
    } else {
        Ok(())
    }
}

fn value_size(value: &DataValue) -> usize {
    match value {
        DataValue::Null | DataValue::Boolean(_) => 1,
        DataValue::Integer(_) | DataValue::Number(_) => 8,
        DataValue::String(value) => value.len(),
        DataValue::List(values) => values.iter().map(value_size).sum(),
        DataValue::Record(values) => values
            .iter()
            .map(|(key, value)| key.len().saturating_add(value_size(value)))
            .sum(),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ExtensionError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("extension storage path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| io_error("create directory", parent, error))?;
    for _ in 0..MAX_TEMP_FILE_ATTEMPTS {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".key-extension-{sequence:016x}.tmp"));
        let mut file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create temporary file", &temporary, error)),
        };
        let result = (|| {
            file.write_all(bytes)
                .map_err(|error| io_error("write temporary file", &temporary, error))?;
            file.sync_all()
                .map_err(|error| io_error("sync temporary file", &temporary, error))?;
            fs::rename(&temporary, path)
                .map_err(|error| io_error("replace extension storage", path, error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
    Err(ExtensionError {
        code: ExtensionErrorCode::TemporarilyUnavailable,
        message: "could not allocate an extension storage temporary file".into(),
        retryable: true,
    })
}

fn invalid(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::InvalidRequest,
        message: message.into(),
        retryable: false,
    }
}

fn internal(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::Internal,
        message: message.into(),
        retryable: true,
    }
}

fn quota(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::QuotaExceeded,
        message: message.into(),
        retryable: false,
    }
}

fn io_error(operation: &str, path: &Path, error: io::Error) -> ExtensionError {
    internal(format!("could not {operation} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use key_extension_host::ExtensionStorage;

    use super::*;

    fn temporary_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("key-extension-storage-{nonce}"))
    }

    #[test]
    fn settings_persist_across_store_instances_and_remain_namespaced() {
        let root = temporary_root();
        let first = JsonExtensionStorage::new(root.clone());
        let a = ExtensionId::parse("org.example.a").unwrap();
        let b = ExtensionId::parse("org.example.b").unwrap();
        first
            .put(
                &a,
                StorageArea::Settings,
                "theme",
                DataValue::String("blue".into()),
                4096,
            )
            .unwrap();
        let reopened = JsonExtensionStorage::new(root.clone());
        assert_eq!(
            reopened.get(&a, StorageArea::Settings, "theme").unwrap(),
            DataValue::String("blue".into())
        );
        assert_eq!(
            reopened.get(&b, StorageArea::Settings, "theme").unwrap(),
            DataValue::Null
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_and_ephemeral_scopes_do_not_leak() {
        let root = temporary_root();
        let store = JsonExtensionStorage::new(root.clone());
        let extension = ExtensionId::parse("org.example.document").unwrap();
        store.select_document(Some("document-one".into())).unwrap();
        store
            .put(
                &extension,
                StorageArea::Document,
                "position",
                DataValue::Integer(3),
                4096,
            )
            .unwrap();
        store
            .put(
                &extension,
                StorageArea::EphemeralCache,
                "preview",
                DataValue::String("cached".into()),
                4096,
            )
            .unwrap();
        store.select_document(Some("document-two".into())).unwrap();
        assert_eq!(
            store
                .get(&extension, StorageArea::Document, "position")
                .unwrap(),
            DataValue::Null
        );
        store.clear_all(StorageArea::EphemeralCache);
        assert_eq!(
            store
                .get(&extension, StorageArea::EphemeralCache, "preview")
                .unwrap(),
            DataValue::Null
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immutable_document_scopes_remain_isolated_under_concurrent_window_io() {
        let root = temporary_root();
        let store = JsonExtensionStorage::new(root.clone());
        let first = store.document_scope("document-one").unwrap();
        let second = store.document_scope("document-two").unwrap();
        let extension = ExtensionId::parse("org.example.concurrent").unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let first_thread = {
            let barrier = barrier.clone();
            let extension = extension.clone();
            thread::spawn(move || {
                barrier.wait();
                for position in 0..32 {
                    first
                        .put(
                            &extension,
                            StorageArea::Document,
                            "position",
                            DataValue::Integer(position),
                            4096,
                        )
                        .unwrap();
                    assert_eq!(
                        first
                            .get(&extension, StorageArea::Document, "position")
                            .unwrap(),
                        DataValue::Integer(position)
                    );
                }
            })
        };
        let second_thread = {
            let barrier = barrier.clone();
            let extension = extension.clone();
            thread::spawn(move || {
                barrier.wait();
                for position in 100..132 {
                    second
                        .put(
                            &extension,
                            StorageArea::Document,
                            "position",
                            DataValue::Integer(position),
                            4096,
                        )
                        .unwrap();
                    assert_eq!(
                        second
                            .get(&extension, StorageArea::Document, "position")
                            .unwrap(),
                        DataValue::Integer(position)
                    );
                }
            })
        };

        first_thread.join().unwrap();
        second_thread.join().unwrap();
        let first = store.document_scope("document-one").unwrap();
        let second = store.document_scope("document-two").unwrap();
        assert_eq!(
            first
                .get(&extension, StorageArea::Document, "position")
                .unwrap(),
            DataValue::Integer(31)
        );
        assert_eq!(
            second
                .get(&extension, StorageArea::Document, "position")
                .unwrap(),
            DataValue::Integer(131)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retargeting_legacy_cursor_cannot_retarget_an_immutable_scope() {
        let root = temporary_root();
        let store = JsonExtensionStorage::new(root.clone());
        let extension = ExtensionId::parse("org.example.scope").unwrap();
        let fixed = store.document_scope("fixed-document").unwrap();

        store
            .select_document(Some("other-document".into()))
            .unwrap();
        fixed
            .put(
                &extension,
                StorageArea::Document,
                "selection",
                DataValue::Integer(7),
                4096,
            )
            .unwrap();
        assert_eq!(
            store
                .get(&extension, StorageArea::Document, "selection")
                .unwrap(),
            DataValue::Null
        );
        assert_eq!(
            fixed
                .get(&extension, StorageArea::Document, "selection")
                .unwrap(),
            DataValue::Integer(7)
        );

        let application = store.application_scope();
        assert_eq!(
            application
                .get(&extension, StorageArea::Document, "selection")
                .unwrap_err()
                .code,
            ExtensionErrorCode::StaleResource
        );
        fs::remove_dir_all(root).unwrap();
    }
}
