//! Shared UI state: the single `AppState` every other module reads and
//! mutates through the thread-local `STATE` cell, plus the `Role` lookup
//! used to dispatch messages in `wndproc`.

use std::cell::RefCell;
use std::collections::HashMap;

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
    pub(crate) qs_pill_hover: bool,
    pub(crate) qs_volume_dragging: bool,
}

thread_local! {
    pub(crate) static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

#[derive(Clone, PartialEq, Eq)]
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
