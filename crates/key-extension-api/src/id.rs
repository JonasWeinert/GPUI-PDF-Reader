use std::{fmt, str::FromStr};

use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{ValidationCode, ValidationError};

const MAX_IDENTIFIER_BYTES: usize = 255;

/// A canonical reverse-domain extension identifier such as
/// `org.example.document-stats`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExtensionId(String);

impl ExtensionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_extension_id(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ExtensionId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ExtensionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExtensionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

fn validate_extension_id(value: &str) -> Result<(), ValidationError> {
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(invalid_id("extension ID exceeds 255 bytes"));
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(invalid_id(
            "extension ID must contain at least two dot-separated segments",
        ));
    }
    if segments.iter().any(|segment| !valid_segment(segment)) {
        return Err(invalid_id(
            "extension ID segments must start with a lowercase ASCII letter and contain only lowercase letters, digits, or hyphens",
        ));
    }
    Ok(())
}

fn valid_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !segment.ends_with('-')
}

fn invalid_id(message: impl Into<String>) -> ValidationError {
    ValidationError::new(ValidationCode::InvalidIdentifier, "$", message)
}

/// An identifier owned by an extension. Its textual form is
/// `<extension-id>/<local-name>`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct NamespacedId(String);

impl NamespacedId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.len() > MAX_IDENTIFIER_BYTES {
            return Err(invalid_id("namespaced ID exceeds 255 bytes"));
        }
        let Some((owner, local)) = value.split_once('/') else {
            return Err(invalid_id(
                "namespaced ID must have the form <extension-id>/<local-name>",
            ));
        };
        ExtensionId::parse(owner)?;
        if local.is_empty()
            || local.split('/').any(|segment| !valid_segment(segment))
            || local.contains("//")
        {
            return Err(invalid_id(
                "local ID path must contain canonical lowercase segments",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn owner(&self) -> ExtensionId {
        let (owner, _) = self.0.split_once('/').expect("validated namespaced ID");
        ExtensionId::parse(owner).expect("validated namespaced ID owner")
    }

    #[must_use]
    pub fn local_name(&self) -> &str {
        self.0
            .split_once('/')
            .map_or("", |(_, local_name)| local_name)
    }
}

impl fmt::Display for NamespacedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for NamespacedId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for NamespacedId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

macro_rules! namespaced_id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub NamespacedId);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
                NamespacedId::parse(value).map(Self)
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }

            #[must_use]
            pub fn owner(&self) -> ExtensionId {
                self.0.owner()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

namespaced_id_type!(CommandId);
namespaced_id_type!(ContributionId);
namespaced_id_type!(TaskId);
namespaced_id_type!(EffectId);
namespaced_id_type!(NativeAdapterId);

/// A contribution-local identifier. Hosts scope it to the containing
/// extension and contribution before using it as a stable UI key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct LocalId(String);

impl LocalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_IDENTIFIER_BYTES
            || value.split('/').any(|segment| !valid_segment(segment))
        {
            return Err(invalid_id(
                "local ID must contain canonical lowercase path segments",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LocalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LocalId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for LocalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// A semantic capability name such as `key:pdf/text`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.len() > MAX_IDENTIFIER_BYTES {
            return Err(invalid_id("capability ID exceeds 255 bytes"));
        }
        let Some((namespace, path)) = value.split_once(':') else {
            return Err(invalid_id(
                "capability ID must have the form <namespace>:<path>",
            ));
        };
        if !valid_segment(namespace)
            || path.is_empty()
            || path.split('/').any(|segment| !valid_segment(segment))
        {
            return Err(invalid_id("capability ID is not canonical"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CapabilityId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for CapabilityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// A host-owned menu insertion point such as `view.appearance`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MenuSlotId(String);

impl MenuSlotId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.len() > MAX_IDENTIFIER_BYTES
            || value.split('.').any(|segment| !valid_segment(segment))
            || !value.contains('.')
        {
            return Err(invalid_id(
                "menu slot must be a canonical dotted name such as view.appearance",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MenuSlotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MenuSlotId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for MenuSlotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// A portable package-relative path. It is not an operating-system path and
/// cannot address a parent directory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PackagePath(String);

impl PackagePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 1024
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains('\\')
            || value.contains('\0')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(ValidationError::new(
                ValidationCode::InvalidPackagePath,
                "$",
                "package path must be relative, normalized, and contain no parent segments",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for PackagePath {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for PackagePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExtensionVersion(pub Version);

impl FromStr for ExtensionVersion {
    type Err = semver::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Version::parse(value).map(Self)
    }
}

impl fmt::Display for ExtensionVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompatibleVersion(pub VersionReq);

impl CompatibleVersion {
    #[must_use]
    pub fn matches(&self, version: &ExtensionVersion) -> bool {
        self.0.matches(&version.0)
    }
}

impl FromStr for CompatibleVersion {
    type Err = semver::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        VersionReq::parse(value).map(Self)
    }
}

impl fmt::Display for CompatibleVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_identifiers() {
        assert!(ExtensionId::parse("org.example.document-stats").is_ok());
        assert!(NamespacedId::parse("org.example.document-stats/show-panel").is_ok());
        assert!(CapabilityId::parse("key:pdf/read-text").is_ok());
        assert!(MenuSlotId::parse("view.appearance").is_ok());
    }

    #[test]
    fn rejects_noncanonical_identifiers() {
        for invalid in ["example", "Org.example", "org..example", "org.example-"] {
            assert!(ExtensionId::parse(invalid).is_err(), "accepted {invalid}");
        }
        assert!(NamespacedId::parse("org.example").is_err());
        assert!(CapabilityId::parse("pdf.text").is_err());
        assert!(MenuSlotId::parse("view").is_err());
    }

    #[test]
    fn rejects_unsafe_package_paths() {
        for invalid in ["", "/root", "../secret", "assets/../secret", "a\\b", "a//b"] {
            assert!(PackagePath::parse(invalid).is_err(), "accepted {invalid}");
        }
        assert!(PackagePath::parse("assets/icons/tool.svg").is_ok());
    }

    #[test]
    fn deserialization_validates_newtypes() {
        let error = serde_json::from_str::<ExtensionId>(r#""Org.Invalid""#).unwrap_err();
        assert!(error.to_string().contains("extension ID"));
    }
}
