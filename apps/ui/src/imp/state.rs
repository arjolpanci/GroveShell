//! Shared UI state: the single `AppState` every other module reads and
//! mutates through the thread-local `STATE` cell, plus the `Role` lookup
//! used to dispatch messages in `wndproc`.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, RECT};

use groveshell_window_model::registry::WindowRegistry;
use groveshell_window_model::workspace::WorkspaceTracker;

use super::overview::{CarouselAnim, CarouselDrag, OverviewMode, WindowDrag};

/// Half of the original 32px guess — the Windows taskbar itself is
/// ~40px, but this shell is meant to feel closer to GNOME's slim bar;
/// 50% taller than the first cut (16px) to fit the clock and quick
/// settings labels comfortably.
///
/// Like every other layout constant in this module, this is a *96-DPI*
/// value: the process is per-monitor-DPI-aware, so all coordinates are
/// physical pixels and every use site must pass through [`scaled`]
/// with the relevant monitor/window DPI, or the bar comes out
/// physically 24px tall — visibly tiny on a 125%/150% laptop panel.
pub(crate) const BAR_HEIGHT: i32 = 24;
/// Rounded radius of the bar's *bottom* two corners (96-DPI); the top
/// edge stays square against the screen edge.
pub(crate) const BAR_CORNER_RADIUS: i32 = 10;

pub(crate) const ANIM_DURATION: std::time::Duration = std::time::Duration::from_millis(250);
pub(crate) const ANIM_TIMER_ID: usize = 1;
pub(crate) const ANIM_TIMER_INTERVAL_MS: u32 = 16;
/// Refreshes the primary bar's clock text once a second.
pub(crate) const CLOCK_TIMER_ID: usize = 2;

/// Converts a 96-DPI layout value to physical pixels at `dpi`,
/// rounding to nearest.
pub(crate) fn scaled(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// Effective DPI of the primary monitor (falling back to the first
/// enumerated one, then to 96) — the reference the overview's card
/// layout is scaled against, matching `card_layout` basing the card
/// geometry on the primary monitor.
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
/// actually assigned it.
pub(crate) struct BarWindow {
    pub(crate) hwnd: HWND,
    pub(crate) rect: RECT,
    pub(crate) is_primary: bool,
}

/// All mutable UI state lives here, on the single UI thread that owns
/// every window created below. `thread_local!` (rather than a `static`)
/// makes that single-thread assumption explicit and avoids `unsafe`
/// globals for what is, in a classic Win32 app, thread-affine state
/// anyway (window procedures for these windows only ever run on the
/// thread that created them).
pub(crate) struct AppState {
    pub(crate) bars: Vec<BarWindow>,
    /// Cached from `bars` for quick access — the only bar with
    /// Activities/clock/Quick Settings, and the one the calendar/QS
    /// flyouts are anchored under.
    pub(crate) primary_bar_hwnd: HWND,
    pub(crate) primary_bar_rect: RECT,
    pub(crate) overview_hwnd: HWND,
    pub(crate) calendar_hwnd: HWND,
    pub(crate) quick_settings_hwnd: HWND,
    pub(crate) overview: OverviewMode,
    pub(crate) calendar_open: bool,
    pub(crate) quick_settings_open: bool,
    /// Captured right before opening whichever flyout is currently
    /// open, so cancelling (Escape / empty-area click) can restore
    /// focus to whatever the user was actually doing. The bars
    /// themselves never become foreground (`WS_EX_NOACTIVATE`), so
    /// this is never just a bar.
    pub(crate) previous_foreground: HWND,
    /// Window→workspace assignment and the dynamic-workspace policy
    /// (see the module docs). Persists across overview open/close
    /// cycles; only the fields below are session-per-overview.
    pub(crate) workspaces: WorkspaceTracker,
    /// Current horizontal scroll position through the carousel, in page
    /// units (page index of whichever page is centered; fractional
    /// while dragging or mid-snap-animation). Only meaningful while the
    /// overview isn't `Closed`.
    pub(crate) carousel_offset: f64,
    pub(crate) carousel_drag: Option<CarouselDrag>,
    pub(crate) carousel_anim: Option<CarouselAnim>,
    /// Set when a carousel snap-animation should close the overview
    /// (focusing this window) once it lands, rather than just
    /// re-centering on the target page.
    pub(crate) carousel_close_after: Option<HWND>,
    /// A window preview being dragged between workspace cards (mutually
    /// exclusive with `carousel_drag` — whichever the press landed on).
    pub(crate) window_drag: Option<WindowDrag>,
    /// Live text of the overview's type-to-search; empty = search off.
    pub(crate) search_query: String,
    /// Stable identity across HWND reuse (see `registry`): consulted by
    /// `sync_workspaces` so a recycled handle doesn't inherit the dead
    /// window's workspace assignment or snapshot.
    pub(crate) window_registry: WindowRegistry,
}

thread_local! {
    pub(crate) static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Bar { is_primary: bool },
    Overview,
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
            };
        }
        if hwnd == st.overview_hwnd {
            Role::Overview
        } else if hwnd == st.calendar_hwnd {
            Role::Calendar
        } else if hwnd == st.quick_settings_hwnd {
            Role::QuickSettings
        } else {
            Role::Other
        }
    })
}
