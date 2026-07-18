use std::sync::Arc;

use key_extension_api::{
    EventSubscription, ExtensionEntrypoint, ExtensionError, ExtensionErrorCode, ExtensionId,
    PackagePath,
};
use key_extension_host::{NativeExtension, WasmExtensionAdapter};

use crate::worker::WasmWorkerExtension;
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

/// Creates a package-scoped adapter over verified immutable bytes.
///
/// Despite the compatibility-preserving name, component compilation is
/// intentionally deferred to the adapter's dedicated worker. Constructing and
/// activating this adapter from a GPUI callback therefore never compiles,
/// instantiates, or calls untrusted code. No WASI or host imports are linked by
/// this transport runtime.
pub fn compile_host_adapter(
    runtime: &WasmRuntime,
    config: WasmHostAdapterConfig,
    component_bytes: &[u8],
) -> Result<WasmExtensionAdapter, WasmDiagnostic> {
    if component_bytes.len() > runtime.limits().maximum_component_bytes {
        return Err(WasmDiagnostic::new(
            WasmDiagnosticCode::ComponentTooLarge,
            crate::WasmStage::Validation,
            format!(
                "component is {} bytes; limit is {} bytes",
                component_bytes.len(),
                runtime.limits().maximum_component_bytes
            ),
        ));
    }
    let component_bytes: Arc<[u8]> = Arc::from(component_bytes);
    let runtime = runtime.clone();
    let extension = config.extension.clone();
    Ok(WasmExtensionAdapter::new(
        extension,
        move |entrypoint: &ExtensionEntrypoint| {
            validate_entrypoint(entrypoint, &config)?;
            let instance = WasmWorkerExtension::spawn(
                runtime.clone(),
                Arc::clone(&component_bytes),
                config.subscriptions.clone(),
            )?;
            Ok(Box::new(instance) as Box<dyn NativeExtension>)
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

pub(crate) fn extension_error_from_diagnostic(diagnostic: WasmDiagnostic) -> ExtensionError {
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
