use key_extension_api::*;

fn extension_id(value: &str) -> ExtensionId {
    ExtensionId::parse(value).unwrap()
}

fn command_id(value: &str) -> CommandId {
    CommandId::parse(value).unwrap()
}

fn contribution_id(value: &str) -> ContributionId {
    ContributionId::parse(value).unwrap()
}

fn local_id(value: &str) -> LocalId {
    LocalId::parse(value).unwrap()
}

fn version(value: &str) -> ExtensionVersion {
    value.parse().unwrap()
}

fn requirement(value: &str) -> CompatibleVersion {
    value.parse().unwrap()
}

fn valid_manifest(id: &str) -> ExtensionManifest {
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension_id(id),
        name: "Document statistics".into(),
        version: version("1.2.3"),
        publisher: Publisher {
            id: "example-publisher".into(),
            name: "Example Publisher".into(),
        },
        description: "Counts pages and words without mutating the document.".into(),
        license: "MIT".into(),
        compatibility: HostCompatibility {
            extension_api: requirement("^1.0"),
            minimum_host: Some(version("0.1.0")),
            platforms: vec![],
        },
        entrypoint: ExtensionEntrypoint::Declarative {
            ui: PackagePath::parse("ui.json").unwrap(),
        },
        dependencies: vec![],
        capabilities: CapabilityRequirements::default(),
        permissions: vec![],
        contributions: ContributionSet::default(),
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn command_item(id: &str, command: &str) -> MenuItem {
    MenuItem {
        id: local_id(id),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Command {
            label: "Show statistics".into(),
            command: command_id(command),
            icon: Some(IconRef::Host("chart".into())),
            enabled: BooleanSource::Constant(true),
            checked: None,
        },
    }
}

#[test]
fn a_nested_host_menu_and_ui_tree_validate() {
    let mut manifest = valid_manifest("org.example.stats");
    let command = "org.example.stats/show";
    manifest.contributions.commands.push(CommandDefinition {
        id: command_id(command),
        title: "Show statistics".into(),
        description: "Open the document statistics panel".into(),
        category: "Document".into(),
    });
    manifest.contributions.menus.push(MenuContribution {
        id: contribution_id("org.example.stats/tools-menu"),
        slot: MenuSlotId::parse("tools.analysis").unwrap(),
        order: ContributionOrder::default(),
        items: vec![MenuItem {
            id: local_id("document-tools"),
            order: MenuItemOrder::default(),
            visible: BooleanSource::Constant(true),
            kind: MenuItemKind::Submenu {
                label: "Document tools".into(),
                icon: None,
                children: vec![command_item("show", command)],
            },
        }],
    });
    manifest.contributions.views.push(UiContribution {
        id: contribution_id("org.example.stats/side-panel"),
        slot: ContributionSlot::SidePanel,
        order: ContributionOrder::default(),
        root: UiNode {
            id: local_id("root"),
            visible: BooleanSource::Constant(true),
            kind: UiNodeKind::Column {
                children: vec![UiNode {
                    id: local_id("title"),
                    visible: BooleanSource::Constant(true),
                    kind: UiNodeKind::Text {
                        text: "Document statistics".into(),
                        selectable: true,
                    },
                }],
            },
        },
    });

    manifest.validate().unwrap();

    let json = serde_json::to_string(&manifest).unwrap();
    let decoded: ExtensionManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, manifest);

    let encoded_toml = toml::to_string(&manifest).unwrap();
    let decoded: ExtensionManifest = toml::from_str(&encoded_toml).unwrap();
    assert_eq!(decoded, manifest);
}

#[test]
fn validation_aggregates_namespace_and_command_errors() {
    let mut manifest = valid_manifest("org.example.stats");
    manifest.contributions.commands.push(CommandDefinition {
        id: command_id("org.other.extension/show"),
        title: "Show".into(),
        description: String::new(),
        category: String::new(),
    });
    manifest.contributions.menus.push(MenuContribution {
        id: contribution_id("org.other.extension/menu"),
        slot: MenuSlotId::parse("tools.analysis").unwrap(),
        order: ContributionOrder::default(),
        items: vec![command_item("show", "org.example.stats/not-declared")],
    });

    let errors = manifest.validate().unwrap_err();
    assert!(errors.contains_code(ValidationCode::ForeignNamespace));
    assert!(errors.contains_code(ValidationCode::UnknownCommand));
    assert!(errors.as_slice().len() >= 3);
}

#[test]
fn cyclic_menu_ordering_is_rejected() {
    let mut manifest = valid_manifest("org.example.stats");
    let command = "org.example.stats/show";
    manifest.contributions.commands.push(CommandDefinition {
        id: command_id(command),
        title: "Show".into(),
        description: String::new(),
        category: String::new(),
    });
    let mut first = command_item("first", command);
    first.order.before.push(local_id("second"));
    let mut second = command_item("second", command);
    second.order.before.push(local_id("first"));
    manifest.contributions.menus.push(MenuContribution {
        id: contribution_id("org.example.stats/menu"),
        slot: MenuSlotId::parse("tools.analysis").unwrap(),
        order: ContributionOrder::default(),
        items: vec![first, second],
    });

    let errors = manifest.validate().unwrap_err();
    assert!(errors.contains_code(ValidationCode::OrderingCycle));
}

#[test]
fn nested_menu_depth_is_bounded() {
    fn submenu(id: &str, child: MenuItem) -> MenuItem {
        MenuItem {
            id: local_id(id),
            order: MenuItemOrder::default(),
            visible: BooleanSource::Constant(true),
            kind: MenuItemKind::Submenu {
                label: id.into(),
                icon: None,
                children: vec![child],
            },
        }
    }

    let mut manifest = valid_manifest("org.example.stats");
    let command = "org.example.stats/show";
    manifest.contributions.commands.push(CommandDefinition {
        id: command_id(command),
        title: "Show".into(),
        description: String::new(),
        category: String::new(),
    });
    manifest.contributions.menus.push(MenuContribution {
        id: contribution_id("org.example.stats/menu"),
        slot: MenuSlotId::parse("tools.analysis").unwrap(),
        order: ContributionOrder::default(),
        items: vec![submenu(
            "one",
            submenu("two", command_item("command", command)),
        )],
    });
    let limits = ValidationLimits {
        maximum_menu_depth: 2,
        ..ValidationLimits::default()
    };

    let errors = manifest.validate_with(&limits).unwrap_err();
    assert!(errors.contains_code(ValidationCode::LimitExceeded));
}

#[test]
fn ui_values_and_stable_node_ids_are_bounded() {
    let mut manifest = valid_manifest("org.example.stats");
    let command = "org.example.stats/show";
    manifest.contributions.commands.push(CommandDefinition {
        id: command_id(command),
        title: "Show".into(),
        description: String::new(),
        category: String::new(),
    });
    manifest.contributions.views.push(UiContribution {
        id: contribution_id("org.example.stats/panel"),
        slot: ContributionSlot::SidePanel,
        order: ContributionOrder::default(),
        root: UiNode {
            id: local_id("duplicate"),
            visible: BooleanSource::Constant(true),
            kind: UiNodeKind::Column {
                children: vec![UiNode {
                    id: local_id("duplicate"),
                    visible: BooleanSource::Constant(true),
                    kind: UiNodeKind::Button {
                        label: "Show".into(),
                        command: command_id(command),
                        payload: Some(DataValue::Number(f64::NAN)),
                    },
                }],
            },
        },
    });

    let errors = manifest.validate().unwrap_err();
    assert!(errors.contains_code(ValidationCode::Duplicate));
    assert!(errors.contains_code(ValidationCode::InvalidValue));
}

#[test]
fn setting_defaults_must_satisfy_the_schema() {
    let mut manifest = valid_manifest("org.example.stats");
    manifest.settings.fields.push(SettingDefinition {
        key: "maximum-pages".into(),
        label: "Maximum pages".into(),
        description: String::new(),
        value_type: SettingType::Integer {
            minimum: Some(1),
            maximum: Some(10),
        },
        default: DataValue::Integer(20),
        sensitive: false,
    });

    let errors = manifest.validate().unwrap_err();
    assert!(errors.contains_code(ValidationCode::InvalidSetting));
}

#[test]
fn dependency_graph_checks_presence_versions_and_cycles() {
    let mut first = valid_manifest("org.example.first");
    let mut second = valid_manifest("org.example.second");
    first.dependencies.push(ExtensionDependency {
        id: second.id.clone(),
        version: requirement("^1.0"),
        optional: false,
    });
    second.dependencies.push(ExtensionDependency {
        id: first.id.clone(),
        version: requirement("^1.0"),
        optional: false,
    });

    let errors = validate_dependency_graph(&[first.clone(), second], &ValidationLimits::default())
        .unwrap_err();
    assert!(errors.contains_code(ValidationCode::DependencyCycle));

    first.dependencies[0].id = extension_id("org.example.missing");
    let errors = validate_dependency_graph(&[first], &ValidationLimits::default()).unwrap_err();
    assert!(errors.contains_code(ValidationCode::MissingDependency));
}

#[test]
fn event_and_effect_envelopes_round_trip_without_runtime_types() {
    let request = EffectRequest {
        id: EffectId::parse("org.example.stats/fetch-text").unwrap(),
        cause: CauseContext {
            id: CauseId::new(10, 20),
            parent: Some(CauseId::new(1, 2)),
            depth: 2,
        },
        effect: ExtensionEffect::CapabilityCall {
            capability: CapabilityId::parse("key:pdf/read-text").unwrap(),
            operation: "read-page".into(),
            input: DataValue::Integer(3),
        },
    };
    let encoded = serde_json::to_string(&request).unwrap();
    let decoded: EffectRequest = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, request);
}
