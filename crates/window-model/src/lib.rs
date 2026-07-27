#![cfg(windows)]

//! A minimal, read-only snapshot of top-level windows, per
//! `docs/PROJECT_PLAN.md` §6.2 (eligible top-level window policy) and
//! Phase 1's "reliable, inspectable model of top-level windows" goal. This
//! is a synchronous `EnumWindows` pass with no caching, hooks, or lifecycle
//! tracking yet — just enough for the Phase 4/5 Activities overview to show
//! something real. `WindowId` generation-counter identity, WinEvent-driven
//! updates, and the full eligibility/diagnostic model described in §6 are
//! deliberately out of scope here and remain a follow-up.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, GW_OWNER, WINDOWPLACEMENT, WS_EX_TOOLWINDOW,
};

/// A window rectangle in screen coordinates, left/top inclusive and
/// right/bottom exclusive — matches Win32's own `RECT` convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// One eligible top-level window as of the moment [`snapshot`] was taken.
/// `hwnd` is the raw handle value; it is only meaningful until the next
/// reconciliation; see the module docs on HWND reuse.
#[derive(Debug, Clone)]
pub struct WindowRecord {
    pub hwnd: isize,
    pub title: String,
    pub pid: u32,
    /// Best-effort executable file name (no directory), e.g. `"notepad.exe"`.
    /// `None` when the process couldn't be opened or queried — most often
    /// because it runs elevated and this process does not (per
    /// `docs/PROJECT_PLAN.md` §12, UIPI limits control over elevated
    /// windows; the same restriction applies to inspecting them).
    pub exe_name: Option<String>,
    /// On-screen bounds, used to seed the Activities overview's "zoom out
    /// from where the window actually is" animation. For a minimized
    /// window this is the last known restored position (`GetWindowRect`
    /// reports a meaningless off-screen rect for minimized windows), so
    /// the overview still has somewhere sensible to zoom from.
    pub rect: Rect,
}

/// Enumerates all top-level windows and returns the ones that look like
/// ordinary, user-facing application windows: visible, unowned, not a tool
/// window, with a non-empty title, and not belonging to this process.
pub fn snapshot() -> Vec<WindowRecord> {
    let mut records: Vec<WindowRecord> = Vec::new();

    // SAFETY: `records` is a local `Vec` whose address is passed through as
    // `lparam` and only ever read back by `enum_proc` during this call;
    // `EnumWindows` is synchronous, so `records` is guaranteed to outlive
    // every callback invocation.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut records as *mut Vec<WindowRecord> as isize));
    }

    records
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` was created from a live `&mut Vec<WindowRecord>` in
    // `snapshot` and this callback only runs synchronously within that
    // call's lifetime.
    let records = &mut *(lparam.0 as *mut Vec<WindowRecord>);

    if let Some(record) = inspect(hwnd) {
        records.push(record);
    }

    TRUE
}

fn inspect(hwnd: HWND) -> Option<WindowRecord> {
    // SAFETY: `hwnd` is supplied by `EnumWindows` and is valid for the
    // duration of this synchronous callback frame.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }

        // Many background/tray apps (and most built-in shell hosts, e.g.
        // Widgets, Search, Shell Experience Host) keep a top-level window
        // that reports `IsWindowVisible == true` but is DWM-cloaked and
        // never actually drawn on screen — the same signal the real
        // taskbar uses to decide whether something gets a button. Without
        // this check, closing an app to the tray would still leave it
        // listed here as if it had an open window.
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
        }

        // Owned windows (dialogs, tool palettes, etc.) are not top-level
        // application windows for this first-iteration model.
        if GetWindow(hwnd, GW_OWNER).map(|owner| owner.0 as isize).unwrap_or(0) != 0 {
            return None;
        }

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }

        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied == 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..copied as usize]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }

        Some(WindowRecord {
            hwnd: hwnd.0 as isize,
            title,
            pid,
            exe_name: exe_name_for_pid(pid),
            rect: window_rect(hwnd),
        })
    }
}

/// Best-effort on-screen bounds for `hwnd`. Minimized windows report a
/// nonsensical off-screen `GetWindowRect` (classically `(-32000, -32000)`),
/// so those fall back to `GetWindowPlacement`'s saved restore rect instead.
/// Otherwise, `DWMWA_EXTENDED_FRAME_BOUNDS` is preferred over a raw
/// `GetWindowRect` because it excludes the invisible resize-border padding
/// many apps have, which would otherwise make the overview's thumbnail
/// slightly larger than the window's visible content.
fn window_rect(hwnd: HWND) -> Rect {
    // SAFETY: `hwnd` is valid for the duration of this synchronous call,
    // same as every other inspection in this module.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(hwnd, &mut placement).is_ok() {
                return placement.rcNormalPosition.into();
            }
        }

        let mut rect = RECT::default();
        let extended_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut std::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok();
        if !extended_ok {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        rect.into()
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

fn exe_name_for_pid(pid: u32) -> Option<String> {
    // SAFETY: `pid` is a live process id obtained from
    // `GetWindowThreadProcessId` immediately before this call. The handle
    // is closed via `CloseHandle` before returning on every path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );

        let _ = windows::Win32::Foundation::CloseHandle(handle);

        result.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}
