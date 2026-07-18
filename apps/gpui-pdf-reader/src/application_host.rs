//! Process-global services, workspace identity, and window lifecycle.

use crate::app_extensions::ReaderExtensions;
use crate::backend::{PdfEngineSupervisor, start_pdf_engine_supervisor};
use crate::system_resources;
use crate::theme::ThemePreference;
use crate::workspace_window::WorkspaceWindow;
#[cfg(target_os = "macos")]
use gpui::TitlebarOptions;
use gpui::{
    AnyWindowHandle, App, Bounds, Entity, Point, SharedString, WindowBounds, WindowHandle,
    WindowOptions, px, size,
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
    item: Option<WorkspaceItemDescriptor>,
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
    paths: HashMap<PathBuf, WindowId>,
    settings_window: Option<WindowId>,
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
            settings_window: None,
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
            .filter_map(|record| record.descriptor.active_view)
            .collect::<Vec<_>>();
        self.windows
            .retain(|_, record| open.contains(&record.handle));
        for view in &removed_views {
            self.resource_registry.remove(participant_for(*view));
        }
        self.view_mru.retain(|view| !removed_views.contains(view));
        self.paths
            .retain(|_, window| self.windows.contains_key(window));
        if self
            .settings_window
            .is_some_and(|window| !self.windows.contains_key(&window))
        {
            self.settings_window = None;
        }
        if self.active_view.is_some_and(|view| {
            !self
                .windows
                .values()
                .any(|record| record.descriptor.active_view == Some(view))
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
                        .descriptor
                        .active_view
                        .is_some_and(|view| participant_for(view) == change.current.id))
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
        let window = WorkspaceWindowDescriptor {
            id: window_id,
            title,
            active_view: Some(view_id),
        };
        (window, item, view)
    }

    fn allocate_settings(&self) -> (WorkspaceWindowDescriptor, WorkspaceViewDescriptor) {
        let window_id = self.ids.window();
        let view_id = self.ids.view();
        (
            WorkspaceWindowDescriptor {
                id: window_id,
                title: "Settings".into(),
                active_view: Some(view_id),
            },
            WorkspaceViewDescriptor {
                id: view_id,
                generation: Generation::INITIAL,
                item_id: None,
                kind: ItemKind::Settings,
                title: "Settings".into(),
                capabilities: BTreeSet::from([Capability::Settings]),
            },
        )
    }

    fn insert_window(
        &mut self,
        descriptor: WorkspaceWindowDescriptor,
        item: Option<WorkspaceItemDescriptor>,
        view: &WorkspaceViewDescriptor,
        handle: AnyWindowHandle,
    ) {
        if let Some(item) = &item
            && let ItemSource::File(path) = &item.source
        {
            self.paths.insert(normalize_path(path), descriptor.id);
        }
        self.windows.insert(
            descriptor.id,
            WindowRecord {
                descriptor,
                item,
                handle,
            },
        );
        self.resource_registry.upsert(ParticipantSnapshot {
            id: participant_for(view.id),
            activity: ActivityLevel::BackgroundWarm,
            profile: resource_profile(&view.kind),
        });
        self.view_mru.retain(|candidate| *candidate != view.id);
        self.view_mru.push_back(view.id);
    }

    fn existing_pdf_window(&self, path: &Path) -> Option<AnyWindowHandle> {
        self.paths
            .get(&normalize_path(path))
            .and_then(|id| self.windows.get(id))
            .map(|record| record.handle)
    }

    fn empty_pdf_window(&self, id: WindowId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|record| record.item.is_none())
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
    for (handle, allocation) in targets {
        if let Some(handle) = handle.downcast::<WorkspaceWindow>() {
            handle
                .update(cx, |workspace, window, cx| {
                    workspace.apply_resource_allocation(allocation, window, cx)
                })
                .ok();
        }
    }
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
        && let Some(existing) = host.read(cx).existing_pdf_window(path)
        && let Some(existing) = existing.downcast::<WorkspaceWindow>()
    {
        existing
            .update(cx, |_, window, _| window.activate_window())
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

pub(crate) fn open_pdf_from(
    host: Entity<ApplicationHost>,
    source: WindowId,
    path: PathBuf,
    cx: &mut App,
) -> Result<WindowHandle<WorkspaceWindow>, String> {
    let path = normalize_path(&path);
    if let Some(existing) = host.read(cx).existing_pdf_window(&path)
        && let Some(existing) = existing.downcast::<WorkspaceWindow>()
    {
        existing
            .update(cx, |_, window, _| window.activate_window())
            .map_err(|error| error.to_string())?;
        return Ok(existing);
    }

    if host.read(cx).empty_pdf_window(source) {
        let (item, view, handle) = {
            let host_ref = host.read(cx);
            let record = host_ref
                .windows
                .get(&source)
                .ok_or_else(|| "source window is no longer open".to_owned())?;
            let item = WorkspaceItemDescriptor {
                id: host_ref.ids.item(),
                generation: Generation::INITIAL,
                kind: ItemKind::Pdf,
                title: file_title(&path, "PDF"),
                source: ItemSource::File(path.clone()),
                capabilities: BTreeSet::from([
                    Capability::Read,
                    Capability::Search,
                    Capability::Annotate,
                    Capability::NavigateOutline,
                ]),
            };
            let view = WorkspaceViewDescriptor {
                id: record
                    .descriptor
                    .active_view
                    .expect("PDF windows own a view"),
                generation: Generation::INITIAL,
                item_id: Some(item.id),
                kind: ItemKind::Pdf,
                title: item.title.clone(),
                capabilities: item.capabilities.clone(),
            };
            (item, view, record.handle)
        };
        let handle = handle
            .downcast::<WorkspaceWindow>()
            .ok_or_else(|| "source window has an unexpected root type".to_owned())?;
        handle
            .update(cx, |workspace, window, cx| {
                workspace.open_pdf(path.clone(), item.clone(), view, window, cx)
            })
            .map_err(|error| error.to_string())?;
        host.update(cx, |host, _| {
            if let Some(record) = host.windows.get_mut(&source) {
                record.descriptor.title = item.title.clone();
                record.item = Some(item);
            }
            host.paths.insert(path, source);
        });
        return Ok(handle);
    }

    open_pdf_window(host, Some(path), cx)
}

pub(crate) fn open_settings_window(
    host: Entity<ApplicationHost>,
    cx: &mut App,
) -> Result<WindowHandle<WorkspaceWindow>, String> {
    if let Some(id) = host.read(cx).settings_window
        && let Some(record) = host.read(cx).windows.get(&id)
        && let Some(handle) = record.handle.downcast::<WorkspaceWindow>()
    {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .map_err(|error| error.to_string())?;
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
        host.settings_window = Some(window_descriptor.id);
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
