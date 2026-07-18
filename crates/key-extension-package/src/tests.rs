use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use key_extension_api::{
    CURRENT_MANIFEST_SCHEMA, CapabilityRequirements, CompatibleVersion, ContributionSet,
    ExtensionEntrypoint, ExtensionId, ExtensionManifest, ExtensionVersion, HostCompatibility,
    PackagePath, Publisher, SettingsSchema, StorageRequirements,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    DenyAllSignatureVerifier, LoadedPackage, PackageError, PackageLimits, PackageLoader,
    PackageSourceKind, Sha256Digest, SignatureMetadata, SignaturePolicy,
    SignatureVerificationError, SignatureVerifier, VerifiedSigner, digest::canonical_content_hash,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "key-extension-package-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn version(value: &str) -> ExtensionVersion {
    ExtensionVersion::from_str(value).unwrap()
}

fn requirement(value: &str) -> CompatibleVersion {
    CompatibleVersion::from_str(value).unwrap()
}

fn wasm_manifest() -> ExtensionManifest {
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: ExtensionId::parse("org.example.package-test").unwrap(),
        name: "Package Test".into(),
        version: version("1.0.0"),
        publisher: Publisher {
            id: "org.example".into(),
            name: "Example".into(),
        },
        description: "Package loader fixture".into(),
        license: "MIT".into(),
        compatibility: HostCompatibility {
            extension_api: requirement("^0.1"),
            minimum_host: None,
            platforms: Vec::new(),
        },
        entrypoint: ExtensionEntrypoint::WasmComponent {
            component: PackagePath::parse("component.wasm").unwrap(),
            world: "key:extension/test@0.1.0".into(),
            ui: Some(PackagePath::parse("ui.json").unwrap()),
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements::default(),
        permissions: Vec::new(),
        contributions: ContributionSet::default(),
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn valid_files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            "manifest.toml".into(),
            toml::to_string(&wasm_manifest()).unwrap().into_bytes(),
        ),
        ("ui.json".into(), br#"{"kind":"panel"}"#.to_vec()),
        (
            "component.wasm".into(),
            b"\0asm\x0d\0\x01\0fixture".to_vec(),
        ),
        (
            "assets/icon.png".into(),
            b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        ),
    ])
}

fn unsigned_policy() -> SignaturePolicy {
    SignaturePolicy {
        require_signed_archives: false,
        allow_unsigned_development_directories: true,
    }
}

fn loader_with(limits: PackageLimits) -> PackageLoader {
    PackageLoader::new(limits, unsigned_policy())
}

fn write_zip(path: &Path, entries: &[(String, Vec<u8>, CompressionMethod)]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    for (name, bytes, compression) in entries {
        let options = SimpleFileOptions::default().compression_method(*compression);
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn write_files(root: &Path, files: &BTreeMap<String, Vec<u8>>) {
    for (path, bytes) in files {
        let destination = root.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, bytes).unwrap();
    }
}

fn archive_from_files(root: &TempDirectory, files: &BTreeMap<String, Vec<u8>>) -> PathBuf {
    let archive = root.path().join("extension.keyext");
    let entries = files
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.clone(), CompressionMethod::Deflated))
        .collect::<Vec<_>>();
    write_zip(&archive, &entries);
    archive
}

fn load_unsigned_archive(files: &BTreeMap<String, Vec<u8>>) -> LoadedPackage {
    let temp = TempDirectory::new();
    let archive = archive_from_files(&temp, files);
    loader_with(PackageLimits::default())
        .load_keyext(archive, &DenyAllSignatureVerifier)
        .unwrap()
}

#[test]
fn archive_and_directory_produce_same_immutable_content() {
    let temp = TempDirectory::new();
    let files = valid_files();
    let archive = archive_from_files(&temp, &files);
    let development = temp.path().join("development");
    fs::create_dir(&development).unwrap();
    write_files(&development, &files);

    let loader = loader_with(PackageLimits::default());
    let archived = loader
        .load_keyext(&archive, &DenyAllSignatureVerifier)
        .unwrap();
    let unpacked = loader
        .load_development_directory(&development, &DenyAllSignatureVerifier)
        .unwrap();
    assert_eq!(archived.source(), PackageSourceKind::KeyextArchive);
    assert_eq!(unpacked.source(), PackageSourceKind::DevelopmentDirectory);
    assert_eq!(archived.content_sha256(), unpacked.content_sha256());
    assert!(archived.archive_sha256().is_some());
    assert!(unpacked.archive_sha256().is_none());
    assert_eq!(archived.assets().len(), 1);
    assert!(archived.ui().is_some());
    assert!(archived.component().is_some());

    fs::write(development.join("ui.json"), b"changed").unwrap();
    assert_eq!(unpacked.ui().unwrap().bytes(), br#"{"kind":"panel"}"#);
}

#[test]
fn default_policy_denies_unsigned_packages() {
    let temp = TempDirectory::new();
    let files = valid_files();
    let archive = archive_from_files(&temp, &files);
    let development = temp.path().join("development");
    fs::create_dir(&development).unwrap();
    write_files(&development, &files);
    let loader = PackageLoader::default();
    assert_eq!(
        loader
            .load_keyext(archive, &DenyAllSignatureVerifier)
            .unwrap_err(),
        PackageError::SignatureRequired
    );
    assert_eq!(
        loader
            .load_development_directory(development, &DenyAllSignatureVerifier)
            .unwrap_err(),
        PackageError::SignatureRequired
    );
}

#[test]
fn traversal_and_duplicate_paths_are_rejected_before_manifest_use() {
    let temp = TempDirectory::new();
    let traversal = temp.path().join("traversal.keyext");
    write_zip(
        &traversal,
        &[(
            "../manifest.toml".into(),
            b"bad".to_vec(),
            CompressionMethod::Stored,
        )],
    );
    let loader = loader_with(PackageLimits::default());
    assert!(matches!(
        loader.load_keyext(traversal, &DenyAllSignatureVerifier),
        Err(PackageError::UnsafePath(_))
    ));

    let duplicate = temp.path().join("duplicate.keyext");
    let manifest = toml::to_string(&wasm_manifest()).unwrap().into_bytes();
    write_zip(
        &duplicate,
        &[
            (
                "manifest.toml".into(),
                manifest.clone(),
                CompressionMethod::Stored,
            ),
            ("manifesx.toml".into(), manifest, CompressionMethod::Stored),
        ],
    );
    // zip rejects duplicate names while writing. Rewrite the equal-length name in
    // both the local and central directory headers to construct the adversarial
    // archive a loader can still receive from an untrusted source.
    let mut duplicate_bytes = fs::read(&duplicate).unwrap();
    let mut replacements = 0;
    for offset in 0..=duplicate_bytes.len() - b"manifesx.toml".len() {
        if &duplicate_bytes[offset..offset + b"manifesx.toml".len()] == b"manifesx.toml" {
            duplicate_bytes[offset..offset + b"manifest.toml".len()]
                .copy_from_slice(b"manifest.toml");
            replacements += 1;
        }
    }
    assert_eq!(replacements, 2);
    fs::write(&duplicate, duplicate_bytes).unwrap();
    let duplicate_result = loader.load_keyext(duplicate, &DenyAllSignatureVerifier);
    assert!(
        matches!(
            duplicate_result,
            Err(PackageError::DuplicatePath(ref path)) if path == "manifest.toml"
        ),
        "unexpected duplicate-name result: {duplicate_result:?}"
    );
}

#[test]
fn zip_symlinks_are_rejected() {
    let temp = TempDirectory::new();
    let archive = temp.path().join("symlink.keyext");
    let file = File::create(&archive).unwrap();
    let mut writer = ZipWriter::new(file);
    writer
        .add_symlink("manifest.toml", "elsewhere", SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();
    assert!(matches!(
        loader_with(PackageLimits::default())
            .load_keyext(archive, &DenyAllSignatureVerifier),
        Err(PackageError::SymlinkEntry(path)) if path == "manifest.toml"
    ));
}

#[cfg(unix)]
#[test]
fn development_directory_symlinks_are_never_followed() {
    use std::os::unix::fs::symlink;

    let temp = TempDirectory::new();
    let development = temp.path().join("development");
    fs::create_dir(&development).unwrap();
    symlink("outside", development.join("manifest.toml")).unwrap();
    assert!(matches!(
        loader_with(PackageLimits::default())
            .load_development_directory(development, &DenyAllSignatureVerifier),
        Err(PackageError::SymlinkEntry(path)) if path == "manifest.toml"
    ));
}

#[test]
fn entry_count_file_total_and_archive_limits_are_independent() {
    let temp = TempDirectory::new();
    let files = valid_files();
    let archive = archive_from_files(&temp, &files);

    let count_limits = PackageLimits {
        maximum_entries: 1,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(count_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::EntryLimit { .. })
    ));

    let file_limits = PackageLimits {
        maximum_file_bytes: 16,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(file_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::FileTooLarge { .. })
    ));

    let total_limits = PackageLimits {
        maximum_total_uncompressed_bytes: 32,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(total_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::TotalSizeLimit { .. })
    ));

    let archive_len = fs::metadata(&archive).unwrap().len();
    let archive_limits = PackageLimits {
        maximum_archive_bytes: archive_len - 1,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(archive_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::ArchiveTooLarge { .. })
    ));

    let asset_count_limits = PackageLimits {
        maximum_assets: 0,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(asset_count_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::AssetLimit {
            actual: 1,
            maximum: 0
        })
    ));

    let asset_size_limits = PackageLimits {
        maximum_asset_bytes: 4,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(asset_size_limits).load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::FileTooLarge { path, .. }) if path == "assets/icon.png"
    ));
}

#[test]
fn highly_compressed_entry_is_rejected_without_expansion() {
    let temp = TempDirectory::new();
    let mut files = valid_files();
    files.insert("assets/data.json".into(), vec![b' '; 512 * 1024]);
    let archive = archive_from_files(&temp, &files);
    let limits = PackageLimits {
        maximum_compression_ratio: 2,
        ..PackageLimits::default()
    };
    assert!(matches!(
        loader_with(limits).load_keyext(archive, &DenyAllSignatureVerifier),
        Err(PackageError::CompressionRatio { path }) if path == "assets/data.json"
    ));
}

#[test]
fn malformed_manifest_ui_component_and_assets_are_rejected() {
    let mut invalid_manifest = valid_files();
    invalid_manifest.insert("manifest.toml".into(), b"not = [toml".to_vec());
    assert!(matches!(
        load_result(invalid_manifest),
        Err(PackageError::InvalidManifest(_))
    ));

    let mut invalid_ui = valid_files();
    invalid_ui.insert("ui.json".into(), b"{".to_vec());
    assert!(matches!(
        load_result(invalid_ui),
        Err(PackageError::InvalidUi(_))
    ));

    let mut invalid_component = valid_files();
    invalid_component.insert("component.wasm".into(), b"not wasm".to_vec());
    assert!(matches!(
        load_result(invalid_component),
        Err(PackageError::InvalidComponent(_))
    ));

    let mut invalid_asset = valid_files();
    invalid_asset.insert("assets/icon.png".into(), b"not png".to_vec());
    assert!(matches!(
        load_result(invalid_asset),
        Err(PackageError::InvalidAsset { .. })
    ));

    let mut unknown_asset = valid_files();
    unknown_asset.insert("assets/tool.exe".into(), b"MZ".to_vec());
    assert!(matches!(
        load_result(unknown_asset),
        Err(PackageError::InvalidAsset { .. })
    ));

    let mut unexpected = valid_files();
    unexpected.insert("README.md".into(), b"surprise".to_vec());
    assert!(matches!(
        load_result(unexpected),
        Err(PackageError::UnexpectedFile(path)) if path == "README.md"
    ));
}

fn load_result(files: BTreeMap<String, Vec<u8>>) -> Result<LoadedPackage, PackageError> {
    let temp = TempDirectory::new();
    let archive = archive_from_files(&temp, &files);
    loader_with(PackageLimits::default()).load_keyext(archive, &DenyAllSignatureVerifier)
}

struct AcceptVerifier;

impl SignatureVerifier for AcceptVerifier {
    fn verify(
        &self,
        metadata: &SignatureMetadata,
        _content_hash: &Sha256Digest,
    ) -> Result<VerifiedSigner, SignatureVerificationError> {
        Ok(VerifiedSigner {
            key_id: metadata.key_id().into(),
            identity: "Verified Publisher".into(),
        })
    }
}

fn signature_toml(hash: Sha256Digest) -> Vec<u8> {
    format!(
        "schema_version = 1\nalgorithm = \"ed25519\"\nkey_id = \"org.example/release\"\npublisher = \"Example\"\nsigned_content_sha256 = \"{hash}\"\nsignature_hex = \"01020304\"\n"
    )
    .into_bytes()
}

#[test]
fn signature_hash_and_verifier_must_both_succeed() {
    let temp = TempDirectory::new();
    let mut files = valid_files();
    let hash = canonical_content_hash(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    );
    files.insert("signature.toml".into(), signature_toml(hash));
    let archive = archive_from_files(&temp, &files);
    let loader = PackageLoader::default();
    assert!(matches!(
        loader.load_keyext(&archive, &DenyAllSignatureVerifier),
        Err(PackageError::SignatureRejected(
            SignatureVerificationError::UnknownKey(_)
        ))
    ));
    let loaded = loader.load_keyext(&archive, &AcceptVerifier).unwrap();
    assert_eq!(
        loaded.signature().unwrap().signer().identity,
        "Verified Publisher"
    );

    files.insert(
        "signature.toml".into(),
        signature_toml(Sha256Digest::from_bytes([7; 32])),
    );
    let mismatch = temp.path().join("mismatch.keyext");
    let entries = files
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.clone(), CompressionMethod::Stored))
        .collect::<Vec<_>>();
    write_zip(&mismatch, &entries);
    assert_eq!(
        loader.load_keyext(mismatch, &AcceptVerifier).unwrap_err(),
        PackageError::SignatureHashMismatch
    );
}

#[test]
fn malformed_signature_metadata_is_rejected_even_when_signatures_are_optional() {
    let mut files = valid_files();
    files.insert("signature.toml".into(), b"algorithm = 'nope'".to_vec());
    assert!(matches!(
        load_result(files),
        Err(PackageError::InvalidSignatureMetadata(_))
    ));
}

#[test]
fn package_hash_changes_with_path_or_content_and_assets_have_own_hashes() {
    let first = load_unsigned_archive(&valid_files());
    let mut changed = valid_files();
    changed.insert(
        "assets/icon.png".into(),
        b"\x89PNG\r\n\x1a\nchanged".to_vec(),
    );
    let second = load_unsigned_archive(&changed);
    assert_ne!(first.content_sha256(), second.content_sha256());
    let asset = second.assets().values().next().unwrap();
    assert_eq!(asset.sha256(), Sha256Digest::of(asset.bytes()));
}
