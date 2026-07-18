//! Versioned WIT parsing and Rust/WIT semantic identity tests.

use std::path::PathBuf;

use key_extension_api::{
    BooleanSource, CapabilityGrant, CapabilityRequest, CapabilityRequirements, CapabilityScope,
    CapabilitySnapshot, CauseContext, CauseId, CommandBehavior, CommandBehaviorAction,
    CommandDefinition, ContributionOrder, ContributionSet, ContributionSlot, DataKind, DataValue,
    DocumentAccess, DocumentMutation, EXTENSION_API_VERSION, EffectRequest, EventEnvelope,
    EventSource, EventSubscription, ExtensionDependency, ExtensionEffect, ExtensionEntrypoint,
    ExtensionError, ExtensionErrorCode, ExtensionEvent, ExtensionManifest, ExtensionUpdate,
    GenerationId, HostCompatibility, IconRef, LifecycleEvent, LifecycleState, MenuContribution,
    MenuItem, MenuItemKind, MenuItemOrder, MetricFormat, NetworkScope, NotificationTone,
    Permission, PermissionRequest, Platform, PlatformRequirement, ProvidedCapability, Publisher,
    ResourceHandle, SelectOption, SettingChoice, SettingDefinition, SettingType, SettingsSchema,
    SnapshotKind, StorageArea, StoragePermission, StorageRequirements, TextEmphasis, TextSpan,
    UiContribution, UiNode, UiNodeKind, UiTab, UiTone, ValidationCode, ValidationError,
    ValidationLimits,
};
use wit_parser::{PackageId, Resolve, Type, TypeDefKind, WorldItem};

fn wit_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("wit")
        .join(format!("v{EXTENSION_API_VERSION}"))
}

fn load_wit() -> (Resolve, PackageId) {
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_path(wit_dir())
        .expect("versioned generic WIT package must parse");
    (resolve, package_id)
}

fn type_kind<'a>(resolve: &'a Resolve, package_id: PackageId, name: &str) -> &'a TypeDefKind {
    let types = resolve.packages[package_id].interfaces["types"];
    let type_id = resolve.interfaces[types].types[name];
    &resolve.types[type_id].kind
}

fn assert_cases(resolve: &Resolve, package_id: PackageId, name: &str, expected: &[&str]) {
    let actual = match type_kind(resolve, package_id, name) {
        TypeDefKind::Enum(cases) => cases
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        TypeDefKind::Variant(cases) => cases
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        kind => panic!("WIT type {name} must be an enum or variant, got {kind:?}"),
    };
    assert_eq!(actual, expected, "Rust/WIT cases diverged for {name}");
}

fn assert_record_fields(resolve: &Resolve, package_id: PackageId, name: &str, expected: &[&str]) {
    let TypeDefKind::Record(record) = type_kind(resolve, package_id, name) else {
        panic!("WIT type {name} must remain a record");
    };
    let actual = record
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "Rust/WIT fields diverged for {name}");
}

fn type_name(resolve: &Resolve, ty: Type) -> Option<&str> {
    let Type::Id(type_id) = ty else {
        return None;
    };
    resolve.types[type_id].name.as_deref()
}

fn type_label(resolve: &Resolve, ty: Type) -> &str {
    match ty {
        Type::Bool => "bool",
        Type::U8 => "u8",
        Type::U16 => "u16",
        Type::U32 => "u32",
        Type::U64 => "u64",
        Type::S8 => "s8",
        Type::S16 => "s16",
        Type::S32 => "s32",
        Type::S64 => "s64",
        Type::F32 => "f32",
        Type::F64 => "f64",
        Type::Char => "char",
        Type::String => "string",
        Type::ErrorContext => "error-context",
        Type::Id(type_id) => resolve.types[type_id]
            .name
            .as_deref()
            .unwrap_or_else(|| resolve.types[type_id].kind.as_str()),
    }
}

fn type_shape(resolve: &Resolve, ty: Type) -> String {
    let Type::Id(type_id) = ty else {
        return type_label(resolve, ty).to_owned();
    };
    let definition = &resolve.types[type_id];
    if let Some(name) = &definition.name {
        return name.clone();
    }
    match &definition.kind {
        TypeDefKind::List(item) => format!("list<{}>", type_shape(resolve, *item)),
        TypeDefKind::Option(item) => format!("option<{}>", type_shape(resolve, *item)),
        TypeDefKind::Result(result) => format!(
            "result<{},{}>",
            result
                .ok
                .map_or_else(|| "_".into(), |ok| type_shape(resolve, ok)),
            result
                .err
                .map_or_else(|| "_".into(), |error| type_shape(resolve, error))
        ),
        TypeDefKind::Type(inner) => type_shape(resolve, *inner),
        kind => kind.as_str().to_owned(),
    }
}

fn assert_variant_payloads(
    resolve: &Resolve,
    package_id: PackageId,
    name: &str,
    expected: &[(&str, Option<&str>)],
) {
    let TypeDefKind::Variant(variant) = type_kind(resolve, package_id, name) else {
        panic!("WIT type {name} must remain a variant");
    };
    let actual = variant
        .cases
        .iter()
        .map(|case| {
            (
                case.name.as_str(),
                case.ty.map(|ty| type_label(resolve, ty)),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "WIT payload types diverged for {name}");
}

fn assert_record_field_types(
    resolve: &Resolve,
    package_id: PackageId,
    name: &str,
    expected: &[(&str, &str)],
) {
    let TypeDefKind::Record(record) = type_kind(resolve, package_id, name) else {
        panic!("WIT type {name} must remain a record");
    };
    let actual = record
        .fields
        .iter()
        .map(|field| (field.name.as_str(), type_shape(resolve, field.ty)))
        .collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(|(field, shape)| (*field, (*shape).to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "WIT field types diverged for {name}");
}

fn assert_named_type(resolve: &Resolve, ty: Type, expected: &str) {
    assert_eq!(type_name(resolve, ty), Some(expected));
}

fn assert_list_of_named_type(resolve: &Resolve, ty: Type, expected: &str) {
    let Type::Id(type_id) = ty else {
        panic!("expected a named WIT list type");
    };
    let TypeDefKind::List(item) = resolve.types[type_id].kind else {
        panic!("expected a WIT list type");
    };
    assert_named_type(resolve, item, expected);
}

fn assert_result_type(
    resolve: &Resolve,
    ty: Type,
    expected_ok: Option<&str>,
    expected_error: &str,
) {
    let Type::Id(type_id) = ty else {
        panic!("expected a named WIT result type");
    };
    let TypeDefKind::Result(result) = &resolve.types[type_id].kind else {
        panic!("expected a WIT result type");
    };
    assert_eq!(result.ok.and_then(|ok| type_name(resolve, ok)), expected_ok);
    assert_eq!(
        result.err.and_then(|error| type_name(resolve, error)),
        Some(expected_error)
    );
}

/// Produces the WIT cases from one exhaustive Rust enum match. Adding a Rust
/// case therefore makes this test fail to compile until the WIT mapping is
/// deliberately updated; removing or renaming a WIT case fails at runtime.
macro_rules! rust_cases {
    ($ty:ty, $value:ident, { $($pattern:pat => $case:literal),+ $(,)? }) => {{
        fn mapped_case($value: &$ty) -> &'static str {
            match $value {
                $($pattern => $case),+
            }
        }
        let _mapping_is_exhaustive: fn(&$ty) -> &'static str = mapped_case;
        [$($case),+]
    }};
}

/// A record pattern without `..` is a compile-time completeness assertion.
/// This complements field-name checks for Rust records whose WIT form is not
/// intentionally arena-normalized.
macro_rules! complete_record {
    ($ty:ident { $($field:ident),+ $(,)? }) => {
        let _record_shape = |value: $ty| {
            let $ty { $($field: _),+ } = value;
        };
    };
}

#[test]
fn versioned_semantic_wit_package_parses() {
    let (resolve, package_id) = load_wit();
    let package = &resolve.packages[package_id];

    assert_eq!(package.name.namespace, "key");
    assert_eq!(package.name.name, "extension-api");
    assert_eq!(
        package.name.version.as_ref().map(ToString::to_string),
        Some(EXTENSION_API_VERSION.into())
    );
    assert!(package.interfaces.contains_key("types"));
    assert!(package.interfaces.contains_key("guest"));
    assert!(package.worlds.contains_key("semantic-extension"));

    let guest = package.interfaces["guest"];
    let types = package.interfaces["types"];
    let world = &resolve.worlds[package.worlds["semantic-extension"]];
    assert_eq!(world.imports.len(), 1);
    assert!(
        world
            .imports
            .values()
            .all(|item| { matches!(item, WorldItem::Interface { id, .. } if *id == types) })
    );
    assert_eq!(world.exports.len(), 1);
    assert!(
        world
            .exports
            .values()
            .any(|item| { matches!(item, WorldItem::Interface { id, .. } if *id == guest) })
    );

    assert_eq!(
        resolve.interfaces[guest]
            .functions
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "subscriptions",
            "activate",
            "handle-event",
            "suspend",
            "resume",
            "unload",
        ]
    );
    let functions = &resolve.interfaces[guest].functions;
    assert!(functions["subscriptions"].params.is_empty());
    assert_list_of_named_type(
        &resolve,
        functions["subscriptions"]
            .result
            .expect("subscriptions returns a list"),
        "event-subscription",
    );
    for function_name in ["activate", "resume"] {
        let function = &functions[function_name];
        assert_eq!(function.params.len(), 1);
        assert_eq!(function.params[0].name, "context");
        assert_named_type(&resolve, function.params[0].ty, "activation-context");
        assert_result_type(
            &resolve,
            function.result.expect("activation returns a result"),
            Some("extension-update"),
            "extension-error",
        );
    }
    let handle_event = &functions["handle-event"];
    assert_eq!(handle_event.params.len(), 1);
    assert_eq!(handle_event.params[0].name, "event");
    assert_named_type(&resolve, handle_event.params[0].ty, "event-envelope");
    assert_result_type(
        &resolve,
        handle_event
            .result
            .expect("event handling returns a result"),
        Some("extension-update"),
        "extension-error",
    );
    let suspend = &functions["suspend"];
    assert_eq!(suspend.params.len(), 1);
    assert_eq!(suspend.params[0].name, "reason");
    assert_eq!(suspend.params[0].ty, Type::String);
    assert_result_type(
        &resolve,
        suspend.result.expect("suspension returns a result"),
        None,
        "extension-error",
    );
    assert!(functions["unload"].params.is_empty());
    assert!(functions["unload"].result.is_none());

    let source = std::fs::read_to_string(wit_dir().join("extension-api.wit"))
        .expect("semantic WIT source is readable");
    assert!(!source.contains("list<u8>"));
    let source = source.to_ascii_lowercase();
    for forbidden in ["json", "gpui", "pdfium", "wasmtime", "filesystem", "socket"] {
        assert!(
            !source.contains(forbidden),
            "semantic WIT leaked implementation term {forbidden}"
        );
    }
}

#[test]
fn protocol_and_lifecycle_cases_map_one_to_one() {
    let (resolve, package_id) = load_wit();

    let cases = rust_cases!(DataValue, value, {
        DataValue::Null => "null",
        DataValue::Boolean(_) => "boolean",
        DataValue::Integer(_) => "integer",
        DataValue::Number(_) => "number",
        DataValue::String(_) => "string",
        DataValue::List(_) => "list",
        DataValue::Record(_) => "record",
    });
    assert_cases(&resolve, package_id, "data-node", &cases);

    let cases = rust_cases!(DataKind, value, {
        DataKind::Null => "null",
        DataKind::Boolean => "boolean",
        DataKind::Integer => "integer",
        DataKind::Number => "number",
        DataKind::String => "string",
        DataKind::List => "list",
        DataKind::Record => "record",
    });
    assert_cases(&resolve, package_id, "data-kind", &cases);

    let cases = rust_cases!(BooleanSource, value, {
        BooleanSource::Constant(_) => "constant",
        BooleanSource::Binding(_) => "binding",
    });
    assert_cases(&resolve, package_id, "boolean-source", &cases);

    let cases = rust_cases!(LifecycleState, value, {
        LifecycleState::Installed => "installed",
        LifecycleState::Validated => "validated",
        LifecycleState::Active => "active",
        LifecycleState::Suspended => "suspended",
        LifecycleState::Disabled => "disabled",
        LifecycleState::Failed => "failed",
        LifecycleState::Unloading => "unloading",
        LifecycleState::Removed => "removed",
    });
    assert_cases(&resolve, package_id, "lifecycle-state", &cases);

    let cases = rust_cases!(LifecycleEvent, value, {
        LifecycleEvent::Installed => "installed",
        LifecycleEvent::Validated => "validated",
        LifecycleEvent::Activated => "activated",
        LifecycleEvent::ApplicationReady => "application-ready",
        LifecycleEvent::DocumentOpening { .. } => "document-opening",
        LifecycleEvent::DocumentOpened { .. } => "document-opened",
        LifecycleEvent::DocumentClosing { .. } => "document-closing",
        LifecycleEvent::DocumentClosed { .. } => "document-closed",
        LifecycleEvent::Suspended { .. } => "suspended",
        LifecycleEvent::Resumed => "resumed",
        LifecycleEvent::SettingsChanged { .. } => "settings-changed",
        LifecycleEvent::Upgrading { .. } => "upgrading",
        LifecycleEvent::Unloading => "unloading",
    });
    assert_cases(&resolve, package_id, "lifecycle-event", &cases);

    let cases = rust_cases!(EventSource, value, {
        EventSource::Host => "host",
        EventSource::Extension(_) => "extension",
    });
    assert_cases(&resolve, package_id, "event-source", &cases);

    let cases = rust_cases!(SnapshotKind, value, {
        SnapshotKind::Application => "application",
        SnapshotKind::Document => "document",
        SnapshotKind::Viewport => "viewport",
        SnapshotKind::Selection => "selection",
        SnapshotKind::Annotation => "annotation",
        SnapshotKind::Theme => "theme",
        SnapshotKind::Capabilities => "capabilities",
    });
    assert_cases(&resolve, package_id, "snapshot-kind", &cases);

    let cases = rust_cases!(EventSubscription, value, {
        EventSubscription::Lifecycle => "lifecycle",
        EventSubscription::Commands => "commands",
        EventSubscription::Capabilities => "capabilities",
        EventSubscription::Snapshot(_) => "snapshot",
        EventSubscription::EffectResults => "effect-results",
        EventSubscription::Custom(_) => "custom",
    });
    assert_cases(&resolve, package_id, "event-subscription", &cases);

    let cases = rust_cases!(ExtensionEvent, value, {
        ExtensionEvent::Lifecycle { .. } => "lifecycle",
        ExtensionEvent::CommandInvoked { .. } => "command-invoked",
        ExtensionEvent::CapabilitiesChanged { .. } => "capabilities-changed",
        ExtensionEvent::SnapshotChanged { .. } => "snapshot-changed",
        ExtensionEvent::EffectCompleted { .. } => "effect-completed",
        ExtensionEvent::TaskCancelled { .. } => "task-cancelled",
        ExtensionEvent::Custom { .. } => "custom",
    });
    assert_cases(&resolve, package_id, "extension-event", &cases);

    let cases = rust_cases!(ExtensionEffect, value, {
        ExtensionEffect::CapabilityCall { .. } => "capability-call",
        ExtensionEffect::OpenContribution { .. } => "open-contribution",
        ExtensionEffect::CloseContribution { .. } => "close-contribution",
        ExtensionEffect::StorageGet { .. } => "storage-get",
        ExtensionEffect::StoragePut { .. } => "storage-put",
        ExtensionEffect::StorageDelete { .. } => "storage-delete",
        ExtensionEffect::CopyText { .. } => "copy-text",
        ExtensionEffect::OpenBrowserUrl { .. } => "open-browser-url",
        ExtensionEffect::Notify { .. } => "notify",
        ExtensionEffect::Confirm { .. } => "confirm",
        ExtensionEffect::StartTask { .. } => "start-task",
        ExtensionEffect::CancelTask { .. } => "cancel-task",
    });
    assert_cases(&resolve, package_id, "extension-effect", &cases);

    let cases = rust_cases!(ExtensionErrorCode, value, {
        ExtensionErrorCode::PermissionDenied => "permission-denied",
        ExtensionErrorCode::CapabilityUnavailable => "capability-unavailable",
        ExtensionErrorCode::InvalidRequest => "invalid-request",
        ExtensionErrorCode::StaleResource => "stale-resource",
        ExtensionErrorCode::NotFound => "not-found",
        ExtensionErrorCode::Conflict => "conflict",
        ExtensionErrorCode::QuotaExceeded => "quota-exceeded",
        ExtensionErrorCode::Cancelled => "cancelled",
        ExtensionErrorCode::DeadlineExceeded => "deadline-exceeded",
        ExtensionErrorCode::TemporarilyUnavailable => "temporarily-unavailable",
        ExtensionErrorCode::Internal => "internal",
    });
    assert_cases(&resolve, package_id, "extension-error-code", &cases);

    let cases = rust_cases!(NotificationTone, value, {
        NotificationTone::Informational => "informational",
        NotificationTone::Success => "success",
        NotificationTone::Warning => "warning",
        NotificationTone::Error => "error",
    });
    assert_cases(&resolve, package_id, "notification-tone", &cases);
}

#[test]
fn capability_permission_and_manifest_cases_map_one_to_one() {
    let (resolve, package_id) = load_wit();

    let cases = rust_cases!(StorageArea, value, {
        StorageArea::Settings => "settings",
        StorageArea::Document => "document",
        StorageArea::EphemeralCache => "ephemeral-cache",
    });
    assert_cases(&resolve, package_id, "storage-area", &cases);

    let cases = rust_cases!(CapabilityScope, value, {
        CapabilityScope::Application => "application",
        CapabilityScope::ActiveDocument => "active-document",
        CapabilityScope::DocumentSet => "document-set",
        CapabilityScope::NamespacedStorage(_) => "namespaced-storage",
        CapabilityScope::Domains(_) => "domains",
    });
    assert_cases(&resolve, package_id, "capability-scope", &cases);

    let cases = rust_cases!(DocumentAccess, value, {
        DocumentAccess::ActiveDocument => "active-document",
        DocumentAccess::UserApprovedDocuments => "user-approved-documents",
    });
    assert_cases(&resolve, package_id, "document-access", &cases);

    let cases = rust_cases!(DocumentMutation, value, {
        DocumentMutation::Redact => "redact",
        DocumentMutation::EditAnnotations => "edit-annotations",
        DocumentMutation::RewriteDocument => "rewrite-document",
    });
    assert_cases(&resolve, package_id, "document-mutation", &cases);

    let cases = rust_cases!(NetworkScope, value, {
        NetworkScope::None => "none",
        NetworkScope::DeclaredDomains(_) => "declared-domains",
        NetworkScope::DeclaredAndUserApproved(_) => "declared-and-user-approved",
        NetworkScope::PublicInternet => "public-internet",
    });
    assert_cases(&resolve, package_id, "network-scope", &cases);

    let cases = rust_cases!(Permission, value, {
        Permission::ReadDocumentMetadata => "read-document-metadata",
        Permission::ReadDocumentText(_) => "read-document-text",
        Permission::ReadSelection => "read-selection",
        Permission::NavigateDocument => "navigate-document",
        Permission::AddDocumentOverlays => "add-document-overlays",
        Permission::AddSidePanel => "add-side-panel",
        Permission::ReadAnnotations => "read-annotations",
        Permission::WriteAnnotations => "write-annotations",
        Permission::MutateDocument(_) => "mutate-document",
        Permission::ClipboardWrite => "clipboard-write",
        Permission::OpenExternalUrl => "open-external-url",
        Permission::Storage(_) => "storage",
        Permission::Network(_) => "network",
    });
    assert_cases(&resolve, package_id, "permission", &cases);

    let cases = rust_cases!(Platform, value, {
        Platform::Macos => "macos",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    });
    assert_cases(&resolve, package_id, "platform", &cases);

    let cases = rust_cases!(ExtensionEntrypoint, value, {
        ExtensionEntrypoint::Declarative { .. } => "declarative",
        ExtensionEntrypoint::WasmComponent { .. } => "wasm-component",
        ExtensionEntrypoint::NativeBuiltin { .. } => "native-builtin",
    });
    assert_cases(&resolve, package_id, "extension-entrypoint", &cases);

    let cases = rust_cases!(SettingType, value, {
        SettingType::Boolean => "boolean",
        SettingType::Integer { .. } => "integer",
        SettingType::Number { .. } => "number",
        SettingType::String { .. } => "string",
        SettingType::Choice { .. } => "choice",
        SettingType::StringList { .. } => "string-list",
    });
    assert_cases(&resolve, package_id, "setting-type", &cases);

    let cases = rust_cases!(ValidationCode, value, {
        ValidationCode::UnsupportedSchema => "unsupported-schema",
        ValidationCode::InvalidIdentifier => "invalid-identifier",
        ValidationCode::InvalidPackagePath => "invalid-package-path",
        ValidationCode::InvalidEntrypoint => "invalid-entrypoint",
        ValidationCode::InvalidField => "invalid-field",
        ValidationCode::StringTooLong => "string-too-long",
        ValidationCode::LimitExceeded => "limit-exceeded",
        ValidationCode::Duplicate => "duplicate",
        ValidationCode::ForeignNamespace => "foreign-namespace",
        ValidationCode::UnknownCommand => "unknown-command",
        ValidationCode::UnknownContribution => "unknown-contribution",
        ValidationCode::MissingDependency => "missing-dependency",
        ValidationCode::IncompatibleDependency => "incompatible-dependency",
        ValidationCode::DependencyCycle => "dependency-cycle",
        ValidationCode::OrderingCycle => "ordering-cycle",
        ValidationCode::InvalidDomain => "invalid-domain",
        ValidationCode::InvalidSetting => "invalid-setting",
        ValidationCode::InvalidValue => "invalid-value",
    });
    assert_cases(&resolve, package_id, "validation-code", &cases);
}

#[test]
fn declarative_ui_and_contribution_cases_map_one_to_one() {
    let (resolve, package_id) = load_wit();

    let cases = rust_cases!(CommandBehaviorAction, value, {
        CommandBehaviorAction::SetState { .. } => "set-state",
        CommandBehaviorAction::OpenContribution { .. } => "open-contribution",
        CommandBehaviorAction::CloseContribution { .. } => "close-contribution",
    });
    assert_cases(&resolve, package_id, "command-behavior-action", &cases);

    let cases = rust_cases!(ContributionSlot, value, {
        ContributionSlot::CommandPalette => "command-palette",
        ContributionSlot::TopToolbar => "top-toolbar",
        ContributionSlot::PdfFloatingToolbar => "pdf-floating-toolbar",
        ContributionSlot::SelectionContextPill => "selection-context-pill",
        ContributionSlot::DocumentOverlay => "document-overlay",
        ContributionSlot::HoverCard => "hover-card",
        ContributionSlot::SidePanel => "side-panel",
        ContributionSlot::ContextMenu => "context-menu",
        ContributionSlot::StatusArea => "status-area",
        ContributionSlot::SettingsPanel => "settings-panel",
    });
    assert_cases(&resolve, package_id, "contribution-slot", &cases);

    let cases = rust_cases!(IconRef, value, {
        IconRef::Host(_) => "host",
        IconRef::Asset(_) => "asset",
    });
    assert_cases(&resolve, package_id, "icon-ref", &cases);

    let cases = rust_cases!(MenuItemKind, value, {
        MenuItemKind::Command { .. } => "command",
        MenuItemKind::Submenu { .. } => "submenu",
        MenuItemKind::Separator => "separator",
    });
    assert_cases(&resolve, package_id, "menu-item-kind", &cases);

    let cases = rust_cases!(TextEmphasis, value, {
        TextEmphasis::Normal => "normal",
        TextEmphasis::Muted => "muted",
        TextEmphasis::Strong => "strong",
        TextEmphasis::Code => "code",
    });
    assert_cases(&resolve, package_id, "text-emphasis", &cases);

    let cases = rust_cases!(MetricFormat, value, {
        MetricFormat::Integer => "integer",
        MetricFormat::Decimal => "decimal",
        MetricFormat::Percentage => "percentage",
        MetricFormat::Bytes => "bytes",
    });
    assert_cases(&resolve, package_id, "metric-format", &cases);

    let cases = rust_cases!(UiTone, value, {
        UiTone::Neutral => "neutral",
        UiTone::Accent => "accent",
        UiTone::Positive => "positive",
        UiTone::Caution => "caution",
        UiTone::Critical => "critical",
    });
    assert_cases(&resolve, package_id, "ui-tone", &cases);

    let cases = rust_cases!(UiNodeKind, value, {
        UiNodeKind::Column { .. } => "column",
        UiNodeKind::Row { .. } => "row",
        UiNodeKind::Stack { .. } => "stack",
        UiNodeKind::Text { .. } => "text",
        UiNodeKind::StyledText { .. } => "styled-text",
        UiNodeKind::Metric { .. } => "metric",
        UiNodeKind::Markdown { .. } => "markdown",
        UiNodeKind::Button { .. } => "button",
        UiNodeKind::IconButton { .. } => "icon-button",
        UiNodeKind::Toggle { .. } => "toggle",
        UiNodeKind::Select { .. } => "select",
        UiNodeKind::TextField { .. } => "text-field",
        UiNodeKind::List { .. } => "list",
        UiNodeKind::Tabs { .. } => "tabs",
        UiNodeKind::Badge { .. } => "badge",
        UiNodeKind::Divider => "divider",
        UiNodeKind::Image { .. } => "image",
        UiNodeKind::Progress { .. } => "progress",
        UiNodeKind::Spacer => "spacer",
    });
    assert_cases(&resolve, package_id, "ui-node-kind", &cases);
}

#[test]
fn semantic_variants_keep_their_typed_payloads() {
    let (resolve, package_id) = load_wit();

    for (name, payloads) in [
        (
            "data-node",
            &[
                ("null", None),
                ("boolean", Some("bool")),
                ("integer", Some("s64")),
                ("number", Some("f64")),
                ("string", Some("string")),
                ("list", Some("list")),
                ("record", Some("list")),
            ][..],
        ),
        (
            "boolean-source",
            &[
                ("constant", Some("bool")),
                ("binding", Some("state-binding")),
            ],
        ),
        (
            "lifecycle-event",
            &[
                ("installed", None),
                ("validated", None),
                ("activated", None),
                ("application-ready", None),
                ("document-opening", Some("lifecycle-document-event")),
                ("document-opened", Some("lifecycle-document-event")),
                ("document-closing", Some("lifecycle-document-event")),
                ("document-closed", Some("lifecycle-document-event")),
                ("suspended", Some("lifecycle-suspended-event")),
                ("resumed", None),
                ("settings-changed", Some("lifecycle-settings-changed-event")),
                ("upgrading", Some("lifecycle-upgrading-event")),
                ("unloading", None),
            ],
        ),
        (
            "event-source",
            &[("host", None), ("extension", Some("extension-id"))],
        ),
        (
            "event-subscription",
            &[
                ("lifecycle", None),
                ("commands", None),
                ("capabilities", None),
                ("snapshot", Some("snapshot-kind")),
                ("effect-results", None),
                ("custom", Some("namespaced-id")),
            ],
        ),
        (
            "extension-event",
            &[
                ("lifecycle", Some("lifecycle-extension-event")),
                ("command-invoked", Some("command-invoked-event")),
                ("capabilities-changed", Some("capabilities-changed-event")),
                ("snapshot-changed", Some("snapshot-changed-event")),
                ("effect-completed", Some("effect-completed-event")),
                ("task-cancelled", Some("task-cancelled-event")),
                ("custom", Some("custom-event")),
            ],
        ),
        (
            "extension-effect",
            &[
                ("capability-call", Some("capability-call-effect")),
                ("open-contribution", Some("contribution-effect")),
                ("close-contribution", Some("contribution-effect")),
                ("storage-get", Some("storage-get-effect")),
                ("storage-put", Some("storage-put-effect")),
                ("storage-delete", Some("storage-delete-effect")),
                ("copy-text", Some("copy-text-effect")),
                ("open-browser-url", Some("open-browser-url-effect")),
                ("notify", Some("notify-effect")),
                ("confirm", Some("confirm-effect")),
                ("start-task", Some("start-task-effect")),
                ("cancel-task", Some("cancel-task-effect")),
            ],
        ),
        (
            "capability-scope",
            &[
                ("application", None),
                ("active-document", None),
                ("document-set", None),
                ("namespaced-storage", Some("storage-area")),
                ("domains", Some("capability-domains")),
            ],
        ),
        (
            "network-scope",
            &[
                ("none", None),
                ("declared-domains", Some("network-domains")),
                ("declared-and-user-approved", Some("network-domains")),
                ("public-internet", None),
            ],
        ),
        (
            "permission",
            &[
                ("read-document-metadata", None),
                ("read-document-text", Some("document-access")),
                ("read-selection", None),
                ("navigate-document", None),
                ("add-document-overlays", None),
                ("add-side-panel", None),
                ("read-annotations", None),
                ("write-annotations", None),
                ("mutate-document", Some("document-mutation")),
                ("clipboard-write", None),
                ("open-external-url", None),
                ("storage", Some("storage-permission")),
                ("network", Some("network-scope")),
            ],
        ),
        (
            "extension-entrypoint",
            &[
                ("declarative", Some("declarative-entrypoint")),
                ("wasm-component", Some("wasm-component-entrypoint")),
                ("native-builtin", Some("native-builtin-entrypoint")),
            ],
        ),
        (
            "setting-type",
            &[
                ("boolean", None),
                ("integer", Some("integer-setting")),
                ("number", Some("number-setting")),
                ("string", Some("string-setting")),
                ("choice", Some("choice-setting")),
                ("string-list", Some("string-list-setting")),
            ],
        ),
        (
            "command-behavior-action",
            &[
                ("set-state", Some("set-state-behavior")),
                ("open-contribution", Some("contribution-behavior")),
                ("close-contribution", Some("contribution-behavior")),
            ],
        ),
        (
            "icon-ref",
            &[("host", Some("string")), ("asset", Some("package-path"))],
        ),
        (
            "menu-item-kind",
            &[
                ("command", Some("menu-command-item")),
                ("submenu", Some("menu-submenu-item")),
                ("separator", None),
            ],
        ),
        (
            "ui-node-kind",
            &[
                ("column", Some("ui-children")),
                ("row", Some("ui-children")),
                ("stack", Some("ui-children")),
                ("text", Some("text-node")),
                ("styled-text", Some("styled-text-node")),
                ("metric", Some("metric-node")),
                ("markdown", Some("markdown-node")),
                ("button", Some("button-node")),
                ("icon-button", Some("icon-button-node")),
                ("toggle", Some("toggle-node")),
                ("select", Some("select-node")),
                ("text-field", Some("text-field-node")),
                ("list", Some("ui-children")),
                ("tabs", Some("tabs-node")),
                ("badge", Some("badge-node")),
                ("divider", None),
                ("image", Some("image-node")),
                ("progress", Some("progress-node")),
                ("spacer", None),
            ],
        ),
    ] {
        assert_variant_payloads(&resolve, package_id, name, payloads);
    }
}

#[test]
fn recursive_and_batch_semantics_use_typed_bounded_containers() {
    let (resolve, package_id) = load_wit();

    for (name, fields) in [
        (
            "data-record-entry",
            &[("key", "string"), ("value", "u32")][..],
        ),
        (
            "data-value",
            &[("nodes", "list<data-node>"), ("root", "u32")],
        ),
        (
            "extension-update",
            &[
                ("state", "option<list<state-entry>>"),
                ("effects", "list<effect-request>"),
            ],
        ),
        (
            "capability-requirements",
            &[
                ("required", "list<capability-request>"),
                ("optional", "list<capability-request>"),
                ("provided", "list<provided-capability>"),
            ],
        ),
        (
            "capability-snapshot",
            &[
                ("granted", "list<capability-grant>"),
                ("missing-optional", "list<capability-id>"),
            ],
        ),
        ("ui-children", &[("children", "list<u32>")]),
        ("ui-tree", &[("nodes", "list<ui-node>"), ("root", "u32")]),
        (
            "menu-submenu-item",
            &[
                ("label", "string"),
                ("icon", "option<icon-ref>"),
                ("children", "list<u32>"),
            ],
        ),
        (
            "menu-tree",
            &[("items", "list<menu-item>"), ("roots", "list<u32>")],
        ),
        ("validation-errors", &[("errors", "list<validation-error>")]),
    ] {
        assert_record_field_types(&resolve, package_id, name, fields);
    }

    let TypeDefKind::Result(effect_result) = type_kind(&resolve, package_id, "effect-result")
    else {
        panic!("effect-result must remain a typed WIT result");
    };
    assert_eq!(
        effect_result.ok.map(|ok| type_shape(&resolve, ok)),
        Some("data-value".into())
    );
    assert_eq!(
        effect_result.err.map(|error| type_shape(&resolve, error)),
        Some("extension-error".into())
    );
}

#[test]
fn major_rust_records_have_matching_typed_wit_shapes() {
    let (resolve, package_id) = load_wit();

    complete_record!(CauseId { high, low });
    complete_record!(CauseContext { id, parent, depth });
    let _generation_shape = |value: GenerationId| {
        let GenerationId(_) = value;
    };
    complete_record!(ResourceHandle {
        kind,
        generation,
        id,
    });
    complete_record!(EventEnvelope {
        cause,
        source,
        sequence,
        event,
    });
    complete_record!(ExtensionUpdate { state, effects });
    complete_record!(EffectRequest { id, cause, effect });
    complete_record!(ExtensionError {
        code,
        message,
        retryable,
    });
    complete_record!(CapabilityRequest { id, version, scope });
    complete_record!(ProvidedCapability { id, version });
    complete_record!(CapabilityRequirements {
        required,
        optional,
        provided,
    });
    complete_record!(PermissionRequest {
        permission,
        reason,
        required,
    });
    complete_record!(CapabilityGrant {
        extension,
        capability,
        scope,
    });
    complete_record!(CapabilitySnapshot {
        granted,
        missing_optional,
    });
    complete_record!(StoragePermission { area, quota_bytes });
    complete_record!(Publisher { id, name });
    complete_record!(PlatformRequirement {
        platform,
        minimum_version,
    });
    complete_record!(HostCompatibility {
        extension_api,
        minimum_host,
        platforms,
    });
    complete_record!(ExtensionDependency {
        id,
        version,
        optional,
    });
    complete_record!(StorageRequirements {
        settings_bytes,
        document_bytes,
        ephemeral_cache_bytes,
    });
    complete_record!(SettingChoice { label, value });
    complete_record!(SettingDefinition {
        key,
        label,
        description,
        value_type,
        default,
        sensitive,
    });
    complete_record!(SettingsSchema { version, fields });
    complete_record!(CommandDefinition {
        id,
        title,
        description,
        category,
    });
    complete_record!(CommandBehavior { command, action });
    complete_record!(ContributionOrder {
        priority,
        before,
        after,
    });
    complete_record!(MenuItemOrder {
        priority,
        before,
        after,
    });
    complete_record!(ContributionSet {
        commands,
        command_behaviors,
        menus,
        views,
    });
    complete_record!(UiContribution {
        id,
        slot,
        order,
        root,
    });
    complete_record!(MenuContribution {
        id,
        slot,
        order,
        items,
    });
    complete_record!(UiNode { id, visible, kind });
    complete_record!(MenuItem {
        id,
        order,
        visible,
        kind,
    });
    complete_record!(TextSpan { text, emphasis });
    complete_record!(SelectOption { label, value });
    complete_record!(UiTab { id, label, content });
    complete_record!(ValidationError {
        code,
        path,
        message,
    });
    complete_record!(ExtensionManifest {
        schema_version,
        id,
        name,
        version,
        publisher,
        description,
        license,
        compatibility,
        entrypoint,
        dependencies,
        capabilities,
        permissions,
        contributions,
        settings,
        storage,
    });
    complete_record!(ValidationLimits {
        maximum_dependencies,
        maximum_capabilities,
        maximum_permissions,
        maximum_contributions,
        maximum_commands,
        maximum_ui_nodes,
        maximum_ui_depth,
        maximum_children_per_node,
        maximum_menu_items,
        maximum_menu_depth,
        maximum_string_bytes,
        maximum_list_items,
        maximum_value_nodes,
        maximum_value_depth,
        maximum_binding_segments,
        maximum_storage_bytes_per_area,
        maximum_settings,
        maximum_effect_batch,
        maximum_event_batch,
        maximum_dispatch_depth,
    });

    for (name, fields) in [
        ("data-record-entry", &["key", "value"][..]),
        ("data-value", &["nodes", "root"]),
        ("state-binding", &["segments"]),
        ("cause-id", &["high", "low"][..]),
        ("cause-context", &["id", "parent", "depth"]),
        ("generation-id", &["value"]),
        ("resource-handle", &["kind", "generation", "id"]),
        ("lifecycle-document-event", &["generation"]),
        ("lifecycle-suspended-event", &["reason"]),
        ("lifecycle-settings-changed-event", &["keys"]),
        ("lifecycle-upgrading-event", &["from", "to"]),
        ("command-invoked-event", &["command", "payload"]),
        ("capabilities-changed-event", &["snapshot"]),
        ("snapshot-changed-event", &["snapshot", "value"]),
        ("effect-completed-event", &["effect", "result"]),
        ("task-cancelled-event", &["task", "reason"]),
        ("custom-event", &["event", "payload"]),
        ("lifecycle-extension-event", &["event"]),
        ("event-envelope", &["cause", "source", "sequence", "event"]),
        ("state-entry", &["key", "value"]),
        ("extension-update", &["state", "effects"]),
        ("effect-request", &["id", "cause", "effect"]),
        ("extension-error", &["code", "message", "retryable"]),
        (
            "capability-call-effect",
            &["capability", "operation", "input"],
        ),
        ("contribution-effect", &["contribution"]),
        ("storage-get-effect", &["area", "key"]),
        ("storage-put-effect", &["area", "key", "value"]),
        ("storage-delete-effect", &["area", "key"]),
        ("copy-text-effect", &["text"]),
        ("open-browser-url-effect", &["url"]),
        ("notify-effect", &["message", "tone"]),
        ("confirm-effect", &["title", "message", "destructive"]),
        ("start-task-effect", &["task", "operation", "input"]),
        ("cancel-task-effect", &["task"]),
        ("domain-pattern", &["value"]),
        ("capability-domains", &["domains"]),
        ("capability-request", &["id", "version", "scope"]),
        ("provided-capability", &["id", "version"]),
        (
            "capability-requirements",
            &["required", "optional", "provided"],
        ),
        ("storage-permission", &["area", "quota-bytes"]),
        ("network-domains", &["domains"]),
        ("permission-request", &["permission", "reason", "required"]),
        ("capability-grant", &["extension", "capability", "scope"]),
        ("capability-snapshot", &["granted", "missing-optional"]),
        ("publisher", &["id", "name"]),
        ("platform-requirement", &["platform", "minimum-version"]),
        (
            "host-compatibility",
            &["extension-api", "minimum-host", "platforms"],
        ),
        ("declarative-entrypoint", &["ui"]),
        ("wasm-component-entrypoint", &["component", "world", "ui"]),
        ("native-builtin-entrypoint", &["adapter", "ui"]),
        ("extension-dependency", &["id", "version", "optional"]),
        (
            "storage-requirements",
            &["settings-bytes", "document-bytes", "ephemeral-cache-bytes"],
        ),
        ("integer-setting", &["minimum", "maximum"]),
        ("number-setting", &["minimum", "maximum"]),
        ("string-setting", &["maximum-bytes"]),
        ("choice-setting", &["options"]),
        (
            "string-list-setting",
            &["maximum-items", "maximum-item-bytes"],
        ),
        ("setting-choice", &["label", "value"]),
        (
            "setting-definition",
            &[
                "key",
                "label",
                "description",
                "value-type",
                "default",
                "sensitive",
            ],
        ),
        ("settings-schema", &["version", "fields"]),
        (
            "command-definition",
            &["id", "title", "description", "category"],
        ),
        ("set-state-behavior", &["binding"]),
        ("contribution-behavior", &["contribution"]),
        ("command-behavior", &["command", "action"]),
        ("contribution-order", &["priority", "before", "after"]),
        ("menu-item-order", &["priority", "before", "after"]),
        (
            "menu-command-item",
            &["label", "command", "payload", "icon", "enabled", "checked"],
        ),
        ("menu-submenu-item", &["label", "icon", "children"]),
        ("menu-contribution", &["id", "slot", "order", "items"]),
        ("ui-children", &["children"]),
        ("text-node", &["text", "selectable"]),
        ("text-span", &["text", "emphasis"]),
        ("styled-text-node", &["spans", "selectable"]),
        ("metric-node", &["label", "value", "format"]),
        ("markdown-node", &["markdown", "selectable"]),
        ("button-node", &["label", "command", "payload"]),
        ("icon-button-node", &["icon", "label", "command"]),
        ("toggle-node", &["label", "value", "command"]),
        ("select-option", &["label", "value"]),
        ("select-node", &["label", "value", "options", "command"]),
        (
            "text-field-node",
            &["label", "value", "command", "maximum-bytes"],
        ),
        ("ui-tab", &["id", "label", "content"]),
        ("tabs-node", &["tabs", "selected", "command"]),
        ("badge-node", &["label", "tone"]),
        ("image-node", &["asset", "alternative-text"]),
        ("progress-node", &["label", "basis-points"]),
        (
            "contribution-set",
            &["commands", "command-behaviors", "menus", "views"],
        ),
        ("ui-contribution", &["id", "slot", "order", "root"]),
        ("ui-node", &["id", "visible", "kind"]),
        ("ui-tree", &["nodes", "root"]),
        ("menu-item", &["id", "order", "visible", "kind"]),
        ("menu-tree", &["items", "roots"]),
        (
            "activation-context",
            &["extension", "generation", "cause", "capabilities"],
        ),
        ("validation-error", &["code", "path", "message"]),
        ("validation-errors", &["errors"]),
    ] {
        assert_record_fields(&resolve, package_id, name, fields);
    }

    assert_record_fields(
        &resolve,
        package_id,
        "extension-manifest",
        &[
            "schema-version",
            "id",
            "name",
            "version",
            "publisher",
            "description",
            "license",
            "compatibility",
            "entrypoint",
            "dependencies",
            "capabilities",
            "permissions",
            "contributions",
            "settings",
            "storage",
        ],
    );

    assert_record_fields(
        &resolve,
        package_id,
        "validation-limits",
        &[
            "maximum-dependencies",
            "maximum-capabilities",
            "maximum-permissions",
            "maximum-contributions",
            "maximum-commands",
            "maximum-ui-nodes",
            "maximum-ui-depth",
            "maximum-children-per-node",
            "maximum-menu-items",
            "maximum-menu-depth",
            "maximum-string-bytes",
            "maximum-list-items",
            "maximum-value-nodes",
            "maximum-value-depth",
            "maximum-binding-segments",
            "maximum-storage-bytes-per-area",
            "maximum-settings",
            "maximum-effect-batch",
            "maximum-event-batch",
            "maximum-dispatch-depth",
        ],
    );
}
