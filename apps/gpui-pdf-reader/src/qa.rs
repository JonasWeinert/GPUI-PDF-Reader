//! Debug-only native QA orchestration.
//!
//! Production startup only installs this module when debug assertions are
//! enabled. The environment protocol stays app-specific while `PdfReader`
//! exposes the narrow instrumentation hooks used by the native E2E scripts.

use crate::application_host::{ApplicationHost, open_pdf_window};
use crate::reader::PdfReader;
use crate::reader::qa::QaReaderResourceSnapshot;
use crate::workspace_window::WorkspaceWindow;
use gpui::{App, AsyncWindowContext, Entity, Keystroke, Point, Window, WindowHandle};
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_MS: u64 = 180;
const DEFAULT_TIMEOUT_MS: u64 = 20_000;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const STABLE_VIEWPORT_DURATION: Duration = Duration::from_millis(250);
const DEFAULT_RESOURCE_SAMPLE_MS: u64 = 100;

struct QaConfig {
    keystrokes: Vec<Keystroke>,
    wheel_deltas: Vec<f32>,
    interval: Duration,
    timeout: Duration,
    report: bool,
    exit_after_report: bool,
    feature_scenario: bool,
    fluid_scenario: bool,
    toc_hover: Option<usize>,
    toc_navigate: Option<usize>,
    link_navigate: Option<usize>,
    link_hover: Option<usize>,
    internal_link_hover: Option<usize>,
    scientific_reference_hover: Option<usize>,
    toc_callout_hold: bool,
    reference_details: bool,
    extension_scenario: Option<String>,
    expected_window_count: Option<usize>,
    expected_tab_count: Option<usize>,
    tab_drag_scenario: Option<String>,
    split_view_scenario: bool,
    split_view_hold: bool,
    tab_hover_hold: bool,
    control_bar_scenario: bool,
    multi_annotation_scenario: bool,
    resource_trace: bool,
    resource_sample_interval: Duration,
    resource_stress: bool,
    progressive_open: bool,
    resource_chaos: bool,
    chaos_rounds: usize,
    chaos_actions_per_window: usize,
    chaos_seed: u64,
    render_dwell: Duration,
}

impl QaConfig {
    fn from_environment() -> Self {
        let keystrokes = std::env::var("GPUI_PDF_READER_QA_KEYS")
            .unwrap_or_default()
            .split_whitespace()
            .map(|key| {
                Keystroke::parse(key)
                    .unwrap_or_else(|_| panic!("invalid GPUI_PDF_READER_QA_KEYS entry: {key}"))
            })
            .collect();
        let wheel_deltas = std::env::var("GPUI_PDF_READER_QA_WHEEL_DELTAS")
            .unwrap_or_default()
            .split_whitespace()
            .map(|value| parse_value("GPUI_PDF_READER_QA_WHEEL_DELTAS entry", value))
            .collect();

        Self {
            keystrokes,
            wheel_deltas,
            interval: Duration::from_millis(
                optional_value("GPUI_PDF_READER_QA_INTERVAL_MS").unwrap_or(DEFAULT_INTERVAL_MS),
            ),
            timeout: Duration::from_millis(
                optional_value("GPUI_PDF_READER_QA_TIMEOUT_MS").unwrap_or(DEFAULT_TIMEOUT_MS),
            ),
            report: flag("GPUI_PDF_READER_QA_REPORT"),
            exit_after_report: flag("GPUI_PDF_READER_QA_EXIT"),
            feature_scenario: flag("GPUI_PDF_READER_QA_FEATURE_SCENARIO"),
            fluid_scenario: flag("GPUI_PDF_READER_QA_FLUID_SCENARIO"),
            toc_hover: optional_value("GPUI_PDF_READER_QA_TOC_HOVER"),
            toc_navigate: optional_value("GPUI_PDF_READER_QA_TOC_NAVIGATE"),
            link_navigate: optional_value("GPUI_PDF_READER_QA_LINK_NAVIGATE"),
            link_hover: optional_value("GPUI_PDF_READER_QA_LINK_HOVER"),
            internal_link_hover: optional_value("GPUI_PDF_READER_QA_INTERNAL_LINK_HOVER"),
            scientific_reference_hover: optional_value(
                "GPUI_PDF_READER_QA_SCIENTIFIC_REFERENCE_HOVER",
            ),
            toc_callout_hold: flag("GPUI_PDF_READER_QA_TOC_CALLOUT_HOLD"),
            reference_details: flag("GPUI_PDF_READER_QA_REFERENCE_DETAILS"),
            extension_scenario: std::env::var("GPUI_PDF_READER_QA_EXTENSION_SCENARIO").ok(),
            expected_window_count: optional_value("GPUI_PDF_READER_QA_WINDOW_COUNT"),
            expected_tab_count: optional_value("GPUI_PDF_READER_QA_TAB_COUNT"),
            tab_drag_scenario: std::env::var("GPUI_PDF_READER_QA_TAB_DRAG_SCENARIO").ok(),
            split_view_scenario: flag("GPUI_PDF_READER_QA_SPLIT_VIEW_SCENARIO"),
            split_view_hold: flag("GPUI_PDF_READER_QA_SPLIT_VIEW_HOLD"),
            tab_hover_hold: flag("GPUI_PDF_READER_QA_TAB_HOVER_HOLD"),
            control_bar_scenario: flag("GPUI_PDF_READER_QA_CONTROL_BAR_SCENARIO"),
            multi_annotation_scenario: flag("GPUI_PDF_READER_QA_MULTI_ANNOTATION_SCENARIO"),
            resource_trace: flag("GPUI_PDF_READER_QA_RESOURCE_TRACE"),
            resource_sample_interval: Duration::from_millis(
                optional_value::<u64>("GPUI_PDF_READER_QA_RESOURCE_SAMPLE_MS")
                    .unwrap_or(DEFAULT_RESOURCE_SAMPLE_MS)
                    .clamp(20, 5_000),
            ),
            resource_stress: flag("GPUI_PDF_READER_QA_RESOURCE_STRESS"),
            progressive_open: flag("GPUI_PDF_READER_QA_PROGRESSIVE_OPEN"),
            resource_chaos: flag("GPUI_PDF_READER_QA_RESOURCE_CHAOS"),
            chaos_rounds: optional_value("GPUI_PDF_READER_QA_CHAOS_ROUNDS").unwrap_or(2),
            chaos_actions_per_window: optional_value("GPUI_PDF_READER_QA_CHAOS_ACTIONS_PER_WINDOW")
                .unwrap_or(4),
            chaos_seed: optional_value("GPUI_PDF_READER_QA_CHAOS_SEED")
                .unwrap_or(0x6b65_792d_7064_6621),
            render_dwell: Duration::from_millis(
                optional_value("GPUI_PDF_READER_QA_RENDER_DWELL_MS").unwrap_or(320),
            ),
        }
    }

    fn requested(&self) -> bool {
        !self.keystrokes.is_empty()
            || !self.wheel_deltas.is_empty()
            || self.report
            || self.exit_after_report
            || self.feature_scenario
            || self.fluid_scenario
            || self.toc_hover.is_some()
            || self.toc_navigate.is_some()
            || self.link_navigate.is_some()
            || self.link_hover.is_some()
            || self.internal_link_hover.is_some()
            || self.scientific_reference_hover.is_some()
            || self.toc_callout_hold
            || self.reference_details
            || self.extension_scenario.is_some()
            || self.expected_window_count.is_some()
            || self.expected_tab_count.is_some()
            || self.tab_drag_scenario.is_some()
            || self.split_view_scenario
            || self.split_view_hold
            || self.tab_hover_hold
            || self.control_bar_scenario
            || self.multi_annotation_scenario
            || self.resource_trace
            || self.resource_stress
            || self.progressive_open
            || self.resource_chaos
    }

    fn has_navigation_setup(&self) -> bool {
        self.toc_hover.is_some()
            || self.toc_navigate.is_some()
            || self.link_navigate.is_some()
            || self.link_hover.is_some()
            || self.internal_link_hover.is_some()
            || self.scientific_reference_hover.is_some()
            || self.toc_callout_hold
    }
}

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn optional_value<T>(name: &str) -> Option<T>
where
    T: FromStr,
    T::Err: Display,
{
    std::env::var(name)
        .ok()
        .map(|value| parse_value(name, &value))
}

fn parse_value<T>(name: &str, value: &str) -> T
where
    T: FromStr,
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|_| panic!("invalid {name}: {value}"))
}

/// Installs the opt-in native QA driver for a reader window.
pub fn install(
    window: WindowHandle<WorkspaceWindow>,
    application_host: Entity<ApplicationHost>,
    deferred_paths: Vec<PathBuf>,
    cx: &mut App,
) {
    apply_initial_overrides(window, cx);
    let config = QaConfig::from_environment();
    if !config.requested() {
        return;
    }

    let resource_trace = config.resource_trace.then(QaResourceTrace::new);
    if let Some(trace) = resource_trace.clone() {
        install_resource_sampler(window, trace, config.resource_sample_interval, cx);
    }

    window
        .update(cx, |_, window, cx| {
            window
                .spawn(cx, async move |cx| {
                    trace_marker(&resource_trace, "startup", "begin", cx);
                    let startup_deadline = Instant::now() + config.timeout;
                    loop {
                        let ready = cx
                            .update(|window, cx| {
                                reader_entity(window, cx).is_some_and(|reader| {
                                    reader.read(cx).qa_viewport_is_settled()
                                })
                            })
                            .unwrap_or(false);
                        if ready {
                            break;
                        }
                        if Instant::now() >= startup_deadline {
                            fail_and_quit(cx, "startup did not settle");
                            return;
                        }
                        cx.background_executor().timer(POLL_INTERVAL).await;
                    }
                    trace_marker(&resource_trace, "startup", "settled", cx);

                    if config.progressive_open {
                        trace_marker(&resource_trace, "progressive-open-1", "settled", cx);
                        for (index, path) in deferred_paths.into_iter().enumerate() {
                            let count = index + 2;
                            let operation = format!("progressive-open-{count}");
                            trace_marker(&resource_trace, &operation, "before", cx);
                            let opened = cx.update(|_, cx| {
                                open_pdf_window(application_host.clone(), Some(path), cx)
                            });
                            let handle = match opened {
                                Ok(Ok(handle)) => handle,
                                Ok(Err(error)) => {
                                    fail_and_quit(
                                        cx,
                                        &format!("progressive PDF open failed: {error}"),
                                    );
                                    return;
                                }
                                Err(error) => {
                                    fail_and_quit(
                                        cx,
                                        &format!("progressive window update failed: {error}"),
                                    );
                                    return;
                                }
                            };
                            if let Err(error) = wait_for_reader_settled(
                                handle,
                                config.timeout,
                                config.render_dwell,
                                cx,
                            )
                            .await
                            {
                                fail_and_quit(cx, &error);
                                return;
                            }
                            trace_marker(&resource_trace, &operation, "settled", cx);
                        }
                    }

                    if let Some(expected) = config.expected_window_count {
                        let deadline = Instant::now() + config.timeout;
                        loop {
                            let (windows, pdf_views, settled) = cx
                                .update(|window, cx| workspace_window_counts(window, cx))
                                .unwrap_or_default();
                            if windows == expected
                                && pdf_views == expected
                                && settled == expected
                            {
                                eprintln!(
                                    "GPUI_PDF_READER_QA_WINDOWS windows={windows} pdf_views={pdf_views} settled={settled}"
                                );
                                break;
                            }
                            if windows > expected {
                                fail_and_quit(
                                    cx,
                                    &format!(
                                        "expected {expected} workspace windows, found {windows}"
                                    ),
                                );
                                return;
                            }
                            if Instant::now() >= deadline {
                                fail_and_quit(
                                    cx,
                                    &format!(
                                        "multi-window startup did not settle: expected={expected} windows={windows} pdf_views={pdf_views} settled={settled}"
                                    ),
                                );
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }

                    if let Some(expected) = config.expected_tab_count {
                        let actual = cx
                            .update(|window, cx| {
                                window
                                    .root::<WorkspaceWindow>()
                                    .flatten()
                                    .map_or(0, |workspace| workspace.read(cx).qa_tab_count())
                            })
                            .unwrap_or(0);
                        if actual != expected {
                            fail_and_quit(
                                cx,
                                &format!("expected {expected} workspace tabs, found {actual}"),
                            );
                            return;
                        }
                        let mut settled = 0;
                        for index in 0..expected {
                            let activated = cx
                                .update(|window, cx| {
                                    let Some(workspace) = window
                                        .root::<WorkspaceWindow>()
                                        .flatten()
                                    else {
                                        return false;
                                    };
                                    workspace.update(cx, |workspace, cx| {
                                        workspace.qa_activate_tab(index, window, cx)
                                    });
                                    true
                                })
                                .unwrap_or(false);
                            if !activated {
                                fail_and_quit(cx, "tab QA could not access the workspace root");
                                return;
                            }
                            let deadline = Instant::now() + config.timeout;
                            loop {
                                let ready = cx
                                    .update(|window, cx| {
                                        reader_entity(window, cx).is_some_and(|reader| {
                                            reader.read(cx).qa_viewport_is_settled()
                                        })
                                    })
                                    .unwrap_or(false);
                                if ready {
                                    settled += 1;
                                    break;
                                }
                                if Instant::now() >= deadline {
                                    fail_and_quit(
                                        cx,
                                        &format!("tab {index} did not settle after activation"),
                                    );
                                    return;
                                }
                                cx.background_executor().timer(POLL_INTERVAL).await;
                            }
                        }
                        let windows = cx.update(|_, cx| cx.windows().len()).unwrap_or(0);
                        eprintln!(
                            "GPUI_PDF_READER_QA_TABS tabs={actual} switched={expected} settled={settled} windows={windows}"
                        );
                    }

                    if let Some(scenario) = config.tab_drag_scenario.as_deref() {
                        match scenario {
                            "reorder" => {
                                let outcome = cx.update(|window, cx| {
                                    let workspace = window
                                        .root::<WorkspaceWindow>()
                                        .flatten()
                                        .ok_or_else(|| "tab reorder root is unavailable".to_owned())?;
                                    let before = workspace.read(cx).qa_tab_view_ids();
                                    if before.len() < 3 {
                                        return Err("tab reorder requires at least three tabs".to_owned());
                                    }
                                    workspace.update(cx, |workspace, cx| {
                                        workspace.qa_reorder_tab(0, before.len(), cx)
                                    });
                                    let after = workspace.read(cx).qa_tab_view_ids();
                                    let mut expected = before[1..].to_vec();
                                    expected.push(before[0]);
                                    if after != expected {
                                        return Err(format!(
                                            "tab reorder mismatch: before={before:?} after={after:?}"
                                        ));
                                    }
                                    Ok(after.len())
                                });
                                match outcome {
                                    Ok(Ok(tabs)) => eprintln!(
                                        "GPUI_PDF_READER_QA_TAB_DRAG scenario=reorder tabs={tabs}"
                                    ),
                                    Ok(Err(message)) => {
                                        fail_and_quit(cx, &message);
                                        return;
                                    }
                                    Err(error) => {
                                        fail_and_quit(cx, &format!("tab reorder failed: {error}"));
                                        return;
                                    }
                                }
                            }
                            "transfer" => {
                                let current_window = cx
                                    .update(|window, cx| {
                                        window
                                            .root::<WorkspaceWindow>()
                                            .flatten()
                                            .map(|workspace| workspace.read(cx).qa_window_and_view().0)
                                    })
                                    .ok()
                                    .flatten();
                                let handles = cx
                                    .update(|_, cx| {
                                        cx.windows()
                                            .into_iter()
                                            .filter_map(|handle| {
                                                handle.downcast::<WorkspaceWindow>()
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                if handles.len() != 2 {
                                    fail_and_quit(
                                        cx,
                                        &format!(
                                            "tab transfer requires two windows, found {}",
                                            handles.len()
                                        ),
                                    );
                                    return;
                                }
                                let Some(current_window) = current_window else {
                                    fail_and_quit(cx, "tab transfer current window is unavailable");
                                    return;
                                };
                                let target = handles.iter().copied().find(|handle| {
                                    handle
                                        .update(cx, |workspace, _, _| workspace.qa_window_and_view().0)
                                        .is_ok_and(|window| window == current_window)
                                });
                                let source = handles.iter().copied().find(|handle| {
                                    handle
                                        .update(cx, |workspace, _, _| workspace.qa_window_and_view().0)
                                        .is_ok_and(|window| window != current_window)
                                });
                                let (Some(source), Some(target)) = (source, target) else {
                                    fail_and_quit(cx, "tab transfer could not distinguish source and target windows");
                                    return;
                                };
                                let source_identity = source.update(cx, |workspace, _, _| {
                                    workspace.qa_window_and_view()
                                });
                                let Ok((source_window, source_view)) = source_identity else {
                                    fail_and_quit(cx, "tab transfer source disappeared");
                                    return;
                                };
                                if target
                                    .update(cx, |workspace, window, cx| {
                                        let insertion = workspace.qa_tab_count();
                                        workspace.qa_transfer_tab_from(
                                            source_window,
                                            source_view,
                                            insertion,
                                            window,
                                            cx,
                                        );
                                    })
                                    .is_err()
                                {
                                    fail_and_quit(cx, "tab transfer target disappeared");
                                    return;
                                }
                                let deadline = Instant::now() + config.timeout;
                                loop {
                                    let state = target.update(cx, |workspace, _, cx| {
                                        let tabs = workspace.qa_tab_count();
                                        let settled = workspace
                                            .reader()
                                            .is_some_and(|reader| {
                                                reader.read(cx).qa_viewport_is_settled()
                                            });
                                        (tabs, settled)
                                    });
                                    let windows = cx.update(|_, cx| cx.windows().len()).unwrap_or(0);
                                    if matches!(state, Ok((2, true))) && windows == 1 {
                                        eprintln!(
                                            "GPUI_PDF_READER_QA_TAB_DRAG scenario=transfer tabs=2 windows=1 settled=1"
                                        );
                                        break;
                                    }
                                    if Instant::now() >= deadline {
                                        fail_and_quit(
                                            cx,
                                            &format!(
                                                "tab transfer did not settle: state={state:?} windows={windows}"
                                            ),
                                        );
                                        return;
                                    }
                                    cx.background_executor().timer(POLL_INTERVAL).await;
                                }
                            }
                            other => {
                                fail_and_quit(cx, &format!("unknown tab drag scenario: {other}"));
                                return;
                            }
                        }
                    }

                    if config.split_view_hold {
                        let held = cx.update(|window, cx| {
                            let Some(workspace) = window.root::<WorkspaceWindow>().flatten() else {
                                return false;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.qa_split_with_other_tab(window, cx);
                            });
                            true
                        });
                        if !matches!(held, Ok(true)) {
                            fail_and_quit(cx, "split-view hold could not access the workspace window");
                            return;
                        }
                        eprintln!("GPUI_PDF_READER_QA_SPLIT_HOLD ready=1");
                    }

                    if config.tab_hover_hold {
                        let held = cx.update(|window, cx| {
                            let Some(workspace) = window.root::<WorkspaceWindow>().flatten() else {
                                return false;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.qa_hold_inactive_tab_hover(cx)
                            })
                        });
                        if !matches!(held, Ok(true)) {
                            fail_and_quit(cx, "tab-hover hold requires an inactive tab");
                            return;
                        }
                        eprintln!("GPUI_PDF_READER_QA_TAB_HOVER_HOLD ready=1");
                    }

                    if config.split_view_scenario {
                        let handle = cx
                            .update(|window, _| window.window_handle().downcast::<WorkspaceWindow>())
                            .ok()
                            .flatten();
                        let Some(handle) = handle else {
                            fail_and_quit(cx, "split-view QA could not access the workspace window");
                            return;
                        };
                        match run_split_view_scenario(handle, config.timeout, cx).await {
                            Ok(report) => eprintln!("GPUI_PDF_READER_QA_SPLIT {report}"),
                            Err(error) => {
                                fail_and_quit(cx, &error);
                                return;
                            }
                        }
                    }

                    if config.control_bar_scenario {
                        let opened = cx.update(|window, cx| {
                            let Some(workspace) = window.root::<WorkspaceWindow>().flatten() else {
                                return false;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.qa_control_bar_open_search(window, cx);
                                workspace.qa_control_bar_set_search_query("page", cx);
                            });
                            true
                        });
                        if !matches!(opened, Ok(true)) {
                            fail_and_quit(cx, "control-bar QA could not access the workspace root");
                            return;
                        }
                        let deadline = Instant::now() + config.timeout;
                        let (owner, results, expanded_height) = loop {
                            let state = cx
                                .update(|window, cx| {
                                    let workspace = window.root::<WorkspaceWindow>().flatten()?;
                                    Some(workspace.read(cx).qa_control_bar_state(cx))
                                })
                                .ok()
                                .flatten();
                            if let Some((owner, true, results, true, height)) = state
                                && results > 0
                                && (height
                                    - (key_ui_gpui::CONTEXT_BAR_HEIGHT + 94.0))
                                    .abs()
                                    <= 0.05
                            {
                                break (owner, results, height);
                            }
                            if Instant::now() >= deadline {
                                fail_and_quit(
                                    cx,
                                    &format!("control bar search did not settle: {state:?}"),
                                );
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        };
                        let active_owner = cx
                            .update(|window, cx| {
                                window
                                    .root::<WorkspaceWindow>()
                                    .flatten()
                                    .map(|workspace| workspace.read(cx).qa_window_and_view().1)
                            })
                            .ok()
                            .flatten();
                        if active_owner != Some(owner) {
                            fail_and_quit(
                                cx,
                                "control bar projection owner did not match the active view",
                            );
                            return;
                        }
                        let closed = cx.update(|window, cx| {
                            let Some(workspace) = window.root::<WorkspaceWindow>().flatten() else {
                                return false;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.qa_control_bar_close_search(window, cx)
                            });
                            true
                        });
                        if !matches!(closed, Ok(true)) {
                            fail_and_quit(cx, "control-bar QA could not close search");
                            return;
                        }
                        let deadline = Instant::now() + config.timeout;
                        loop {
                            let state = cx
                                .update(|window, cx| {
                                    let workspace = window.root::<WorkspaceWindow>().flatten()?;
                                    Some(workspace.read(cx).qa_control_bar_state(cx))
                                })
                                .ok()
                                .flatten();
                            if let Some((_, false, 0, true, height)) = state
                                && (height - key_ui_gpui::CONTEXT_BAR_HEIGHT).abs() <= 0.1
                            {
                                eprintln!(
                                    "GPUI_PDF_READER_QA_CONTROL_BAR owner={} results={} expanded_height={expanded_height:.1} collapsed_height={height:.1} reset=1",
                                    owner.get(), results
                                );
                                break;
                            }
                            if Instant::now() >= deadline {
                                fail_and_quit(
                                    cx,
                                    &format!("control bar did not collapse and reset: {state:?}"),
                                );
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }

                    if config.multi_annotation_scenario {
                        let tab_count = cx
                            .update(|window, cx| {
                                window
                                    .root::<WorkspaceWindow>()
                                    .flatten()
                                    .map_or(0, |workspace| workspace.read(cx).qa_tab_count())
                            })
                            .unwrap_or(0);
                        if tab_count < 2 {
                            fail_and_quit(
                                cx,
                                "multi-annotation QA requires at least two PDF tabs",
                            );
                            return;
                        }
                        let mut contexts = Vec::with_capacity(tab_count);
                        for index in 0..tab_count {
                            let activated = cx
                                .update(|window, cx| {
                                    let Some(workspace) =
                                        window.root::<WorkspaceWindow>().flatten()
                                    else {
                                        return false;
                                    };
                                    workspace.update(cx, |workspace, cx| {
                                        workspace.qa_activate_tab(index, window, cx)
                                    });
                                    true
                                })
                                .unwrap_or(false);
                            if !activated {
                                fail_and_quit(
                                    cx,
                                    "multi-annotation QA could not activate a PDF tab",
                                );
                                return;
                            }
                            let deadline = Instant::now() + config.timeout;
                            loop {
                                let outcome = cx
                                    .update(|window, cx| {
                                        let Some(reader) = reader_entity(window, cx) else {
                                            return Ok(false);
                                        };
                                        reader.update(cx, |reader, cx| {
                                            reader.qa_drive_fluid_scenario(window, cx)
                                        })
                                    })
                                    .unwrap_or_else(|error| {
                                        Err(format!(
                                            "multi-annotation window update failed: {error}"
                                        ))
                                    });
                                match outcome {
                                    Ok(true) => break,
                                    Ok(false) => {}
                                    Err(message) => {
                                        fail_and_quit(cx, &message);
                                        return;
                                    }
                                }
                                if Instant::now() >= deadline {
                                    fail_and_quit(
                                        cx,
                                        &format!(
                                            "multi-annotation tab {index} did not settle"
                                        ),
                                    );
                                    return;
                                }
                                cx.background_executor().timer(POLL_INTERVAL).await;
                            }
                            let context = cx
                                .update(|window, cx| {
                                    reader_entity(window, cx)
                                        .map(|reader| reader.read(cx).qa_annotation_context())
                                })
                                .ok()
                                .flatten();
                            let Some(Ok(context)) = context else {
                                fail_and_quit(
                                    cx,
                                    &format!(
                                        "multi-annotation tab {index} has invalid ownership state: {context:?}"
                                    ),
                                );
                                return;
                            };
                            contexts.push(context);
                        }
                        let unique_views = contexts
                            .iter()
                            .map(|context| context.0)
                            .collect::<std::collections::BTreeSet<_>>()
                            .len();
                        let unique_paths = contexts
                            .iter()
                            .map(|context| context.1.clone())
                            .collect::<std::collections::BTreeSet<_>>()
                            .len();
                        if unique_views != tab_count || unique_paths != tab_count {
                            fail_and_quit(
                                cx,
                                "multi-annotation contexts were reused across PDF tabs",
                            );
                            return;
                        }
                        eprintln!(
                            "GPUI_PDF_READER_QA_MULTI_ANNOTATIONS tabs={tab_count} unique_views={unique_views} unique_paths={unique_paths} persisted={}",
                            contexts.len()
                        );
                    }

                    if config.resource_stress {
                        trace_marker(&resource_trace, "resource-stress", "before", cx);
                        let handles = cx
                            .update(|_, cx| {
                                cx.windows()
                                    .into_iter()
                                    .filter_map(|handle| handle.downcast::<WorkspaceWindow>())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        for (window_index, handle) in handles.into_iter().enumerate() {
                            for (wheel_index, delta) in
                                [80.0_f32; 8].into_iter().chain([-80.0_f32; 4]).enumerate()
                            {
                                let operation = format!(
                                    "stress-{window_index}-wheel-{wheel_index}-delta-{delta}"
                                );
                                trace_marker(&resource_trace, &operation, "before", cx);
                                let outcome = handle.update(cx, |workspace, window, cx| {
                                    window.activate_window();
                                    let Some(reader) = workspace.reader() else {
                                        return Err("resource stress reader is unavailable".to_owned());
                                    };
                                    let viewport = window.viewport_size();
                                    let position =
                                        Point::new(viewport.width / 2.0, viewport.height / 2.0);
                                    reader.update(cx, |reader, cx| {
                                        reader.qa_command_wheel(delta, position, window, cx)
                                    });
                                    Ok(())
                                });
                                match outcome {
                                    Ok(Ok(())) => {}
                                    Ok(Err(message)) => {
                                        fail_and_quit(
                                            cx,
                                            &format!("resource stress failed: {message}"),
                                        );
                                        return;
                                    }
                                    Err(error) => {
                                        fail_and_quit(
                                            cx,
                                            &format!("resource stress window failed: {error}"),
                                        );
                                        return;
                                    }
                                }
                                cx.background_executor().timer(config.interval).await;
                                trace_marker(&resource_trace, &operation, "after", cx);
                            }
                        }

                        let settle_deadline = Instant::now() + config.timeout;
                        loop {
                            let (_, pdf_views, settled) = cx
                                .update(|window, cx| workspace_window_counts(window, cx))
                                .unwrap_or_default();
                            if pdf_views > 0 && settled == pdf_views {
                                break;
                            }
                            if Instant::now() >= settle_deadline {
                                fail_and_quit(cx, "resource stress did not settle every PDF view");
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                        trace_marker(&resource_trace, "resource-stress", "settled", cx);
                    }

                    if config.resource_chaos {
                        trace_marker(&resource_trace, "resource-chaos", "before", cx);
                        let mut handles = cx
                            .update(|_, cx| {
                                cx.windows()
                                    .into_iter()
                                    .filter_map(|handle| handle.downcast::<WorkspaceWindow>())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let mut random = QaRandom::new(config.chaos_seed);
                        for round in 0..config.chaos_rounds {
                            random.shuffle(&mut handles);
                            for (window_index, handle) in handles.iter().copied().enumerate() {
                                let operation = format!("chaos-{round}-window-{window_index}");
                                trace_marker(&resource_trace, &operation, "activate", cx);
                                if handle
                                    .update(cx, |_, window, _| window.activate_window())
                                    .is_err()
                                {
                                    fail_and_quit(cx, "chaos activation lost a workspace window");
                                    return;
                                }
                                if let Err(error) = wait_for_reader_settled(
                                    handle,
                                    config.timeout,
                                    config.render_dwell,
                                    cx,
                                )
                                .await
                                {
                                    fail_and_quit(cx, &error);
                                    return;
                                }

                                for action in 0..config.chaos_actions_per_window {
                                    let kind = random.next_usize(5);
                                    let magnitude = 120.0 + random.next_usize(5) as f32 * 70.0;
                                    let signed = if random.next_usize(2) == 0 {
                                        magnitude
                                    } else {
                                        -magnitude
                                    };
                                    let action_name = match kind {
                                        0 => "zoom",
                                        1 => "scroll-y",
                                        2 => "scroll-x",
                                        3 => "scroll-diagonal",
                                        _ => "zoom-reverse",
                                    };
                                    let marker = format!(
                                        "{operation}-action-{action}-{action_name}-{signed}"
                                    );
                                    trace_marker(&resource_trace, &marker, "before", cx);
                                    let outcome = handle.update(cx, |workspace, window, cx| {
                                        let reader = workspace.reader().ok_or_else(|| {
                                            "chaos reader is unavailable".to_owned()
                                        })?;
                                        let viewport = window.viewport_size();
                                        let x = viewport.width
                                            * (0.2 + random.next_usize(61) as f32 / 100.0);
                                        let y = viewport.height
                                            * (0.2 + random.next_usize(61) as f32 / 100.0);
                                        let position = Point::new(x, y);
                                        reader.update(cx, |reader, cx| match kind {
                                            0 => reader.qa_wheel(
                                                0.0, signed, true, position, window, cx,
                                            ),
                                            1 => reader.qa_wheel(
                                                0.0, signed, false, position, window, cx,
                                            ),
                                            2 => reader.qa_wheel(
                                                signed, 0.0, false, position, window, cx,
                                            ),
                                            3 => reader.qa_wheel(
                                                signed * 0.45,
                                                signed,
                                                false,
                                                position,
                                                window,
                                                cx,
                                            ),
                                            _ => reader.qa_wheel(
                                                0.0, -signed, true, position, window, cx,
                                            ),
                                        });
                                        Ok::<_, String>(())
                                    });
                                    if !matches!(outcome, Ok(Ok(()))) {
                                        fail_and_quit(cx, "chaos input failed");
                                        return;
                                    }
                                    if let Err(error) = wait_for_reader_settled(
                                        handle,
                                        config.timeout,
                                        config.render_dwell,
                                        cx,
                                    )
                                    .await
                                    {
                                        fail_and_quit(cx, &error);
                                        return;
                                    }
                                    trace_marker(&resource_trace, &marker, "settled", cx);
                                }
                            }
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(1_500))
                            .await;
                        if let Err(error) = wait_for_all_resources_settled(config.timeout, cx).await
                        {
                            fail_and_quit(cx, &error);
                            return;
                        }
                        trace_marker(&resource_trace, "resource-chaos", "settled", cx);
                    }

                    if config.has_navigation_setup() {
                        let outcome = cx
                            .update(|window, cx| {
                                let Some(reader) = reader_entity(window, cx) else {
                                    return Err("TOC QA reader is unavailable".to_owned());
                                };
                                reader.update(cx, |reader, cx| {
                                    if let Some(index) = config.toc_hover {
                                        reader.qa_set_toc_hovered(index, window, cx)?;
                                    }
                                    if config.toc_callout_hold {
                                        reader.qa_hold_toc_callout(window, cx)?;
                                    }
                                    if let Some(index) = config.toc_navigate {
                                        reader.qa_navigate_toc(index, window, cx)?;
                                    }
                                    if let Some(index) = config.link_navigate {
                                        reader.qa_navigate_link(index, window, cx)?;
                                    }
                                    if let Some(index) = config.link_hover {
                                        reader.qa_hover_link(index, window, cx)?;
                                    }
                                    if let Some(ordinal) = config.internal_link_hover {
                                        reader.qa_hover_internal_link(ordinal, window, cx)?;
                                    }
                                    if let Some(ordinal) = config.scientific_reference_hover {
                                        reader.qa_hover_scientific_reference(ordinal, window, cx)?;
                                    }
                                    Ok(())
                                })
                            })
                            .unwrap_or_else(|error| {
                                Err(format!("TOC QA window update failed: {error}"))
                            });
                        if let Err(message) = outcome {
                            fail_and_quit(cx, &message);
                            return;
                        }
                    }

                    if config.reference_details {
                        let Some(ordinal) = config.scientific_reference_hover else {
                            fail_and_quit(
                                cx,
                                "reference details require GPUI_PDF_READER_QA_SCIENTIFIC_REFERENCE_HOVER",
                            );
                            return;
                        };
                        let deadline = Instant::now() + config.timeout;
                        loop {
                            let outcome = cx
                                .update(|window, cx| {
                                    let Some(reader) = reader_entity(window, cx) else {
                                        return Err(
                                            "reference details reader is unavailable".to_owned()
                                        );
                                    };
                                    reader.update(cx, |reader, cx| {
                                        reader.qa_open_reference_details(ordinal, window, cx)
                                    })
                                })
                                .unwrap_or_else(|error| {
                                    Err(format!(
                                        "reference details window update failed: {error}"
                                    ))
                                });
                            match outcome {
                                Ok(true) => break,
                                Ok(false) => {}
                                Err(message) => {
                                    fail_and_quit(cx, &message);
                                    return;
                                }
                            }
                            if Instant::now() >= deadline {
                                fail_and_quit(cx, "reference details did not load");
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }

                    if config.feature_scenario || config.fluid_scenario {
                        let deadline = Instant::now() + config.timeout;
                        loop {
                            let outcome = cx
                                .update(|window, cx| {
                                    let Some(reader) = reader_entity(window, cx) else {
                                        return Ok(false);
                                    };
                                    reader.update(cx, |reader, cx| {
                                        if config.fluid_scenario {
                                            reader.qa_drive_fluid_scenario(window, cx)
                                        } else {
                                            reader.qa_drive_feature_scenario(window, cx)
                                        }
                                    })
                                })
                                .unwrap_or_else(|error| {
                                    Err(format!("feature scenario window update failed: {error}"))
                                });
                            match outcome {
                                Ok(true) => break,
                                Ok(false) => {}
                                Err(message) => {
                                    fail_and_quit(cx, &message);
                                    return;
                                }
                            }
                            if Instant::now() >= deadline {
                                fail_and_quit(
                                    cx,
                                    if config.fluid_scenario {
                                        "Fluid scenario did not settle"
                                    } else {
                                        "feature scenario did not settle"
                                    },
                                );
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }

                    if let Some(extension_scenario) = config.extension_scenario.as_deref() {
                        let deadline = Instant::now() + config.timeout;
                        loop {
                            let outcome = cx
                                .update(|window, cx| {
                                    let Some(reader) = reader_entity(window, cx) else {
                                        return Ok(false);
                                    };
                                    reader.update(cx, |reader, cx| {
                                        reader.qa_drive_extension_scenario(
                                            extension_scenario,
                                            window,
                                            cx,
                                        )
                                    })
                                })
                                .unwrap_or_else(|error| {
                                    Err(format!(
                                        "extension scenario window update failed: {error}"
                                    ))
                                });
                            match outcome {
                                Ok(true) => break,
                                Ok(false) => {}
                                Err(message) => {
                                    fail_and_quit(cx, &message);
                                    return;
                                }
                            }
                            if Instant::now() >= deadline {
                                let _ = cx.update(|window, cx| {
                                    if let Some(reader) = reader_entity(window, cx) {
                                        eprintln!(
                                            "GPUI_PDF_READER_QA_EXTENSION_TIMEOUT {}",
                                            reader.read(cx).qa_report()
                                        );
                                    }
                                });
                                fail_and_quit(
                                    cx,
                                    &format!(
                                        "extension scenario {extension_scenario:?} did not settle"
                                    ),
                                );
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }

                    for (index, keystroke) in config.keystrokes.into_iter().enumerate() {
                        let operation =
                            trace_operation_label("key", index, &format!("{keystroke:?}"));
                        trace_marker(&resource_trace, &operation, "before", cx);
                        let _ = cx.update(|window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        });
                        cx.background_executor().timer(config.interval).await;
                        trace_marker(&resource_trace, &operation, "after", cx);
                    }
                    for (index, delta_y) in config.wheel_deltas.into_iter().enumerate() {
                        let operation =
                            trace_operation_label("wheel", index, &delta_y.to_string());
                        trace_marker(&resource_trace, &operation, "before", cx);
                        let _ = cx.update(|window, cx| {
                            let viewport = window.viewport_size();
                            let position =
                                Point::new(viewport.width / 2.0, viewport.height / 2.0);
                            if let Some(reader) = reader_entity(window, cx) {
                                reader.update(cx, |reader, cx| {
                                    reader.qa_command_wheel(delta_y, position, window, cx);
                                });
                            }
                        });
                        cx.background_executor().timer(config.interval).await;
                        trace_marker(&resource_trace, &operation, "after", cx);
                    }

                    if config.report || config.exit_after_report {
                        let settle_deadline = Instant::now() + config.timeout;
                        let mut stable_since = None;
                        loop {
                            let settled = cx
                                .update(|window, cx| {
                                    reader_entity(window, cx).is_some_and(|reader| {
                                        let reader = reader.read(cx);
                                        if config.progressive_open || config.resource_chaos {
                                            reader.qa_resource_is_settled()
                                        } else {
                                            reader.qa_viewport_is_settled()
                                        }
                                    })
                                })
                                .unwrap_or(false);
                            let now = Instant::now();
                            if settled {
                                let since = stable_since.get_or_insert(now);
                                if now.duration_since(*since) >= STABLE_VIEWPORT_DURATION {
                                    break;
                                }
                            } else {
                                stable_since = None;
                            }
                            if now >= settle_deadline {
                                fail_and_quit(cx, "viewport did not settle");
                                return;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                        trace_marker(&resource_trace, "viewport", "settled", cx);
                        let _ = cx.update(|window, cx| {
                            if config.report
                                && let Some(reader) = reader_entity(window, cx)
                            {
                                eprintln!("{}", reader.read(cx).qa_report());
                            }
                            if config.exit_after_report {
                                cx.quit();
                            }
                        });
                    }
                })
                .detach();
        })
        .ok();
}

async fn run_split_view_scenario(
    handle: WindowHandle<WorkspaceWindow>,
    timeout: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let before = handle
        .update(cx, |workspace, window, cx| {
            let before = workspace.qa_tab_view_ids();
            if before.len() != 2 {
                return Err(format!(
                    "split-view QA requires exactly two tabs, found {}",
                    before.len()
                ));
            }
            workspace.qa_split_with_other_tab(window, cx);
            Ok(before)
        })
        .map_err(|error| format!("split-view setup failed: {error}"))??;

    let (first, second) = {
        let (tabs, visible, active, split) = handle
            .update(cx, |workspace, _, _| workspace.qa_split_state())
            .map_err(|error| format!("split-view state unavailable: {error}"))?;
        let Some((first, second, ratio)) = split else {
            return Err("two tabs did not become one compound split tab".to_owned());
        };
        if tabs != 1 || visible.len() != 2 || !visible.contains(&active) {
            return Err(format!(
                "invalid compound state: tabs={tabs} visible={visible:?} active={active:?}"
            ));
        }
        if (ratio - 0.5).abs() > f32::EPSILON {
            return Err(format!("initial split ratio was {ratio}, expected 0.5"));
        }
        (first, second)
    };

    wait_for_split_resources(handle, timeout, cx).await?;
    let initial_active = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state().2)
        .map_err(|error| format!("split active view unavailable: {error}"))?;
    let next_active = if initial_active == first {
        second
    } else {
        first
    };
    handle
        .update(cx, |workspace, window, cx| {
            workspace.qa_activate_split_view(next_active, window, cx)
        })
        .map_err(|error| format!("split activation failed: {error}"))?;
    wait_for_split_resources(handle, timeout, cx).await?;
    let active_after = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state().2)
        .map_err(|error| format!("split active view unavailable: {error}"))?;
    if active_after != next_active {
        return Err("click-equivalent split activation did not switch active view".to_owned());
    }
    let control_owner = handle
        .update(cx, |workspace, _, cx| workspace.qa_control_bar_state(cx).0)
        .map_err(|error| format!("split control-bar state unavailable: {error}"))?;
    if control_owner != active_after {
        return Err(format!(
            "split control bar followed {control_owner}, expected active view {active_after}"
        ));
    }

    handle
        .update(cx, |workspace, _, cx| {
            workspace.qa_set_split_ratio(0.35, cx)
        })
        .map_err(|error| format!("split resize failed: {error}"))?;
    let resized = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state().3)
        .map_err(|error| format!("resized split state unavailable: {error}"))?
        .ok_or_else(|| "split disappeared during resize".to_owned())?;
    if (resized.2 - 0.35).abs() > 0.001 {
        return Err(format!("split resize produced ratio {}", resized.2));
    }

    handle
        .update(cx, |workspace, window, cx| {
            workspace.qa_swap_split(window, cx)
        })
        .map_err(|error| format!("split swap failed: {error}"))?;
    let swapped = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state().3)
        .map_err(|error| format!("swapped split state unavailable: {error}"))?
        .ok_or_else(|| "split disappeared during swap".to_owned())?;
    if swapped.0 != resized.1 || swapped.1 != resized.0 || (swapped.2 - 0.65).abs() > 0.001 {
        return Err(format!("split swap produced unexpected state: {swapped:?}"));
    }

    handle
        .update(cx, |workspace, window, cx| {
            workspace.qa_separate_split(window, cx)
        })
        .map_err(|error| format!("split separation failed: {error}"))?;
    let separated = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state())
        .map_err(|error| format!("separated state unavailable: {error}"))?;
    if separated.0 != 2 || separated.1.len() != 1 || separated.3.is_some() {
        return Err(format!(
            "split separation produced invalid state: {separated:?}"
        ));
    }

    handle
        .update(cx, |workspace, window, cx| {
            workspace.qa_split_with_other_tab(window, cx)
        })
        .map_err(|error| format!("second split failed: {error}"))?;
    let close_view = handle
        .update(cx, |workspace, _, _| {
            workspace.qa_split_state().3.map(|split| split.0)
        })
        .map_err(|error| format!("second split state unavailable: {error}"))?
        .ok_or_else(|| "views did not recombine after separation".to_owned())?;
    handle
        .update(cx, |workspace, window, cx| {
            workspace.qa_close_split_view(close_view, window, cx)
        })
        .map_err(|error| format!("split child close failed: {error}"))?;
    let closed = handle
        .update(cx, |workspace, _, _| workspace.qa_split_state())
        .map_err(|error| format!("closed split state unavailable: {error}"))?;
    if closed.0 != 1 || closed.1.len() != 1 || closed.3.is_some() {
        return Err(format!(
            "split child close produced invalid state: {closed:?}"
        ));
    }

    Ok(format!(
        "opened={} visible=2 switched=1 bar_owner=1 resized=1 swapped=1 separated=1 closed=1",
        before.len()
    ))
}

async fn wait_for_split_resources(
    handle: WindowHandle<WorkspaceWindow>,
    timeout: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let (readers, settled, interactive, visible, pane_aligned) = handle
            .update(cx, |workspace, window, cx| {
                let readers = workspace.readers();
                let window_width = f32::from(window.viewport_size().width).max(1.0);
                let settled = readers
                    .iter()
                    .filter(|reader| reader.read(cx).qa_viewport_is_settled())
                    .count();
                let activities = readers
                    .iter()
                    .map(|reader| reader.read(cx).qa_resource_snapshot().activity)
                    .collect::<Vec<_>>();
                let pane_aligned = readers.iter().all(|reader| {
                    let (width, height, measured) = reader.read(cx).qa_viewport_contract();
                    measured.is_some_and(|(measured_width, measured_height)| {
                        (width - measured_width).abs() <= 1.0
                            && (height - measured_height).abs() <= 1.0
                            && width < window_width * 0.75
                    })
                });
                (
                    readers.len(),
                    settled,
                    activities
                        .iter()
                        .filter(|activity| {
                            **activity == key_workspace_core::ActivityLevel::ForegroundInteractive
                        })
                        .count(),
                    activities
                        .iter()
                        .filter(|activity| {
                            **activity == key_workspace_core::ActivityLevel::ForegroundVisible
                        })
                        .count(),
                    pane_aligned,
                )
            })
            .map_err(|error| format!("split resource state unavailable: {error}"))?;
        if readers == 2 && settled == 2 && interactive == 1 && visible == 1 && pane_aligned {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "split resources did not settle: readers={readers} settled={settled} interactive={interactive} visible={visible} pane_aligned={pane_aligned}"
            ));
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    }
}

fn apply_initial_overrides(window: WindowHandle<WorkspaceWindow>, cx: &mut App) {
    if let Ok(theme_name) = std::env::var("GPUI_PDF_READER_QA_THEME") {
        window
            .update(cx, |workspace, window, cx| {
                if let Some(reader) = workspace.reader() {
                    reader.update(cx, |reader, cx| {
                        assert!(
                            reader.qa_select_theme(&theme_name, window, cx),
                            "unknown GPUI_PDF_READER_QA_THEME: {theme_name}"
                        );
                    });
                }
            })
            .ok();
    }
    if let Ok(value) = std::env::var("GPUI_PDF_READER_QA_PDF_DARK") {
        let enabled = match value.as_str() {
            "on" | "1" | "true" => true,
            "off" | "0" | "false" => false,
            _ => panic!("invalid GPUI_PDF_READER_QA_PDF_DARK: {value}"),
        };
        window
            .update(cx, |workspace, window, cx| {
                if let Some(reader) = workspace.reader() {
                    reader.update(cx, |reader, cx| {
                        reader.qa_set_pdf_dark_mode(enabled, window, cx)
                    });
                }
            })
            .ok();
    }
}

fn reader_entity(window: &Window, cx: &App) -> Option<Entity<PdfReader>> {
    window
        .root::<WorkspaceWindow>()
        .flatten()
        .and_then(|workspace| workspace.read(cx).reader())
}

fn workspace_window_counts(current: &Window, cx: &App) -> (usize, usize, usize) {
    let windows = cx.windows();
    let current_handle = current.window_handle();
    let mut pdf_views = 0;
    let mut settled = 0;
    for handle in &windows {
        if *handle == current_handle {
            let Some(workspace) = current.root::<WorkspaceWindow>().flatten() else {
                continue;
            };
            for reader in workspace.read(cx).readers() {
                pdf_views += 1;
                settled += usize::from(reader.read(cx).qa_resource_is_settled());
            }
            continue;
        }
        let Some(handle) = handle.downcast::<WorkspaceWindow>() else {
            continue;
        };
        let Ok(workspace) = handle.read(cx) else {
            continue;
        };
        for reader in workspace.readers() {
            pdf_views += 1;
            settled += usize::from(reader.read(cx).qa_resource_is_settled());
        }
    }
    (windows.len(), pdf_views, settled)
}

#[derive(Clone)]
struct QaResourceTrace {
    started: Instant,
    sequence: Arc<AtomicU64>,
    system: key_workspace_core::SystemResources,
}

impl QaResourceTrace {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            sequence: Arc::new(AtomicU64::new(0)),
            system: crate::system_resources::detect(),
        }
    }

    fn emit(&self, operation: &str, phase: &str, window: &Window, cx: &App) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let elapsed_us = self.started.elapsed().as_micros();
        let allocator = crate::qa_resources::allocator_snapshot();
        let process = crate::qa_resources::process_memory_snapshot().unwrap_or_default();
        let views = workspace_resource_totals(window, cx);
        eprintln!(
            "GPUI_PDF_READER_QA_RESOURCE seq={sequence} elapsed_us={elapsed_us} operation={operation} phase={phase} physical_memory={} low_power={} virtual={} resident={} resident_peak={} footprint={} footprint_peak={} alloc_live={} alloc_peak={} alloc_calls={} dealloc_calls={} realloc_calls={} allocated_bytes={} deallocated_bytes={} views={} interactive={} visible={} idle={} warm={} cold={} suspended={} allocation_cpu={} allocation_gpu={} allocation_workers={} tile_bytes={} tile_resident_estimate={} tile_limit={} tile_base_limit={} tiles={} pending_tiles={} text_pages={} trimmed_views={} hibernated_views={} retention_percent={}",
            self.system.physical_memory_bytes,
            u8::from(self.system.low_power_mode),
            process.virtual_bytes,
            process.resident_bytes,
            process.peak_resident_bytes,
            process.physical_footprint_bytes,
            process.peak_physical_footprint_bytes,
            allocator.live_bytes,
            allocator.peak_live_bytes,
            allocator.alloc_calls,
            allocator.dealloc_calls,
            allocator.realloc_calls,
            allocator.allocated_bytes,
            allocator.deallocated_bytes,
            views.views,
            views.interactive,
            views.visible,
            views.idle,
            views.warm,
            views.cold,
            views.suspended,
            views.allocated_cpu_bytes,
            views.allocated_gpu_bytes,
            views.allocated_workers,
            views.cached_tile_bytes,
            views.estimated_tile_resident_bytes,
            views.tile_cache_limit_bytes,
            views.base_tile_cache_limit_bytes,
            views.cached_tiles,
            views.pending_tiles,
            views.cached_text_pages,
            views.trimmed_views,
            views.hibernated_views,
            views.cache_retention_percent,
        );
    }
}

fn install_resource_sampler(
    window: WindowHandle<WorkspaceWindow>,
    trace: QaResourceTrace,
    interval: Duration,
    cx: &mut App,
) {
    window
        .update(cx, |_, window, cx| {
            window
                .spawn(cx, async move |cx| {
                    loop {
                        let sampled = cx
                            .update(|window, cx| trace.emit("timeline", "sample", window, cx))
                            .is_ok();
                        if !sampled {
                            break;
                        }
                        cx.background_executor().timer(interval).await;
                    }
                })
                .detach();
        })
        .ok();
}

fn trace_marker(
    trace: &Option<QaResourceTrace>,
    operation: &str,
    phase: &str,
    cx: &mut gpui::AsyncWindowContext,
) {
    let Some(trace) = trace else {
        return;
    };
    let _ = cx.update(|window, cx| trace.emit(operation, phase, window, cx));
}

fn trace_operation_label(kind: &str, index: usize, detail: &str) -> String {
    let detail = detail
        .chars()
        .take(80)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{kind}-{index}-{detail}")
}

#[derive(Default)]
struct QaWorkspaceResourceTotals {
    views: u64,
    interactive: u64,
    visible: u64,
    idle: u64,
    warm: u64,
    cold: u64,
    suspended: u64,
    allocated_cpu_bytes: u64,
    allocated_gpu_bytes: u64,
    allocated_workers: u64,
    cached_tile_bytes: u64,
    estimated_tile_resident_bytes: u64,
    tile_cache_limit_bytes: u64,
    base_tile_cache_limit_bytes: u64,
    cached_tiles: u64,
    pending_tiles: u64,
    cached_text_pages: u64,
    trimmed_views: u64,
    hibernated_views: u64,
    cache_retention_percent: u64,
}

impl QaWorkspaceResourceTotals {
    fn add(&mut self, snapshot: QaReaderResourceSnapshot) {
        use key_workspace_core::ActivityLevel;
        self.views += 1;
        match snapshot.activity {
            ActivityLevel::ForegroundInteractive => self.interactive += 1,
            ActivityLevel::ForegroundVisible => self.visible += 1,
            ActivityLevel::ForegroundIdle => self.idle += 1,
            ActivityLevel::BackgroundWarm => self.warm += 1,
            ActivityLevel::BackgroundCold => self.cold += 1,
            ActivityLevel::Suspended => self.suspended += 1,
        }
        self.allocated_cpu_bytes = self
            .allocated_cpu_bytes
            .saturating_add(snapshot.allocated_cpu_bytes);
        self.allocated_gpu_bytes = self
            .allocated_gpu_bytes
            .saturating_add(snapshot.allocated_gpu_bytes);
        self.allocated_workers = self
            .allocated_workers
            .saturating_add(snapshot.allocated_workers);
        self.cached_tile_bytes = self
            .cached_tile_bytes
            .saturating_add(snapshot.cached_tile_bytes);
        self.estimated_tile_resident_bytes = self
            .estimated_tile_resident_bytes
            .saturating_add(snapshot.estimated_tile_resident_bytes);
        self.tile_cache_limit_bytes = self
            .tile_cache_limit_bytes
            .saturating_add(snapshot.tile_cache_limit_bytes);
        self.base_tile_cache_limit_bytes = self
            .base_tile_cache_limit_bytes
            .saturating_add(snapshot.base_tile_cache_limit_bytes);
        self.cached_tiles = self.cached_tiles.saturating_add(snapshot.cached_tiles);
        self.pending_tiles = self.pending_tiles.saturating_add(snapshot.pending_tiles);
        self.cached_text_pages = self
            .cached_text_pages
            .saturating_add(snapshot.cached_text_pages);
        self.trimmed_views += u64::from(snapshot.cache_trimmed);
        self.hibernated_views += u64::from(snapshot.worker_hibernated);
        self.cache_retention_percent = self
            .cache_retention_percent
            .max(snapshot.cache_retention_percent);
    }
}

fn workspace_resource_totals(current: &Window, cx: &App) -> QaWorkspaceResourceTotals {
    let mut totals = QaWorkspaceResourceTotals::default();
    let current_handle = current.window_handle();
    for handle in cx.windows() {
        if handle == current_handle {
            let Some(workspace) = current.root::<WorkspaceWindow>().flatten() else {
                continue;
            };
            for reader in workspace.read(cx).readers() {
                totals.add(reader.read(cx).qa_resource_snapshot());
            }
            continue;
        }
        let Some(handle) = handle.downcast::<WorkspaceWindow>() else {
            continue;
        };
        let Ok(workspace) = handle.read(cx) else {
            continue;
        };
        for reader in workspace.readers() {
            totals.add(reader.read(cx).qa_resource_snapshot());
        }
    }
    totals
}

async fn wait_for_reader_settled(
    handle: WindowHandle<WorkspaceWindow>,
    timeout: Duration,
    dwell: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut stable_since = None;
    loop {
        let settled = handle
            .update(cx, |workspace, _, cx| {
                workspace
                    .reader()
                    .is_some_and(|reader| reader.read(cx).qa_viewport_is_settled())
            })
            .unwrap_or(false);
        if settled {
            let since = stable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= dwell {
                return Ok(());
            }
        } else {
            stable_since = None;
        }
        if Instant::now() >= deadline {
            return Err("PDF viewport did not settle during progressive resource QA".to_owned());
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    }
}

async fn wait_for_all_resources_settled(
    timeout: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let (windows, pdf_views, settled) = cx
            .update(|window, cx| workspace_window_counts(window, cx))
            .unwrap_or_default();
        if windows > 0 && pdf_views > 0 && settled == pdf_views {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "resource settlement timed out: windows={windows} pdf_views={pdf_views} settled={settled}"
            ));
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    }
}

struct QaRandom(u64);

impl QaRandom {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn next_usize(&mut self, upper: usize) -> usize {
        debug_assert!(upper > 0);
        (self.next() % upper as u64) as usize
    }

    fn shuffle<T>(&mut self, values: &mut [T]) {
        for index in (1..values.len()).rev() {
            values.swap(index, self.next_usize(index + 1));
        }
    }
}

fn fail_and_quit(cx: &mut gpui::AsyncWindowContext, message: &str) {
    eprintln!("GPUI_PDF_READER_QA_ERROR {message}");
    let _ = cx.update(|_, cx| cx.quit());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_configuration_does_not_start_the_driver() {
        let config = QaConfig {
            keystrokes: Vec::new(),
            wheel_deltas: Vec::new(),
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            report: false,
            exit_after_report: false,
            feature_scenario: false,
            fluid_scenario: false,
            toc_hover: None,
            toc_navigate: None,
            link_navigate: None,
            link_hover: None,
            internal_link_hover: None,
            scientific_reference_hover: None,
            toc_callout_hold: false,
            reference_details: false,
            extension_scenario: None,
            expected_window_count: None,
            expected_tab_count: None,
            tab_drag_scenario: None,
            split_view_scenario: false,
            split_view_hold: false,
            tab_hover_hold: false,
            control_bar_scenario: false,
            multi_annotation_scenario: false,
            resource_trace: false,
            resource_sample_interval: Duration::from_millis(DEFAULT_RESOURCE_SAMPLE_MS),
            resource_stress: false,
            progressive_open: false,
            resource_chaos: false,
            chaos_rounds: 2,
            chaos_actions_per_window: 4,
            chaos_seed: 1,
            render_dwell: Duration::from_millis(320),
        };

        assert!(!config.requested());
        assert!(!config.has_navigation_setup());
    }

    #[test]
    fn chaos_randomization_is_deterministic_and_bounded() {
        let mut first = QaRandom::new(42);
        let mut second = QaRandom::new(42);
        let first_values = (0..32).map(|_| first.next_usize(7)).collect::<Vec<_>>();
        let second_values = (0..32).map(|_| second.next_usize(7)).collect::<Vec<_>>();
        assert_eq!(first_values, second_values);
        assert!(first_values.iter().all(|value| *value < 7));
    }
}
