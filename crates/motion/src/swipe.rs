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
