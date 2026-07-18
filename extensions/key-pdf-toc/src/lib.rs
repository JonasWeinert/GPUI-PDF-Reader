//! First-party native pilot for the pre-stable PDF extension contract.
//!
//! The application owns the visual TOC rail and sends bounded semantic
//! selections through [`navigate_command_id`]. This adapter owns no GPUI or
//! PDFium values. It resolves the active document and outline through host
//! capabilities, then requests navigation through the same effect arbitration
//! used by sandboxed extensions.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, str::FromStr};

use key_extension_api::{
    CURRENT_MANIFEST_SCHEMA, CapabilityRequest, CapabilityRequirements, CapabilityScope,
    CommandDefinition, CommandId, CompatibleVersion, ContributionSet, DataValue, EffectId,
    EffectRequest, EffectResult, EventEnvelope, EventSubscription, ExtensionEffect,
    ExtensionEntrypoint, ExtensionError, ExtensionErrorCode, ExtensionEvent, ExtensionId,
    ExtensionManifest, ExtensionVersion, HostCompatibility, NativeAdapterId, PermissionRequest,
    Publisher, SettingsSchema, StorageRequirements,
};
use key_extension_host::{NativeExtension, NativeExtensionAdapter, NativeUpdate};
use key_pdf_extension_api::{
    DocumentDestination, DocumentHandle, NavigationFocusRequest, NavigationRequest,
    OutlineSnapshot, PageIndex, PdfCapability, TextTargetHint,
};
use serde::{Serialize, de::DeserializeOwned};

/// Stable ID of the bundled native TOC pilot.
pub const TOC_EXTENSION_ID: &str = "com.jonasweinert.gpuipdf.toc";
/// Host-registered adapter selected by the bundled manifest.
pub const TOC_ADAPTER_ID: &str = "com.jonasweinert.gpuipdf.toc/native";
/// Command invoked by the trusted TOC presentation.
pub const TOC_NAVIGATE_COMMAND_ID: &str = "com.jonasweinert.gpuipdf.toc/navigate";

/// A bounded semantic selection from the host-owned TOC presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TocSelection {
    /// Outline title shown to the user.
    pub title: String,
    /// Zero-based destination page supplied by the PDF outline.
    pub page: PageIndex,
}

impl TocSelection {
    /// Creates a semantic selection. Payload validation also rejects an empty
    /// title when the command crosses the extension boundary.
    #[must_use]
    pub fn new(title: impl Into<String>, page: PageIndex) -> Self {
        Self {
            title: title.into(),
            page,
        }
    }

    /// Encodes this selection as the runtime-neutral command payload.
    #[must_use]
    pub fn into_payload(self) -> DataValue {
        DataValue::Record(BTreeMap::from([
            ("page".into(), DataValue::Integer(i64::from(self.page.0))),
            ("title".into(), DataValue::String(self.title)),
        ]))
    }

    fn from_payload(payload: &DataValue) -> Result<Self, ExtensionError> {
        let DataValue::Record(fields) = payload else {
            return Err(invalid_request("TOC navigation requires a record payload"));
        };
        let Some(DataValue::String(title)) = fields.get("title") else {
            return Err(invalid_request("TOC navigation requires a string title"));
        };
        if title.trim().is_empty() {
            return Err(invalid_request("TOC navigation title cannot be empty"));
        }
        let Some(DataValue::Integer(page)) = fields.get("page") else {
            return Err(invalid_request("TOC navigation requires an integer page"));
        };
        let page = u32::try_from(*page)
            .map(PageIndex)
            .map_err(|_| invalid_request("TOC navigation page must fit a zero-based u32"))?;
        Ok(Self {
            title: title.clone(),
            page,
        })
    }
}

/// Returns the parsed bundled extension ID.
#[must_use]
pub fn extension_id() -> ExtensionId {
    ExtensionId::parse(TOC_EXTENSION_ID).expect("static TOC extension ID is canonical")
}

/// Returns the parsed host-native adapter ID.
#[must_use]
pub fn native_adapter_id() -> NativeAdapterId {
    NativeAdapterId::parse(TOC_ADAPTER_ID).expect("static TOC adapter ID is canonical")
}

/// Returns the command presented by the native TOC pilot.
#[must_use]
pub fn navigate_command_id() -> CommandId {
    CommandId::parse(TOC_NAVIGATE_COMMAND_ID).expect("static TOC command ID is canonical")
}

/// Creates the host-registered adapter. Packages cannot provide this native
/// code; the application must explicitly register it before activation.
#[must_use]
pub fn native_adapter() -> NativeExtensionAdapter {
    NativeExtensionAdapter::new(native_adapter_id(), || {
        Box::new(TocExtension::default()) as Box<dyn NativeExtension>
    })
}

/// Creates the bundled manifest for host installation.
#[must_use]
pub fn manifest() -> ExtensionManifest {
    let command = navigate_command_id();
    let required_capabilities = [
        PdfCapability::DocumentMetadata,
        PdfCapability::Outline,
        PdfCapability::Navigation,
    ]
    .into_iter()
    .map(|capability| CapabilityRequest {
        id: capability.capability_id(),
        version: compatible("^0.1"),
        scope: CapabilityScope::ActiveDocument,
    })
    .collect();
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension_id(),
        name: "PDF outline navigation".into(),
        version: version("1.0.0"),
        publisher: Publisher {
            id: "jonasweinert".into(),
            name: "Jonas Weinert".into(),
        },
        description: "Bundled alternate TOC navigation using the permissioned PDF API".into(),
        license: "MIT".into(),
        compatibility: HostCompatibility {
            extension_api: compatible("^0.1"),
            minimum_host: Some(version("0.1.0")),
            platforms: Vec::new(),
        },
        entrypoint: ExtensionEntrypoint::NativeBuiltin {
            adapter: native_adapter_id(),
            ui: None,
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements {
            required: required_capabilities,
            optional: Vec::new(),
            provided: Vec::new(),
        },
        permissions: vec![
            PermissionRequest {
                permission: PdfCapability::Outline.required_permission(),
                reason: "Read the active document outline shown in the TOC rail".into(),
                required: true,
            },
            PermissionRequest {
                permission: PdfCapability::Navigation.required_permission(),
                reason: "Move the active PDF to the outline item selected by the user".into(),
                required: true,
            },
        ],
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: command,
                title: "Navigate PDF outline".into(),
                description: "Navigate to a semantic PDF outline destination".into(),
                category: "PDF Navigation".into(),
            }],
            ..ContributionSet::default()
        },
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

/// Native controller for the TOC pilot.
///
/// At most one capability effect is in flight. Repeated selections replace
/// `latest`, so rapid rail input is coalesced without an unbounded navigation
/// queue. If a newer selection arrives after navigation was requested, one
/// follow-up resolution begins when the current request completes.
#[derive(Default)]
pub struct TocExtension {
    latest: Option<TocSelection>,
    pending: Option<PendingCall>,
    next_effect: u64,
}

#[derive(Clone, Debug)]
enum PendingCall {
    ActiveDocument {
        effect: EffectId,
    },
    Outline {
        effect: EffectId,
    },
    Navigation {
        effect: EffectId,
        target: TocSelection,
    },
}

impl PendingCall {
    fn effect(&self) -> &EffectId {
        match self {
            Self::ActiveDocument { effect }
            | Self::Outline { effect }
            | Self::Navigation { effect, .. } => effect,
        }
    }
}

impl NativeExtension for TocExtension {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        vec![
            EventSubscription::Commands,
            EventSubscription::EffectResults,
        ]
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError> {
        match &event.event {
            ExtensionEvent::CommandInvoked { command, payload }
                if command == &navigate_command_id() =>
            {
                self.latest = Some(TocSelection::from_payload(payload)?);
                if self.pending.is_none() {
                    self.request_active_document(event)
                } else {
                    Ok(status_update("queued", None))
                }
            }
            ExtensionEvent::EffectCompleted { effect, result }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.effect() == effect) =>
            {
                self.complete_pending(event, result)
            }
            _ => Ok(NativeUpdate::default()),
        }
    }

    fn unload(&mut self) {
        self.latest = None;
        self.pending = None;
    }

    fn invalidate_document_scope(&mut self) {
        self.latest = None;
        self.pending = None;
    }
}

impl TocExtension {
    fn complete_pending(
        &mut self,
        event: &EventEnvelope,
        result: &EffectResult,
    ) -> Result<NativeUpdate, ExtensionError> {
        let pending = self
            .pending
            .take()
            .expect("completion is guarded by a matching pending call");
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.latest = None;
                return Ok(status_update("unavailable", Some(error.message.clone())));
            }
        };
        match pending {
            PendingCall::ActiveDocument { .. } => {
                let document: Option<DocumentHandle> = decode_value(value.clone())?;
                let Some(document) = document else {
                    self.latest = None;
                    return Ok(status_update(
                        "unavailable",
                        Some("No PDF document is active".into()),
                    ));
                };
                self.request_outline(event, document)
            }
            PendingCall::Outline { .. } => {
                let outline: OutlineSnapshot = decode_value(value.clone())?;
                let Some(target) = self.latest.clone() else {
                    return Ok(NativeUpdate::default());
                };
                self.request_navigation(event, outline, target)
            }
            PendingCall::Navigation { target, .. } => {
                // Decode the receipt so malformed host responses cannot be
                // silently accepted as a successful pilot navigation.
                let _: key_pdf_extension_api::NavigationReceipt = decode_value(value.clone())?;
                if self.latest.as_ref().is_some_and(|latest| latest != &target) {
                    self.request_active_document(event)
                } else {
                    self.latest = None;
                    Ok(status_update("idle", None))
                }
            }
        }
    }

    fn request_active_document(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<NativeUpdate, ExtensionError> {
        let effect = self.effect_id("active-document")?;
        self.pending = Some(PendingCall::ActiveDocument {
            effect: effect.clone(),
        });
        Ok(capability_update(
            effect,
            event,
            PdfCapability::DocumentMetadata,
            "active-document",
            DataValue::Null,
            "resolving",
        ))
    }

    fn request_outline(
        &mut self,
        event: &EventEnvelope,
        document: DocumentHandle,
    ) -> Result<NativeUpdate, ExtensionError> {
        let effect = self.effect_id("outline")?;
        self.pending = Some(PendingCall::Outline {
            effect: effect.clone(),
        });
        Ok(capability_update(
            effect,
            event,
            PdfCapability::Outline,
            "read",
            encode_value(&document)?,
            "resolving",
        ))
    }

    fn request_navigation(
        &mut self,
        event: &EventEnvelope,
        outline: OutlineSnapshot,
        target: TocSelection,
    ) -> Result<NativeUpdate, ExtensionError> {
        let mut destination = outline
            .entries
            .iter()
            .find(|entry| entry.title == target.title && entry.destination.page == target.page)
            .or_else(|| {
                outline
                    .entries
                    .iter()
                    .find(|entry| entry.title == target.title)
            })
            .map_or_else(
                || DocumentDestination::page(target.page),
                |entry| entry.destination.clone(),
            );
        if destination.text_hint.is_none() {
            destination.text_hint = Some(TextTargetHint {
                text: target.title.clone(),
                occurrence: 0,
            });
        }
        let request = NavigationRequest {
            document: outline.document,
            destination,
            placement: Default::default(),
            focus: Some(NavigationFocusRequest::default()),
        };
        let effect = self.effect_id("navigation")?;
        self.pending = Some(PendingCall::Navigation {
            effect: effect.clone(),
            target,
        });
        Ok(capability_update(
            effect,
            event,
            PdfCapability::Navigation,
            "navigate",
            encode_value(&request)?,
            "navigating",
        ))
    }

    fn effect_id(&mut self, operation: &str) -> Result<EffectId, ExtensionError> {
        self.next_effect = self
            .next_effect
            .checked_add(1)
            .ok_or_else(|| ExtensionError {
                code: ExtensionErrorCode::Internal,
                message: "TOC effect counter exhausted".into(),
                retryable: false,
            })?;
        EffectId::parse(format!(
            "{TOC_EXTENSION_ID}/{operation}-{}",
            self.next_effect
        ))
        .map_err(|error| ExtensionError {
            code: ExtensionErrorCode::Internal,
            message: format!("could not create a TOC effect ID: {error}"),
            retryable: false,
        })
    }
}

fn capability_update(
    effect: EffectId,
    event: &EventEnvelope,
    capability: PdfCapability,
    operation: &str,
    input: DataValue,
    status: &str,
) -> NativeUpdate {
    let mut update = status_update(status, None);
    update.effects.push(EffectRequest {
        id: effect,
        cause: event.cause,
        effect: ExtensionEffect::CapabilityCall {
            capability: capability.capability_id(),
            operation: operation.into(),
            input,
        },
    });
    update
}

fn status_update(status: &str, error: Option<String>) -> NativeUpdate {
    NativeUpdate::with_state(BTreeMap::from([
        ("status".into(), DataValue::String(status.into())),
        (
            "error".into(),
            error.map_or(DataValue::Null, DataValue::String),
        ),
    ]))
}

fn encode_value(value: &impl Serialize) -> Result<DataValue, ExtensionError> {
    serde_json::to_value(value)
        .map_err(|error| invalid_request(format!("could not encode PDF request: {error}")))
        .and_then(json_to_data_value)
}

fn decode_value<T: DeserializeOwned>(value: DataValue) -> Result<T, ExtensionError> {
    serde_json::from_value(data_value_to_json(value))
        .map_err(|error| invalid_request(format!("invalid PDF capability response: {error}")))
}

fn data_value_to_json(value: DataValue) -> serde_json::Value {
    match value {
        DataValue::Null => serde_json::Value::Null,
        DataValue::Boolean(value) => serde_json::Value::Bool(value),
        DataValue::Integer(value) => value.into(),
        DataValue::Number(value) => serde_json::Number::from_f64(value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        DataValue::String(value) => value.into(),
        DataValue::List(values) => {
            serde_json::Value::Array(values.into_iter().map(data_value_to_json).collect())
        }
        DataValue::Record(values) => serde_json::Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, data_value_to_json(value)))
                .collect(),
        ),
    }
}

fn json_to_data_value(value: serde_json::Value) -> Result<DataValue, ExtensionError> {
    match value {
        serde_json::Value::Null => Ok(DataValue::Null),
        serde_json::Value::Bool(value) => Ok(DataValue::Boolean(value)),
        serde_json::Value::Number(value) if value.as_i64().is_some() => Ok(DataValue::Integer(
            value.as_i64().expect("number was checked as i64"),
        )),
        serde_json::Value::Number(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(DataValue::Number)
            .ok_or_else(|| invalid_request("PDF request contains an unsupported number")),
        serde_json::Value::String(value) => Ok(DataValue::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_data_value)
            .collect::<Result<Vec<_>, _>>()
            .map(DataValue::List),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_data_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(DataValue::Record),
    }
}

fn invalid_request(message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code: ExtensionErrorCode::InvalidRequest,
        message: message.into(),
        retryable: false,
    }
}

fn version(value: &str) -> ExtensionVersion {
    ExtensionVersion::from_str(value).expect("static TOC version is valid")
}

fn compatible(value: &str) -> CompatibleVersion {
    CompatibleVersion::from_str(value).expect("static TOC compatibility range is valid")
}

#[cfg(test)]
mod tests {
    use key_extension_api::{
        CauseContext, CauseId, EventSource, ExtensionError, ExtensionErrorCode, GenerationId,
        Permission,
    };
    use key_extension_host::{ExtensionHost, HostConfig, PackageMetadata, PermissionDecision};
    use key_pdf_extension_api::{
        DocumentHandle, NavigationReceipt, OutlineEntry, OutlineEntryId, PDF_EXTENSION_API_VERSION,
        SnapshotRevision,
    };

    use super::*;

    fn document(generation: u64) -> DocumentHandle {
        DocumentHandle::from_host_parts(GenerationId(generation), 1).expect("valid document")
    }

    fn outline(document: DocumentHandle) -> OutlineSnapshot {
        OutlineSnapshot {
            document,
            revision: SnapshotRevision(1),
            entries: vec![OutlineEntry {
                id: OutlineEntryId(7),
                parent: None,
                depth: 0,
                title: "Results".into(),
                destination: DocumentDestination::page(PageIndex(3)),
            }],
            truncated: false,
        }
    }

    fn receipt(request: &NavigationRequest) -> NavigationReceipt {
        NavigationReceipt {
            document: request.document,
            viewport_revision: SnapshotRevision(2),
            destination: request.destination.clone(),
            focus_scheduled: request.focus.is_some(),
        }
    }

    fn configured_host() -> ExtensionHost {
        let mut host = ExtensionHost::new(HostConfig::default());
        for capability in [
            PdfCapability::DocumentMetadata,
            PdfCapability::Outline,
            PdfCapability::Navigation,
        ] {
            let id = capability.capability_id();
            host.register_host_capability(id.clone(), version(PDF_EXTENSION_API_VERSION));
            host.require_capability_permission(id, capability.required_permission());
        }
        host.register_native_adapter(native_adapter());
        let package = manifest();
        host.install(package.clone(), PackageMetadata::bundled())
            .expect("manifest installs");
        for request in package.permissions {
            host.set_permission_decision(
                package.id.clone(),
                request.permission,
                PermissionDecision::Granted,
            );
        }
        host.activate(&package.id).expect("pilot activates");
        host
    }

    fn one_effect(host: &mut ExtensionHost) -> key_extension_host::ArbitratedEffect {
        let report = host.process_tick();
        assert_eq!(report.effects.len(), 1, "expected one bounded effect");
        report.effects.into_iter().next().expect("one effect")
    }

    fn complete(
        host: &mut ExtensionHost,
        effect: &key_extension_host::ArbitratedEffect,
        value: DataValue,
    ) {
        host.complete_effect(effect, Ok(value))
            .expect("effect remains pending");
    }

    #[test]
    fn host_manages_the_complete_outline_to_navigation_capability_chain() {
        let mut host = configured_host();
        let command = navigate_command_id();
        host.invoke_command(
            &command,
            TocSelection::new("Results", PageIndex(3)).into_payload(),
        )
        .expect("declared command queues");

        let active = one_effect(&mut host);
        assert_capability(&active, PdfCapability::DocumentMetadata, "active-document");
        let document = document(11);
        complete(
            &mut host,
            &active,
            encode_value(&Some(document)).expect("document encodes"),
        );

        let read_outline = one_effect(&mut host);
        assert_capability(&read_outline, PdfCapability::Outline, "read");
        complete(
            &mut host,
            &read_outline,
            encode_value(&outline(document)).expect("outline encodes"),
        );

        let navigation = one_effect(&mut host);
        assert_capability(&navigation, PdfCapability::Navigation, "navigate");
        let ExtensionEffect::CapabilityCall { input, .. } = &navigation.request.effect else {
            panic!("navigation must remain a semantic capability call");
        };
        let request: NavigationRequest = decode_value(input.clone()).expect("request decodes");
        assert_eq!(request.document, document);
        assert_eq!(request.destination.page, PageIndex(3));
        assert_eq!(
            request.destination.text_hint,
            Some(TextTargetHint {
                text: "Results".into(),
                occurrence: 0,
            })
        );
        assert_eq!(request.placement.viewport_anchor_y, 0.5);
        assert!(request.focus.is_some());
        complete(
            &mut host,
            &navigation,
            encode_value(&receipt(&request)).expect("receipt encodes"),
        );
        assert!(host.process_tick().effects.is_empty());
        assert_eq!(
            host.extension_state(&extension_id())
                .and_then(|state| state.get("status")),
            Some(&DataValue::String("idle".into()))
        );
    }

    #[test]
    fn rapid_selections_are_coalesced_to_the_latest_target() {
        let mut host = configured_host();
        let command = navigate_command_id();
        host.invoke_command(
            &command,
            TocSelection::new("Introduction", PageIndex(0)).into_payload(),
        )
        .unwrap();
        let active = one_effect(&mut host);

        host.invoke_command(
            &command,
            TocSelection::new("Results", PageIndex(3)).into_payload(),
        )
        .unwrap();
        assert!(host.process_tick().effects.is_empty());

        let document = document(4);
        complete(&mut host, &active, encode_value(&Some(document)).unwrap());
        let read_outline = one_effect(&mut host);
        complete(
            &mut host,
            &read_outline,
            encode_value(&outline(document)).unwrap(),
        );
        let navigation = one_effect(&mut host);
        let ExtensionEffect::CapabilityCall { input, .. } = &navigation.request.effect else {
            panic!("expected navigation call");
        };
        let request: NavigationRequest = decode_value(input.clone()).unwrap();
        assert_eq!(request.destination.page, PageIndex(3));
        assert_eq!(
            request
                .destination
                .text_hint
                .as_ref()
                .map(|hint| hint.text.as_str()),
            Some("Results")
        );
    }

    #[test]
    fn missing_bounded_outline_entry_falls_back_to_page_and_text_hint() {
        let mut extension = TocExtension::default();
        let command_event = event(ExtensionEvent::CommandInvoked {
            command: navigate_command_id(),
            payload: TocSelection::new("Appendix", PageIndex(9)).into_payload(),
        });
        let update = extension.handle_event(&command_event).unwrap();
        let active = update.effects[0].id.clone();
        let document = document(2);
        let update = extension
            .handle_event(&event(ExtensionEvent::EffectCompleted {
                effect: active,
                result: Ok(encode_value(&Some(document)).unwrap()),
            }))
            .unwrap();
        let outline_effect = update.effects[0].id.clone();
        let update = extension
            .handle_event(&event(ExtensionEvent::EffectCompleted {
                effect: outline_effect,
                result: Ok(encode_value(&outline(document)).unwrap()),
            }))
            .unwrap();
        let ExtensionEffect::CapabilityCall { input, .. } = &update.effects[0].effect else {
            panic!("expected navigation capability");
        };
        let request: NavigationRequest = decode_value(input.clone()).unwrap();
        assert_eq!(request.destination.page, PageIndex(9));
        assert_eq!(request.destination.y_fraction, None);
        assert_eq!(request.destination.text_hint.unwrap().text, "Appendix");
    }

    #[test]
    fn malformed_commands_and_capability_failures_do_not_leave_work_pending() {
        let mut extension = TocExtension::default();
        let malformed = event(ExtensionEvent::CommandInvoked {
            command: navigate_command_id(),
            payload: DataValue::Null,
        });
        assert_eq!(
            extension.handle_event(&malformed).unwrap_err().code,
            ExtensionErrorCode::InvalidRequest
        );

        let valid = event(ExtensionEvent::CommandInvoked {
            command: navigate_command_id(),
            payload: TocSelection::new("Results", PageIndex(3)).into_payload(),
        });
        let update = extension.handle_event(&valid).unwrap();
        let effect = update.effects[0].id.clone();
        let failure = extension
            .handle_event(&event(ExtensionEvent::EffectCompleted {
                effect,
                result: Err(ExtensionError {
                    code: ExtensionErrorCode::StaleResource,
                    message: "document was replaced".into(),
                    retryable: false,
                }),
            }))
            .unwrap();
        assert!(failure.effects.is_empty());

        let retry = extension.handle_event(&valid).unwrap();
        assert_eq!(
            retry.effects.len(),
            1,
            "failure must release the in-flight slot"
        );
    }

    #[test]
    fn document_invalidation_releases_pending_authority_and_rejects_old_completion() {
        let mut host = configured_host();
        let command = navigate_command_id();
        let selection = TocSelection::new("Results", PageIndex(3)).into_payload();
        host.invoke_command(&command, selection.clone()).unwrap();
        let stale = one_effect(&mut host);

        host.invalidate_document_scope([]);
        assert!(
            host.complete_effect(&stale, Ok(encode_value(&Some(document(2))).unwrap()))
                .is_err(),
            "the old document's effect authority must be invalidated"
        );

        host.invoke_command(&command, selection).unwrap();
        let fresh = one_effect(&mut host);
        assert_capability(&fresh, PdfCapability::DocumentMetadata, "active-document");
        assert_ne!(fresh.request.id, stale.request.id);
    }

    #[test]
    fn manifest_requests_only_the_authority_used_by_the_pilot() {
        let manifest = manifest();
        manifest
            .validate()
            .expect("bundled pilot manifest is valid");
        assert_eq!(
            manifest
                .capabilities
                .required
                .iter()
                .map(|request| request.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                PdfCapability::DocumentMetadata.as_str(),
                PdfCapability::Outline.as_str(),
                PdfCapability::Navigation.as_str(),
            ]
        );
        assert_eq!(
            manifest
                .permissions
                .iter()
                .map(|request| request.permission.clone())
                .collect::<Vec<_>>(),
            vec![
                Permission::ReadDocumentMetadata,
                Permission::NavigateDocument
            ]
        );
    }

    fn assert_capability(
        effect: &key_extension_host::ArbitratedEffect,
        capability: PdfCapability,
        operation: &str,
    ) {
        let ExtensionEffect::CapabilityCall {
            capability: actual,
            operation: actual_operation,
            ..
        } = &effect.request.effect
        else {
            panic!("expected a capability call");
        };
        assert_eq!(actual, &capability.capability_id());
        assert_eq!(actual_operation, operation);
    }

    fn event(event: ExtensionEvent) -> EventEnvelope {
        EventEnvelope {
            cause: CauseContext {
                id: CauseId::new(1, 1),
                parent: None,
                depth: 0,
            },
            source: EventSource::Host,
            sequence: 1,
            event,
        }
    }
}
