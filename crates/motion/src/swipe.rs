//! Swipe-gesture physics, ported from GNOME Shell's
//! `js/ui/swipeTracker.js`. Constants are GNOME's, deliberately: they
//! are tuned against real touchpads, and the feel of a fling is the
//! whole point of using them rather than inventing numbers.

use crate::lerp::Easing;

/// Samples older than this are dropped before velocity is computed, so
/// a slow drag that ends with a flick reports the flick.
pub const EVENT_HISTORY_THRESHOLD_MS: u64 = 150;

/// Pixels per wheel notch. `WM_MOUSEWHEEL` reports multiples (and, on a
/// precision touchpad, fractions) of `WHEEL_DELTA`; this is the factor
/// GNOME applies to turn scroll units into gesture pixels.
pub const SCROLL_MULTIPLIER: f64 = 10.0;

const MIN_ANIMATION_DURATION_MS: f64 = 100.0;
const MAX_ANIMATION_DURATION_MS: f64 = 400.0;
const VELOCITY_THRESHOLD_TOUCH: f64 = 0.3;
const VELOCITY_THRESHOLD_TOUCHPAD: f64 = 0.6;
const DECELERATION_TOUCH: f64 = 0.998;
const DECELERATION_TOUCHPAD: f64 = 0.997;
const VELOCITY_CURVE_THRESHOLD: f64 = 2.0;
const DECELERATION_PARABOLA_MULTIPLIER: f64 = 0.35;
/// The derivative of ease-out-cubic at t=0, which is why a landing
/// animation must use `Easing::OutCubic` for this to be the right factor.
const DURATION_MULTIPLIER: f64 = 3.0;
const ANIMATION_BASE_VELOCITY: f64 = 0.002;
const EPSILON: f64 = 0.005;

/// A rolling window of recent gesture deltas, used to turn a stream of
/// scroll events into one velocity at release.
pub struct EventHistory {
    samples: Vec<(u64, f64)>,
}

impl Default for EventHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHistory {
    pub fn new() -> Self {
        Self { samples: Vec::new() }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Drops samples older than the threshold relative to `time_ms`.
    pub fn trim(&mut self, time_ms: u64) {
        let cutoff = time_ms.saturating_sub(EVENT_HISTORY_THRESHOLD_MS);
        match self.samples.iter().position(|(t, _)| *t >= cutoff) {
            Some(first_kept) => {
                self.samples.drain(..first_kept);
            }
            None => self.samples.clear(),
        }
    }

    pub fn append(&mut self, time_ms: u64, delta: f64) {
        self.trim(time_ms);
        self.samples.push((time_ms, delta));
    }

    /// Pixels per millisecond over the window. The first sample's delta
    /// is excluded — it describes motion that happened *before* the
    /// window opened — and an empty span reports zero rather than
    /// dividing by it.
    pub fn velocity(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }
        let first_time = self.samples[0].0;
        let last_time = self.samples[self.samples.len() - 1].0;
        if first_time == last_time {
            return 0.0;
        }
        let total: f64 = self.samples[1..].iter().map(|(_, d)| *d).sum();
        total / (last_time - first_time) as f64
    }
}

/// Which device the gesture came from — they get different velocity
/// thresholds and deceleration, because a finger on glass and a finger
/// on a touchpad do not behave alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Touchpad,
    Touch,
}

impl InputKind {
    fn velocity_threshold(self) -> f64 {
        match self {
            InputKind::Touchpad => VELOCITY_THRESHOLD_TOUCHPAD,
            InputKind::Touch => VELOCITY_THRESHOLD_TOUCH,
        }
    }

    fn deceleration(self) -> f64 {
        match self {
            InputKind::Touchpad => DECELERATION_TOUCHPAD,
            InputKind::Touch => DECELERATION_TOUCH,
        }
    }
}

/// Where a released gesture should land, and how long it should take to
/// get there. `easing` is `OutCubic` because `DURATION_MULTIPLIER` is
/// that curve's derivative at zero — using any other curve here makes
/// the hand-off from finger to animation visibly jerk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwipeEnd {
    pub duration_ms: u32,
    pub end_progress: f64,
    pub easing: Easing,
}

pub struct SwipeTracker {
    kind: InputKind,
    /// Snap points in ascending order. Empty until `confirm_swipe`.
    snap_points: Vec<f64>,
    distance: f64,
    progress: f64,
    initial_progress: f64,
    cancel_progress: f64,
    scrolling: bool,
    cancelled: bool,
    history: EventHistory,
    /// When false (the default), a gesture may not travel more than one
    /// snap point beyond where it started.
    pub allow_long_swipes: bool,
}

impl SwipeTracker {
    pub fn new(kind: InputKind) -> Self {
        Self {
            kind,
            snap_points: Vec::new(),
            distance: 1.0,
            progress: 0.0,
            initial_progress: 0.0,
            cancel_progress: 0.0,
            scrolling: false,
            cancelled: false,
            history: EventHistory::new(),
            allow_long_swipes: false,
        }
    }

    /// Arms the tracker. `cancel` must be a snap point or another
    /// non-transient value: it is where a cancelled gesture returns to,
    /// and when a gesture interrupts a running transition it should be
    /// that transition's target rather than the live value.
    pub fn confirm_swipe(
        &mut self,
        distance: f64,
        snap_points: Vec<f64>,
        current: f64,
        cancel: f64,
    ) {
        self.distance = if distance.abs() < f64::EPSILON { 1.0 } else { distance };
        self.snap_points = snap_points;
        self.progress = current;
        self.initial_progress = current;
        self.cancel_progress = cancel;
        self.cancelled = false;
        self.history.reset();
        self.scrolling = true;
    }

    pub fn is_scrolling(&self) -> bool {
        self.scrolling
    }

    pub fn progress(&self) -> f64 {
        self.progress
    }

    /// Feeds one gesture delta in pixels, returning the new progress.
    pub fn update(&mut self, delta_px: f64) -> f64 {
        if !self.scrolling {
            return self.progress;
        }
        self.progress += delta_px / self.distance;
        let (lower, upper) = self.bounds(self.initial_progress);
        self.progress = self.progress.clamp(lower, upper);
        self.progress
    }

    /// `update`, plus recording the sample so `end_from_history` can
    /// recover a velocity at release.
    pub fn append_event(&mut self, time_ms: u64, delta_px: f64) -> f64 {
        self.history.append(time_ms, delta_px);
        self.update(delta_px)
    }

    pub fn end_from_history(&mut self, now_ms: u64) -> SwipeEnd {
        self.history.trim(now_ms);
        let velocity = self.history.velocity();
        self.end(velocity)
    }

    pub fn cancel(&mut self) -> SwipeEnd {
        self.cancelled = true;
        self.end(0.0)
    }

    /// Releases the gesture at `velocity` (pixels per millisecond).
    pub fn end(&mut self, velocity: f64) -> SwipeEnd {
        let end_progress = self.end_progress(velocity);

        // From here on velocity is in progress-units per ms, matching
        // the quantity the duration is derived from.
        let mut velocity = velocity / self.distance;
        if (end_progress - self.progress) * velocity <= 0.0 {
            velocity = ANIMATION_BASE_VELOCITY;
        }

        let n_points = (self.progress - end_progress).abs().ceil().max(1.0);
        let max_duration = MAX_ANIMATION_DURATION_MS * (1.0 + n_points).log2();
        let mut duration = ((self.progress - end_progress) / velocity * DURATION_MULTIPLIER).abs();
        if duration > 0.0 {
            duration = duration.clamp(
                MIN_ANIMATION_DURATION_MS,
                max_duration.max(MIN_ANIMATION_DURATION_MS),
            );
        }

        self.reset();

        SwipeEnd {
            duration_ms: duration.round() as u32,
            end_progress,
            easing: Easing::OutCubic,
        }
    }

    fn reset(&mut self) {
        self.scrolling = false;
        self.snap_points.clear();
        self.initial_progress = 0.0;
        self.cancel_progress = 0.0;
        self.progress = 0.0;
        self.cancelled = false;
        self.history.reset();
    }

    fn closest_point(&self, pos: f64) -> usize {
        let mut best = 0;
        let mut best_distance = f64::INFINITY;
        for (i, p) in self.snap_points.iter().enumerate() {
            let d = (p - pos).abs();
            if d < best_distance {
                best_distance = d;
                best = i;
            }
        }
        best
    }

    fn next_point(&self, pos: f64) -> usize {
        self.snap_points
            .iter()
            .position(|p| *p >= pos)
            .unwrap_or(self.snap_points.len().saturating_sub(1))
    }

    fn previous_point(&self, pos: f64) -> usize {
        self.snap_points.iter().rposition(|p| *p <= pos).unwrap_or(0)
    }

    fn point_for_projection(&self, pos: f64, velocity: f64) -> usize {
        let initial = self.closest_point(self.initial_progress);
        let prev = self.previous_point(pos);
        let next = self.next_point(pos);

        if (if velocity > 0.0 { prev } else { next }) == initial {
            return if velocity > 0.0 { next } else { prev };
        }
        self.closest_point(pos)
    }

    /// The range a gesture may reach, given where it started. Without
    /// `allow_long_swipes` this is one snap point either side, which is
    /// what stops a hard flick from skipping pages
    /// (`swipeTracker.js:547`).
    fn bounds(&self, pos: f64) -> (f64, f64) {
        if self.snap_points.is_empty() {
            return (pos, pos);
        }
        if self.allow_long_swipes {
            return (self.snap_points[0], self.snap_points[self.snap_points.len() - 1]);
        }

        let closest = self.closest_point(pos);
        let (prev, next) = if (self.snap_points[closest] - pos).abs() < EPSILON {
            (closest, closest)
        } else {
            (self.previous_point(pos), self.next_point(pos))
        };

        let lower = prev.saturating_sub(1);
        let upper = (next + 1).min(self.snap_points.len() - 1);
        (self.snap_points[lower], self.snap_points[upper])
    }

    /// Where the gesture lands: the nearest point below the velocity
    /// threshold, otherwise a projection along a deceleration curve
    /// (`swipeTracker.js:596`).
    fn end_progress(&self, velocity: f64) -> f64 {
        if self.snap_points.is_empty() {
            return self.progress;
        }
        if self.cancelled {
            return self.cancel_progress;
        }
        if velocity.abs() < self.kind.velocity_threshold() {
            return self.snap_points[self.closest_point(self.progress)];
        }

        let decel = self.kind.deceleration();
        let slope = decel / (1.0 - decel) / 1000.0;

        let mut pos = if velocity.abs() > VELOCITY_CURVE_THRESHOLD {
            let c = slope / 2.0 / DECELERATION_PARABOLA_MULTIPLIER;
            let x = velocity.abs() - VELOCITY_CURVE_THRESHOLD + c;
            slope * VELOCITY_CURVE_THRESHOLD + DECELERATION_PARABOLA_MULTIPLIER * x * x
                - DECELERATION_PARABOLA_MULTIPLIER * c * c
        } else {
            velocity.abs() * slope
        };

        pos = pos * velocity.signum() + self.progress;
        let (lower, upper) = self.bounds(self.initial_progress);
        pos = pos.clamp(lower, upper);

        self.snap_points[self.point_for_projection(pos, velocity)]
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn an_empty_history_has_no_velocity() {
        assert_eq!(EventHistory::new().velocity(), 0.0);
    }

    #[test]
    fn a_single_sample_has_no_velocity() {
        let mut h = EventHistory::new();
        h.append(1000, 12.0);
        assert_eq!(h.velocity(), 0.0);
    }

    #[test]
    fn velocity_is_total_delta_over_the_period_excluding_the_first_sample() {
        let mut h = EventHistory::new();
        // GNOME drops the first sample's delta and divides by the span
        // from first to last (`swipeTracker.js:71`).
        h.append(1000, 0.0);
        h.append(1010, 10.0);
        h.append(1020, 10.0);
        assert!((h.velocity() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn identical_timestamps_do_not_divide_by_zero() {
        let mut h = EventHistory::new();
        h.append(1000, 5.0);
        h.append(1000, 5.0);
        assert_eq!(h.velocity(), 0.0);
    }

    #[test]
    fn appending_trims_samples_older_than_the_threshold() {
        let mut h = EventHistory::new();
        h.append(1000, 100.0);
        h.append(1000 + EVENT_HISTORY_THRESHOLD_MS + 50, 0.0);
        h.append(1000 + EVENT_HISTORY_THRESHOLD_MS + 60, 10.0);
        // The 100.0 sample aged out, so it cannot inflate the fling.
        assert!((h.velocity() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reset_clears_everything() {
        let mut h = EventHistory::new();
        h.append(1000, 1.0);
        h.append(1010, 1.0);
        h.reset();
        assert_eq!(h.velocity(), 0.0);
    }
}

#[cfg(test)]
mod tracker_tests {
    use super::*;

    fn overview_tracker() -> SwipeTracker {
        let mut t = SwipeTracker::new(InputKind::Touchpad);
        t.confirm_swipe(1000.0, vec![0.0, 1.0], 0.0, 0.0);
        t
    }

    fn carousel_tracker(at: f64) -> SwipeTracker {
        let mut t = SwipeTracker::new(InputKind::Touchpad);
        t.confirm_swipe(500.0, vec![0.0, 1.0, 2.0, 3.0], at, at);
        t
    }

    #[test]
    fn an_unconfirmed_tracker_ignores_updates() {
        let mut t = SwipeTracker::new(InputKind::Touchpad);
        assert!(!t.is_scrolling());
        assert_eq!(t.update(100.0), 0.0);
    }

    #[test]
    fn update_converts_pixels_to_progress_over_the_distance() {
        let mut t = overview_tracker();
        assert!((t.update(250.0) - 0.25).abs() < 1e-9);
        assert!((t.update(250.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn update_clamps_to_the_outer_snap_points() {
        let mut t = overview_tracker();
        t.update(5000.0);
        assert_eq!(t.progress(), 1.0);
        t.update(-9000.0);
        assert_eq!(t.progress(), 0.0);
    }

    #[test]
    fn bounds_allow_only_one_point_either_side_of_the_start() {
        let mut t = carousel_tracker(1.0);
        t.update(10_000.0);
        assert_eq!(t.progress(), 2.0, "should stop one page past the start");
    }

    #[test]
    fn long_swipes_may_cross_the_whole_range_when_allowed() {
        let mut t = carousel_tracker(1.0);
        t.allow_long_swipes = true;
        t.update(10_000.0);
        assert_eq!(t.progress(), 3.0);
    }

    #[test]
    fn a_slow_release_snaps_to_the_nearest_point() {
        let mut t = overview_tracker();
        t.update(300.0);
        let end = t.end(0.0);
        assert_eq!(end.end_progress, 0.0, "below the velocity threshold, nearest wins");

        let mut t = overview_tracker();
        t.update(700.0);
        assert_eq!(t.end(0.0).end_progress, 1.0);
    }

    #[test]
    fn a_flick_carries_past_the_nearest_point() {
        let mut t = overview_tracker();
        t.update(200.0);
        let end = t.end(3.0);
        assert_eq!(end.end_progress, 1.0, "momentum should finish the open");
    }

    #[test]
    fn a_flick_backwards_returns_to_the_start() {
        let mut t = overview_tracker();
        t.update(800.0);
        let end = t.end(-3.0);
        assert_eq!(end.end_progress, 0.0);
    }

    #[test]
    fn the_landing_duration_is_clamped_and_uses_the_cubic_curve() {
        let mut t = overview_tracker();
        t.update(500.0);
        let end = t.end(50.0);
        assert!(end.duration_ms >= MIN_ANIMATION_DURATION_MS as u32);
        assert!(end.duration_ms <= MAX_ANIMATION_DURATION_MS as u32);
        assert_eq!(end.easing, Easing::OutCubic);
    }

    #[test]
    fn a_release_with_no_movement_still_animates_for_a_sane_time() {
        let mut t = overview_tracker();
        t.update(500.0);
        let end = t.end(0.0);
        assert!(end.duration_ms >= MIN_ANIMATION_DURATION_MS as u32);
    }

    #[test]
    fn cancel_returns_to_the_cancel_point_not_the_nearest_one() {
        let mut t = SwipeTracker::new(InputKind::Touchpad);
        // Review Focus 5: a gesture that began while a transition was
        // already heading to 1.0 must cancel to 1.0, not back to 0.0.
        t.confirm_swipe(1000.0, vec![0.0, 1.0], 0.1, 1.0);
        t.update(50.0);
        let end = t.cancel();
        assert_eq!(end.end_progress, 1.0);
    }

    #[test]
    fn ending_resets_the_tracker_for_the_next_gesture() {
        let mut t = overview_tracker();
        t.update(500.0);
        let _ = t.end(0.0);
        assert!(!t.is_scrolling());
        assert_eq!(t.update(100.0), 0.0, "a reset tracker ignores updates");
    }

    #[test]
    fn end_from_history_uses_the_appended_events() {
        let mut t = overview_tracker();
        t.append_event(1000, 0.0);
        t.append_event(1010, 30.0);
        t.append_event(1020, 30.0);
        // 60px over 20ms = 3 px/ms, a clear flick.
        let end = t.end_from_history(1020);
        assert_eq!(end.end_progress, 1.0);
    }
}
