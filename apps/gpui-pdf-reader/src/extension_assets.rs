//! Bounded, immutable assets for host-rendered extension UI.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    error::Error,
    fmt,
    io::Cursor,
    sync::{Arc, RwLock},
};

use gpui::{AssetSource, Result as GpuiResult, SharedString};
use image::{ImageFormat, ImageReader};
use key_extension_api::{ExtensionId, PackagePath};

const EXTENSION_ASSET_PREFIX: &str = "key-extensions/";
const MAX_ASSET_BYTES: usize = 4 * 1024 * 1024;
const MAX_EXTENSION_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 4_096;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionAssetError {
    TooLarge,
    UnsupportedImage,
    InvalidImage,
    InvalidDimensions,
    DuplicatePath(PackagePath),
}

impl fmt::Display for ExtensionAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("extension image assets exceed the byte limit"),
            Self::UnsupportedImage => {
                formatter.write_str("extension UI images must be PNG, JPEG, or safe SVG")
            }
            Self::InvalidImage => formatter.write_str("extension image asset is malformed"),
            Self::InvalidDimensions => {
                formatter.write_str("extension image dimensions exceed the host limit")
            }
            Self::DuplicatePath(path) => {
                write!(
                    formatter,
                    "extension image asset '{}' is duplicated",
                    path.as_str()
                )
            }
        }
    }
}

impl Error for ExtensionAssetError {}

/// Thread-safe immutable blob registry shared by the installer and GPUI's
/// application asset source. A complete extension namespace is swapped only
/// after every referenced image passes validation.
#[derive(Default)]
pub struct ExtensionAssetStore {
    assets: RwLock<BTreeMap<String, Arc<[u8]>>>,
}

impl ExtensionAssetStore {
    pub fn replace_extension(
        &self,
        owner: &ExtensionId,
        assets: impl IntoIterator<Item = (PackagePath, Vec<u8>)>,
    ) -> Result<(), ExtensionAssetError> {
        let prefix = extension_prefix(owner);
        let mut total = 0usize;
        let mut validated = BTreeMap::new();
        for (path, bytes) in assets {
            total = total
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_EXTENSION_ASSET_BYTES)
                .ok_or(ExtensionAssetError::TooLarge)?;
            validate_image(&bytes)?;
            let key = format!("{prefix}{}", path.as_str());
            if validated.insert(key, Arc::<[u8]>::from(bytes)).is_some() {
                return Err(ExtensionAssetError::DuplicatePath(path));
            }
        }

        let mut current = self
            .assets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.retain(|key, _| !key.starts_with(&prefix));
        current.extend(validated);
        Ok(())
    }

    pub fn remove_extension(&self, owner: &ExtensionId) {
        let prefix = extension_prefix(owner);
        self.assets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, _| !key.starts_with(&prefix));
    }

    fn load(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        self.assets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .map(|bytes| Cow::Owned(bytes.to_vec()))
    }

    fn list(&self, path: &str) -> Vec<SharedString> {
        self.assets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|key| key.starts_with(path))
            .cloned()
            .map(SharedString::from)
            .collect()
    }
}

/// Composite GPUI source retaining the component icon bundle while serving
/// extension-owned virtual paths from memory. Filesystem paths are never
/// interpreted at asset-load time.
pub struct ReaderAssetSource {
    extensions: Arc<ExtensionAssetStore>,
}

impl ReaderAssetSource {
    #[must_use]
    pub fn new(extensions: Arc<ExtensionAssetStore>) -> Self {
        Self { extensions }
    }
}

impl AssetSource for ReaderAssetSource {
    fn load(&self, path: &str) -> GpuiResult<Option<Cow<'static, [u8]>>> {
        if path.starts_with(EXTENSION_ASSET_PREFIX) {
            Ok(self.extensions.load(path))
        } else {
            gpui_component_assets::Assets.load(path)
        }
    }

    fn list(&self, path: &str) -> GpuiResult<Vec<SharedString>> {
        if path.starts_with(EXTENSION_ASSET_PREFIX) {
            Ok(self.extensions.list(path))
        } else {
            gpui_component_assets::Assets.list(path)
        }
    }
}

fn extension_prefix(owner: &ExtensionId) -> String {
    format!("{EXTENSION_ASSET_PREFIX}{owner}/")
}

fn validate_image(bytes: &[u8]) -> Result<(), ExtensionAssetError> {
    if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
        return Err(ExtensionAssetError::TooLarge);
    }
    if safe_svg(bytes) {
        return Ok(());
    }
    let format = image::guess_format(bytes).map_err(|_| ExtensionAssetError::InvalidImage)?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Err(ExtensionAssetError::UnsupportedImage);
    }
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| ExtensionAssetError::InvalidImage)?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_EDGE
        || height > MAX_IMAGE_EDGE
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(ExtensionAssetError::InvalidDimensions);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PIXEL_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31, 0, 3, 3,
        2, 0, 239, 191, 105, 191, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn owner() -> ExtensionId {
        ExtensionId::parse("org.example.assets").unwrap()
    }

    #[test]
    fn assets_are_namespaced_replaced_and_removed_atomically() {
        let store = ExtensionAssetStore::default();
        let path = PackagePath::parse("images/preview.png").unwrap();
        store
            .replace_extension(&owner(), [(path, ONE_PIXEL_PNG.to_vec())])
            .unwrap();
        assert!(
            store
                .load("key-extensions/org.example.assets/images/preview.png")
                .is_some()
        );

        assert!(matches!(
            store.replace_extension(
                &owner(),
                [(
                    PackagePath::parse("images/bad.png").unwrap(),
                    b"not an image".to_vec()
                )]
            ),
            Err(ExtensionAssetError::InvalidImage)
        ));
        assert!(
            store
                .load("key-extensions/org.example.assets/images/preview.png")
                .is_some()
        );

        store.remove_extension(&owner());
        assert!(
            store
                .load("key-extensions/org.example.assets/images/preview.png")
                .is_none()
        );
    }
}
