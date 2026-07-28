//! Monitor enumeration: real bounds, work areas, and DPI, plus the
//! left-to-right ordering pinned monitor-workspaces are created in.

use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

const MONITORINFOF_PRIMARY: u32 = 1;

#[derive(Clone, Copy)]
pub(crate) struct MonitorInfo {
    /// Full monitor bounds in virtual-screen coordinates (which can be
    /// negative — the primary monitor anchors the origin, so a monitor
    /// to its left or above it has negative coordinates).
    pub(crate) rect: RECT,
    /// The monitor's work area as of enumeration — captured before
    /// this shell registers its own AppBars or hides the Windows
    /// taskbar, so it's exactly what must be restored at shutdown.
    pub(crate) work: RECT,
    pub(crate) is_primary: bool,
    /// Effective DPI of this monitor (96 = 100% scaling), used to
    /// convert this module's 96-DPI layout constants to physical
    /// pixels via [`super::state::scaled`].
    pub(crate) dpi: u32,
}

/// Enumerates real monitors via `EnumDisplayMonitors`, falling back to
/// a single synthetic monitor covering `GetSystemMetrics(SM_CXSCREEN
/// /SM_CYSCREEN)` if that call somehow returns nothing (shouldn't
/// happen on any real system, but every caller relies on this list
/// being non-empty).
pub(crate) fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    // SAFETY: `monitors` is a local `Vec` whose address is passed
    // through as `lparam` and only read back by `monitor_enum_proc`
    // during this synchronous call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_enum_proc),
            LPARAM(&mut monitors as *mut Vec<MonitorInfo> as isize),
        );
    }

    if monitors.is_empty() {
        // SAFETY: no preconditions; a plain metrics query.
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let rect = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        monitors.push(MonitorInfo {
            rect,
            work: rect,
            is_primary: true,
            dpi: 96,
        });
    }
    monitors
}

/// Monitors ordered left-to-right by real screen position — the order
/// pinned monitor-workspaces are created in, so e.g. a laptop screen
/// physically to the left of an external monitor becomes workspace 0
/// with the external monitor as workspace 1, matching how the carousel
/// then peeks left/right from whichever is current.
pub(crate) fn monitors_sorted_by_x() -> Vec<MonitorInfo> {
    let mut monitors = enumerate_monitors();
    monitors.sort_by_key(|m| m.rect.left);
    monitors
}

/// Which of `monitors` contains the point `(center_x, center_y)`, if
/// any — used to seed/assign a window to the workspace matching the
/// real monitor it's actually on.
pub(crate) fn monitor_index_for_center(monitors: &[MonitorInfo], center_x: i32, center_y: i32) -> Option<usize> {
    monitors.iter().position(|m| {
        center_x >= m.rect.left && center_x < m.rect.right && center_y >= m.rect.top && center_y < m.rect.bottom
    })
}

unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` was created from a live `&mut Vec<MonitorInfo>`
    // in `enumerate_monitors`, and this callback runs synchronously
    // within that call's lifetime.
    let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        monitors.push(MonitorInfo {
            rect: info.rcMonitor,
            work: info.rcWork,
            is_primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            dpi: dpi_x.max(96),
        });
    }
    TRUE
}
