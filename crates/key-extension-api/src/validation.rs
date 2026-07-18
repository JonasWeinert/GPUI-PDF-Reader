use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

use crate::{
    CURRENT_MANIFEST_SCHEMA, CapabilityScope, ContributionId, DataValue, ExtensionEntrypoint,
    ExtensionManifest, MenuItem, MenuItemKind, NetworkScope, Permission, SettingType, StateBinding,
    UiNode, UiNodeKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCode {
    UnsupportedSchema,
    InvalidIdentifier,
    InvalidPackagePath,
    InvalidEntrypoint,
    InvalidField,
    StringTooLong,
    LimitExceeded,
    Duplicate,
    ForeignNamespace,
    UnknownCommand,
    MissingDependency,
    IncompatibleDependency,
    DependencyCycle,
    OrderingCycle,
    InvalidDomain,
    InvalidSetting,
    InvalidValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: ValidationCode,
    pub path: String,
    pub message: String,
}

impl ValidationError {
    #[must_use]
    pub fn new(code: ValidationCode, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl Error for ValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors(Vec<ValidationError>);

impl ValidationErrors {
    #[must_use]
    pub fn new(errors: Vec<ValidationError>) -> Self {
        Self(errors)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ValidationError] {
        &self.0
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<ValidationError> {
        self.0
    }

    #[must_use]
    pub fn contains_code(&self, code: ValidationCode) -> bool {
        self.0.iter().any(|error| error.code == code)
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} extension contract validation error(s)",
            self.0.len()
        )
    }
}

impl Error for ValidationErrors {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationLimits {
    pub maximum_dependencies: usize,
    pub maximum_capabilities: usize,
    pub maximum_permissions: usize,
    pub maximum_contributions: usize,
    pub maximum_commands: usize,
    pub maximum_ui_nodes: usize,
    pub maximum_ui_depth: usize,
    pub maximum_children_per_node: usize,
    pub maximum_menu_items: usize,
    pub maximum_menu_depth: usize,
    pub maximum_string_bytes: usize,
    pub maximum_list_items: usize,
    pub maximum_value_nodes: usize,
    pub maximum_value_depth: usize,
    pub maximum_binding_segments: usize,
    pub maximum_storage_bytes_per_area: u64,
    pub maximum_settings: usize,
    pub maximum_effect_batch: usize,
    pub maximum_event_batch: usize,
    pub maximum_dispatch_depth: u16,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            maximum_dependencies: 128,
            maximum_capabilities: 128,
            maximum_permissions: 64,
            maximum_contributions: 128,
            maximum_commands: 256,
            maximum_ui_nodes: 2_048,
            maximum_ui_depth: 32,
            maximum_children_per_node: 256,
            maximum_menu_items: 512,
            maximum_menu_depth: 8,
            maximum_string_bytes: 16_384,
            maximum_list_items: 1_024,
            maximum_value_nodes: 4_096,
            maximum_value_depth: 16,
            maximum_binding_segments: 16,
            maximum_storage_bytes_per_area: 64 * 1024 * 1024,
            maximum_settings: 256,
            maximum_effect_batch: 64,
            maximum_event_batch: 256,
            maximum_dispatch_depth: 32,
        }
    }
}

impl ExtensionManifest {
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        self.validate_with(&ValidationLimits::default())
    }

    pub fn validate_with(&self, limits: &ValidationLimits) -> Result<(), ValidationErrors> {
        let mut validator = ManifestValidator::new(self, limits);
        validator.validate();
        validator.finish()
    }
}

struct ManifestValidator<'a> {
    manifest: &'a ExtensionManifest,
    limits: &'a ValidationLimits,
    errors: Vec<ValidationError>,
    declared_commands: BTreeSet<String>,
}

impl<'a> ManifestValidator<'a> {
    fn new(manifest: &'a ExtensionManifest, limits: &'a ValidationLimits) -> Self {
        Self {
            manifest,
            limits,
            errors: Vec::new(),
            declared_commands: BTreeSet::new(),
        }
    }

    fn finish(self) -> Result<(), ValidationErrors> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors::new(self.errors))
        }
    }

    fn validate(&mut self) {
        if self.manifest.schema_version != CURRENT_MANIFEST_SCHEMA {
            self.error(
                ValidationCode::UnsupportedSchema,
                "schema_version",
                format!(
                    "schema {} is unsupported; expected {CURRENT_MANIFEST_SCHEMA}",
                    self.manifest.schema_version
                ),
            );
        }
        for (path, value) in [
            ("name", self.manifest.name.as_str()),
            ("publisher.id", self.manifest.publisher.id.as_str()),
            ("publisher.name", self.manifest.publisher.name.as_str()),
            ("description", self.manifest.description.as_str()),
            ("license", self.manifest.license.as_str()),
        ] {
            self.nonempty_string(path, value);
        }
        self.validate_entrypoint();
        self.validate_dependencies();
        self.validate_capabilities();
        self.validate_permissions();
        self.validate_contributions();
        self.validate_settings();
        for (path, bytes) in [
            (
                "storage.settings_bytes",
                self.manifest.storage.settings_bytes,
            ),
            (
                "storage.document_bytes",
                self.manifest.storage.document_bytes,
            ),
            (
                "storage.ephemeral_cache_bytes",
                self.manifest.storage.ephemeral_cache_bytes,
            ),
        ] {
            if bytes > self.limits.maximum_storage_bytes_per_area {
                self.limit(path, self.limits.maximum_storage_bytes_per_area);
            }
        }
    }

    fn validate_entrypoint(&mut self) {
        match &self.manifest.entrypoint {
            ExtensionEntrypoint::Declarative { .. } => {}
            ExtensionEntrypoint::WasmComponent { world, .. } => {
                if world.is_empty()
                    || world.len() > 255
                    || !world.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b':' | b'/' | b'@' | b'-' | b'.')
                    })
                {
                    self.error(
                        ValidationCode::InvalidEntrypoint,
                        "entrypoint.world",
                        "component world is not canonical",
                    );
                }
            }
            ExtensionEntrypoint::NativeBuiltin { adapter, .. } => {
                if adapter.owner() != self.manifest.id {
                    self.error(
                        ValidationCode::ForeignNamespace,
                        "entrypoint.adapter",
                        "native adapter must be owned by the extension",
                    );
                }
            }
        }
    }

    fn validate_dependencies(&mut self) {
        self.check_count(
            "dependencies",
            self.manifest.dependencies.len(),
            self.limits.maximum_dependencies,
        );
        let mut ids = BTreeSet::new();
        for (index, dependency) in self.manifest.dependencies.iter().enumerate() {
            let path = format!("dependencies[{index}].id");
            if dependency.id == self.manifest.id {
                self.error(
                    ValidationCode::DependencyCycle,
                    path.clone(),
                    "extension depends on itself",
                );
            }
            if !ids.insert(dependency.id.clone()) {
                self.error(
                    ValidationCode::Duplicate,
                    path,
                    "duplicate extension dependency",
                );
            }
        }
    }

    fn validate_capabilities(&mut self) {
        let capabilities = &self.manifest.capabilities;
        let total =
            capabilities.required.len() + capabilities.optional.len() + capabilities.provided.len();
        self.check_count("capabilities", total, self.limits.maximum_capabilities);
        let mut ids = BTreeSet::new();
        for (group, requests) in [
            ("required", capabilities.required.as_slice()),
            ("optional", capabilities.optional.as_slice()),
        ] {
            for (index, request) in requests.iter().enumerate() {
                let path = format!("capabilities.{group}[{index}]");
                if !ids.insert(request.id.clone()) {
                    self.error(
                        ValidationCode::Duplicate,
                        &path,
                        "duplicate capability request",
                    );
                }
                if let CapabilityScope::Domains(domains) = &request.scope {
                    self.validate_domains(&format!("{path}.scope"), domains);
                }
            }
        }
        for (index, capability) in capabilities.provided.iter().enumerate() {
            if !ids.insert(capability.id.clone()) {
                self.error(
                    ValidationCode::Duplicate,
                    format!("capabilities.provided[{index}]"),
                    "capability cannot be requested and provided more than once",
                );
            }
        }
    }

    fn validate_permissions(&mut self) {
        self.check_count(
            "permissions",
            self.manifest.permissions.len(),
            self.limits.maximum_permissions,
        );
        let mut serialized = BTreeSet::new();
        for (index, request) in self.manifest.permissions.iter().enumerate() {
            let path = format!("permissions[{index}]");
            self.nonempty_string(&format!("{path}.reason"), &request.reason);
            let identity = format!("{:?}", request.permission);
            if !serialized.insert(identity) {
                self.error(
                    ValidationCode::Duplicate,
                    &path,
                    "duplicate permission request",
                );
            }
            match &request.permission {
                Permission::Network(
                    NetworkScope::DeclaredDomains(domains)
                    | NetworkScope::DeclaredAndUserApproved(domains),
                ) => {
                    self.validate_domains(&path, domains);
                }
                Permission::Storage(storage)
                    if storage.quota_bytes > self.limits.maximum_storage_bytes_per_area =>
                {
                    self.limit(&path, self.limits.maximum_storage_bytes_per_area);
                }
                _ => {}
            }
        }
    }

    fn validate_domains(&mut self, path: &str, domains: &[crate::DomainPattern]) {
        self.check_count(path, domains.len(), self.limits.maximum_list_items);
        let mut seen = BTreeSet::new();
        for (index, domain) in domains.iter().enumerate() {
            if !domain.is_canonical() {
                self.error(
                    ValidationCode::InvalidDomain,
                    format!("{path}[{index}]"),
                    "domain must be a lowercase DNS name with an optional *. prefix",
                );
            }
            if !seen.insert(domain) {
                self.error(
                    ValidationCode::Duplicate,
                    format!("{path}[{index}]"),
                    "duplicate domain",
                );
            }
        }
    }

    fn validate_contributions(&mut self) {
        let contributions = &self.manifest.contributions;
        self.check_count(
            "contributions",
            contributions.menus.len() + contributions.views.len(),
            self.limits.maximum_contributions,
        );
        self.check_count(
            "contributions.commands",
            contributions.commands.len(),
            self.limits.maximum_commands,
        );
        for (index, command) in contributions.commands.iter().enumerate() {
            let path = format!("contributions.commands[{index}]");
            if command.id.owner() != self.manifest.id {
                self.error(
                    ValidationCode::ForeignNamespace,
                    format!("{path}.id"),
                    "command must be owned by the extension",
                );
            }
            if !self
                .declared_commands
                .insert(command.id.as_str().to_owned())
            {
                self.error(
                    ValidationCode::Duplicate,
                    format!("{path}.id"),
                    "duplicate command",
                );
            }
            self.nonempty_string(&format!("{path}.title"), &command.title);
            self.string(&format!("{path}.description"), &command.description);
            self.string(&format!("{path}.category"), &command.category);
        }

        let mut ids = BTreeSet::new();
        for (index, menu) in contributions.menus.iter().enumerate() {
            let path = format!("contributions.menus[{index}]");
            self.contribution_id(&path, &menu.id, &mut ids);
            let mut item_ids = BTreeSet::new();
            let mut count = 0;
            self.validate_menu_items(
                &menu.items,
                &format!("{path}.items"),
                1,
                &mut count,
                &mut item_ids,
            );
        }
        for (index, view) in contributions.views.iter().enumerate() {
            let path = format!("contributions.views[{index}]");
            self.contribution_id(&path, &view.id, &mut ids);
            let mut node_ids = BTreeSet::new();
            let mut count = 0;
            self.validate_ui_node(
                &view.root,
                &format!("{path}.root"),
                1,
                &mut count,
                &mut node_ids,
            );
        }
    }

    fn contribution_id(&mut self, path: &str, id: &ContributionId, ids: &mut BTreeSet<String>) {
        if id.owner() != self.manifest.id {
            self.error(
                ValidationCode::ForeignNamespace,
                format!("{path}.id"),
                "contribution must be owned by the extension",
            );
        }
        if !ids.insert(id.as_str().to_owned()) {
            self.error(
                ValidationCode::Duplicate,
                format!("{path}.id"),
                "duplicate contribution",
            );
        }
    }

    fn validate_menu_items(
        &mut self,
        items: &[MenuItem],
        path: &str,
        depth: usize,
        count: &mut usize,
        ids: &mut BTreeSet<String>,
    ) {
        if depth > self.limits.maximum_menu_depth {
            self.limit(path, self.limits.maximum_menu_depth);
            return;
        }
        self.check_count(path, items.len(), self.limits.maximum_children_per_node);
        let sibling_ids = items
            .iter()
            .map(|item| item.id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let mut edges = BTreeMap::<String, Vec<String>>::new();
        for (index, item) in items.iter().enumerate() {
            *count += 1;
            let item_path = format!("{path}[{index}]");
            if *count > self.limits.maximum_menu_items {
                self.limit(path, self.limits.maximum_menu_items);
                return;
            }
            if !ids.insert(item.id.as_str().to_owned()) {
                self.error(
                    ValidationCode::Duplicate,
                    format!("{item_path}.id"),
                    "menu item IDs must be unique within a contribution",
                );
            }
            self.validate_binding_source(&format!("{item_path}.visible"), &item.visible);
            let outgoing = edges.entry(item.id.as_str().to_owned()).or_default();
            for before in &item.order.before {
                if !sibling_ids.contains(before.as_str()) {
                    self.error(
                        ValidationCode::InvalidField,
                        format!("{item_path}.order.before"),
                        "ordering target must be a sibling item",
                    );
                } else {
                    outgoing.push(before.as_str().to_owned());
                }
            }
            for after in &item.order.after {
                if !sibling_ids.contains(after.as_str()) {
                    self.error(
                        ValidationCode::InvalidField,
                        format!("{item_path}.order.after"),
                        "ordering target must be a sibling item",
                    );
                } else {
                    edges
                        .entry(after.as_str().to_owned())
                        .or_default()
                        .push(item.id.as_str().to_owned());
                }
            }
            match &item.kind {
                MenuItemKind::Command {
                    label,
                    command,
                    payload,
                    icon,
                    enabled,
                    checked,
                } => {
                    self.nonempty_string(&format!("{item_path}.label"), label);
                    self.command_reference(&format!("{item_path}.command"), command.as_str());
                    self.validate_binding_source(&format!("{item_path}.enabled"), enabled);
                    if let Some(payload) = payload {
                        self.validate_value(&format!("{item_path}.payload"), payload);
                    }
                    if let Some(checked) = checked {
                        self.validate_binding_source(&format!("{item_path}.checked"), checked);
                    }
                    if let Some(icon) = icon {
                        self.validate_icon(&format!("{item_path}.icon"), icon);
                    }
                }
                MenuItemKind::Submenu {
                    label,
                    icon,
                    children,
                } => {
                    self.nonempty_string(&format!("{item_path}.label"), label);
                    if let Some(icon) = icon {
                        self.validate_icon(&format!("{item_path}.icon"), icon);
                    }
                    if children.is_empty() {
                        self.error(
                            ValidationCode::InvalidField,
                            format!("{item_path}.children"),
                            "submenu cannot be empty",
                        );
                    }
                    self.validate_menu_items(
                        children,
                        &format!("{item_path}.children"),
                        depth + 1,
                        count,
                        ids,
                    );
                }
                MenuItemKind::Separator => {}
            }
        }
        if directed_cycle(&edges) {
            self.error(
                ValidationCode::OrderingCycle,
                path,
                "menu item ordering contains a cycle",
            );
        }
    }

    fn validate_ui_node(
        &mut self,
        node: &UiNode,
        path: &str,
        depth: usize,
        count: &mut usize,
        ids: &mut BTreeSet<String>,
    ) {
        if depth > self.limits.maximum_ui_depth {
            self.limit(path, self.limits.maximum_ui_depth);
            return;
        }
        *count += 1;
        if *count > self.limits.maximum_ui_nodes {
            self.limit(path, self.limits.maximum_ui_nodes);
            return;
        }
        if !ids.insert(node.id.as_str().to_owned()) {
            self.error(
                ValidationCode::Duplicate,
                format!("{path}.id"),
                "UI node IDs must be unique within a contribution",
            );
        }
        self.validate_binding_source(&format!("{path}.visible"), &node.visible);
        match &node.kind {
            UiNodeKind::Column { children }
            | UiNodeKind::Row { children }
            | UiNodeKind::Stack { children }
            | UiNodeKind::List { children } => {
                self.check_count(path, children.len(), self.limits.maximum_children_per_node);
                for (index, child) in children.iter().enumerate() {
                    self.validate_ui_node(
                        child,
                        &format!("{path}.children[{index}]"),
                        depth + 1,
                        count,
                        ids,
                    );
                }
            }
            UiNodeKind::Text { text, .. } => self.string(&format!("{path}.text"), text),
            UiNodeKind::StyledText { spans, .. } => {
                self.check_count(path, spans.len(), self.limits.maximum_list_items);
                for (index, span) in spans.iter().enumerate() {
                    self.string(&format!("{path}.spans[{index}].text"), &span.text);
                }
            }
            UiNodeKind::Markdown { markdown, .. } => {
                self.string(&format!("{path}.markdown"), markdown)
            }
            UiNodeKind::Button {
                label,
                command,
                payload,
            } => {
                self.nonempty_string(&format!("{path}.label"), label);
                self.command_reference(&format!("{path}.command"), command.as_str());
                if let Some(payload) = payload {
                    self.validate_value(&format!("{path}.payload"), payload);
                }
            }
            UiNodeKind::IconButton {
                icon,
                label,
                command,
            } => {
                self.nonempty_string(&format!("{path}.label"), label);
                self.validate_icon(&format!("{path}.icon"), icon);
                self.command_reference(&format!("{path}.command"), command.as_str());
            }
            UiNodeKind::Toggle {
                label,
                value,
                command,
            } => {
                self.nonempty_string(&format!("{path}.label"), label);
                self.validate_binding(&format!("{path}.value"), value);
                self.command_reference(&format!("{path}.command"), command.as_str());
            }
            UiNodeKind::Select {
                label,
                value,
                options,
                command,
            } => {
                self.nonempty_string(&format!("{path}.label"), label);
                self.validate_binding(&format!("{path}.value"), value);
                self.check_count(path, options.len(), self.limits.maximum_list_items);
                for (index, option) in options.iter().enumerate() {
                    self.nonempty_string(&format!("{path}.options[{index}].label"), &option.label);
                    self.validate_value(&format!("{path}.options[{index}].value"), &option.value);
                }
                self.command_reference(&format!("{path}.command"), command.as_str());
            }
            UiNodeKind::TextField {
                label,
                value,
                command,
                maximum_bytes,
            } => {
                self.nonempty_string(&format!("{path}.label"), label);
                self.validate_binding(&format!("{path}.value"), value);
                self.command_reference(&format!("{path}.command"), command.as_str());
                if *maximum_bytes as usize > self.limits.maximum_string_bytes {
                    self.limit(
                        &format!("{path}.maximum_bytes"),
                        self.limits.maximum_string_bytes,
                    );
                }
            }
            UiNodeKind::Tabs {
                tabs,
                selected,
                command,
            } => {
                self.check_count(path, tabs.len(), self.limits.maximum_children_per_node);
                self.validate_binding(&format!("{path}.selected"), selected);
                self.command_reference(&format!("{path}.command"), command.as_str());
                for (index, tab) in tabs.iter().enumerate() {
                    self.nonempty_string(&format!("{path}.tabs[{index}].label"), &tab.label);
                    self.validate_ui_node(
                        &tab.content,
                        &format!("{path}.tabs[{index}].content"),
                        depth + 1,
                        count,
                        ids,
                    );
                }
            }
            UiNodeKind::Badge { label, .. } | UiNodeKind::Progress { label, .. } => {
                self.nonempty_string(&format!("{path}.label"), label);
                if let UiNodeKind::Progress { basis_points, .. } = &node.kind
                    && *basis_points > 10_000
                {
                    self.error(
                        ValidationCode::InvalidField,
                        format!("{path}.basis_points"),
                        "progress must be between 0 and 10000 basis points",
                    );
                }
            }
            UiNodeKind::Image {
                alternative_text, ..
            } => self.nonempty_string(&format!("{path}.alternative_text"), alternative_text),
            UiNodeKind::Divider | UiNodeKind::Spacer => {}
        }
    }

    fn validate_icon(&mut self, path: &str, icon: &crate::IconRef) {
        if let crate::IconRef::Host(name) = icon {
            self.nonempty_string(path, name);
        }
    }

    fn command_reference(&mut self, path: &str, command: &str) {
        if !self.declared_commands.contains(command) {
            self.error(
                ValidationCode::UnknownCommand,
                path,
                "command is not declared by this extension",
            );
        }
    }

    fn validate_binding_source(&mut self, path: &str, source: &crate::BooleanSource) {
        if let crate::BooleanSource::Binding(binding) = source {
            self.validate_binding(path, binding);
        }
    }

    fn validate_binding(&mut self, path: &str, binding: &StateBinding) {
        if binding.0.is_empty()
            || binding.0.len() > self.limits.maximum_binding_segments
            || binding.0.iter().any(|segment| {
                segment.is_empty()
                    || segment.len() > self.limits.maximum_string_bytes
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            self.error(
                ValidationCode::InvalidField,
                path,
                "state binding has invalid or excessive path segments",
            );
        }
    }

    fn validate_value(&mut self, path: &str, value: &DataValue) {
        let mut nodes = 0;
        self.validate_value_inner(path, value, 1, &mut nodes);
    }

    fn validate_value_inner(
        &mut self,
        path: &str,
        value: &DataValue,
        depth: usize,
        nodes: &mut usize,
    ) {
        if depth > self.limits.maximum_value_depth {
            self.limit(path, self.limits.maximum_value_depth);
            return;
        }
        *nodes += 1;
        if *nodes > self.limits.maximum_value_nodes {
            self.limit(path, self.limits.maximum_value_nodes);
            return;
        }
        match value {
            DataValue::Number(value) if !value.is_finite() => {
                self.error(ValidationCode::InvalidValue, path, "numbers must be finite");
            }
            DataValue::String(value) => self.string(path, value),
            DataValue::List(values) => {
                self.check_count(path, values.len(), self.limits.maximum_list_items);
                for (index, value) in values.iter().enumerate() {
                    self.validate_value_inner(&format!("{path}[{index}]"), value, depth + 1, nodes);
                }
            }
            DataValue::Record(values) => {
                self.check_count(path, values.len(), self.limits.maximum_list_items);
                for (key, value) in values {
                    self.nonempty_string(&format!("{path}.key"), key);
                    self.validate_value_inner(&format!("{path}.{key}"), value, depth + 1, nodes);
                }
            }
            _ => {}
        }
    }

    fn validate_settings(&mut self) {
        self.check_count(
            "settings.fields",
            self.manifest.settings.fields.len(),
            self.limits.maximum_settings,
        );
        let mut keys = BTreeSet::new();
        for (index, setting) in self.manifest.settings.fields.iter().enumerate() {
            let path = format!("settings.fields[{index}]");
            if setting.key.is_empty()
                || !setting.key.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'.')
                })
            {
                self.error(
                    ValidationCode::InvalidSetting,
                    format!("{path}.key"),
                    "setting key is not canonical",
                );
            }
            if !keys.insert(setting.key.as_str()) {
                self.error(
                    ValidationCode::Duplicate,
                    format!("{path}.key"),
                    "duplicate setting key",
                );
            }
            self.nonempty_string(&format!("{path}.label"), &setting.label);
            self.string(&format!("{path}.description"), &setting.description);
            self.validate_value(&format!("{path}.default"), &setting.default);
            if !setting.value_type.accepts(&setting.default) {
                self.error(
                    ValidationCode::InvalidSetting,
                    format!("{path}.default"),
                    "default does not satisfy the setting type",
                );
            }
            match &setting.value_type {
                SettingType::Integer {
                    minimum: Some(minimum),
                    maximum: Some(maximum),
                } if minimum > maximum => {
                    self.error(
                        ValidationCode::InvalidSetting,
                        format!("{path}.value_type"),
                        "minimum exceeds maximum",
                    );
                }
                SettingType::Number {
                    minimum: Some(minimum),
                    maximum: Some(maximum),
                } if !minimum.is_finite() || !maximum.is_finite() || minimum > maximum => {
                    self.error(
                        ValidationCode::InvalidSetting,
                        format!("{path}.value_type"),
                        "number bounds must be finite and ordered",
                    );
                }
                SettingType::Choice { options } if options.is_empty() => {
                    self.error(
                        ValidationCode::InvalidSetting,
                        format!("{path}.value_type.options"),
                        "choice requires at least one option",
                    );
                }
                _ => {}
            }
        }
    }

    fn nonempty_string(&mut self, path: &str, value: &str) {
        if value.trim().is_empty() {
            self.error(
                ValidationCode::InvalidField,
                path,
                "value must not be empty",
            );
        }
        self.string(path, value);
    }

    fn string(&mut self, path: &str, value: &str) {
        if value.len() > self.limits.maximum_string_bytes {
            self.limit(path, self.limits.maximum_string_bytes);
        }
    }

    fn check_count(&mut self, path: &str, actual: usize, maximum: usize) {
        if actual > maximum {
            self.limit(path, maximum);
        }
    }

    fn limit(&mut self, path: &str, maximum: impl fmt::Display) {
        self.error(
            ValidationCode::LimitExceeded,
            path,
            format!("configured limit of {maximum} exceeded"),
        );
    }

    fn error(&mut self, code: ValidationCode, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError::new(code, path, message));
    }
}

/// Validate installed-manifest dependency presence, compatibility, and cycles.
/// Optional dependencies are checked only when the target is present.
pub fn validate_dependency_graph(
    manifests: &[ExtensionManifest],
    limits: &ValidationLimits,
) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();
    let mut by_id = BTreeMap::new();
    for (index, manifest) in manifests.iter().enumerate() {
        if by_id.insert(manifest.id.clone(), manifest).is_some() {
            errors.push(ValidationError::new(
                ValidationCode::Duplicate,
                format!("manifests[{index}].id"),
                "duplicate installed extension ID",
            ));
        }
        if let Err(manifest_errors) = manifest.validate_with(limits) {
            errors.extend(manifest_errors.into_vec().into_iter().map(|mut error| {
                error.path = format!("manifests[{index}].{}", error.path);
                error
            }));
        }
    }

    let mut edges = BTreeMap::<String, Vec<String>>::new();
    for (index, manifest) in manifests.iter().enumerate() {
        let owner = manifest.id.as_str().to_owned();
        edges.entry(owner.clone()).or_default();
        for (dependency_index, dependency) in manifest.dependencies.iter().enumerate() {
            match by_id.get(&dependency.id) {
                None if !dependency.optional => errors.push(ValidationError::new(
                    ValidationCode::MissingDependency,
                    format!("manifests[{index}].dependencies[{dependency_index}]"),
                    format!("required extension {} is not installed", dependency.id),
                )),
                Some(target) => {
                    if !dependency.version.matches(&target.version) {
                        errors.push(ValidationError::new(
                            ValidationCode::IncompatibleDependency,
                            format!("manifests[{index}].dependencies[{dependency_index}]"),
                            format!(
                                "installed version {} does not match {}",
                                target.version, dependency.version
                            ),
                        ));
                    }
                    edges
                        .entry(owner.clone())
                        .or_default()
                        .push(dependency.id.as_str().to_owned());
                }
                None => {}
            }
        }
    }
    if directed_cycle(&edges) {
        errors.push(ValidationError::new(
            ValidationCode::DependencyCycle,
            "manifests",
            "extension dependency graph contains a cycle",
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors::new(errors))
    }
}

fn directed_cycle(edges: &BTreeMap<String, Vec<String>>) -> bool {
    fn visit(
        node: &str,
        edges: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }
        if visited.contains(node) {
            return false;
        }
        visiting.insert(node.to_owned());
        if edges.get(node).is_some_and(|targets| {
            targets
                .iter()
                .any(|target| visit(target, edges, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(node);
        visited.insert(node.to_owned());
        false
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    edges
        .keys()
        .any(|node| visit(node, edges, &mut visiting, &mut visited))
}
