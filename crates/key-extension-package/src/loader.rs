use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    fs::{self, File},
    io::{Cursor, Read},
    path::{Component, Path},
};

use key_extension_api::{
    ExtensionEntrypoint, ExtensionManifest, PackagePath, ValidationError, ValidationLimits,
};
use zip::{CompressionMethod, ZipArchive};

use crate::{
    LoadedPackage, PackageBlob, PackageSourceKind, Sha256Digest, SignatureMetadata,
    SignatureMetadataError, SignatureVerificationError, SignatureVerifier,
    VerifiedPackageSignature, canonical_content_hash,
};

const MANIFEST_PATH: &str = "manifest.toml";
const SIGNATURE_PATH: &str = "signature.toml";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLimits {
    pub maximum_archive_bytes: u64,
    pub maximum_entries: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_uncompressed_bytes: u64,
    pub maximum_compression_ratio: u64,
    pub maximum_manifest_bytes: u64,
    pub maximum_ui_bytes: u64,
    pub maximum_component_bytes: u64,
    pub maximum_assets: usize,
    pub maximum_asset_bytes: u64,
    pub maximum_signature_bytes: u64,
    pub manifest_validation: ValidationLimits,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            maximum_archive_bytes: 64 * 1024 * 1024,
            maximum_entries: 512,
            maximum_file_bytes: 32 * 1024 * 1024,
            maximum_total_uncompressed_bytes: 96 * 1024 * 1024,
            maximum_compression_ratio: 200,
            maximum_manifest_bytes: 1024 * 1024,
            maximum_ui_bytes: 4 * 1024 * 1024,
            maximum_component_bytes: 32 * 1024 * 1024,
            maximum_assets: 256,
            maximum_asset_bytes: 16 * 1024 * 1024,
            maximum_signature_bytes: 32 * 1024,
            manifest_validation: ValidationLimits::default(),
        }
    }
}

/// Signature requirements are deny-by-default for both package sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignaturePolicy {
    pub require_signed_archives: bool,
    pub allow_unsigned_development_directories: bool,
}

impl Default for SignaturePolicy {
    fn default() -> Self {
        Self {
            require_signed_archives: true,
            allow_unsigned_development_directories: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageError {
    Io {
        path: String,
        message: String,
    },
    SourceIsSymlink(String),
    SourceType(String),
    ArchiveTooLarge {
        actual: u64,
        maximum: u64,
    },
    InvalidArchive(String),
    EntryLimit {
        actual: usize,
        maximum: usize,
    },
    UnsafePath(String),
    DuplicatePath(String),
    SymlinkEntry(String),
    EncryptedEntry(String),
    UnsupportedCompression(String),
    FileTooLarge {
        path: String,
        actual: u64,
        maximum: u64,
    },
    TotalSizeLimit {
        actual: u64,
        maximum: u64,
    },
    CompressionRatio {
        path: String,
    },
    TruncatedEntry(String),
    MissingManifest,
    InvalidManifest(String),
    ManifestValidation(Vec<ValidationError>),
    UnexpectedFile(String),
    MissingEntrypointFile(String),
    InvalidUi(String),
    InvalidComponent(String),
    AssetLimit {
        actual: usize,
        maximum: usize,
    },
    InvalidAsset {
        path: String,
        reason: String,
    },
    SignatureRequired,
    InvalidSignatureMetadata(SignatureMetadataError),
    SignatureHashMismatch,
    SignatureRejected(SignatureVerificationError),
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(formatter, "{path}: {message}"),
            Self::SourceIsSymlink(path) => write!(formatter, "package source is a symlink: {path}"),
            Self::SourceType(path) => write!(formatter, "unsupported package source type: {path}"),
            Self::ArchiveTooLarge { actual, maximum } => {
                write!(formatter, "archive has {actual} bytes; limit is {maximum}")
            }
            Self::InvalidArchive(message) => write!(formatter, "invalid ZIP archive: {message}"),
            Self::EntryLimit { actual, maximum } => {
                write!(
                    formatter,
                    "package has {actual} entries; limit is {maximum}"
                )
            }
            Self::UnsafePath(path) => write!(formatter, "unsafe package path: {path}"),
            Self::DuplicatePath(path) => write!(formatter, "duplicate package path: {path}"),
            Self::SymlinkEntry(path) => write!(formatter, "symlinks are forbidden: {path}"),
            Self::EncryptedEntry(path) => write!(formatter, "encrypted entry is forbidden: {path}"),
            Self::UnsupportedCompression(path) => {
                write!(formatter, "unsupported compression for entry: {path}")
            }
            Self::FileTooLarge {
                path,
                actual,
                maximum,
            } => write!(formatter, "{path} has {actual} bytes; limit is {maximum}"),
            Self::TotalSizeLimit { actual, maximum } => write!(
                formatter,
                "package expands to {actual} bytes; limit is {maximum}"
            ),
            Self::CompressionRatio { path } => {
                write!(formatter, "entry exceeds compression ratio limit: {path}")
            }
            Self::TruncatedEntry(path) => write!(formatter, "entry size mismatch: {path}"),
            Self::MissingManifest => formatter.write_str("package has no manifest.toml"),
            Self::InvalidManifest(message) => write!(formatter, "invalid manifest: {message}"),
            Self::ManifestValidation(errors) => {
                write!(
                    formatter,
                    "manifest has {} validation error(s)",
                    errors.len()
                )
            }
            Self::UnexpectedFile(path) => write!(formatter, "undeclared package file: {path}"),
            Self::MissingEntrypointFile(path) => {
                write!(formatter, "declared entrypoint file is missing: {path}")
            }
            Self::InvalidUi(message) => write!(formatter, "invalid UI document: {message}"),
            Self::InvalidComponent(message) => {
                write!(formatter, "invalid WebAssembly component: {message}")
            }
            Self::AssetLimit { actual, maximum } => {
                write!(formatter, "package has {actual} assets; limit is {maximum}")
            }
            Self::InvalidAsset { path, reason } => {
                write!(formatter, "invalid asset {path}: {reason}")
            }
            Self::SignatureRequired => {
                formatter.write_str("a verified package signature is required")
            }
            Self::InvalidSignatureMetadata(error) => error.fmt(formatter),
            Self::SignatureHashMismatch => {
                formatter.write_str("signature metadata names a different content hash")
            }
            Self::SignatureRejected(error) => error.fmt(formatter),
        }
    }
}

impl Error for PackageError {}

#[derive(Clone, Debug, Default)]
pub struct PackageLoader {
    limits: PackageLimits,
    signature_policy: SignaturePolicy,
}

impl PackageLoader {
    #[must_use]
    pub const fn new(limits: PackageLimits, signature_policy: SignaturePolicy) -> Self {
        Self {
            limits,
            signature_policy,
        }
    }

    #[must_use]
    pub const fn limits(&self) -> &PackageLimits {
        &self.limits
    }

    #[must_use]
    pub const fn signature_policy(&self) -> SignaturePolicy {
        self.signature_policy
    }

    /// Load a `.keyext` ZIP into an immutable in-memory package.
    ///
    /// # Errors
    ///
    /// Rejects unsafe paths and file types, encrypted or unsupported entries,
    /// violated resource limits, invalid contracts/content, and signatures
    /// rejected by policy or the supplied verifier.
    pub fn load_keyext(
        &self,
        path: impl AsRef<Path>,
        verifier: &dyn SignatureVerifier,
    ) -> Result<LoadedPackage, PackageError> {
        let path = path.as_ref();
        let metadata = symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::SourceIsSymlink(display_path(path)));
        }
        if !metadata.is_file() {
            return Err(PackageError::SourceType(display_path(path)));
        }
        if metadata.len() > self.limits.maximum_archive_bytes {
            return Err(PackageError::ArchiveTooLarge {
                actual: metadata.len(),
                maximum: self.limits.maximum_archive_bytes,
            });
        }
        let archive_bytes = read_file_bounded(path, self.limits.maximum_archive_bytes)?;
        let archive_sha256 = Sha256Digest::of(&archive_bytes);
        let files = self.read_zip(&archive_bytes)?;
        self.build_loaded(
            files,
            PackageSourceKind::KeyextArchive,
            Some(archive_sha256),
            verifier,
        )
    }

    /// Snapshot an unpacked development directory without following symlinks.
    ///
    /// # Errors
    ///
    /// Rejects non-directories, all symlinks and special files, unsafe names,
    /// resource-limit violations, invalid contracts/content, and signatures
    /// rejected by policy or the supplied verifier.
    pub fn load_development_directory(
        &self,
        path: impl AsRef<Path>,
        verifier: &dyn SignatureVerifier,
    ) -> Result<LoadedPackage, PackageError> {
        let root = path.as_ref();
        let metadata = symlink_metadata(root)?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::SourceIsSymlink(display_path(root)));
        }
        if !metadata.is_dir() {
            return Err(PackageError::SourceType(display_path(root)));
        }
        let files = self.read_directory(root)?;
        self.build_loaded(
            files,
            PackageSourceKind::DevelopmentDirectory,
            None,
            verifier,
        )
    }

    fn read_zip(&self, bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
        let mut archive = ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
        // zip's public archive index is keyed by file name, so duplicate central
        // directory names are collapsed before `by_index` can observe them. Audit
        // the bounded central directory itself first so an attacker cannot smuggle
        // last-entry-wins ambiguity into a package.
        audit_central_directory(
            bytes,
            archive.central_directory_start(),
            self.limits.maximum_entries,
        )?;
        if archive.len() > self.limits.maximum_entries {
            return Err(PackageError::EntryLimit {
                actual: archive.len(),
                maximum: self.limits.maximum_entries,
            });
        }
        let mut files = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut total = 0_u64;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
            let raw_name = std::str::from_utf8(entry.name_raw())
                .map_err(|_| PackageError::UnsafePath("<non-UTF-8>".into()))?;
            let (path, directory) = canonical_archive_path(raw_name, entry.is_dir())?;
            if !seen.insert(path.clone()) {
                return Err(PackageError::DuplicatePath(path));
            }
            if entry.is_symlink() || (!directory && !entry.is_file()) {
                return Err(PackageError::SymlinkEntry(path));
            }
            if directory {
                continue;
            }
            if entry.encrypted() {
                return Err(PackageError::EncryptedEntry(path));
            }
            if !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ) {
                return Err(PackageError::UnsupportedCompression(path));
            }
            let declared = entry.size();
            self.check_file_size(&path, declared, self.limits.maximum_file_bytes)?;
            total = checked_total(
                total,
                declared,
                self.limits.maximum_total_uncompressed_bytes,
            )?;
            if compression_ratio_exceeded(
                declared,
                entry.compressed_size(),
                self.limits.maximum_compression_ratio,
            ) {
                return Err(PackageError::CompressionRatio { path });
            }
            let content =
                read_stream_bounded(&mut entry, self.limits.maximum_file_bytes, path.as_str())?;
            if u64::try_from(content.len()).ok() != Some(declared) {
                return Err(PackageError::TruncatedEntry(path));
            }
            files.insert(path, content);
        }
        Ok(files)
    }

    fn read_directory(&self, root: &Path) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        let mut entries_seen = 0_usize;
        let mut total = 0_u64;
        while let Some(directory) = pending.pop() {
            let mut entries = fs::read_dir(&directory)
                .map_err(|error| io_error(&directory, error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| io_error(&directory, error))?;
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                entries_seen = entries_seen.saturating_add(1);
                if entries_seen > self.limits.maximum_entries {
                    return Err(PackageError::EntryLimit {
                        actual: entries_seen,
                        maximum: self.limits.maximum_entries,
                    });
                }
                let path = entry.path();
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
                let logical = canonical_relative_path(root, &path)?;
                if metadata.file_type().is_symlink() {
                    return Err(PackageError::SymlinkEntry(logical));
                }
                if metadata.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !metadata.is_file() {
                    return Err(PackageError::SourceType(display_path(&path)));
                }
                self.check_file_size(&logical, metadata.len(), self.limits.maximum_file_bytes)?;
                total = checked_total(
                    total,
                    metadata.len(),
                    self.limits.maximum_total_uncompressed_bytes,
                )?;
                let content = read_file_bounded(&path, self.limits.maximum_file_bytes)?;
                if files.insert(logical.clone(), content).is_some() {
                    return Err(PackageError::DuplicatePath(logical));
                }
            }
        }
        Ok(files)
    }

    fn build_loaded(
        &self,
        mut files: BTreeMap<String, Vec<u8>>,
        source: PackageSourceKind,
        archive_sha256: Option<Sha256Digest>,
        verifier: &dyn SignatureVerifier,
    ) -> Result<LoadedPackage, PackageError> {
        let signature_bytes = files.remove(SIGNATURE_PATH);
        if signature_bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() as u64 > self.limits.maximum_signature_bytes)
        {
            return Err(PackageError::FileTooLarge {
                path: SIGNATURE_PATH.into(),
                actual: signature_bytes.as_ref().map_or(0, Vec::len) as u64,
                maximum: self.limits.maximum_signature_bytes,
            });
        }
        let manifest_bytes = files
            .get(MANIFEST_PATH)
            .ok_or(PackageError::MissingManifest)?;
        self.check_file_size(
            MANIFEST_PATH,
            manifest_bytes.len() as u64,
            self.limits.maximum_manifest_bytes,
        )?;
        let manifest_text = std::str::from_utf8(manifest_bytes)
            .map_err(|_| PackageError::InvalidManifest("manifest is not UTF-8".into()))?;
        let manifest: ExtensionManifest = toml::from_str(manifest_text)
            .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
        manifest
            .validate_with(&self.limits.manifest_validation)
            .map_err(|errors| PackageError::ManifestValidation(errors.into_vec()))?;

        let (ui_path, component_path) = entrypoint_paths(&manifest.entrypoint);
        if ui_path.as_ref() == component_path.as_ref() && ui_path.is_some() {
            return Err(PackageError::InvalidComponent(
                "UI and component paths must differ".into(),
            ));
        }
        let ui_bytes = optional_declared_file(&files, ui_path.as_ref())?;
        if let Some((path, bytes)) = ui_path.as_ref().zip(ui_bytes) {
            self.check_file_size(
                path.as_str(),
                bytes.len() as u64,
                self.limits.maximum_ui_bytes,
            )?;
            validate_ui(bytes, &self.limits)?;
        }
        let component_bytes = optional_declared_file(&files, component_path.as_ref())?;
        if let Some((path, bytes)) = component_path.as_ref().zip(component_bytes) {
            self.check_file_size(
                path.as_str(),
                bytes.len() as u64,
                self.limits.maximum_component_bytes,
            )?;
            validate_component(bytes)?;
        }

        let mut asset_paths = Vec::new();
        for path in files.keys() {
            if path == MANIFEST_PATH
                || ui_path.as_ref().is_some_and(|ui| ui.as_str() == path)
                || component_path
                    .as_ref()
                    .is_some_and(|component| component.as_str() == path)
            {
                continue;
            }
            if !path.starts_with("assets/") {
                return Err(PackageError::UnexpectedFile(path.clone()));
            }
            asset_paths.push(path.clone());
        }
        if asset_paths.len() > self.limits.maximum_assets {
            return Err(PackageError::AssetLimit {
                actual: asset_paths.len(),
                maximum: self.limits.maximum_assets,
            });
        }
        for path in &asset_paths {
            let bytes = files.get(path).expect("collected package path exists");
            self.check_file_size(path, bytes.len() as u64, self.limits.maximum_asset_bytes)?;
            validate_asset(path, bytes)?;
        }

        let content_sha256 = canonical_content_hash(
            files
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        );
        let signature =
            self.verify_signature(signature_bytes.as_deref(), source, content_sha256, verifier)?;

        let manifest_blob = PackageBlob::new(
            PackagePath::parse(MANIFEST_PATH).expect("constant package path"),
            files.remove(MANIFEST_PATH).expect("manifest exists"),
        );
        let ui = ui_path.map(|path| {
            PackageBlob::new(
                path.clone(),
                files.remove(path.as_str()).expect("declared UI exists"),
            )
        });
        let component = component_path.map(|path| {
            PackageBlob::new(
                path.clone(),
                files
                    .remove(path.as_str())
                    .expect("declared component exists"),
            )
        });
        let mut assets = BTreeMap::new();
        for path in asset_paths {
            let package_path = PackagePath::parse(path.clone()).expect("validated package path");
            let blob = PackageBlob::new(
                package_path.clone(),
                files.remove(&path).expect("asset exists"),
            );
            assets.insert(package_path, blob);
        }
        debug_assert!(files.is_empty());
        Ok(LoadedPackage::new(
            source,
            manifest,
            manifest_blob,
            ui,
            component,
            assets,
            content_sha256,
            archive_sha256,
            signature,
        ))
    }

    fn verify_signature(
        &self,
        bytes: Option<&[u8]>,
        source: PackageSourceKind,
        content_sha256: Sha256Digest,
        verifier: &dyn SignatureVerifier,
    ) -> Result<Option<VerifiedPackageSignature>, PackageError> {
        let required = match source {
            PackageSourceKind::KeyextArchive => self.signature_policy.require_signed_archives,
            PackageSourceKind::DevelopmentDirectory => {
                !self.signature_policy.allow_unsigned_development_directories
            }
        };
        let Some(bytes) = bytes else {
            return if required {
                Err(PackageError::SignatureRequired)
            } else {
                Ok(None)
            };
        };
        let metadata =
            SignatureMetadata::parse(bytes).map_err(PackageError::InvalidSignatureMetadata)?;
        if metadata.signed_content_hash() != content_sha256 {
            return Err(PackageError::SignatureHashMismatch);
        }
        let signer = verifier
            .verify(&metadata, &content_sha256)
            .map_err(PackageError::SignatureRejected)?;
        if signer.key_id != metadata.key_id() {
            return Err(PackageError::SignatureRejected(
                SignatureVerificationError::InvalidSignature,
            ));
        }
        Ok(Some(VerifiedPackageSignature::new(metadata, signer)))
    }

    fn check_file_size(&self, path: &str, actual: u64, maximum: u64) -> Result<(), PackageError> {
        if actual > maximum {
            Err(PackageError::FileTooLarge {
                path: path.into(),
                actual,
                maximum,
            })
        } else {
            Ok(())
        }
    }
}

fn audit_central_directory(
    archive: &[u8],
    start: u64,
    maximum_entries: usize,
) -> Result<(), PackageError> {
    const CENTRAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    const FIXED_HEADER_BYTES: usize = 46;
    const NAME_LENGTH_OFFSET: usize = 28;
    const EXTRA_LENGTH_OFFSET: usize = 30;
    const COMMENT_LENGTH_OFFSET: usize = 32;

    let mut cursor = usize::try_from(start)
        .ok()
        .filter(|start| *start <= archive.len())
        .ok_or_else(|| {
            PackageError::InvalidArchive("central directory offset is invalid".into())
        })?;
    let mut count = 0_usize;
    let mut seen = BTreeSet::new();
    while archive
        .get(cursor..cursor.saturating_add(4))
        .is_some_and(|signature| signature == CENTRAL_HEADER_SIGNATURE)
    {
        let header_end = cursor.checked_add(FIXED_HEADER_BYTES).ok_or_else(|| {
            PackageError::InvalidArchive("central directory header overflows".into())
        })?;
        let header = archive.get(cursor..header_end).ok_or_else(|| {
            PackageError::InvalidArchive("truncated central directory header".into())
        })?;
        let name_length = usize::from(u16::from_le_bytes([
            header[NAME_LENGTH_OFFSET],
            header[NAME_LENGTH_OFFSET + 1],
        ]));
        let extra_length = usize::from(u16::from_le_bytes([
            header[EXTRA_LENGTH_OFFSET],
            header[EXTRA_LENGTH_OFFSET + 1],
        ]));
        let comment_length = usize::from(u16::from_le_bytes([
            header[COMMENT_LENGTH_OFFSET],
            header[COMMENT_LENGTH_OFFSET + 1],
        ]));
        let name_end = header_end.checked_add(name_length).ok_or_else(|| {
            PackageError::InvalidArchive("central directory name overflows".into())
        })?;
        let raw_name = archive.get(header_end..name_end).ok_or_else(|| {
            PackageError::InvalidArchive("truncated central directory name".into())
        })?;
        let raw_name = std::str::from_utf8(raw_name)
            .map_err(|_| PackageError::UnsafePath("<non-UTF-8>".into()))?;
        let (path, _) = canonical_archive_path(raw_name, raw_name.ends_with('/'))?;
        if !seen.insert(path.clone()) {
            return Err(PackageError::DuplicatePath(path));
        }
        count = count.saturating_add(1);
        if count > maximum_entries {
            return Err(PackageError::EntryLimit {
                actual: count,
                maximum: maximum_entries,
            });
        }
        cursor = name_end
            .checked_add(extra_length)
            .and_then(|next| next.checked_add(comment_length))
            .filter(|next| *next <= archive.len())
            .ok_or_else(|| {
                PackageError::InvalidArchive("truncated central directory entry".into())
            })?;
    }
    Ok(())
}

fn symlink_metadata(path: &Path) -> Result<fs::Metadata, PackageError> {
    fs::symlink_metadata(path).map_err(|error| io_error(path, error))
}

fn io_error(path: &Path, error: std::io::Error) -> PackageError {
    PackageError::Io {
        path: display_path(path),
        message: error.to_string(),
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_file_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, PackageError> {
    let file = File::open(path).map_err(|error| io_error(path, error))?;
    read_stream_bounded(file, maximum, &display_path(path))
}

fn read_stream_bounded(
    mut reader: impl Read,
    maximum: u64,
    path: &str,
) -> Result<Vec<u8>, PackageError> {
    let capacity = usize::try_from(maximum.min(1024 * 1024)).unwrap_or(1024 * 1024);
    let mut output = Vec::with_capacity(capacity);
    reader
        .by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut output)
        .map_err(|error| PackageError::Io {
            path: path.into(),
            message: error.to_string(),
        })?;
    if output.len() as u64 > maximum {
        return Err(PackageError::FileTooLarge {
            path: path.into(),
            actual: output.len() as u64,
            maximum,
        });
    }
    Ok(output)
}

fn canonical_archive_path(raw: &str, directory: bool) -> Result<(String, bool), PackageError> {
    let raw = if directory {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    };
    let path =
        PackagePath::parse(raw.to_owned()).map_err(|_| PackageError::UnsafePath(raw.to_owned()))?;
    Ok((path.as_str().to_owned(), directory))
}

fn canonical_relative_path(root: &Path, path: &Path) -> Result<String, PackageError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PackageError::UnsafePath(display_path(path)))?;
    let mut segments = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => segments.push(
                segment
                    .to_str()
                    .ok_or_else(|| PackageError::UnsafePath(display_path(relative)))?,
            ),
            _ => return Err(PackageError::UnsafePath(display_path(relative))),
        }
    }
    let logical = segments.join("/");
    PackagePath::parse(logical.clone()).map_err(|_| PackageError::UnsafePath(logical.clone()))?;
    Ok(logical)
}

fn checked_total(current: u64, additional: u64, maximum: u64) -> Result<u64, PackageError> {
    let actual = current.saturating_add(additional);
    if actual > maximum {
        Err(PackageError::TotalSizeLimit { actual, maximum })
    } else {
        Ok(actual)
    }
}

fn compression_ratio_exceeded(uncompressed: u64, compressed: u64, maximum: u64) -> bool {
    uncompressed > 0 && (compressed == 0 || uncompressed > compressed.saturating_mul(maximum))
}

fn entrypoint_paths(
    entrypoint: &ExtensionEntrypoint,
) -> (Option<PackagePath>, Option<PackagePath>) {
    match entrypoint {
        ExtensionEntrypoint::Declarative { ui } => (Some(ui.clone()), None),
        ExtensionEntrypoint::WasmComponent { component, ui, .. } => {
            (ui.clone(), Some(component.clone()))
        }
        ExtensionEntrypoint::NativeBuiltin { ui, .. } => (ui.clone(), None),
    }
}

fn optional_declared_file<'a>(
    files: &'a BTreeMap<String, Vec<u8>>,
    path: Option<&PackagePath>,
) -> Result<Option<&'a [u8]>, PackageError> {
    path.map(|path| {
        files
            .get(path.as_str())
            .map(Vec::as_slice)
            .ok_or_else(|| PackageError::MissingEntrypointFile(path.as_str().into()))
    })
    .transpose()
}

fn validate_ui(bytes: &[u8], limits: &PackageLimits) -> Result<(), PackageError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| PackageError::InvalidUi(error.to_string()))?;
    let mut nodes = 0_usize;
    if !json_is_bounded(
        &value,
        1,
        &mut nodes,
        limits.manifest_validation.maximum_value_depth,
        limits.manifest_validation.maximum_value_nodes,
        limits.manifest_validation.maximum_list_items,
        limits.manifest_validation.maximum_string_bytes,
    ) {
        return Err(PackageError::InvalidUi(
            "UI JSON exceeds structural limits".into(),
        ));
    }
    Ok(())
}

fn json_is_bounded(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
    maximum_depth: usize,
    maximum_nodes: usize,
    maximum_items: usize,
    maximum_string_bytes: usize,
) -> bool {
    *nodes = nodes.saturating_add(1);
    if depth > maximum_depth || *nodes > maximum_nodes {
        return false;
    }
    match value {
        serde_json::Value::String(value) => value.len() <= maximum_string_bytes,
        serde_json::Value::Array(values) => {
            values.len() <= maximum_items
                && values.iter().all(|value| {
                    json_is_bounded(
                        value,
                        depth + 1,
                        nodes,
                        maximum_depth,
                        maximum_nodes,
                        maximum_items,
                        maximum_string_bytes,
                    )
                })
        }
        serde_json::Value::Object(values) => {
            values.len() <= maximum_items
                && values.iter().all(|(key, value)| {
                    key.len() <= maximum_string_bytes
                        && json_is_bounded(
                            value,
                            depth + 1,
                            nodes,
                            maximum_depth,
                            maximum_nodes,
                            maximum_items,
                            maximum_string_bytes,
                        )
                })
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

fn validate_component(bytes: &[u8]) -> Result<(), PackageError> {
    if bytes.len() < 8 || !bytes.starts_with(b"\0asm") {
        return Err(PackageError::InvalidComponent(
            "component does not have WebAssembly magic/version bytes".into(),
        ));
    }
    Ok(())
}

fn validate_asset(path: &str, bytes: &[u8]) -> Result<(), PackageError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| PackageError::InvalidAsset {
            path: path.into(),
            reason: "asset has no supported extension".into(),
        })?;
    let valid = match extension.as_str() {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "json" => serde_json::from_slice::<serde_json::Value>(bytes).is_ok(),
        "toml" => std::str::from_utf8(bytes)
            .ok()
            .and_then(|source| toml::from_str::<toml::Value>(source).ok())
            .is_some(),
        "svg" => safe_svg(bytes),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(PackageError::InvalidAsset {
            path: path.into(),
            reason: "unsupported extension or malformed content".into(),
        })
    }
}

fn safe_svg(bytes: &[u8]) -> bool {
    let Ok(source) = std::str::from_utf8(bytes) else {
        return false;
    };
    let lower = source.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    (trimmed.starts_with("<svg") || trimmed.starts_with("<?xml"))
        && !lower.contains("<script")
        && !lower.contains("<!doctype")
        && !lower.contains("<!entity")
        && !lower.contains("javascript:")
        && !lower.contains("href=\"http:")
        && !lower.contains("href=\"https:")
        && !lower.contains("href='http:")
        && !lower.contains("href='https:")
}
