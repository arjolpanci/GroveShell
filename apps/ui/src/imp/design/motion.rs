//! Motion tokens (spec §3.3): shared easing curves and named durations,
//! all gated by `reduced_motion` and scaled by `animation_scale` so every
//! surface animates with one consistent feel. Built on the existing
//! `state::animation_config()` mirror.

use super::super::state;

/// Duration for small, snappy transitions (hover highlights, presses).
pub(crate) const FAST_MS: u32 = 180;

/// Duration for larger transitions (flyouts opening, dock reveal).
pub(crate) const BASE_MS: u32 = 250;

/// Cubic ease-out: fast start, gentle settle. `f(0)=0`, `f(1)=1`.
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// Cubic ease-in-out: gentle at both ends. `f(0)=0`, `f(0.5)=0.5`, `f(1)=1`.
pub(crate) fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

/// The effective duration of a named transition given the live animation
/// config: zero when reduced motion is on (so transitions resolve
/// instantly), otherwise the named value times `animation_scale`.
pub(crate) fn effective_ms(named: u32) -> u32 {
    let (scale, reduced) = state::animation_config();
    effective_ms_with(named, reduced, scale)
}

/// Pure core of [`effective_ms`], split out so the scaling/reduced-motion
/// rule is unit-testable without touching thread-local state.
pub(crate) fn effective_ms_with(named: u32, reduced: bool, scale: f32) -> u32 {
    if reduced {
        0
    } else {
        (named as f32 * scale).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_hits_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
    }

    #[test]
    fn ease_out_cubic_is_ahead_of_linear_midway() {
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn ease_out_cubic_clamps_out_of_range_input() {
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
    }

    #[test]
    fn ease_in_out_cubic_is_symmetric_around_midpoint() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
    }

    #[test]
    fn reduced_motion_zeroes_duration() {
        assert_eq!(effective_ms_with(BASE_MS, true, 1.0), 0);
    }

    #[test]
    fn scale_multiplies_duration() {
        assert_eq!(effective_ms_with(BASE_MS, false, 2.0), BASE_MS * 2);
        assert_eq!(effective_ms_with(FAST_MS, false, 1.0), FAST_MS);
    }
}
