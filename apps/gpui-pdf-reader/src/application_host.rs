//! Process-global services, workspace identity, and window lifecycle.

use crate::app_extensions::ReaderExtensions;
use crate::backend::{PdfEngineSupervisor, start_pdf_engine_supervisor};
use crate::system_resources;
use crate::theme::ThemePreference;
use crate::workspace_window::WorkspaceWindow;
#[cfg(target_os = "macos")]
use gpui::TitlebarOptions;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, Point, SharedString, Window, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};
#[cfg(feature = "scholarly-network")]
use key_reference::{ReferenceDocumentScope, ReferenceExecutor};
use key_sidecar_store::AnnotationService;
use key_workspace_core::{
    ActivityLevel, Capability, Generation, IdGenerator, ItemKind, ItemSource, ParticipantSnapshot,
    ResourceAllocation, ResourceAmount, ResourceCoordinator, ResourceMode, ResourceParticipantId,
    ResourceProfile, ResourceRegistry, ViewId, WindowId, WorkspaceItemDescriptor,
    WorkspaceViewDescriptor, WorkspaceWindowDescriptor,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::{cell::RefCell, rc::Rc};

struct WindowRecord {
    descriptor: WorkspaceWindowDescriptor,
    views: BTreeMap<ViewId, Option<WorkspaceItemDescriptor>>,
    handle: AnyWindowHandle,
}

/// Long-lived application owner. It has no rendered surface and contains no
/// document-view interaction state.
pub(crate) struct ApplicationHost {
    extensions: Rc<RefCell<ReaderExtensions>>,
    annotation_service: AnnotationService,
    pdf_engine: PdfEngineSupervisor,
    #[cfg(feature = "scholarly-network")]
    reference_executor: ReferenceExecutor,
    ids: IdGenerator,
    windows: BTreeMap<WindowId, WindowRecord>,
    paths: HashMap<PathBuf, (WindowId, ViewId)>,
    settings_view: Option<(WindowId, ViewId)>,
    active_view: Option<ViewId>,
    view_mru: VecDeque<ViewId>,
    resources: ResourceCoordinator,
    resource_registry: ResourceRegistry,
    theme_preference: ThemePreference,
    selected_theme: Option<SharedString>,
}

impl ApplicationHost {
    pub(crate) fn new(extensions: ReaderExtensions) -> Self {
        Self {
            extensions: Rc::new(RefCell::new(extensions)),
            annotation_service: AnnotationService::start(),
            pdf_engine: start_pdf_engine_supervisor()
                .expect("failed to start the process-wide PDFium engine owner"),
            #[cfg(feature = "scholarly-network")]
            reference_executor: ReferenceExecutor::global(),
            ids: IdGenerator::new(1),
            windows: BTreeMap::new(),
            paths: HashMap::new(),
            settings_view: None,
            active_view: None,
            view_mru: VecDeque::new(),
            resources: ResourceCoordinator::new(ResourceMode::Auto, system_resources::detect()),
            resource_registry: ResourceRegistry::default(),
            theme_preference: ThemePreference::System,
            selected_theme: None,
        }
    }

    pub(crate) fn extensions(&self) -> Rc<RefCell<ReaderExtensions>> {
        self.extensions.clone()
    }

    pub(crate) fn annotation_service(&self) -> AnnotationService {
        self.annotation_service.clone()
    }

    pub(crate) fn pdf_engine(&self) -> PdfEngineSupervisor {
        self.pdf_engine.clone()
    }

    #[cfg(feature = "scholarly-network")]
    pub(crate) fn reference_document_scope(&self) -> ReferenceDocumentScope {
        self.reference_executor.document_scope()
    }

    pub(crate) fn resource_coordinator(&self) -> ResourceCoordinator {
        self.resources
    }

    pub(crate) fn requested_resource_mode(&self) -> ResourceMode {
        self.resources.mode()
    }

    pub(crate) fn theme_selection(&self) -> (ThemePreference, Option<SharedString>) {
        (self.theme_preference, self.selected_theme.clone())
    }

    pub(crate) fn set_theme_selection(
        &mut self,
        preference: ThemePreference,
        selected: Option<SharedString>,
    ) {
        self.theme_preference = preference;
        self.selected_theme = selected;
    }

    fn set_active_view(&mut self, view: ViewId) -> bool {
        let changed = self.active_view != Some(view);
        self.active_view = Some(view);
        self.view_mru.retain(|candidate| *candidate != view);
        self.view_mru.push_front(view);
        self.refresh_activity_levels();
        changed
    }

    pub(crate) fn is_active_view(&self, view: ViewId) -> bool {
        self.active_view == Some(view)
    }

    pub(crate) fn prune_closed_windows(&mut self, open: &[AnyWindowHandle]) {
        let removed_views = self
            .windows
            .values()
            .filter(|record| !open.contains(&record.handle))
            .flat_map(|record| record.views.keys().copied())
            .collect::<Vec<_>>();
        self.windows
            .retain(|_, record| open.contains(&record.handle));
        for view in &removed_views {
            self.resource_registry.remove(participant_for(*view));
        }
        self.view_mru.retain(|view| !removed_views.contains(view));
        self.paths
            .retain(|_, (window, _)| self.windows.contains_key(window));
        if self.settings_view.is_some_and(|(window, view)| {
            !self
                .windows
                .get(&window)
                .is_some_and(|record| record.views.contains_key(&view))
        }) {
            self.settings_view = None;
        }
        if self.active_view.is_some_and(|view| {
            !self
                .windows
                .values()
                .any(|record| record.views.contains_key(&view))
        }) {
            self.active_view = None;
        }
        self.refresh_activity_levels();
    }

    fn refresh_activity_levels(&mut self) {
        for (index, view) in self.view_mru.iter().copied().enumerate() {
            let activity = if Some(view) == self.active_view {
                ActivityLevel::ForegroundInteractive
            } else {
                match index {
                    0 | 1 => ActivityLevel::BackgroundWarm,
                    2 | 3 => ActivityLevel::BackgroundCold,
                    _ => ActivityLevel::Suspended,
                }
            };
            self.resource_registry
                .set_activity(participant_for(view), activity);
        }
    }

    fn reconcile_resource_targets(&mut self) -> Vec<(AnyWindowHandle, ResourceAllocation)> {
        let reconciliation = self.resource_registry.reconcile(&self.resources);
        reconciliation
            .changed
            .into_iter()
            .filter_map(|change| {
                self.windows.values().find_map(|record| {
                    (record
                        .views
                        .keys()
                        .any(|view| participant_for(*view) == change.current.id))
                    .then_some((record.handle, change.current))
                })
            })
            .collect()
    }

    fn allocate_pdf(
        &self,
        path: Option<&Path>,
    ) -> (
        WorkspaceWindowDescriptor,
        Option<WorkspaceItemDescriptor>,
        WorkspaceViewDescriptor,
    ) {
        let window_id = self.ids.window();
        let (item, view) = self.allocate_pdf_view(path);
        let title = view.title.clone();
        let window = WorkspaceWindowDescriptor {
            id: window_id,
            title,
            active_view: Some(view.id),
        };
        (window, item, view)
    }

    fn allocate_pdf_view(
        &self,
        path: Option<&Path>,
    ) -> (Option<WorkspaceItemDescriptor>, WorkspaceViewDescriptor) {
        let view_id = self.ids.view();
        let item = path.map(|path| WorkspaceItemDescriptor {
            id: self.ids.item(),
            generation: Generation::INITIAL,
            kind: ItemKind::Pdf,
            title: file_title(path, "PDF"),
            source: ItemSource::File(path.to_path_buf()),
            capabilities: BTreeSet::from([
                Capability::Read,
                Capability::Search,
                Capability::Annotate,
                Capability::NavigateOutline,
            ]),
        });
        let title = item
            .as_ref()
            .map_or_else(|| "GPUI PDF Reader".to_owned(), |item| item.title.clone());
        let view = WorkspaceViewDescriptor {
            id: view_id,
            generation: Generation::INITIAL,
            item_id: item.as_ref().map(|item| item.id),
            kind: ItemKind::Pdf,
            title: title.clone(),
            capabilities: item
                .as_ref()
                .map_or_else(BTreeSet::new, |item| item.capabilities.clone()),
        };
        (item, view)
    }

    fn allocate_settings(&self) -> (WorkspaceWindowDescriptor, WorkspaceViewDescriptor) {
        let window_id = self.ids.window();
        let view = self.allocate_settings_view();
        (
            WorkspaceWindowDescriptor {
                id: window_id,
                title: "Settings".into(),
                active_view: Some(view.id),
            },
            view,
        )
    }

    fn allocate_settings_view(&self) -> WorkspaceViewDescriptor {
        WorkspaceViewDescriptor {
            id: self.ids.view(),
            generation: Generation::INITIAL,
            item_id: None,
            kind: ItemKind::Settings,
            title: "Settings".into(),
            capabilities: BTreeSet::from([Capability::Settings]),
        }
    }

    fn insert_window(
        &mut self,
        descriptor: WorkspaceWindowDescriptor,
        item: Option<WorkspaceItemDescriptor>,
        view: &WorkspaceViewDescriptor,
        handle: AnyWindowHandle,
    ) {
        let window_id = descriptor.id;
        let mut views = BTreeMap::new();
        views.insert(view.id, item.clone());
        self.windows.insert(
            descriptor.id,
            WindowRecord {
                descriptor,
                views,
                handle,
            },
        );
        self.register_view_path(item.as_ref(), window_id, view.id);
        self.register_participant(view);
    }

    fn register_view(
        &mut self,
        window: WindowId,
        item: Option<WorkspaceItemDescriptor>,
        view: &WorkspaceViewDescriptor,
    ) {
        if let Some(record) = self.windows.get_mut(&window) {
            record.views.insert(view.id, item.clone());
            record.descriptor.active_view = Some(view.id);
            record.descriptor.title = view.title.clone();
        }
        self.register_view_path(item.as_ref(), window, view.id);
        self.register_participant(view);
    }

    fn register_view_path(
        &mut self,
        item: Option<&WorkspaceItemDescriptor>,
        window: WindowId,
        view: ViewId,
    ) {
        if let Some(item) = item
            && let ItemSource::File(path) = &item.source
        {
            self.paths.insert(normalize_path(path), (window, view));
        }
    }

    fn register_participant(&mut self, view: &WorkspaceViewDescriptor) {
        self.resource_registry.upsert(ParticipantSnapshot {
            id: participant_for(view.id),
            activity: ActivityLevel::BackgroundWarm,
            profile: resource_profile(&view.kind),
        });
        self.view_mru.retain(|candidate| *candidate != view.id);
        self.view_mru.push_back(view.id);
        self.refresh_activity_levels();
    }

    fn existing_pdf_view(&self, path: &Path) -> Option<(AnyWindowHandle, ViewId)> {
        self.paths
            .get(&normalize_path(path))
            .and_then(|(id, view)| self.windows.get(id).map(|record| (record.handle, *view)))
    }

    fn empty_pdf_window(&self, id: WindowId) -> bool {
        self.windows
            .get(&id)
            .and_then(|record| record.descriptor.active_view.map(|view| (record, view)))
            .is_some_and(|(record, view)| record.views.get(&view).is_some_and(Option::is_none))
    }
}

const MIB: u64 = 1024 * 1024;

fn participant_for(view: ViewId) -> ResourceParticipantId {
    ResourceParticipantId::from_raw(view.get())
}

fn resource_profile(kind: &ItemKind) -> ResourceProfile {
    match kind {
        ItemKind::Pdf => ResourceProfile {
            minimum: ResourceAmount {
                cpu_memory_bytes: 24 * MIB,
                gpu_memory_bytes: 16 * MIB,
                worker_slots: 0,
                network_slots: 0,
            },
            target: ResourceAmount {
                cpu_memory_bytes: 192 * MIB,
                gpu_memory_bytes: 128 * MIB,
                worker_slots: 2,
                network_slots: 2,
            },
            weight: 3,
            suspendable: true,
        },
        ItemKind::Settings => ResourceProfile {
            minimum: ResourceAmount {
                cpu_memory_bytes: 4 * MIB,
                gpu_memory_bytes: 2 * MIB,
                worker_slots: 0,
                network_slots: 0,
            },
            target: ResourceAmount {
                cpu_memory_bytes: 16 * MIB,
                gpu_memory_bytes: 8 * MIB,
                worker_slots: 0,
                network_slots: 0,
            },
            weight: 1,
            suspendable: true,
        },
        ItemKind::Markdown | ItemKind::Video | ItemKind::Custom(_) => ResourceProfile::new(
            ResourceAmount {
                cpu_memory_bytes: 8 * MIB,
                gpu_memory_bytes: 4 * MIB,
                worker_slots: 0,
                network_slots: 0,
            },
            ResourceAmount {
                cpu_memory_bytes: 128 * MIB,
                gpu_memory_bytes: 64 * MIB,
                worker_slots: 1,
                network_slots: 1,
            },
        ),
    }
}

fn apply_resource_targets(targets: Vec<(AnyWindowHandle, ResourceAllocation)>, cx: &mut App) {
    if targets.is_empty() {
        return;
    }
    // Activation callbacks run while their source window is on GPUI's update
    // stack. Defer all handles together so the current window is addressable
    // again and every participant actually receives its new allocation.
    cx.defer(move |cx| {
        for (handle, allocation) in targets {
            if let Some(handle) = handle.downcast::<WorkspaceWindow>() {
                handle
                    .update(cx, |workspace, window, cx| {
                        workspace.apply_resource_allocation(allocation, window, cx)
                    })
                    .ok();
            }
        }
    });
}

pub(crate) fn activate_workspace_view(
    host: Entity<ApplicationHost>,
    view: ViewId,
    cx: &mut App,
) -> bool {
    let (changed, targets) = host.update(cx, |host, _| {
        let changed = host.set_active_view(view);
        (changed, host.reconcile_resource_targets())
    });
    apply_resource_targets(targets, cx);
    changed
}

pub(crate) fn select_workspace_tab(
    host: Entity<ApplicationHost>,
    window: WindowId,
    view: ViewId,
    title: String,
    cx: &mut App,
) -> bool {
    host.update(cx, |host, _| {
        if let Some(record) = host.windows.get_mut(&window) {
            record.descriptor.active_view = Some(view);
            record.descriptor.title = title;
        }
    });
    activate_workspace_view(host, view, cx)
}

pub(crate) fn remove_workspace_view(
    host: Entity<ApplicationHost>,
    window: WindowId,
    view: ViewId,
    next_active: Option<(ViewId, String)>,
    cx: &mut App,
) {
    let targets = host.update(cx, |host, _| {
        if let Some(record) = host.windows.get_mut(&window) {
            record.views.remove(&view);
            record.descriptor.active_view = next_active.as_ref().map(|(view, _)| *view);
            if let Some((_, title)) = next_active.as_ref() {
                record.descriptor.title = title.clone();
            }
        }
        host.paths.retain(|_, (_, candidate)| *candidate != view);
        if host
            .settings_view
            .is_some_and(|(_, candidate)| candidate == view)
        {
            host.settings_view = None;
        }
        host.resource_registry.remove(participant_for(view));
        host.view_mru.retain(|candidate| *candidate != view);
        if host.active_view == Some(view) {
            host.active_view = next_active.as_ref().map(|(view, _)| *view);
        }
        host.refresh_activity_levels();
        host.reconcile_resource_targets()
    });
    apply_resource_targets(targets, cx);
}

pub(crate) fn workspace_window_handle(
    host: &Entity<ApplicationHost>,
    window: WindowId,
    cx: &App,
) -> Option<WindowHandle<WorkspaceWindow>> {
    host.read(cx)
        .windows
        .get(&window)
        .and_then(|record| record.handle.downcast::<WorkspaceWindow>())
}

pub(crate) fn transfer_workspace_view_registration(
    host: Entity<ApplicationHost>,
    source: WindowId,
    target: WindowId,
    view: &WorkspaceViewDescriptor,
    cx: &mut App,
) {
    if source == target {
        return;
    }
    host.update(cx, |host, _| {
        let item = host
            .windows
            .get_mut(&source)
            .and_then(|record| record.views.remove(&view.id))
            .flatten();
        if let Some(source_record) = host.windows.get_mut(&source)
            && source_record.descriptor.active_view == Some(view.id)
        {
            source_record.descriptor.active_view = None;
        }
        if let Some(target_record) = host.windows.get_mut(&target) {
            target_record.views.insert(view.id, item);
            target_record.descriptor.active_view = Some(view.id);
            target_record.descriptor.title = view.title.clone();
        }
        for (window, candidate) in host.paths.values_mut() {
            if *candidate == view.id {
                *window = target;
            }
        }
        if host
            .settings_view
            .is_some_and(|(_, candidate)| candidate == view.id)
        {
            host.settings_view = Some((target, view.id));
        }
    });
}

pub(crate) fn idle_workspace_view(host: Entity<ApplicationHost>, view: ViewId, cx: &mut App) {
    let targets = host.update(cx, |host, _| {
        if host.active_view == Some(view) {
            host.resource_registry
                .set_activity(participant_for(view), ActivityLevel::ForegroundIdle);
        }
        host.reconcile_resource_targets()
    });
    apply_resource_targets(targets, cx);
}

pub(crate) fn set_resource_mode(host: Entity<ApplicationHost>, mode: ResourceMode, cx: &mut App) {
    let targets = host.update(cx, |host, _| {
        host.resources.set_mode(mode);
        host.reconcile_resource_targets()
    });
    apply_resource_targets(targets, cx);
}

pub(crate) fn prune_workspace_windows(
    host: Entity<ApplicationHost>,
    open: &[AnyWindowHandle],
    cx: &mut App,
) {
    let targets = host.update(cx, |host, _| {
        host.prune_closed_windows(open);
        host.reconcile_resource_targets()
    });
    apply_resource_targets(targets, cx);
}

pub(crate) fn open_pdf_window(
    host: Entity<ApplicationHost>,
    path: Option<PathBuf>,
    cx: &mut App,
) -> Result<WindowHandle<WorkspaceWindow>, String> {
    if let Some(path) = path.as_deref()
        && let Some((existing, view)) = host.read(cx).existing_pdf_view(path)
        && let Some(existing) = existing.downcast::<WorkspaceWindow>()
    {
        existing
            .update(cx, |workspace, window, cx| {
                workspace.activate_tab(view, window, cx);
                window.activate_window();
            })
            .map_err(|error| error.to_string())?;
        return Ok(existing);
    }

    let normalized = path.as_deref().map(normalize_path);
    let (window_descriptor, item_descriptor, view_descriptor) =
        host.read(cx).allocate_pdf(normalized.as_deref());
    let host_for_window = host.clone();
    let descriptor_for_window = window_descriptor.clone();
    let item_for_window = item_descriptor.clone();
    let view_for_window = view_descriptor.clone();
    let path_for_window = normalized.clone();
    let handle = cx
        .open_window(
            window_options(host.read(cx).windows.len()),
            move |window, cx| {
                WorkspaceWindow::new_pdf(
                    host_for_window,
                    descriptor_for_window,
                    item_for_window,
                    view_for_window,
                    path_for_window,
                    window,
                    cx,
                )
            },
        )
        .map_err(|error| error.to_string())?;
    host.update(cx, |host, _| {
        host.insert_window(
            window_descriptor,
            item_descriptor,
            &view_descriptor,
            handle.into(),
        )
    });
    activate_workspace_view(host, view_descriptor.id, cx);
    Ok(handle)
}

/// Opens a picker result through the live root of the source window. GPUI has
/// removed that window from `App::windows` while an async window callback runs,
/// so re-entering its stored handle here would fail with `window not found`.
pub(crate) fn open_pdf_from_window(
    host: Entity<ApplicationHost>,
    source: WindowId,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let workspace = window
        .root::<WorkspaceWindow>()
        .flatten()
        .ok_or_else(|| "source window has an unexpected root type".to_owned())?;
    workspace.update(cx, |workspace, cx| {
        open_pdf_from_workspace(host, source, path, workspace, window, cx)
    })
}

fn open_pdf_from_workspace(
    host: Entity<ApplicationHost>,
    source: WindowId,
    path: PathBuf,
    workspace: &mut WorkspaceWindow,
    window: &mut Window,
    cx: &mut Context<WorkspaceWindow>,
) -> Result<(), String> {
    let path = normalize_path(&path);
    if let Some((existing_id, existing_view)) = host.read(cx).paths.get(&path).copied() {
        if existing_id == source {
            workspace.activate_tab(existing_view, window, cx);
            window.activate_window();
            return Ok(());
        }
        let existing = host
            .read(cx)
            .windows
            .get(&existing_id)
            .map(|record| record.handle)
            .ok_or_else(|| "existing PDF window is no longer registered".to_owned())?;
        let Some(existing) = existing.downcast::<WorkspaceWindow>() else {
            return Err("existing PDF window has an unexpected root type".to_owned());
        };
        existing
            .update(cx, |workspace, window, cx| {
                workspace.activate_tab(existing_view, window, cx);
                window.activate_window();
            })
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    if host.read(cx).empty_pdf_window(source) {
        let (item, view) = {
            let host_ref = host.read(cx);
            let record = host_ref
                .windows
                .get(&source)
                .ok_or_else(|| "source window is no longer open".to_owned())?;
            let (item, view) = host_ref.allocate_pdf_view(Some(&path));
            let item = item.expect("a path-backed PDF allocation includes an item");
            let view = WorkspaceViewDescriptor {
                id: record
                    .descriptor
                    .active_view
                    .expect("PDF windows own a view"),
                generation: view.generation,
                item_id: Some(item.id),
                kind: ItemKind::Pdf,
                title: item.title.clone(),
                capabilities: item.capabilities.clone(),
            };
            (item, view)
        };
        let view_id = view.id;
        workspace.open_pdf(path.clone(), item.clone(), view, window, cx);
        host.update(cx, |host, _| {
            if let Some(record) = host.windows.get_mut(&source) {
                record.descriptor.title = item.title.clone();
                if let Some(view) = record.descriptor.active_view {
                    record.views.insert(view, Some(item));
                }
            }
            host.paths.insert(path, (source, view_id));
        });
        return Ok(());
    }

    let (item, view) = host.read(cx).allocate_pdf_view(Some(&path));
    workspace.add_pdf_tab(item.clone(), view.clone(), path, window, cx);
    host.update(cx, |host, _| host.register_view(source, item, &view));
    activate_workspace_view(host, view.id, cx);
    Ok(())
}

pub(crate) fn open_pdf_tab(
    host: Entity<ApplicationHost>,
    handle: WindowHandle<WorkspaceWindow>,
    path: PathBuf,
    cx: &mut App,
) -> Result<(), String> {
    handle
        .update(cx, |workspace, window, cx| {
            open_pdf_from_workspace(host, workspace.window_id(), path, workspace, window, cx)
        })
        .map_err(|error| error.to_string())?
}

pub(crate) fn open_settings_window(
    host: Entity<ApplicationHost>,
    cx: &mut App,
) -> Result<WindowHandle<WorkspaceWindow>, String> {
    if let Some((id, view)) = host.read(cx).settings_view
        && let Some(record) = host.read(cx).windows.get(&id)
        && let Some(handle) = record.handle.downcast::<WorkspaceWindow>()
    {
        handle
            .update(cx, |workspace, window, cx| {
                workspace.activate_tab(view, window, cx);
                window.activate_window();
            })
            .map_err(|error| error.to_string())?;
        return Ok(handle);
    }

    if let Some((window_id, record_handle)) = host.read(cx).active_view.and_then(|active| {
        host.read(cx).windows.iter().find_map(|(id, record)| {
            record
                .views
                .contains_key(&active)
                .then_some((*id, record.handle))
        })
    }) && let Some(handle) = record_handle.downcast::<WorkspaceWindow>()
    {
        let view = host.read(cx).allocate_settings_view();
        handle
            .update(cx, |workspace, window, cx| {
                workspace.add_settings_tab(view.clone(), window, cx);
                window.activate_window();
            })
            .map_err(|error| error.to_string())?;
        host.update(cx, |host, _| {
            host.settings_view = Some((window_id, view.id));
            host.register_view(window_id, None, &view);
        });
        activate_workspace_view(host, view.id, cx);
        return Ok(handle);
    }

    let (window_descriptor, view_descriptor) = host.read(cx).allocate_settings();
    let host_for_window = host.clone();
    let descriptor_for_window = window_descriptor.clone();
    let view_for_window = view_descriptor.clone();
    let handle = cx
        .open_window(
            window_options(host.read(cx).windows.len()),
            move |window, cx| {
                WorkspaceWindow::new_settings(
                    host_for_window,
                    descriptor_for_window,
                    view_for_window,
                    window,
                    cx,
                )
            },
        )
        .map_err(|error| error.to_string())?;
    host.update(cx, |host, _| {
        host.settings_view = Some((window_descriptor.id, view_descriptor.id));
        host.insert_window(window_descriptor, None, &view_descriptor, handle.into());
    });
    activate_workspace_view(host, view_descriptor.id, cx);
    Ok(handle)
}

fn window_options(index: usize) -> WindowOptions {
    let cascade = (index.min(8) as f32) * 24.0;
    let bounds = Bounds {
        origin: Point::new(px(120.0 + cascade), px(80.0 + cascade)),
        size: size(px(1180.0), px(820.0)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(700.0), px(480.0))),
        focus: true,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(Point::new(px(14.0), px(18.0))),
        }),
        ..options
    };
    options
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn file_title(path: &Path, fallback: &str) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::normalize_path;
    use std::path::Path;

    #[test]
    fn missing_paths_remain_stable_without_panicking() {
        assert_eq!(
            normalize_path(Path::new("definitely-missing.pdf")),
            Path::new("definitely-missing.pdf")
        );
    }
}
