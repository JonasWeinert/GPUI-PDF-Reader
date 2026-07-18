use std::{collections::BTreeMap, error::Error, fmt};

use key_extension_api::{
    ExtensionId, ExtensionManifest, LifecycleState, ValidationError, ValidationLimits,
    lifecycle_transition_allowed, validate_dependency_graph,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageOrigin {
    Bundled,
    TrustedLocal,
    ThirdParty,
}

impl PackageOrigin {
    #[must_use]
    pub const fn allowed_in_safe_mode(self) -> bool {
        matches!(self, Self::Bundled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageMetadata {
    pub origin: PackageOrigin,
    pub content_hash: Option<String>,
    pub publisher_verified: bool,
}

impl PackageMetadata {
    #[must_use]
    pub const fn bundled() -> Self {
        Self {
            origin: PackageOrigin::Bundled,
            content_hash: None,
            publisher_verified: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PackageRecord {
    manifest: ExtensionManifest,
    metadata: PackageMetadata,
    state: LifecycleState,
    validation_errors: Vec<ValidationError>,
}

impl PackageRecord {
    #[must_use]
    pub fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    #[must_use]
    pub const fn metadata(&self) -> &PackageMetadata {
        &self.metadata
    }

    #[must_use]
    pub const fn state(&self) -> LifecycleState {
        self.state
    }

    #[must_use]
    pub fn validation_errors(&self) -> &[ValidationError] {
        &self.validation_errors
    }

    pub(crate) fn transition(&mut self, state: LifecycleState) -> Result<(), RegistryError> {
        if !lifecycle_transition_allowed(self.state, state) {
            return Err(RegistryError::InvalidTransition {
                extension: self.manifest.id.clone(),
                from: self.state,
                to: state,
            });
        }
        self.state = state;
        Ok(())
    }

    pub(crate) fn set_validation_errors(&mut self, errors: Vec<ValidationError>) {
        self.validation_errors = errors;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RegistryError {
    AlreadyInstalled(ExtensionId),
    NotInstalled(ExtensionId),
    PackageBusy(ExtensionId),
    InvalidManifest {
        extension: ExtensionId,
        errors: Vec<ValidationError>,
    },
    InvalidTransition {
        extension: ExtensionId,
        from: LifecycleState,
        to: LifecycleState,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled(id) => write!(formatter, "extension {id} is already installed"),
            Self::NotInstalled(id) => write!(formatter, "extension {id} is not installed"),
            Self::PackageBusy(id) => write!(formatter, "extension {id} must be unloaded first"),
            Self::InvalidManifest { extension, errors } => write!(
                formatter,
                "extension {extension} has {} validation error(s)",
                errors.len()
            ),
            Self::InvalidTransition {
                extension,
                from,
                to,
            } => write!(
                formatter,
                "extension {extension} cannot transition from {from:?} to {to:?}"
            ),
        }
    }
}

impl Error for RegistryError {}

#[derive(Debug)]
pub struct PackageRegistry {
    limits: ValidationLimits,
    records: BTreeMap<ExtensionId, PackageRecord>,
}

impl PackageRegistry {
    #[must_use]
    pub fn new(limits: ValidationLimits) -> Self {
        Self {
            limits,
            records: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> &ValidationLimits {
        &self.limits
    }

    pub fn install(
        &mut self,
        manifest: ExtensionManifest,
        metadata: PackageMetadata,
    ) -> Result<(), RegistryError> {
        let id = manifest.id.clone();
        if self.records.contains_key(&id) {
            return Err(RegistryError::AlreadyInstalled(id));
        }
        if let Err(errors) = manifest.validate_with(&self.limits) {
            return Err(RegistryError::InvalidManifest {
                extension: id,
                errors: errors.into_vec(),
            });
        }
        let mut record = PackageRecord {
            manifest,
            metadata,
            state: LifecycleState::Installed,
            validation_errors: Vec::new(),
        };
        record.transition(LifecycleState::Validated)?;
        self.records.insert(id, record);
        Ok(())
    }

    pub fn replace(
        &mut self,
        manifest: ExtensionManifest,
        metadata: PackageMetadata,
    ) -> Result<(), RegistryError> {
        let id = manifest.id.clone();
        if let Some(record) = self.records.get(&id)
            && matches!(
                record.state,
                LifecycleState::Active | LifecycleState::Suspended | LifecycleState::Unloading
            )
        {
            return Err(RegistryError::PackageBusy(id));
        }
        if let Err(errors) = manifest.validate_with(&self.limits) {
            return Err(RegistryError::InvalidManifest {
                extension: id,
                errors: errors.into_vec(),
            });
        }
        self.records.insert(
            id,
            PackageRecord {
                manifest,
                metadata,
                state: LifecycleState::Validated,
                validation_errors: Vec::new(),
            },
        );
        Ok(())
    }

    /// Revalidates the complete installed dependency graph and annotates every
    /// affected record. Activation also performs a target-specific check.
    pub fn validate_installed_graph(&mut self) -> Result<(), Vec<ValidationError>> {
        let manifests = self
            .records
            .values()
            .filter(|record| record.state != LifecycleState::Removed)
            .map(|record| record.manifest.clone())
            .collect::<Vec<_>>();
        for record in self.records.values_mut() {
            record.validation_errors.clear();
        }
        match validate_dependency_graph(&manifests, &self.limits) {
            Ok(()) => Ok(()),
            Err(errors) => {
                let errors = errors.into_vec();
                for record in self.records.values_mut() {
                    record.set_validation_errors(errors.clone());
                }
                Err(errors)
            }
        }
    }

    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&PackageRecord> {
        self.records.get(id)
    }

    pub(crate) fn get_mut(&mut self, id: &ExtensionId) -> Option<&mut PackageRecord> {
        self.records.get_mut(id)
    }

    pub(crate) fn remove_record(&mut self, id: &ExtensionId) -> Option<PackageRecord> {
        self.records.remove(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ExtensionId, &PackageRecord)> {
        self.records.iter()
    }

    #[must_use]
    pub fn manifests(&self) -> Vec<ExtensionManifest> {
        self.records
            .values()
            .map(|record| record.manifest.clone())
            .collect()
    }
}
