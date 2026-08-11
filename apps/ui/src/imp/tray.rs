//! System-tray overflow hosting (spec §7). Rather than fragile
//! cross-process re-rendering of third-party tray icons, this *hosts the
//! real* Windows notification-area overflow ("hidden icons") window: our
//! bar chevron finds that window, positions it under the chevron, and shows
//! it, so the actual app icons and their real click behavior are used.
//!
//! Best-effort by design: the overflow window's class name has changed
//! across Windows 11 builds, so [`overflow_available`] tries the known ones
//! and returns `false` when none is found — the bar then hides the chevron
//! and only the curated indicators remain.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, IsWindowVisible, SetWindowPos, ShowWindow, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE, SW_SHOW,
};

/// Class names the notification-area overflow window has used across
/// Windows builds, newest-interesting first. `NotifyIconOverflowWindow` is
/// the long-standing classic-tray class; the XAML-island classes appear on
/// newer Windows 11 shells.
fn overflow_class_candidates() -> [PCWSTR; 3] {
    [
        windows::core::w!("NotifyIconOverflowWindow"),
        windows::core::w!("TopLevelWindowForOverflowXamlIsland"),
        windows::core::w!("XamlExplorerHostIslandWindow"),
    ]
}

/// The real overflow window, if this build exposes one under a class we
/// recognize.
fn find_overflow_window() -> Option<HWND> {
    for class in overflow_class_candidates() {
        // SAFETY: `class` is a static wide-string literal; `FindWindowW`
        // with a null title is a plain top-level lookup with no lifetime
        // requirements. Returns an error/null when the class isn't present.
        if let Ok(hwnd) = unsafe { FindWindowW(class, PCWSTR::null()) } {
            if !hwnd.0.is_null() {
                return Some(hwnd);
            }
        }
    }
    None
}

/// Whether a tray-overflow window exists to host. The bar shows its chevron
/// only when this is true.
pub(crate) fn overflow_available() -> bool {
    find_overflow_window().is_some()
}

/// Top-left position for the overflow window so its right edge sits under
/// the chevron's right edge, dropped just below the bar, clamped to the
/// monitor. Pure so it can be unit-tested without any window present.
pub(crate) fn overflow_position(
    chevron: RECT,
    size_w: i32,
    monitor_left: i32,
    monitor_right: i32,
    below_y: i32,
) -> (i32, i32) {
    let x = (chevron.right - size_w).clamp(monitor_left, (monitor_right - size_w).max(monitor_left));
    (x, below_y)
}

/// Toggles the real overflow window: if it's already visible, hide it;
/// otherwise move it under `chevron_screen` (clamped to the monitor whose
/// right edge is `monitor_right`, left edge `monitor_left`) and show it.
/// No-op when no overflow window is found.
pub(crate) fn toggle_overflow(chevron_screen: RECT, monitor_left: i32, monitor_right: i32) {
    let Some(hwnd) = find_overflow_window() else { return };
    // SAFETY: `hwnd` is a live top-level window handle from
    // `find_overflow_window`; each call below is a documented window
    // operation that fails harmlessly on a stale handle.
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            return;
        }
        let mut rect = RECT::default();
        let width = if GetWindowRect(hwnd, &mut rect).is_ok() {
            rect.right - rect.left
        } else {
            200
        };
        let (x, y) = overflow_position(chevron_screen, width, monitor_left, monitor_right, chevron_screen.bottom);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: r, bottom: b }
    }

    #[test]
    fn overflow_right_aligns_under_chevron() {
        // chevron right at 500, overflow 200 wide -> left at 300.
        assert_eq!(overflow_position(rect(480, 0, 500, 24), 200, 0, 1000, 30), (300, 30));
    }

    #[test]
    fn overflow_clamps_to_monitor_left() {
        // chevron near the left edge can't push the window off-screen.
        assert_eq!(overflow_position(rect(30, 0, 50, 24), 200, 0, 1000, 30).0, 0);
    }

    #[test]
    fn overflow_clamps_to_monitor_right() {
        // A chevron past the right edge clamps so the window stays on-screen.
        assert_eq!(overflow_position(rect(1180, 0, 1200, 24), 200, 0, 1000, 30).0, 800);
    }
}
