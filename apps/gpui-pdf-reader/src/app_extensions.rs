//! GPUI PDF Reader's composition root for bundled and installed extensions.
//!
//! Feature implementations speak the runtime-neutral extension protocol. The
//! reader shell remains responsible for executing approved effects against
//! GPUI, PDF sessions, storage, and operating-system services.

use std::str::FromStr;

use gpui::MenuItem as GpuiMenuItem;
use gpui_component::{ThemeConfig, ThemeMode};
use key_extension_api::{
    BooleanSource, CURRENT_MANIFEST_SCHEMA, CapabilityId, CapabilityRequest,
    CapabilityRequirements, CapabilityScope, CommandDefinition, CommandId, CompatibleVersion,
    ContributionId, ContributionOrder, ContributionSet, DataValue, EffectId, EffectRequest,
    EffectResult, EventEnvelope, EventSubscription, ExtensionEffect, ExtensionEntrypoint,
    ExtensionError, ExtensionErrorCode, ExtensionEvent, ExtensionId, ExtensionManifest,
    ExtensionVersion, HostCompatibility, LocalId, MenuContribution, MenuItem, MenuItemKind,
    MenuItemOrder, MenuSlotId, NativeAdapterId, Publisher, SettingsSchema, StorageRequirements,
};
use key_extension_gpui::{BoundedStateMap, native_menu_slots, resolve_menu_slots};
use key_extension_host::{
    ArbitratedEffect, CollectedContributions, ExtensionHost, HostConfig, HostError,
    NativeExtension, NativeExtensionAdapter, NativeUpdate, PackageMetadata, TickReport,
};

const THEME_EXTENSION: &str = "com.jonasweinert.gpuipdf.theme";
const THEME_ADAPTER: &str = "com.jonasweinert.gpuipdf.theme/native";
const SELECT_THEME_COMMAND: &str = "com.jonasweinert.gpuipdf.theme/select";
const THEME_CAPABILITY: &str = "key:ui/theme";
const THEME_MENU: &str = "com.jonasweinert.gpuipdf.theme/view-theme-menu";

/// Owns all extension lifecycle state for one reader application instance.
/// It is deliberately independent from GPUI's global application state.
pub struct ReaderExtensions {
    host: ExtensionHost,
    theme_command: CommandId,
    startup_error: Option<String>,
}

impl ReaderExtensions {
    pub fn new(themes: &[ThemeConfig], safe_mode: bool) -> Result<Self, HostError> {
        let extension = extension_id();
        let adapter = native_adapter_id();
        let theme_command = theme_command_id();
        let capability = theme_capability_id();

        let config = HostConfig {
            safe_mode,
            ..HostConfig::default()
        };
        let mut host = ExtensionHost::new(config);
        host.register_host_capability(capability.clone(), version("1.0.0"));
        let command_for_adapter = theme_command.clone();
        host.register_native_adapter(NativeExtensionAdapter::new(adapter, move || {
            Box::new(ThemeExtension {
                command: command_for_adapter.clone(),
                capability: capability.clone(),
            }) as Box<dyn NativeExtension>
        }));
        host.install(theme_manifest(themes), PackageMetadata::bundled())?;
        host.activate(&extension)?;

        Ok(Self {
            host,
            theme_command,
            startup_error: None,
        })
    }

    /// Keeps the product usable if a bundled package is corrupt or
    /// incompatible. The failed feature contributes no UI and the reason is
    /// surfaced by the reader instead of aborting application startup.
    pub fn disabled(safe_mode: bool, error: impl Into<String>) -> Self {
        let config = HostConfig {
            safe_mode,
            ..HostConfig::default()
        };
        Self {
            host: ExtensionHost::new(config),
            theme_command: theme_command_id(),
            startup_error: Some(error.into()),
        }
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub fn contributions(&mut self) -> CollectedContributions {
        self.host.collect_contributions()
    }

    /// Resolve a host-owned menubar slot into native GPUI menu items. The
    /// extension contributes semantic data only; GPUI actions are constructed
    /// by the trusted bridge.
    pub fn native_menu_items(&mut self, slot: &str) -> Vec<GpuiMenuItem> {
        let Ok(slot) = MenuSlotId::parse(slot) else {
            return Vec::new();
        };
        let contributions = self.contributions();
        let resolved = resolve_menu_slots(&contributions, &BoundedStateMap::default());
        native_menu_slots(&resolved)
            .into_iter()
            .find(|candidate| candidate.slot == slot)
            .map_or_else(Vec::new, |candidate| candidate.items)
    }

    /// Queues a command through the bounded host dispatcher and returns only
    /// effects that survived capability, permission, size, and loop checks.
    pub fn invoke_command(
        &mut self,
        command: &CommandId,
        payload: Option<DataValue>,
    ) -> Result<Vec<ArbitratedEffect>, HostError> {
        self.host
            .invoke_command(command, payload.unwrap_or(DataValue::Null))?;
        Ok(self.host.process_tick().effects)
    }

    pub fn complete_effect(
        &mut self,
        effect: &ArbitratedEffect,
        result: EffectResult,
    ) -> Result<TickReport, HostError> {
        self.host.complete_effect(effect, result)?;
        Ok(self.host.process_tick())
    }

    pub fn theme_command(&self) -> &CommandId {
        &self.theme_command
    }

    pub fn latest_diagnostic_message(&self) -> Option<String> {
        self.host
            .diagnostics()
            .last()
            .map(|diagnostic| diagnostic.message.clone())
    }
}

struct ThemeExtension {
    command: CommandId,
    capability: CapabilityId,
}

impl NativeExtension for ThemeExtension {
    fn subscriptions(&self) -> Vec<EventSubscription> {
        vec![EventSubscription::Commands]
    }

    fn handle_event(&mut self, event: &EventEnvelope) -> Result<NativeUpdate, ExtensionError> {
        let ExtensionEvent::CommandInvoked { command, payload } = &event.event else {
            return Ok(NativeUpdate::default());
        };
        if command != &self.command {
            return Ok(NativeUpdate::default());
        }
        let DataValue::String(theme) = payload else {
            return Err(ExtensionError {
                code: ExtensionErrorCode::InvalidRequest,
                message: "theme selection requires a string payload".into(),
                retryable: false,
            });
        };
        let id = EffectId::parse(format!("{THEME_EXTENSION}/apply-theme-{}", event.sequence))
            .expect("generated effect ID is canonical");
        Ok(NativeUpdate::with_effects(vec![EffectRequest {
            id,
            cause: event.cause,
            effect: ExtensionEffect::CapabilityCall {
                capability: self.capability.clone(),
                operation: "select".into(),
                input: DataValue::String(theme.clone()),
            },
        }]))
    }
}

fn theme_manifest(themes: &[ThemeConfig]) -> ExtensionManifest {
    let command = theme_command_id();
    let mut theme_items = vec![command_item(
        "follow-system",
        "Follow System (Default)",
        &command,
        "",
    )];
    let mode_items = [
        theme_submenu("Light", "light", ThemeMode::Light, themes, &command),
        theme_submenu("Dark", "dark", ThemeMode::Dark, themes, &command),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !mode_items.is_empty() {
        theme_items.push(separator("mode-separator"));
        theme_items.extend(mode_items);
    }
    ExtensionManifest {
        schema_version: CURRENT_MANIFEST_SCHEMA,
        id: extension_id(),
        name: "Reader themes".into(),
        version: version("1.0.0"),
        publisher: Publisher {
            id: "jonasweinert".into(),
            name: "Jonas Weinert".into(),
        },
        description: "Bundled theme selection for GPUI PDF Reader".into(),
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
            required: vec![CapabilityRequest {
                id: theme_capability_id(),
                version: compatible("^1.0"),
                scope: CapabilityScope::Application,
            }],
            optional: Vec::new(),
            provided: Vec::new(),
        },
        permissions: Vec::new(),
        contributions: ContributionSet {
            commands: vec![CommandDefinition {
                id: command.clone(),
                title: "Select theme".into(),
                description: "Apply a bundled reader theme or follow the system appearance".into(),
                category: "Appearance".into(),
            }],
            menus: vec![MenuContribution {
                id: ContributionId::parse(THEME_MENU).expect("static contribution ID is valid"),
                slot: MenuSlotId::parse("view.appearance").expect("static menu slot is valid"),
                order: ContributionOrder::default(),
                items: vec![submenu("theme-root", "Theme", theme_items)],
            }],
            views: Vec::new(),
        },
        settings: SettingsSchema::default(),
        storage: StorageRequirements::default(),
    }
}

fn theme_submenu(
    label: &str,
    id: &str,
    mode: ThemeMode,
    themes: &[ThemeConfig],
    command: &CommandId,
) -> Option<MenuItem> {
    let children = themes
        .iter()
        .filter(|theme| theme.mode == mode)
        .enumerate()
        .map(|(index, theme)| {
            command_item(
                &format!("{id}/theme-{index}"),
                theme.name.as_ref(),
                command,
                theme.name.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    (!children.is_empty()).then(|| submenu(id, label, children))
}

fn command_item(id: &str, label: &str, command: &CommandId, payload: &str) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("generated menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Command {
            label: label.into(),
            command: command.clone(),
            payload: Some(DataValue::String(payload.into())),
            icon: None,
            enabled: BooleanSource::Constant(true),
            checked: None,
        },
    }
}

fn submenu(id: &str, label: &str, children: Vec<MenuItem>) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("static menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Submenu {
            label: label.into(),
            icon: None,
            children,
        },
    }
}

fn separator(id: &str) -> MenuItem {
    MenuItem {
        id: LocalId::parse(id).expect("static menu item ID is valid"),
        order: MenuItemOrder::default(),
        visible: BooleanSource::Constant(true),
        kind: MenuItemKind::Separator,
    }
}

fn extension_id() -> ExtensionId {
    ExtensionId::parse(THEME_EXTENSION).expect("static extension ID is valid")
}

fn native_adapter_id() -> NativeAdapterId {
    NativeAdapterId::parse(THEME_ADAPTER).expect("static adapter ID is valid")
}

fn theme_command_id() -> CommandId {
    CommandId::parse(SELECT_THEME_COMMAND).expect("static command ID is valid")
}

fn theme_capability_id() -> CapabilityId {
    CapabilityId::parse(THEME_CAPABILITY).expect("static capability ID is valid")
}

fn version(value: &str) -> ExtensionVersion {
    ExtensionVersion::from_str(value).expect("static extension version is valid")
}

fn compatible(value: &str) -> CompatibleVersion {
    CompatibleVersion::from_str(value).expect("static compatibility range is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme(name: &str, mode: ThemeMode) -> ThemeConfig {
        ThemeConfig {
            name: name.to_owned().into(),
            mode,
            ..ThemeConfig::default()
        }
    }

    #[test]
    fn bundled_theme_manifest_is_valid_and_nested_in_appearance_slot() {
        let themes = [
            test_theme("Paper", ThemeMode::Light),
            test_theme("Midnight", ThemeMode::Dark),
        ];
        let manifest = theme_manifest(&themes);
        manifest.validate().expect("bundled manifest must validate");
        assert_eq!(
            manifest.contributions.menus[0].slot.as_str(),
            "view.appearance"
        );
        let MenuItemKind::Submenu { children, .. } = &manifest.contributions.menus[0].items[0].kind
        else {
            panic!("theme root must be a submenu");
        };
        assert_eq!(children.len(), 4);
    }

    #[test]
    fn theme_command_round_trips_through_host_arbitration() {
        let themes = [test_theme("Midnight", ThemeMode::Dark)];
        let mut extensions = ReaderExtensions::new(&themes, false).expect("host starts");
        let command = extensions.theme_command().clone();
        let effects = extensions
            .invoke_command(&command, Some(DataValue::String("Midnight".into())))
            .expect("command dispatches");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0].request.effect,
            ExtensionEffect::CapabilityCall {
                capability,
                operation,
                input: DataValue::String(name),
            } if capability.as_str() == THEME_CAPABILITY
                && operation == "select"
                && name == "Midnight"
        ));
        extensions
            .complete_effect(&effects[0], Ok(DataValue::Null))
            .expect("effect completion is accepted");
    }

    #[test]
    fn bundled_extension_remains_available_in_safe_mode() {
        let mut extensions = ReaderExtensions::new(&[], true).expect("bundled extension starts");
        assert_eq!(extensions.contributions().menus.len(), 1);
    }

    #[test]
    fn failed_bundled_extension_can_be_isolated_without_losing_the_host() {
        let mut extensions = ReaderExtensions::disabled(true, "broken package");
        assert_eq!(extensions.startup_error(), Some("broken package"));
        assert!(extensions.native_menu_items("view.appearance").is_empty());
    }
}
