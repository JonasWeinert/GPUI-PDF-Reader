use std::sync::Arc;

use key_extension_api::{
    EventEnvelope, EventSubscription, ExtensionEntrypoint, ExtensionError, ExtensionErrorCode,
    ExtensionId, PackagePath,
};
use key_extension_host::{ActivationContext, NativeExtension, NativeUpdate, WasmExtensionAdapter};

use crate::{WasmDiagnostic, WasmDiagnosticCode, WasmRuntime};

/// Immutable package identity and event contract bound to verified component
/// bytes by the installer. The host checks the manifest entrypoint again at
/// activation so a package replacement cannot reuse stale compiled code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmHostAdapterConfig {
    pub extension: ExtensionId,
    pub component: PackagePath,
    pub world: String,
    pub subscriptions: Vec<EventSubscription>,
}

/// Compiles verified bytes once and creates a package-scoped host adapter.
/// No WASI or host imports are linked by this transport runtime.
pub fn compile_host_adapter(
    runtime: &WasmRuntime,
    config: WasmHostAdapterConfig,
    component_bytes: &[u8],
) -> Result<WasmExtensionAdapter, WasmDiagnostic> {
    let compiled = Arc::new(runtime.compile(component_bytes)?);
    let extension = config.extension.clone();
    Ok(WasmExtensionAdapter::new(
        extension,
        move |entrypoint: &ExtensionEntrypoint| {
            validate_entrypoint(entrypoint, &config)?;
            let instance = compiled
                .instantiate()
                .map_err(extension_error_from_diagnostic)?;
            Ok(Box::new(WasmHostExtension {
                instance,
                subscriptions: config.subscriptions.clone(),
            }) as Box<dyn NativeExtension>)
        },
    ))
}

fn validate_entrypoint(
    entrypoint: &ExtensionEntrypoint,
    config: &WasmHostAdapterConfig,
) -> Result<(), ExtensionError> {
    match entrypoint {
        ExtensionEntrypoint::WasmComponent {
            component, world, ..
        } if component == &config.component && world == &config.world => Ok(()),
        ExtensionEntrypoint::WasmComponent { .. } => Err(ExtensionError {
            code: ExtensionErrorCode::Conflict,
            message: "registered component bytes do not match the active package manifest".into(),
            retryable: false,
        }),
        _ => Err(ExtensionError {
            code: ExtensionErrorCode::InvalidRequest,
            message: "WebAssembly adapter received a non-component entrypoint".into(),
            retryable: false,
        }),
    }
}

struct WasmHostExtension {
    instance: crate::WasmExtensionInstance,
    subscriptions: Vec<EventSubscription>,
}

impl NativeExtension for WasmHostExtension {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        self.subscriptions.clone()
    }

    fn activate(&mut self, _context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        self.instance
            .activate()
            .map_err(extension_error_from_diagnostic)?;
        Ok(NativeUpdate::default())
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError> {
        self.instance
            .dispatch(event)
            .map(NativeUpdate::with_effects)
            .map_err(extension_error_from_diagnostic)
    }

    fn suspend(&mut self, _reason: &str) -> Result<(), ExtensionError> {
        self.instance
            .suspend()
            .map_err(extension_error_from_diagnostic)
    }

    fn resume(&mut self, _context: &ActivationContext) -> Result<NativeUpdate, ExtensionError> {
        self.instance
            .resume()
            .map_err(extension_error_from_diagnostic)?;
        Ok(NativeUpdate::default())
    }

    fn unload(&mut self) {
        let _ = self.instance.unload();
    }
}

fn extension_error_from_diagnostic(diagnostic: WasmDiagnostic) -> ExtensionError {
    let (code, retryable) = match diagnostic.code {
        WasmDiagnosticCode::FuelExhausted
        | WasmDiagnosticCode::MemoryLimitExceeded
        | WasmDiagnosticCode::TableLimitExceeded
        | WasmDiagnosticCode::ResourceLimitExceeded
        | WasmDiagnosticCode::HostCallLimitExceeded
        | WasmDiagnosticCode::InputLimitExceeded
        | WasmDiagnosticCode::OutputLimitExceeded
        | WasmDiagnosticCode::TooManyEffects
        | WasmDiagnosticCode::ComponentTooLarge => (ExtensionErrorCode::QuotaExceeded, false),
        WasmDiagnosticCode::DeadlineExceeded => (ExtensionErrorCode::DeadlineExceeded, true),
        WasmDiagnosticCode::InvalidConfiguration
        | WasmDiagnosticCode::InvalidComponent
        | WasmDiagnosticCode::MissingImport
        | WasmDiagnosticCode::MissingExport
        | WasmDiagnosticCode::SignatureMismatch
        | WasmDiagnosticCode::Serialization
        | WasmDiagnosticCode::GuestRejected => (ExtensionErrorCode::InvalidRequest, false),
        WasmDiagnosticCode::InvalidLifecycle | WasmDiagnosticCode::Unloaded => {
            (ExtensionErrorCode::Conflict, false)
        }
        WasmDiagnosticCode::RuntimeUnavailable
        | WasmDiagnosticCode::InstantiationFailed
        | WasmDiagnosticCode::Trap => (ExtensionErrorCode::Internal, false),
    };
    ExtensionError {
        code,
        message: diagnostic.to_string(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WasmDiagnostic, WasmStage};

    #[test]
    fn resource_failures_map_to_quota_errors() {
        let error = extension_error_from_diagnostic(WasmDiagnostic::new(
            WasmDiagnosticCode::MemoryLimitExceeded,
            WasmStage::Invocation,
            "too much memory",
        ));
        assert_eq!(error.code, ExtensionErrorCode::QuotaExceeded);
        assert!(!error.retryable);
    }

    #[test]
    fn manifest_binding_rejects_stale_component_identity() {
        let config = WasmHostAdapterConfig {
            extension: ExtensionId::parse("org.example.stats").unwrap(),
            component: PackagePath::parse("stats.wasm").unwrap(),
            world: "key:stats/extension@0.1.0".into(),
            subscriptions: Vec::new(),
        };
        let wrong = ExtensionEntrypoint::WasmComponent {
            component: PackagePath::parse("replaced.wasm").unwrap(),
            world: config.world.clone(),
            ui: None,
        };
        assert_eq!(
            validate_entrypoint(&wrong, &config).unwrap_err().code,
            ExtensionErrorCode::Conflict
        );
    }
}
