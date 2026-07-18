//! GPUI window shell shared by document and host-owned workspace views.

use crate::application_host::{
    ApplicationHost, activate_workspace_view, open_pdf_from, open_settings_window,
    set_resource_mode,
};
use crate::reader::PdfReader;
use crate::{OpenDocument, OpenSettings};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions,
    Render, SharedString, Window, div, prelude::*, px,
};
use gpui_component::{Icon, IconName, Theme};
use key_ui_gpui::ThemeTokens;
use key_workspace_core::{
    DockPanel, ItemKind, ResourceAllocation, ResourceMode, WorkspaceItemDescriptor,
    WorkspaceLayout, WorkspaceViewDescriptor, WorkspaceWindowDescriptor,
};
use std::path::PathBuf;

enum WorkspaceContent {
    Pdf(Entity<PdfReader>),
    Settings,
}

/// Window-scoped chrome and placement owner. Document views own their local
/// panels; left/right/bottom docks here are reserved for workspace-wide tools.
pub(crate) struct WorkspaceWindow {
    host: Entity<ApplicationHost>,
    descriptor: WorkspaceWindowDescriptor,
    item: Option<WorkspaceItemDescriptor>,
    view: WorkspaceViewDescriptor,
    layout: WorkspaceLayout,
    content: WorkspaceContent,
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
        let entity = cx.new(|_| Self {
            host,
            descriptor,
            item,
            layout: WorkspaceLayout::single_view(view.id),
            view,
            content: WorkspaceContent::Pdf(reader),
            focus_handle,
            warning: None,
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
        let entity = cx.new(|_| Self {
            host,
            descriptor,
            item: None,
            view,
            layout,
            content: WorkspaceContent::Settings,
            focus_handle,
            warning: None,
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
        if !window.is_window_active() {
            return;
        }
        let view = self.view.id;
        let changed = activate_workspace_view(self.host.clone(), view, cx);
        if changed && let WorkspaceContent::Pdf(reader) = &self.content {
            reader.update(cx, |reader, cx| reader.activate_extension_scope(window, cx));
        }
    }

    pub(crate) fn apply_resource_allocation(
        &mut self,
        allocation: ResourceAllocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let WorkspaceContent::Pdf(reader) = &self.content {
            reader.update(cx, |reader, cx| {
                reader.apply_resource_allocation(allocation, window, cx)
            });
        }
        cx.notify();
    }

    pub(crate) fn focus_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.content {
            WorkspaceContent::Pdf(reader) => window.focus(&reader.read(cx).focus_handle(cx)),
            WorkspaceContent::Settings => window.focus(&self.focus_handle),
        }
    }

    pub(crate) fn reader(&self) -> Option<Entity<PdfReader>> {
        match &self.content {
            WorkspaceContent::Pdf(reader) => Some(reader.clone()),
            WorkspaceContent::Settings => None,
        }
    }

    pub(crate) fn open_pdf(
        &mut self,
        path: PathBuf,
        item: WorkspaceItemDescriptor,
        view: WorkspaceViewDescriptor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let WorkspaceContent::Pdf(reader) = &self.content else {
            return;
        };
        self.descriptor.title = item.title.clone();
        self.item = Some(item);
        self.view = view;
        reader.update(cx, |reader, cx| reader.open_path(path, window, cx));
        self.focus_content(window, cx);
        cx.notify();
    }

    fn open_dialog(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
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
                    let _ = cx.update(|_, cx| {
                        if let Err(error) = open_pdf_from(host, source, path, cx) {
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
                    .h(px(52.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .pl(px(86.0))
                    .pr_4()
                    .border_b_1()
                    .border_color(tokens.surface.border)
                    .bg(tokens.surface.chrome)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Settings"),
            )
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
        div()
            .h_full()
            .w(px(self.layout.docks.left.extent))
            .flex_none()
            .pt(px(52.0))
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

impl Focusable for WorkspaceWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            WorkspaceContent::Pdf(reader) => reader.read(cx).focus_handle(cx),
            WorkspaceContent::Settings => self.focus_handle.clone(),
        }
    }
}

impl Render for WorkspaceWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.content {
            WorkspaceContent::Pdf(reader) => reader.clone().into_any_element(),
            WorkspaceContent::Settings => self.render_settings(cx),
        };
        let left_dock = (self.layout.docks.left.open && self.view.kind == ItemKind::Settings)
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
            .bg(tokens.surface.background)
            .children(left_dock)
            .child(div().h_full().flex_1().overflow_hidden().child(content))
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
