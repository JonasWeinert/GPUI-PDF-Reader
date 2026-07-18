//! Durable, app-owned state for user-selected extension packages.
//!
//! This registry records identity, source integrity, enablement, and explicit
//! permission decisions. It deliberately owns no extension runtime, renderer,
//! or UI types. Loading the registry never loads or executes a package.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use key_extension_api::{DataValue, DomainPattern, ExtensionId, NetworkScope, Permission};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub const EXTENSION_REGISTRY_FILE_NAME: &str = "extensions.json";
pub const EXTENSION_REGISTRY_SCHEMA_VERSION: u16 = 1;

const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
const MAX_REGISTRY_ENTRIES: usize = 512;
const MAX_SOURCE_PATH_BYTES: usize = 4096;
const MAX_PERMISSION_DECISIONS: usize = 64;
const MAX_PERMISSION_DOMAINS: usize = 128;
const MAX_SETTINGS_BYTES: usize = 256 * 1024;
const MAX_SETTINGS_NODES: usize = 4_096;
const MAX_SETTINGS_DEPTH: usize = 16;
const SHA256_HEX_BYTES: usize = 64;

/// Resolves the app-owned data root without depending on a process-wide
/// singleton. Tests and portable builds can inject an explicit absolute path
/// with `GPUI_PDF_READER_DATA_DIR`.
pub fn default_app_data_root() -> Result<PathBuf, ExtensionRegistryError> {
    if let Some(override_path) = std::env::var_os("GPUI_PDF_READER_DATA_DIR") {
        let path = PathBuf::from(override_path);
        if !path.is_absolute() {
            return Err(ExtensionRegistryError::InvalidRegistryPath(
                "GPUI_PDF_READER_DATA_DIR must be absolute".into(),
            ));
        }
        return Ok(path);
    }

    #[cfg(target_os = "macos")]
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/GPUI PDF Reader"));
    #[cfg(target_os = "windows")]
    let path = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("GPUI PDF Reader"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let path = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })
        .map(|root| root.join("gpui-pdf-reader"));
    #[cfg(not(any(unix, target_os = "windows")))]
    let path: Option<PathBuf> = None;

    path.filter(|path| path.is_absolute()).ok_or_else(|| {
        ExtensionRegistryError::InvalidRegistryPath(
            "could not resolve an absolute per-user app data directory".into(),
        )
    })
}

/// A persisted decision for a required package permission. There is no
/// `Undecided` state on disk: an absent permission has not been decided.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredPermissionDecision {
    Granted,
    Denied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPermissionDecision {
    pub permission: Permission,
    pub decision: RequiredPermissionDecision,
}

/// Validated state for one user-selected extension package.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionRegistryEntry {
    pub extension: ExtensionId,
    pub source_path: PathBuf,
    pub expected_content_sha256: String,
    pub enabled: bool,
    pub required_permissions: Vec<StoredPermissionDecision>,
    pub settings: Option<DataValue>,
}

/// Untrusted input accepted by [`ExtensionRegistry::upsert`]. The registry
/// canonicalizes `source_path` and validates every other field before writing.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionRegistryEntryInput {
    pub extension: ExtensionId,
    pub source_path: PathBuf,
    pub expected_content_sha256: String,
    pub enabled: bool,
    pub required_permissions: Vec<StoredPermissionDecision>,
    pub settings: Option<DataValue>,
}

#[derive(Debug)]
pub enum ExtensionRegistryError {
    InvalidRegistryPath(String),
    RegistryTooLarge {
        bytes: u64,
        maximum: usize,
    },
    TooManyEntries {
        count: usize,
        maximum: usize,
    },
    UnsupportedSchema(u16),
    DuplicateExtension(ExtensionId),
    InvalidSourcePath(String),
    InvalidContentHash,
    TooManyPermissionDecisions {
        count: usize,
        maximum: usize,
    },
    DuplicatePermission,
    InvalidPermission(String),
    InvalidSettings(String),
    ExtensionNotFound(ExtensionId),
    AtomicRollbackFailed {
        path: PathBuf,
        commit_error: io::Error,
        rollback_error: io::Error,
    },
    Json(serde_json::Error),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ExtensionRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegistryPath(reason) => {
                write!(formatter, "invalid extension registry path: {reason}")
            }
            Self::RegistryTooLarge { bytes, maximum } => write!(
                formatter,
                "extension registry is {bytes} bytes; the maximum is {maximum} bytes"
            ),
            Self::TooManyEntries { count, maximum } => write!(
                formatter,
                "extension registry has {count} entries; the maximum is {maximum}"
            ),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported extension registry schema {version}")
            }
            Self::DuplicateExtension(extension) => {
                write!(
                    formatter,
                    "duplicate extension registry entry for {extension}"
                )
            }
            Self::InvalidSourcePath(reason) => {
                write!(formatter, "invalid extension source path: {reason}")
            }
            Self::InvalidContentHash => formatter
                .write_str("extension content hash must be 64 lowercase hexadecimal characters"),
            Self::TooManyPermissionDecisions { count, maximum } => write!(
                formatter,
                "extension has {count} permission decisions; the maximum is {maximum}"
            ),
            Self::DuplicatePermission => {
                formatter.write_str("extension contains duplicate permission decisions")
            }
            Self::InvalidPermission(reason) => {
                write!(
                    formatter,
                    "invalid persisted extension permission: {reason}"
                )
            }
            Self::InvalidSettings(reason) => {
                write!(formatter, "invalid persisted extension settings: {reason}")
            }
            Self::ExtensionNotFound(extension) => {
                write!(formatter, "extension {extension} is not in the registry")
            }
            Self::AtomicRollbackFailed {
                path,
                commit_error,
                rollback_error,
            } => write!(
                formatter,
                "extension registry commit at {} failed ({commit_error}) and the previous file could not be restored ({rollback_error})",
                path.display()
            ),
            Self::Json(error) => write!(formatter, "invalid extension registry JSON: {error}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} extension registry at {}: {source}",
                path.display()
            ),
        }
    }
}

impl Error for ExtensionRegistryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::AtomicRollbackFailed { commit_error, .. } => Some(commit_error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ExtensionRegistryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// A bounded registry whose filesystem location is injected by the app.
/// Mutating operations write a complete validated snapshot before changing
/// the in-memory state, so a failed write leaves both snapshots untouched.
#[derive(Debug)]
pub struct ExtensionRegistry {
    path: PathBuf,
    entries: BTreeMap<ExtensionId, ExtensionRegistryEntry>,
}

impl ExtensionRegistry {
    /// Loads a registry from an explicit absolute path. A missing file is an
    /// empty registry and is not created until the first mutation.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ExtensionRegistryError> {
        let path = path.into();
        validate_registry_path(&path)?;
        let entries = load_entries(&path)?;
        Ok(Self { path, entries })
    }

    /// Loads the standard registry filename below an injected app data root.
    pub fn load_from_root(root: impl AsRef<Path>) -> Result<Self, ExtensionRegistryError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(ExtensionRegistryError::InvalidRegistryPath(
                "the app data root must be absolute".into(),
            ));
        }
        Self::load(root.join(EXTENSION_REGISTRY_FILE_NAME))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns entries in deterministic extension-ID order.
    #[must_use]
    pub fn list(&self) -> Vec<ExtensionRegistryEntry> {
        self.entries.values().cloned().collect()
    }

    #[must_use]
    pub fn get(&self, extension: &ExtensionId) -> Option<&ExtensionRegistryEntry> {
        self.entries.get(extension)
    }

    /// Inserts or replaces an entry after resolving the selected source to its
    /// canonical absolute path.
    pub fn upsert(
        &mut self,
        input: ExtensionRegistryEntryInput,
    ) -> Result<ExtensionRegistryEntry, ExtensionRegistryError> {
        let entry = validate_input(input)?;
        let mut next = self.entries.clone();
        next.insert(entry.extension.clone(), entry.clone());
        self.commit(next)?;
        Ok(entry)
    }

    pub fn remove(
        &mut self,
        extension: &ExtensionId,
    ) -> Result<Option<ExtensionRegistryEntry>, ExtensionRegistryError> {
        let mut next = self.entries.clone();
        let Some(removed) = next.remove(extension) else {
            return Ok(None);
        };
        self.commit(next)?;
        Ok(Some(removed))
    }

    pub fn set_enabled(
        &mut self,
        extension: &ExtensionId,
        enabled: bool,
    ) -> Result<(), ExtensionRegistryError> {
        let mut next = self.entries.clone();
        let entry = next
            .get_mut(extension)
            .ok_or_else(|| ExtensionRegistryError::ExtensionNotFound(extension.clone()))?;
        if entry.enabled == enabled {
            return Ok(());
        }
        entry.enabled = enabled;
        self.commit(next)
    }

    pub fn set_permission_decision(
        &mut self,
        extension: &ExtensionId,
        permission: Permission,
        decision: RequiredPermissionDecision,
    ) -> Result<(), ExtensionRegistryError> {
        let mut next = self.entries.clone();
        let entry = next
            .get_mut(extension)
            .ok_or_else(|| ExtensionRegistryError::ExtensionNotFound(extension.clone()))?;
        validate_permission(&permission)?;
        if let Some(stored) = entry
            .required_permissions
            .iter_mut()
            .find(|stored| stored.permission == permission)
        {
            stored.decision = decision;
        } else {
            if entry.required_permissions.len() == MAX_PERMISSION_DECISIONS {
                return Err(ExtensionRegistryError::TooManyPermissionDecisions {
                    count: MAX_PERMISSION_DECISIONS + 1,
                    maximum: MAX_PERMISSION_DECISIONS,
                });
            }
            entry.required_permissions.push(StoredPermissionDecision {
                permission,
                decision,
            });
        }
        sort_permission_decisions(&mut entry.required_permissions)?;
        self.commit(next)
    }

    pub fn replace_permission_decisions(
        &mut self,
        extension: &ExtensionId,
        required_permissions: Vec<StoredPermissionDecision>,
    ) -> Result<(), ExtensionRegistryError> {
        let required_permissions = validate_permission_decisions(required_permissions)?;
        let mut next = self.entries.clone();
        let entry = next
            .get_mut(extension)
            .ok_or_else(|| ExtensionRegistryError::ExtensionNotFound(extension.clone()))?;
        entry.required_permissions = required_permissions;
        self.commit(next)
    }

    pub fn replace_settings(
        &mut self,
        extension: &ExtensionId,
        settings: Option<DataValue>,
    ) -> Result<(), ExtensionRegistryError> {
        let settings = validate_settings(settings)?;
        let mut next = self.entries.clone();
        let entry = next
            .get_mut(extension)
            .ok_or_else(|| ExtensionRegistryError::ExtensionNotFound(extension.clone()))?;
        entry.settings = settings;
        self.commit(next)
    }

    /// Removes a stored decision, returning the permission to the host's
    /// undecided state. This does not grant or deny anything implicitly.
    pub fn clear_permission_decision(
        &mut self,
        extension: &ExtensionId,
        permission: &Permission,
    ) -> Result<bool, ExtensionRegistryError> {
        let mut next = self.entries.clone();
        let entry = next
            .get_mut(extension)
            .ok_or_else(|| ExtensionRegistryError::ExtensionNotFound(extension.clone()))?;
        let previous_len = entry.required_permissions.len();
        entry
            .required_permissions
            .retain(|stored| &stored.permission != permission);
        if entry.required_permissions.len() == previous_len {
            return Ok(false);
        }
        self.commit(next)?;
        Ok(true)
    }

    fn commit(
        &mut self,
        entries: BTreeMap<ExtensionId, ExtensionRegistryEntry>,
    ) -> Result<(), ExtensionRegistryError> {
        persist_entries(&self.path, &entries)?;
        self.entries = entries;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u16,
    entries: Vec<RegistryEntryWire>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntryWire {
    extension: ExtensionId,
    source_path: String,
    expected_content_sha256: String,
    enabled: bool,
    required_permissions: Vec<StoredPermissionDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<DataValue>,
}

fn load_entries(
    path: &Path,
) -> Result<BTreeMap<ExtensionId, ExtensionRegistryEntry>, ExtensionRegistryError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => return Err(io_error("open", path, source)),
    };
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect", path, source))?;
    if metadata.len() > MAX_REGISTRY_BYTES as u64 {
        return Err(ExtensionRegistryError::RegistryTooLarge {
            bytes: metadata.len(),
            maximum: MAX_REGISTRY_BYTES,
        });
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_REGISTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read", path, source))?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(ExtensionRegistryError::RegistryTooLarge {
            bytes: bytes.len() as u64,
            maximum: MAX_REGISTRY_BYTES,
        });
    }

    let document: RegistryDocument = serde_json::from_slice(&bytes)?;
    if document.schema_version != EXTENSION_REGISTRY_SCHEMA_VERSION {
        return Err(ExtensionRegistryError::UnsupportedSchema(
            document.schema_version,
        ));
    }
    if document.entries.len() > MAX_REGISTRY_ENTRIES {
        return Err(ExtensionRegistryError::TooManyEntries {
            count: document.entries.len(),
            maximum: MAX_REGISTRY_ENTRIES,
        });
    }

    let mut entries = BTreeMap::new();
    for wire in document.entries {
        let entry = validate_wire(wire)?;
        let extension = entry.extension.clone();
        if entries.insert(extension.clone(), entry).is_some() {
            return Err(ExtensionRegistryError::DuplicateExtension(extension));
        }
    }
    Ok(entries)
}

fn validate_input(
    input: ExtensionRegistryEntryInput,
) -> Result<ExtensionRegistryEntry, ExtensionRegistryError> {
    validate_content_hash(&input.expected_content_sha256)?;
    let source_path = fs::canonicalize(&input.source_path)
        .map_err(|source| io_error("canonicalize package source", &input.source_path, source))?;
    validate_stored_source_path(&source_path)?;
    let required_permissions = validate_permission_decisions(input.required_permissions)?;
    let settings = validate_settings(input.settings)?;
    Ok(ExtensionRegistryEntry {
        extension: input.extension,
        source_path,
        expected_content_sha256: input.expected_content_sha256,
        enabled: input.enabled,
        required_permissions,
        settings,
    })
}

fn validate_wire(
    wire: RegistryEntryWire,
) -> Result<ExtensionRegistryEntry, ExtensionRegistryError> {
    validate_content_hash(&wire.expected_content_sha256)?;
    let source_path = PathBuf::from(wire.source_path);
    validate_stored_source_path(&source_path)?;
    let required_permissions = validate_permission_decisions(wire.required_permissions)?;
    let settings = validate_settings(wire.settings)?;
    Ok(ExtensionRegistryEntry {
        extension: wire.extension,
        source_path,
        expected_content_sha256: wire.expected_content_sha256,
        enabled: wire.enabled,
        required_permissions,
        settings,
    })
}

fn validate_registry_path(path: &Path) -> Result<(), ExtensionRegistryError> {
    if !path.is_absolute() {
        return Err(ExtensionRegistryError::InvalidRegistryPath(
            "the registry path must be absolute".into(),
        ));
    }
    if path.file_name().is_none() {
        return Err(ExtensionRegistryError::InvalidRegistryPath(
            "the registry path must name a file".into(),
        ));
    }
    let Some(path_text) = path.to_str() else {
        return Err(ExtensionRegistryError::InvalidRegistryPath(
            "the registry path must be valid UTF-8".into(),
        ));
    };
    if path_text.len() > MAX_SOURCE_PATH_BYTES {
        return Err(ExtensionRegistryError::InvalidRegistryPath(
            "the registry path is too long".into(),
        ));
    }
    Ok(())
}

fn validate_stored_source_path(path: &Path) -> Result<(), ExtensionRegistryError> {
    if !path.is_absolute() {
        return Err(ExtensionRegistryError::InvalidSourcePath(
            "path must be absolute".into(),
        ));
    }
    let Some(path_text) = path.to_str() else {
        return Err(ExtensionRegistryError::InvalidSourcePath(
            "path must be valid UTF-8".into(),
        ));
    };
    if path_text.len() > MAX_SOURCE_PATH_BYTES {
        return Err(ExtensionRegistryError::InvalidSourcePath(
            "path is too long".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ExtensionRegistryError::InvalidSourcePath(
            "path must not contain '.' or '..' components".into(),
        ));
    }
    Ok(())
}

fn validate_content_hash(hash: &str) -> Result<(), ExtensionRegistryError> {
    if hash.len() != SHA256_HEX_BYTES
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ExtensionRegistryError::InvalidContentHash);
    }
    Ok(())
}

fn validate_permission_decisions(
    mut decisions: Vec<StoredPermissionDecision>,
) -> Result<Vec<StoredPermissionDecision>, ExtensionRegistryError> {
    if decisions.len() > MAX_PERMISSION_DECISIONS {
        return Err(ExtensionRegistryError::TooManyPermissionDecisions {
            count: decisions.len(),
            maximum: MAX_PERMISSION_DECISIONS,
        });
    }
    for (index, stored) in decisions.iter().enumerate() {
        validate_permission(&stored.permission)?;
        if decisions[..index]
            .iter()
            .any(|previous| previous.permission == stored.permission)
        {
            return Err(ExtensionRegistryError::DuplicatePermission);
        }
    }
    sort_permission_decisions(&mut decisions)?;
    Ok(decisions)
}

fn validate_settings(
    settings: Option<DataValue>,
) -> Result<Option<DataValue>, ExtensionRegistryError> {
    let Some(settings) = settings else {
        return Ok(None);
    };
    let encoded = serde_json::to_vec(&settings)?;
    if encoded.len() > MAX_SETTINGS_BYTES {
        return Err(ExtensionRegistryError::InvalidSettings(format!(
            "settings use {} encoded bytes; the maximum is {MAX_SETTINGS_BYTES}",
            encoded.len()
        )));
    }

    fn visit(
        value: &DataValue,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<(), ExtensionRegistryError> {
        if depth > MAX_SETTINGS_DEPTH {
            return Err(ExtensionRegistryError::InvalidSettings(
                "settings nesting is too deep".into(),
            ));
        }
        *nodes = nodes.saturating_add(1);
        if *nodes > MAX_SETTINGS_NODES {
            return Err(ExtensionRegistryError::InvalidSettings(
                "settings contain too many values".into(),
            ));
        }
        match value {
            DataValue::String(value) if value.len() > MAX_SETTINGS_BYTES => {
                return Err(ExtensionRegistryError::InvalidSettings(
                    "a settings string is too large".into(),
                ));
            }
            DataValue::List(values) => {
                for value in values {
                    visit(value, depth + 1, nodes)?;
                }
            }
            DataValue::Record(values) => {
                for (key, value) in values {
                    if key.is_empty()
                        || key.len() > 128
                        || !key
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    {
                        return Err(ExtensionRegistryError::InvalidSettings(
                            "settings contain an invalid record key".into(),
                        ));
                    }
                    visit(value, depth + 1, nodes)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    let mut nodes = 0;
    visit(&settings, 0, &mut nodes)?;
    if !matches!(settings, DataValue::Record(_)) {
        return Err(ExtensionRegistryError::InvalidSettings(
            "settings root must be a record".into(),
        ));
    }
    Ok(Some(settings))
}

fn validate_permission(permission: &Permission) -> Result<(), ExtensionRegistryError> {
    let domains = match permission {
        Permission::Network(NetworkScope::DeclaredDomains(domains))
        | Permission::Network(NetworkScope::DeclaredAndUserApproved(domains)) => Some(domains),
        _ => None,
    };
    let Some(domains) = domains else {
        return Ok(());
    };
    if domains.len() > MAX_PERMISSION_DOMAINS {
        return Err(ExtensionRegistryError::InvalidPermission(format!(
            "network scope has {} domains; the maximum is {MAX_PERMISSION_DOMAINS}",
            domains.len()
        )));
    }
    for (index, domain) in domains.iter().enumerate() {
        if !domain.is_canonical() {
            return Err(ExtensionRegistryError::InvalidPermission(format!(
                "network domain '{}' is not canonical",
                domain.0
            )));
        }
        if domains[..index]
            .iter()
            .any(|previous: &DomainPattern| previous == domain)
        {
            return Err(ExtensionRegistryError::InvalidPermission(format!(
                "network domain '{}' is duplicated",
                domain.0
            )));
        }
    }
    Ok(())
}

fn sort_permission_decisions(
    decisions: &mut [StoredPermissionDecision],
) -> Result<(), ExtensionRegistryError> {
    let mut keyed = decisions
        .iter()
        .map(|decision| {
            serde_json::to_string(&decision.permission)
                .map(|key| (key, decision.clone()))
                .map_err(ExtensionRegistryError::Json)
        })
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    for (target, (_, decision)) in decisions.iter_mut().zip(keyed) {
        *target = decision;
    }
    Ok(())
}

fn persist_entries(
    path: &Path,
    entries: &BTreeMap<ExtensionId, ExtensionRegistryEntry>,
) -> Result<(), ExtensionRegistryError> {
    if entries.len() > MAX_REGISTRY_ENTRIES {
        return Err(ExtensionRegistryError::TooManyEntries {
            count: entries.len(),
            maximum: MAX_REGISTRY_ENTRIES,
        });
    }
    let document = RegistryDocument {
        schema_version: EXTENSION_REGISTRY_SCHEMA_VERSION,
        entries: entries
            .values()
            .map(|entry| RegistryEntryWire {
                extension: entry.extension.clone(),
                source_path: entry.source_path.to_string_lossy().into_owned(),
                expected_content_sha256: entry.expected_content_sha256.clone(),
                enabled: entry.enabled,
                required_permissions: entry.required_permissions.clone(),
                settings: entry.settings.clone(),
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(ExtensionRegistryError::RegistryTooLarge {
            bytes: bytes.len() as u64,
            maximum: MAX_REGISTRY_BYTES,
        });
    }
    atomic_write_with_hooks(path, &bytes, |_| Ok(()), sync_directory)
}

#[cfg(test)]
fn atomic_write_with_precommit(
    path: &Path,
    bytes: &[u8],
    before_commit: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), ExtensionRegistryError> {
    atomic_write_with_hooks(path, bytes, before_commit, sync_directory)
}

fn atomic_write_with_hooks(
    path: &Path,
    bytes: &[u8],
    before_commit: impl FnOnce(&Path) -> io::Result<()>,
    after_commit: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), ExtensionRegistryError> {
    let parent = path.parent().ok_or_else(|| {
        ExtensionRegistryError::InvalidRegistryPath("registry has no parent directory".into())
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create parent directory for", path, source))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|source| io_error("create temporary", path, source))?;
    temporary
        .write_all(bytes)
        .map_err(|source| io_error("write temporary", path, source))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| io_error("synchronize temporary", path, source))?;
    before_commit(temporary.path())
        .map_err(|source| io_error("prepare atomic replacement of", path, source))?;

    let previous = backup_previous_file(path, parent)?;
    temporary
        .persist(path)
        .map_err(|error| io_error("atomically replace", path, error.error))?;
    if let Err(commit_error) = after_commit(parent) {
        if let Err(rollback_error) = restore_previous_file(path, parent, previous) {
            return Err(ExtensionRegistryError::AtomicRollbackFailed {
                path: path.to_path_buf(),
                commit_error,
                rollback_error,
            });
        }
        return Err(io_error(
            "synchronize parent directory for",
            path,
            commit_error,
        ));
    }
    Ok(())
}

fn backup_previous_file(
    path: &Path,
    parent: &Path,
) -> Result<Option<NamedTempFile>, ExtensionRegistryError> {
    let mut source = match File::open(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("open previous", path, source)),
    };
    let length = source
        .metadata()
        .map_err(|error| io_error("inspect previous", path, error))?
        .len();
    if length > MAX_REGISTRY_BYTES as u64 {
        return Err(ExtensionRegistryError::RegistryTooLarge {
            bytes: length,
            maximum: MAX_REGISTRY_BYTES,
        });
    }
    let mut backup = NamedTempFile::new_in(parent)
        .map_err(|source| io_error("create rollback backup for", path, source))?;
    io::copy(&mut source, &mut backup)
        .map_err(|source| io_error("copy rollback backup for", path, source))?;
    backup
        .as_file_mut()
        .sync_all()
        .map_err(|source| io_error("synchronize rollback backup for", path, source))?;
    Ok(Some(backup))
}

fn restore_previous_file(
    path: &Path,
    parent: &Path,
    previous: Option<NamedTempFile>,
) -> io::Result<()> {
    if let Some(previous) = previous {
        previous.persist(path).map_err(|error| error.error)?;
    } else {
        fs::remove_file(path)?;
    }
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ExtensionRegistryError {
    ExtensionRegistryError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn extension(value: &str) -> ExtensionId {
        ExtensionId::parse(value).expect("valid test extension ID")
    }

    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let directory = TempDir::new().expect("temporary directory");
        let source = directory.path().join("package");
        fs::create_dir(&source).expect("package source");
        let registry_path = directory.path().join("state").join("extensions.json");
        (directory, source, registry_path)
    }

    fn input(id: &str, source_path: &Path) -> ExtensionRegistryEntryInput {
        ExtensionRegistryEntryInput {
            extension: extension(id),
            source_path: source_path.to_path_buf(),
            expected_content_sha256: HASH_A.into(),
            enabled: true,
            required_permissions: vec![StoredPermissionDecision {
                permission: Permission::ReadDocumentMetadata,
                decision: RequiredPermissionDecision::Granted,
            }],
            settings: None,
        }
    }

    #[test]
    fn round_trip_is_canonical_bounded_and_deterministically_ordered() {
        let (_directory, source, path) = setup();
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input("org.example.z-last", &source))
            .expect("insert last");
        let mut first = input("org.example.a-first", &source.join("..").join("package"));
        first.expected_content_sha256 = HASH_B.into();
        first.required_permissions = vec![
            StoredPermissionDecision {
                permission: Permission::OpenExternalUrl,
                decision: RequiredPermissionDecision::Denied,
            },
            StoredPermissionDecision {
                permission: Permission::ReadDocumentMetadata,
                decision: RequiredPermissionDecision::Granted,
            },
        ];
        first.settings = Some(DataValue::Record(BTreeMap::from([(
            "theme-preset".into(),
            DataValue::String("graphite".into()),
        )])));
        let inserted = registry.upsert(first).expect("insert first");
        assert!(inserted.source_path.is_absolute());
        assert!(!inserted.source_path.to_string_lossy().contains(".."));

        let text = fs::read_to_string(&path).expect("registry JSON");
        assert!(
            text.find("org.example.a-first").expect("first ID")
                < text.find("org.example.z-last").expect("last ID")
        );
        let loaded = ExtensionRegistry::load(&path).expect("reload registry");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.list()[0], inserted);
    }

    #[test]
    fn corrupt_oversized_duplicate_and_noncanonical_state_is_rejected() {
        let (directory, source, path) = setup();
        fs::create_dir_all(path.parent().expect("parent")).expect("state directory");
        fs::write(&path, b"{ definitely not JSON").expect("corrupt registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::Json(_))
        ));

        fs::write(&path, vec![b'x'; MAX_REGISTRY_BYTES + 1]).expect("oversized registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::RegistryTooLarge { .. })
        ));

        let canonical = fs::canonicalize(&source).expect("canonical source");
        let duplicate = format!(
            r#"{{"schema_version":1,"entries":[{{"extension":"org.example.same","source_path":{},"expected_content_sha256":"{HASH_A}","enabled":true,"required_permissions":[]}},{{"extension":"org.example.same","source_path":{},"expected_content_sha256":"{HASH_B}","enabled":false,"required_permissions":[]}}]}}"#,
            serde_json::to_string(&canonical.to_string_lossy()).expect("path JSON"),
            serde_json::to_string(&canonical.to_string_lossy()).expect("path JSON")
        );
        fs::write(&path, duplicate).expect("duplicate registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::DuplicateExtension(id)) if id == extension("org.example.same")
        ));

        let noncanonical = format!(
            r#"{{"schema_version":1,"entries":[{{"extension":"org.example.bad-path","source_path":"/tmp/../escape","expected_content_sha256":"{HASH_A}","enabled":true,"required_permissions":[]}}]}}"#
        );
        fs::write(&path, noncanonical).expect("noncanonical registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::InvalidSourcePath(_))
        ));

        let duplicate_permission = format!(
            r#"{{"schema_version":1,"entries":[{{"extension":"org.example.duplicate-permission","source_path":{},"expected_content_sha256":"{HASH_A}","enabled":true,"required_permissions":[{{"permission":{{"kind":"read_document_metadata"}},"decision":"granted"}},{{"permission":{{"kind":"read_document_metadata"}},"decision":"denied"}}]}}]}}"#,
            serde_json::to_string(&canonical.to_string_lossy()).expect("path JSON")
        );
        fs::write(&path, duplicate_permission).expect("duplicate permissions");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::DuplicatePermission)
        ));

        drop(directory);
    }

    #[test]
    fn failed_precommit_preserves_the_previous_valid_file() {
        let (_directory, source, path) = setup();
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input("org.example.stable", &source))
            .expect("initial write");
        let previous = fs::read(&path).expect("previous registry");

        let error = atomic_write_with_precommit(&path, b"replacement", |_| {
            Err(io::Error::other("injected precommit failure"))
        })
        .expect_err("injected failure");
        assert!(matches!(error, ExtensionRegistryError::Io { .. }));
        assert_eq!(fs::read(&path).expect("preserved registry"), previous);
        assert_eq!(
            ExtensionRegistry::load(&path)
                .expect("previous state remains valid")
                .len(),
            1
        );

        let error = atomic_write_with_hooks(
            &path,
            b"replacement after rename",
            |_| Ok(()),
            |_| Err(io::Error::other("injected postcommit failure")),
        )
        .expect_err("injected postcommit failure");
        assert!(matches!(error, ExtensionRegistryError::Io { .. }));
        assert_eq!(fs::read(&path).expect("rolled-back registry"), previous);
        assert_eq!(
            ExtensionRegistry::load(&path)
                .expect("rolled-back state remains valid")
                .len(),
            1
        );
    }

    #[test]
    fn permission_decisions_are_explicit_replaceable_and_unique() {
        let (_directory, source, path) = setup();
        let id = extension("org.example.permissions");
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input(id.as_str(), &source))
            .expect("insert extension");
        registry
            .set_permission_decision(
                &id,
                Permission::ReadDocumentMetadata,
                RequiredPermissionDecision::Denied,
            )
            .expect("change decision");
        registry
            .set_permission_decision(
                &id,
                Permission::OpenExternalUrl,
                RequiredPermissionDecision::Granted,
            )
            .expect("add decision");
        let stored = registry.get(&id).expect("entry");
        assert_eq!(stored.required_permissions.len(), 2);
        assert!(stored.required_permissions.iter().any(|decision| {
            decision.permission == Permission::ReadDocumentMetadata
                && decision.decision == RequiredPermissionDecision::Denied
        }));

        registry
            .replace_permission_decisions(
                &id,
                vec![StoredPermissionDecision {
                    permission: Permission::ReadSelection,
                    decision: RequiredPermissionDecision::Granted,
                }],
            )
            .expect("replace decisions");
        assert_eq!(
            registry.get(&id).expect("entry").required_permissions.len(),
            1
        );

        assert!(
            registry
                .clear_permission_decision(&id, &Permission::ReadSelection)
                .expect("clear decision")
        );
        assert!(
            !registry
                .clear_permission_decision(&id, &Permission::ReadSelection)
                .expect("already absent")
        );
        assert!(
            registry
                .get(&id)
                .expect("entry")
                .required_permissions
                .is_empty()
        );

        let duplicate = vec![
            StoredPermissionDecision {
                permission: Permission::ReadSelection,
                decision: RequiredPermissionDecision::Granted,
            },
            StoredPermissionDecision {
                permission: Permission::ReadSelection,
                decision: RequiredPermissionDecision::Denied,
            },
        ];
        assert!(matches!(
            registry.replace_permission_decisions(&id, duplicate),
            Err(ExtensionRegistryError::DuplicatePermission)
        ));
        assert_eq!(
            registry
                .get(&id)
                .expect("unchanged entry")
                .required_permissions
                .len(),
            0
        );
    }

    #[test]
    fn settings_are_bounded_durable_and_transactional() {
        let (_directory, source, path) = setup();
        let id = extension("org.example.settings");
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input(id.as_str(), &source))
            .expect("insert extension");
        let settings = DataValue::Record(BTreeMap::from([(
            "appearance".into(),
            DataValue::Record(BTreeMap::from([(
                "preset".into(),
                DataValue::String("graphite".into()),
            )])),
        )]));
        registry
            .replace_settings(&id, Some(settings.clone()))
            .expect("persist settings");
        assert_eq!(
            ExtensionRegistry::load(&path)
                .expect("reload")
                .get(&id)
                .and_then(|entry| entry.settings.clone()),
            Some(settings)
        );

        let invalid = DataValue::Record(BTreeMap::from([(
            "invalid.key".into(),
            DataValue::Boolean(true),
        )]));
        assert!(matches!(
            registry.replace_settings(&id, Some(invalid)),
            Err(ExtensionRegistryError::InvalidSettings(_))
        ));
        assert!(
            registry
                .get(&id)
                .is_some_and(|entry| entry.settings.is_some())
        );
    }

    #[test]
    fn entry_path_and_permission_count_limits_are_enforced() {
        let (_directory, source, path) = setup();
        fs::create_dir_all(path.parent().expect("parent")).expect("state directory");
        let long_path = format!("/{}", "p".repeat(MAX_SOURCE_PATH_BYTES));
        let document = format!(
            r#"{{"schema_version":1,"entries":[{{"extension":"org.example.long-path","source_path":{},"expected_content_sha256":"{HASH_A}","enabled":true,"required_permissions":[]}}]}}"#,
            serde_json::to_string(&long_path).expect("path JSON")
        );
        fs::write(&path, document).expect("long path registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::InvalidSourcePath(_))
        ));

        let mut registry =
            ExtensionRegistry::load(path.with_file_name("bounded.json")).expect("empty registry");
        let mut too_many = input("org.example.too-many-permissions", &source);
        too_many.required_permissions = (0..=MAX_PERMISSION_DECISIONS)
            .map(|_| StoredPermissionDecision {
                permission: Permission::ReadSelection,
                decision: RequiredPermissionDecision::Granted,
            })
            .collect();
        assert!(matches!(
            registry.upsert(too_many),
            Err(ExtensionRegistryError::TooManyPermissionDecisions { .. })
        ));
    }

    #[test]
    fn entry_count_limit_is_checked_before_materializing_registry_state() {
        let (_directory, source, path) = setup();
        fs::create_dir_all(path.parent().expect("parent")).expect("state directory");
        let canonical = fs::canonicalize(source).expect("canonical source");
        let source_json = serde_json::to_string(&canonical.to_string_lossy()).expect("path JSON");
        let entries = (0..=MAX_REGISTRY_ENTRIES)
            .map(|index| {
                format!(
                    r#"{{"extension":"org.example.item-{index}","source_path":{source_json},"expected_content_sha256":"{HASH_A}","enabled":true,"required_permissions":[]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            &path,
            format!(
                r#"{{"schema_version":{EXTENSION_REGISTRY_SCHEMA_VERSION},"entries":[{entries}]}}"#
            ),
        )
        .expect("entry-heavy registry");
        assert!(matches!(
            ExtensionRegistry::load(&path),
            Err(ExtensionRegistryError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn enablement_and_removal_are_durable_and_missing_ids_are_safe() {
        let (_directory, source, path) = setup();
        let id = extension("org.example.removable");
        let missing = extension("org.example.missing");
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input(id.as_str(), &source))
            .expect("insert extension");
        registry.set_enabled(&id, false).expect("disable");
        assert!(!registry.get(&id).expect("disabled entry").enabled);
        assert!(matches!(
            registry.set_enabled(&missing, true),
            Err(ExtensionRegistryError::ExtensionNotFound(value)) if value == missing
        ));
        assert!(registry.remove(&missing).expect("remove missing").is_none());
        assert_eq!(
            registry
                .remove(&id)
                .expect("remove existing")
                .expect("entry")
                .extension,
            id
        );
        assert!(
            ExtensionRegistry::load(&path)
                .expect("reload empty registry")
                .is_empty()
        );
    }

    #[test]
    fn malformed_hash_and_permission_scope_do_not_replace_valid_state() {
        let (_directory, source, path) = setup();
        let id = extension("org.example.validation");
        let mut registry = ExtensionRegistry::load(&path).expect("empty registry");
        registry
            .upsert(input(id.as_str(), &source))
            .expect("insert extension");
        let previous = fs::read(&path).expect("valid state");

        let mut bad_hash = input(id.as_str(), &source);
        bad_hash.expected_content_sha256 = "ABC".into();
        assert!(matches!(
            registry.upsert(bad_hash),
            Err(ExtensionRegistryError::InvalidContentHash)
        ));

        let invalid_domain =
            Permission::Network(NetworkScope::DeclaredDomains(vec![DomainPattern(
                "HTTPS://example.org".into(),
            )]));
        assert!(matches!(
            registry.set_permission_decision(
                &id,
                invalid_domain,
                RequiredPermissionDecision::Granted
            ),
            Err(ExtensionRegistryError::InvalidPermission(_))
        ));
        assert_eq!(fs::read(&path).expect("unchanged valid state"), previous);
    }
}
