use std::collections::VecDeque;

use key_extension_api::ExtensionId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    PackageInstalled,
    PackageReplaced,
    ValidationFailed,
    DependencyUnavailable,
    CapabilityUnavailable,
    PermissionRequired,
    PermissionDenied,
    AdapterUnavailable,
    UnsupportedEntrypoint,
    Activated,
    ActivationFailed,
    EventDropped,
    EventBatchLimited,
    DispatchDepthExceeded,
    FeedbackLoopDetected,
    EffectRejected,
    EffectCoalesced,
    ExtensionFault,
    ExtensionSuspended,
    ExtensionResumed,
    ExtensionUnloaded,
    SafeModeSkipped,
    ContributionOrderFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostDiagnostic {
    pub sequence: u64,
    pub extension: Option<ExtensionId>,
    pub severity: DiagnosticSeverity,
    pub code: DiagnosticCode,
    pub message: String,
}

#[derive(Debug)]
pub(crate) struct DiagnosticLog {
    capacity: usize,
    next_sequence: u64,
    entries: VecDeque<HostDiagnostic>,
}

impl DiagnosticLog {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_sequence: 1,
            entries: VecDeque::new(),
        }
    }

    pub(crate) fn push(
        &mut self,
        extension: Option<ExtensionId>,
        severity: DiagnosticSeverity,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(HostDiagnostic {
            sequence: self.next_sequence,
            extension,
            severity,
            code,
            message: message.into(),
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
    }

    pub(crate) fn snapshot(&self) -> Vec<HostDiagnostic> {
        self.entries.iter().cloned().collect()
    }

    pub(crate) fn drain(&mut self) -> Vec<HostDiagnostic> {
        self.entries.drain(..).collect()
    }
}
