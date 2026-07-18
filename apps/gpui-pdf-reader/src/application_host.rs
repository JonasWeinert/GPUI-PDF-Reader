//! Process-global services, workspace identity, and window lifecycle.

use crate::app_extensions::ReaderExtensions;
use crate::workspace_window::WorkspaceWindow;
#[cfg(target_os = "macos")]
use gpui::TitlebarOptions;
use gpui::{
    AnyWindowHandle, App, Bounds, Entity, Point, WindowBounds, WindowHandle, WindowOptions, px,
    size,
};
use key_workspace_core::{
    Capability, Generation, IdGenerator, ItemKind, ItemSource, ResourceCoordinator, ResourceMode,
    SystemResources, ViewId, WindowId, WorkspaceItemDescriptor, WorkspaceViewDescriptor,
    WorkspaceWindowDescriptor,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::{cell::RefCell, rc::Rc};

const GIB: u64 = 1024 * 1024 * 1024;

struct WindowRecord {
    descriptor: WorkspaceWindowDescriptor,
    item: Option<WorkspaceItemDescriptor>,
    handle: AnyWindowHandle,
}

/// Long-lived application owner. It has no rendered surface and contains no
/// document-view interaction state.
pub(crate) struct ApplicationHost {
    extensions: Rc<RefCell<ReaderExtensions>>,
    ids: IdGenerator,
    windows: BTreeMap<WindowId, WindowRecord>,
    paths: HashMap<PathBuf, WindowId>,
    settings_window: Option<WindowId>,
    active_view: Option<ViewId>,
    resources: ResourceCoordinator,
}

impl ApplicationHost {
    pub(crate) fn new(extensions: ReaderExtensions) -> Self {
        let logical_cpus = std::thread::available_parallelism()
            .map_or(1, |parallelism| parallelism.get())
            .min(usize::from(u16::MAX)) as u16;
        Self {
            extensions: Rc::new(RefCell::new(extensions)),
            ids: IdGenerator::new(1),
            windows: BTreeMap::new(),
            paths: HashMap::new(),
            settings_window: None,
            active_view: None,
            // A conservative portable baseline until the platform signal
            // adapter supplies live physical-memory and power information.
            resources: ResourceCoordinator::new(
                ResourceMode::Auto,
                SystemResources {
                    physical_memory_bytes: 8 * GIB,
                    logical_cpus,
                    low_power_mode: false,
                },
            ),
        }
    }

    pub(crate) fn extensions(&self) -> Rc<RefCell<ReaderExtensions>> {
        self.extensions.clone()
    }

    pub(crate) fn resource_coordinator(&self) -> ResourceCoordinator {
        self.resources
    }

    pub(crate) fn set_active_view(&mut self, view: ViewId) -> bool {
        let changed = self.active_view != Some(view);
        self.active_view = Some(view);
        changed
    }

    pub(crate) fn is_active_view(&self, view: ViewId) -> bool {
        self.active_view == Some(view)
    }

    pub(crate) fn prune_closed_windows(&mut self, open: &[AnyWindowHandle]) {
        self.windows
            .retain(|_, record| open.contains(&record.handle));
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
    let path_for_window = normalized.clone();
    let handle = cx
        .open_window(
            window_options(host.read(cx).windows.len()),
            move |window, cx| {
                WorkspaceWindow::new_pdf(
                    host_for_window,
                    descriptor_for_window,
                    item_for_window,
                    view_descriptor,
                    path_for_window,
                    window,
                    cx,
                )
            },
        )
        .map_err(|error| error.to_string())?;
    host.update(cx, |host, _| {
        host.insert_window(window_descriptor, item_descriptor, handle.into())
    });
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
    let handle = cx
        .open_window(
            window_options(host.read(cx).windows.len()),
            move |window, cx| {
                WorkspaceWindow::new_settings(
                    host_for_window,
                    descriptor_for_window,
                    view_descriptor,
                    window,
                    cx,
                )
            },
        )
        .map_err(|error| error.to_string())?;
    host.update(cx, |host, _| {
        host.settings_window = Some(window_descriptor.id);
        host.insert_window(window_descriptor, None, handle.into());
    });
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
