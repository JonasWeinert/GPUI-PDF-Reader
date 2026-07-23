/// A bounded `[0, 1]` transition using frame-rate-independent exponential
/// smoothing.
///
/// The transition is intentionally independent of GPUI. Owners decide when to
/// request another animation frame and use [`is_animating`](Self::is_animating)
/// to stop doing so once the value has settled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitTransition {
    value: f32,
    target: f32,
}

impl Default for UnitTransition {
    /// Creates a transition settled at zero.
    fn default() -> Self {
        Self::hidden()
    }
}

impl UnitTransition {
    /// Response used by [`advance`](Self::advance).
    pub const DEFAULT_RESPONSE: f32 = 22.0;

    /// Distance at which a transition snaps exactly to its target.
    pub const SETTLE_EPSILON: f32 = 0.001;

    const MIN_FRAME_TIME: f32 = 1.0 / 240.0;
    const MAX_FRAME_TIME: f32 = 0.05;

    /// Creates a transition settled at `value`.
    ///
    /// Values outside the unit interval are clamped. Non-finite values become
    /// zero so malformed timing or persisted UI state cannot poison layout.
    #[must_use]
    pub fn new(value: f32) -> Self {
        let value = sanitize_unit(value);
        Self {
            value,
            target: value,
        }
    }

    /// Creates a transition settled at zero.
    #[must_use]
    pub const fn hidden() -> Self {
        Self {
            value: 0.0,
            target: 0.0,
        }
    }

    /// Creates a transition settled at one.
    #[must_use]
    pub const fn visible() -> Self {
        Self {
            value: 1.0,
            target: 1.0,
        }
    }

    /// Returns the current bounded value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }

    /// Returns the bounded destination value.
    #[must_use]
    pub const fn target(self) -> f32 {
        self.target
    }

    /// Changes the destination without changing the current value.
    pub fn set_target(&mut self, target: f32) {
        self.target = sanitize_unit(target);
    }

    /// Targets one when `visible` and zero otherwise.
    pub fn set_visible(&mut self, visible: bool) {
        self.target = f32::from(visible);
    }

    /// Reverses the current destination.
    pub fn toggle(&mut self) {
        self.target = if self.target > 0.5 { 0.0 } else { 1.0 };
    }

    /// Immediately settles both the value and destination at `value`.
    pub fn snap_to(&mut self, value: f32) {
        let value = sanitize_unit(value);
        self.value = value;
        self.target = value;
    }

    /// Returns whether another animation step is required.
    #[must_use]
    pub fn is_animating(self) -> bool {
        (self.value - self.target).abs() > Self::SETTLE_EPSILON
    }

    /// Advances with [`DEFAULT_RESPONSE`](Self::DEFAULT_RESPONSE).
    ///
    /// The return value is `true` while another frame is required.
    pub fn advance(&mut self, elapsed_seconds: f32) -> bool {
        self.advance_with_response(elapsed_seconds, Self::DEFAULT_RESPONSE)
    }

    /// Advances with a caller-selected exponential response.
    ///
    /// Invalid, zero, or negative elapsed times and responses are ignored.
    /// Frame time is bounded to avoid a delayed frame visibly teleporting a
    /// component or an extremely small frame stalling progress.
    ///
    /// The return value is `true` while another frame is required.
    pub fn advance_with_response(&mut self, elapsed_seconds: f32, response: f32) -> bool {
        if !self.is_animating() {
            self.value = self.target;
            return false;
        }
        if !elapsed_seconds.is_finite()
            || elapsed_seconds <= 0.0
            || !response.is_finite()
            || response <= 0.0
        {
            return true;
        }

        let elapsed_seconds = elapsed_seconds.clamp(Self::MIN_FRAME_TIME, Self::MAX_FRAME_TIME);
        let blend = 1.0 - (-response * elapsed_seconds).exp();
        self.value += (self.target - self.value) * blend;
        self.value = sanitize_unit(self.value);
        if (self.target - self.value).abs() <= Self::SETTLE_EPSILON {
            self.value = self.target;
        }
        self.is_animating()
    }

    /// Advances using a resolved design-system response. `None` represents a
    /// reduced-motion policy and settles immediately at the current target.
    pub fn advance_with_optional_response(
        &mut self,
        elapsed_seconds: f32,
        response: Option<f32>,
    ) -> bool {
        if let Some(response) = response {
            self.advance_with_response(elapsed_seconds, response)
        } else {
            self.value = self.target;
            false
        }
    }

    /// Interpolates between two scalar values using the current progress.
    #[must_use]
    pub fn interpolate(self, start: f32, end: f32) -> f32 {
        start + (end - start) * self.value
    }
}

fn sanitize_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::UnitTransition;

    #[test]
    fn construction_and_mutation_never_escape_the_unit_interval() {
        assert_eq!(UnitTransition::new(-10.0).value(), 0.0);
        assert_eq!(UnitTransition::new(10.0).value(), 1.0);
        assert_eq!(UnitTransition::new(f32::NAN).value(), 0.0);

        let mut transition = UnitTransition::hidden();
        transition.set_target(3.0);
        assert_eq!(transition.target(), 1.0);
        transition.snap_to(f32::INFINITY);
        assert_eq!(transition, UnitTransition::hidden());
    }

    #[test]
    fn transition_settles_in_both_directions_and_can_reverse_mid_flight() {
        let mut transition = UnitTransition::hidden();
        transition.set_visible(true);
        let mut prior = transition.value();
        for _ in 0..240 {
            transition.advance(1.0 / 60.0);
            assert!(transition.value() >= prior);
            assert!((0.0..=1.0).contains(&transition.value()));
            prior = transition.value();
        }
        assert_eq!(transition.value(), 1.0);
        assert!(!transition.is_animating());

        transition.set_visible(false);
        transition.advance_with_response(1.0 / 60.0, 24.0);
        let closing_value = transition.value();
        assert!(closing_value < 1.0);
        transition.toggle();
        transition.advance_with_response(1.0 / 60.0, 24.0);
        assert!(transition.value() > closing_value);
    }

    #[test]
    fn invalid_timing_does_not_corrupt_or_advance_state() {
        let mut transition = UnitTransition::hidden();
        transition.set_visible(true);
        for elapsed in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(transition.advance(elapsed));
            assert_eq!(transition.value(), 0.0);
        }
        for response in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(transition.advance_with_response(1.0 / 60.0, response));
            assert_eq!(transition.value(), 0.0);
        }
    }

    #[test]
    fn interpolation_uses_current_value() {
        let transition = UnitTransition::new(0.25);
        assert!((transition.interpolate(20.0, 40.0) - 25.0).abs() < f32::EPSILON);
    }

    #[test]
    fn equivalent_elapsed_time_is_nearly_frame_rate_independent() {
        let mut sixty_hz = UnitTransition::hidden();
        let mut one_twenty_hz = UnitTransition::hidden();
        sixty_hz.set_visible(true);
        one_twenty_hz.set_visible(true);
        for _ in 0..30 {
            sixty_hz.advance(1.0 / 60.0);
        }
        for _ in 0..60 {
            one_twenty_hz.advance(1.0 / 120.0);
        }
        assert!((sixty_hz.value() - one_twenty_hz.value()).abs() <= UnitTransition::SETTLE_EPSILON);
    }
}
