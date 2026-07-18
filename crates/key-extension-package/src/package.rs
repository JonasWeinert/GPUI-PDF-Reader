use std::{collections::BTreeMap, sync::Arc};

use key_extension_api::{ExtensionManifest, PackagePath};

use crate::{Sha256Digest, SignatureMetadata, VerifiedSigner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageSourceKind {
    KeyextArchive,
    DevelopmentDirectory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageBlob {
    path: PackagePath,
    bytes: Arc<[u8]>,
    sha256: Sha256Digest,
}

impl PackageBlob {
    pub(crate) fn new(path: PackagePath, bytes: Vec<u8>) -> Self {
        let sha256 = Sha256Digest::of(&bytes);
        Self {
            path,
            bytes: bytes.into(),
            sha256,
        }
    }

    #[must_use]
    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn sha256(&self) -> Sha256Digest {
        self.sha256
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPackageSignature {
    metadata: SignatureMetadata,
    signer: VerifiedSigner,
}

impl VerifiedPackageSignature {
    pub(crate) fn new(metadata: SignatureMetadata, signer: VerifiedSigner) -> Self {
        Self { metadata, signer }
    }

    #[must_use]
    pub const fn metadata(&self) -> &SignatureMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn signer(&self) -> &VerifiedSigner {
        &self.signer
    }
}

/// Fully validated, memory-owned package. It retains no file or archive handle
/// and cannot observe later changes to a development directory.
#[derive(Clone, Debug)]
pub struct LoadedPackage {
    source: PackageSourceKind,
    manifest: ExtensionManifest,
    manifest_blob: PackageBlob,
    ui: Option<PackageBlob>,
    component: Option<PackageBlob>,
    assets: BTreeMap<PackagePath, PackageBlob>,
    content_sha256: Sha256Digest,
    archive_sha256: Option<Sha256Digest>,
    signature: Option<VerifiedPackageSignature>,
}

impl LoadedPackage {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: PackageSourceKind,
        manifest: ExtensionManifest,
        manifest_blob: PackageBlob,
        ui: Option<PackageBlob>,
        component: Option<PackageBlob>,
        assets: BTreeMap<PackagePath, PackageBlob>,
        content_sha256: Sha256Digest,
        archive_sha256: Option<Sha256Digest>,
        signature: Option<VerifiedPackageSignature>,
    ) -> Self {
        Self {
            source,
            manifest,
            manifest_blob,
            ui,
            component,
            assets,
            content_sha256,
            archive_sha256,
            signature,
        }
    }

    #[must_use]
    pub const fn source(&self) -> PackageSourceKind {
        self.source
    }

    #[must_use]
    pub const fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    #[must_use]
    pub const fn manifest_blob(&self) -> &PackageBlob {
        &self.manifest_blob
    }

    #[must_use]
    pub const fn ui(&self) -> Option<&PackageBlob> {
        self.ui.as_ref()
    }

    #[must_use]
    pub const fn component(&self) -> Option<&PackageBlob> {
        self.component.as_ref()
    }

    #[must_use]
    pub const fn assets(&self) -> &BTreeMap<PackagePath, PackageBlob> {
        &self.assets
    }

    #[must_use]
    pub const fn content_sha256(&self) -> Sha256Digest {
        self.content_sha256
    }

    #[must_use]
    pub const fn archive_sha256(&self) -> Option<Sha256Digest> {
        self.archive_sha256
    }

    #[must_use]
    pub const fn signature(&self) -> Option<&VerifiedPackageSignature> {
        self.signature.as_ref()
    }
}
