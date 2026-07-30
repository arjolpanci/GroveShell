//! The Activities overview's dock: GNOME-dash-style row of app icons
//! along the bottom of the focused card, built fresh each time the
//! overview opens or rebuilds. Overview-only by design (see the
//! project's memory on this decision) — there is no always-visible
//! desktop dock.
//!
//! Two sources feed it:
//! - **Pinned apps**: GroveShell's own persisted list (see
//!   `dock_pins.rs`), seeded once from the real Windows taskbar's pinned
//!   shortcuts on first run and independent of it from then on — pinning
//!   or unpinning here never touches the real taskbar.
//! - **Running-but-unpinned apps**: one entry per distinct executable
//!   among currently-tracked windows that didn't already match a
//!   pinned shortcut, so nothing running is ever left off the dock.
//!
//! A click focuses (running — cycling through its windows on repeat
//! clicks, see `next_window`) or launches (pinned, not running) an
//! entry. Right-click offers pin/unpin and "open new window" for pinned
//! entries (see `show_context_menu`). Dragging a **pinned** entry
//! reorders the dock, or (dropped on a workspace card) launches it
//! assigned to that workspace (see `DockDrag`/`on_dock_drag_end`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::HICON;
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{SHGetFileInfoW, ShellExecuteW, IShellLinkW, ShellLink, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
    MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
};

use super::state::scaled;

/// 96-DPI dock layout metrics.
const DOCK_ICON_SIZE: i32 = 44;
const DOCK_ICON_GAP: i32 = 14;
const DOCK_PADDING_X: i32 = 14;
const DOCK_PADDING_Y: i32 = 10;
/// Gap between the dock bar's bottom edge and the bottom of the card
/// margin reserved for it (see `CARD_MARGIN_BOTTOM` in `overview.rs`) —
/// small and GNOME-dash-like, sitting close to the screen's bottom edge
/// rather than floating in the middle of the reserved margin band.
const DOCK_MARGIN_BOTTOM: i32 = 8;
pub(crate) const DOCK_CORNER_RADIUS: i32 = 18;
const DOCK_RUNNING_DOT_RADIUS: i32 = 3;
/// Hard cap so a very cluttered taskbar/desktop can't turn the dock
/// into an unreadable strip spanning the whole card.
const DOCK_MAX_APPS: usize = 14;

/// One dock entry: a pinned shortcut, a running-but-unpinned app, or
/// both at once (a pinned app that's also currently running).
pub(crate) struct DockApp {
    pub(crate) icon: Option<HICON>,
    /// The pinned shortcut's own path, `ShellExecute`-launched directly
    /// (letting Explorer's own shortcut resolution handle working
    /// directory, arguments, elevation prompts, and so on) — `None` for
    /// a running-but-unpinned entry, which can only ever be focused.
    pub(crate) launch_path: Option<PathBuf>,
    /// Currently-tracked windows for this app, if any; a click focuses
    /// the first one instead of launching when this is non-empty.
    pub(crate) windows: Vec<isize>,
}

/// The page-local dock bar rect and each icon's slot within it —
/// horizontally centered under the focused card, sitting in the
/// bottom-margin gap `card_layout` already reserves for it. Self-
/// contained like `card_layout`, for the same reason: cheap to
/// recompute, and this module doesn't handle live display-topology
/// changes either.
pub(crate) fn dock_layout(monitor: &str, count: usize) -> (RECT, Vec<RECT>) {
    let (card_rect, _) = super::overview::card_layout(monitor);
    let dpi = super::state::reference_dpi();
    let icon_size = scaled(DOCK_ICON_SIZE, dpi);
    let gap = scaled(DOCK_ICON_GAP, dpi);
    let pad_x = scaled(DOCK_PADDING_X, dpi);
    let pad_y = scaled(DOCK_PADDING_Y, dpi);

    let count = count.max(1) as i32;
    let content_w = count * icon_size + (count - 1).max(0) * gap;
    let bar_w = content_w + pad_x * 2;
    let bar_h = icon_size + pad_y * 2;

    let cx = (card_rect.left + card_rect.right) / 2;
    let bar_left = cx - bar_w / 2;
    let bar_bottom = card_rect.bottom + scaled(super::overview::CARD_MARGIN_BOTTOM, dpi)
        - scaled(DOCK_MARGIN_BOTTOM, dpi);
    let bar_top = bar_bottom - bar_h;

    let bar_rect = RECT {
        left: bar_left,
        top: bar_top,
        right: bar_left + bar_w,
        bottom: bar_bottom,
    };
    let slots = (0..count)
        .map(|i| {
            let left = bar_left + pad_x + i * (icon_size + gap);
            RECT {
                left,
                top: bar_top + pad_y,
                right: left + icon_size,
                bottom: bar_top + pad_y + icon_size,
            }
        })
        .collect();
    (bar_rect, slots)
}

/// Whether a dock entry should show a running-indicator dot beneath it.
pub(crate) fn dock_running_dot_radius() -> i32 {
    scaled(DOCK_RUNNING_DOT_RADIUS, super::state::reference_dpi())
}

thread_local! {
    /// Icons extracted from pinned `.lnk` files via `SHGetFileInfoW`,
    /// keyed by shortcut path. Unlike `window_icon`'s borrowed handles,
    /// these are ours to free — but since the pinned set is small
    /// (typically under twenty) and stable for the process's lifetime,
    /// they're simply cached forever rather than churned every time the
    /// overview rebuilds, the same tradeoff `WALLPAPER_BITMAP` makes.
    static PINNED_ICON_CACHE: RefCell<HashMap<PathBuf, isize>> = RefCell::new(HashMap::new());

    /// GroveShell's own pinned-app list — the authoritative source for
    /// `build_dock_apps`'s pinned entries from now on (see
    /// `dock_pins.rs`'s module doc for why this isn't the real
    /// taskbar's pin folder anymore). Loaded once at startup via
    /// `init_pinned_list`.
    static PINNED_PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Loads the persisted pinned list (seeding it from the real taskbar's
/// current pins if this is the first run) into `PINNED_PATHS`. Must run
/// once, at startup, before the first `build_dock_apps` call.
pub(crate) fn init_pinned_list() {
    let Some(path) = super::dock_pins::pins_file_path() else { return };
    let pins = super::dock_pins::load_or_seed(&path, taskbar_pinned_shortcuts);
    PINNED_PATHS.with(|p| *p.borrow_mut() = pins);
}

/// The current pinned list, in order — read by `build_dock_apps`.
pub(crate) fn pinned_paths() -> Vec<PathBuf> {
    PINNED_PATHS.with(|p| p.borrow().clone())
}

fn persist_pinned_paths(pins: &[PathBuf]) {
    if let Some(path) = super::dock_pins::pins_file_path() {
        super::dock_pins::save(&path, pins);
    }
}

/// Adds `path` to the end of the pinned list (a no-op if already
/// pinned) and persists it.
pub(crate) fn pin_app(path: PathBuf) {
    PINNED_PATHS.with(|p| {
        let mut pins = p.borrow_mut();
        if !pins.contains(&path) {
            pins.push(path);
            persist_pinned_paths(&pins);
        }
    });
}

/// Removes `path` from the pinned list, if present, and persists it.
pub(crate) fn unpin_app(path: &Path) {
    PINNED_PATHS.with(|p| {
        let mut pins = p.borrow_mut();
        if let Some(i) = pins.iter().position(|p| p == path) {
            pins.remove(i);
            persist_pinned_paths(&pins);
        }
    });
}

/// Icon for any file path, via `SHGetFileInfoW` — cached forever per
/// path, same tradeoff as `PINNED_ICON_CACHE`'s doc comment explains.
/// Also usable for search results' app icons, not just the dock.
pub(crate) fn file_icon(path: &Path) -> Option<HICON> {
    if let Some(cached) = PINNED_ICON_CACHE.with(|c| c.borrow().get(path).copied()) {
        return Some(HICON(cached as *mut c_void));
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is nul-terminated and outlives the call; `info` is
    // a local, zeroed struct outliving it too. The returned `hIcon`, on
    // success, is a new icon this process owns.
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || info.hIcon.is_invalid() {
        return None;
    }
    PINNED_ICON_CACHE.with(|c| c.borrow_mut().insert(path.to_path_buf(), info.hIcon.0 as isize));
    Some(info.hIcon)
}

/// Resolves a `.lnk` shortcut's real target path via `IShellLinkW`,
/// rather than trying to match running windows against the shortcut
/// file itself — the target's executable name is what actually shows
/// up as a window's `exe_name`.
fn resolve_shortcut_target(lnk_path: &Path) -> Option<PathBuf> {
    // SAFETY: every call here is synchronous and its result fully
    // consumed before returning; no aliasing or lifetime hazards beyond
    // the ordinary COM contract already relied on elsewhere (volume
    // control uses the same pattern).
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist_file: IPersistFile = shell_link.cast().ok()?;
        let wide: Vec<u16> = lnk_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        persist_file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;

        let mut buf = [0u16; 260];
        let mut find_data = WIN32_FIND_DATAW::default();
        shell_link.GetPath(&mut buf, &mut find_data, 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

fn taskbar_pinned_shortcuts() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = PathBuf::from(appdata).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut shortcuts: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk")))
        .collect();
    shortcuts.sort();
    shortcuts
}

/// Builds the dock's contents fresh: pinned shortcuts (in whatever
/// order the filesystem lists them — matching the real taskbar's exact
/// pin order isn't attempted, since that lives in an undocumented
/// registry blob) first, each annotated with any currently-tracked
/// windows that resolve to the same target executable, followed by one
/// entry per remaining running app that has no pinned shortcut at all.
pub(crate) fn build_dock_apps(live: &[groveshell_window_model::WindowRecord]) -> Vec<DockApp> {
    let mut apps = Vec::new();
    let mut claimed = vec![false; live.len()];

    for lnk in pinned_paths() {
        if apps.len() >= DOCK_MAX_APPS {
            break;
        }
        let target_path = resolve_shortcut_target(&lnk);
        let target_exe = target_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        let windows: Vec<isize> = match &target_exe {
            Some(exe) => live
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    let matches = !claimed[i]
                        && w.exe_name.as_deref().is_some_and(|e| e.to_lowercase() == *exe);
                    if matches {
                        claimed[i] = true;
                    }
                    matches.then_some(w.hwnd)
                })
                .collect(),
            None => Vec::new(),
        };
        // The target executable's own icon has no shortcut-arrow overlay,
        // unlike asking the shell for the `.lnk` file's icon directly;
        // fall back to the `.lnk` itself only if resolving the target
        // failed (still an icon, just possibly with the overlay).
        let icon = target_path.as_deref().and_then(file_icon).or_else(|| file_icon(&lnk));
        apps.push(DockApp {
            icon,
            launch_path: Some(lnk),
            windows,
        });
    }

    // Remaining (unclaimed) windows, grouped by exe — one dock entry
    // per distinct running app that isn't already pinned.
    let mut seen_exes: Vec<String> = Vec::new();
    for (i, window) in live.iter().enumerate() {
        if apps.len() >= DOCK_MAX_APPS {
            break;
        }
        if claimed[i] {
            continue;
        }
        let Some(exe) = window.exe_name.as_deref() else {
            continue;
        };
        let exe_lower = exe.to_lowercase();
        if seen_exes.contains(&exe_lower) {
            continue;
        }
        seen_exes.push(exe_lower.clone());
        let windows: Vec<isize> = live
            .iter()
            .enumerate()
            .filter(|(j, w)| {
                !claimed[*j] && w.exe_name.as_deref().is_some_and(|e| e.to_lowercase() == exe_lower)
            })
            .map(|(_, w)| w.hwnd)
            .collect();
        let icon = super::overview::window_icon(HWND(window.hwnd as *mut c_void));
        apps.push(DockApp {
            icon,
            launch_path: None,
            windows,
        });
    }

    apps
}

thread_local! {
    /// Last-focused window per dock entry, keyed by the entry's
    /// lowercased exe name (the same key `build_dock_apps` already uses
    /// to group windows into one entry, for both pinned and running-
    /// unpinned entries) — lets repeat clicks on a multi-window app
    /// cycle through its windows instead of always refocusing the
    /// first one. Cleared implicitly by just going stale (an exe key
    /// for an app that's no longer running simply never gets read
    /// again); no explicit eviction needed.
    static LAST_FOCUSED: RefCell<HashMap<String, isize>> = RefCell::new(HashMap::new());
}

/// The window to focus next in `windows`, given whichever one was last
/// focused (or `None` if this entry has never been clicked, or its
/// previously-focused window closed) — advances past `last_focused` and
/// wraps around; falls back to the first window if `last_focused` isn't
/// (or no longer is) in `windows`.
pub(crate) fn next_window(windows: &[isize], last_focused: Option<isize>) -> Option<isize> {
    if windows.is_empty() {
        return None;
    }
    let Some(last) = last_focused else {
        return Some(windows[0]);
    };
    match windows.iter().position(|&w| w == last) {
        Some(i) => Some(windows[(i + 1) % windows.len()]),
        None => Some(windows[0]),
    }
}

/// Activates the dock entry at `index`: focuses its next tracked
/// window (cycling past whichever one was last focused, if any —
/// switching that window's workspace into view first, same as clicking
/// a search result) if it has one, otherwise launches its pinned
/// shortcut. No-op if the index is stale (dock rebuilt or overview
/// closed between hover and click) or the entry somehow has neither.
pub(crate) fn activate_dock_app(monitor: &str, index: usize) {
    let target = super::state::STATE.with(|s| {
        let state = s.borrow();
        let st = state.as_ref()?;
        let ov = st.overviews.get(monitor)?;
        let app = ov.dock_apps.get(index)?;
        if !app.windows.is_empty() {
            let exe_key = app
                .windows
                .first()
                .and_then(|&hwnd| groveshell_window_model::describe(hwnd))
                .and_then(|w| w.exe_name)
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            let last = LAST_FOCUSED.with(|m| m.borrow().get(&exe_key).copied());
            let hwnd = next_window(&app.windows, last).unwrap();
            LAST_FOCUSED.with(|m| {
                m.borrow_mut().insert(exe_key, hwnd);
            });
            let tracker = st.workspaces.get(monitor);
            let id = tracker.and_then(|t| t.workspace_of(hwnd));
            let page = id.and_then(|id| tracker.and_then(|t| t.index_of(id)));
            let current = tracker.map(|t| t.current_index()).unwrap_or(0);
            Some((Some(hwnd), page, current, None))
        } else {
            app.launch_path.clone().map(|path| (None, None, 0, Some(path)))
        }
    });
    let Some((hwnd, page, current, launch_path)) = target else {
        return;
    };

    if let Some(hwnd) = hwnd {
        let handle = HWND(hwnd as *mut c_void);
        match page {
            Some(page) if page != current => super::overview::snap_carousel_to(monitor, page, Some(handle)),
            _ => super::overview::close_overview(monitor, Some(handle)),
        }
        return;
    }

    if let Some(path) = launch_path {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is nul-terminated and outlives the call;
        // ShellExecuteW is fire-and-forget here, same as search's app
        // launch path.
        unsafe {
            let _ = ShellExecuteW(
                HWND(std::ptr::null_mut()),
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
        super::overview::close_overview(monitor, None);
    }
}

const MENU_ID_UNPIN: u32 = 1;
const MENU_ID_PIN: u32 = 2;
const MENU_ID_OPEN_NEW_WINDOW: u32 = 3;

/// Shows the right-click context menu for the dock entry at `index`
/// (already resolved by the caller via `dock_layout`'s slot hit-test),
/// then performs whichever action was chosen. A running-but-unpinned
/// entry (no `launch_path`) gets no menu at all — right-clicking it is
/// a no-op, matching today's behavior — since there's no shortcut to
/// pin or relaunch from a bare running-window entry.
pub(crate) fn show_context_menu(monitor: &str, overview_hwnd: HWND, index: usize) {
    let Some(app_launch_path) = super::state::STATE.with(|s| {
        let state = s.borrow();
        let ov = state.as_ref()?.overviews.get(monitor)?;
        ov.dock_apps.get(index)?.launch_path.clone()
    }) else {
        return;
    };

    // SAFETY: every call here is a standard, synchronous Win32 popup-menu
    // sequence; `menu` is destroyed before returning on every path.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let is_pinned = pinned_paths().contains(&app_launch_path);
        let pin_label = if is_pinned { w!("Unpin from dock") } else { w!("Pin to dock") };
        let pin_id = if is_pinned { MENU_ID_UNPIN } else { MENU_ID_PIN };
        let _ = AppendMenuW(menu, MF_STRING, pin_id as usize, pin_label);
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_OPEN_NEW_WINDOW as usize, w!("Open new window"));

        let mut point = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(overview_hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            point.x,
            point.y,
            0,
            overview_hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as u32 {
            MENU_ID_UNPIN => unpin_app(&app_launch_path),
            MENU_ID_PIN => pin_app(app_launch_path),
            MENU_ID_OPEN_NEW_WINDOW => {
                let wide: Vec<u16> =
                    app_launch_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                let _ = ShellExecuteW(
                    HWND(std::ptr::null_mut()),
                    w!("open"),
                    PCWSTR(wide.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
            _ => {}
        }
    }
    super::overview::rebuild_open_overview_pages(monitor);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_window_with_no_last_focused_returns_the_first() {
        assert_eq!(next_window(&[10, 20, 30], None), Some(10));
    }

    #[test]
    fn next_window_advances_past_the_last_focused() {
        assert_eq!(next_window(&[10, 20, 30], Some(10)), Some(20));
    }

    #[test]
    fn next_window_wraps_around_after_the_last_entry() {
        assert_eq!(next_window(&[10, 20, 30], Some(30)), Some(10));
    }

    #[test]
    fn next_window_falls_back_to_first_if_last_focused_is_gone() {
        assert_eq!(next_window(&[10, 20, 30], Some(999)), Some(10));
    }

    #[test]
    fn next_window_with_no_windows_returns_none() {
        assert_eq!(next_window(&[], Some(10)), None);
    }
}
