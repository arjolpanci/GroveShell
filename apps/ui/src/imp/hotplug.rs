//! Live monitor hotplug: reconciles `AppState`'s per-monitor bars,
//! workspace trackers, and overview windows against the real, current
//! monitor topology whenever Windows reports a `WM_DISPLAYCHANGE`. See
//! `docs/superpowers/specs/2026-07-28-per-monitor-workspaces-design.md`
//! §A/§F.

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, RGN_OR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, MoveWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use groveshell_common::{Error, Result};
use groveshell_window_model::workspace::WorkspaceTracker;

use super::bar::{register_appbar, unregister_appbar};
use super::monitors::enumerate_monitors;
use super::overview::OverviewInstance;
use super::state::{scaled, BarWindow, BAR_CORNER_RADIUS, BAR_HEIGHT, STATE};
use super::workspaces::unpark_window;

/// Re-runs monitor enumeration and diffs it against `AppState.bars` by
/// device name: any newly-connected monitor gets a bar, a workspace
/// tracker, and an overview window; any monitor that's now missing has
/// its windows reassigned to the primary monitor's current workspace
/// and its bar/tracker/overview torn down.
pub(crate) fn reconcile_monitors(hinstance: HINSTANCE) -> Result<()> {
    let monitors = enumerate_monitors();
    let current_names: Vec<String> = monitors.iter().map(|m| m.device_name.clone()).collect();

    let existing_names: Vec<String> = STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.bars.iter().map(|b| b.monitor.clone()).collect()).unwrap_or_default()
    });

    // Disconnected: anything tracked that's no longer in the live list.
    for removed in existing_names.iter().filter(|n| !current_names.contains(n)) {
        remove_monitor(removed);
    }

    // Connected: anything live that isn't tracked yet.
    for monitor in monitors.iter().filter(|m| !existing_names.contains(&m.device_name)) {
        add_monitor(hinstance, monitor)?;
    }

    // A surviving monitor's primary status or geometry may have
    // changed (e.g. the old primary was unplugged and Windows promoted
    // another one) — refresh `primary_monitor`/`primary_bar_hwnd` to
    // match reality.
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
                state.primary_monitor = primary.device_name.clone();
                if let Some(bar) = state.bars.iter().find(|b| b.monitor == primary.device_name) {
                    state.primary_bar_hwnd = bar.hwnd;
                    state.primary_bar_rect = bar.rect;
                }
            }
        }
    });

    Ok(())
}

fn add_monitor(hinstance: HINSTANCE, monitor: &super::monitors::MonitorInfo) -> Result<()> {
    // SAFETY: mirrors the startup bar-creation loop in `mod.rs::main`
    // exactly (same window class, same AppBar registration, same
    // rounded-corner region), just for one monitor after the fact.
    unsafe {
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

        let bar_rect = register_appbar(bar_hwnd, monitor.rect.left, monitor.rect.top, width, bar_height);
        let _ = MoveWindow(bar_hwnd, bar_rect.left, bar_rect.top, bar_rect.right - bar_rect.left, bar_rect.bottom - bar_rect.top, true);

        let radius = scaled(BAR_CORNER_RADIUS, monitor.dpi);
        let region_w = bar_rect.right - bar_rect.left;
        let region_h = bar_rect.bottom - bar_rect.top;
        let region = CreateRoundRectRgn(0, 0, region_w + 1, region_h + 1, radius * 2, radius * 2);
        let top_square = CreateRectRgn(0, 0, region_w + 1, (region_h - radius).max(0));
        CombineRgn(region, region, top_square, RGN_OR);
        let _ = DeleteObject(top_square);
        SetWindowRgn(bar_hwnd, region, true);

        let overview_width = monitor.rect.right - monitor.rect.left;
        let overview_height = monitor.rect.bottom - monitor.rect.top;
        let overview_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("GroveShellOverview"),
            w!("GroveShell Activities"),
            WS_POPUP,
            monitor.rect.left,
            monitor.rect.top,
            overview_width,
            overview_height,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(Error::Windows)?;

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.bars.push(BarWindow {
                    hwnd: bar_hwnd,
                    rect: bar_rect,
                    is_primary: monitor.is_primary,
                    monitor: monitor.device_name.clone(),
                });
                state.workspaces.insert_monitor(monitor.device_name.clone(), WorkspaceTracker::with_monitor_workspaces(1, 0));
                state.overviews.insert(monitor.device_name.clone(), OverviewInstance::new(overview_hwnd));
            }
        });
        let _ = InvalidateRect(bar_hwnd, None, true);
    }
    Ok(())
}

fn remove_monitor(device_name: &str) {
    let (bar_hwnd, overview_hwnd, orphaned_windows, primary) = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return (None, None, Vec::new(), String::new());
        };
        let bar_hwnd = state.bars.iter().position(|b| b.monitor == device_name).map(|i| state.bars.remove(i).hwnd);
        let overview_hwnd = state.overviews.remove(device_name).map(|ov| ov.hwnd);
        let orphaned = state.workspaces.remove_monitor(device_name)
            .map(|t| t.workspace_ids().to_vec().into_iter().flat_map(|id| t.windows_on(id)).collect())
            .unwrap_or_default();
        (bar_hwnd, overview_hwnd, orphaned, state.primary_monitor.clone())
    });

    // Reassign every orphaned window onto the primary monitor's
    // current workspace, un-parking any that were parked on a
    // background workspace of the removed monitor (they must become
    // visible now — there's no monitor left to hide them off of).
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if let Some(tracker) = state.workspaces.get_mut(&primary) {
                let target = tracker.current_index();
                for hwnd in &orphaned_windows {
                    tracker.assign_to_index(*hwnd, target);
                }
            }
        }
    });
    for hwnd in orphaned_windows {
        unpark_window(HWND(hwnd as *mut std::ffi::c_void));
    }

    // SAFETY: both handles, if present, were valid windows created by
    // this process; destroying an already-torn-down window would be a
    // caller bug, not the case here since each is removed from
    // `AppState` exactly once, right before this call.
    unsafe {
        if let Some(hwnd) = overview_hwnd {
            let _ = DestroyWindow(hwnd);
        }
        if let Some(hwnd) = bar_hwnd {
            unregister_appbar(hwnd);
            let _ = DestroyWindow(hwnd);
        }
    }
}
