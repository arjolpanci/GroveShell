//! Interpolation and easing: the arithmetic every progress-driven
//! surface shares.

/// Linear interpolation. Deliberately does **not** clamp `t` — the
/// `Adjustment` clamps its own value, and leaving this open keeps an
/// overshooting curve usable later.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// A rectangle in f64, in whatever coordinate space the caller is using.
///
/// f64 rather than Win32's integer `RECT` on purpose: a transition
/// interpolates these every frame for 20-30 frames, and rounding at each
/// step accumulates visible drift. Callers round once, at the Win32 or
/// D2D boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl RectF {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self { left, top, right, bottom }
    }

    /// From an origin and a size, which is how most layout code thinks.
    pub fn from_size(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self { left, top, right: left + width, bottom: top + height }
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }

    pub fn center(&self) -> (f64, f64) {
        ((self.left + self.right) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

/// Edge-wise interpolation, matching `Clutter.ActorBox.interpolate`
/// (`overviewControls.js:215`).
pub fn lerp_rect(a: RectF, b: RectF, t: f64) -> RectF {
    RectF {
        left: lerp(a.left, b.left, t),
        top: lerp(a.top, b.top, t),
        right: lerp(a.right, b.right, t),
        bottom: lerp(a.bottom, b.bottom, t),
    }
}

/// The easing curves the overview uses. GNOME picks a different one per
/// direction and the asymmetry is deliberate: opening eases out of a
/// sine (`overviewControls.js:724`), closing out of a quad
/// (`overviewControls.js:742`), and a released fling lands on a cubic
/// (`overviewControls.js:791`) because `DURATION_MULTIPLIER = 3` is that
/// curve's derivative at t=0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    OutQuad,
    OutCubic,
    OutSine,
}

impl Easing {
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::OutCubic => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            Easing::OutSine => (t * std::f64::consts::FRAC_PI_2).sin(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_hits_both_endpoints() {
        assert_eq!(lerp(10.0, 20.0, 0.0), 10.0);
        assert_eq!(lerp(10.0, 20.0, 1.0), 20.0);
        assert_eq!(lerp(10.0, 20.0, 0.5), 15.0);
    }

    #[test]
    fn lerp_extrapolates_rather_than_clamping() {
        // The adjustment clamps; lerp must not, so an overshoot curve
        // (a future spring) keeps working.
        assert_eq!(lerp(0.0, 10.0, 1.5), 15.0);
    }

    #[test]
    fn lerp_rect_interpolates_every_edge() {
        let a = RectF::new(0.0, 0.0, 100.0, 50.0);
        let b = RectF::new(10.0, 20.0, 110.0, 120.0);
        let mid = lerp_rect(a, b, 0.5);
        assert_eq!(mid, RectF::new(5.0, 10.0, 105.0, 85.0));
    }

    #[test]
    fn rect_reports_size_and_centre() {
        let r = RectF::from_size(10.0, 20.0, 100.0, 40.0);
        assert_eq!(r.right, 110.0);
        assert_eq!(r.bottom, 60.0);
        assert_eq!(r.width(), 100.0);
        assert_eq!(r.height(), 40.0);
        assert_eq!(r.center(), (60.0, 40.0));
    }

    #[test]
    fn every_easing_mode_hits_its_endpoints() {
        for mode in [Easing::Linear, Easing::OutQuad, Easing::OutCubic, Easing::OutSine] {
            assert_eq!(mode.apply(0.0), 0.0, "{mode:?} at 0");
            assert!((mode.apply(1.0) - 1.0).abs() < 1e-9, "{mode:?} at 1");
        }
    }

    #[test]
    fn ease_out_modes_lead_linear_at_the_midpoint() {
        for mode in [Easing::OutQuad, Easing::OutCubic, Easing::OutSine] {
            assert!(mode.apply(0.5) > 0.5, "{mode:?} should be ahead of linear");
        }
    }

    #[test]
    fn easing_clamps_out_of_range_input() {
        assert_eq!(Easing::OutCubic.apply(-1.0), 0.0);
        assert_eq!(Easing::OutCubic.apply(2.0), 1.0);
    }
}
