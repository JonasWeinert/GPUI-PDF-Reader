mod backend;
mod model;
mod native_gestures;
mod reader;

#[cfg(debug_assertions)]
use gpui::Keystroke;
use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, Point, WindowBounds, WindowOptions,
    actions, px, size,
};
use std::path::PathBuf;
#[cfg(debug_assertions)]
use std::time::Duration;

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
        ScrollUp,
        ScrollDown,
        ScrollLeft,
        ScrollRight,
        PageUp,
        PageDown,
        FirstPage,
        LastPage,
        Quit,
    ]
);

fn main() {
    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
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
                    MenuItem::action("Copy", CopySelection),
                    MenuItem::action("Select All", SelectAll),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Zoom In", ZoomIn),
                    MenuItem::action("Zoom Out", ZoomOut),
                    MenuItem::action("Actual Size", ActualSize),
                    MenuItem::action("Fit Width", FitWidth),
                ],
            },
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let bounds = Bounds {
            origin: Point::new(px(120.0), px(80.0)),
            size: size(px(1180.0), px(820.0)),
        };
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: true,
                    ..Default::default()
                },
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
            if !keystrokes.is_empty() || !wheel_deltas.is_empty() || report || exit_after_report {
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
        KeyBinding::new("cmd-c", CopySelection, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("up", ScrollUp, None),
        KeyBinding::new("down", ScrollDown, None),
        KeyBinding::new("left", ScrollLeft, None),
        KeyBinding::new("right", ScrollRight, None),
        KeyBinding::new("pageup", PageUp, None),
        KeyBinding::new("pagedown", PageDown, None),
        KeyBinding::new("space", PageDown, None),
        KeyBinding::new("shift-space", PageUp, None),
        KeyBinding::new("home", FirstPage, None),
        KeyBinding::new("end", LastPage, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
}
