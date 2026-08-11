#![cfg(windows)]

//! A minimal, read-only snapshot of top-level windows, per
//! `docs/PROJECT_PLAN.md` §6.2 (eligible top-level window policy) and
//! Phase 1's "reliable, inspectable model of top-level windows" goal. This
//! is a synchronous `EnumWindows` pass with no caching, hooks, or lifecycle
//! tracking yet — just enough for the Phase 4/5 Activities overview to show
//! something real. `WindowId` generation-counter identity, WinEvent-driven
//! updates, and the full eligibility/diagnostic model described in §6 are
//! deliberately out of scope here and remain a follow-up.

pub mod registry;
pub mod workspace;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WINDOWPLACEMENT, WS_EX_TOOLWINDOW,
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
    /// Win32 window class name (e.g. `"Chrome_WidgetWin_1"`). Always
    /// present — an empty string if `GetClassNameW` failed. Used by the
    /// compatibility ignore list to match windows by class, and useful as
    /// a diagnostic when exe identity is unavailable (elevated windows).
    pub class: String,
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

    if let Some(record) = inspect(hwnd, true) {
        records.push(record);
    }

    TRUE
}

/// Re-inspects a single, previously-known window by handle, bypassing the
/// `IsWindowVisible` gate that [`snapshot`] applies. Used by workspace
/// switching (see [`workspace`]): a window parked on an inactive workspace
/// is hidden via `ShowWindow(SW_HIDE)`, so a fresh `snapshot()` would never
/// see it again even though it's still eligible and still assigned to a
/// workspace. Returns `None` if the handle no longer refers to a live,
/// eligible top-level window (closed, or reused for something ineligible —
/// see the module docs on HWND reuse; this crate has no generation-counter
/// identity yet, so a rare same-tick reuse can't be distinguished here).
pub fn describe(hwnd: isize) -> Option<WindowRecord> {
    inspect(HWND(hwnd as *mut std::ffi::c_void), false)
}

/// One connected display as of the moment [`monitors`] was called.
#[derive(Debug, Clone, Copy)]
pub struct MonitorRecord {
    /// Full monitor bounds in virtual-screen coordinates.
    pub rect: Rect,
    /// The monitor's work area (bounds minus taskbar/AppBar reservations).
    pub work: Rect,
    pub is_primary: bool,
    /// Effective DPI of this monitor (96 = 100% scale, 144 = 150%, …).
    /// `96` if the query failed. Combine with [`scale_for_dpi`] to get the
    /// scale factor. Meaningful only in a per-monitor-DPI-aware process
    /// (see [`make_process_dpi_aware`]).
    pub dpi: u32,
}

/// Enumerates connected monitors. Coordinates are physical pixels only if
/// the calling process is per-monitor DPI aware (see
/// [`make_process_dpi_aware`]); a DPI-unaware caller gets virtualized
/// values.
pub fn monitors() -> Vec<MonitorRecord> {
    unsafe extern "system" fn monitor_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` is the address of the local `Vec` below, only
        // used during this synchronous enumeration.
        let out = &mut *(lparam.0 as *mut Vec<MonitorRecord>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            const MONITORINFOF_PRIMARY: u32 = 1;
            out.push(MonitorRecord {
                rect: info.rcMonitor.into(),
                work: info.rcWork.into(),
                is_primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                dpi: effective_dpi(hmonitor),
            });
        }
        TRUE
    }

    let mut records: Vec<MonitorRecord> = Vec::new();
    // SAFETY: `records` outlives the synchronous enumeration; see
    // `snapshot` for the identical pattern.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_proc),
            LPARAM(&mut records as *mut Vec<MonitorRecord> as isize),
        );
    }
    records
}

/// Opts the calling process into per-monitor DPI awareness so window and
/// monitor rects come back in real physical pixels. Must run before any
/// DPI-sensitive call; harmless if it fails (coordinates just come back
/// virtualized). Exposed here so CLI consumers don't need their own
/// `windows` dependency for one call.
pub fn make_process_dpi_aware() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // SAFETY: plain process-wide mode switch, documented to fail (not
    // misbehave) if a DPI context was already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Effective DPI of one monitor via `GetDpiForMonitor`, or `96` (100%) if
/// the query fails. Pulled out so [`monitors`] and any future per-monitor
/// layout code share one definition of "this display's DPI".
fn effective_dpi(hmonitor: HMONITOR) -> u32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let mut dpi_x: u32 = 96;
    let mut dpi_y: u32 = 96;
    // SAFETY: `hmonitor` is a live handle supplied by `EnumDisplayMonitors`
    // for the duration of the enclosing synchronous enumeration; both out
    // pointers are to locals that outlive the call. Documented to fail
    // (leaving the locals untouched) rather than misbehave.
    unsafe {
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    dpi_x
}

/// Scale factor for a DPI value: `96` DPI → `1.0`, `144` → `1.5`, etc.
/// The one place logical↔physical conversions get their multiplier, so the
/// convention lives in exactly one tested spot.
pub fn scale_for_dpi(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

/// Convert a logical (96-DPI) pixel length to physical pixels at `dpi`,
/// rounded to the nearest integer — the direction used when placing a
/// fixed-size shell element (bar height, dock icon) on a scaled monitor.
pub fn logical_to_physical(logical: i32, dpi: u32) -> i32 {
    (logical as f32 * scale_for_dpi(dpi)).round() as i32
}

/// Convert a physical pixel length at `dpi` back to logical (96-DPI)
/// pixels — the direction used when normalizing a window rect read off a
/// scaled monitor into resolution-independent units.
pub fn physical_to_logical(physical: i32, dpi: u32) -> i32 {
    (physical as f32 / scale_for_dpi(dpi)).round() as i32
}

/// Whether `hwnd` still refers to a live window at all, regardless of
/// eligibility. Used to prune workspace assignments for windows that have
/// since been destroyed.
pub fn is_alive(hwnd: isize) -> bool {
    // SAFETY: `hwnd` is just a numeric value passed to a query API; `IsWindow`
    // documented-returns false for any value that isn't a live window handle.
    unsafe { IsWindow(HWND(hwnd as *mut std::ffi::c_void)).as_bool() }
}

fn inspect(hwnd: HWND, require_visible: bool) -> Option<WindowRecord> {
    // SAFETY: `hwnd` is either supplied by `EnumWindows` (valid for the
    // duration of that synchronous callback frame) or a previously-known
    // handle passed to `describe`, which documented-fails harmlessly below
    // if the handle is no longer live.
    unsafe {
        if require_visible && !IsWindowVisible(hwnd).as_bool() {
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
            class: class_name(hwnd),
            rect: window_rect(hwnd),
        })
    }
}

/// Best-effort Win32 class name for `hwnd`; empty string on failure.
fn class_name(hwnd: HWND) -> String {
    // SAFETY: `hwnd` is valid for the duration of this synchronous call
    // (same contract as every other inspection here); `buf` is a fixed
    // 256-wide buffer and `GetClassNameW` never writes past the length it
    // is given, returning the count actually copied.
    unsafe {
        let mut buf = [0u16; 256];
        let copied = GetClassNameW(hwnd, &mut buf);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..copied as usize])
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

/// Returns every window (top-level or itself owned) that `owner` owns,
/// directly or transitively — e.g. a dialog that itself owns a color
/// picker — unfiltered by visibility, title, or tool-window status, since
/// a not-yet-shown or hidden owned window should still follow its owner.
/// Win32 does not move an owned window when its owner moves (unlike a
/// true child window), so callers use this to carry owned dialogs along
/// during a workspace switch or a cross-monitor drag.
pub fn owned_windows_of(owner: isize) -> Vec<isize> {
    let mut pairs: Vec<(isize, isize)> = Vec::new();
    // SAFETY: `pairs` is a local `Vec` whose address is passed through as
    // `lparam` and only ever read back by `enum_owner_pairs_proc` during
    // this call; `EnumWindows` is synchronous, so `pairs` is guaranteed to
    // outlive every callback invocation.
    unsafe {
        let _ = EnumWindows(
            Some(enum_owner_pairs_proc),
            LPARAM(&mut pairs as *mut Vec<(isize, isize)> as isize),
        );
    }
    owned_windows_from_pairs(owner, &pairs)
}

unsafe extern "system" fn enum_owner_pairs_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` was created from a live `&mut Vec<(isize, isize)>`
    // in `owned_windows_of` and this callback only runs synchronously
    // within that call's lifetime.
    let pairs = &mut *(lparam.0 as *mut Vec<(isize, isize)>);
    let owner = GetWindow(hwnd, GW_OWNER).map(|o| o.0 as isize).unwrap_or(0);
    if owner != 0 {
        pairs.push((hwnd.0 as isize, owner));
    }
    TRUE
}

/// Pure transitive walk: every hwnd in `pairs` whose owner is `root`,
/// directly or through a chain, using a visited set so a pathological
/// owner cycle can't loop forever or include `root` in its own result.
fn owned_windows_from_pairs(root: isize, pairs: &[(isize, isize)]) -> Vec<isize> {
    let mut result = Vec::new();
    let mut frontier = vec![root];
    let mut visited = std::collections::HashSet::new();
    visited.insert(root);
    while let Some(current) = frontier.pop() {
        for &(hwnd, owner) in pairs {
            if owner == current && visited.insert(hwnd) {
                result.push(hwnd);
                frontier.push(hwnd);
            }
        }
    }
    result
}

#[cfg(test)]
mod dpi_tests {
    use super::{logical_to_physical, physical_to_logical, scale_for_dpi};

    #[test]
    fn scale_factor_matches_common_windows_scales() {
        assert_eq!(scale_for_dpi(96), 1.0);
        assert_eq!(scale_for_dpi(120), 1.25);
        assert_eq!(scale_for_dpi(144), 1.5);
        assert_eq!(scale_for_dpi(192), 2.0);
    }

    #[test]
    fn logical_and_physical_convert_in_both_directions() {
        // 32 logical px at 150% is 48 physical px, and back.
        assert_eq!(logical_to_physical(32, 144), 48);
        assert_eq!(physical_to_logical(48, 144), 32);
        // Identity at 100%.
        assert_eq!(logical_to_physical(100, 96), 100);
        assert_eq!(physical_to_logical(100, 96), 100);
    }

    #[test]
    fn conversion_rounds_to_nearest_pixel() {
        // 44 logical px at 125% = 55.0 exactly.
        assert_eq!(logical_to_physical(44, 120), 55);
        // 45 logical px at 125% = 56.25 → 56.
        assert_eq!(logical_to_physical(45, 120), 56);
    }
}

#[cfg(test)]
mod owned_windows_tests {
    use super::owned_windows_from_pairs;

    #[test]
    fn finds_direct_and_transitive_owned_windows() {
        // 1 owns 2; 2 owns 3 (transitive); 4 is owned by an unrelated
        // window (99) and must not appear.
        let pairs = vec![(2, 1), (3, 2), (4, 99)];
        let mut owned = owned_windows_from_pairs(1, &pairs);
        owned.sort();
        assert_eq!(owned, vec![2, 3]);
    }

    #[test]
    fn returns_empty_when_nothing_is_owned() {
        let pairs = vec![(2, 1)];
        assert_eq!(owned_windows_from_pairs(5, &pairs), Vec::<isize>::new());
    }

    #[test]
    fn an_owner_cycle_does_not_infinite_loop() {
        // Pathological: 1 owns 2 and 2 "owns" 1 back. Must terminate and
        // must not include the root itself in its own owned set.
        let pairs = vec![(1, 2), (2, 1)];
        let mut owned = owned_windows_from_pairs(1, &pairs);
        owned.sort();
        assert_eq!(owned, vec![2]);
    }
}
