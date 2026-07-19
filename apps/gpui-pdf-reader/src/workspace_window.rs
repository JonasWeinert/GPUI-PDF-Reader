//! GPUI window shell shared by document and host-owned workspace views.

use crate::application_host::{
    ApplicationHost, activate_workspace_views, idle_workspace_view, open_pdf_from_window,
    open_settings_window, remove_workspace_view, select_workspace_tab_views, set_resource_mode,
    transfer_workspace_view_registration, workspace_window_handle,
};
use crate::reader::PdfReader;
use crate::text_field::{TextField, TextFieldEvent};
use crate::{OpenDocument, OpenSettings};
use gpui::{
    App, AppContext, BoxShadow, Context, CursorStyle, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Render,
    ScrollHandle, SharedString, Window, div, point, prelude::*, px, relative,
};
use gpui_component::{Icon, IconName, Theme};
use key_ui_gpui::{
    CONTEXT_BAR_HEIGHT, TAB_BAR_HEIGHT, TAB_HOVER_CARD_WIDTH, TabBarAction, TabDragPayload,
    TabDropAction, TabHoverCard, TabIndexAction, TabPresentation, TabSearchPopover,
    TabSegmentAction, TabStrip, ThemeTokens, UnitTransition, WorkspaceContextBar, tab_hover_card_x,
    tab_search_popover_x,
};
use key_workspace_core::{
    ContentPlacement, DockPanel, ItemKind, ResourceAllocation, ResourceMode, SplitAxis, TabId,
    ViewId, WindowDocks, WorkspaceItemDescriptor, WorkspaceTabLayout, WorkspaceViewDescriptor,
    WorkspaceWindowDescriptor,
};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

enum WorkspaceContent {
    Pdf(Entity<PdfReader>),
    Settings,
}

struct WorkspaceViewSlot {
    item: Option<WorkspaceItemDescriptor>,
    view: WorkspaceViewDescriptor,
    content: WorkspaceContent,
}

impl WorkspaceViewSlot {
    fn source_detail(&self) -> SharedString {
        match self.item.as_ref().map(|item| &item.source) {
            Some(key_workspace_core::ItemSource::File(path)) => path.display().to_string().into(),
            _ if self.view.kind == ItemKind::Settings => "Application settings".into(),
            _ => "New PDF tab".into(),
        }
    }

    fn state_detail(&self, cx: &App) -> SharedString {
        match &self.content {
            WorkspaceContent::Pdf(reader) => reader.read(cx).tab_detail(),
            WorkspaceContent::Settings => "Application settings".into(),
        }
    }

    fn hover_title(&self, cx: &App) -> SharedString {
        match &self.content {
            WorkspaceContent::Pdf(reader) => reader
                .read(cx)
                .tab_hover_detail()
                .title
                .filter(|title| !title.trim().is_empty())
                .map_or_else(|| self.view.title.clone().into(), Into::into),
            WorkspaceContent::Settings => self.view.title.clone().into(),
        }
    }

    fn hover_status(&self, cx: &App) -> SharedString {
        match &self.content {
            WorkspaceContent::Pdf(reader) => {
                let detail = reader.read(cx).tab_hover_detail();
                if detail.page_count == 0 {
                    return detail.status;
                }
                let page = format!("p {}/{}", detail.current_page, detail.page_count);
                detail.section.map_or_else(
                    || page.clone().into(),
                    |section| format!("{}, {page}", end_truncate(&section, 34)).into(),
                )
            }
            WorkspaceContent::Settings => "Application settings".into(),
        }
    }

    fn hover_source_detail(&self) -> SharedString {
        start_truncate(self.source_detail().as_ref(), 34).into()
    }

    fn hover_card(&self, tokens: ThemeTokens, cx: &App) -> TabHoverCard {
        let icon = match &self.content {
            WorkspaceContent::Pdf(_) => IconName::File,
            WorkspaceContent::Settings => IconName::Settings,
        };
        let card = TabHoverCard::new(tokens, self.hover_title(cx)).leading(
            div()
                .size(px(32.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .bg(tokens.action.accent_soft)
                .text_color(tokens.action.accent)
                .child(Icon::new(icon).size(px(15.0))),
        );

        match &self.content {
            WorkspaceContent::Pdf(_) => card.subtitle(self.hover_status(cx)).footer(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(tokens.content.tertiary)
                    .child(Icon::new(IconName::Folder).size(px(13.0)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(self.hover_source_detail()),
                    ),
            ),
            WorkspaceContent::Settings => card.subtitle("Application settings"),
        }
    }
}

struct WorkspaceTab {
    layout: WorkspaceTabLayout,
    docks: WindowDocks,
    views: Vec<WorkspaceViewSlot>,
}

impl WorkspaceTab {
    fn single(id: TabId, slot: WorkspaceViewSlot) -> Self {
        Self {
            layout: WorkspaceTabLayout::single(id, slot.view.id),
            docks: WindowDocks::default(),
            views: vec![slot],
        }
    }

    fn active_slot(&self) -> &WorkspaceViewSlot {
        self.views
            .iter()
            .find(|slot| slot.view.id == self.layout.active_view)
            .expect("supported tab layout contains its active view")
    }

    fn active_slot_mut(&mut self) -> &mut WorkspaceViewSlot {
        self.views
            .iter_mut()
            .find(|slot| slot.view.id == self.layout.active_view)
            .expect("supported tab layout contains its active view")
    }

    fn slot(&self, view: ViewId) -> Option<&WorkspaceViewSlot> {
        self.views.iter().find(|slot| slot.view.id == view)
    }

    fn contains_view(&self, view: ViewId) -> bool {
        self.slot(view).is_some()
    }

    fn visible_views(&self) -> Vec<ViewId> {
        self.layout.visible_views()
    }

    fn split_views(&self) -> Option<(ViewId, ViewId, f32)> {
        let ContentPlacement::Split {
            ratio,
            first,
            second,
            ..
        } = &self.layout.content
        else {
            return None;
        };
        let ContentPlacement::View { view_id: first } = first.as_ref() else {
            return None;
        };
        let ContentPlacement::View { view_id: second } = second.as_ref() else {
            return None;
        };
        Some((*first, *second, *ratio))
    }

    fn is_split(&self) -> bool {
        self.split_views().is_some()
    }

    fn set_split_ratio(&mut self, ratio: f32) {
        if let ContentPlacement::Split { ratio: current, .. } = &mut self.layout.content {
            *current = ratio.clamp(0.2, 0.8);
        }
    }

    fn source_detail(&self) -> SharedString {
        self.active_slot().source_detail()
    }

    fn state_detail(&self, cx: &App) -> SharedString {
        self.active_slot().state_detail(cx)
    }

    fn hover_card(&self, tokens: ThemeTokens, cx: &App) -> TabHoverCard {
        let Some((first, second, _)) = self.split_views() else {
            return self.active_slot().hover_card(tokens, cx);
        };
        let active = self.layout.active_view;
        let rows = [first, second].into_iter().map(|view| {
            let slot = self
                .slot(view)
                .expect("split placement views have presentation slots");
            let title: SharedString = slot.view.title.clone().into();
            let detail = slot.hover_status(cx);
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    Icon::new(if slot.view.kind == ItemKind::Settings {
                        IconName::Settings
                    } else {
                        IconName::File
                    })
                    .size(px(14.0))
                    .text_color(if view == active {
                        tokens.action.accent
                    } else {
                        tokens.content.tertiary
                    }),
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
                .when(view == active, |row| {
                    row.child(div().size(px(6.0)).rounded_full().bg(tokens.action.accent))
                })
        });
        TabHoverCard::new(tokens, "Split view")
            .leading(
                div()
                    .size(px(32.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .bg(tokens.action.accent_soft)
                    .text_color(tokens.action.accent)
                    .child(Icon::new(IconName::Frame).size(px(15.0))),
            )
            .subtitle("Two views in one tab")
            .body(div().flex().flex_col().children(rows))
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
    split_menu_open: bool,
    split_resizing: bool,
    split_activation: UnitTransition,
    split_activation_from: Option<ViewId>,
    split_animation_frame_queued: bool,
    last_split_animation_tick: Instant,
    focus_handle: FocusHandle,
    warning: Option<SharedString>,
    recently_moved_tab: Option<TabId>,
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
        let tab_id = host.read(cx).allocate_tab_id();
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
                tabs: vec![WorkspaceTab::single(
                    tab_id,
                    WorkspaceViewSlot {
                        item,
                        view,
                        content: WorkspaceContent::Pdf(reader),
                    },
                )],
                active_tab: 0,
                tab_scroll: ScrollHandle::new(),
                hovered_tab: None,
                tab_search_open: false,
                tab_search_query: String::new(),
                tab_search_field,
                split_menu_open: false,
                split_resizing: false,
                split_activation: UnitTransition::visible(),
                split_activation_from: None,
                split_animation_frame_queued: false,
                last_split_animation_tick: Instant::now(),
                focus_handle,
                warning: None,
                recently_moved_tab: None,
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
        let tab_id = host.read(cx).allocate_tab_id();
        let mut docks = WindowDocks::default();
        docks.left.open = true;
        docks.left.extent = 216.0;
        docks.left.panels.push(DockPanel {
            id: "core.settings-navigation".into(),
            title: "Settings".into(),
            visible: true,
        });
        docks.left.active_panel = Some("core.settings-navigation".into());
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
                    layout: WorkspaceTabLayout::single(tab_id, view.id),
                    docks,
                    views: vec![WorkspaceViewSlot {
                        item: None,
                        view,
                        content: WorkspaceContent::Settings,
                    }],
                }],
                active_tab: 0,
                tab_scroll: ScrollHandle::new(),
                hovered_tab: None,
                tab_search_open: false,
                tab_search_query: String::new(),
                tab_search_field,
                split_menu_open: false,
                split_resizing: false,
                split_activation: UnitTransition::visible(),
                split_activation_from: None,
                split_animation_frame_queued: false,
                last_split_animation_tick: Instant::now(),
                focus_handle,
                warning: None,
                recently_moved_tab: None,
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
        let visible = self.active_tab().visible_views();
        if !window.is_window_active() {
            idle_workspace_view(self.host.clone(), view, cx);
            return;
        }
        let changed = activate_workspace_views(self.host.clone(), view, &visible, cx);
        if changed && let WorkspaceContent::Pdf(reader) = &self.active_tab().active_slot().content {
            reader.update(cx, |reader, cx| reader.activate_extension_scope(window, cx));
        }
    }

    fn active_tab(&self) -> &WorkspaceTab {
        &self.tabs[self.active_tab]
    }

    fn active_view(&self) -> &WorkspaceViewDescriptor {
        &self.active_tab().active_slot().view
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
            .flat_map(|tab| &tab.views)
            .find(|slot| slot.view.id == view)
            .map(|slot| &slot.content)
        {
            reader.update(cx, |reader, cx| {
                reader.apply_resource_allocation(allocation, window, cx)
            });
        }
        cx.notify();
    }

    pub(crate) fn focus_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.active_tab().active_slot().content {
            WorkspaceContent::Pdf(reader) => window.focus(&reader.read(cx).focus_handle(cx)),
            WorkspaceContent::Settings => window.focus(&self.focus_handle),
        }
    }

    pub(crate) fn reader(&self) -> Option<Entity<PdfReader>> {
        match &self.active_tab().active_slot().content {
            WorkspaceContent::Pdf(reader) => Some(reader.clone()),
            WorkspaceContent::Settings => None,
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn readers(&self) -> Vec<Entity<PdfReader>> {
        self.tabs
            .iter()
            .flat_map(|tab| &tab.views)
            .filter_map(|slot| match &slot.content {
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
    pub(crate) fn qa_window_and_view(&self) -> (key_workspace_core::WindowId, ViewId) {
        (self.descriptor.id, self.active_view().id)
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_tab_view_ids(&self) -> Vec<ViewId> {
        self.tabs.iter().map(|tab| tab.layout.active_view).collect()
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

    #[cfg(debug_assertions)]
    pub(crate) fn qa_reorder_tab(
        &mut self,
        source: usize,
        insertion: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get(source).map(|tab| tab.layout.id) {
            self.reorder_tab(tab, insertion, cx);
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_transfer_tab_from(
        &mut self,
        source: key_workspace_core::WindowId,
        view: ViewId,
        insertion: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = workspace_window_handle(&self.host, source, cx)
            .and_then(|handle| {
                handle
                    .update(cx, |workspace, _, _| {
                        workspace
                            .tabs
                            .iter()
                            .find(|tab| tab.contains_view(view))
                            .map(|tab| tab.layout.id)
                    })
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| TabId::from_raw(view.get()));
        self.handle_tab_drop(
            &TabDragPayload::new(source.get(), tab.get(), view.get(), "QA transfer"),
            insertion,
            window,
            cx,
        );
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_split_with_other_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(other_index) = (0..self.tabs.len()).find(|index| *index != self.active_tab) {
            self.split_with_tab(other_index, window, cx);
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_split_state(
        &self,
    ) -> (usize, Vec<ViewId>, ViewId, Option<(ViewId, ViewId, f32)>) {
        (
            self.tabs.len(),
            self.active_tab().visible_views(),
            self.active_view().id,
            self.active_tab().split_views(),
        )
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_activate_split_view(
        &mut self,
        view: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_split_view(view, window, cx);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_set_split_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        self.tabs[self.active_tab].set_split_ratio(ratio);
        cx.notify();
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_swap_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.swap_split_views(window, cx);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_separate_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.separate_split(window, cx);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_close_split_view(
        &mut self,
        view: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_split_view(view, window, cx);
    }

    pub(crate) fn open_pdf(
        &mut self,
        path: PathBuf,
        item: WorkspaceItemDescriptor,
        view: WorkspaceViewDescriptor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reader) = self.tabs[self.active_tab]
            .views
            .iter()
            .find(|slot| slot.view.id == self.tabs[self.active_tab].layout.active_view)
            .and_then(|slot| match &slot.content {
                WorkspaceContent::Pdf(reader) => Some(reader.clone()),
                WorkspaceContent::Settings => None,
            })
        else {
            return;
        };
        self.descriptor.title = item.title.clone();
        let view_id = view.id;
        let view_title = view.title.clone();
        {
            let tab = &mut self.tabs[self.active_tab];
            let slot = tab.active_slot_mut();
            slot.item = Some(item);
            slot.view = view;
            tab.layout.active_view = view_id;
            tab.layout.content = key_workspace_core::ContentPlacement::View { view_id };
        }
        window.set_window_title(&format!("{} — GPUI PDF Reader", view_title));
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
        let tab_id = self.host.read(cx).allocate_tab_id();
        self.tabs.push(WorkspaceTab::single(
            tab_id,
            WorkspaceViewSlot {
                item,
                view,
                content: WorkspaceContent::Pdf(reader),
            },
        ));
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
        let mut docks = WindowDocks::default();
        docks.left.open = true;
        docks.left.extent = 216.0;
        docks.left.panels.push(DockPanel {
            id: "core.settings-navigation".into(),
            title: "Settings".into(),
            visible: true,
        });
        docks.left.active_panel = Some("core.settings-navigation".into());
        let tab_id = self.host.read(cx).allocate_tab_id();
        self.tabs.push(WorkspaceTab {
            layout: WorkspaceTabLayout::single(tab_id, view.id),
            docks,
            views: vec![WorkspaceViewSlot {
                item: None,
                view,
                content: WorkspaceContent::Settings,
            }],
        });
        self.active_tab = self.tabs.len() - 1;
        self.tab_scroll.scroll_to_item(self.active_tab);
        self.finish_tab_activation(window, cx);
    }

    fn split_with_tab(&mut self, other_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if other_index >= self.tabs.len()
            || other_index == self.active_tab
            || self.active_tab().is_split()
            || self.tabs[other_index].is_split()
        {
            return;
        }
        let active_tab_id = self.active_tab().layout.id;
        let active_view = self.active_view().id;
        let other = self.tabs.remove(other_index);
        if other_index < self.active_tab {
            self.active_tab -= 1;
        }
        let second_view = other.layout.active_view;
        let tab = &mut self.tabs[self.active_tab];
        tab.views.extend(other.views);
        let Some(layout) = WorkspaceTabLayout::split_pair(
            active_tab_id,
            SplitAxis::Horizontal,
            0.5,
            active_view,
            second_view,
            active_view,
        ) else {
            self.warning = Some("These views could not be combined".into());
            cx.notify();
            return;
        };
        tab.layout = layout;
        self.split_menu_open = false;
        self.hovered_tab = None;
        self.mark_tab_moved(active_tab_id, cx);
        self.finish_tab_activation(window, cx);
    }

    fn split_from_drop(
        &mut self,
        drag: &TabDragPayload,
        dropped_on_left: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab().is_split() {
            return;
        }
        let target_tab = self.active_tab().layout.id;
        let source_window = key_workspace_core::WindowId::from_raw(drag.source_window);
        let source_tab = TabId::from_raw(drag.tab);

        if source_window == self.descriptor.id {
            let Some(source_index) = self.tabs.iter().position(|tab| tab.layout.id == source_tab)
            else {
                return;
            };
            self.split_with_tab(source_index, window, cx);
        } else {
            let insertion = self.tabs.len();
            self.handle_tab_drop(drag, insertion, window, cx);
            let Some(target_index) = self.tabs.iter().position(|tab| tab.layout.id == target_tab)
            else {
                return;
            };
            let Some(source_index) = self.tabs.iter().position(|tab| tab.layout.id == source_tab)
            else {
                return;
            };
            self.active_tab = target_index;
            self.split_with_tab(source_index, window, cx);
        }

        if dropped_on_left && self.active_tab().is_split() {
            self.swap_split_views(window, cx);
        }
    }

    fn activate_split_view(&mut self, view: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active_tab().contains_view(view) || self.active_view().id == view {
            return;
        }
        self.begin_split_activation(view, window, cx);
        self.tabs[self.active_tab].layout.active_view = view;
        self.finish_tab_activation(window, cx);
    }

    fn swap_split_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((first, second, ratio)) = self.active_tab().split_views() else {
            return;
        };
        let tab = &mut self.tabs[self.active_tab];
        tab.layout.content = ContentPlacement::Split {
            axis: SplitAxis::Horizontal,
            ratio: 1.0 - ratio,
            first: Box::new(ContentPlacement::View { view_id: second }),
            second: Box::new(ContentPlacement::View { view_id: first }),
        };
        self.split_menu_open = false;
        self.finish_tab_activation(window, cx);
    }

    fn separate_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((first, second, _)) = self.active_tab().split_views() else {
            return;
        };
        let active = self.active_view().id;
        let detached = if active == first { second } else { first };
        let detached_slot = {
            let tab = &mut self.tabs[self.active_tab];
            let index = tab
                .views
                .iter()
                .position(|slot| slot.view.id == detached)
                .expect("split placement views have slots");
            let slot = tab.views.remove(index);
            tab.layout = WorkspaceTabLayout::single(tab.layout.id, active);
            slot
        };
        let new_tab_id = self.host.read(cx).allocate_tab_id();
        let new_tab = WorkspaceTab::single(new_tab_id, detached_slot);
        let insertion = if detached == first {
            self.active_tab
        } else {
            self.active_tab + 1
        };
        self.tabs.insert(insertion, new_tab);
        if detached == first {
            self.active_tab += 1;
        }
        self.reset_split_activation();
        self.split_menu_open = false;
        self.finish_tab_activation(window, cx);
    }

    fn close_split_view(&mut self, view: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((first, second, _)) = self.active_tab().split_views() else {
            return;
        };
        if view != first && view != second {
            return;
        }
        let other = if view == first { second } else { first };
        let slot_index = self.tabs[self.active_tab]
            .views
            .iter()
            .position(|slot| slot.view.id == view)
            .expect("split placement views have slots");
        if matches!(
            &self.tabs[self.active_tab].views[slot_index].content,
            WorkspaceContent::Pdf(reader) if reader.read(cx).tab_close_requires_confirmation()
        ) {
            self.warning =
                Some("Finish or discard the open comment draft before closing this view.".into());
            cx.notify();
            return;
        }
        let removed = self.tabs[self.active_tab].views.remove(slot_index);
        if let WorkspaceContent::Pdf(reader) = &removed.content {
            reader.update(cx, |reader, _| reader.prepare_tab_close());
        }
        let tab_id = self.active_tab().layout.id;
        self.tabs[self.active_tab].layout = WorkspaceTabLayout::single(tab_id, other);
        let next = self.tabs[self.active_tab]
            .slot(other)
            .expect("remaining split view has a slot")
            .view
            .clone();
        self.reset_split_activation();
        remove_workspace_view(
            self.host.clone(),
            self.descriptor.id,
            removed.view.id,
            Some((next.id, next.title.clone())),
            cx,
        );
        self.split_menu_open = false;
        self.finish_tab_activation(window, cx);
    }

    fn begin_split_resize(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.split_resizing = true;
        cx.notify();
    }

    fn resize_split(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.split_resizing || !self.active_tab().is_split() {
            return;
        }
        let dock = if self.active_tab().docks.left.open {
            self.active_tab().docks.left.extent
        } else {
            0.0
        };
        let available = (f32::from(window.viewport_size().width) - dock).max(1.0);
        let ratio = (f32::from(event.position.x) - dock) / available;
        self.tabs[self.active_tab].set_split_ratio(ratio);
        cx.notify();
    }

    fn end_split_resize(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.split_resizing {
            self.split_resizing = false;
            cx.notify();
        }
    }

    fn begin_split_activation(
        &mut self,
        next: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.active_view().id;
        if previous == next {
            return;
        }
        let starting_emphasis = if self.split_activation_from == Some(next) {
            1.0 - self.split_activation.value()
        } else {
            0.0
        };
        self.split_activation_from = Some(previous);
        self.split_activation.snap_to(starting_emphasis);
        self.split_activation.set_target(1.0);
        self.last_split_animation_tick = Instant::now();
        self.queue_split_animation_frame(window, cx);
    }

    fn reset_split_activation(&mut self) {
        self.split_activation.snap_to(1.0);
        self.split_activation_from = None;
    }

    fn queue_split_animation_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split_animation_frame_queued {
            return;
        }
        self.split_animation_frame_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            weak.update(cx, |workspace, cx| {
                workspace.advance_split_animation(window, cx)
            })
            .ok();
        });
        cx.notify();
    }

    fn advance_split_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_animation_frame_queued = false;
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_split_animation_tick)
            .as_secs_f32();
        self.last_split_animation_tick = now;
        if self.split_activation.advance_with_response(elapsed, 26.0) {
            self.queue_split_animation_frame(window, cx);
        } else {
            self.split_activation_from = None;
            cx.notify();
        }
    }

    pub(crate) fn activate_tab(
        &mut self,
        view: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.contains_view(view)) {
            if index == self.active_tab && self.tabs[index].is_split() {
                self.begin_split_activation(view, window, cx);
            } else if index != self.active_tab {
                self.reset_split_activation();
            }
            self.tabs[index].layout.active_view = view;
            self.activate_tab_index(index, window, cx);
        }
    }

    fn activate_tab_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active_tab {
            self.reset_split_activation();
        }
        self.active_tab = index;
        self.tab_search_open = false;
        self.hovered_tab = None;
        self.tab_scroll.scroll_to_item(index);
        self.finish_tab_activation(window, cx);
    }

    fn activate_tab_view(
        &mut self,
        index: usize,
        view: ViewId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.tabs.len() || !self.tabs[index].contains_view(view) {
            return;
        }
        if index == self.active_tab && self.tabs[index].is_split() {
            self.begin_split_activation(view, window, cx);
        } else if index != self.active_tab {
            self.reset_split_activation();
        }
        self.tabs[index].layout.active_view = view;
        self.activate_tab_index(index, window, cx);
    }

    fn finish_tab_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.active_view().clone();
        let visible = self.active_tab().visible_views();
        self.descriptor.active_view = Some(view.id);
        self.descriptor.title = view.title.clone();
        window.set_window_title(&format!("{} — GPUI PDF Reader", view.title));
        let changed = select_workspace_tab_views(
            self.host.clone(),
            self.descriptor.id,
            view.id,
            &visible,
            view.title,
            cx,
        );
        if changed && let WorkspaceContent::Pdf(reader) = &self.active_tab().active_slot().content {
            reader.update(cx, |reader, cx| reader.activate_extension_scope(window, cx));
        }
        self.focus_content(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].views.iter().any(|slot| {
            matches!(&slot.content, WorkspaceContent::Pdf(reader) if reader.read(cx).tab_close_requires_confirmation())
        }) {
            self.warning =
                Some("Finish or discard the open comment draft before closing this tab.".into());
            cx.notify();
            return;
        }
        if self.tabs.len() == 1 {
            window.remove_window();
            return;
        }
        for slot in &self.tabs[index].views {
            if let WorkspaceContent::Pdf(reader) = &slot.content {
                reader.update(cx, |reader, _| reader.prepare_tab_close());
            }
        }
        let previous_count = self.tabs.len();
        let removed = self.tabs.remove(index);
        self.active_tab = active_index_after_close(previous_count, self.active_tab, index)
            .expect("closing one of several tabs leaves an active tab");
        let next = self.active_view().clone();
        for slot in removed.views {
            remove_workspace_view(
                self.host.clone(),
                self.descriptor.id,
                slot.view.id,
                Some((next.id, next.title.clone())),
                cx,
            );
        }
        self.finish_tab_activation(window, cx);
    }

    fn handle_tab_drop(
        &mut self,
        drag: &TabDragPayload,
        insertion: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_window = key_workspace_core::WindowId::from_raw(drag.source_window);
        let tab_id = TabId::from_raw(drag.tab);
        if source_window == self.descriptor.id {
            self.reorder_tab(tab_id, insertion, cx);
            return;
        }
        let Some(source_handle) = workspace_window_handle(&self.host, source_window, cx) else {
            self.warning = Some("The source window is no longer available".into());
            cx.notify();
            return;
        };
        let transferred = source_handle
            .update(cx, |source, source_window, cx| {
                source.take_tab_for_transfer(tab_id, source_window, cx)
            })
            .ok()
            .flatten();
        let Some((mut tab, source_became_empty)) = transferred else {
            self.warning = Some("This tab could not be moved".into());
            cx.notify();
            return;
        };
        for slot in &tab.views {
            transfer_workspace_view_registration(
                self.host.clone(),
                source_window,
                self.descriptor.id,
                &slot.view,
                cx,
            );
        }
        self.rebuild_transferred_content(&mut tab, window, cx);
        let insertion = insertion.min(self.tabs.len());
        let tab_id = tab.layout.id;
        self.tabs.insert(insertion, tab);
        self.active_tab = insertion;
        self.mark_tab_moved(tab_id, cx);
        self.tab_scroll.scroll_to_item(insertion);
        self.finish_tab_activation(window, cx);
        if source_became_empty {
            source_handle
                .update(cx, |_, source_window, _| source_window.remove_window())
                .ok();
        }
    }

    fn reorder_tab(&mut self, tab_id: TabId, insertion: usize, cx: &mut Context<Self>) {
        let Some(source) = self.tabs.iter().position(|tab| tab.layout.id == tab_id) else {
            return;
        };
        let destination = reorder_destination(self.tabs.len(), source, insertion);
        if source == destination {
            return;
        }
        let active_tab = self.active_tab().layout.id;
        let tab = self.tabs.remove(source);
        self.tabs.insert(destination, tab);
        self.active_tab = self
            .tabs
            .iter()
            .position(|tab| tab.layout.id == active_tab)
            .expect("the active tab remains present after reordering");
        self.mark_tab_moved(tab_id, cx);
        self.tab_scroll.scroll_to_item(destination);
        self.hovered_tab = None;
        cx.notify();
    }

    fn mark_tab_moved(&mut self, tab: TabId, cx: &mut Context<Self>) {
        self.recently_moved_tab = Some(tab);
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(280))
                .await;
            weak.update(cx, |workspace, cx| {
                if workspace.recently_moved_tab == Some(tab) {
                    workspace.recently_moved_tab = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn take_tab_for_transfer(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(WorkspaceTab, bool)> {
        let index = self
            .tabs
            .iter()
            .position(|candidate| candidate.layout.id == tab)?;
        if self.tabs[index].views.iter().any(|slot| {
            matches!(&slot.content, WorkspaceContent::Pdf(reader) if reader.read(cx).tab_close_requires_confirmation())
        }) {
            self.warning =
                Some("Finish or discard the open comment draft before moving this tab.".into());
            cx.notify();
            return None;
        }
        let previous_count = self.tabs.len();
        let removed = self.tabs.remove(index);
        self.hovered_tab = None;
        let became_empty = self.tabs.is_empty();
        if !became_empty {
            self.active_tab = active_index_after_close(previous_count, self.active_tab, index)
                .expect("moving one of several tabs leaves an active tab");
            self.finish_tab_activation(window, cx);
        }
        Some((removed, became_empty))
    }

    fn rebuild_transferred_content(
        &self,
        tab: &mut WorkspaceTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for slot in &mut tab.views {
            let WorkspaceContent::Pdf(source_reader) = &slot.content else {
                continue;
            };
            let snapshot = source_reader.read(cx).transfer_snapshot();
            source_reader.update(cx, |reader, _| reader.prepare_tab_close());
            let path = slot.item.as_ref().and_then(|item| match &item.source {
                key_workspace_core::ItemSource::File(path) => Some(path.clone()),
                _ => None,
            });
            let reader = PdfReader::new(
                path,
                self.host.clone(),
                self.descriptor.id,
                slot.view.id,
                window,
                cx,
            );
            reader.update(cx, |reader, _| reader.restore_after_transfer(snapshot));
            slot.content = WorkspaceContent::Pdf(reader);
        }
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
                tab.views.iter().any(|slot| {
                    tab_matches_query(
                        &slot.view.title,
                        slot.source_detail().as_ref(),
                        &self.tab_search_query,
                    )
                })
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
            .enumerate()
            .map(|(index, tab)| {
                let active = tab.active_slot();
                let presentation = if let Some((first, second, _)) = tab.split_views() {
                    let first_slot = tab.slot(first).expect("split first view has a slot");
                    let second_slot = tab.slot(second).expect("split second view has a slot");
                    let presentation = TabPresentation::new(
                        first_slot.view.title.clone(),
                        first_slot.state_detail(cx),
                    )
                    .view(first.get())
                    .split(
                        second.get(),
                        second_slot.view.title.clone(),
                        second_slot.state_detail(cx),
                        usize::from(tab.layout.active_view == second),
                    );
                    if index == self.active_tab {
                        let outgoing = self
                            .split_activation_from
                            .map(|view| usize::from(view == second));
                        presentation.segment_transition(outgoing, self.split_activation.value())
                    } else {
                        presentation
                    }
                } else {
                    TabPresentation::new(active.view.title.clone(), tab.state_detail(cx))
                        .view(active.view.id.get())
                };
                presentation
                    .draggable(TabDragPayload::new(
                        self.descriptor.id.get(),
                        tab.layout.id.get(),
                        active.view.id.get(),
                        active.view.title.clone(),
                    ))
                    .recently_moved(self.recently_moved_tab == Some(tab.layout.id))
            })
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
        let segment_weak = weak.clone();
        let activate_segment: TabSegmentAction = Rc::new(move |index, view, _, window, cx| {
            segment_weak
                .update(cx, |workspace, cx| {
                    workspace.activate_tab_view(index, ViewId::from_raw(view), window, cx)
                })
                .ok();
        });
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
        let drop_weak = weak.clone();
        let drop: TabDropAction = Rc::new(move |drag, insertion, window, cx| {
            drop_weak
                .update(cx, |workspace, cx| {
                    workspace.handle_tab_drop(drag, insertion, window, cx)
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
        .on_activate_segment(activate_segment)
        .on_close(close)
        .on_drop(drop)
        .into_any_element()
    }

    fn render_tab_search(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let weak = cx.weak_entity();
        let filtered_indices = self.filtered_tab_indices();
        let row_count = filtered_indices.len();
        let rows = filtered_indices
            .into_iter()
            .enumerate()
            .map(|(row_index, index)| {
                let tab = &self.tabs[index];
                let active_slot = tab.active_slot();
                let active = index == self.active_tab;
                let is_last = row_index + 1 == row_count;
                let split = tab.split_views();
                let title: SharedString = split.map_or_else(
                    || active_slot.view.title.clone().into(),
                    |(first, second, _)| {
                        let first = &tab.slot(first).expect("split first slot").view.title;
                        let second = &tab.slot(second).expect("split second slot").view.title;
                        format!("{first}  +  {second}").into()
                    },
                );
                let detail: SharedString = split.map_or_else(
                    || tab.source_detail(),
                    |_| format!("Split view · {}", tab.state_detail(cx)).into(),
                );
                let selected_view = tab
                    .views
                    .iter()
                    .find(|slot| {
                        tab_matches_query(
                            &slot.view.title,
                            slot.source_detail().as_ref(),
                            &self.tab_search_query,
                        )
                    })
                    .map_or(tab.layout.active_view, |slot| slot.view.id);
                let weak = weak.clone();
                div()
                    .id(("tab-search-result", index))
                    .relative()
                    .min_h(px(58.0))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if active {
                        tokens.action.accent_soft
                    } else {
                        tokens.surface.background
                    })
                    .hover(move |row| row.bg(tokens.surface.muted))
                    .on_click(move |_, window, cx| {
                        weak.update(cx, |workspace, cx| {
                            workspace.activate_tab_view(index, selected_view, window, cx)
                        })
                        .ok();
                    })
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Icon::new(if split.is_some() {
                            IconName::Frame
                        } else if active_slot.view.kind == ItemKind::Settings {
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
                    .when(active, |row| {
                        row.child(
                            div()
                                .size(px(6.0))
                                .flex_none()
                                .rounded_full()
                                .bg(tokens.action.accent),
                        )
                    })
                    .when(!is_last, |row| {
                        row.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left(px(42.0))
                                .right_2()
                                .h(px(1.0))
                                .bg(tokens.surface.border.opacity(0.58)),
                        )
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        TabSearchPopover::new(tokens, self.tab_search_field.clone(), rows).into_any_element()
    }

    fn render_tab_hover_card(&self, window: &Window, cx: &App) -> Option<gpui::AnyElement> {
        let index = self.hovered_tab.filter(|index| *index != self.active_tab)?;
        let tab = self.tabs.get(index)?;
        let width = f32::from(window.viewport_size().width);
        let x = self.tab_scroll.bounds_for_item(index).map_or_else(
            || tab_hover_card_x(112.0, 180.0, width, TAB_HOVER_CARD_WIDTH),
            |bounds| {
                tab_hover_card_x(
                    f32::from(bounds.origin.x),
                    f32::from(bounds.size.width),
                    width,
                    TAB_HOVER_CARD_WIDTH,
                )
            },
        );
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        Some(
            div()
                .absolute()
                .top(px(TAB_BAR_HEIGHT + 8.0))
                .left(px(x))
                .child(tab.hover_card(tokens, cx))
                .into_any_element(),
        )
    }

    fn render_context_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let weak = cx.weak_entity();
        let split = self.active_tab().split_views();
        let active = self.active_view();
        let active_title: SharedString = active.title.clone().into();
        let split_button = div()
            .id("workspace-split-menu-button")
            .relative()
            .size(px(32.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .cursor_pointer()
            .text_color(if split.is_some() {
                tokens.action.accent
            } else {
                tokens.content.secondary
            })
            .bg(if self.split_menu_open {
                tokens.action.accent_soft
            } else {
                tokens.surface.background
            })
            .hover(move |button| button.bg(tokens.action.control_hover))
            .on_click(move |_, _, cx| {
                weak.update(cx, |workspace, cx| {
                    workspace.split_menu_open = !workspace.split_menu_open;
                    workspace.tab_search_open = false;
                    cx.notify();
                })
                .ok();
            })
            .child(Icon::new(IconName::Frame).size(px(16.0)))
            .when_some(split, |button, (first, _, _)| {
                button.child(
                    div()
                        .absolute()
                        .bottom(px(4.0))
                        .left(if active.id == first {
                            px(7.0)
                        } else {
                            px(18.0)
                        })
                        .w(px(7.0))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(tokens.action.accent),
                )
            });
        WorkspaceContextBar::new(tokens)
            .bottom_border(split.is_none())
            .leading(split_button)
            .center(
                div()
                    .min_w_0()
                    .max_w(px(420.0))
                    .px_3()
                    .py_1()
                    .rounded_lg()
                    .bg(tokens.surface.muted.opacity(0.72))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_sm()
                    .text_color(tokens.content.secondary)
                    .child(active_title),
            )
            .into_any_element()
    }

    fn render_split_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let weak = cx.weak_entity();
        let row = |id: &'static str,
                   icon: IconName,
                   label: &'static str,
                   action: Rc<
            dyn Fn(&mut WorkspaceWindow, &mut Window, &mut Context<WorkspaceWindow>),
        >| {
            let weak = weak.clone();
            div()
                .id(id)
                .h(px(38.0))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .hover(move |item| item.bg(tokens.action.control_hover))
                .on_click(move |_, window, cx| {
                    weak.update(cx, |workspace, cx| action(workspace, window, cx))
                        .ok();
                })
                .child(
                    Icon::new(icon)
                        .size(px(15.0))
                        .text_color(tokens.content.secondary),
                )
                .child(label)
                .into_any_element()
        };
        let mut rows = Vec::new();
        if let Some((first, second, _)) = self.active_tab().split_views() {
            rows.push(row(
                "workspace-split-swap",
                IconName::Replace,
                "Swap positions",
                Rc::new(|workspace, window, cx| workspace.swap_split_views(window, cx)),
            ));
            rows.push(row(
                "workspace-split-separate",
                IconName::GalleryVerticalEnd,
                "Separate views",
                Rc::new(|workspace, window, cx| workspace.separate_split(window, cx)),
            ));
            rows.push(row(
                "workspace-split-close-left",
                IconName::PanelLeftClose,
                "Close left view",
                Rc::new(move |workspace, window, cx| workspace.close_split_view(first, window, cx)),
            ));
            rows.push(row(
                "workspace-split-close-right",
                IconName::PanelRightClose,
                "Close right view",
                Rc::new(move |workspace, window, cx| {
                    workspace.close_split_view(second, window, cx)
                }),
            ));
        } else {
            for (index, tab) in self.tabs.iter().enumerate() {
                if index == self.active_tab || tab.is_split() {
                    continue;
                }
                let title: SharedString = tab.active_slot().view.title.clone().into();
                let weak = weak.clone();
                rows.push(
                    div()
                        .id(("workspace-split-with", index))
                        .h(px(42.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |item| item.bg(tokens.action.control_hover))
                        .on_click(move |_, window, cx| {
                            weak.update(cx, |workspace, cx| {
                                workspace.split_with_tab(index, window, cx)
                            })
                            .ok();
                        })
                        .child(
                            Icon::new(if tab.active_slot().view.kind == ItemKind::Settings {
                                IconName::Settings
                            } else {
                                IconName::File
                            })
                            .size(px(15.0))
                            .text_color(tokens.action.accent),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_sm()
                                .child(title),
                        )
                        .into_any_element(),
                );
            }
            if rows.is_empty() {
                rows.push(
                    div()
                        .px_3()
                        .py_3()
                        .text_sm()
                        .text_color(tokens.content.tertiary)
                        .child("Open another tab to create a split")
                        .into_any_element(),
                );
            }
        }
        div()
            .absolute()
            .top(px(TAB_BAR_HEIGHT + CONTEXT_BAR_HEIGHT + 6.0))
            .left(px(12.0))
            .w(px(268.0))
            .p_2()
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(tokens.surface.border)
            .shadow_lg()
            .bg(tokens.surface.overlay)
            .text_color(tokens.content.primary)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(tokens.content.tertiary)
                    .child(if self.active_tab().is_split() {
                        "Split view"
                    } else {
                        "Split with"
                    }),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_view_slot(&mut self, view: ViewId, cx: &mut Context<Self>) -> gpui::AnyElement {
        let reader = self
            .active_tab()
            .slot(view)
            .and_then(|slot| match &slot.content {
                WorkspaceContent::Pdf(reader) => Some(reader.clone()),
                WorkspaceContent::Settings => None,
            });
        reader.map_or_else(|| self.render_settings(cx), IntoElement::into_any_element)
    }

    fn render_workspace_content(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some((first, second, ratio)) = self.active_tab().split_views() else {
            let content = self.render_view_slot(self.active_view().id, cx);
            let tokens = ThemeTokens::from_theme(Theme::global(cx));
            let left_weak = cx.weak_entity();
            let right_weak = left_weak.clone();
            let drop_target =
                |id: &'static str, left: bool, weak: gpui::WeakEntity<WorkspaceWindow>| {
                    div()
                        .id(id)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .when(left, |target| target.left_0())
                        .when(!left, |target| target.right_0())
                        .w(px(28.0))
                        .drag_over::<TabDragPayload>(move |style, _, _, _| {
                            style
                                .w(px(118.0))
                                .border_2()
                                .border_color(tokens.action.accent.opacity(0.72))
                                .bg(tokens.action.accent_soft.opacity(0.72))
                        })
                        .on_drop(move |drag: &TabDragPayload, window, cx| {
                            weak.update(cx, |workspace, cx| {
                                workspace.split_from_drop(drag, left, window, cx)
                            })
                            .ok();
                        })
                };
            return div()
                .relative()
                .size_full()
                .overflow_hidden()
                .child(content)
                .child(drop_target("workspace-split-drop-left", true, left_weak))
                .child(drop_target("workspace-split-drop-right", false, right_weak))
                .into_any_element();
        };
        let active = self.active_view().id;
        let first_content = self.render_view_slot(first, cx);
        let second_content = self.render_view_slot(second, cx);
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let activation_progress = self.split_activation.value();
        let activation_from = self.split_activation_from;
        let first_emphasis =
            split_pane_emphasis(first, active, activation_from, activation_progress);
        let second_emphasis =
            split_pane_emphasis(second, active, activation_from, activation_progress);
        let first_weak = cx.weak_entity();
        let second_weak = first_weak.clone();
        let pane = |view: ViewId,
                    content: gpui::AnyElement,
                    emphasis: f32,
                    weak: gpui::WeakEntity<WorkspaceWindow>| {
            let border_alpha = 0.5 + emphasis * 0.42;
            let separator_alpha = (1.0 - emphasis) * 0.82;
            div().relative().min_w_0().h_full().child(
                div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .rounded_xl()
                    .border_l_1()
                    .border_r_1()
                    .border_b_1()
                    .border_color(tokens.surface.border.opacity(border_alpha))
                    .bg(tokens.surface.background)
                    .shadow(vec![
                        BoxShadow {
                            color: gpui::black().opacity(0.13 * emphasis),
                            offset: point(px(0.0), px(2.0)),
                            blur_radius: px(5.0),
                            spread_radius: px(-1.0),
                        },
                        BoxShadow {
                            color: gpui::black().opacity(0.16 * emphasis),
                            offset: point(px(0.0), px(7.0)),
                            blur_radius: px(19.0),
                            spread_radius: px(-4.0),
                        },
                    ])
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        weak.update(cx, |workspace, cx| {
                            workspace.activate_split_view(view, window, cx)
                        })
                        .ok();
                    })
                    .child(content)
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded_xl()
                            .border_l_1()
                            .border_r_1()
                            .border_b_1()
                            .border_color(tokens.action.accent_border.opacity(0.2 * emphasis)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_3()
                            .right_3()
                            .h(px(1.0))
                            .rounded_full()
                            .bg(tokens.surface.border.opacity(separator_alpha)),
                    ),
            )
        };
        div()
            .size_full()
            .flex()
            .px(px(8.0))
            .pb(px(8.0))
            .bg(tokens.surface.muted.opacity(0.68))
            .child(
                pane(first, first_content, first_emphasis, first_weak)
                    .w(relative(ratio))
                    .flex_none(),
            )
            .child(
                div()
                    .id("workspace-split-divider")
                    .relative()
                    .h_full()
                    .w(px(14.0))
                    .flex_none()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_split_resize))
                    .child(
                        div()
                            .absolute()
                            .top(px(22.0))
                            .bottom(px(22.0))
                            .left(px(6.0))
                            .w(px(2.0))
                            .rounded_full()
                            .bg(if self.split_resizing {
                                tokens.action.accent
                            } else {
                                tokens.surface.border.opacity(0.68)
                            }),
                    ),
            )
            .child(
                pane(second, second_content, second_emphasis, second_weak)
                    .min_w_0()
                    .flex_1(),
            )
            .into_any_element()
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
        let extent = self.active_tab().docks.left.extent;
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

fn reorder_destination(tab_count: usize, source: usize, insertion: usize) -> usize {
    if tab_count == 0 || source >= tab_count {
        return 0;
    }
    let insertion = insertion.min(tab_count);
    if insertion > source {
        insertion.saturating_sub(1).min(tab_count - 1)
    } else {
        insertion.min(tab_count - 1)
    }
}

fn end_truncate(text: &str, maximum_characters: usize) -> String {
    if text.chars().count() <= maximum_characters || maximum_characters < 2 {
        return text.to_owned();
    }
    let kept = maximum_characters - 1;
    let mut value = text.chars().take(kept).collect::<String>();
    value.push('…');
    value
}

fn start_truncate(text: &str, maximum_characters: usize) -> String {
    let count = text.chars().count();
    if count <= maximum_characters || maximum_characters < 2 {
        return text.to_owned();
    }
    let kept = maximum_characters - 1;
    let suffix = text.chars().skip(count - kept).collect::<String>();
    format!("…{suffix}")
}

fn split_pane_emphasis(
    view: ViewId,
    active: ViewId,
    outgoing: Option<ViewId>,
    progress: f32,
) -> f32 {
    let progress = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if view == active {
        progress
    } else if outgoing == Some(view) {
        1.0 - progress
    } else {
        0.0
    }
}

impl Focusable for WorkspaceWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.active_tab().active_slot().content {
            WorkspaceContent::Pdf(reader) => reader.read(cx).focus_handle(cx),
            WorkspaceContent::Settings => self.focus_handle.clone(),
        }
    }
}

impl Render for WorkspaceWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.render_workspace_content(cx);
        let left_dock = (self.active_tab().docks.left.open
            && self.active_view().kind == ItemKind::Settings)
            .then(|| self.render_settings_dock(cx));
        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let viewport_width = f32::from(window.viewport_size().width);
        let search_x = tab_search_popover_x(viewport_width);
        div()
            .key_context("WorkspaceWindow")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::open_dialog))
            .on_action(cx.listener(Self::open_settings))
            .on_mouse_move(cx.listener(Self::resize_split))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_split_resize))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_split_resize))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface.background)
            .child(self.render_tab_bar(cx))
            .child(self.render_context_bar(cx))
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
                        .top(px(TAB_BAR_HEIGHT + 8.0))
                        .left(px(search_x))
                        .child(self.render_tab_search(cx)),
                )
            })
            .children(self.render_tab_hover_card(window, cx))
            .when(self.split_menu_open, |root| {
                root.child(self.render_split_menu(cx))
            })
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

#[cfg(test)]
mod tests {
    use super::{
        active_index_after_close, end_truncate, reorder_destination, split_pane_emphasis,
        start_truncate, tab_matches_query,
    };
    use key_workspace_core::ViewId;

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

    #[test]
    fn tab_reordering_accounts_for_removal_before_insertion() {
        assert_eq!(reorder_destination(4, 0, 4), 3);
        assert_eq!(reorder_destination(4, 3, 0), 0);
        assert_eq!(reorder_destination(4, 1, 2), 1);
        assert_eq!(reorder_destination(4, 1, 3), 2);
    }

    #[test]
    fn hover_text_truncation_preserves_the_useful_edge() {
        assert_eq!(end_truncate("abcdefgh", 5), "abcd…");
        assert_eq!(start_truncate("/one/two/file.pdf", 9), "…file.pdf");
        assert_eq!(start_truncate("short", 9), "short");
    }

    #[test]
    fn split_pane_emphasis_crossfades_without_a_brightness_jump() {
        let first = ViewId::from_raw(1);
        let second = ViewId::from_raw(2);
        for progress in [0.0, 0.15, 0.5, 0.85, 1.0] {
            let outgoing = split_pane_emphasis(first, second, Some(first), progress);
            let incoming = split_pane_emphasis(second, second, Some(first), progress);
            assert!((outgoing + incoming - 1.0).abs() < f32::EPSILON);
        }
        assert_eq!(split_pane_emphasis(second, second, None, 1.0), 1.0);
        assert_eq!(split_pane_emphasis(first, second, None, 1.0), 0.0);
        assert_eq!(split_pane_emphasis(second, second, None, f32::NAN), 1.0);
    }
}
