use crate::model::TextBounds;
use std::time::{Duration, Instant};

pub const NAVIGATION_FOCUS_DURATION: Duration = Duration::from_millis(500);
pub const NAVIGATION_FOCUS_PULSE_DURATION: Duration = Duration::from_millis(360);
const MAX_NAVIGATION_FOCUS_RUNS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NavigationFocusTone {
    #[default]
    Accent,
    SearchMatch,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NavigationFocusMotion {
    #[default]
    Sweep,
    Pulse,
}

impl NavigationFocusMotion {
    pub fn duration(self) -> Duration {
        match self {
            Self::Sweep => NAVIGATION_FOCUS_DURATION,
            Self::Pulse => NAVIGATION_FOCUS_PULSE_DURATION,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationFocusTarget {
    pub page: usize,
    pub y_fraction: f32,
    pub text_runs: Vec<TextBounds>,
    pub tone: NavigationFocusTone,
    pub motion: NavigationFocusMotion,
}

impl NavigationFocusTarget {
    pub fn new(page: usize, y_fraction: f32, text_runs: Vec<TextBounds>) -> Self {
        let y_fraction = if y_fraction.is_finite() {
            y_fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let text_runs: Vec<_> = text_runs
            .into_iter()
            .filter_map(|bounds| {
                let values = [bounds.left, bounds.top, bounds.right, bounds.bottom];
                values.iter().all(|value| value.is_finite()).then(|| {
                    let left = bounds.left.clamp(0.0, 1.0);
                    let top = bounds.top.clamp(0.0, 1.0);
                    let right = bounds.right.clamp(left, 1.0);
                    let bottom = bounds.bottom.clamp(top, 1.0);
                    TextBounds {
                        left,
                        top,
                        right,
                        bottom,
                    }
                })
            })
            .filter(|bounds| bounds.right > bounds.left && bounds.bottom > bounds.top)
            .take(MAX_NAVIGATION_FOCUS_RUNS)
            .collect();
        let text_runs = coalesce_collinear_runs(text_runs);
        Self {
            page,
            y_fraction,
            text_runs,
            tone: NavigationFocusTone::Accent,
            motion: NavigationFocusMotion::Sweep,
        }
    }

    pub fn with_tone(mut self, tone: NavigationFocusTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn with_motion(mut self, motion: NavigationFocusMotion) -> Self {
        self.motion = motion;
        self
    }
}

fn coalesce_collinear_runs(runs: Vec<TextBounds>) -> Vec<TextBounds> {
    if runs.len() < 2 {
        return runs;
    }
    let union = runs.iter().fold(runs[0], |union, bounds| TextBounds {
        left: union.left.min(bounds.left),
        top: union.top.min(bounds.top),
        right: union.right.max(bounds.right),
        bottom: union.bottom.max(bounds.bottom),
    });
    let max_width = runs
        .iter()
        .map(|bounds| bounds.right - bounds.left)
        .max_by(f32::total_cmp)
        .unwrap_or(0.0);
    let max_height = runs
        .iter()
        .map(|bounds| bounds.bottom - bounds.top)
        .max_by(f32::total_cmp)
        .unwrap_or(0.0);
    let (min_center_x, max_center_x, min_center_y, max_center_y) = runs.iter().fold(
        (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ),
        |(min_x, max_x, min_y, max_y), bounds| {
            let center_x = (bounds.left + bounds.right) * 0.5;
            let center_y = (bounds.top + bounds.bottom) * 0.5;
            (
                min_x.min(center_x),
                max_x.max(center_x),
                min_y.min(center_y),
                max_y.max(center_y),
            )
        },
    );
    let union_width = union.right - union.left;
    let union_height = union.bottom - union.top;
    let horizontal_line =
        union_width >= union_height && max_center_y - min_center_y <= max_height * 0.75;
    let vertical_line =
        union_height > union_width && max_center_x - min_center_x <= max_width * 0.75;
    if horizontal_line || vertical_line {
        vec![union]
    } else {
        runs
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationFocusFrame {
    pub target: NavigationFocusTarget,
    pub sweep: f32,
    pub intensity: f32,
    pub scale: f32,
}

#[derive(Clone, Debug)]
struct ActiveNavigationFocus {
    target: NavigationFocusTarget,
    started_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct NavigationFocusEffect {
    pending: Option<NavigationFocusTarget>,
    active: Option<ActiveNavigationFocus>,
}

impl NavigationFocusEffect {
    pub fn queue(&mut self, target: NavigationFocusTarget) {
        self.pending = Some(target);
        self.active = None;
    }

    pub fn cancel(&mut self) {
        self.pending = None;
        self.active = None;
    }

    pub fn is_busy(&self, now: Instant) -> bool {
        self.pending.is_some()
            || self.active.as_ref().is_some_and(|active| {
                now.saturating_duration_since(active.started_at) < active.target.motion.duration()
            })
    }

    /// Advances the effect after its owner has updated navigation geometry.
    ///
    /// A queued focus begins only when the associated navigation has settled,
    /// so the cue never races across the screen while the document is moving.
    /// Returns `true` while another animation frame is required.
    pub fn advance(&mut self, now: Instant, navigation_settled: bool) -> bool {
        if navigation_settled && let Some(target) = self.pending.take() {
            self.active = Some(ActiveNavigationFocus {
                target,
                started_at: now,
            });
        }
        if self.active.as_ref().is_some_and(|active| {
            now.saturating_duration_since(active.started_at) >= active.target.motion.duration()
        }) {
            self.active = None;
        }
        self.pending.is_some() || self.active.is_some()
    }

    pub fn frame(&self, now: Instant) -> Option<NavigationFocusFrame> {
        let active = self.active.as_ref()?;
        let progress = (now
            .saturating_duration_since(active.started_at)
            .as_secs_f32()
            / active.target.motion.duration().as_secs_f32())
        .clamp(0.0, 1.0);
        let sweep_progress = (progress / 0.72).clamp(0.0, 1.0);
        let sweep = 1.0 - (1.0 - sweep_progress).powi(3);
        let (intensity, scale) = match active.target.motion {
            NavigationFocusMotion::Sweep => {
                let fade_in = (progress / 0.12).clamp(0.0, 1.0);
                let fade_out = ((1.0 - progress) / 0.45).clamp(0.0, 1.0);
                (fade_in.min(fade_out), 1.0)
            }
            NavigationFocusMotion::Pulse => (
                (std::f32::consts::PI * progress).sin().max(0.0).powf(0.72),
                pulse_scale(progress),
            ),
        };
        Some(NavigationFocusFrame {
            target: active.target.clone(),
            sweep,
            intensity,
            scale,
        })
    }
}

fn pulse_scale(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.34 {
        let phase = progress / 0.34;
        1.0 + 0.04 * (1.0 - (1.0 - phase).powi(3))
    } else if progress < 0.72 {
        let phase = (progress - 0.34) / 0.38;
        let eased = phase * phase * (3.0 - 2.0 * phase);
        1.04 + (0.995 - 1.04) * eased
    } else {
        let phase = (progress - 0.72) / 0.28;
        let eased = 1.0 - (1.0 - phase).powi(2);
        0.995 + (1.0 - 0.995) * eased
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_waits_for_navigation_then_sweeps_and_expires() {
        let start = Instant::now();
        let target = NavigationFocusTarget::new(
            2,
            0.6,
            vec![TextBounds {
                left: 0.2,
                top: 0.58,
                right: 0.7,
                bottom: 0.64,
            }],
        );
        let mut effect = NavigationFocusEffect::default();
        effect.queue(target.clone());

        assert!(effect.is_busy(start));
        assert!(effect.advance(start, false));
        assert_eq!(effect.frame(start), None);
        assert!(effect.advance(start, true));

        let middle = start + Duration::from_millis(250);
        let frame = effect.frame(middle).expect("focus should be visible");
        assert_eq!(frame.target, target);
        assert!(frame.sweep > 0.8);
        assert!(frame.intensity > 0.5);
        assert_eq!(frame.scale, 1.0);

        let end = start + NAVIGATION_FOCUS_DURATION;
        assert!(!effect.advance(end, true));
        assert!(!effect.is_busy(end));
        assert_eq!(effect.frame(end), None);
    }

    #[test]
    fn a_new_target_replaces_an_active_focus() {
        let start = Instant::now();
        let mut effect = NavigationFocusEffect::default();
        effect.queue(NavigationFocusTarget::new(0, 0.2, Vec::new()));
        assert!(effect.advance(start, true));
        effect.queue(NavigationFocusTarget::new(3, 0.8, Vec::new()));
        assert_eq!(effect.frame(start), None);
        assert!(effect.advance(start + Duration::from_millis(10), true));
        assert_eq!(
            effect
                .frame(start + Duration::from_millis(20))
                .expect("replacement should animate")
                .target
                .page,
            3
        );
    }

    #[test]
    fn cancel_clears_both_queued_and_active_focus() {
        let start = Instant::now();
        let mut effect = NavigationFocusEffect::default();
        effect.queue(NavigationFocusTarget::new(0, 0.2, Vec::new()));
        effect.cancel();
        assert!(!effect.is_busy(start));
        assert!(!effect.advance(start, true));

        effect.queue(NavigationFocusTarget::new(1, 0.4, Vec::new()));
        assert!(effect.advance(start, true));
        effect.cancel();
        assert!(!effect.is_busy(start));
        assert_eq!(effect.frame(start), None);
    }

    #[test]
    fn target_sanitizes_untrusted_geometry_and_caps_work() {
        let mut runs = vec![
            TextBounds {
                left: f32::NAN,
                top: 0.0,
                right: 1.0,
                bottom: 0.1,
            },
            TextBounds {
                left: -0.2,
                top: 0.1,
                right: 1.4,
                bottom: 0.2,
            },
        ];
        runs.extend((0..MAX_NAVIGATION_FOCUS_RUNS + 10).map(|index| TextBounds {
            left: 0.1 + (index % 8) as f32 * 0.1,
            top: (index / 8) as f32 * 0.1,
            right: 0.15 + (index % 8) as f32 * 0.1,
            bottom: (index / 8) as f32 * 0.1 + 0.05,
        }));

        let target = NavigationFocusTarget::new(1, f32::INFINITY, runs);

        assert_eq!(target.y_fraction, 0.0);
        assert_eq!(target.text_runs.len(), MAX_NAVIGATION_FOCUS_RUNS);
        assert_eq!(
            target.text_runs[0],
            TextBounds {
                left: 0.0,
                top: 0.1,
                right: 1.0,
                bottom: 0.2,
            }
        );
        assert!(target.text_runs.iter().all(|bounds| {
            bounds.left >= 0.0
                && bounds.right <= 1.0
                && bounds.top >= 0.0
                && bounds.bottom <= 1.0
                && bounds.right > bounds.left
                && bounds.bottom > bounds.top
        }));
    }

    #[test]
    fn collinear_glyph_runs_become_one_smooth_focus_region() {
        let horizontal = NavigationFocusTarget::new(
            0,
            0.2,
            vec![
                TextBounds {
                    left: 0.1,
                    top: 0.2,
                    right: 0.2,
                    bottom: 0.24,
                },
                TextBounds {
                    left: 0.21,
                    top: 0.201,
                    right: 0.3,
                    bottom: 0.241,
                },
            ],
        );
        assert_eq!(
            horizontal.text_runs,
            vec![TextBounds {
                left: 0.1,
                top: 0.2,
                right: 0.3,
                bottom: 0.241,
            }]
        );

        let vertical = NavigationFocusTarget::new(
            0,
            0.2,
            vec![
                TextBounds {
                    left: 0.6,
                    top: 0.2,
                    right: 0.64,
                    bottom: 0.3,
                },
                TextBounds {
                    left: 0.601,
                    top: 0.31,
                    right: 0.641,
                    bottom: 0.4,
                },
            ],
        );
        assert_eq!(vertical.text_runs.len(), 1);

        let multiline = NavigationFocusTarget::new(
            0,
            0.2,
            vec![
                TextBounds {
                    left: 0.1,
                    top: 0.2,
                    right: 0.5,
                    bottom: 0.24,
                },
                TextBounds {
                    left: 0.1,
                    top: 0.3,
                    right: 0.42,
                    bottom: 0.34,
                },
            ],
        );
        assert_eq!(multiline.text_runs.len(), 2);
    }

    #[test]
    fn focus_targets_default_to_swept_accent_and_accept_semantics() {
        let accent = NavigationFocusTarget::new(0, 0.2, Vec::new());
        assert_eq!(accent.tone, NavigationFocusTone::Accent);
        assert_eq!(accent.motion, NavigationFocusMotion::Sweep);

        let search = accent
            .with_tone(NavigationFocusTone::SearchMatch)
            .with_motion(NavigationFocusMotion::Pulse);
        assert_eq!(search.tone, NavigationFocusTone::SearchMatch);
        assert_eq!(search.motion, NavigationFocusMotion::Pulse);
    }

    #[test]
    fn pulse_scales_out_settles_back_and_uses_the_shorter_duration() {
        let start = Instant::now();
        let mut effect = NavigationFocusEffect::default();
        effect.queue(
            NavigationFocusTarget::new(0, 0.2, Vec::new())
                .with_motion(NavigationFocusMotion::Pulse),
        );
        assert!(effect.advance(start, true));

        let expanded = effect
            .frame(start + Duration::from_millis(120))
            .expect("pulse should be visible");
        assert!(expanded.scale > 1.035);
        assert!(expanded.intensity > 0.8);

        let settling = effect
            .frame(start + Duration::from_millis(260))
            .expect("pulse should still be settling");
        assert!(settling.scale < 1.0);

        let end = start + NAVIGATION_FOCUS_PULSE_DURATION;
        assert!(!effect.advance(end, true));
        assert_eq!(effect.frame(end), None);
    }
}
