use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, Styled, StyledImage as _, Subscription, Window, div, img,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    progress::Progress,
    text::TextView,
    v_flex,
};
use key_extension_api::{
    CommandId, DataValue, ExtensionId, IconRef, LocalId, PackagePath, StateBinding, TextEmphasis,
    UiContribution, UiNode, UiNodeKind, UiTone,
};
use key_extension_host::OwnedView;

use crate::{BoundedStateMap, InvokeExtensionCommand, resolve_host_icon};

/// Purely resolved UI state used by both the renderer and non-GPUI tests.
/// Only the selected tab's content is part of the visible tree.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedUiState {
    pub visible_nodes: Vec<LocalId>,
    pub selected_tabs: BTreeMap<LocalId, usize>,
}

/// Resolve visibility and tab selection without constructing GPUI elements.
#[must_use]
pub fn resolve_ui_state(root: &UiNode, state: &BoundedStateMap) -> ResolvedUiState {
    let mut resolved = ResolvedUiState::default();
    resolve_node_state(root, state, &mut resolved);
    resolved
}

fn resolve_node_state(node: &UiNode, state: &BoundedStateMap, resolved: &mut ResolvedUiState) {
    if !state.resolve_boolean(&node.visible) {
        return;
    }
    resolved.visible_nodes.push(node.id.clone());
    match &node.kind {
        UiNodeKind::Tabs { tabs, selected, .. } => {
            if tabs.is_empty() {
                return;
            }
            let ids = tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>();
            let index = state.selected_index(selected, &ids).unwrap_or(0);
            resolved.selected_tabs.insert(node.id.clone(), index);
            resolve_node_state(&tabs[index].content, state, resolved);
        }
        _ => {
            for child in node.kind.children() {
                resolve_node_state(child, state, resolved);
            }
        }
    }
}

#[derive(Clone)]
struct TextFieldSpec {
    binding: StateBinding,
}

/// A trusted host-owned renderer for one already-validated contribution.
///
/// The extension supplies data, never GPUI callbacks or handles. All focus,
/// theme, asset loading, and action dispatch stay under host control.
pub struct DeclarativeView {
    owner: ExtensionId,
    contribution: UiContribution,
    state: BoundedStateMap,
    inputs: BTreeMap<LocalId, Entity<InputState>>,
    input_specs: BTreeMap<LocalId, TextFieldSpec>,
    _subscriptions: Vec<Subscription>,
}

impl DeclarativeView {
    /// Build a retained renderer. The supplied contribution must already have
    /// passed `key-extension-api` and `key-extension-host` validation.
    #[must_use]
    pub fn new(
        owned: OwnedView,
        state: BoundedStateMap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut fields = Vec::new();
        collect_text_fields(&owned.view.root, &mut fields);

        let mut inputs = BTreeMap::new();
        let mut input_specs = BTreeMap::new();
        let mut subscriptions = Vec::new();

        for (id, label, binding, command, maximum_bytes) in fields {
            let initial_value = state
                .resolve_string(&binding)
                .unwrap_or_default()
                .to_owned();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(initial_value)
                    .placeholder(label)
                    .validate(move |text, _| text.len() <= maximum_bytes as usize)
            });
            let subscription_binding = binding.clone();
            let subscription_command = command.clone();
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input: &Entity<InputState>, event: &InputEvent, window, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    if this.state.resolve_string(&subscription_binding) == Some(value.as_str()) {
                        return;
                    }
                    dispatch_command(
                        window,
                        cx,
                        subscription_command.clone(),
                        Some(DataValue::String(value)),
                    );
                },
            ));
            inputs.insert(id.clone(), input);
            input_specs.insert(id, TextFieldSpec { binding });
        }

        Self {
            owner: owned.owner,
            contribution: owned.view,
            state,
            inputs,
            input_specs,
            _subscriptions: subscriptions,
        }
    }

    #[must_use]
    pub fn owner(&self) -> &ExtensionId {
        &self.owner
    }

    #[must_use]
    pub fn contribution(&self) -> &UiContribution {
        &self.contribution
    }

    #[must_use]
    pub fn state(&self) -> &BoundedStateMap {
        &self.state
    }

    /// Replace the immutable snapshot and synchronize retained text fields.
    /// Programmatic synchronization does not echo a command back to the host.
    pub fn set_state(
        &mut self,
        state: BoundedStateMap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state = state;
        let changes = self
            .input_specs
            .iter()
            .filter_map(|(id, spec)| {
                let desired = self
                    .state
                    .resolve_string(&spec.binding)
                    .unwrap_or_default()
                    .to_owned();
                self.inputs.get(id).map(|input| (input.clone(), desired))
            })
            .collect::<Vec<_>>();
        for (input, desired) in changes {
            if input.read(cx).value().as_ref() != desired {
                input.update(cx, |input, cx| input.set_value(desired, window, cx));
            }
        }
        cx.notify();
    }

    fn render_node(&self, node: &UiNode, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        if !self.state.resolve_boolean(&node.visible) {
            return None;
        }

        let element =
            match &node.kind {
                UiNodeKind::Column { children } => v_flex()
                    .gap_2()
                    .children(
                        children
                            .iter()
                            .filter_map(|child| self.render_node(child, window, cx)),
                    )
                    .into_any_element(),
                UiNodeKind::Row { children } => h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(
                        children
                            .iter()
                            .filter_map(|child| self.render_node(child, window, cx)),
                    )
                    .into_any_element(),
                UiNodeKind::Stack { children } => {
                    let mut children = children
                        .iter()
                        .filter_map(|child| self.render_node(child, window, cx));
                    let Some(first) = children.next() else {
                        return Some(div().into_any_element());
                    };
                    div()
                        .relative()
                        .child(first)
                        .children(children.map(|child| {
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .left_0()
                                .child(child)
                        }))
                        .into_any_element()
                }
                UiNodeKind::Text { text, selectable } => TextView::html(
                    self.element_key(&node.id, "text"),
                    plain_text_html(text),
                    window,
                    cx,
                )
                .selectable(*selectable)
                .into_any_element(),
                UiNodeKind::StyledText { spans, selectable } => {
                    let html = spans
                        .iter()
                        .map(|span| {
                            let escaped = plain_text_html(&span.text);
                            match span.emphasis {
                                TextEmphasis::Normal | TextEmphasis::Muted => escaped,
                                TextEmphasis::Strong => format!("<strong>{escaped}</strong>"),
                                TextEmphasis::Code => format!("<code>{escaped}</code>"),
                            }
                        })
                        .collect::<String>();
                    TextView::html(self.element_key(&node.id, "styled-text"), html, window, cx)
                        .selectable(*selectable)
                        .into_any_element()
                }
                UiNodeKind::Markdown {
                    markdown,
                    selectable,
                } => TextView::markdown(
                    self.element_key(&node.id, "markdown"),
                    markdown.clone(),
                    window,
                    cx,
                )
                .selectable(*selectable)
                .into_any_element(),
                UiNodeKind::Button {
                    label,
                    command,
                    payload,
                } => Button::new(self.element_key(&node.id, "button"))
                    .label(label.clone())
                    .small()
                    .on_click(command_handler(command.clone(), payload.clone()))
                    .into_any_element(),
                UiNodeKind::IconButton {
                    icon,
                    label,
                    command,
                } => Button::new(self.element_key(&node.id, "icon-button"))
                    .icon(self.render_icon(icon))
                    .tooltip(label.clone())
                    .small()
                    .compact()
                    .on_click(command_handler(command.clone(), None))
                    .into_any_element(),
                UiNodeKind::Toggle {
                    label,
                    value,
                    command,
                } => {
                    let checked =
                        matches!(self.state.resolve(value), Some(DataValue::Boolean(true)));
                    let command = command.clone();
                    Checkbox::new(self.element_key(&node.id, "toggle"))
                        .label(label.clone())
                        .checked(checked)
                        .small()
                        .on_click(move |checked, window, cx| {
                            dispatch_command(
                                window,
                                cx,
                                command.clone(),
                                Some(DataValue::Boolean(*checked)),
                            );
                        })
                        .into_any_element()
                }
                UiNodeKind::Select {
                    label,
                    value,
                    options,
                    command,
                } => {
                    let selected = self.state.resolve(value);
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(label.clone()),
                        )
                        .child(h_flex().gap_1().flex_wrap().children(
                            options.iter().enumerate().map(|(index, option)| {
                                Button::new(self.element_key(&node.id, &format!("select-{index}")))
                                    .label(option.label.clone())
                                    .small()
                                    .selected(selected == Some(&option.value))
                                    .on_click(command_handler(
                                        command.clone(),
                                        Some(option.value.clone()),
                                    ))
                            }),
                        ))
                        .into_any_element()
                }
                UiNodeKind::TextField { label, .. } => {
                    let Some(input) = self.inputs.get(&node.id) else {
                        return Some(missing_control(label, cx));
                    };
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(label.clone()),
                        )
                        .child(Input::new(input).small().w_full())
                        .into_any_element()
                }
                UiNodeKind::List { children } => v_flex()
                    .gap_1()
                    .children(children.iter().filter_map(|child| {
                        self.render_node(child, window, cx).map(|child| {
                            h_flex()
                                .items_start()
                                .gap_2()
                                .child(div().text_color(cx.theme().muted_foreground).child("•"))
                                .child(div().flex_1().child(child))
                        })
                    }))
                    .into_any_element(),
                UiNodeKind::Tabs {
                    tabs,
                    selected,
                    command,
                } => {
                    if tabs.is_empty() {
                        return Some(div().into_any_element());
                    }
                    let ids = tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>();
                    let selected_index = self.state.selected_index(selected, &ids).unwrap_or(0);
                    let tab_buttons =
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .children(tabs.iter().enumerate().map(|(index, tab)| {
                                Button::new(
                                    self.element_key(&node.id, &format!("tab-{}", tab.id.as_str())),
                                )
                                .label(tab.label.clone())
                                .small()
                                .ghost()
                                .selected(index == selected_index)
                                .on_click(command_handler(
                                    command.clone(),
                                    Some(DataValue::String(tab.id.as_str().to_owned())),
                                ))
                            }));
                    v_flex()
                        .gap_2()
                        .child(tab_buttons)
                        .children(self.render_node(&tabs[selected_index].content, window, cx))
                        .into_any_element()
                }
                UiNodeKind::Badge { label, tone } => {
                    let (background, foreground) = match tone {
                        UiTone::Neutral => (cx.theme().muted, cx.theme().muted_foreground),
                        UiTone::Accent => (cx.theme().primary, cx.theme().primary_foreground),
                        UiTone::Positive => (cx.theme().success, cx.theme().success_foreground),
                        UiTone::Caution => (cx.theme().warning, cx.theme().warning_foreground),
                        UiTone::Critical => (cx.theme().danger, cx.theme().danger_foreground),
                    };
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .rounded_full()
                        .text_xs()
                        .bg(background)
                        .text_color(foreground)
                        .child(label.clone())
                        .into_any_element()
                }
                UiNodeKind::Divider => div()
                    .h(px(1.))
                    .w_full()
                    .bg(cx.theme().border)
                    .into_any_element(),
                UiNodeKind::Image {
                    asset,
                    alternative_text,
                } => {
                    let fallback_text = alternative_text.clone();
                    let fallback_color = cx.theme().muted_foreground;
                    img(extension_asset_key(&self.owner, asset))
                        .max_w_full()
                        .max_h(px(320.))
                        .with_fallback(move || {
                            h_flex()
                                .gap_2()
                                .text_color(fallback_color)
                                .child(Icon::new(IconName::File))
                                .child(fallback_text.clone())
                                .into_any_element()
                        })
                        .into_any_element()
                }
                UiNodeKind::Progress {
                    label,
                    basis_points,
                } => v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(label.clone()),
                    )
                    .child(Progress::new().value(f32::from(*basis_points) / 100.0))
                    .into_any_element(),
                UiNodeKind::Spacer => div().flex_grow().into_any_element(),
            };
        Some(element)
    }

    fn element_key(&self, node: &LocalId, suffix: &str) -> SharedString {
        format!(
            "key-extension/{}/{}/{}/{}",
            self.owner.as_str(),
            self.contribution.id.as_str(),
            node.as_str(),
            suffix
        )
        .into()
    }

    fn render_icon(&self, icon: &IconRef) -> Icon {
        match icon {
            IconRef::Host(name) => resolve_host_icon(name)
                .map(Icon::new)
                .unwrap_or_else(|| Icon::new(IconName::File)),
            IconRef::Asset(path) => Icon::empty().path(extension_asset_key(&self.owner, path)),
        }
    }
}

impl Render for DeclarativeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "key-extension-view/{}",
                self.contribution.id.as_str()
            )))
            .children(self.render_node(&self.contribution.root, window, cx))
    }
}

fn collect_text_fields(
    node: &UiNode,
    fields: &mut Vec<(LocalId, String, StateBinding, CommandId, u32)>,
) {
    if let UiNodeKind::TextField {
        label,
        value,
        command,
        maximum_bytes,
    } = &node.kind
    {
        fields.push((
            node.id.clone(),
            label.clone(),
            value.clone(),
            command.clone(),
            *maximum_bytes,
        ));
    }
    for child in node.kind.children() {
        collect_text_fields(child, fields);
    }
    if let UiNodeKind::Tabs { tabs, .. } = &node.kind {
        for tab in tabs {
            collect_text_fields(&tab.content, fields);
        }
    }
}

fn dispatch_command(
    window: &mut Window,
    cx: &mut App,
    command: CommandId,
    payload: Option<DataValue>,
) {
    window.dispatch_action(Box::new(InvokeExtensionCommand { command, payload }), cx);
}

fn command_handler(
    command: CommandId,
    payload: Option<DataValue>,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| dispatch_command(window, cx, command.clone(), payload.clone())
}

fn missing_control(label: &str, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::TriangleAlert))
        .child(label.to_owned())
        .into_any_element()
}

/// Embedded assets are namespaced by their validated owner. The application's
/// trusted `AssetSource` serves these virtual keys from the installed package;
/// they are never interpreted as operating-system paths.
#[must_use]
pub fn extension_asset_key(owner: &ExtensionId, path: &PackagePath) -> SharedString {
    format!("key-extensions/{}/{}", owner.as_str(), path.as_str()).into()
}

fn plain_text_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '\n' => escaped.push_str("<br>"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use key_extension_api::{BooleanSource, UiTab};

    use super::*;

    fn local(value: &str) -> LocalId {
        LocalId::parse(value).unwrap()
    }

    fn text(id: &str) -> UiNode {
        UiNode {
            id: local(id),
            visible: BooleanSource::Constant(true),
            kind: UiNodeKind::Text {
                text: id.into(),
                selectable: true,
            },
        }
    }

    #[test]
    fn pure_resolver_filters_hidden_nodes_and_only_walks_selected_tab() {
        let root = UiNode {
            id: local("root"),
            visible: BooleanSource::Constant(true),
            kind: UiNodeKind::Column {
                children: vec![
                    UiNode {
                        id: local("hidden"),
                        visible: BooleanSource::Binding(StateBinding::new(["show-hidden"])),
                        kind: UiNodeKind::Text {
                            text: "secret".into(),
                            selectable: true,
                        },
                    },
                    UiNode {
                        id: local("tabs"),
                        visible: BooleanSource::Constant(true),
                        kind: UiNodeKind::Tabs {
                            tabs: vec![
                                UiTab {
                                    id: local("overview"),
                                    label: "Overview".into(),
                                    content: text("overview-content"),
                                },
                                UiTab {
                                    id: local("details"),
                                    label: "Details".into(),
                                    content: text("details-content"),
                                },
                            ],
                            selected: StateBinding::new(["active-tab"]),
                            command: CommandId::parse("org.example.reader/select-tab").unwrap(),
                        },
                    },
                ],
            },
        };
        let state = BoundedStateMap::new(
            BTreeMap::from([
                ("show-hidden".into(), DataValue::Boolean(false)),
                ("active-tab".into(), DataValue::String("details".into())),
            ]),
            Default::default(),
        )
        .unwrap();

        let resolved = resolve_ui_state(&root, &state);
        assert_eq!(
            resolved.visible_nodes,
            vec![local("root"), local("tabs"), local("details-content")]
        );
        assert_eq!(resolved.selected_tabs.get(&local("tabs")), Some(&1));
    }

    #[test]
    fn pure_resolver_defaults_invalid_tab_selection_to_first() {
        let root = UiNode {
            id: local("tabs"),
            visible: BooleanSource::Constant(true),
            kind: UiNodeKind::Tabs {
                tabs: vec![UiTab {
                    id: local("overview"),
                    label: "Overview".into(),
                    content: text("content"),
                }],
                selected: StateBinding::new(["missing"]),
                command: CommandId::parse("org.example.reader/select-tab").unwrap(),
            },
        };
        let resolved = resolve_ui_state(&root, &BoundedStateMap::default());
        assert_eq!(resolved.selected_tabs.get(&local("tabs")), Some(&0));
        assert_eq!(
            resolved.visible_nodes,
            vec![local("tabs"), local("content")]
        );
    }

    #[test]
    fn asset_keys_are_namespaced_and_plain_text_is_escaped() {
        let owner = ExtensionId::parse("org.example.reader").unwrap();
        let path = PackagePath::parse("images/preview.png").unwrap();
        assert_eq!(
            extension_asset_key(&owner, &path).as_ref(),
            "key-extensions/org.example.reader/images/preview.png"
        );
        assert_eq!(
            plain_text_html("<one> &\n\"two\""),
            "&lt;one&gt; &amp;<br>&quot;two&quot;"
        );
    }
}
