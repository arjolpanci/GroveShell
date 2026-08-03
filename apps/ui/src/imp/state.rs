//! Shared UI state: the single `AppState` every other module reads and
//! mutates through the thread-local `STATE` cell, plus the `Role` lookup
//! used to dispatch messages in `wndproc`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, RECT};

use groveshell_window_model::registry::WindowRegistry;
use groveshell_window_model::Rect;

use super::gpu::GpuSurface;
use super::monitor_workspaces::MonitorWorkspaces;
use super::overview::OverviewInstance;

pub(crate) const BAR_HEIGHT: i32 = 24;
pub(crate) const BAR_CORNER_RADIUS: i32 = 10;

pub(crate) const ANIM_DURATION: std::time::Duration = std::time::Duration::from_millis(250);
pub(crate) const ANIM_TIMER_ID: usize = 1;
pub(crate) const ANIM_TIMER_INTERVAL_MS: u32 = 16;
pub(crate) const CLOCK_TIMER_ID: usize = 2;

pub(crate) fn scaled(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// Effective DPI of the primary monitor — used only where a value must
/// be primary-anchored on purpose (the clock/Quick-Settings pill).
/// Per-monitor overview/bar code must use *that monitor's own* DPI
/// instead (see `monitors::MonitorInfo::dpi`), never this.
pub(crate) fn reference_dpi() -> u32 {
    let monitors = super::monitors::monitors_sorted_by_x();
    monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first())
        .map(|m| m.dpi)
        .unwrap_or(96)
}

/// One monitor's top-level bar window plus the rect the AppBar system
/// actually assigned it and which monitor (by device name) it belongs
/// to.
pub(crate) struct BarWindow {
    pub(crate) hwnd: HWND,
    pub(crate) rect: RECT,
    pub(crate) is_primary: bool,
    pub(crate) monitor: String,
}

pub(crate) struct AppState {
    pub(crate) bars: Vec<BarWindow>,
    pub(crate) config: groveshell_config::Config,
    pub(crate) primary_bar_hwnd: HWND,
    pub(crate) primary_bar_rect: RECT,
    /// The primary monitor's device name — the fallback target for
    /// windows/workspaces orphaned by a monitor unplug (Task 10).
    pub(crate) primary_monitor: String,
    pub(crate) calendar_hwnd: HWND,
    /// `None` if GPU rendering isn't available (see `gpu::is_enabled`) —
    /// `paint_calendar`/`toggle_calendar` fall back to plain GDI in that
    /// case, unchanged from before this feature existed.
    pub(crate) calendar_gpu: Option<GpuSurface>,
    pub(crate) quick_settings_hwnd: HWND,
    pub(crate) calendar_open: bool,
    pub(crate) quick_settings_open: bool,
    pub(crate) previous_foreground: HWND,
    /// Per-monitor window->workspace assignment (see
    /// `monitor_workspaces::MonitorWorkspaces`).
    pub(crate) workspaces: MonitorWorkspaces,
    /// Per-monitor Activities overview state, keyed by device name.
    pub(crate) overviews: HashMap<String, OverviewInstance>,
    pub(crate) window_registry: WindowRegistry,
    /// Last-observed on-screen rect per live window, updated every
    /// `sync_workspaces` tick. Used only to compute how far a window
    /// moved since the previous tick when a monitor mismatch is detected,
    /// so the same delta can be applied to any of its owned windows
    /// (dialogs) — see `workspaces::sync_workspaces`.
    pub(crate) window_rects: HashMap<isize, Rect>,
    /// Which bar (by hwnd, since Activities/dots exist on every monitor's
    /// bar) and which clickable region within it the cursor currently
    /// sits over — drives both the hover-highlight paint (`bar::paint_bar`)
    /// and the hand cursor (`WM_SETCURSOR` in `mod.rs`), so a user can
    /// actually tell something's clickable before clicking it.
    pub(crate) hovered_bar_region: Option<(HWND, super::bar::BarRegion)>,
    pub(crate) qs_volume_dragging: bool,
}

thread_local! {
    pub(crate) static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// The current primary bar's `HWND`, mirrored outside the thread-local
/// `STATE` cell (0 means "not yet known") so background threads — namely
/// `config_reload_listener`'s pipe-listener thread — can read it without
/// touching `STATE`'s `RefCell`, which belongs exclusively to the UI
/// thread. `STATE.with` from a non-UI thread wouldn't panic, but it also
/// wouldn't see the UI thread's data: thread-locals are per-thread
/// storage, so the pipe thread would silently and permanently read back
/// `None` instead of the real value. Kept in sync with
/// `AppState::primary_bar_hwnd` at every write site (initial startup in
/// `main`, and `hotplug::reconcile_monitors`'s primary-promotion path)
/// via `set_primary_bar_hwnd`.
static PRIMARY_BAR_HWND: AtomicIsize = AtomicIsize::new(0);

/// Updates the cross-thread mirror of the primary bar's `HWND`. Call
/// this every time `AppState::primary_bar_hwnd` is written.
pub(crate) fn set_primary_bar_hwnd(hwnd: HWND) {
    PRIMARY_BAR_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// Reads the cross-thread mirror of the primary bar's `HWND`. Safe to
/// call from any thread, including background IPC-listener threads that
/// must never touch `STATE` directly.
pub(crate) fn primary_bar_hwnd() -> Option<HWND> {
    match PRIMARY_BAR_HWND.load(Ordering::Relaxed) {
        0 => None,
        raw => Some(HWND(raw as *mut std::ffi::c_void)),
    }
}

thread_local! {
    /// Mirrors `AppState.config.appearance.{animation_scale,reduced_motion}`
    /// outside `STATE`'s `RefCell`, for the same reason `PRIMARY_BAR_HWND`
    /// mirrors the primary bar's `HWND` above — except the problem here
    /// isn't cross-thread access, it's *same-thread re-entrancy*:
    /// `util::progress_dur` is called from deep inside animation code
    /// (`overview::on_animation_tick` and friends) that already holds
    /// `STATE`'s borrow for the whole duration of its work, so a second,
    /// nested `STATE.with(|s| s.borrow())` inside `progress_dur` panics
    /// with "RefCell already mutably borrowed" the instant an overview
    /// animates (confirmed live: this crashed the process on the very
    /// first `WM_TIMER` tick after opening Activities). A separate
    /// thread-local `Cell` has no aliasing relationship with `STATE`'s
    /// `RefCell` at all, so `progress_dur` can read it regardless of
    /// what borrow `STATE` is currently under. Kept in sync at every
    /// place `AppState.config` is set or replaced (initial load in
    /// `main`, and the `WM_APP_CONFIG_RELOADED` handler) via
    /// `set_animation_config`.
    static ANIMATION_SCALE: Cell<f32> = const { Cell::new(1.0) };
    static REDUCED_MOTION: Cell<bool> = const { Cell::new(false) };

    /// Same idea, same reason, for `AppState.config.appearance.{dock_icon_size,dock_alignment}`:
    /// `dock::dock_layout` used to read these straight out of `STATE`, and
    /// is itself called from deep inside code that already holds `STATE`'s
    /// borrow (`overview::on_overview_hover` and several other overview
    /// call sites) — confirmed live: this crashed the process the moment
    /// the mouse moved over an open overview, every single time, the same
    /// "RefCell already mutably borrowed" abort `ANIMATION_SCALE` exists to
    /// avoid for animation code. Kept in sync at the same two places as
    /// `ANIMATION_SCALE`/`REDUCED_MOTION` via `set_dock_config`.
    static DOCK_ICON_SIZE: Cell<i32> = const { Cell::new(44) };
    static DOCK_ALIGNMENT: RefCell<String> = RefCell::new(String::new());

    /// Same idea again for `AppState.config.appearance.top_bar_height`:
    /// `overview::card_layout` (which needs the bar's real current height
    /// to know where the card grid starts) is called from the same
    /// deeply-nested overview/dock code that already holds `STATE`'s
    /// borrow, so it reads this mirror instead of `STATE` directly. Kept
    /// in sync at the same two places as the others, plus the live
    /// top-bar-resize path in `mod.rs`'s `WM_APP_CONFIG_RELOADED` handler
    /// (which already resizes the real bar windows to this value).
    static TOP_BAR_HEIGHT: Cell<i32> = const { Cell::new(BAR_HEIGHT) };
}

/// Updates the re-entrancy-safe mirror of the animation-affecting config
/// fields. Call this every time `AppState.config` is set or replaced.
pub(crate) fn set_animation_config(scale: f32, reduced_motion: bool) {
    ANIMATION_SCALE.with(|c| c.set(scale));
    REDUCED_MOTION.with(|c| c.set(reduced_motion));
}

/// Reads the re-entrancy-safe mirror of the animation-affecting config
/// fields. Safe to call from anywhere, including from inside an active
/// `STATE.with` borrow — see `ANIMATION_SCALE`'s doc comment.
pub(crate) fn animation_config() -> (f32, bool) {
    (ANIMATION_SCALE.with(|c| c.get()), REDUCED_MOTION.with(|c| c.get()))
}

/// Updates the re-entrancy-safe mirror of the dock-affecting config
/// fields. Call this every time `AppState.config` is set or replaced.
pub(crate) fn set_dock_config(icon_size: i32, alignment: &str) {
    DOCK_ICON_SIZE.with(|c| c.set(icon_size));
    DOCK_ALIGNMENT.with(|c| *c.borrow_mut() = alignment.to_string());
}

/// Reads the re-entrancy-safe mirror of the dock-affecting config fields.
/// Safe to call from anywhere, including from inside an active `STATE.with`
/// borrow — see `DOCK_ICON_SIZE`'s doc comment. Falls back to "center" if
/// `set_dock_config` hasn't run yet (shouldn't happen outside tests: `main`
/// sets it before any window/message loop exists).
pub(crate) fn dock_config() -> (i32, String) {
    let alignment = DOCK_ALIGNMENT.with(|c| c.borrow().clone());
    (DOCK_ICON_SIZE.with(|c| c.get()), if alignment.is_empty() { "center".to_string() } else { alignment })
}

/// Updates the re-entrancy-safe mirror of the configured top bar height.
/// Call this every time `AppState.config` is set or replaced, and whenever
/// the live bars are resized to a new height.
pub(crate) fn set_bar_height_config(height: i32) {
    TOP_BAR_HEIGHT.with(|c| c.set(height));
}

/// Reads the re-entrancy-safe mirror of the configured top bar height —
/// the bar's *actual* current height (96-DPI reference value, scale with
/// [`scaled`] same as any other layout constant), not the `BAR_HEIGHT`
/// constant that value was originally tuned against. Safe to call from
/// anywhere, including from inside an active `STATE.with` borrow — see
/// `TOP_BAR_HEIGHT`'s doc comment.
pub(crate) fn bar_height_config() -> i32 {
    TOP_BAR_HEIGHT.with(|c| c.get())
}

/// How much bigger the bar's *contents* (icons, dots, gear glyph, fonts —
/// everything `bar.rs` sizes off a 96-DPI constant) should be relative to
/// their originally-tuned size, given the configured height vs. the
/// `BAR_HEIGHT` constant those sizes were tuned against. Growing only the
/// bar window (via `bar_height_config`) without this left every icon the
/// same size, just re-centered in extra empty space — this is what makes
/// "make the bar bigger" actually make its contents bigger too.
pub(crate) fn bar_content_scale() -> f64 {
    bar_height_config() as f64 / BAR_HEIGHT as f64
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Bar { is_primary: bool, monitor: String },
    Overview { monitor: String },
    Calendar,
    QuickSettings,
    Other,
}

pub(crate) fn role_of(hwnd: HWND) -> Role {
    STATE.with(|s| {
        let state = s.borrow();
        let Some(st) = state.as_ref() else {
            return Role::Other;
        };
        if let Some(bar) = st.bars.iter().find(|b| b.hwnd == hwnd) {
            return Role::Bar {
                is_primary: bar.is_primary,
                monitor: bar.monitor.clone(),
            };
        }
        if let Some((monitor, _)) = st.overviews.iter().find(|(_, ov)| ov.hwnd == hwnd) {
            return Role::Overview { monitor: monitor.clone() };
        }
        if hwnd == st.calendar_hwnd {
            Role::Calendar
        } else if hwnd == st.quick_settings_hwnd {
            Role::QuickSettings
        } else {
            Role::Other
        }
    })
}
