//! One shared open/close lifecycle for the shell's pop-up surfaces (spec
//! §5): Quick Settings and the calendar both drive their open and close
//! through this, so the two flyouts animate as one system rather than
//! each inventing its own timing.
//!
//! Both currently use [`Flyout::reveal_extent`] — they unroll downward
//! from the bar's edge by animating window height, which needs no
//! per-pixel alpha. [`Flyout::scale_opacity`] is the alternative for a
//! surface that scales and fades instead; it is kept because that is the
//! right motion for a surface anchored somewhere other than a screen
//! edge, and the session menu ships as a native `TrackPopupMenu`.
//!
//! Motion is gated by `reduced_motion` (via `design::motion`): a zero
//! duration collapses `Opening`/`Closing` straight to their terminal state
//! so there is never a stuck half-open frame.

use std::time::{Duration, Instant};

use super::design::motion::{self, BASE_MS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FlyoutPhase {
    Hidden,
    Opening,
    Open,
    Closing,
}

pub(crate) struct Flyout {
    pub(crate) phase: FlyoutPhase,
    started: Instant,
    duration: Duration,
}

impl Flyout {
    pub(crate) fn new() -> Self {
        Self { phase: FlyoutPhase::Hidden, started: Instant::now(), duration: Duration::ZERO }
    }

    /// Begins opening. With reduced motion (zero effective duration) this
    /// lands directly in `Open`.
    pub(crate) fn open(&mut self) {
        self.begin(FlyoutPhase::Opening, FlyoutPhase::Open);
    }

    /// Begins closing. With reduced motion this lands directly in `Hidden`.
    pub(crate) fn close(&mut self) {
        if self.phase == FlyoutPhase::Hidden {
            return;
        }
        self.begin(FlyoutPhase::Closing, FlyoutPhase::Hidden);
    }

    /// Opens with no animation regardless of config (used where an instant
    /// appearance is wanted).
    #[allow(dead_code, reason = "both flyouts animate; kept for a surface that should not")]
    pub(crate) fn open_instant(&mut self) {
        self.phase = FlyoutPhase::Open;
        self.duration = Duration::ZERO;
    }

    fn begin(&mut self, transient: FlyoutPhase, terminal: FlyoutPhase) {
        let ms = motion::effective_ms(BASE_MS);
        self.started = Instant::now();
        self.duration = Duration::from_millis(ms as u64);
        self.phase = if ms == 0 { terminal } else { transient };
    }

    /// Whether the flyout should be drawn at all this frame.
    pub(crate) fn is_visible(&self) -> bool {
        self.phase != FlyoutPhase::Hidden
    }

    /// Whether an animation is still in flight (so the caller keeps ticking).
    pub(crate) fn is_animating(&self) -> bool {
        matches!(self.phase, FlyoutPhase::Opening | FlyoutPhase::Closing)
    }

    /// Advances the animation to `now`, transitioning `Opening→Open` /
    /// `Closing→Hidden` when it completes, and returns the eased 0..1
    /// progress of the *current* phase (1.0 for the steady `Open`/`Hidden`
    /// states).
    pub(crate) fn tick(&mut self, now: Instant) -> f32 {
        match self.phase {
            FlyoutPhase::Open => 1.0,
            FlyoutPhase::Hidden => 1.0,
            FlyoutPhase::Opening | FlyoutPhase::Closing => {
                let raw = if self.duration.is_zero() {
                    1.0
                } else {
                    (now.saturating_duration_since(self.started).as_secs_f32()
                        / self.duration.as_secs_f32())
                    .clamp(0.0, 1.0)
                };
                if raw >= 1.0 {
                    self.phase = if self.phase == FlyoutPhase::Opening {
                        FlyoutPhase::Open
                    } else {
                        FlyoutPhase::Hidden
                    };
                    return 1.0;
                }
                motion::ease_out_cubic(raw)
            }
        }
    }

    /// The (scale, opacity) to draw the flyout content at, given the last
    /// `tick` progress `p`. Opening grows `0.96→1.0` while fading in;
    /// Closing shrinks back while fading out; `Open` is full, `Hidden` is
    /// collapsed/invisible.
    #[allow(dead_code, reason = "both flyouts unroll; kept for surfaces not anchored to a screen edge")]
    pub(crate) fn scale_opacity(&self, p: f32) -> (f32, f32) {
        match self.phase {
            FlyoutPhase::Open => (1.0, 1.0),
            FlyoutPhase::Hidden => (0.96, 0.0),
            FlyoutPhase::Opening => (0.96 + 0.04 * p, p),
            FlyoutPhase::Closing => (1.0 - 0.04 * p, 1.0 - p),
        }
    }

    /// The window height to show at eased progress `p` for a flyout that
    /// unrolls downward to `full` (spec §5).
    ///
    /// Opening grows `0 → full`, Closing rolls back `full → 0`, `Open` is
    /// `full` and `Hidden` is `0`. `p` is clamped, so an out-of-range
    /// progress can never yield a negative height (which Win32 would
    /// reject) or one past the flyout's own layout.
    ///
    /// This is the reveal counterpart to [`Flyout::scale_opacity`]: a
    /// surface either unrolls (clipped by its window bounds, needing no
    /// per-pixel alpha) or scales and fades, not both.
    pub(crate) fn reveal_extent(&self, p: f32, full: i32) -> i32 {
        let fraction = match self.phase {
            FlyoutPhase::Open => 1.0,
            FlyoutPhase::Hidden => 0.0,
            FlyoutPhase::Opening => p.clamp(0.0, 1.0),
            FlyoutPhase::Closing => 1.0 - p.clamp(0.0, 1.0),
        };
        (full as f32 * fraction).round() as i32
    }

    /// Test-only: jump the current transition to completion.
    #[cfg(test)]
    pub(crate) fn force_complete(&mut self) {
        self.duration = Duration::ZERO;
        let _ = self.tick(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_moves_hidden_to_opening_or_open() {
        let mut f = Flyout::new();
        f.open();
        // Under normal motion it's Opening; under reduced motion it's Open.
        assert!(matches!(f.phase, FlyoutPhase::Opening | FlyoutPhase::Open));
        assert!(f.is_visible());
    }

    #[test]
    fn opening_completes_to_open() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        f.force_complete();
        assert_eq!(f.phase, FlyoutPhase::Open);
    }

    #[test]
    fn close_from_open_goes_to_closing_then_hidden() {
        let mut f = Flyout::new();
        f.open_instant();
        f.phase = FlyoutPhase::Closing; // simulate a nonzero-duration close
        f.force_complete();
        assert_eq!(f.phase, FlyoutPhase::Hidden);
        assert!(!f.is_visible());
    }

    #[test]
    fn close_on_hidden_is_a_noop() {
        let mut f = Flyout::new();
        f.close();
        assert_eq!(f.phase, FlyoutPhase::Hidden);
    }

    #[test]
    fn open_instant_is_fully_open() {
        let mut f = Flyout::new();
        f.open_instant();
        assert_eq!(f.phase, FlyoutPhase::Open);
        assert_eq!(f.scale_opacity(1.0), (1.0, 1.0));
    }

    #[test]
    fn reveal_extent_spans_zero_to_full_while_opening() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        assert_eq!(f.reveal_extent(0.0, 400), 0);
        assert_eq!(f.reveal_extent(1.0, 400), 400);
    }

    #[test]
    fn reveal_extent_is_monotonic_while_opening() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        let mut previous = -1;
        for step in 0..=10 {
            let extent = f.reveal_extent(step as f32 / 10.0, 400);
            assert!(extent >= previous, "extent went backwards at step {step}");
            previous = extent;
        }
    }

    #[test]
    fn reveal_extent_rolls_back_up_while_closing() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Closing;
        assert_eq!(f.reveal_extent(0.0, 400), 400);
        assert_eq!(f.reveal_extent(1.0, 400), 0);
    }

    #[test]
    fn reveal_extent_is_full_when_open_and_zero_when_hidden() {
        let mut f = Flyout::new();
        f.open_instant();
        assert_eq!(f.reveal_extent(1.0, 400), 400);

        let hidden = Flyout::new();
        assert_eq!(hidden.reveal_extent(1.0, 400), 0);
    }

    #[test]
    fn reveal_extent_never_leaves_the_zero_to_full_range() {
        // Progress is clamped upstream, but a stray out-of-range value
        // must never produce a negative height (a Win32 resize error) or
        // one past `full` (a flyout taller than its own layout).
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        for p in [-5.0f32, -0.1, 1.1, 7.0] {
            let extent = f.reveal_extent(p, 400);
            assert!((0..=400).contains(&extent), "extent {extent} out of range for p={p}");
        }
    }

    #[test]
    fn opening_scale_grows_and_fades_in() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        let (s0, o0) = f.scale_opacity(0.0);
        let (s1, o1) = f.scale_opacity(1.0);
        assert!(s1 > s0 && o1 > o0);
        assert_eq!((s1, o1), (1.0, 1.0));
    }
}
