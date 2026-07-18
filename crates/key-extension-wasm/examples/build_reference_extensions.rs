//! Rebuild the checked-in reference extension packages from typed manifests
//! and auditable Component Model text.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use key_extension_api::{
    BooleanSource, CURRENT_MANIFEST_SCHEMA, CapabilityRequest, CapabilityRequirements,
    CapabilityScope, CommandBehavior, CommandBehaviorAction, CommandDefinition, CommandId,
    CompatibleVersion, ContributionId, ContributionOrder, ContributionSet, ContributionSlot,
    DataValue, DocumentAccess, ExtensionEntrypoint, ExtensionId, ExtensionManifest,
    ExtensionVersion, HostCompatibility, IconRef, LocalId, MenuContribution, MenuItem,
    MenuItemKind, MenuItemOrder, MenuSlotId, MetricFormat, PackagePath, Permission,
    PermissionRequest, Publisher, SelectOption, SettingDefinition, SettingType, SettingsSchema,
    StateBinding, StorageRequirements, TextEmphasis, TextSpan, UiContribution, UiNode, UiNodeKind,
    UiTone,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let workspace = workspace_root()?;
    build_theme_pack(&workspace)?;
    build_document_statistics(&workspace)?;
    build_adversarial_loop(&workspace)?;
    build_native_escape(&workspace)?;
    println!(
        "rebuilt reference extension packages in {}",
        workspace.display()
    );
    Ok(())
}

fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = env::args_os().nth(1) {
        return Ok(PathBuf::from(path).canonicalize()?);
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn build_theme_pack(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let root = workspace.join("extensions/reference-theme-pack/package");
    fs::create_dir_all(&root)?;
    write_manifest(&root, &theme_manifest())?;
    write_json(
        root.join("ui.json"),
        &json!({
            "schema_version": 1,
            "kind": "theme_preset_pack",
            "state": { "selected_preset": ["settings", "theme-preset"] },
            "tokens_only": true,
            "note": "The trusted host maps semantic theme tokens to GPUI styling."
        }),
    )?;
    Ok(())
}

fn build_document_statistics(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let root = workspace.join("extensions/reference-document-statistics");
    let package = root.join("package");
    fs::create_dir_all(&package)?;
    write_manifest(&package, &statistics_manifest())?;
    write_json(
        package.join("ui.json"),
        &json!({
            "schema_version": 1,
            "kind": "document_statistics",
            "updates_from": "snapshot_changed",
            "bindings": {
                "runtime_ready": ["runtime-ready"],
                "page_count": ["document", "statistics", "page-count"],
                "word_count": ["document", "statistics", "word-count"],
                "character_count": ["document", "statistics", "character-count"]
            }
        }),
    )?;
    let source = fs::read_to_string(root.join("source/component.wat"))?;
    let component = wat::parse_str(&source)?;
    fs::write(package.join("component.wasm"), component)?;
    Ok(())
}

fn build_adversarial_loop(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let root = workspace.join("extensions/reference-adversarial-loop");
    let package = root.join("package");
    fs::create_dir_all(&package)?;
    write_manifest(&package, &adversarial_loop_manifest())?;
    let source = fs::read_to_string(root.join("source/component.wat"))?;
    let component = wat::parse_str(&source)?;
    fs::write(package.join("component.wasm"), component)?;
    Ok(())
}

fn build_native_escape(workspace: &Path) -> Result<(), Box<dyn Error>> {
    let package = workspace.join("extensions/reference-native-escape/package");
    fs::create_dir_all(&package)?;
    write_manifest(&package, &native_escape_manifest())?;
    Ok(())
}

fn write_manifest(
    root: &std::path::Path,
    manifest: &ExtensionManifest,
) -> Result<(), Box<dyn Error>> {
    manifest.validate()?;
    fs::write(
        root.join("manifest.toml"),
        toml::to_string_pretty(manifest)?,
    )?;
    Ok(())
}

fn write_json(path: PathBuf, value: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn theme_manifest() -> ExtensionManifest {
    let extension = extension("org.key.reference.theme-pack");
    let apply = command("org.key.reference.theme-pack/apply-preset");
    let choices = [
        ("Warm Paper", "warm-paper"),
        ("Graphite", "graphite"),
        ("High Contrast", "high-contrast"),
    ];
    let menu_items = choices
        .iter()
        .enumerate()
        .map(|(index, (label, value))| MenuItem {
            id: local(&format!("preset-{index}")),
            order: MenuItemOrder::default(),
            visible: visible(),
            kind: MenuItemKind::Command {
                label: (*label).into(),
                command: apply.clone(),
                payload: Some(DataValue::String((*value).into())),
                icon: Some(IconRef::Host("palette".into())),
                enabled: visible(),
                checked: None,
            },
        })
        .collect::<Vec<_>>();
    let select_options = choices
        .iter()
        .map(|(label, value)| SelectOption {
            label: (*label).into(),
            value: DataValue::String((*value).into()),
        })
        .collect::<Vec<_>>();

    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension,
        name: "Reference Theme Pack".into(),
        version: version("1.0.0"),
        publisher: reference_publisher(),
        description: "Declarative theme preset and nested menubar reference package.".into(),
        license: "MIT".into(),
        compatibility: compatibility(),
        entrypoint: ExtensionEntrypoint::Declarative {
            ui: package_path("ui.json"),
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements::default(),
        permissions: Vec::new(),
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: apply.clone(),
                title: "Apply reference theme preset".into(),
                description: "Select a host-rendered semantic theme preset.".into(),
                category: "Appearance".into(),
            }],
            command_behaviors: vec![CommandBehavior {
                command: apply.clone(),
                action: CommandBehaviorAction::SetState {
                    binding: StateBinding::new(["settings", "theme-preset"]),
                },
            }],
            menus: vec![MenuContribution {
                id: contribution("org.key.reference.theme-pack/appearance-menu"),
                slot: MenuSlotId::parse("view.appearance").expect("static menu slot"),
                order: ContributionOrder::default(),
                items: vec![MenuItem {
                    id: local("reference-themes"),
                    order: MenuItemOrder::default(),
                    visible: visible(),
                    kind: MenuItemKind::Submenu {
                        label: "Reference themes".into(),
                        icon: Some(IconRef::Host("palette".into())),
                        children: menu_items,
                    },
                }],
            }],
            views: vec![UiContribution {
                id: contribution("org.key.reference.theme-pack/settings"),
                slot: ContributionSlot::SettingsPanel,
                order: ContributionOrder::default(),
                root: UiNode {
                    id: local("settings-root"),
                    visible: visible(),
                    kind: UiNodeKind::Column {
                        children: vec![
                            UiNode {
                                id: local("intro"),
                                visible: visible(),
                                kind: UiNodeKind::Markdown {
                                    markdown: "### Reference themes\nPresets use host-owned semantic tokens; packages do not inject GPUI code or arbitrary styles.".into(),
                                    selectable: true,
                                },
                            },
                            UiNode {
                                id: local("preset"),
                                visible: visible(),
                                kind: UiNodeKind::Select {
                                    label: "Theme preset".into(),
                                    value: StateBinding::new(["settings", "theme-preset"]),
                                    options: select_options.clone(),
                                    command: apply,
                                },
                            },
                        ],
                    },
                },
            }],
        },
        settings: SettingsSchema {
            version: 1,
            fields: vec![
                SettingDefinition {
                    key: "theme-preset".into(),
                    label: "Theme preset".into(),
                    description: "Host-rendered semantic palette preset.".into(),
                    value_type: SettingType::Choice {
                        options: select_options
                            .into_iter()
                            .map(|option| key_extension_api::SettingChoice {
                                label: option.label,
                                value: option.value,
                            })
                            .collect(),
                    },
                    default: DataValue::String("warm-paper".into()),
                    sensitive: false,
                },
                SettingDefinition {
                    key: "display-name".into(),
                    label: "Preset label".into(),
                    description: "A short local label shown with this preset.".into(),
                    value_type: SettingType::String {
                        maximum_bytes: Some(80),
                    },
                    default: DataValue::String("Reading palette".into()),
                    sensitive: false,
                },
                SettingDefinition {
                    key: "follow-document".into(),
                    label: "Follow document appearance".into(),
                    description: "Allow the preset to respond to document appearance changes."
                        .into(),
                    value_type: SettingType::Boolean,
                    default: DataValue::Boolean(true),
                    sensitive: false,
                },
            ],
        },
        storage: StorageRequirements {
            settings_bytes: 4 * 1024,
            ..StorageRequirements::default()
        },
    }
}

fn statistics_manifest() -> ExtensionManifest {
    let open = command("org.key.reference.document-statistics/open");
    let panel = contribution("org.key.reference.document-statistics/panel");
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension("org.key.reference.document-statistics"),
        name: "Reference Document Statistics".into(),
        version: version("1.0.0"),
        publisher: reference_publisher(),
        description: "Sandboxed Component Model lifecycle and PDF statistics side-panel pilot."
            .into(),
        license: "MIT".into(),
        compatibility: compatibility(),
        entrypoint: ExtensionEntrypoint::WasmComponent {
            component: package_path("component.wasm"),
            world: "key:extension-runtime/extension@0.1.0".into(),
            ui: Some(package_path("ui.json")),
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements {
            required: vec![
                CapabilityRequest {
                    id: capability("key:pdf/document-metadata"),
                    version: compatible("^0.1"),
                    scope: CapabilityScope::ActiveDocument,
                },
                CapabilityRequest {
                    id: capability("key:pdf/text"),
                    version: compatible("^0.1"),
                    scope: CapabilityScope::ActiveDocument,
                },
            ],
            optional: Vec::new(),
            provided: Vec::new(),
        },
        permissions: vec![
            PermissionRequest {
                permission: Permission::ReadDocumentMetadata,
                reason: "Display page count and document identity.".into(),
                required: true,
            },
            PermissionRequest {
                permission: Permission::ReadDocumentText(DocumentAccess::ActiveDocument),
                reason: "Count words and characters in the active document.".into(),
                required: true,
            },
            PermissionRequest {
                permission: Permission::AddSidePanel,
                reason: "Show the document statistics panel.".into(),
                required: true,
            },
        ],
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: open.clone(),
                title: "Show document statistics".into(),
                description: "Open the sandboxed document statistics side panel.".into(),
                category: "Document".into(),
            }],
            command_behaviors: vec![CommandBehavior {
                command: open.clone(),
                action: CommandBehaviorAction::OpenContribution {
                    contribution: panel.clone(),
                },
            }],
            menus: vec![MenuContribution {
                id: contribution("org.key.reference.document-statistics/tools-menu"),
                slot: MenuSlotId::parse("tools.extensions").expect("static menu slot"),
                order: ContributionOrder::default(),
                items: vec![MenuItem {
                    id: local("show-statistics"),
                    order: MenuItemOrder::default(),
                    visible: visible(),
                    kind: MenuItemKind::Command {
                        label: "Document statistics".into(),
                        command: open.clone(),
                        payload: None,
                        icon: Some(IconRef::Host("chart".into())),
                        enabled: visible(),
                        checked: None,
                    },
                }],
            }],
            views: vec![UiContribution {
                id: panel,
                slot: ContributionSlot::SidePanel,
                order: ContributionOrder::default(),
                root: UiNode {
                    id: local("panel-root"),
                    visible: visible(),
                    kind: UiNodeKind::Column {
                        children: vec![
                            UiNode {
                                id: local("title"),
                                visible: visible(),
                                kind: UiNodeKind::StyledText {
                                    spans: vec![TextSpan {
                                        text: "Document statistics".into(),
                                        emphasis: TextEmphasis::Strong,
                                    }],
                                    selectable: true,
                                },
                            },
                            UiNode {
                                id: local("runtime"),
                                visible: visible(),
                                kind: UiNodeKind::Badge {
                                    label: "Sandboxed WebAssembly".into(),
                                    tone: UiTone::Positive,
                                },
                            },
                            UiNode {
                                id: local("divider"),
                                visible: visible(),
                                kind: UiNodeKind::Divider,
                            },
                            statistic_metric("pages", "Pages", "page-count"),
                            statistic_metric("words", "Known words", "word-count"),
                            statistic_metric("characters", "Known characters", "character-count"),
                            UiNode {
                                id: local("status"),
                                visible: BooleanSource::Binding(StateBinding::new([
                                    "runtime-ready",
                                ])),
                                kind: UiNodeKind::Text {
                                    text: "The sandbox received a bounded document event.".into(),
                                    selectable: true,
                                },
                            },
                        ],
                    },
                },
            }],
        },
        settings: SettingsSchema::default(),
        storage: StorageRequirements {
            ephemeral_cache_bytes: 64 * 1024,
            ..StorageRequirements::default()
        },
    }
}

fn adversarial_loop_manifest() -> ExtensionManifest {
    let probe = command("org.key.reference.adversarial-loop/probe");
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension("org.key.reference.adversarial-loop"),
        name: "Reference Adversarial Loop".into(),
        version: version("1.0.0"),
        publisher: reference_publisher(),
        description: "Security fixture whose event handler intentionally exhausts its fuel budget."
            .into(),
        license: "MIT".into(),
        compatibility: compatibility(),
        entrypoint: ExtensionEntrypoint::WasmComponent {
            component: package_path("component.wasm"),
            world: "key:extension-runtime/extension@0.1.0".into(),
            ui: None,
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements {
            required: vec![CapabilityRequest {
                id: capability("key:pdf/document-metadata"),
                version: compatible("^0.1"),
                scope: CapabilityScope::ActiveDocument,
            }],
            optional: Vec::new(),
            provided: Vec::new(),
        },
        permissions: vec![PermissionRequest {
            permission: Permission::ReadDocumentMetadata,
            reason: "Receive a bounded document event used by the containment test.".into(),
            required: true,
        }],
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: probe,
                title: "Run adversarial containment probe".into(),
                description: "Deliver another bounded event to the hostile test guest.".into(),
                category: "Security testing".into(),
            }],
            ..ContributionSet::default()
        },
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn native_escape_manifest() -> ExtensionManifest {
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension("org.key.reference.native-escape"),
        name: "Reference Native Escape".into(),
        version: version("1.0.0"),
        publisher: reference_publisher(),
        description: "Security fixture proving that local packages cannot request native code."
            .into(),
        license: "MIT".into(),
        compatibility: compatibility(),
        entrypoint: ExtensionEntrypoint::NativeBuiltin {
            adapter: key_extension_api::NativeAdapterId::parse(
                "org.key.reference.native-escape/forbidden",
            )
            .expect("static adapter ID"),
            ui: None,
        },
        dependencies: Vec::new(),
        capabilities: CapabilityRequirements::default(),
        permissions: Vec::new(),
        contributions: ContributionSet::default(),
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn statistic_metric(id: &str, label: &str, statistic: &str) -> UiNode {
    UiNode {
        id: local(id),
        visible: visible(),
        kind: UiNodeKind::Metric {
            label: label.into(),
            value: StateBinding::new(["document", "statistics", statistic]),
            format: MetricFormat::Integer,
        },
    }
}

fn compatibility() -> HostCompatibility {
    HostCompatibility {
        extension_api: compatible("^0.1"),
        minimum_host: Some(version("0.1.0")),
        platforms: Vec::new(),
    }
}

fn reference_publisher() -> Publisher {
    Publisher {
        id: "key-reference".into(),
        name: "Key Reference Extensions".into(),
    }
}

fn extension(value: &str) -> ExtensionId {
    ExtensionId::parse(value).expect("static extension ID")
}

fn command(value: &str) -> CommandId {
    CommandId::parse(value).expect("static command ID")
}

fn contribution(value: &str) -> ContributionId {
    ContributionId::parse(value).expect("static contribution ID")
}

fn capability(value: &str) -> key_extension_api::CapabilityId {
    key_extension_api::CapabilityId::parse(value).expect("static capability ID")
}

fn local(value: &str) -> LocalId {
    LocalId::parse(value).expect("static local ID")
}

fn package_path(value: &str) -> PackagePath {
    PackagePath::parse(value).expect("static package path")
}

fn version(value: &str) -> ExtensionVersion {
    ExtensionVersion::from_str(value).expect("static extension version")
}

fn compatible(value: &str) -> CompatibleVersion {
    CompatibleVersion::from_str(value).expect("static version requirement")
}

fn visible() -> BooleanSource {
    BooleanSource::Constant(true)
}
