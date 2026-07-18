//! Debug-only native QA orchestration.
//!
//! Production startup only installs this module when debug assertions are
//! enabled. The environment protocol stays app-specific while `PdfReader`
//! exposes the narrow instrumentation hooks used by the native E2E scripts.

use crate::reader::PdfReader;
use crate::reader::qa::QaReaderResourceSnapshot;
use crate::workspace_window::WorkspaceWindow;
use gpui::{App, Entity, Keystroke, Point, Window, WindowHandle};
use std::fmt::Display;
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
    resource_trace: bool,
    resource_sample_interval: Duration,
    resource_stress: bool,
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
            resource_trace: flag("GPUI_PDF_READER_QA_RESOURCE_TRACE"),
            resource_sample_interval: Duration::from_millis(
                optional_value::<u64>("GPUI_PDF_READER_QA_RESOURCE_SAMPLE_MS")
                    .unwrap_or(DEFAULT_RESOURCE_SAMPLE_MS)
                    .clamp(20, 5_000),
            ),
            resource_stress: flag("GPUI_PDF_READER_QA_RESOURCE_STRESS"),
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
            || self.resource_trace
            || self.resource_stress
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
pub fn install(window: WindowHandle<WorkspaceWindow>, cx: &mut App) {
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
                                        reader.read(cx).qa_viewport_is_settled()
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

fn apply_initial_overrides(window: WindowHandle<WorkspaceWindow>, cx: &mut App) {
    if flag("GPUI_PDF_READER_QA_FLUID_VIEW") {
        window
            .update(cx, |workspace, window, cx| {
                if let Some(reader) = workspace.reader() {
                    reader.update(cx, |reader, cx| reader.qa_use_fluid_view(window, cx));
                }
            })
            .ok();
    }
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
            let Some(reader) = reader_entity(current, cx) else {
                continue;
            };
            pdf_views += 1;
            settled += usize::from(reader.read(cx).qa_viewport_is_settled());
            continue;
        }
        let Some(handle) = handle.downcast::<WorkspaceWindow>() else {
            continue;
        };
        let Ok(workspace) = handle.read(cx) else {
            continue;
        };
        let Some(reader) = workspace.reader() else {
            continue;
        };
        pdf_views += 1;
        settled += usize::from(reader.read(cx).qa_viewport_is_settled());
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
            "GPUI_PDF_READER_QA_RESOURCE seq={sequence} elapsed_us={elapsed_us} operation={operation} phase={phase} physical_memory={} low_power={} virtual={} resident={} resident_peak={} alloc_live={} alloc_peak={} alloc_calls={} dealloc_calls={} realloc_calls={} allocated_bytes={} deallocated_bytes={} views={} interactive={} idle={} warm={} cold={} suspended={} allocation_cpu={} allocation_gpu={} allocation_workers={} tile_bytes={} tile_resident_estimate={} tile_limit={} tiles={} pending_tiles={} text_pages={}",
            self.system.physical_memory_bytes,
            u8::from(self.system.low_power_mode),
            process.virtual_bytes,
            process.resident_bytes,
            process.peak_resident_bytes,
            allocator.live_bytes,
            allocator.peak_live_bytes,
            allocator.alloc_calls,
            allocator.dealloc_calls,
            allocator.realloc_calls,
            allocator.allocated_bytes,
            allocator.deallocated_bytes,
            views.views,
            views.interactive,
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
            views.cached_tiles,
            views.pending_tiles,
            views.cached_text_pages,
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
    cached_tiles: u64,
    pending_tiles: u64,
    cached_text_pages: u64,
}

impl QaWorkspaceResourceTotals {
    fn add(&mut self, snapshot: QaReaderResourceSnapshot) {
        use key_workspace_core::ActivityLevel;
        self.views += 1;
        match snapshot.activity {
            ActivityLevel::ForegroundInteractive => self.interactive += 1,
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
        self.cached_tiles = self.cached_tiles.saturating_add(snapshot.cached_tiles);
        self.pending_tiles = self.pending_tiles.saturating_add(snapshot.pending_tiles);
        self.cached_text_pages = self
            .cached_text_pages
            .saturating_add(snapshot.cached_text_pages);
    }
}

fn workspace_resource_totals(current: &Window, cx: &App) -> QaWorkspaceResourceTotals {
    let mut totals = QaWorkspaceResourceTotals::default();
    let current_handle = current.window_handle();
    for handle in cx.windows() {
        if handle == current_handle {
            if let Some(reader) = reader_entity(current, cx) {
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
        let Some(reader) = workspace.reader() else {
            continue;
        };
        totals.add(reader.read(cx).qa_resource_snapshot());
    }
    totals
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
            resource_trace: false,
            resource_sample_interval: Duration::from_millis(DEFAULT_RESOURCE_SAMPLE_MS),
            resource_stress: false,
        };

        assert!(!config.requested());
        assert!(!config.has_navigation_setup());
    }
}
