use serde::{Deserialize, Serialize};

use crate::{
    CapabilityRequirements, CompatibleVersion, ContributionSet, DataValue, ExtensionId,
    ExtensionVersion, NativeAdapterId, PackagePath, PermissionRequest,
};

pub const CURRENT_MANIFEST_SCHEMA: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub schema_version: u16,
    pub id: ExtensionId,
    pub name: String,
    pub version: ExtensionVersion,
    pub publisher: Publisher,
    pub description: String,
    /// SPDX expression. License-policy interpretation belongs to the host.
    pub license: String,
    pub compatibility: HostCompatibility,
    pub entrypoint: ExtensionEntrypoint,
    #[serde(default)]
    pub dependencies: Vec<ExtensionDependency>,
    #[serde(default)]
    pub capabilities: CapabilityRequirements,
    #[serde(default)]
    pub permissions: Vec<PermissionRequest>,
    #[serde(default)]
    pub contributions: ContributionSet,
    #[serde(default)]
    pub settings: SettingsSchema,
    #[serde(default)]
    pub storage: StorageRequirements,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Publisher {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostCompatibility {
    pub extension_api: CompatibleVersion,
    pub minimum_host: Option<ExtensionVersion>,
    #[serde(default)]
    pub platforms: Vec<PlatformRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformRequirement {
    pub platform: Platform,
    pub minimum_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

/// Native entrypoints are host-registered built-ins only. An installer must
/// never treat `NativeBuiltin` as authority to load code from a package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionEntrypoint {
    Declarative {
        ui: PackagePath,
    },
    WasmComponent {
        component: PackagePath,
        world: String,
        ui: Option<PackagePath>,
    },
    NativeBuiltin {
        adapter: NativeAdapterId,
        ui: Option<PackagePath>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDependency {
    pub id: ExtensionId,
    pub version: CompatibleVersion,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageRequirements {
    #[serde(default)]
    pub settings_bytes: u64,
    #[serde(default)]
    pub document_bytes: u64,
    #[serde(default)]
    pub ephemeral_cache_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsSchema {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub fields: Vec<SettingDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingDefinition {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub value_type: SettingType,
    pub default: DataValue,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingType {
    Boolean,
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    String {
        maximum_bytes: Option<u32>,
    },
    Choice {
        options: Vec<SettingChoice>,
    },
    StringList {
        maximum_items: Option<u32>,
        maximum_item_bytes: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingChoice {
    pub label: String,
    pub value: DataValue,
}

impl SettingType {
    #[must_use]
    pub fn accepts(&self, value: &DataValue) -> bool {
        match (self, value) {
            (Self::Boolean, DataValue::Boolean(_)) => true,
            (Self::Integer { minimum, maximum }, DataValue::Integer(value)) => {
                minimum.is_none_or(|minimum| *value >= minimum)
                    && maximum.is_none_or(|maximum| *value <= maximum)
            }
            (Self::Number { minimum, maximum }, DataValue::Number(value)) => {
                value.is_finite()
                    && minimum.is_none_or(|minimum| *value >= minimum)
                    && maximum.is_none_or(|maximum| *value <= maximum)
            }
            (Self::String { maximum_bytes }, DataValue::String(value)) => {
                maximum_bytes.is_none_or(|maximum| value.len() <= maximum as usize)
            }
            (Self::Choice { options }, value) => {
                options.iter().any(|option| option.value == *value)
            }
            (
                Self::StringList {
                    maximum_items,
                    maximum_item_bytes,
                },
                DataValue::List(values),
            ) => {
                maximum_items.is_none_or(|maximum| values.len() <= maximum as usize)
                    && values.iter().all(|value| {
                        let DataValue::String(value) = value else {
                            return false;
                        };
                        maximum_item_bytes.is_none_or(|maximum| value.len() <= maximum as usize)
                    })
            }
            _ => false,
        }
    }
}
