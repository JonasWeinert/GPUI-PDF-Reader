use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WasmStage {
    Configuration,
    Validation,
    Compilation,
    Instantiation,
    Activation,
    Invocation,
    Deactivation,
    Unload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WasmDiagnosticCode {
    RuntimeUnavailable,
    InvalidConfiguration,
    ComponentTooLarge,
    InvalidComponent,
    MissingImport,
    InstantiationFailed,
    InvalidLifecycle,
    MissingExport,
    SignatureMismatch,
    FuelExhausted,
    DeadlineExceeded,
    Trap,
    MemoryLimitExceeded,
    TableLimitExceeded,
    ResourceLimitExceeded,
    HostCallLimitExceeded,
    InputLimitExceeded,
    OutputLimitExceeded,
    Serialization,
    TooManyEffects,
    GuestRejected,
    Unloaded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmDiagnostic {
    pub code: WasmDiagnosticCode,
    pub stage: WasmStage,
    pub message: String,
}

impl WasmDiagnostic {
    #[must_use]
    pub fn new(code: WasmDiagnosticCode, stage: WasmStage, message: impl Into<String>) -> Self {
        let mut message = message.into();
        const MAX_DIAGNOSTIC_BYTES: usize = 2_048;
        if message.len() > MAX_DIAGNOSTIC_BYTES {
            let mut boundary = MAX_DIAGNOSTIC_BYTES;
            while !message.is_char_boundary(boundary) {
                boundary -= 1;
            }
            message.truncate(boundary);
            message.push('…');
        }
        Self {
            code,
            stage,
            message,
        }
    }
}

impl fmt::Display for WasmDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} during {:?}: {}",
            self.code, self.stage, self.message
        )
    }
}

impl Error for WasmDiagnostic {}
