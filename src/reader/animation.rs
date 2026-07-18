/// A bounded scalar transition with exponential smoothing.
///
/// UI owners decide the response rate, while this primitive centralizes the
/// settling threshold, frame-time bounds, and value clamping used throughout
/// the reader.
#[derive(Clone, Copy, Debug)]
pub(super) struct UnitTransition {
    pub(super) progress: f32,
    pub(super) target: f32,
}

impl Default for UnitTransition {
    fn default() -> Self {
        Self {
            progress: 0.0,
            target: 0.0,
        }
    }
}

impl UnitTransition {
    const SETTLE_EPSILON: f32 = 0.001;

    pub(super) fn is_animating(self) -> bool {
        (self.progress - self.target).abs() > Self::SETTLE_EPSILON
    }

    pub(super) fn advance(&mut self, dt: f32) {
        self.advance_with_response(dt, 22.0);
    }

    pub(super) fn advance_with_response(&mut self, dt: f32, response: f32) {
        let blend = 1.0 - (-response * dt.clamp(1.0 / 240.0, 0.05)).exp();
        self.progress += (self.target - self.progress) * blend;
        if (self.target - self.progress).abs() < Self::SETTLE_EPSILON {
            self.progress = self.target;
        }
        self.progress = self.progress.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::UnitTransition;

    #[test]
    fn transition_settles_and_stays_bounded() {
        let mut transition = UnitTransition {
            progress: 0.0,
            target: 1.0,
        };
        for _ in 0..240 {
            transition.advance(1.0 / 60.0);
            assert!((0.0..=1.0).contains(&transition.progress));
        }
        assert_eq!(transition.progress, 1.0);
        assert!(!transition.is_animating());

        transition.target = 0.0;
        transition.advance_with_response(1.0 / 60.0, 24.0);
        assert!(transition.progress < 1.0);
        assert!(transition.is_animating());
    }
}
