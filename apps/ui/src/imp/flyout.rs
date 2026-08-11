//! One shared open/close lifecycle for the shell's pop-up surfaces (spec
//! §5.1): Quick Settings, the calendar, the session menu, and the tray
//! overflow all animate the same way — grow from the anchor edge with a
//! subtle scale + fade — so they feel like one system. The heavy existing
//! flyouts (Quick Settings, calendar) keep their own windows; new flyouts
//! (session, tray overflow) drive their whole open/close through this.
//!
//! Motion is gated by `reduced_motion` (via `design::motion`): a zero
//! duration collapses `Opening`/`Closing` straight to their terminal state
//! so there is never a stuck half-open frame.

// Consumed by the session-menu and tray-overflow flyouts in the following
// Phase 4 tasks; allowed module-wide so this task's commit stays
// warning-clean rather than landing a half-wired consumer just to satisfy
// the linter. Removed once those flyouts are in.
#![allow(dead_code)]

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
    pub(crate) fn scale_opacity(&self, p: f32) -> (f32, f32) {
        match self.phase {
            FlyoutPhase::Open => (1.0, 1.0),
            FlyoutPhase::Hidden => (0.96, 0.0),
            FlyoutPhase::Opening => (0.96 + 0.04 * p, p),
            FlyoutPhase::Closing => (1.0 - 0.04 * p, 1.0 - p),
        }
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
    fn opening_scale_grows_and_fades_in() {
        let mut f = Flyout::new();
        f.phase = FlyoutPhase::Opening;
        let (s0, o0) = f.scale_opacity(0.0);
        let (s1, o1) = f.scale_opacity(1.0);
        assert!(s1 > s0 && o1 > o0);
        assert_eq!((s1, o1), (1.0, 1.0));
    }
}
