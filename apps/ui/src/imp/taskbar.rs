//! Taking the screen over from the real Windows taskbar while this shell
//! runs, and giving it back cleanly on exit.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, ShowWindow, SW_HIDE, SW_SHOW, SPIF_SENDCHANGE, SPI_SETWORKAREA,
    SystemParametersInfoW,
};

use super::monitors::MonitorInfo;
use super::state::scaled;

thread_local! {
    /// Every monitor's work area exactly as it was before this shell
    /// hid the Windows taskbar and claimed the screen — restored at
    /// shutdown so Explorer's taskbar gets its reservation back.
    pub(crate) static ORIGINAL_WORK_AREAS: RefCell<Vec<RECT>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    /// The taskbar's AppBar state (`ABM_GETSTATE`) before this shell
    /// switched it to auto-hide — restored at clean shutdown.
    static PREVIOUS_TASKBAR_STATE: RefCell<Option<u32>> = const { RefCell::new(None) };
}

pub(crate) fn taskbar_appbar_state() -> u32 {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: plain query; `abd` outlives the call.
    (unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) }) as u32
}

pub(crate) fn set_taskbar_appbar_state(state: u32) {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        lParam: LPARAM(state as isize),
        ..Default::default()
    };
    // SAFETY: plain state set; `abd` outlives the call.
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut abd);
    }
}

/// Takes the screen over from the Windows taskbar, or gives it back.
///
/// Hiding the taskbar window alone isn't enough: its AppBar work-area
/// reservation stays registered, so the strip it occupied remains dead
/// space that maximized windows won't use, and Explorer re-asserts
/// that reservation on any AppBar recalculation (which is also how an
/// explicit `SPI_SETWORKAREA` kept getting reverted). Switching the
/// taskbar to *auto-hide* state first (`ABM_SETSTATE`) makes Explorer
/// itself release the reservation; the `SW_HIDE` on top keeps it from
/// ever popping in. The pre-existing state is saved and restored at
/// clean shutdown, and a 1-second watchdog re-hides it if Explorer
/// re-shows it (see the `CLOCK_TIMER_ID` handler).
pub(crate) fn set_windows_taskbar_visible(visible: bool) {
    if visible {
        if let Some(previous) = PREVIOUS_TASKBAR_STATE.with(|s| s.borrow_mut().take()) {
            set_taskbar_appbar_state(previous);
        }
    } else {
        PREVIOUS_TASKBAR_STATE.with(|s| {
            let mut slot = s.borrow_mut();
            if slot.is_none() {
                *slot = Some(taskbar_appbar_state());
            }
        });
        set_taskbar_appbar_state(ABS_AUTOHIDE);
    }
    set_taskbar_windows_visible(visible);
}

/// Just the `ShowWindow` half of [`set_windows_taskbar_visible`] —
/// also used by the periodic re-hide watchdog.
pub(crate) fn set_taskbar_windows_visible(visible: bool) {
    let cmd = if visible { SW_SHOW } else { SW_HIDE };
    // SAFETY: plain window lookups/show-state changes on another
    // process's windows; all documented-fail harmlessly if Explorer
    // isn't running.
    unsafe {
        if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
            let _ = ShowWindow(tray, cmd);
        }
        let mut previous = HWND(std::ptr::null_mut());
        while let Ok(next) =
            FindWindowExW(HWND(std::ptr::null_mut()), previous, w!("Shell_SecondaryTrayWnd"), None)
        {
            if next.0.is_null() {
                break;
            }
            let _ = ShowWindow(next, cmd);
            previous = next;
        }
    }
}

/// Sets one monitor's work area (the rect maximized windows fill).
/// `SPI_SETWORKAREA` applies to whichever monitor contains the rect.
pub(crate) fn set_work_area(rect: RECT) {
    let mut rect = rect;
    // SAFETY: `rect` is a live local for the duration of the call.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut c_void),
            SPIF_SENDCHANGE,
        );
    }
}

/// With the taskbar hidden, its work-area reservation lingers — give
/// every monitor's apps the full screen minus this shell's own bar.
pub(crate) fn claim_work_areas(monitors: &[MonitorInfo]) {
    // The bar's *configured* current height, not the `BAR_HEIGHT` tuned
    // default — see `state::bar_height_config`'s doc comment; using the
    // stale constant here reserved the wrong amount of space the moment
    // the user changed the Top Bar settings page's height slider.
    let bar_height = super::state::bar_height_config();
    for monitor in monitors {
        set_work_area(RECT {
            left: monitor.rect.left,
            top: monitor.rect.top + scaled(bar_height, monitor.dpi),
            right: monitor.rect.right,
            bottom: monitor.rect.bottom,
        });
    }
}

pub(crate) fn restore_work_areas() {
    let areas = ORIGINAL_WORK_AREAS.with(|w| w.borrow().clone());
    for rect in areas {
        set_work_area(rect);
    }
}
