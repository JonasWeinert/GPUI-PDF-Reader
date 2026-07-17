use crate::model::{DocumentLayout, TextBounds};
use crate::navigation_focus::{NavigationFocusMotion, NavigationFocusTarget, NavigationFocusTone};

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentJump {
    page: usize,
    x_fraction: Option<f32>,
    y_fraction: Option<f32>,
    viewport_anchor_y: f32,
    center_horizontal: bool,
    focus_runs: Vec<TextBounds>,
    focus_tone: NavigationFocusTone,
    focus_motion: NavigationFocusMotion,
    show_focus: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDocumentJump {
    pub x: f32,
    pub y: f32,
    pub focus: Option<NavigationFocusTarget>,
}

impl DocumentJump {
    pub fn new(page: usize) -> Self {
        Self {
            page,
            x_fraction: None,
            y_fraction: None,
            viewport_anchor_y: 0.5,
            center_horizontal: false,
            focus_runs: Vec::new(),
            focus_tone: NavigationFocusTone::Accent,
            focus_motion: NavigationFocusMotion::Sweep,
            show_focus: false,
        }
    }

    pub fn position(mut self, x_fraction: Option<f32>, y_fraction: Option<f32>) -> Self {
        self.x_fraction = finite_fraction(x_fraction);
        self.y_fraction = finite_fraction(y_fraction);
        self
    }

    pub fn viewport_anchor_y(mut self, anchor: f32) -> Self {
        if anchor.is_finite() {
            self.viewport_anchor_y = anchor.clamp(0.0, 1.0);
        }
        self
    }

    pub fn center_horizontal(mut self, center: bool) -> Self {
        self.center_horizontal = center;
        self
    }

    pub fn focus(
        mut self,
        runs: Vec<TextBounds>,
        tone: NavigationFocusTone,
        motion: NavigationFocusMotion,
    ) -> Self {
        self.focus_runs = runs;
        self.focus_tone = tone;
        self.focus_motion = motion;
        self.show_focus = true;
        self
    }

    pub fn resolve(
        &self,
        layout: &DocumentLayout,
        current_x: f32,
        viewport_width: f32,
        viewport_height: f32,
        max_x: f32,
    ) -> Option<ResolvedDocumentJump> {
        let page = layout.page_rect(self.page)?;
        let maximum_y = (layout.content_height - viewport_height).max(0.0);
        let x = if self.center_horizontal {
            self.x_fraction.map_or(current_x, |fraction| {
                page.x + page.width * fraction - viewport_width * 0.5
            })
        } else {
            current_x
        }
        .clamp(0.0, max_x.max(0.0));
        let y = self.y_fraction.map_or(page.y, |fraction| {
            page.y + page.height * fraction - viewport_height * self.viewport_anchor_y
        });
        let y = y.clamp(0.0, maximum_y);
        let focus = self.show_focus.then(|| {
            NavigationFocusTarget::new(
                self.page,
                self.y_fraction.unwrap_or(0.0),
                self.focus_runs.clone(),
            )
            .with_tone(self.focus_tone)
            .with_motion(self.focus_motion)
        });
        Some(ResolvedDocumentJump { x, y, focus })
    }
}

fn finite_fraction(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageSize;

    fn layout() -> DocumentLayout {
        DocumentLayout::new(
            &[
                PageSize {
                    width: 600.0,
                    height: 800.0,
                },
                PageSize {
                    width: 400.0,
                    height: 500.0,
                },
            ],
            1.0,
            900.0,
        )
    }

    #[test]
    fn known_positions_resolve_scroll_and_focus_together() {
        let layout = layout();
        let page = layout.page_rect(1).unwrap();
        let run = TextBounds {
            left: 0.2,
            top: 0.4,
            right: 0.5,
            bottom: 0.45,
        };
        let resolved = DocumentJump::new(1)
            .position(Some(0.75), Some(0.4))
            .viewport_anchor_y(0.35)
            .center_horizontal(true)
            .focus(
                vec![run],
                NavigationFocusTone::SearchMatch,
                NavigationFocusMotion::Pulse,
            )
            .resolve(&layout, 12.0, 300.0, 240.0, 500.0)
            .unwrap();

        assert!((resolved.x - (page.x + page.width * 0.75 - 150.0)).abs() < 0.001);
        assert!((resolved.y - (page.y + page.height * 0.4 - 84.0)).abs() < 0.001);
        let focus = resolved.focus.unwrap();
        assert_eq!(focus.page, 1);
        assert_eq!(focus.text_runs, vec![run]);
        assert_eq!(focus.tone, NavigationFocusTone::SearchMatch);
        assert_eq!(focus.motion, NavigationFocusMotion::Pulse);
    }

    #[test]
    fn page_only_jumps_top_align_and_preserve_horizontal_scroll() {
        let layout = layout();
        let page = layout.page_rect(1).unwrap();
        let resolved = DocumentJump::new(1)
            .position(Some(f32::NAN), Some(f32::INFINITY))
            .resolve(&layout, 42.0, 300.0, 240.0, 500.0)
            .unwrap();
        assert_eq!(resolved.x, 42.0);
        assert_eq!(resolved.y, page.y);
        assert_eq!(resolved.focus, None);
    }
}
