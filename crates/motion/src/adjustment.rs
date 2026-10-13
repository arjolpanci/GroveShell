//! The single progress value an overview derives every box, scale and
//! opacity from — a port of the `St.Adjustment` at the centre of GNOME
//! Shell's `js/ui/overviewControls.js`.
//!
//! The point of funnelling everything through one number is that the
//! transition becomes data rather than control flow: it can be set to any
//! point (so a finger can drive it), reversed mid-flight (because nothing
//! holds a start time of its own), and read identically by every surface.

use std::time::{Duration, Instant};

use crate::lerp::{lerp, Easing};

/// A transition in flight. `from` is captured at the value the
/// adjustment held when it started, so reversing mid-transition eases
/// from wherever it actually got to, never from a nominal state.
#[derive(Clone, Copy, Debug)]
struct Transition {
    from: f64,
    to: f64,
    started: Instant,
    duration: Duration,
    easing: Easing,
}

/// What a surface needs to know to interpolate between two states —
/// GNOME's `getStateTransitionParams` (`overviewControls.js:279`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionParams {
    /// A transition is running, or a gesture holds the value.
    pub transitioning: bool,
    pub current: f64,
    /// The bracketing states, direction-aware: opening gives
    /// `(floor, ceil)` and closing `(ceil, floor)`, which is what lets a
    /// reversal interpolate between the right pair instead of snapping.
    pub initial: f64,
    pub final_state: f64,
    /// 0..1 within that bracket.
    pub progress: f64,
}

pub struct Adjustment {
    value: f64,
    lower: f64,
    upper: f64,
    transition: Option<Transition>,
    gesture_in_progress: bool,
}

impl Adjustment {
    pub fn new(lower: f64, upper: f64, value: f64) -> Self {
        Self {
            value: value.clamp(lower, upper),
            lower,
            upper,
            transition: None,
            gesture_in_progress: false,
        }
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    /// Raises the upper bound (the carousel learns its page count only
    /// once the overview is built). Clamps the live value into the new
    /// range so a shrinking workspace list cannot leave it out of bounds.
    pub fn set_bounds(&mut self, lower: f64, upper: f64) {
        self.lower = lower;
        self.upper = upper.max(lower);
        self.value = self.value.clamp(self.lower, self.upper);
    }

    /// Jumps to `v`, cancelling any transition. This is what a gesture
    /// calls every frame, so it must never ease: the finger is the curve.
    pub fn set_value(&mut self, v: f64) {
        self.transition = None;
        self.value = v.clamp(self.lower, self.upper);
    }

    /// Starts easing toward `target`. A zero duration — which is what
    /// `effective_ms` returns under reduced motion — lands immediately
    /// rather than storing a transition nothing could divide by.
    pub fn ease_to_at(&mut self, target: f64, duration_ms: u32, easing: Easing, now: Instant) {
        let target = target.clamp(self.lower, self.upper);
        if duration_ms == 0 {
            self.transition = None;
            self.value = target;
            return;
        }
        self.transition = Some(Transition {
            from: self.value,
            to: target,
            started: now,
            duration: Duration::from_millis(duration_ms as u64),
            easing,
        });
    }

    /// Advances to `now`. Returns whether a transition is still in
    /// flight, which is what the caller's frame timer keys off.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(t) = self.transition else {
            return false;
        };
        let elapsed = now.saturating_duration_since(t.started).as_secs_f64();
        let raw = (elapsed / t.duration.as_secs_f64()).clamp(0.0, 1.0);
        if raw >= 1.0 {
            self.value = t.to;
            self.transition = None;
            return false;
        }
        self.value = lerp(t.from, t.to, t.easing.apply(raw));
        true
    }

    pub fn transition_target(&self) -> Option<f64> {
        self.transition.map(|t| t.to)
    }

    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }

    pub fn clear_transition(&mut self) {
        self.transition = None;
    }

    pub fn set_gesture_in_progress(&mut self, in_progress: bool) {
        self.gesture_in_progress = in_progress;
    }

    pub fn gesture_in_progress(&self) -> bool {
        self.gesture_in_progress
    }

    pub fn transition_params(&self) -> TransitionParams {
        let current = self.value;
        let (mut initial, mut final_state) = match self.transition {
            Some(t) => (t.from, t.to),
            None => (current, current),
        };
        if initial > final_state {
            initial = initial.ceil();
            final_state = final_state.floor();
        } else {
            initial = initial.floor();
            final_state = final_state.ceil();
        }
        let length = (final_state - initial).abs();
        let progress = if length > 0.0 {
            ((current - initial) / length).abs()
        } else {
            1.0
        };
        TransitionParams {
            transitioning: self.transition.is_some() || self.gesture_in_progress,
            current,
            initial,
            final_state,
            progress,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_fresh_adjustment_rests_at_its_value() {
        let adj = Adjustment::new(0.0, 1.0, 0.0);
        assert_eq!(adj.value(), 0.0);
        assert!(!adj.is_transitioning());
        assert_eq!(adj.transition_target(), None);
    }

    #[test]
    fn set_value_clamps_to_the_bounds() {
        let mut adj = Adjustment::new(0.0, 1.0, 0.0);
        adj.set_value(2.5);
        assert_eq!(adj.value(), 1.0);
        adj.set_value(-3.0);
        assert_eq!(adj.value(), 0.0);
    }

    #[test]
    fn a_gesture_setting_a_value_cancels_a_running_transition() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 0.0);
        adj.ease_to_at(1.0, 250, Easing::OutSine, now);
        adj.set_value(0.4);
        assert!(!adj.is_transitioning());
        assert_eq!(adj.value(), 0.4);
    }

    #[test]
    fn tick_advances_along_the_curve_and_completes() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 0.0);
        adj.ease_to_at(1.0, 200, Easing::Linear, now);

        assert!(adj.tick(at(now, 100)));
        assert!((adj.value() - 0.5).abs() < 1e-9);

        assert!(!adj.tick(at(now, 200)));
        assert_eq!(adj.value(), 1.0);
        assert!(!adj.is_transitioning());
    }

    #[test]
    fn tick_is_idle_when_nothing_is_running() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 0.3);
        assert!(!adj.tick(at(now, 16)));
        assert_eq!(adj.value(), 0.3);
    }

    /// Review Focus 3: reduced motion makes every duration zero.
    #[test]
    fn a_zero_duration_transition_lands_immediately_without_dividing_by_zero() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 0.0);
        adj.ease_to_at(1.0, 0, Easing::OutSine, now);
        assert_eq!(adj.value(), 1.0);
        assert!(!adj.is_transitioning());
        assert!(!adj.tick(now));
        assert_eq!(adj.value(), 1.0);
    }

    #[test]
    fn transition_params_report_the_bracketing_pair_while_opening() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 0.0);
        adj.ease_to_at(1.0, 200, Easing::Linear, now);
        adj.tick(at(now, 50));

        let p = adj.transition_params();
        assert!(p.transitioning);
        assert_eq!(p.initial, 0.0);
        assert_eq!(p.final_state, 1.0);
        assert!((p.progress - 0.25).abs() < 1e-9);
    }

    #[test]
    fn transition_params_flip_the_bracket_when_closing() {
        let now = Instant::now();
        let mut adj = Adjustment::new(0.0, 1.0, 1.0);
        adj.ease_to_at(0.0, 200, Easing::Linear, now);
        adj.tick(at(now, 50));

        let p = adj.transition_params();
        // Closing: initial is ceil'd and final floor'd, so the pair is
        // still (1, 0) rather than (0, 1) — `overviewControls.js:289`.
        assert_eq!(p.initial, 1.0);
        assert_eq!(p.final_state, 0.0);
        assert!((p.progress - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_resting_adjustment_reports_full_progress_and_no_transition() {
        let adj = Adjustment::new(0.0, 1.0, 1.0);
        let p = adj.transition_params();
        assert!(!p.transitioning);
        assert_eq!(p.current, 1.0);
        assert_eq!(p.progress, 1.0);
    }

    #[test]
    fn a_gesture_counts_as_transitioning_even_with_no_transition_object() {
        let mut adj = Adjustment::new(0.0, 1.0, 0.5);
        adj.set_gesture_in_progress(true);
        assert!(adj.transition_params().transitioning);
        assert!(adj.gesture_in_progress());
    }
}
