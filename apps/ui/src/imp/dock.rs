//! The Activities overview's dock: GNOME-dash-style row of app icons
//! along the bottom of the focused card, built fresh each time the
//! overview opens or rebuilds. Overview-only by design (see the
//! project's memory on this decision) — there is no always-visible
//! desktop dock.
//!
//! Two sources feed it, mirrored rather than owned by us, since there's
//! no settings UI yet to manage pins directly:
//! - **Pinned apps**: read straight from the real Windows taskbar's own
//!   pinned-shortcut folder, so it reflects whatever the user already
//!   pinned to their (now-hidden) taskbar.
//! - **Running-but-unpinned apps**: one entry per distinct executable
//!   among currently-tracked windows that didn't already match a
//!   pinned shortcut, so nothing running is ever left off the dock.
//!
//! A click launches (pinned, not running) or focuses (running — the
//! first tracked window for that app); there's no jump-menu/right-click
//! yet (§10.3's fuller spec), no reordering, and no pin/unpin — all
//! deferred along with the rest of "settings" per the project's scope
//! decision for this pass.

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

use super::state::scaled;

/// 96-DPI dock layout metrics.
const DOCK_ICON_SIZE: i32 = 44;
const DOCK_ICON_GAP: i32 = 14;
const DOCK_PADDING_X: i32 = 14;
const DOCK_PADDING_Y: i32 = 10;
/// Gap between the dock bar's bottom edge and the bottom of the card
/// margin reserved for it (see `CARD_MARGIN_BOTTOM` in `overview.rs`).
const DOCK_MARGIN_BOTTOM: i32 = 20;
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
}

fn pinned_icon(lnk_path: &Path) -> Option<HICON> {
    if let Some(cached) = PINNED_ICON_CACHE.with(|c| c.borrow().get(lnk_path).copied()) {
        return Some(HICON(cached as *mut c_void));
    }
    let wide: Vec<u16> = lnk_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
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
    PINNED_ICON_CACHE.with(|c| c.borrow_mut().insert(lnk_path.to_path_buf(), info.hIcon.0 as isize));
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

    for lnk in taskbar_pinned_shortcuts() {
        if apps.len() >= DOCK_MAX_APPS {
            break;
        }
        let target_exe = resolve_shortcut_target(&lnk)
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
        apps.push(DockApp {
            icon: pinned_icon(&lnk),
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

/// Activates the dock entry at `index`: focuses its first tracked
/// window if it has one (switching that window's workspace into view
/// first, same as clicking a search result), otherwise launches its
/// pinned shortcut. No-op if the index is stale (dock rebuilt or
/// overview closed between hover and click) or the entry somehow has
/// neither.
pub(crate) fn activate_dock_app(monitor: &str, index: usize) {
    let target = super::state::STATE.with(|s| {
        let state = s.borrow();
        let st = state.as_ref()?;
        let ov = st.overviews.get(monitor)?;
        let app = ov.dock_apps.get(index)?;
        if let Some(&hwnd) = app.windows.first() {
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
