use serde::{Deserialize, Serialize};

use crate::{CapabilityId, CompatibleVersion, ExtensionId};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub id: CapabilityId,
    pub version: CompatibleVersion,
    #[serde(default)]
    pub scope: CapabilityScope,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CapabilityScope {
    #[default]
    Application,
    ActiveDocument,
    DocumentSet,
    NamespacedStorage(StorageArea),
    Domains(Vec<DomainPattern>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageArea {
    Settings,
    Document,
    EphemeralCache,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvidedCapability {
    pub id: CapabilityId,
    pub version: crate::ExtensionVersion,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    #[serde(default)]
    pub required: Vec<CapabilityRequest>,
    #[serde(default)]
    pub optional: Vec<CapabilityRequest>,
    #[serde(default)]
    pub provided: Vec<ProvidedCapability>,
}

/// A normalized host name, optionally prefixed by `*.` to cover subdomains.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DomainPattern(pub String);

impl DomainPattern {
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        let domain = self.0.strip_prefix("*.").unwrap_or(&self.0);
        !domain.is_empty()
            && domain.len() <= 253
            && domain.is_ascii()
            && domain == domain.to_ascii_lowercase()
            && domain.split('.').count() >= 2
            && domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub permission: Permission,
    pub reason: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
}

const fn required_by_default() -> bool {
    true
}

/// User-sensitive authority. Capabilities describe protocol availability;
/// permissions describe what data or side effects the user is granting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "scope", rename_all = "snake_case")]
pub enum Permission {
    ReadDocumentMetadata,
    ReadDocumentText(DocumentAccess),
    ReadSelection,
    NavigateDocument,
    AddDocumentOverlays,
    AddSidePanel,
    ReadAnnotations,
    WriteAnnotations,
    MutateDocument(DocumentMutation),
    ClipboardWrite,
    OpenExternalUrl,
    Storage(StoragePermission),
    Network(NetworkScope),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentAccess {
    ActiveDocument,
    UserApprovedDocuments,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentMutation {
    Redact,
    EditAnnotations,
    RewriteDocument,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoragePermission {
    pub area: StorageArea,
    pub quota_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "domains", rename_all = "snake_case")]
pub enum NetworkScope {
    None,
    DeclaredDomains(Vec<DomainPattern>),
    DeclaredAndUserApproved(Vec<DomainPattern>),
    PublicInternet,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub extension: ExtensionId,
    pub capability: CapabilityId,
    pub scope: CapabilityScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    pub granted: Vec<CapabilityGrant>,
    pub missing_optional: Vec<CapabilityId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_patterns_are_deliberately_narrow() {
        for valid in ["api.openalex.org", "*.semanticscholar.org"] {
            assert!(
                DomainPattern(valid.into()).is_canonical(),
                "rejected {valid}"
            );
        }
        for invalid in [
            "localhost",
            "HTTPS://example.org",
            "example.org:443",
            "-bad.org",
        ] {
            assert!(
                !DomainPattern(invalid.into()).is_canonical(),
                "accepted {invalid}"
            );
        }
    }
}
