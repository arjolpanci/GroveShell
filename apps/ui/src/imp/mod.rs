//! Process bootstrap (`main`) and the shared `wndproc` that dispatches
//! every window message to the right module. See the crate-level doc
//! comment in `main.rs` for the overall shell design.

mod bar;
mod calendar;
mod dock;
mod gpu;
mod hotplug;
mod icons;
mod monitors;
mod monitor_workspaces;
mod movesize;
mod overview;
mod quick_settings;
mod radios;
mod state;
mod taskbar;
mod theme;
mod util;
mod wifi;
mod workspaces;

use std::ffi::c_void;
use std::time::Duration;

use groveshell_common::{Error, Result};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, InvalidateRect,
    SetWindowRgn, RGN_OR,
};
use windows::Win32::Graphics::GdiPlus::{GdiplusStartup, GdiplusStartupInput};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::MK_LBUTTON;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_ESCAPE, VK_LEFT, VK_RIGHT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use groveshell_window_model::registry::WindowRegistry;
use groveshell_window_model::workspace::WorkspaceTracker;

use bar::{on_bar_click, on_bar_hover, on_bar_mouse_leave, paint_bar, register_appbar, unregister_appbar, QS_LABEL_MARGIN};
use calendar::{hide_calendar, paint_calendar, CAL_HEIGHT, CAL_WIDTH};
use monitors::{enumerate_monitors, monitor_index_for_center};
use movesize::{
    check_hot_corners, install_move_size_hooks, on_drag_timer, uninstall_move_size_hooks,
    DRAG_TIMER_ID, HOTCORNER_INTERVAL_MS, HOTCORNER_TIMER_ID,
};
use overview::{
    close_overview, on_animation_tick, on_overview_arrow, on_overview_char,
    on_overview_drag_end, on_overview_drag_move, on_overview_drag_start, on_overview_hover,
    paint_overview, repaint_overview,
};
use quick_settings::{
    hide_quick_settings, on_quick_settings_mouse_down, on_quick_settings_mouse_move,
    on_quick_settings_mouse_up, paint_quick_settings, QS_COLOR_KEY, QS_HEIGHT, QS_SHADOW_MARGIN,
    QS_WIDTH,
};
use state::{
    role_of, scaled, AppState, BarWindow, Role, ANIM_TIMER_ID, BAR_CORNER_RADIUS, BAR_HEIGHT,
    CLOCK_TIMER_ID, STATE,
};
use taskbar::{
    claim_work_areas, restore_work_areas, set_taskbar_appbar_state, set_taskbar_windows_visible,
    set_windows_taskbar_visible, ORIGINAL_WORK_AREAS,
};
use workspaces::{
    install_win_event_hooks, move_focused_window_relative, on_window_sync_timer,
    switch_workspace_relative, uninstall_win_event_hooks, unpark_window, HOTKEY_MOVE_WIN_NEXT,
    HOTKEY_MOVE_WIN_PREV, HOTKEY_WS_NEXT, HOTKEY_WS_PREV, SYNC_TIMER_ID,
};

/// Not exposed by this version of the `windows` crate's
/// `WindowsAndMessaging` module under this name; the value itself
/// (`0x02A3`) is a stable, decades-old Win32 constant.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub fn main() -> Result<()> {
    // SAFETY: must run before any window is created or any DPI-
    // sensitive API is called (GetWindowRect, GetSystemMetrics,
    // EnumDisplayMonitors, DWM window attributes, ...) — this is the
    // very first thing `main` does. Without it, Windows silently
    // "DPI-virtualizes" coordinates for this process, and different
    // APIs virtualize inconsistently: `DwmGetWindowAttribute`'s
    // extended frame bounds come back in true physical pixels
    // regardless, while a plain `GetWindowRect` from a DPI-unaware
    // process gets scaled to look like 96 DPI. On a single monitor
    // both just happen to agree; on a mixed-DPI multi-monitor setup
    // they don't, and window-model's rect for anything on the
    // non-100%-scaled monitor comes out the wrong size/position
    // relative to everything else this process computes — this was
    // reproduced directly (a window measuring ~1129x635 via
    // `GetWindowRect` reported as ~2236x1259 here, matching a 200%
    // scale factor exactly) before adding this call.
    let _ = unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    };

    let _log_guard = groveshell_common::logging::init("ui")?;
    tracing::info!("groveshell-ui starting");

    let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
    tracing::info!("joined shell job object");

    // SAFETY: called once, early, on the same single thread that makes
    // every other COM call in this process (volume control).
    // Initialization failure just means volume control degrades to
    // "unavailable" via the `Option`-returning helpers below — it's not
    // fatal to the rest of the shell.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    // SAFETY: must run once, before any other `Gdip*` call — used to
    // draw the real wallpaper into overview cards (`draw_wallpaper_into`).
    // Never shut down: this process only ever exits via `PostQuitMessage`
    // and normal process teardown, same as the window classes registered
    // below are never unregistered.
    let mut gdiplus_token: usize = 0;
    let gdiplus_input = GdiplusStartupInput {
        GdiplusVersion: 1,
        ..Default::default()
    };
    let _ = unsafe { GdiplusStartup(&mut gdiplus_token, &gdiplus_input, std::ptr::null_mut()) };

    // Decided once, here, before any window exists — see gpu.rs. Every
    // later GPU-path call site just checks `gpu::is_enabled()`; this is
    // never retried or re-evaluated.
    gpu::init();

    // SAFETY: every Win32 call below either has a call-site safety
    // comment or is a plain value/query with no aliasing or lifetime
    // requirements (e.g. `GetSystemMetrics`).
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(Error::Windows)?;
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        register_class(hinstance, w!("GroveShellBar"), Some(wndproc), 0x00202020)?;
        register_class(
            hinstance,
            w!("GroveShellOverview"),
            Some(wndproc),
            0x00404040,
        )?;
        register_class(
            hinstance,
            w!("GroveShellCalendar"),
            Some(wndproc),
            0x00303030,
        )?;
        register_class(
            hinstance,
            w!("GroveShellQuickSettings"),
            Some(wndproc),
            0x00303030,
        )?;

        // One bar per monitor (Phase 4). WS_EX_TOOLWINDOW keeps each
        // out of the taskbar/alt-tab and (as a side effect) out of its
        // own Activities listing, since `window-model::snapshot`
        // excludes tool windows. WS_EX_NOACTIVATE means clicking
        // anything on a bar never makes it the foreground window —
        // without it, `GetForegroundWindow()` when opening a flyout
        // would see the bar itself instead of whatever app the user
        // was actually using, breaking "restore focus on cancel."
        // Self-heal from a previous run that died without restoring
        // the taskbar (hard kill, crash): if it's hidden right now,
        // bring it back — and give Explorer a moment to re-reserve its
        // strip — *before* capturing "original" work areas below, or
        // this run would capture (and later dutifully restore) the
        // broken state.
        if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if !IsWindowVisible(tray).as_bool() {
                set_taskbar_appbar_state(0);
                set_taskbar_windows_visible(true);
                std::thread::sleep(Duration::from_millis(300));
            }
        }

        let monitors = enumerate_monitors();
        let mut bars = Vec::new();
        for monitor in &monitors {
            let width = monitor.rect.right - monitor.rect.left;
            let bar_height = scaled(BAR_HEIGHT, monitor.dpi);
            let bar_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("GroveShellBar"),
                w!("GroveShell"),
                WS_POPUP | WS_VISIBLE,
                monitor.rect.left,
                monitor.rect.top,
                width,
                bar_height,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            // Register this bar as a top-edge AppBar (the same
            // mechanism the Windows taskbar uses) so it reserves its
            // strip of *its own monitor's* work area — the AppBar API
            // has been monitor-aware since Windows 8, determined by
            // which monitor the given rect falls on.
            let bar_rect =
                register_appbar(bar_hwnd, monitor.rect.left, monitor.rect.top, width, bar_height);
            let _ = MoveWindow(
                bar_hwnd,
                bar_rect.left,
                bar_rect.top,
                bar_rect.right - bar_rect.left,
                bar_rect.bottom - bar_rect.top,
                true,
            );

            // Round only the bar's *bottom* two corners: the window
            // region is a fully rounded rect unioned with a square
            // strip covering the top edge. `SetWindowRgn` takes
            // ownership of the region on success.
            let radius = scaled(BAR_CORNER_RADIUS, monitor.dpi);
            let region_w = bar_rect.right - bar_rect.left;
            let region_h = bar_rect.bottom - bar_rect.top;
            let region = CreateRoundRectRgn(0, 0, region_w + 1, region_h + 1, radius * 2, radius * 2);
            let top_square = CreateRectRgn(0, 0, region_w + 1, (region_h - radius).max(0));
            CombineRgn(region, region, top_square, RGN_OR);
            let _ = DeleteObject(top_square);
            SetWindowRgn(bar_hwnd, region, true);

            bars.push(BarWindow {
                hwnd: bar_hwnd,
                rect: bar_rect,
                is_primary: monitor.is_primary,
                monitor: monitor.device_name.clone(),
            });
        }

        // This shell replaces the Windows taskbar while it runs: hide
        // it (and give apps its reserved strip back), remembering the
        // pre-existing work areas to restore at clean shutdown.
        ORIGINAL_WORK_AREAS.with(|w| *w.borrow_mut() = monitors.iter().map(|m| m.work).collect());
        set_windows_taskbar_visible(false);
        claim_work_areas(&monitors);

        let primary = bars.iter().find(|b| b.is_primary).unwrap_or(&bars[0]);
        let primary_bar_hwnd = primary.hwnd;
        let primary_bar_rect = primary.rect;

        // One Activities overview per monitor now, sized to that monitor's
        // own rect (not the virtual screen) — see the design doc §D. Created
        // eagerly at startup (not lazily) since there's no live-resize path
        // for an already-open overview window yet; lazy creation is only
        // needed for hotplug (Task 10), which creates one on demand there.
        let mut overviews: std::collections::HashMap<String, overview::OverviewInstance> =
            std::collections::HashMap::new();
        for monitor in &monitors {
            let width = monitor.rect.right - monitor.rect.left;
            let height = monitor.rect.bottom - monitor.rect.top;
            let overview_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("GroveShellOverview"),
                w!("GroveShell Activities"),
                WS_POPUP,
                monitor.rect.left,
                monitor.rect.top,
                width,
                height,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;
            overviews.insert(monitor.device_name.clone(), overview::OverviewInstance::new(overview_hwnd));
        }

        // Calendar + notifications flyout, centered under the primary
        // bar's clock label, clamped so it never runs off that
        // monitor's edges.
        let primary_bar_width = primary_bar_rect.right - primary_bar_rect.left;
        let calendar_x = (primary_bar_rect.left + primary_bar_width / 2 - CAL_WIDTH / 2)
            .clamp(primary_bar_rect.left, (primary_bar_rect.right - CAL_WIDTH).max(primary_bar_rect.left));
        let calendar_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("GroveShellCalendar"),
            w!("GroveShell Calendar"),
            WS_POPUP,
            calendar_x,
            primary_bar_rect.bottom,
            CAL_WIDTH,
            CAL_HEIGHT,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(Error::Windows)?;

        // Quick Settings flyout, right-aligned under the primary bar's
        // right label. Unlike the bar itself, `QS_WIDTH`/`QS_HEIGHT` are
        // 96-DPI values that `quick_settings.rs`'s own paint/hit-test
        // code (`qs_layout`) already scales internally — the window
        // itself has to be created at that same scaled size, or the
        // content it lays out overflows a window physically too small
        // to hold it on any monitor above 100% scaling. The window is
        // also bigger than the visible card by `QS_SHADOW_MARGIN` on
        // every side (room for the drop shadow) and layered with a
        // color-key so that margin — and the card's rounded corners —
        // are actually transparent; see the module docs on `paint_quick_settings`.
        let primary_dpi = GetDpiForWindow(primary_bar_hwnd).max(96);
        let qs_margin = scaled(QS_SHADOW_MARGIN, primary_dpi);
        let qs_width = scaled(QS_WIDTH, primary_dpi) + qs_margin * 2;
        let qs_height = scaled(QS_HEIGHT, primary_dpi) + qs_margin * 2;
        let qs_card_target_right = primary_bar_rect.right - scaled(QS_LABEL_MARGIN, primary_dpi);
        let qs_x = (qs_card_target_right + qs_margin - qs_width)
            .clamp(primary_bar_rect.left - qs_margin, (primary_bar_rect.right - qs_width).max(primary_bar_rect.left - qs_margin));
        let quick_settings_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("GroveShellQuickSettings"),
            w!("GroveShell Quick Settings"),
            WS_POPUP,
            qs_x,
            primary_bar_rect.bottom,
            qs_width,
            qs_height,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(Error::Windows)?;
        let _ = SetLayeredWindowAttributes(
            quick_settings_hwnd,
            COLORREF(QS_COLOR_KEY),
            0,
            LWA_COLORKEY,
        );

        let bar_hwnds: Vec<HWND> = bars.iter().map(|b| b.hwnd).collect();

        // One pinned workspace per currently-connected monitor, ordered
        // left-to-right by real screen position, with the primary
        // monitor's slot starting as current — see the module docs.
        // Seed each with whatever windows are already on that monitor
        // right now so a fresh launch doesn't show every open window
        // crammed onto workspace 0.
        let mut workspaces = monitor_workspaces::MonitorWorkspaces::new();
        for monitor in &monitors {
            workspaces.insert_monitor(
                monitor.device_name.clone(),
                WorkspaceTracker::with_monitor_workspaces(1, 0),
            );
        }
        for window in groveshell_window_model::snapshot() {
            let center_x = (window.rect.left + window.rect.right) / 2;
            let center_y = (window.rect.top + window.rect.bottom) / 2;
            let target_monitor = monitor_index_for_center(&monitors, center_x, center_y)
                .and_then(|i| monitors.get(i))
                .or_else(|| monitors.iter().find(|m| m.is_primary))
                .map(|m| m.device_name.clone());
            if let Some(device_name) = target_monitor {
                if let Some(tracker) = workspaces.get_mut(&device_name) {
                    tracker.assign_to_index(window.hwnd, 0);
                }
            }
        }

        let primary_monitor = monitors.iter().find(|m| m.is_primary)
            .map(|m| m.device_name.clone())
            .unwrap_or_else(|| monitors[0].device_name.clone());

        STATE.with(|s| {
            *s.borrow_mut() = Some(AppState {
                bars,
                primary_bar_hwnd,
                primary_bar_rect,
                primary_monitor,
                calendar_hwnd,
                quick_settings_hwnd,
                calendar_open: false,
                quick_settings_open: false,
                previous_foreground: HWND(std::ptr::null_mut()),
                workspaces,
                overviews,
                window_registry: WindowRegistry::new(),
                window_rects: std::collections::HashMap::new(),
                qs_pill_hover: false,
                qs_volume_dragging: false,
            });
        });

        // A `MoveWindow` above may have already triggered and consumed
        // a `WM_PAINT` for a bar before `STATE` existed, in which case
        // `wndproc` fell back to `DefWindowProcW` and the primary bar's
        // labels never actually got drawn. Force one more repaint on
        // every bar now that `STATE` is ready so they always show up,
        // regardless of how that first paint landed.
        for bar_hwnd in bar_hwnds {
            let _ = InvalidateRect(bar_hwnd, None, true);
        }

        // Global workspace-switching hotkeys plus the clock and hot-
        // corner timers, all owned by whichever window is currently the
        // primary bar (delivered as `WM_HOTKEY`/`WM_TIMER` to it, since
        // it already has a message loop and outlives every flyout).
        // Re-run whenever hotplug promotes a new primary bar, since a
        // destroyed window silently drops everything it owned.
        install_primary_bar_extras(primary_bar_hwnd);

        install_win_event_hooks();
        install_move_size_hooks();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

/// SAFETY: `wndproc` must be a valid `WNDPROC`-compatible function
/// pointer for the lifetime of the registered class, which holds for
/// the whole process lifetime here (classes are never unregistered).
unsafe fn register_class(
    hinstance: windows::Win32::Foundation::HINSTANCE,
    class_name: windows::core::PCWSTR,
    wndproc: WNDPROC,
    background: u32,
) -> Result<()> {
    let class = WNDCLASSW {
        lpfnWndProc: wndproc,
        hInstance: hinstance,
        lpszClassName: class_name,
        hCursor: LoadCursorW(None, IDC_ARROW).map_err(Error::Windows)?,
        hbrBackground: CreateSolidBrush(COLORREF(background)),
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        return Err(Error::Windows(windows::core::Error::from_win32()));
    }
    Ok(())
}

/// Registers the primary-bar-owned hotkeys (workspace-switch,
/// move-window) and timers (clock, hot corners) on `hwnd`. Called once
/// at startup on the initial primary bar, and again from
/// `hotplug::reconcile_monitors` whenever a monitor hotplug promotes a
/// different bar to primary — the previous holder's hotkeys/timers, if
/// any, are silently released when Windows destroys that window, so
/// nothing needs to be explicitly unregistered first.
pub(crate) fn install_primary_bar_extras(hwnd: HWND) {
    // SAFETY: all Win32 calls here are best-effort registrations against
    // a single valid window handle the caller owns; failure (e.g.
    // another app already holds one of these hotkey combinations)
    // degrades that one feature rather than being fatal.
    unsafe {
        SetTimer(hwnd, CLOCK_TIMER_ID, 1000, None);
        let _ = RegisterHotKey(hwnd, HOTKEY_WS_PREV, MOD_CONTROL | MOD_ALT, VK_LEFT.0 as u32);
        let _ = RegisterHotKey(hwnd, HOTKEY_WS_NEXT, MOD_CONTROL | MOD_ALT, VK_RIGHT.0 as u32);
        let _ = RegisterHotKey(
            hwnd,
            HOTKEY_MOVE_WIN_PREV,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT,
            VK_LEFT.0 as u32,
        );
        let _ = RegisterHotKey(
            hwnd,
            HOTKEY_MOVE_WIN_NEXT,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT,
            VK_RIGHT.0 as u32,
        );
        SetTimer(hwnd, HOTCORNER_TIMER_ID, HOTCORNER_INTERVAL_MS, None);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let role = role_of(hwnd);

    match msg {
        WM_PAINT => match role {
            Role::Bar { is_primary, monitor } => {
                paint_bar(hwnd, is_primary, &monitor);
                LRESULT(0)
            }
            Role::Overview { monitor } => {
                paint_overview(hwnd, &monitor);
                LRESULT(0)
            }
            Role::Calendar => {
                paint_calendar(hwnd);
                LRESULT(0)
            }
            Role::QuickSettings => {
                paint_quick_settings(hwnd);
                LRESULT(0)
            }
            Role::Other => DefWindowProcW(hwnd, msg, wparam, lparam),
        },
        // The overview paints its own backdrop into a back buffer (see
        // `paint_overview`), so the class-brush erase pass is both
        // redundant and the source of a visible background flash
        // between erase and repaint while dragging — claim it handled.
        WM_ERASEBKGND if matches!(role, Role::Overview { .. }) || role == Role::QuickSettings => {
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            match role {
                Role::Overview { monitor } => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                    on_overview_drag_start(&monitor, x, y);
                    LRESULT(0)
                }
                Role::QuickSettings => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                    on_quick_settings_mouse_down(hwnd, x, y);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_MOUSEMOVE => {
            match role {
                Role::Overview { monitor } => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                    if wparam.0 & (MK_LBUTTON.0 as usize) != 0 {
                        on_overview_drag_move(&monitor, x, y);
                    } else {
                        on_overview_hover(&monitor, x, y);
                    }
                }
                Role::QuickSettings => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    on_quick_settings_mouse_move(hwnd, x);
                }
                Role::Bar { is_primary: true, .. } => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                    on_bar_hover(hwnd, x, y, true);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            if let Role::Bar { is_primary: true, .. } = role {
                on_bar_mouse_leave(hwnd);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            match role {
                Role::Bar { is_primary, monitor } => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    on_bar_click(hwnd, x, is_primary, &monitor);
                }
                Role::Overview { monitor } => {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                    on_overview_drag_end(&monitor, x, y);
                }
                Role::QuickSettings => {
                    on_quick_settings_mouse_up();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CHAR if matches!(role, Role::Overview { .. }) => {
            if let Role::Overview { monitor } = role {
                on_overview_char(&monitor, wparam.0 as u32);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                match role {
                    Role::Overview { monitor } => {
                        // Escape backs out one layer: an active search
                        // first, the overview itself second.
                        let searching = STATE.with(|s| {
                            s.borrow_mut()
                                .as_mut()
                                .and_then(|st| st.overviews.get_mut(&monitor))
                                .map(|ov| {
                                    let searching = !ov.search_query.is_empty();
                                    ov.search_query.clear();
                                    searching
                                })
                                .unwrap_or(false)
                        });
                        if searching {
                            repaint_overview(hwnd);
                        } else {
                            close_overview(&monitor, None);
                        }
                    }
                    Role::Calendar => hide_calendar(true),
                    Role::QuickSettings => hide_quick_settings(true),
                    _ => {}
                }
            } else if let Role::Overview { monitor } = &role {
                let searching = STATE
                    .with(|s| {
                        s.borrow()
                            .as_ref()
                            .and_then(|st| st.overviews.get(monitor.as_str()))
                            .map(|ov| !ov.search_query.is_empty())
                    })
                    .unwrap_or(false);
                if !searching {
                    if wparam.0 == VK_LEFT.0 as usize {
                        on_overview_arrow(monitor, -1);
                    } else if wparam.0 == VK_RIGHT.0 as usize {
                        on_overview_arrow(monitor, 1);
                    }
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // LOWORD(wParam) == WA_INACTIVE means this window just lost
            // activation — e.g. the user clicked somewhere else — which
            // is this shell's cue to auto-dismiss a flyout, the same as
            // clicking away from the real taskbar's date/time or quick
            // settings panel does.
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                match role {
                    Role::Calendar => hide_calendar(false),
                    Role::QuickSettings => hide_quick_settings(false),
                    _ => {}
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_HOTKEY => {
            match wparam.0 as i32 {
                HOTKEY_WS_PREV => switch_workspace_relative(-1),
                HOTKEY_WS_NEXT => switch_workspace_relative(1),
                HOTKEY_MOVE_WIN_PREV => move_focused_window_relative(-1),
                HOTKEY_MOVE_WIN_NEXT => move_focused_window_relative(1),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // SAFETY: `GetModuleHandleW(None)` is a plain, idempotent query;
            // `hinstance` is only used to create new windows for newly
            // connected monitors, exactly as at startup.
            if let Ok(module) = unsafe { GetModuleHandleW(None) } {
                let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);
                let _ = hotplug::reconcile_monitors(hinstance);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                ANIM_TIMER_ID => {
                    if let Role::Overview { monitor } = role {
                        on_animation_tick(&monitor);
                    }
                }
                SYNC_TIMER_ID => on_window_sync_timer(hwnd),
                HOTCORNER_TIMER_ID => check_hot_corners(),
                DRAG_TIMER_ID => on_drag_timer(),
                CLOCK_TIMER_ID => {
                    let primary =
                        STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
                    if let Some(primary) = primary {
                        let _ = InvalidateRect(primary, None, true);
                    }
                    // Explorer re-shows an auto-hidden taskbar on edge
                    // hover or the Win key; while this shell runs it
                    // should stay gone.
                    if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
                        if IsWindowVisible(tray).as_bool() {
                            set_taskbar_windows_visible(false);
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Role::Bar { is_primary, .. } = role {
                unregister_appbar(hwnd);
                if is_primary {
                    uninstall_win_event_hooks();
                    uninstall_move_size_hooks();
                    let _ = UnregisterHotKey(hwnd, HOTKEY_WS_PREV);
                    let _ = UnregisterHotKey(hwnd, HOTKEY_WS_NEXT);
                    let _ = UnregisterHotKey(hwnd, HOTKEY_MOVE_WIN_PREV);
                    let _ = UnregisterHotKey(hwnd, HOTKEY_MOVE_WIN_NEXT);
                    // Without a workspace manager running, "parked
                    // off-screen" just means "stranded off-screen" —
                    // bring every tracked window back before going
                    // away. (A hard kill still strands them; the
                    // `top` guard in `park_window`/`unpark_window`
                    // lets the next run recover those.)
                    let tracked: Vec<isize> = STATE.with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| st.workspaces.all_tracked_windows())
                            .unwrap_or_default()
                    });
                    for tracked_hwnd in tracked {
                        unpark_window(HWND(tracked_hwnd as *mut c_void));
                    }
                    set_windows_taskbar_visible(true);
                    restore_work_areas();
                    PostQuitMessage(0);
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
