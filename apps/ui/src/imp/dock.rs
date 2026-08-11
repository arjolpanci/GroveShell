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
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHGetImageList, ShellExecuteW, IShellLinkW, ShellLink, SHFILEINFOW, SHGFI_ICON,
    SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHIL_JUMBO,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
    MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
};
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS};

use super::state::scaled;

/// 96-DPI dock layout metrics.
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
/// Gap on each side of the divider between the pinned section (left)
/// and the running-but-unpinned section (right) — GNOME/macOS-dash-
/// style. Only inserted when both sections are non-empty.
const DOCK_DIVIDER_GAP: i32 = 10;
const DOCK_DIVIDER_WIDTH: i32 = 2;
/// Above this many open windows, an app's dock entry stops adding more
/// running-indicator dots — beyond a few, the dot row stops being
/// useful information and would otherwise grow the dock unboundedly.
pub(crate) const DOCK_MAX_RUNNING_DOTS: usize = 4;
pub(crate) const DOCK_RUNNING_DOT_GAP: i32 = 4;
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

/// How many of `apps` are pinned (left of the divider, GNOME/macOS-
/// dash-style) — `build_dock_apps` always places pinned entries first,
/// so this is just a count, not a search.
pub(crate) fn pinned_count(apps: &[DockApp]) -> usize {
    apps.iter().filter(|a| a.launch_path.is_some()).count()
}

/// The dock bar's left edge given a work area's horizontal span, the
/// bar's total content width, and a horizontal alignment — "left"/"right"
/// hug the work area's exact edge; any unrecognized alignment string
/// falls back to "center" rather than panicking, since `Config::validate()`
/// is the actual gate on invalid values and this function must stay total.
pub(crate) fn anchor_x(work_area_left: i32, work_area_right: i32, content_w: i32, alignment: &str) -> i32 {
    match alignment {
        "left" => work_area_left,
        "right" => (work_area_right - content_w).max(work_area_left),
        _ => (work_area_left + work_area_right) / 2 - content_w / 2,
    }
}

/// The page-local dock bar rect, each icon's slot within it, and (if
/// there's at least one pinned entry *and* at least one running-but-
/// unpinned entry) the vertical divider rect separating them —
/// horizontally centered under the focused card, sitting in the
/// bottom-margin gap `card_layout` already reserves for it. Self-
/// contained like `card_layout`, for the same reason: cheap to
/// recompute, and this module doesn't handle live display-topology
/// changes either. `pinned_count` must be the number of the first
/// `pinned_count` slots (by index) that are pinned entries — the rest
/// are running-unpinned, matching `pinned_count()`'s own contract.

pub(crate) fn dock_layout(monitor: &str, pinned_count: usize, total_count: usize) -> (RECT, Vec<RECT>, Option<RECT>) {
    let (card_rect, _) = super::overview::card_layout(monitor);
    let dpi = super::state::reference_dpi();
    // Reads the re-entrancy-safe mirror, never `STATE` directly — this is
    // called from deep inside code that may already hold `STATE`'s borrow
    // (e.g. `overview::on_overview_hover`), and re-borrowing it here
    // panicked every time the mouse moved over an open overview. See
    // `state::DOCK_ICON_SIZE`'s doc comment.
    let (icon_size_cfg, alignment) = super::state::dock_config();
    let icon_size = scaled(icon_size_cfg, dpi);
    let gap = scaled(DOCK_ICON_GAP, dpi);
    let pad_x = scaled(DOCK_PADDING_X, dpi);
    let pad_y = scaled(DOCK_PADDING_Y, dpi);
    let divider_gap = scaled(DOCK_DIVIDER_GAP, dpi);
    let divider_width = scaled(DOCK_DIVIDER_WIDTH, dpi);

    let count = total_count.max(1) as i32;
    let has_divider = pinned_count > 0 && pinned_count < total_count;
    let content_w = count * icon_size
        + (count - 1).max(0) * gap
        + if has_divider { divider_gap * 2 + divider_width } else { 0 };
    let bar_w = content_w + pad_x * 2;
    let bar_h = icon_size + pad_y * 2;

    let bar_left = anchor_x(card_rect.left, card_rect.right, bar_w, &alignment);
    let bar_bottom = card_rect.bottom + scaled(super::overview::CARD_MARGIN_BOTTOM, dpi)
        - scaled(DOCK_MARGIN_BOTTOM, dpi);
    let bar_top = bar_bottom - bar_h;

    let bar_rect = RECT {
        left: bar_left,
        top: bar_top,
        right: bar_left + bar_w,
        bottom: bar_bottom,
    };

    let mut slots = Vec::with_capacity(count as usize);
    let mut divider_rect = None;
    let mut x = bar_left + pad_x;
    for i in 0..count {
        if has_divider && i as usize == pinned_count {
            x += divider_gap;
            divider_rect = Some(RECT {
                left: x,
                top: bar_top + pad_y / 2,
                right: x + divider_width,
                bottom: bar_bottom - pad_y / 2,
            });
            x += divider_width + divider_gap;
        }
        slots.push(RECT {
            left: x,
            top: bar_top + pad_y,
            right: x + icon_size,
            bottom: bar_top + pad_y + icon_size,
        });
        x += icon_size + gap;
    }
    (bar_rect, slots, divider_rect)
}

/// Whether a dock entry should show a running-indicator dot beneath it.
pub(crate) fn dock_running_dot_radius() -> i32 {
    scaled(DOCK_RUNNING_DOT_RADIUS, super::state::reference_dpi())
}

/// Rects for up to `DOCK_MAX_RUNNING_DOTS` running-indicator dots
/// beneath `slot` (an icon's own rect), evenly spaced and centered
/// under the icon — GNOME/macOS-dash-style: one dot per open window,
/// capped so a very multi-windowed app doesn't grow the dock. Pure
/// (radius/gap supplied by the caller, already DPI-scaled) so it's
/// testable without a live monitor.
pub(crate) fn running_dot_rects(slot: RECT, window_count: usize, radius: i32, gap: i32) -> Vec<RECT> {
    let n = window_count.min(DOCK_MAX_RUNNING_DOTS);
    if n == 0 {
        return Vec::new();
    }
    let diameter = radius * 2;
    let total_w = n as i32 * diameter + (n as i32 - 1).max(0) * gap;
    let cx = (slot.left + slot.right) / 2;
    let start_x = cx - total_w / 2;
    let dot_y = slot.bottom + radius + 2;
    (0..n as i32)
        .map(|i| {
            let dx = start_x + i * (diameter + gap);
            RECT {
                left: dx,
                top: dot_y - radius,
                right: dx + diameter,
                bottom: dot_y + radius,
            }
        })
        .collect()
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
/// current pins if this is the first run — falling back to
/// `default_pins()` if the real taskbar has nothing pinned either, so a
/// fresh profile never starts with a genuinely empty dock) into
/// `PINNED_PATHS`. Must run once, at startup, before the first
/// `build_dock_apps` call.
pub(crate) fn init_pinned_list() {
    let Some(path) = super::dock_pins::pins_file_path() else { return };
    let pins = super::dock_pins::load_or_seed(&path, || {
        let taskbar = taskbar_pinned_shortcuts();
        if taskbar.is_empty() {
            default_pins()
        } else {
            taskbar
        }
    });
    PINNED_PATHS.with(|p| *p.borrow_mut() = pins);
}

/// Sensible defaults when there's nothing to seed the pinned list from
/// (a fresh profile with no real taskbar pins found): File Explorer, a
/// browser (Edge, since it ships with every Windows install), and
/// Windows Settings — each included only if it actually exists on this
/// machine, since a broken pin pointing at nothing would be worse than
/// an empty dock.
fn default_pins() -> Vec<PathBuf> {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let explorer = windir.join("explorer.exe");
    let settings = windir.join(r"ImmersiveControlPanel\SystemSettings.exe");
    let edge = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .map(|p| p.join(r"Microsoft\Edge\Application\msedge.exe"))
        .find(|p| p.exists());

    [Some(explorer), edge, Some(settings)]
        .into_iter()
        .flatten()
        .filter(|p| p.exists())
        .collect()
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

/// The shell's 256x256 "jumbo" icon for `path`, via the system image
/// list — much higher resolution than `SHGFI_LARGEICON`'s ~32x32, which
/// left dock/search icons visibly blocky once stretched up to the
/// configured `DOCK_ICON_SIZE` (44px and up, more at higher DPI or a
/// larger configured size). `icon_to_hbitmap`/D2D's linear interpolation
/// downscale a jumbo icon cleanly; GDI's `DrawIconEx` upscaling a small
/// one cannot. `None` if the path has no shell icon or the jumbo image
/// list is unavailable (older Windows) — `file_icon` falls back to the
/// smaller icon in that case.
fn jumbo_icon(path: &Path) -> Option<HICON> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is nul-terminated and outlives the call; `info` is a
    // local, zeroed struct outliving it too. `SHGFI_SYSICONINDEX` (no
    // `SHGFI_ICON`) only fills `info.iIcon`, allocating no icon handle.
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if result == 0 {
        return None;
    }
    // SAFETY: `SHGetImageList` is a standard, precondition-free shell32
    // query; the returned `IImageList` is released when it goes out of
    // scope. `GetIcon`'s returned `HICON`, on success, is a new icon
    // this process owns.
    unsafe {
        let image_list: IImageList = SHGetImageList(SHIL_JUMBO as i32).ok()?;
        let icon = image_list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?;
        (!icon.is_invalid()).then_some(icon)
    }
}

/// Icon for any file path — the shell's jumbo icon (`jumbo_icon`) if
/// available, falling back to `SHGetFileInfoW`'s smaller large icon
/// otherwise. Cached forever per path, same tradeoff as
/// `PINNED_ICON_CACHE`'s doc comment explains. Also usable for search
/// results' app icons, not just the dock.
pub(crate) fn file_icon(path: &Path) -> Option<HICON> {
    if let Some(cached) = PINNED_ICON_CACHE.with(|c| c.borrow().get(path).copied()) {
        return Some(HICON(cached as *mut c_void));
    }
    if let Some(icon) = jumbo_icon(path) {
        PINNED_ICON_CACHE.with(|c| c.borrow_mut().insert(path.to_path_buf(), icon.0 as isize));
        return Some(icon);
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
/// up as a window's `exe_name`. If `lnk_path` isn't actually a `.lnk`
/// (e.g. one of `default_pins`'s raw `.exe` paths), it already *is* the
/// target — returned as-is rather than fed through `IShellLinkW`, which
/// only understands real shortcut files.
fn resolve_shortcut_target(lnk_path: &Path) -> Option<PathBuf> {
    if !lnk_path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
        return Some(lnk_path.to_path_buf());
    }
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

/// Icon via the shortcut's own explicit icon location
/// (`IShellLinkW::GetIconLocation`), extracted directly from that
/// resource with `ExtractIconExW` — bypasses the shell's icon-decoration
/// pipeline entirely (that's what adds the arrow overlay for `.lnk`
/// files, and what `file_icon`-on-the-resolved-target can't reach for a
/// shortcut like File Explorer's, whose target path resolves to nothing
/// useful but which sets an explicit icon location instead). `None` if
/// the shortcut has no icon location set (common for ordinary app
/// shortcuts, which fall back to the resolved target's icon instead —
/// see `build_dock_apps`) or if extraction fails.
fn shortcut_icon(lnk_path: &Path) -> Option<HICON> {
    // SAFETY: same COM contract as `resolve_shortcut_target`; every
    // handle here is either released by its own COM wrapper or, for the
    // extracted `HICON`, owned by the caller from here on.
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist_file: IPersistFile = shell_link.cast().ok()?;
        let wide: Vec<u16> = lnk_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        persist_file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;

        let mut icon_path_buf = [0u16; 260];
        let mut icon_index: i32 = 0;
        shell_link.GetIconLocation(&mut icon_path_buf, &mut icon_index).ok()?;
        let len = icon_path_buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }
        let icon_path: Vec<u16> = icon_path_buf[..len].iter().copied().chain(std::iter::once(0)).collect();
        let mut large_icon = HICON::default();
        let extracted = windows::Win32::UI::Shell::ExtractIconExW(
            PCWSTR(icon_path.as_ptr()),
            icon_index,
            Some(&mut large_icon),
            None,
            1,
        );
        (extracted > 0 && !large_icon.is_invalid()).then_some(large_icon)
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
        // Prefer the shortcut's own explicit icon location if it set one
        // (handles special shortcuts like File Explorer's, whose target
        // path doesn't resolve to anything icon-bearing but which sets
        // an explicit icon resource) — falls through to the target
        // executable's own icon (no shortcut-arrow overlay, unlike
        // asking the shell for the `.lnk` file's icon directly), and
        // only as a last resort the `.lnk` itself (still an icon, just
        // possibly with the overlay).
        let icon = shortcut_icon(&lnk)
            .or_else(|| target_path.as_deref().and_then(file_icon))
            .or_else(|| file_icon(&lnk));
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
        let icon = super::overview::window_icon_large(HWND(window.hwnd as *mut c_void));
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
        tracing::info!(index, "show_context_menu: entry has no launch_path (running-unpinned), no menu");
        return;
    };
    tracing::info!(path = ?app_launch_path, "show_context_menu: building menu");

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
        tracing::info!(cmd = cmd.0, "show_context_menu: TrackPopupMenu returned");

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

/// An in-progress drag of a **pinned** dock icon — either reordering it
/// within the dock, or dragging it out onto a workspace card to open a
/// new instance there. Which of the two happens is decided purely by
/// where the pointer releases (see `on_dock_drag_end`), not by anything
/// decided at drag-start.
pub(crate) struct DockDrag {
    pub(crate) start_x: i32,
    pub(crate) start_y: i32,
    pub(crate) cur_x: i32,
    pub(crate) cur_y: i32,
    pub(crate) max_delta: i32,
    /// This pin's index among `pinned_paths()` at drag-start (stable for
    /// the duration of one drag — the pinned list isn't rebuilt mid-drag).
    pub(crate) from_index: usize,
    /// Ghost icon to draw at the cursor while dragging.
    pub(crate) icon: Option<HICON>,
}

/// Ends a drag that moved past the click threshold (the caller already
/// handled the below-threshold "was actually just a click" case, same
/// as `WindowDrag`/`CarouselDrag`'s own end handlers do). Reorders if
/// dropped within the dock bar; launches assigned to a workspace if
/// dropped on a card; cancels (no-op) otherwise.
pub(crate) fn on_dock_drag_end(monitor: &str, drag: DockDrag, x: i32, y: i32) {
    let pins = pinned_paths();
    // `dock_layout` must be called with the *total* displayed entry
    // count (pinned + running-unpinned), exactly like every other
    // hit-test in this file (`on_overview_click`, `on_overview_hover`)
    // — using only `pins.len()` would compute a narrower bar than what's
    // actually on screen (running-unpinned entries widen it), shifting
    // every slot's x position and silently breaking this hit-test.
    let total_count = super::state::STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.overviews.get(monitor))
            .map(|ov| ov.dock_apps.len())
    });
    let Some(total_count) = total_count else { return };
    let (bar_rect, slots, _divider) = dock_layout(monitor, pins.len(), total_count.max(1));
    if x >= bar_rect.left && x < bar_rect.right && y >= bar_rect.top && y < bar_rect.bottom {
        // Reorder: target index is whichever *pinned* slot's center is
        // closest to the drop point — only the first `pins.len()` slots
        // are pinned entries (`build_dock_apps` always places pinned
        // entries first), so running-unpinned slots (if any) are never
        // eligible reorder targets.
        let target = slots
            .iter()
            .take(pins.len())
            .enumerate()
            .min_by_key(|(_, r)| ((r.left + r.right) / 2 - x).abs())
            .map(|(i, _)| i)
            .unwrap_or(drag.from_index);
        let reordered = super::dock_pins::reorder(&pins, drag.from_index, target);
        PINNED_PATHS.with(|p| *p.borrow_mut() = reordered.clone());
        persist_pinned_paths(&reordered);
        super::overview::rebuild_open_overview_pages(monitor);
        return;
    }

    let (card_rect, pitch) = super::overview::card_layout(monitor);
    let carousel_offset = super::state::STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| st.overviews.get(monitor)).map(|ov| ov.carousel_offset)
    });
    let Some(carousel_offset) = carousel_offset else { return };
    if y < card_rect.top || y >= card_rect.bottom {
        return; // Released outside any card — cancel.
    }
    let card_center_x = (card_rect.left + card_rect.right) / 2;
    let approx_offset = carousel_offset + (x - card_center_x) as f64 / pitch as f64;
    let max_page = super::state::STATE
        .with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.workspaces.get(monitor))
                .map(|t| t.workspace_ids().len())
        })
        .unwrap_or(0)
        .saturating_sub(1);
    let target_page = approx_offset.round().clamp(0.0, max_page as f64) as usize;
    if (approx_offset - target_page as f64).abs() > 0.5 {
        return; // Not confidently over any specific card — cancel.
    }

    let Some(launch_path) = pins.get(drag.from_index).cloned() else { return };
    let wide: Vec<u16> = launch_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = windows::Win32::UI::Shell::SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<windows::Win32::UI::Shell::SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("open"),
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is a local, fully-initialized struct valid for the
    // duration of this call; `wide` outlives it. `info.hProcess`, on
    // success with `SEE_MASK_NOCLOSEPROCESS`, is a process handle this
    // call site owns and must close.
    unsafe {
        if ShellExecuteExW(&mut info).is_ok() && !info.hProcess.is_invalid() {
            let pid = GetProcessId(info.hProcess);
            super::pending_launch::register(super::pending_launch::PendingLaunch {
                process_id: pid,
                monitor: monitor.to_string(),
                workspace_index: target_page,
                expires_at: std::time::Instant::now() + super::pending_launch::PENDING_LAUNCH_TIMEOUT,
            });
            let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        }
    }
    super::overview::close_overview(monitor, None);
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

    #[test]
    fn running_dot_rects_returns_one_dot_centered_under_the_icon() {
        let slot = RECT { left: 0, top: 0, right: 20, bottom: 20 };
        let dots = running_dot_rects(slot, 1, 3, 4);
        assert_eq!(dots.len(), 1);
        assert_eq!((dots[0].left + dots[0].right) / 2, 10);
    }

    #[test]
    fn running_dot_rects_caps_at_the_max_even_with_more_windows() {
        let slot = RECT { left: 0, top: 0, right: 20, bottom: 20 };
        let dots = running_dot_rects(slot, 10, 3, 4);
        assert_eq!(dots.len(), DOCK_MAX_RUNNING_DOTS);
    }

    #[test]
    fn running_dot_rects_with_zero_windows_returns_empty() {
        let slot = RECT { left: 0, top: 0, right: 20, bottom: 20 };
        assert!(running_dot_rects(slot, 0, 3, 4).is_empty());
    }

    #[test]
    fn running_dot_rects_are_evenly_spaced_and_symmetric_around_the_icon_center() {
        let slot = RECT { left: 0, top: 0, right: 40, bottom: 20 };
        let dots = running_dot_rects(slot, 3, 3, 4);
        assert_eq!(dots.len(), 3);
        let icon_center = (slot.left + slot.right) / 2;
        let first_center = (dots[0].left + dots[0].right) / 2;
        let last_center = (dots[2].left + dots[2].right) / 2;
        assert_eq!(icon_center - first_center, last_center - icon_center);
    }

    #[test]
    fn pinned_count_counts_only_entries_with_a_launch_path() {
        let apps = vec![
            DockApp { icon: None, launch_path: Some(PathBuf::from("a")), windows: vec![] },
            DockApp { icon: None, launch_path: Some(PathBuf::from("b")), windows: vec![] },
            DockApp { icon: None, launch_path: None, windows: vec![1] },
        ];
        assert_eq!(pinned_count(&apps), 2);
    }

    #[test]
    fn anchor_x_center_centers_content_in_the_work_area() {
        // work area 0..200, content 40 wide -> left edge at 80
        assert_eq!(anchor_x(0, 200, 40, "center"), 80);
    }

    #[test]
    fn anchor_x_left_hugs_the_left_edge() {
        assert_eq!(anchor_x(0, 200, 40, "left"), 0);
    }

    #[test]
    fn anchor_x_right_hugs_the_right_edge() {
        assert_eq!(anchor_x(0, 200, 40, "right"), 160);
    }

    #[test]
    fn anchor_x_falls_back_to_center_for_an_unknown_alignment() {
        assert_eq!(anchor_x(0, 200, 40, "bogus"), 80);
    }
}
