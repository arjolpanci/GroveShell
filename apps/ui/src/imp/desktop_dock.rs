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

use windows::Win32::Foundation::RECT;

use super::design::motion::{self, BASE_MS};

/// 96-DPI layout metrics for the floating desktop dock. Deliberately a
/// touch roomier than the overview dock (`dock.rs`) so it reads as a
/// first-class, macOS-style dock rather than a compact dash.
pub(crate) const ICON_GAP: i32 = 12;
pub(crate) const PAD_X: i32 = 14;
pub(crate) const PAD_Y: i32 = 10;
pub(crate) const CORNER_RADIUS: i32 = 22;
/// Gap between the dock panel's bottom and the monitor's work-area bottom.
pub(crate) const MARGIN_BOTTOM: i32 = 10;
/// Peak magnification of the icon directly under the pointer — the crest
/// of the wave. 1.0 would be no magnification.
pub(crate) const MAX_SCALE: f32 = 1.65;
/// Running-indicator dot radius (96-DPI).
pub(crate) const RUNNING_DOT_RADIUS: i32 = 3;

/// The dock's fixed (resting) panel geometry in the dock window's own
/// local coordinates, plus each icon's baseline center-x. The window is
/// sized taller and a little wider than the panel so magnified icons can
/// grow upward out of the panel and outward at the ends without being
/// clipped — the classic dock "the icon pops above the tray" look.
///
/// Returns `(panel_rect, icon_centers, baseline_bottom, window_w,
/// window_h)`, all DPI-scaled. `baseline_bottom` is the local y that every
/// icon's *bottom* edge sits on; icons grow upward from it as they
/// magnify. Pure and total (clamps an empty dock to one slot) so it's unit
/// testable without a live monitor.
pub(crate) fn panel_geometry(app_count: usize, icon_size_ref: i32, dpi: u32) -> PanelGeometry {
    let scaled = |v: i32| super::state::scaled(v, dpi);
    let icon = super::state::scaled(icon_size_ref, dpi).max(1);
    let gap = scaled(ICON_GAP);
    let pad_x = scaled(PAD_X);
    let pad_y = scaled(PAD_Y);

    let count = app_count.max(1) as i32;
    let panel_w = count * icon + (count - 1).max(0) * gap + pad_x * 2;
    let panel_h = icon + pad_y * 2;

    let max_icon = (icon as f32 * MAX_SCALE).round() as i32;
    // Side headroom so an end icon at full magnification (scaled about its
    // own center) doesn't clip the window edge; top headroom so a crest
    // icon grows fully above the panel.
    let side_pad = (max_icon - icon) / 2 + gap;
    let top_pad = (max_icon - icon).max(0) + pad_y;
    let window_w = panel_w + side_pad * 2;
    let window_h = panel_h + top_pad;

    let panel_left = side_pad;
    let panel_top = window_h - panel_h;
    let panel_rect = RECT {
        left: panel_left,
        top: panel_top,
        right: panel_left + panel_w,
        bottom: window_h,
    };
    let baseline_bottom = window_h - pad_y;

    let mut centers = Vec::with_capacity(count as usize);
    let first_center = panel_left + pad_x + icon / 2;
    for i in 0..count {
        centers.push(first_center + i * (icon + gap));
    }

    PanelGeometry { panel_rect, centers, baseline_bottom, window_w, window_h, icon }
}

/// Result of [`panel_geometry`] — see its doc comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PanelGeometry {
    pub(crate) panel_rect: RECT,
    pub(crate) centers: Vec<i32>,
    pub(crate) baseline_bottom: i32,
    pub(crate) window_w: i32,
    pub(crate) window_h: i32,
    /// The resting (un-magnified) icon size, DPI-scaled.
    pub(crate) icon: i32,
}

/// A smooth wave-magnification factor (>= 1.0) for an icon whose baseline
/// center is `distance` px from the pointer, peaking at [`MAX_SCALE`]
/// directly under it and easing back to 1.0 with a Gaussian falloff whose
/// width is `sigma`. `cursor` of `None` (pointer not over the dock) yields
/// 1.0 everywhere — a flat, resting dock. Pure and cheap.
pub(crate) fn magnify_factor(distance: f32, sigma: f32) -> f32 {
    let sigma = sigma.max(1.0);
    let x = distance / sigma;
    1.0 + (MAX_SCALE - 1.0) * (-0.5 * x * x).exp()
}

/// Each icon's on-screen rect for the current pointer position, applying
/// the wave: every icon stays centered on its fixed baseline center and
/// bottom-anchored to `baseline_bottom`, scaled by [`magnify_factor`] of
/// its distance to `cursor_x` (window-local). `progress` (0..1) eases the
/// whole effect in/out so the wave doesn't snap on when the pointer
/// arrives — the rendered scale is lerped from 1.0 toward its target by
/// `progress`. Fixed centers keep hit-testing stable (see
/// [`icon_at`]). Pure.
pub(crate) fn wave_icon_rects(
    geo: &PanelGeometry,
    cursor_x: Option<i32>,
    progress: f32,
) -> Vec<RECT> {
    let progress = progress.clamp(0.0, 1.0);
    let sigma = (geo.icon as f32) * 1.3;
    geo.centers
        .iter()
        .map(|&cx| {
            let target = match cursor_x {
                Some(x) => magnify_factor((x - cx).abs() as f32, sigma),
                None => 1.0,
            };
            let scale = 1.0 + (target - 1.0) * progress;
            let size = (geo.icon as f32 * scale).round() as i32;
            let half = size / 2;
            RECT {
                left: cx - half,
                top: geo.baseline_bottom - size,
                right: cx - half + size,
                bottom: geo.baseline_bottom,
            }
        })
        .collect()
}

/// Which icon index the pointer at window-local `x` is over, or `None` if
/// it's past the ends. Hit-tests the *fixed* baseline centers (pitch =
/// icon + gap), so a click lands on the same app whether the dock is
/// resting or mid-wave — the magnified rects only move things visually.
pub(crate) fn icon_at(geo: &PanelGeometry, x: i32, dpi: u32) -> Option<usize> {
    if geo.centers.is_empty() {
        return None;
    }
    let pitch = geo.icon + super::state::scaled(ICON_GAP, dpi);
    let half = pitch / 2;
    geo.centers
        .iter()
        .enumerate()
        .find(|(_, &cx)| (x - cx).abs() <= half)
        .map(|(i, _)| i)
}

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

    #[test]
    fn panel_geometry_spaces_centers_by_one_pitch() {
        let geo = panel_geometry(3, 48, 96);
        assert_eq!(geo.centers.len(), 3);
        let pitch = geo.centers[1] - geo.centers[0];
        assert_eq!(geo.centers[2] - geo.centers[1], pitch);
        // Pitch is one icon plus one gap at this DPI.
        assert_eq!(pitch, 48 + ICON_GAP);
    }

    #[test]
    fn panel_geometry_window_is_taller_and_wider_than_the_panel() {
        let geo = panel_geometry(5, 48, 96);
        let panel_w = geo.panel_rect.right - geo.panel_rect.left;
        let panel_h = geo.panel_rect.bottom - geo.panel_rect.top;
        assert!(geo.window_w > panel_w, "window needs side headroom for end-icon magnification");
        assert!(geo.window_h > panel_h, "window needs top headroom for the wave crest");
    }

    #[test]
    fn panel_geometry_clamps_an_empty_dock_to_one_slot() {
        let geo = panel_geometry(0, 48, 96);
        assert_eq!(geo.centers.len(), 1);
        assert!(geo.window_w > 0 && geo.window_h > 0);
    }

    #[test]
    fn magnify_factor_peaks_under_the_pointer_and_decays() {
        let sigma = 60.0;
        let at_zero = magnify_factor(0.0, sigma);
        assert!((at_zero - MAX_SCALE).abs() < 1e-5);
        assert!(magnify_factor(sigma, sigma) < at_zero);
        assert!(magnify_factor(sigma * 4.0, sigma) < magnify_factor(sigma, sigma));
        // Far away it's essentially resting size.
        assert!(magnify_factor(sigma * 6.0, sigma) < 1.001);
    }

    #[test]
    fn wave_with_no_cursor_leaves_every_icon_at_rest() {
        let geo = panel_geometry(4, 48, 96);
        let rects = wave_icon_rects(&geo, None, 1.0);
        for r in &rects {
            assert_eq!(r.right - r.left, geo.icon);
            assert_eq!(r.bottom, geo.baseline_bottom);
        }
    }

    #[test]
    fn wave_magnifies_the_icon_under_the_cursor_the_most() {
        let geo = panel_geometry(5, 48, 96);
        let cursor = geo.centers[2];
        let rects = wave_icon_rects(&geo, Some(cursor), 1.0);
        let widths: Vec<i32> = rects.iter().map(|r| r.right - r.left).collect();
        assert!(widths[2] > widths[1]);
        assert!(widths[2] > widths[3]);
        // Magnified icons stay bottom-anchored (grow upward).
        for r in &rects {
            assert_eq!(r.bottom, geo.baseline_bottom);
        }
    }

    #[test]
    fn wave_progress_zero_is_a_flat_dock() {
        let geo = panel_geometry(5, 48, 96);
        let rects = wave_icon_rects(&geo, Some(geo.centers[2]), 0.0);
        for r in &rects {
            assert_eq!(r.right - r.left, geo.icon);
        }
    }

    #[test]
    fn icon_at_hits_each_center_and_misses_past_the_ends() {
        let geo = panel_geometry(3, 48, 96);
        assert_eq!(icon_at(&geo, geo.centers[0], 96), Some(0));
        assert_eq!(icon_at(&geo, geo.centers[2], 96), Some(2));
        assert_eq!(icon_at(&geo, geo.centers[0] - 1000, 96), None);
        assert_eq!(icon_at(&geo, geo.centers[2] + 1000, 96), None);
    }
}
