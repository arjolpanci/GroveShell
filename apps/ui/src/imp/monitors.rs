//! Monitor enumeration: real bounds, work areas, and DPI, plus the
//! left-to-right ordering pinned monitor-workspaces are created in.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW, MonitorFromPoint,
    MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

const MONITORINFOF_PRIMARY: u32 = 1;

#[derive(Clone)]
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
    /// Stable identity for this monitor within the current session (e.g.
    /// `\\.\DISPLAY1`), from `MONITORINFOEXW::szDevice`. Used to key
    /// per-monitor workspace/overview state instead of `HMONITOR` (not
    /// guaranteed stable across hotplug) or screen position (changes
    /// when a monitor is added/removed to the left of another).
    pub(crate) device_name: String,
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
            device_name: "\\\\.\\DISPLAY-FALLBACK".to_string(),
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

fn device_name_of(hmonitor: HMONITOR) -> Option<String> {
    // SAFETY: `hmonitor` came from `MonitorFromWindow`/`MonitorFromPoint`,
    // which always return a valid handle (falling back to the nearest
    // real monitor per `MONITOR_DEFAULTTONEAREST`).
    unsafe {
        let mut info = MONITORINFOEXW {
            monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
            return None;
        }
        let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        Some(String::from_utf16_lossy(&info.szDevice[..len]))
    }
}

/// The device name of the monitor `hwnd` is currently on (nearest match
/// if it straddles more than one) — used to route a keyboard shortcut
/// triggered while that window is focused.
#[allow(dead_code)]
pub(crate) fn monitor_key_of_window(hwnd: HWND) -> Option<String> {
    // SAFETY: `hwnd` is a live window; a plain geometry query.
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    device_name_of(hmonitor)
}

/// The device name of the monitor the given screen point is on — used to
/// route a hot-corner trigger or Win-key tap to whichever monitor the
/// cursor is actually over.
#[allow(dead_code)]
pub(crate) fn monitor_key_at_point(pt: POINT) -> Option<String> {
    // SAFETY: plain geometry query, no preconditions.
    let hmonitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    device_name_of(hmonitor)
}

/// The `MonitorInfo` matching `device_name`, if it's still connected.
#[allow(dead_code)]
pub(crate) fn monitor_by_device_name<'a>(
    monitors: &'a [MonitorInfo],
    device_name: &str,
) -> Option<&'a MonitorInfo> {
    monitors.iter().find(|m| m.device_name == device_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(device_name: &str, left: i32) -> MonitorInfo {
        MonitorInfo {
            rect: RECT { left, top: 0, right: left + 1920, bottom: 1080 },
            work: RECT { left, top: 0, right: left + 1920, bottom: 1080 },
            is_primary: left == 0,
            dpi: 96,
            device_name: device_name.to_string(),
        }
    }

    #[test]
    fn finds_monitor_by_device_name_regardless_of_order() {
        let monitors = vec![fake("\\\\.\\DISPLAY2", 1920), fake("\\\\.\\DISPLAY1", 0)];
        let found = monitor_by_device_name(&monitors, "\\\\.\\DISPLAY1").unwrap();
        assert!(found.is_primary);
        assert_eq!(found.rect.left, 0);
    }

    #[test]
    fn missing_device_name_returns_none() {
        let monitors = vec![fake("\\\\.\\DISPLAY1", 0)];
        assert!(monitor_by_device_name(&monitors, "\\\\.\\DISPLAY9").is_none());
    }
}

unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

    let mut info = MONITORINFOEXW {
        monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let device_name = String::from_utf16_lossy(
            &info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len())],
        );
        monitors.push(MonitorInfo {
            rect: info.monitorInfo.rcMonitor,
            work: info.monitorInfo.rcWork,
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            dpi: dpi_x.max(96),
            device_name,
        });
    }
    TRUE
}
