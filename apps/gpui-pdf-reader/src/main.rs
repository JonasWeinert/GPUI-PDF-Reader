mod annotations;
mod app_extensions;
mod application_host;
mod backend;
mod document_jump;
mod extension_assets;
#[cfg(feature = "installable-extensions")]
mod extension_packages;
#[cfg(feature = "installable-extensions")]
mod extension_registry;
mod floating_panel;
#[cfg(feature = "scholarly-network")]
mod link_preview;
#[cfg(not(feature = "scholarly-network"))]
#[path = "link_preview_disabled.rs"]
mod link_preview;
mod link_resolution;
mod markdown_editor;
mod model;
mod native_gestures;
mod navigation_focus;
mod pdf_capability_bridge;
#[cfg(debug_assertions)]
mod qa;
#[cfg(debug_assertions)]
mod qa_resources;
mod reader;
#[cfg(feature = "scholarly-network")]
mod scholarly;
#[cfg(not(feature = "scholarly-network"))]
#[path = "scholarly_disabled.rs"]
mod scholarly;
mod scientific;
mod search;
mod system_resources;
mod text_field;
mod theme;
mod workspace_window;

use gpui::{App, AppContext, Application, KeyBinding, Menu, MenuItem, actions};
use markdown_editor::{
    CommentBackspace, CommentCancel, CommentDelete, CommentDown, CommentEnd, CommentHome,
    CommentLeft, CommentNewline, CommentRight, CommentSave, CommentSelectDown, CommentSelectLeft,
    CommentSelectRight, CommentSelectUp, CommentToggleBold, CommentToggleBulletedList,
    CommentToggleCode, CommentToggleItalic, CommentToggleNumberedList, CommentUp,
};
use std::{path::PathBuf, sync::Arc};
use text_field::{
    FieldBackspace, FieldCancel, FieldDelete, FieldEnd, FieldHome, FieldLeft, FieldRight,
    FieldSelectLeft, FieldSelectRight, FieldSubmit,
};

pub use key_editor_gpui::{
    MarkdownCopy as EditCopy, MarkdownCut as EditCut, MarkdownPaste as EditPaste,
    MarkdownSelectAll as EditSelectAll,
};

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = gpui_pdf_reader, no_json)]
pub struct OpenExtensionDetails {
    pub extension: key_extension_api::ExtensionId,
}

const READER_NAVIGATION_CONTEXT: &str = "PdfReader && !TextField && !MarkdownEditor";

actions!(
    gpui_pdf_reader,
    [
        OpenDocument,
        InstallExtension,
        ManageExtensions,
        ZoomIn,
        ZoomOut,
        ActualSize,
        FitWidth,
        CopySelection,
        SelectAll,
        ScrollUp,
        ScrollDown,
        ScrollLeft,
        ScrollRight,
        PageUp,
        PageDown,
        FirstPage,
        LastPage,
        Find,
        ToggleComments,
        AddComment,
        ClassicView,
        NextSearchResult,
        PreviousSearchResult,
        FluidView,
        OpenSettings,
        Quit,
    ]
);

fn main() {
    let initial_paths = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    let extension_assets = Arc::new(extension_assets::ExtensionAssetStore::default());
    let application = Application::new().with_assets(extension_assets::ReaderAssetSource::new(
        extension_assets.clone(),
    ));
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        bind_keys(cx);
        let safe_mode = std::env::var_os("GPUI_PDF_READER_SAFE_MODE").is_some();
        let mut extensions = app_extensions::ReaderExtensions::new_with_assets(
            theme::bundled_themes(),
            safe_mode,
            extension_assets.clone(),
        )
        .unwrap_or_else(|error| {
            app_extensions::ReaderExtensions::disabled_with_assets(
                safe_mode,
                format!("Reader themes could not start: {error}"),
                extension_assets.clone(),
            )
        });
        rebuild_application_menus(&mut extensions, cx);
        let application_host = cx.new(|_| application_host::ApplicationHost::new(extensions));
        let mut initial_paths = initial_paths.into_iter();
        let window =
            application_host::open_pdf_window(application_host.clone(), initial_paths.next(), cx)
                .expect("failed to open the GPUI PDF Reader window");
        for path in initial_paths {
            application_host::open_pdf_window(application_host.clone(), Some(path), cx)
                .expect("failed to open an additional GPUI PDF Reader window");
        }

        window
            .update(cx, |workspace, window, cx| {
                workspace.focus_content(window, cx);
            })
            .ok();

        // Native QA remains debug-only and is isolated from application startup.
        #[cfg(debug_assertions)]
        qa::install(window, cx);
        let host_for_close = application_host.clone();
        cx.on_window_closed(move |cx| {
            let open = cx.windows();
            application_host::prune_workspace_windows(host_for_close.clone(), &open, cx);
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Rebuild native host-owned slots after extension lifecycle changes. Nested
/// extension submenus are already converted to native GPUI menus by the
/// trusted bridge; packages never receive `App` or platform menu handles.
pub(crate) fn rebuild_application_menus(
    extensions: &mut app_extensions::ReaderExtensions,
    cx: &mut App,
) {
    let theme_menu_items = extensions.native_menu_items("view.appearance");
    let analysis_menu_items = extensions.native_menu_items("tools.analysis");
    #[cfg(feature = "installable-extensions")]
    let extension_entries = extensions.extension_tool_entries();
    let mut view_items = vec![
        MenuItem::action("Classic View", ClassicView),
        MenuItem::action("Fluid View", FluidView),
        MenuItem::separator(),
    ];
    view_items.extend(theme_menu_items);
    view_items.extend([
        MenuItem::separator(),
        MenuItem::action("Zoom In", ZoomIn),
        MenuItem::action("Zoom Out", ZoomOut),
        MenuItem::action("Actual Size", ActualSize),
        MenuItem::action("Fit Width", FitWidth),
        MenuItem::separator(),
        MenuItem::action("Comments", ToggleComments),
    ]);
    let mut file_items = vec![
        MenuItem::action("Open…", OpenDocument),
        MenuItem::action("Settings…", OpenSettings),
    ];
    #[cfg(feature = "installable-extensions")]
    file_items.extend([
        MenuItem::separator(),
        MenuItem::action("Install or Update Extension…", InstallExtension),
        MenuItem::action("Manage Extensions…", ManageExtensions),
    ]);
    #[cfg(not(feature = "installable-extensions"))]
    file_items.extend([
        MenuItem::separator(),
        MenuItem::action("Manage Extensions…", ManageExtensions),
    ]);
    file_items.extend([
        MenuItem::separator(),
        MenuItem::action("Quit GPUI PDF Reader", Quit),
    ]);
    let mut tools_items = analysis_menu_items;
    if !tools_items.is_empty() {
        tools_items.push(MenuItem::separator());
    }
    let extension_tools = {
        let mut items = Vec::new();
        #[cfg(feature = "installable-extensions")]
        for entry in extension_entries {
            if let Some(action) = entry.action {
                items.push(MenuItem::action(entry.name, action));
            } else {
                items.push(MenuItem::action(
                    entry.name,
                    OpenExtensionDetails {
                        extension: entry.extension,
                    },
                ));
            }
        }
        if !items.is_empty() {
            items.push(MenuItem::separator());
        }
        #[cfg(feature = "installable-extensions")]
        items.push(MenuItem::action("Install or Update…", InstallExtension));
        items.push(MenuItem::action("Manage…", ManageExtensions));
        MenuItem::submenu(Menu {
            name: "Extensions".into(),
            items,
        })
    };
    tools_items.push(extension_tools);

    cx.set_menus(vec![
        Menu {
            name: "File".into(),
            items: file_items,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Cut", EditCut),
                MenuItem::action("Copy", EditCopy),
                MenuItem::action("Paste", EditPaste),
                MenuItem::action("Select All", EditSelectAll),
                MenuItem::separator(),
                MenuItem::action("Find…", Find),
                MenuItem::action("Find Next", NextSearchResult),
                MenuItem::action("Find Previous", PreviousSearchResult),
                MenuItem::separator(),
                MenuItem::action("Add Comment", AddComment),
            ],
        },
        Menu {
            name: "View".into(),
            items: view_items,
        },
        Menu {
            name: "Tools".into(),
            items: tools_items,
        },
    ]);
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenDocument, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ActualSize, None),
        KeyBinding::new("cmd-c", EditCopy, None),
        KeyBinding::new("cmd-x", EditCut, None),
        KeyBinding::new("cmd-v", EditPaste, None),
        KeyBinding::new("cmd-a", EditSelectAll, None),
        KeyBinding::new("cmd-f", Find, None),
        KeyBinding::new("cmd-g", NextSearchResult, None),
        KeyBinding::new("cmd-shift-g", PreviousSearchResult, None),
        KeyBinding::new("cmd-alt-m", AddComment, None),
        KeyBinding::new("up", ScrollUp, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("down", ScrollDown, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("left", ScrollLeft, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("right", ScrollRight, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("space", PageDown, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("shift-space", PageUp, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("home", FirstPage, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("end", LastPage, Some(READER_NAVIGATION_CONTEXT)),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("backspace", FieldBackspace, Some("TextField")),
        KeyBinding::new("delete", FieldDelete, Some("TextField")),
        KeyBinding::new("left", FieldLeft, Some("TextField")),
        KeyBinding::new("right", FieldRight, Some("TextField")),
        KeyBinding::new("shift-left", FieldSelectLeft, Some("TextField")),
        KeyBinding::new("shift-right", FieldSelectRight, Some("TextField")),
        KeyBinding::new("home", FieldHome, Some("TextField")),
        KeyBinding::new("end", FieldEnd, Some("TextField")),
        KeyBinding::new("enter", FieldSubmit, Some("TextField")),
        KeyBinding::new("escape", FieldCancel, Some("TextField")),
        KeyBinding::new("backspace", CommentBackspace, Some("MarkdownEditor")),
        KeyBinding::new("delete", CommentDelete, Some("MarkdownEditor")),
        KeyBinding::new("left", CommentLeft, Some("MarkdownEditor")),
        KeyBinding::new("right", CommentRight, Some("MarkdownEditor")),
        KeyBinding::new("up", CommentUp, Some("MarkdownEditor")),
        KeyBinding::new("down", CommentDown, Some("MarkdownEditor")),
        KeyBinding::new("shift-left", CommentSelectLeft, Some("MarkdownEditor")),
        KeyBinding::new("shift-right", CommentSelectRight, Some("MarkdownEditor")),
        KeyBinding::new("shift-up", CommentSelectUp, Some("MarkdownEditor")),
        KeyBinding::new("shift-down", CommentSelectDown, Some("MarkdownEditor")),
        KeyBinding::new("home", CommentHome, Some("MarkdownEditor")),
        KeyBinding::new("end", CommentEnd, Some("MarkdownEditor")),
        KeyBinding::new("enter", CommentNewline, Some("MarkdownEditor")),
        KeyBinding::new("cmd-b", CommentToggleBold, Some("MarkdownEditor")),
        KeyBinding::new("cmd-i", CommentToggleItalic, Some("MarkdownEditor")),
        KeyBinding::new("cmd-e", CommentToggleCode, Some("MarkdownEditor")),
        KeyBinding::new(
            "cmd-shift-8",
            CommentToggleBulletedList,
            Some("MarkdownEditor"),
        ),
        KeyBinding::new(
            "cmd-shift-7",
            CommentToggleNumberedList,
            Some("MarkdownEditor"),
        ),
        KeyBinding::new("cmd-enter", CommentSave, Some("MarkdownEditor")),
        KeyBinding::new("escape", CommentCancel, Some("MarkdownEditor")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::READER_NAVIGATION_CONTEXT;
    use gpui::{AssetSource, KeyBindingContextPredicate, KeyContext};
    use gpui_component::{IconName, IconNamed};

    #[test]
    fn reader_navigation_context_excludes_both_text_editors() {
        let predicate = KeyBindingContextPredicate::parse(READER_NAVIGATION_CONTEXT).unwrap();
        let reader = KeyContext::parse("PdfReader").unwrap();
        let search = KeyContext::parse("TextField").unwrap();
        let comment = KeyContext::parse("MarkdownEditor").unwrap();

        assert_eq!(predicate.depth_of(std::slice::from_ref(&reader)), Some(1));
        assert_eq!(predicate.depth_of(&[reader.clone(), search]), None);
        assert_eq!(predicate.depth_of(&[reader, comment]), None);
    }

    #[test]
    fn every_reader_icon_is_present_in_the_component_asset_bundle() {
        let assets = gpui_component_assets::Assets;
        for icon in [
            IconName::ALargeSmall,
            IconName::ArrowDown,
            IconName::ArrowRight,
            IconName::ArrowUp,
            IconName::Asterisk,
            IconName::BookOpen,
            IconName::Calendar,
            IconName::CaseSensitive,
            IconName::Check,
            IconName::ChevronLeft,
            IconName::ChevronDown,
            IconName::ChevronRight,
            IconName::ChevronUp,
            IconName::CircleCheck,
            IconName::CircleUser,
            IconName::CircleX,
            IconName::Close,
            IconName::Copy,
            IconName::ExternalLink,
            IconName::EyeOff,
            IconName::File,
            IconName::Globe,
            IconName::Info,
            IconName::LoaderCircle,
            IconName::Menu,
            IconName::Minus,
            IconName::Moon,
            IconName::PanelRight,
            IconName::Plus,
            IconName::Search,
            IconName::SquareTerminal,
            IconName::Sun,
        ] {
            let path = icon.path();
            assert!(
                assets.load(path.as_ref()).unwrap().is_some(),
                "missing gpui-component icon asset: {path:?}"
            );
        }
    }
}
