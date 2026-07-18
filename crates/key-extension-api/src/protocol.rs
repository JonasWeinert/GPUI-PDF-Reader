use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    CapabilityId, CapabilitySnapshot, CommandId, ContributionId, DataValue, EffectId, ExtensionId,
    ExtensionVersion, NamespacedId, StorageArea, TaskId,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CauseId {
    pub high: u64,
    pub low: u64,
}

impl CauseId {
    pub const ZERO: Self = Self { high: 0, low: 0 };

    #[must_use]
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CauseContext {
    pub id: CauseId,
    pub parent: Option<CauseId>,
    pub depth: u16,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GenerationId(pub u64);

/// Opaque, generation-scoped authority. Hosts reject handles whose generation
/// no longer matches the document or extension session.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ResourceHandle {
    pub kind: String,
    pub generation: GenerationId,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Installed,
    Validated,
    Active,
    Suspended,
    Disabled,
    Failed,
    Unloading,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    Installed,
    Validated,
    Activated,
    ApplicationReady,
    DocumentOpening {
        generation: GenerationId,
    },
    DocumentOpened {
        generation: GenerationId,
    },
    DocumentClosing {
        generation: GenerationId,
    },
    DocumentClosed {
        generation: GenerationId,
    },
    Suspended {
        reason: String,
    },
    Resumed,
    SettingsChanged {
        keys: Vec<String>,
    },
    Upgrading {
        from: ExtensionVersion,
        to: ExtensionVersion,
    },
    Unloading,
}

#[must_use]
pub const fn lifecycle_transition_allowed(from: LifecycleState, to: LifecycleState) -> bool {
    use LifecycleState::{
        Active, Disabled, Failed, Installed, Removed, Suspended, Unloading, Validated,
    };
    matches!(
        (from, to),
        (Installed, Validated | Disabled | Removed)
            | (Validated, Active | Disabled | Failed | Removed)
            | (Active, Suspended | Disabled | Failed | Unloading)
            | (Suspended, Active | Disabled | Failed | Unloading)
            | (Failed, Disabled | Validated | Removed)
            | (Disabled, Validated | Removed)
            | (Unloading, Disabled | Removed)
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum EventSource {
    Host,
    Extension(ExtensionId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub cause: CauseContext,
    pub source: EventSource,
    pub sequence: u64,
    pub event: ExtensionEvent,
}

/// One bounded state/effect update returned by either a trusted native adapter
/// or a sandboxed component. `state` replaces the extension's immutable UI
/// state snapshot atomically; omitting it retains the prior snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtensionUpdate {
    #[serde(default)]
    pub state: Option<BTreeMap<String, DataValue>>,
    #[serde(default)]
    pub effects: Vec<EffectRequest>,
}

impl ExtensionUpdate {
    #[must_use]
    pub fn with_effects(effects: Vec<EffectRequest>) -> Self {
        Self {
            state: None,
            effects,
        }
    }

    #[must_use]
    pub fn with_state(state: BTreeMap<String, DataValue>) -> Self {
        Self {
            state: Some(state),
            effects: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionEvent {
    Lifecycle {
        event: LifecycleEvent,
    },
    CommandInvoked {
        command: CommandId,
        payload: DataValue,
    },
    CapabilitiesChanged {
        snapshot: CapabilitySnapshot,
    },
    SnapshotChanged {
        snapshot: SnapshotKind,
        value: DataValue,
    },
    EffectCompleted {
        effect: EffectId,
        result: EffectResult,
    },
    TaskCancelled {
        task: TaskId,
        reason: String,
    },
    Custom {
        event: NamespacedId,
        payload: DataValue,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    Application,
    Document,
    Viewport,
    Selection,
    Annotation,
    Theme,
    Capabilities,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSubscription {
    Lifecycle,
    Commands,
    Capabilities,
    Snapshot(SnapshotKind),
    EffectResults,
    Custom(NamespacedId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectRequest {
    pub id: EffectId,
    pub cause: CauseContext,
    pub effect: ExtensionEffect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionEffect {
    /// Invoke one operation on a separately granted semantic capability.
    CapabilityCall {
        capability: CapabilityId,
        operation: String,
        input: DataValue,
    },
    OpenContribution {
        contribution: ContributionId,
    },
    CloseContribution {
        contribution: ContributionId,
    },
    StorageGet {
        area: StorageArea,
        key: String,
    },
    StoragePut {
        area: StorageArea,
        key: String,
        value: DataValue,
    },
    StorageDelete {
        area: StorageArea,
        key: String,
    },
    CopyText {
        text: String,
    },
    OpenBrowserUrl {
        url: String,
    },
    Notify {
        message: String,
        tone: NotificationTone,
    },
    Confirm {
        title: String,
        message: String,
        destructive: bool,
    },
    StartTask {
        task: TaskId,
        operation: String,
        input: DataValue,
    },
    CancelTask {
        task: TaskId,
    },
}

pub type EffectResult = Result<DataValue, ExtensionError>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionError {
    pub code: ExtensionErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionErrorCode {
    PermissionDenied,
    CapabilityUnavailable,
    InvalidRequest,
    StaleResource,
    NotFound,
    Conflict,
    QuotaExceeded,
    Cancelled,
    DeadlineExceeded,
    TemporarilyUnavailable,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTone {
    Informational,
    Success,
    Warning,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_rejects_skipping_validation() {
        assert!(!lifecycle_transition_allowed(
            LifecycleState::Installed,
            LifecycleState::Active
        ));
        assert!(lifecycle_transition_allowed(
            LifecycleState::Installed,
            LifecycleState::Validated
        ));
        assert!(lifecycle_transition_allowed(
            LifecycleState::Active,
            LifecycleState::Suspended
        ));
    }
}
