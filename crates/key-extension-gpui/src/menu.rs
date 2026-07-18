use std::collections::BTreeMap;

use gpui::{Menu, MenuItem as NativeMenuItem};
use key_extension_api::{CommandId, DataValue, IconRef, MenuItem, MenuItemKind, MenuSlotId};
use key_extension_host::CollectedContributions;

use crate::BoundedStateMap;

/// The single action type emitted by every declarative control and native menu
/// entry. The application listens once and routes the typed command to the
/// semantic extension host.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = key_extension, no_json)]
pub struct InvokeExtensionCommand {
    pub command: CommandId,
    pub payload: Option<DataValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMenuSlot {
    pub slot: MenuSlotId,
    pub items: Vec<ResolvedMenuItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedMenuItem {
    Command {
        label: String,
        action: InvokeExtensionCommand,
        icon: Option<IconRef>,
        enabled: bool,
        checked: Option<bool>,
    },
    Submenu {
        label: String,
        icon: Option<IconRef>,
        children: Vec<ResolvedMenuItem>,
    },
    Separator,
}

/// Resolve visibility and state bindings, retaining the deterministic order
/// already arbitrated by `key-extension-host`.
#[must_use]
pub fn resolve_menu_slots(contributions: &CollectedContributions) -> Vec<ResolvedMenuSlot> {
    let mut slots = BTreeMap::<MenuSlotId, Vec<ResolvedMenuItem>>::new();
    for owned in &contributions.menus {
        let state = BoundedStateMap::new(owned.state.clone(), Default::default())
            .expect("host-owned extension menu state is bounded");
        let target = slots.entry(owned.menu.slot.clone()).or_default();
        target.extend(resolve_menu_items(&owned.menu.items, &state));
        normalize_separators(target);
    }
    slots
        .into_iter()
        .filter_map(|(slot, mut items)| {
            normalize_separators(&mut items);
            (!items.is_empty()).then_some(ResolvedMenuSlot { slot, items })
        })
        .collect()
}

fn resolve_menu_items(items: &[MenuItem], state: &BoundedStateMap) -> Vec<ResolvedMenuItem> {
    let mut resolved = Vec::new();
    for item in items {
        if !state.resolve_boolean(&item.visible) {
            continue;
        }
        match &item.kind {
            MenuItemKind::Command {
                label,
                command,
                payload,
                icon,
                enabled,
                checked,
            } => resolved.push(ResolvedMenuItem::Command {
                label: label.clone(),
                action: InvokeExtensionCommand {
                    command: command.clone(),
                    payload: payload.clone(),
                },
                icon: icon.clone(),
                enabled: state.resolve_boolean(enabled),
                checked: checked.as_ref().map(|value| state.resolve_boolean(value)),
            }),
            MenuItemKind::Submenu {
                label,
                icon,
                children,
            } => {
                let mut children = resolve_menu_items(children, state);
                normalize_separators(&mut children);
                if !children.is_empty() {
                    resolved.push(ResolvedMenuItem::Submenu {
                        label: label.clone(),
                        icon: icon.clone(),
                        children,
                    });
                }
            }
            MenuItemKind::Separator => resolved.push(ResolvedMenuItem::Separator),
        }
    }
    normalize_separators(&mut resolved);
    resolved
}

fn normalize_separators(items: &mut Vec<ResolvedMenuItem>) {
    while matches!(items.first(), Some(ResolvedMenuItem::Separator)) {
        items.remove(0);
    }
    while matches!(items.last(), Some(ResolvedMenuItem::Separator)) {
        items.pop();
    }
    let mut previous_separator = false;
    items.retain(|item| {
        let separator = matches!(item, ResolvedMenuItem::Separator);
        let keep = !(separator && previous_separator);
        previous_separator = separator;
        keep
    });
}

pub struct NativeMenuSlot {
    pub slot: MenuSlotId,
    pub items: Vec<NativeMenuItem>,
}

/// Convert resolved slots to GPUI's native menu representation. GPUI 0.2.2
/// cannot disable or check individual instances of one parameterized action,
/// so disabled entries are omitted while checked state remains available in
/// `ResolvedMenuSlot` for custom in-window menus.
#[must_use]
pub fn native_menu_slots(slots: &[ResolvedMenuSlot]) -> Vec<NativeMenuSlot> {
    slots
        .iter()
        .filter_map(|slot| {
            let items = native_items(&slot.items);
            (!items.is_empty()).then(|| NativeMenuSlot {
                slot: slot.slot.clone(),
                items,
            })
        })
        .collect()
}

fn native_items(items: &[ResolvedMenuItem]) -> Vec<NativeMenuItem> {
    let mut native = Vec::new();
    let mut pending_separator = false;
    for item in items {
        match item {
            ResolvedMenuItem::Command {
                label,
                action,
                enabled: true,
                ..
            } => {
                push_pending_separator(&mut native, &mut pending_separator);
                native.push(NativeMenuItem::action(label.clone(), action.clone()));
            }
            ResolvedMenuItem::Command { enabled: false, .. } => {}
            ResolvedMenuItem::Submenu {
                label, children, ..
            } => {
                let children = native_items(children);
                if !children.is_empty() {
                    push_pending_separator(&mut native, &mut pending_separator);
                    native.push(NativeMenuItem::submenu(Menu {
                        name: label.clone().into(),
                        items: children,
                    }));
                }
            }
            ResolvedMenuItem::Separator => {
                if !native.is_empty() {
                    pending_separator = true;
                }
            }
        }
    }
    native
}

fn push_pending_separator(native: &mut Vec<NativeMenuItem>, pending: &mut bool) {
    if *pending {
        native.push(NativeMenuItem::separator());
        *pending = false;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gpui::MenuItem as GpuiMenuItem;
    use key_extension_api::{
        BooleanSource, ContributionId, ContributionOrder, ExtensionId, LocalId, MenuContribution,
        MenuItemOrder, StateBinding,
    };
    use key_extension_host::OwnedMenu;

    use super::*;

    fn id(value: &str) -> ExtensionId {
        ExtensionId::parse(value).unwrap()
    }

    fn local(value: &str) -> LocalId {
        LocalId::parse(value).unwrap()
    }

    fn command_item(label: &str, visible: BooleanSource, enabled: BooleanSource) -> MenuItem {
        MenuItem {
            id: local(&label.to_ascii_lowercase()),
            order: MenuItemOrder::default(),
            visible,
            kind: MenuItemKind::Command {
                label: label.into(),
                command: CommandId::parse("org.example.menu/select-theme").unwrap(),
                payload: Some(DataValue::String(label.into())),
                icon: Some(IconRef::Host("palette".into())),
                enabled,
                checked: Some(BooleanSource::Binding(StateBinding::new(["selected"]))),
            },
        }
    }

    fn contributions(items: Vec<MenuItem>) -> CollectedContributions {
        CollectedContributions {
            menus: vec![OwnedMenu {
                owner: id("org.example.menu"),
                state: BTreeMap::new(),
                menu: MenuContribution {
                    id: ContributionId::parse("org.example.menu/themes").unwrap(),
                    slot: MenuSlotId::parse("view.appearance").unwrap(),
                    order: ContributionOrder::default(),
                    items,
                },
            }],
            ..CollectedContributions::default()
        }
    }

    #[test]
    fn bindings_filter_and_resolve_nested_menu_entries() {
        let state = BoundedStateMap::new(
            BTreeMap::from([
                ("show".into(), DataValue::Boolean(true)),
                ("enabled".into(), DataValue::Boolean(false)),
                ("selected".into(), DataValue::Boolean(true)),
            ]),
            Default::default(),
        )
        .unwrap();
        let mut input = contributions(vec![
            MenuItem {
                id: local("folder"),
                order: MenuItemOrder::default(),
                visible: BooleanSource::Binding(StateBinding::new(["show"])),
                kind: MenuItemKind::Submenu {
                    label: "Themes".into(),
                    icon: None,
                    children: vec![
                        MenuItem {
                            id: local("leading-separator"),
                            order: MenuItemOrder::default(),
                            visible: BooleanSource::Constant(true),
                            kind: MenuItemKind::Separator,
                        },
                        command_item(
                            "Dark",
                            BooleanSource::Constant(true),
                            BooleanSource::Binding(StateBinding::new(["enabled"])),
                        ),
                    ],
                },
            },
            command_item(
                "Hidden",
                BooleanSource::Constant(false),
                BooleanSource::Constant(true),
            ),
        ]);
        input.menus[0].state = state.values().clone();
        let slots = resolve_menu_slots(&input);
        assert_eq!(slots.len(), 1);
        let ResolvedMenuItem::Submenu { children, .. } = &slots[0].items[0] else {
            panic!("expected submenu")
        };
        let ResolvedMenuItem::Command {
            action,
            enabled,
            checked,
            ..
        } = &children[0]
        else {
            panic!("expected command")
        };
        assert!(!enabled);
        assert_eq!(*checked, Some(true));
        assert_eq!(action.payload, Some(DataValue::String("Dark".into())));
        assert!(native_menu_slots(&slots).is_empty());
    }

    #[test]
    fn native_menu_action_preserves_typed_payload() {
        let slots = resolve_menu_slots(&contributions(vec![command_item(
            "Dark",
            BooleanSource::Constant(true),
            BooleanSource::Constant(true),
        )]));
        let native = native_menu_slots(&slots);
        let GpuiMenuItem::Action { action, .. } = &native[0].items[0] else {
            panic!("expected action")
        };
        let action = action
            .as_any()
            .downcast_ref::<InvokeExtensionCommand>()
            .unwrap();
        assert_eq!(action.payload, Some(DataValue::String("Dark".into())));
    }

    #[test]
    fn native_conversion_normalizes_separators_after_disabled_entries_are_omitted() {
        let enabled = InvokeExtensionCommand {
            command: CommandId::parse("org.example.menu/select-theme").unwrap(),
            payload: None,
        };
        let slots = vec![ResolvedMenuSlot {
            slot: MenuSlotId::parse("view.appearance").unwrap(),
            items: vec![
                ResolvedMenuItem::Command {
                    label: "First".into(),
                    action: enabled.clone(),
                    icon: None,
                    enabled: true,
                    checked: None,
                },
                ResolvedMenuItem::Separator,
                ResolvedMenuItem::Command {
                    label: "Disabled".into(),
                    action: enabled.clone(),
                    icon: None,
                    enabled: false,
                    checked: None,
                },
                ResolvedMenuItem::Separator,
                ResolvedMenuItem::Command {
                    label: "Last".into(),
                    action: enabled,
                    icon: None,
                    enabled: true,
                    checked: None,
                },
                ResolvedMenuItem::Separator,
            ],
        }];

        let native = native_menu_slots(&slots);
        assert_eq!(native[0].items.len(), 3);
        assert!(matches!(native[0].items[0], GpuiMenuItem::Action { .. }));
        assert!(matches!(native[0].items[1], GpuiMenuItem::Separator));
        assert!(matches!(native[0].items[2], GpuiMenuItem::Action { .. }));
    }
}
