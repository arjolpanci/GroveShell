//! The opt-in persistent desktop dock (spec §5.3 / §7). Off by default
//! (`dock_mode = "overview"`); `"always"` shows a floating dock at the
//! bottom of the primary monitor, `"autohide"` slides it away and reveals
//! it when the pointer hits the bottom edge.
//!
//! Deliberately a **floating, top-most overlay** — it does *not* register
//! an AppBar or reserve work area. Reserving a strip is what can strand the
//! desktop when a window mis-registers (the project has fought exactly that
//! with the taskbar), and a persistent dock doesn't need it: it simply
//! floats over the bottom of the screen like a GNOME dash. This keeps the
//! risky work-area machinery out of an opt-in surface entirely.
//!
//! This module owns the reveal state machine and geometry (pure, tested);
//! the window creation/paint wiring lives alongside it and reuses the
//! overview dock's app model (`dock::build_dock_apps`) rather than
//! duplicating icon logic.

// The reveal state machine and slide geometry are complete and unit-tested;
// the floating dock window that consumes them is the remaining build-out,
// deferred because a new always-on-top surface needs live verification of
// its placement, autohide feel, and click routing. Allowed module-wide
// until that window lands.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use super::design::motion::{self, BASE_MS};

/// Autohide reveal phases. `Shown`/`Hidden` are the resting states;
/// `Revealing`/`Hiding` are the sliding transitions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RevealPhase {
    Hidden,
    Revealing,
    Shown,
    Hiding,
}

/// The autohide reveal state machine. Driven by pointer hot-edge hits and
/// leaves, advanced by `tick`.
pub(crate) struct AutoHide {
    pub(crate) phase: RevealPhase,
    started: Instant,
    duration: Duration,
}

impl AutoHide {
    pub(crate) fn new() -> Self {
        Self { phase: RevealPhase::Hidden, started: Instant::now(), duration: Duration::ZERO }
    }

    /// The pointer reached the bottom hot-edge: start revealing (or cancel
    /// an in-progress hide, snapping back to fully shown).
    pub(crate) fn on_hot_edge(&mut self) {
        match self.phase {
            RevealPhase::Hidden => self.begin(RevealPhase::Revealing),
            RevealPhase::Hiding => self.phase = RevealPhase::Shown,
            _ => {}
        }
    }

    /// The pointer left the dock (after a dwell): start hiding.
    pub(crate) fn on_leave(&mut self) {
        if matches!(self.phase, RevealPhase::Shown | RevealPhase::Revealing) {
            self.begin(RevealPhase::Hiding);
        }
    }

    fn begin(&mut self, transient: RevealPhase) {
        let ms = motion::effective_ms(BASE_MS);
        self.started = Instant::now();
        self.duration = Duration::from_millis(ms as u64);
        if ms == 0 {
            self.phase = terminal_of(transient);
        } else {
            self.phase = transient;
        }
    }

    /// Advances to `now`, completing `Revealing→Shown` / `Hiding→Hidden`,
    /// and returns the eased 0..1 progress of the current transition (1.0
    /// when resting).
    pub(crate) fn tick(&mut self, now: Instant) -> f32 {
        match self.phase {
            RevealPhase::Shown | RevealPhase::Hidden => 1.0,
            RevealPhase::Revealing | RevealPhase::Hiding => {
                let raw = if self.duration.is_zero() {
                    1.0
                } else {
                    (now.saturating_duration_since(self.started).as_secs_f32()
                        / self.duration.as_secs_f32())
                    .clamp(0.0, 1.0)
                };
                if raw >= 1.0 {
                    self.phase = terminal_of(self.phase);
                    return 1.0;
                }
                motion::ease_out_cubic(raw)
            }
        }
    }

    pub(crate) fn is_animating(&self) -> bool {
        matches!(self.phase, RevealPhase::Revealing | RevealPhase::Hiding)
    }

    /// Vertical offset (px) to draw the dock at for the current phase and
    /// tick progress `p`: `0` fully shown, `height` fully hidden below the
    /// screen edge. Revealing slides up (`height→0`), Hiding slides down.
    pub(crate) fn offset(&self, p: f32, height: i32) -> i32 {
        let h = height as f32;
        let off = match self.phase {
            RevealPhase::Shown => 0.0,
            RevealPhase::Hidden => h,
            RevealPhase::Revealing => h * (1.0 - p),
            RevealPhase::Hiding => h * p,
        };
        off.round() as i32
    }

    #[cfg(test)]
    pub(crate) fn force(&mut self, phase: RevealPhase) {
        self.phase = phase;
    }
}

fn terminal_of(transient: RevealPhase) -> RevealPhase {
    match transient {
        RevealPhase::Revealing => RevealPhase::Shown,
        RevealPhase::Hiding => RevealPhase::Hidden,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_edge_from_hidden_starts_revealing_or_shows() {
        let mut a = AutoHide::new();
        a.on_hot_edge();
        assert!(matches!(a.phase, RevealPhase::Revealing | RevealPhase::Shown));
    }

    #[test]
    fn leave_from_shown_starts_hiding_or_hides() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Shown);
        a.on_leave();
        assert!(matches!(a.phase, RevealPhase::Hiding | RevealPhase::Hidden));
    }

    #[test]
    fn hot_edge_cancels_an_in_progress_hide() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Hiding);
        a.on_hot_edge();
        assert_eq!(a.phase, RevealPhase::Shown);
    }

    #[test]
    fn offset_is_zero_when_shown_and_full_when_hidden() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Shown);
        assert_eq!(a.offset(1.0, 60), 0);
        a.force(RevealPhase::Hidden);
        assert_eq!(a.offset(1.0, 60), 60);
    }

    #[test]
    fn revealing_slides_up_from_hidden_toward_shown() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Revealing);
        assert_eq!(a.offset(0.0, 60), 60); // just started: fully down
        assert_eq!(a.offset(1.0, 60), 0); // complete: fully up
    }
}
