//! Versioned WIT parsing and Rust/WIT semantic identity tests.

use std::path::PathBuf;

use key_extension_api::{
    BooleanSource, CapabilityGrant, CapabilityRequest, CapabilityRequirements, CapabilityScope,
    CapabilitySnapshot, CauseContext, CauseId, CommandBehaviorAction, CommandDefinition,
    ContributionOrder, ContributionSet, ContributionSlot, DataKind, DataValue, DocumentAccess,
    DocumentMutation, EXTENSION_API_VERSION, EffectRequest, EventEnvelope, EventSource,
    EventSubscription, ExtensionEffect, ExtensionEntrypoint, ExtensionError, ExtensionErrorCode,
    ExtensionEvent, ExtensionManifest, ExtensionUpdate, GenerationId, IconRef, LifecycleEvent,
    LifecycleState, MenuItem, MenuItemKind, MetricFormat, NetworkScope, NotificationTone,
    Permission, PermissionRequest, Platform, ResourceHandle, SettingType, SnapshotKind,
    StorageArea, StorageRequirements, TextEmphasis, UiNode, UiNodeKind, UiTone, ValidationCode,
    ValidationError, ValidationLimits,
};
use wit_parser::{PackageId, Resolve, TypeDefKind};

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

    let source = std::fs::read_to_string(wit_dir().join("extension-api.wit"))
        .expect("semantic WIT source is readable");
    assert!(!source.contains("list<u8>"));
    assert!(!source.to_ascii_lowercase().contains("json"));
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
    complete_record!(StorageRequirements {
        settings_bytes,
        document_bytes,
        ephemeral_cache_bytes,
    });
    complete_record!(CommandDefinition {
        id,
        title,
        description,
        category,
    });
    complete_record!(ContributionOrder {
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
    complete_record!(UiNode { id, visible, kind });
    complete_record!(MenuItem {
        id,
        order,
        visible,
        kind,
    });
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
        ("cause-id", &["high", "low"][..]),
        ("cause-context", &["id", "parent", "depth"]),
        ("generation-id", &["value"]),
        ("resource-handle", &["kind", "generation", "id"]),
        ("event-envelope", &["cause", "source", "sequence", "event"]),
        ("extension-update", &["state", "effects"]),
        ("effect-request", &["id", "cause", "effect"]),
        ("extension-error", &["code", "message", "retryable"]),
        ("capability-request", &["id", "version", "scope"]),
        (
            "capability-requirements",
            &["required", "optional", "provided"],
        ),
        ("permission-request", &["permission", "reason", "required"]),
        ("capability-grant", &["extension", "capability", "scope"]),
        ("capability-snapshot", &["granted", "missing-optional"]),
        (
            "storage-requirements",
            &["settings-bytes", "document-bytes", "ephemeral-cache-bytes"],
        ),
        (
            "command-definition",
            &["id", "title", "description", "category"],
        ),
        ("contribution-order", &["priority", "before", "after"]),
        (
            "contribution-set",
            &["commands", "command-behaviors", "menus", "views"],
        ),
        ("ui-node", &["id", "visible", "kind"]),
        ("ui-tree", &["nodes", "root"]),
        ("menu-item", &["id", "order", "visible", "kind"]),
        ("menu-tree", &["items", "roots"]),
        (
            "activation-context",
            &["extension", "generation", "cause", "capabilities"],
        ),
        ("validation-error", &["code", "path", "message"]),
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
