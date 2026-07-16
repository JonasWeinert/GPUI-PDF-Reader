mod annotations;
mod backend;
mod comment_editor;
mod model;
mod native_gestures;
mod reader;
mod search;
mod text_field;
mod theme;

use comment_editor::{
    CommentBackspace, CommentCancel, CommentDelete, CommentDown, CommentEnd, CommentHome,
    CommentLeft, CommentRight, CommentSave, CommentSelectDown, CommentSelectLeft,
    CommentSelectRight, CommentSelectUp, CommentToggleBold, CommentToggleBulletedList,
    CommentToggleCode, CommentToggleItalic, CommentToggleNumberedList, CommentUp,
};
#[cfg(debug_assertions)]
use gpui::Keystroke;
#[cfg(target_os = "macos")]
use gpui::TitlebarOptions;
use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, Point, SharedString, WindowBounds,
    WindowOptions, actions, px, size,
};
use std::path::PathBuf;
#[cfg(debug_assertions)]
use std::time::Duration;
use text_field::{
    FieldBackspace, FieldCancel, FieldDelete, FieldEnd, FieldHome, FieldLeft, FieldRight,
    FieldSelectLeft, FieldSelectRight, FieldSubmit,
};

const READER_NAVIGATION_CONTEXT: &str = "PdfReader && !TextField && !CommentEditor";

actions!(
    gpui_pdf_reader,
    [
        OpenDocument,
        ZoomIn,
        ZoomOut,
        ActualSize,
        FitWidth,
        CopySelection,
        SelectAll,
        EditCopy,
        EditCut,
        EditPaste,
        EditSelectAll,
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
        Quit,
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = gpui_pdf_reader, no_json)]
pub struct SelectTheme {
    pub name: SharedString,
}

fn main() {
    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);

    let application = Application::new().with_assets(gpui_component_assets::Assets);
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        bind_keys(cx);
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open…", OpenDocument),
                    MenuItem::separator(),
                    MenuItem::action("Quit GPUI PDF Reader", Quit),
                ],
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
                items: vec![
                    MenuItem::action("Classic View", ClassicView),
                    MenuItem::action("Fluid View", FluidView),
                    MenuItem::separator(),
                    theme::theme_menu(),
                    MenuItem::separator(),
                    MenuItem::action("Zoom In", ZoomIn),
                    MenuItem::action("Zoom Out", ZoomOut),
                    MenuItem::action("Actual Size", ActualSize),
                    MenuItem::action("Fit Width", FitWidth),
                    MenuItem::separator(),
                    MenuItem::action("Comments", ToggleComments),
                ],
            },
        ]);
        let bounds = Bounds {
            origin: Point::new(px(120.0), px(80.0)),
            size: size(px(1180.0), px(820.0)),
        };
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // The side panel needs room for both its editor/list and a usable
            // document viewport. Prevent a focused editor from becoming
            // invisible in an impossibly narrow window.
            window_min_size: Some(size(px(700.0), px(480.0))),
            focus: true,
            ..Default::default()
        };
        #[cfg(target_os = "macos")]
        let window_options = WindowOptions {
            // GPUI owns the chrome so the toolbar and macOS traffic lights read
            // as one continuous titlebar. Other platforms retain their native
            // decorations until their platform-specific chrome is developed.
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(Point::new(px(14.0), px(18.0))),
            }),
            ..window_options
        };
        let window = cx
            .open_window(
                window_options,
                move |window, cx| reader::PdfReader::new(initial_path.clone(), window, cx),
            )
            .expect("failed to open the GPUI PDF Reader window");

        window
            .update(cx, |reader, window, cx| {
                window.focus(&reader.focus_handle(cx));
            })
            .ok();

        // A debug-only, opt-in hook lets native QA exercise real GPUI
        // keystrokes and the production command-wheel handler without macOS
        // Accessibility permission.
        #[cfg(debug_assertions)]
        {
            if std::env::var_os("GPUI_PDF_READER_QA_FLUID_VIEW").is_some() {
                window
                    .update(cx, |reader, window, cx| {
                        reader.qa_use_fluid_view(window, cx);
                    })
                    .ok();
            }
            if let Ok(theme_name) = std::env::var("GPUI_PDF_READER_QA_THEME") {
                window
                    .update(cx, |reader, window, cx| {
                        assert!(
                            reader.qa_select_theme(&theme_name, window, cx),
                            "unknown GPUI_PDF_READER_QA_THEME: {theme_name}"
                        );
                    })
                    .ok();
            }
            if let Ok(pdf_dark) = std::env::var("GPUI_PDF_READER_QA_PDF_DARK") {
                let enabled = match pdf_dark.as_str() {
                    "on" | "1" | "true" => true,
                    "off" | "0" | "false" => false,
                    _ => panic!("invalid GPUI_PDF_READER_QA_PDF_DARK: {pdf_dark}"),
                };
                window
                    .update(cx, |reader, window, cx| {
                        reader.qa_set_pdf_dark_mode(enabled, window, cx);
                    })
                    .ok();
            }
            let keystrokes: Vec<_> = std::env::var("GPUI_PDF_READER_QA_KEYS")
                .unwrap_or_default()
                .split_whitespace()
                .map(|key| {
                    Keystroke::parse(key)
                        .unwrap_or_else(|_| panic!("invalid GPUI_PDF_READER_QA_KEYS entry: {key}"))
                })
                .collect();
            let wheel_deltas: Vec<_> = std::env::var("GPUI_PDF_READER_QA_WHEEL_DELTAS")
                .unwrap_or_default()
                .split_whitespace()
                .map(|delta| {
                    delta.parse::<f32>().unwrap_or_else(|_| {
                        panic!("invalid GPUI_PDF_READER_QA_WHEEL_DELTAS entry: {delta}")
                    })
                })
                .collect();
            let interval = std::env::var("GPUI_PDF_READER_QA_INTERVAL_MS")
                .ok()
                .map(|value| {
                    value.parse::<u64>().unwrap_or_else(|_| {
                        panic!("invalid GPUI_PDF_READER_QA_INTERVAL_MS: {value}")
                    })
                })
                .unwrap_or(180);
            let timeout = std::env::var("GPUI_PDF_READER_QA_TIMEOUT_MS")
                .ok()
                .map(|value| {
                    value.parse::<u64>().unwrap_or_else(|_| {
                        panic!("invalid GPUI_PDF_READER_QA_TIMEOUT_MS: {value}")
                    })
                })
                .unwrap_or(20_000);
            let report = std::env::var_os("GPUI_PDF_READER_QA_REPORT").is_some();
            let exit_after_report = std::env::var_os("GPUI_PDF_READER_QA_EXIT").is_some();
            let feature_scenario =
                std::env::var_os("GPUI_PDF_READER_QA_FEATURE_SCENARIO").is_some();
            let fluid_scenario =
                std::env::var_os("GPUI_PDF_READER_QA_FLUID_SCENARIO").is_some();
            let toc_hover = std::env::var("GPUI_PDF_READER_QA_TOC_HOVER")
                .ok()
                .map(|value| {
                    value.parse::<usize>().unwrap_or_else(|_| {
                        panic!("invalid GPUI_PDF_READER_QA_TOC_HOVER: {value}")
                    })
                });
            let toc_navigate = std::env::var("GPUI_PDF_READER_QA_TOC_NAVIGATE")
                .ok()
                .map(|value| {
                    value.parse::<usize>().unwrap_or_else(|_| {
                        panic!("invalid GPUI_PDF_READER_QA_TOC_NAVIGATE: {value}")
                    })
                });
            let toc_callout_hold =
                std::env::var_os("GPUI_PDF_READER_QA_TOC_CALLOUT_HOLD").is_some();
            if !keystrokes.is_empty()
                || !wheel_deltas.is_empty()
                || report
                || exit_after_report
                || feature_scenario
                || fluid_scenario
                || toc_hover.is_some()
                || toc_navigate.is_some()
                || toc_callout_hold
            {
                window
                    .update(cx, |_, window, cx| {
                        window
                            .spawn(cx, async move |cx| {
                                let startup_deadline =
                                    std::time::Instant::now() + Duration::from_millis(timeout);
                                loop {
                                    let ready = cx
                                        .update(|window, cx| {
                                            window
                                                .root::<reader::PdfReader>()
                                                .flatten()
                                                .is_some_and(|reader| {
                                                    reader.read(cx).qa_viewport_is_settled()
                                                })
                                        })
                                        .unwrap_or(false);
                                    if ready {
                                        break;
                                    }
                                    if std::time::Instant::now() >= startup_deadline {
                                        eprintln!(
                                            "GPUI_PDF_READER_QA_ERROR startup did not settle"
                                        );
                                        let _ = cx.update(|_, cx| cx.quit());
                                        return;
                                    }
                                    cx.background_executor()
                                        .timer(Duration::from_millis(25))
                                        .await;
                                }
                                if toc_hover.is_some() || toc_navigate.is_some() || toc_callout_hold {
                                    let outcome = cx
                                        .update(|window, cx| {
                                            let Some(Some(reader)) =
                                                window.root::<reader::PdfReader>()
                                            else {
                                                return Err("TOC QA reader is unavailable".to_owned());
                                            };
                                            reader.update(cx, |reader, cx| {
                                                if let Some(index) = toc_hover {
                                                    reader.qa_set_toc_hovered(index, window, cx)?;
                                                }
                                                if toc_callout_hold {
                                                    reader.qa_hold_toc_callout(window, cx)?;
                                                }
                                                if let Some(index) = toc_navigate {
                                                    reader.qa_navigate_toc(index, window, cx)?;
                                                }
                                                Ok(())
                                            })
                                        })
                                        .unwrap_or_else(|error| {
                                            Err(format!("TOC QA window update failed: {error}"))
                                        });
                                    if let Err(message) = outcome {
                                        eprintln!("GPUI_PDF_READER_QA_ERROR {message}");
                                        let _ = cx.update(|_, cx| cx.quit());
                                        return;
                                    }
                                }
                                if feature_scenario || fluid_scenario {
                                    let feature_deadline =
                                        std::time::Instant::now() + Duration::from_millis(timeout);
                                    loop {
                                        let outcome = cx
                                            .update(|window, cx| {
                                                let Some(Some(reader)) =
                                                    window.root::<reader::PdfReader>()
                                                else {
                                                    return Ok(false);
                                                };
                                                reader.update(cx, |reader, cx| {
                                                    if fluid_scenario {
                                                        reader.qa_drive_fluid_scenario(window, cx)
                                                    } else {
                                                        reader.qa_drive_feature_scenario(window, cx)
                                                    }
                                                })
                                            })
                                            .unwrap_or_else(|error| {
                                                Err(format!(
                                                    "feature scenario window update failed: {error}"
                                                ))
                                            });
                                        match outcome {
                                            Ok(true) => break,
                                            Ok(false) => {}
                                            Err(message) => {
                                                eprintln!(
                                                    "GPUI_PDF_READER_QA_ERROR {message}"
                                                );
                                                let _ = cx.update(|_, cx| cx.quit());
                                                return;
                                            }
                                        }
                                        if std::time::Instant::now() >= feature_deadline {
                                            eprintln!(
                                                    "GPUI_PDF_READER_QA_ERROR {} scenario did not settle",
                                                    if fluid_scenario { "Fluid" } else { "feature" }
                                                );
                                            let _ = cx.update(|_, cx| cx.quit());
                                            return;
                                        }
                                        cx.background_executor()
                                            .timer(Duration::from_millis(25))
                                            .await;
                                    }
                                }
                                for keystroke in keystrokes {
                                    let _ = cx.update(|window, cx| {
                                        window.dispatch_keystroke(keystroke, cx);
                                    });
                                    cx.background_executor()
                                        .timer(Duration::from_millis(interval))
                                        .await;
                                }
                                for delta_y in wheel_deltas {
                                    let _ = cx.update(|window, cx| {
                                        let viewport = window.viewport_size();
                                        let position =
                                            Point::new(viewport.width / 2.0, viewport.height / 2.0);
                                        if let Some(Some(reader)) =
                                            window.root::<reader::PdfReader>()
                                        {
                                            reader.update(cx, |reader, cx| {
                                                reader.qa_command_wheel(
                                                    delta_y, position, window, cx,
                                                );
                                            });
                                        }
                                    });
                                    cx.background_executor()
                                        .timer(Duration::from_millis(interval))
                                        .await;
                                }
                                if report || exit_after_report {
                                    let settle_deadline =
                                        std::time::Instant::now() + Duration::from_millis(timeout);
                                    let mut stable_since = None;
                                    loop {
                                        let settled = cx
                                            .update(|window, cx| {
                                                window
                                                    .root::<reader::PdfReader>()
                                                    .flatten()
                                                    .is_some_and(|reader| {
                                                        reader.read(cx).qa_viewport_is_settled()
                                                    })
                                            })
                                            .unwrap_or(false);
                                        let now = std::time::Instant::now();
                                        if settled {
                                            let since = stable_since.get_or_insert(now);
                                            if now.duration_since(*since)
                                                >= Duration::from_millis(250)
                                            {
                                                break;
                                            }
                                        } else {
                                            stable_since = None;
                                        }
                                        if now >= settle_deadline {
                                            eprintln!(
                                                "GPUI_PDF_READER_QA_ERROR viewport did not settle"
                                            );
                                            let _ = cx.update(|_, cx| cx.quit());
                                            return;
                                        }
                                        cx.background_executor()
                                            .timer(Duration::from_millis(25))
                                            .await;
                                    }
                                    let _ = cx.update(|window, cx| {
                                        if report
                                            && let Some(Some(reader)) =
                                                window.root::<reader::PdfReader>()
                                        {
                                            eprintln!("{}", reader.read(cx).qa_report());
                                        }
                                        if exit_after_report {
                                            cx.quit();
                                        }
                                    });
                                }
                            })
                            .detach();
                    })
                    .ok();
            }
        }
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenDocument, None),
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
        KeyBinding::new("backspace", CommentBackspace, Some("CommentEditor")),
        KeyBinding::new("delete", CommentDelete, Some("CommentEditor")),
        KeyBinding::new("left", CommentLeft, Some("CommentEditor")),
        KeyBinding::new("right", CommentRight, Some("CommentEditor")),
        KeyBinding::new("up", CommentUp, Some("CommentEditor")),
        KeyBinding::new("down", CommentDown, Some("CommentEditor")),
        KeyBinding::new("shift-left", CommentSelectLeft, Some("CommentEditor")),
        KeyBinding::new("shift-right", CommentSelectRight, Some("CommentEditor")),
        KeyBinding::new("shift-up", CommentSelectUp, Some("CommentEditor")),
        KeyBinding::new("shift-down", CommentSelectDown, Some("CommentEditor")),
        KeyBinding::new("home", CommentHome, Some("CommentEditor")),
        KeyBinding::new("end", CommentEnd, Some("CommentEditor")),
        KeyBinding::new("cmd-b", CommentToggleBold, Some("CommentEditor")),
        KeyBinding::new("cmd-i", CommentToggleItalic, Some("CommentEditor")),
        KeyBinding::new("cmd-e", CommentToggleCode, Some("CommentEditor")),
        KeyBinding::new(
            "cmd-shift-8",
            CommentToggleBulletedList,
            Some("CommentEditor"),
        ),
        KeyBinding::new(
            "cmd-shift-7",
            CommentToggleNumberedList,
            Some("CommentEditor"),
        ),
        KeyBinding::new("cmd-enter", CommentSave, Some("CommentEditor")),
        KeyBinding::new("escape", CommentCancel, Some("CommentEditor")),
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
        let comment = KeyContext::parse("CommentEditor").unwrap();

        assert_eq!(predicate.depth_of(std::slice::from_ref(&reader)), Some(1));
        assert_eq!(predicate.depth_of(&[reader.clone(), search]), None);
        assert_eq!(predicate.depth_of(&[reader, comment]), None);
    }

    #[test]
    fn every_reader_icon_is_present_in_the_component_asset_bundle() {
        let assets = gpui_component_assets::Assets;
        for icon in [
            IconName::ArrowDown,
            IconName::ArrowRight,
            IconName::ArrowUp,
            IconName::BookOpen,
            IconName::ChevronLeft,
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
