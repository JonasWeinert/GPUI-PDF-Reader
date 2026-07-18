//! GPUI window shell shared by document and host-owned workspace views.

use crate::application_host::{
    ApplicationHost, activate_workspace_view, idle_workspace_view, open_pdf_from_window,
    open_settings_window, remove_workspace_view, select_workspace_tab, set_resource_mode,
};
use crate::reader::PdfReader;
use crate::text_field::{TextField, TextFieldEvent};
use crate::{OpenDocument, OpenSettings};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions,
    Render, ScrollHandle, SharedString, Window, div, prelude::*, px,
};
use gpui_component::{Icon, IconName, Theme};
use key_ui_gpui::{
    TAB_BAR_HEIGHT, TabBarAction, TabHoverCard, TabIndexAction, TabPresentation, TabSearchPopover,
    TabStrip, ThemeTokens,
};
use key_workspace_core::{
    DockPanel, ItemKind, ResourceAllocation, ResourceMode, ViewId, WorkspaceItemDescriptor,
    WorkspaceLayout, WorkspaceViewDescriptor, WorkspaceWindowDescriptor,
};
use std::path::PathBuf;
use std::rc::Rc;

enum WorkspaceContent {
    Pdf(Entity<PdfReader>),
    Settings,
}

struct WorkspaceTab {
    item: Option<WorkspaceItemDescriptor>,
    view: WorkspaceViewDescriptor,
    layout: WorkspaceLayout,
    content: WorkspaceContent,
}

impl WorkspaceTab {
    fn source_detail(&self) -> SharedString {
        match self.item.as_ref().map(|item| &item.source) {
            Some(key_workspace_core::ItemSource::File(path)) => path.display().to_string().into(),
            _ if self.view.kind == ItemKind::Settings => "Application settings".into(),
            _ => "New PDF tab".into(),
        }
    }

    fn hover_detail(&self, cx: &App) -> SharedString {
        match &self.content {
            WorkspaceContent::Pdf(reader) => {
                let source = self.source_detail();
                let state = reader.read(cx).tab_detail();
                format!("{state}  ·  {source}").into()
            }
            WorkspaceContent::Settings => self.source_detail(),
        }
    }
}

/// Window-scoped chrome and placement owner. Document views own their local
/// panels; left/right/bottom docks here are reserved for workspace-wide tools.
pub(crate) struct WorkspaceWindow {
    host: Entity<ApplicationHost>,
    descriptor: WorkspaceWindowDescriptor,
    tabs: Vec<WorkspaceTab>,
    active_tab: usize,
    tab_scroll: ScrollHandle,
    hovered_tab: Option<usize>,
    tab_search_open: bool,
    tab_search_query: String,
    tab_search_field: Entity<TextField>,
    focus_handle: FocusHandle,
    warning: Option<SharedString>,
}

impl WorkspaceWindow {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_pdf(
        host: Entity<ApplicationHost>,
        descriptor: WorkspaceWindowDescriptor,
        item: Option<WorkspaceItemDescriptor>,
        view: WorkspaceViewDescriptor,
        initial_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let reader = PdfReader::new(
            initial_path,
            host.clone(),
            descriptor.id,
            view.id,
            window,
            cx,
        );
        let focus_handle = cx.focus_handle();
        let tab_search_field = cx.new(|cx| TextField::new(cx, "Search open tabs", 512));
        let field_for_events = tab_search_field.clone();
        let entity: Entity<Self> = cx.new(|cx| {
            cx.subscribe_in(
                &field_for_events,
                window,
                |workspace: &mut WorkspaceWindow, _, event, window, cx| match event {
                    TextFieldEvent::Changed(query) => {
                        workspace.tab_search_query = query.clone();
                        cx.notify();
                    }
                    TextFieldEvent::Submit => {
                        workspace.activate_first_tab_search_result(window, cx)
                    }
                    TextFieldEvent::Cancel => {
                        workspace.tab_search_open = false;
                        cx.notify();
                    }
                    TextFieldEvent::Rejected(rejection) => {
                        workspace.warning = Some(rejection.to_string().into());
                        cx.notify();
                    }
                },
            )
            .detach();
            Self {
                host,
                descriptor,
                tabs: vec![WorkspaceTab {
                    item,
                    layout: WorkspaceLayout::single_view(view.id),
                    view,
                    content: WorkspaceContent::Pdf(reader),
                }],
                active_tab: 0,
                tab_scroll: ScrollHandle::new(),
                hovered_tab: None,
                tab_search_open: false,
                tab_search_query: String::new(),
                tab_search_field,
                focus_handle,
                warning: None,
            }
        });
        Self::observe_activation(&entity, window, cx);
        entity
    }

    pub(crate) fn new_settings(
        host: Entity<ApplicationHost>,
        descriptor: WorkspaceWindowDescriptor,
        view: WorkspaceViewDescriptor,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let focus_handle = cx.focus_handle();
        let mut layout = WorkspaceLayout::single_view(view.id);
        layout.docks.left.open = true;
        layout.docks.left.extent = 216.0;
        layout.docks.left.panels.push(DockPanel {
            id: "core.settings-navigation".into(),
            title: "Settings".into(),
            visible: true,
        });
        layout.docks.left.active_panel = Some("core.settings-navigation".into());
        let tab_search_field = cx.new(|cx| TextField::new(cx, "Search open tabs", 512));
        let field_for_events = tab_search_field.clone();
        let entity: Entity<Self> = cx.new(|cx| {
            cx.subscribe_in(
                &field_for_events,
                window,
                |workspace: &mut WorkspaceWindow, _, event, window, cx| match event {
                    TextFieldEvent::Changed(query) => {
                        workspace.tab_search_query = query.clone();
                        cx.notify();
                    }
                    TextFieldEvent::Submit => {
                        workspace.activate_first_tab_search_result(window, cx)
                    }
                    TextFieldEvent::Cancel => {
                        workspace.tab_search_open = false;
                        cx.notify();
                    }
                    TextFieldEvent::Rejected(rejection) => {
                        workspace.warning = Some(rejection.to_string().into());
                        cx.notify();
                    }
                },
            )
            .detach();
            Self {
                host,
                descriptor,
                tabs: vec![WorkspaceTab {
                    item: None,
                    view,
                    layout,
                    content: WorkspaceContent::Settings,
                }],
                active_tab: 0,
                tab_scroll: ScrollHandle::new(),
                hovered_tab: None,
                tab_search_open: false,
                tab_search_query: String::new(),
                tab_search_field,
                focus_handle,
                warning: None,
            }
        });
        Self::observe_activation(&entity, window, cx);
        entity
    }

    fn observe_activation(entity: &Entity<Self>, window: &mut Window, cx: &mut App) {
        entity.update(cx, |_, cx| {
            cx.observe_window_activation(window, |workspace, window, cx| {
                workspace.activation_changed(window, cx);
            })
            .detach();
        });
    }

    fn activation_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.active_view().id;
        if !window.is_window_active() {
            idle_workspace_view(self.host.clone(), view, cx);
            return;
        }
        let changed = activate_workspace_view(self.host.clone(), view, cx);
        if changed && let WorkspaceContent::Pdf(reader) = &self.active_tab().content {
            reader.update(cx, |reader, cx| reader.activate_extension_scope(window, cx));
        }
    }

    fn active_tab(&self) -> &WorkspaceTab {
        &self.tabs[self.active_tab]
    }

    fn active_view(&self) -> &WorkspaceViewDescriptor {
        &self.active_tab().view
    }

    pub(crate) fn window_id(&self) -> key_workspace_core::WindowId {
        self.descriptor.id
    }

    pub(crate) fn apply_resource_allocation(
        &mut self,
        allocation: ResourceAllocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = ViewId::from_raw(allocation.id.get());
        if let Some(WorkspaceContent::Pdf(reader)) = self
            .tabs
            .iter()
            .find(|tab| tab.view.id == view)
            .map(|tab| &tab.content)
        {
            reader.update(cx, |reader, cx| {
                reader.apply_resource_allocation(allocation, window, cx)
            });
        }
        cx.notify();
    }

    pub(crate) fn focus_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.active_tab().content {
            WorkspaceContent::Pdf(reader) => window.focus(&reader.read(cx).focus_handle(cx)),
            WorkspaceContent::Settings => window.focus(&self.focus_handle),
        }
    }

    pub(crate) fn reader(&self) -> Option<Entity<PdfReader>> {
        match &self.active_tab().content {
            WorkspaceContent::Pdf(reader) => Some(reader.clone()),
            WorkspaceContent::Settings => None,
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn readers(&self) -> Vec<Entity<PdfReader>> {
        self.tabs
            .iter()
            .filter_map(|tab| match &tab.content {
                WorkspaceContent::Pdf(reader) => Some(reader.clone()),
                WorkspaceContent::Settings => None,
            })
            .collect()
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_tab_index(index, window, cx);
    }

    pub(crate) fn open_pdf(
        &mut self,
        path: PathBuf,
        item: WorkspaceItemDescriptor,
        view: WorkspaceViewDescriptor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[self.active_tab];
        let WorkspaceContent::Pdf(reader) = &tab.content else {
            return;
        };
        self.descriptor.title = item.title.clone();
        tab.item = Some(item);
        tab.view = view;
        window.set_window_title(&format!("{} — GPUI PDF Reader", tab.view.title));
        reader.update(cx, |reader, cx| reader.open_path(path, window, cx));
        self.focus_content(window, cx);
        cx.notify();
    }

    pub(crate) fn add_pdf_tab(
        &mut self,
        item: Option<WorkspaceItemDescriptor>,
        view: WorkspaceViewDescriptor,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reader = PdfReader::new(
            Some(path),
            self.host.clone(),
            self.descriptor.id,
            view.id,
            window,
            cx,
        );
        self.tabs.push(WorkspaceTab {
            item,
            layout: WorkspaceLayout::single_view(view.id),
            view,
            content: WorkspaceContent::Pdf(reader),
        });
        self.active_tab = self.tabs.len() - 1;
        self.tab_scroll.scroll_to_item(self.active_tab);
        self.finish_tab_activation(window, cx);
    }

    pub(crate) fn add_settings_tab(
        &mut self,
        view: WorkspaceViewDescriptor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut layout = WorkspaceLayout::single_view(view.id);
        layout.docks.left.open = true;
        layout.docks.left.extent = 216.0;
        layout.docks.left.panels.push(DockPanel {
            id: "core.settings-navigation".into(),
            title: "Settings".into(),
            visible: true,
        });
        layout.docks.left.active_panel = Some("core.settings-navigation".into());
        self.tabs.push(WorkspaceTab {
            item: None,
            view,
            layout,
            content: WorkspaceContent::Settings,
        });
        self.active_tab = self.tabs.len() - 1;
        self.tab_scroll.scroll_to_item(self.active_tab);
        self.finish_tab_activation(window, cx);
    }

    pub(crate) fn activate_tab(
        &mut self,
        view: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.view.id == view) {
            self.activate_tab_index(index, window, cx);
        }
    }

    fn activate_tab_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active_tab = index;
        self.tab_search_open = false;
        self.hovered_tab = None;
        self.tab_scroll.scroll_to_item(index);
        self.finish_tab_activation(window, cx);
    }

    fn finish_tab_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.active_view().clone();
        self.descriptor.active_view = Some(view.id);
        self.descriptor.title = view.title.clone();
        window.set_window_title(&format!("{} — GPUI PDF Reader", view.title));
        let changed = select_workspace_tab(
            self.host.clone(),
            self.descriptor.id,
            view.id,
            view.title,
            cx,
        );
        if changed && let WorkspaceContent::Pdf(reader) = &self.active_tab().content {
            reader.update(cx, |reader, cx| reader.activate_extension_scope(window, cx));
        }
        self.focus_content(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if let WorkspaceContent::Pdf(reader) = &self.tabs[index].content
            && reader.read(cx).tab_close_requires_confirmation()
        {
            self.warning =
                Some("Finish or discard the open comment draft before closing this tab.".into());
            cx.notify();
            return;
        }
        if self.tabs.len() == 1 {
            window.remove_window();
            return;
        }
        if let WorkspaceContent::Pdf(reader) = &self.tabs[index].content {
            reader.update(cx, |reader, _| reader.prepare_tab_close());
        }
        let previous_count = self.tabs.len();
        let removed = self.tabs.remove(index);
        self.active_tab = active_index_after_close(previous_count, self.active_tab, index)
            .expect("closing one of several tabs leaves an active tab");
        let next = self.active_view().clone();
        remove_workspace_view(
            self.host.clone(),
            self.descriptor.id,
            removed.view.id,
            Some((next.id, next.title.clone())),
            cx,
        );
        self.finish_tab_activation(window, cx);
    }

    fn activate_first_tab_search_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.filtered_tab_indices().into_iter().next() {
            self.activate_tab_index(index, window, cx);
        }
    }

    fn filtered_tab_indices(&self) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                tab_matches_query(
                    &tab.view.title,
                    tab.source_detail().as_ref(),
                    &self.tab_search_query,
                )
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn open_dialog(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.show_open_dialog(window, cx);
    }

    fn show_open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open PDF".into()),
        });
        let weak = cx.weak_entity();
        let host = self.host.clone();
        let source = self.descriptor.id;
        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = prompt.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let _ = cx.update(|window, cx| {
                        if let Err(error) = open_pdf_from_window(host, source, path, window, cx) {
                            weak.update(cx, |workspace, cx| {
                                workspace.warning = Some(error.into());
                                cx.notify();
                            })
                            .ok();
                        }
                    });
                }
            })
            .detach();
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let tabs = self
            .tabs
            .iter()
            .map(|tab| TabPresentation::new(tab.view.title.clone(), tab.hover_detail(cx)))
            .collect();
        let weak = cx.weak_entity();
        let activate_weak = weak.clone();
        let activate: TabIndexAction = Rc::new(move |index, _, window, cx| {
            activate_weak
                .update(cx, |workspace, cx| {
                    workspace.activate_tab_index(index, window, cx)
                })
                .ok();
        });
        let close_weak = weak.clone();
        let close: TabIndexAction = Rc::new(move |index, _, window, cx| {
            close_weak
                .update(cx, |workspace, cx| workspace.close_tab(index, window, cx))
                .ok();
        });
        let hover_weak = weak.clone();
        let hover = Rc::new(move |index, _window: &mut Window, cx: &mut App| {
            hover_weak
                .update(cx, |workspace, cx| {
                    workspace.hovered_tab = index;
                    cx.notify();
                })
                .ok();
        });
        let sidebar: TabBarAction = Rc::new(|_, _, _| {});
        let search_weak = weak.clone();
        let search: TabBarAction = Rc::new(move |_, window, cx| {
            search_weak
                .update(cx, |workspace, cx| {
                    workspace.tab_search_open = !workspace.tab_search_open;
                    if workspace.tab_search_open {
                        window.focus(&workspace.tab_search_field.read(cx).focus_handle(cx));
                    }
                    cx.notify();
                })
                .ok();
        });
        let new_weak = weak;
        let new_tab: TabBarAction = Rc::new(move |_, window, cx| {
            new_weak
                .update(cx, |workspace, cx| workspace.show_open_dialog(window, cx))
                .ok();
        });
        TabStrip::new(
            tokens,
            tabs,
            self.active_tab,
            self.tab_scroll.clone(),
            activate,
            hover,
            sidebar,
            search,
            new_tab,
        )
        .on_close(close)
        .into_any_element()
    }

    fn render_tab_search(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let weak = cx.weak_entity();
        let rows = self
            .filtered_tab_indices()
            .into_iter()
            .map(|index| {
                let tab = &self.tabs[index];
                let active = index == self.active_tab;
                let title: SharedString = tab.view.title.clone().into();
                let detail = tab.source_detail();
                let weak = weak.clone();
                div()
                    .id(("tab-search-result", index))
                    .p_3()
                    .rounded_lg()
                    .cursor_pointer()
                    .bg(if active {
                        tokens.action.accent_soft
                    } else {
                        tokens.surface.overlay
                    })
                    .hover(move |row| row.bg(tokens.action.control_hover))
                    .on_click(move |_, window, cx| {
                        weak.update(cx, |workspace, cx| {
                            workspace.activate_tab_index(index, window, cx)
                        })
                        .ok();
                    })
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Icon::new(if tab.view.kind == ItemKind::Settings {
                            IconName::Settings
                        } else {
                            IconName::File
                        })
                        .size(px(16.0))
                        .text_color(tokens.action.accent),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_xs()
                                    .text_color(tokens.content.tertiary)
                                    .child(detail),
                            ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        TabSearchPopover::new(tokens, self.tab_search_field.clone(), rows).into_any_element()
    }

    fn render_tab_hover_card(&self, window: &Window, cx: &App) -> Option<gpui::AnyElement> {
        let index = self.hovered_tab.filter(|index| *index != self.active_tab)?;
        let tab = self.tabs.get(index)?;
        let width = f32::from(window.viewport_size().width);
        let x = self
            .tab_scroll
            .bounds_for_item(index)
            .map_or(112.0, |bounds| f32::from(bounds.origin.x))
            .clamp(8.0, (width - 258.0).max(8.0));
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        Some(
            div()
                .absolute()
                .top(px(TAB_BAR_HEIGHT + 6.0))
                .left(px(x))
                .child(TabHoverCard::new(
                    tokens,
                    tab.view.title.clone(),
                    tab.hover_detail(cx),
                ))
                .into_any_element(),
        )
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = open_settings_window(self.host.clone(), cx) {
            self.warning = Some(error.into());
            cx.notify();
        }
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let plan = self.host.read(cx).resource_coordinator().plan(&[]);
        let requested_mode = self.host.read(cx).requested_resource_mode();
        let effective_mode: SharedString = format!("{:?}", plan.effective_mode).into();
        let section = |title: &'static str, detail: &'static str| {
            div()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(tokens.surface.border)
                .bg(tokens.surface.overlay)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                        .child(
                            div()
                                .text_sm()
                                .text_color(tokens.content.secondary)
                                .child(detail),
                        ),
                )
                .child(Icon::new(IconName::ChevronRight).text_color(tokens.content.tertiary))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface.background)
            .text_color(tokens.content.primary)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .p_8()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(720.0))
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child("Application settings"))
                            .child(div().text_color(tokens.content.secondary).child("This is a real workspace view backed by the same host, placement, dock, and resource contracts as PDF views. Controls are intentionally illustrative for now."))
                            .child(section("Appearance", "Theme and document presentation"))
                            .child(section("Extensions", "Permissions, settings, and contributed tools"))
                            .child(
                                div()
                                    .p_4()
                                    .rounded_lg()
                                    .bg(tokens.action.accent_soft)
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Resource mode"))
                                            .child(div().text_sm().text_color(tokens.content.secondary).child(format!("Coordinates cache and background-work budgets across every view. Auto currently resolves to {effective_mode}."))),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_2()
                                            .children([
                                                ResourceMode::Auto,
                                                ResourceMode::Saver,
                                                ResourceMode::Balanced,
                                                ResourceMode::Performance,
                                            ].into_iter().enumerate().map(|(index, mode)| {
                                                let host = self.host.clone();
                                                let selected = mode == requested_mode;
                                                div()
                                                    .id(("resource-mode", index))
                                                    .px_3()
                                                    .py_1()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(if selected { tokens.action.accent } else { tokens.surface.border })
                                                    .bg(if selected { tokens.action.accent } else { tokens.surface.overlay })
                                                    .text_color(if selected { tokens.content.on_accent } else { tokens.content.primary })
                                                    .text_sm()
                                                    .cursor_pointer()
                                                    .hover(|style| style.bg(tokens.action.accent_hover))
                                                    .on_click(cx.listener(move |_, _, _, cx| {
                                                        set_resource_mode(host.clone(), mode, cx);
                                                        cx.notify();
                                                    }))
                                                    .child(format!("{mode:?}"))
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_settings_dock(&self, cx: &App) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let extent = self.active_tab().layout.docks.left.extent;
        div()
            .h_full()
            .w(px(extent))
            .flex_none()
            .px_3()
            .border_r_1()
            .border_color(tokens.surface.border)
            .bg(tokens.surface.sidebar)
            .child(
                div()
                    .mt_4()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(tokens.action.accent_soft)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("General"),
            )
            .child(
                div()
                    .mt_1()
                    .px_3()
                    .py_2()
                    .text_color(tokens.content.secondary)
                    .child("Workspace"),
            )
            .child(
                div()
                    .mt_1()
                    .px_3()
                    .py_2()
                    .text_color(tokens.content.secondary)
                    .child("Performance"),
            )
            .into_any_element()
    }
}

fn tab_matches_query(title: &str, detail: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || title.to_lowercase().contains(&query)
        || detail.to_lowercase().contains(&query)
}

fn active_index_after_close(tab_count: usize, active: usize, closed: usize) -> Option<usize> {
    if tab_count <= 1 || active >= tab_count || closed >= tab_count {
        return None;
    }
    let remaining = tab_count - 1;
    Some(if closed < active {
        active - 1
    } else if active >= remaining {
        remaining - 1
    } else {
        active
    })
}

#[cfg(test)]
mod tests {
    use super::{active_index_after_close, tab_matches_query};

    #[test]
    fn tab_search_matches_titles_and_paths_case_insensitively() {
        assert!(tab_matches_query(
            "Research Paper.pdf",
            "/Users/example/Documents/Research Paper.pdf",
            " research "
        ));
        assert!(tab_matches_query(
            "Paper.pdf",
            "/Users/example/Citations/Paper.pdf",
            "citations"
        ));
        assert!(!tab_matches_query("Paper.pdf", "/tmp/Paper.pdf", "notes"));
        assert!(tab_matches_query("Anything", "/tmp/a", "   "));
    }

    #[test]
    fn tab_close_keeps_a_stable_neighbor_active() {
        assert_eq!(active_index_after_close(4, 2, 0), Some(1));
        assert_eq!(active_index_after_close(4, 2, 2), Some(2));
        assert_eq!(active_index_after_close(4, 3, 3), Some(2));
        assert_eq!(active_index_after_close(1, 0, 0), None);
        assert_eq!(active_index_after_close(3, 4, 0), None);
    }
}

impl Focusable for WorkspaceWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.active_tab().content {
            WorkspaceContent::Pdf(reader) => reader.read(cx).focus_handle(cx),
            WorkspaceContent::Settings => self.focus_handle.clone(),
        }
    }
}

impl Render for WorkspaceWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.active_tab().content {
            WorkspaceContent::Pdf(reader) => reader.clone().into_any_element(),
            WorkspaceContent::Settings => self.render_settings(cx),
        };
        let left_dock = (self.active_tab().layout.docks.left.open
            && self.active_view().kind == ItemKind::Settings)
            .then(|| self.render_settings_dock(cx));
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        div()
            .key_context("WorkspaceWindow")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::open_dialog))
            .on_action(cx.listener(Self::open_settings))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface.background)
            .child(self.render_tab_bar(cx))
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .flex()
                    .children(left_dock)
                    .child(div().h_full().flex_1().overflow_hidden().child(content)),
            )
            .when(self.tab_search_open, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(px(TAB_BAR_HEIGHT + 6.0))
                        .right(px(48.0))
                        .child(self.render_tab_search(cx)),
                )
            })
            .children(self.render_tab_hover_card(window, cx))
            .when_some(self.warning.clone(), |root, warning| {
                root.child(
                    div()
                        .absolute()
                        .bottom_4()
                        .left_4()
                        .right_4()
                        .p_3()
                        .rounded_lg()
                        .bg(tokens.status.danger_soft)
                        .text_color(tokens.status.danger)
                        .child(warning),
                )
            })
    }
}
