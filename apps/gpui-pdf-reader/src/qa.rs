//! Debug-only native QA orchestration.
//!
//! Production startup only installs this module when debug assertions are
//! enabled. The environment protocol stays app-specific while `PdfReader`
//! exposes the narrow instrumentation hooks used by the native E2E scripts.

use crate::reader::PdfReader;
use gpui::{App, Keystroke, Point, WindowHandle};
use std::fmt::Display;
use std::str::FromStr;
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_MS: u64 = 180;
const DEFAULT_TIMEOUT_MS: u64 = 20_000;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const STABLE_VIEWPORT_DURATION: Duration = Duration::from_millis(250);

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
pub fn install(window: WindowHandle<PdfReader>, cx: &mut App) {
    apply_initial_overrides(window, cx);
    let config = QaConfig::from_environment();
    if !config.requested() {
        return;
    }

    window
        .update(cx, |_, window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let startup_deadline = Instant::now() + config.timeout;
                    loop {
                        let ready = cx
                            .update(|window, cx| {
                                window.root::<PdfReader>().flatten().is_some_and(|reader| {
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

                    if config.has_navigation_setup() {
                        let outcome = cx
                            .update(|window, cx| {
                                let Some(Some(reader)) = window.root::<PdfReader>() else {
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
                                    let Some(Some(reader)) = window.root::<PdfReader>() else {
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
                                    let Some(Some(reader)) = window.root::<PdfReader>() else {
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
                                    let Some(Some(reader)) = window.root::<PdfReader>() else {
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
                                    if let Some(Some(reader)) = window.root::<PdfReader>() {
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

                    for keystroke in config.keystrokes {
                        let _ = cx.update(|window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        });
                        cx.background_executor().timer(config.interval).await;
                    }
                    for delta_y in config.wheel_deltas {
                        let _ = cx.update(|window, cx| {
                            let viewport = window.viewport_size();
                            let position =
                                Point::new(viewport.width / 2.0, viewport.height / 2.0);
                            if let Some(Some(reader)) = window.root::<PdfReader>() {
                                reader.update(cx, |reader, cx| {
                                    reader.qa_command_wheel(delta_y, position, window, cx);
                                });
                            }
                        });
                        cx.background_executor().timer(config.interval).await;
                    }

                    if config.report || config.exit_after_report {
                        let settle_deadline = Instant::now() + config.timeout;
                        let mut stable_since = None;
                        loop {
                            let settled = cx
                                .update(|window, cx| {
                                    window.root::<PdfReader>().flatten().is_some_and(|reader| {
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
                        let _ = cx.update(|window, cx| {
                            if config.report
                                && let Some(Some(reader)) = window.root::<PdfReader>()
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

fn apply_initial_overrides(window: WindowHandle<PdfReader>, cx: &mut App) {
    if flag("GPUI_PDF_READER_QA_FLUID_VIEW") {
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
    if let Ok(value) = std::env::var("GPUI_PDF_READER_QA_PDF_DARK") {
        let enabled = match value.as_str() {
            "on" | "1" | "true" => true,
            "off" | "0" | "false" => false,
            _ => panic!("invalid GPUI_PDF_READER_QA_PDF_DARK: {value}"),
        };
        window
            .update(cx, |reader, window, cx| {
                reader.qa_set_pdf_dark_mode(enabled, window, cx);
            })
            .ok();
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
        };

        assert!(!config.requested());
        assert!(!config.has_navigation_setup());
    }
}
