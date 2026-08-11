//! Workspace switching, live window tracking (WinEvent hooks), and the
//! park/unpark mechanics that make an inactive workspace's windows
//! disappear from the real screen without ever being `SW_HIDE`-hidden.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, IsIconic, KillTimer, SetTimer, SetWindowPos,
    ShowWindow, SWP_NOSIZE, SWP_NOZORDER, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA,
};

use super::bar::refresh_bar_indicator;
use super::monitors::monitors_sorted_by_x;
use super::overview::{
    capture_window_snapshot, drop_window_snapshot, rebuild_open_overview_pages, retain_window_snapshots,
    snap_carousel_to, OverviewMode,
};
use super::state::{role_of, Role, STATE};

pub(crate) const HOTKEY_WS_PREV: i32 = 3001;
pub(crate) const HOTKEY_WS_NEXT: i32 = 3002;
pub(crate) const HOTKEY_MOVE_WIN_PREV: i32 = 3003;
pub(crate) const HOTKEY_MOVE_WIN_NEXT: i32 = 3004;

/// One-shot debounce for WinEvent-driven window re-syncs: bursts of
/// create/destroy/show/hide events (an app opening ten windows, a
/// teardown cascade) collapse into a single `sync_workspaces` pass.
pub(crate) const SYNC_TIMER_ID: usize = 3;
const SYNC_DEBOUNCE_MS: u32 = 250;

/// How far below its real position a window is moved when its
/// workspace stops being current. Parked off-screen rather than
/// `SW_HIDE`-hidden deliberately: DWM renders *nothing* for a hidden
/// window's thumbnail — an off-screen window at least stays a real,
/// visible window. Far enough down to clear any plausible monitor
/// arrangement, small enough to stay well inside the ±32767 coordinate
/// space, and vertical so the window's nearest monitor (and therefore
/// its DPI) doesn't change.
///
/// Off-screen alone isn't enough for previews, though: many apps
/// (browsers, UWP) detect they're fully occluded/off-screen and stop
/// producing frames, and DWM can evict their surface entirely — which
/// made parked windows' live thumbnails go blank shortly after
/// parking. That's why `park_window` also captures a static snapshot
/// first (see `overview::WINDOW_SNAPSHOTS`); the overview paints that
/// instead of a live thumbnail for parked windows.
const WORKSPACE_PARK_DY: i32 = 20000;

/// Moves `hwnd` to its parked (off-screen) position. Skips minimized
/// windows — their on-screen rect is a `-32000` placeholder whose
/// restore position Windows manages separately, and they have no live
/// pixels to keep renderable anyway. The `top` guard makes parking
/// idempotent (and keeps a window left parked by a crashed previous
/// run from being pushed further out).
pub(crate) fn park_window(hwnd: HWND) {
    // SAFETY: `hwnd` was tracked as a real window; if it has since
    // closed every call here documented-fails harmlessly.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top >= WORKSPACE_PARK_DY / 2 {
            return;
        }
        capture_window_snapshot(hwnd);
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top + WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // Win32 does not move an owned window (a dialog, color picker, etc.)
    // when its owner moves — carry every currently-live owned window
    // along so it doesn't strand on the wrong workspace. Recursing
    // through `park_window` itself (rather than inlining the move) means
    // a multi-level owner chain (a dialog that owns a further dialog)
    // parks correctly too; each recursive call's own early-return guards
    // make this cheap and safe even though `owned_windows_of` already
    // returns the full transitive set.
    for owned in groveshell_window_model::owned_windows_of(hwnd.0 as isize) {
        park_window(HWND(owned as *mut c_void));
    }
}

/// Inverse of [`park_window`]: returns `hwnd` to its real position (a
/// fixed offset, so no per-window bookkeeping to drift out of date)
/// and retires its park-time snapshot — the live window is preview
/// material again.
pub(crate) fn unpark_window(hwnd: HWND) {
    drop_window_snapshot(hwnd.0 as isize);
    // SAFETY: same as `park_window`.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top < WORKSPACE_PARK_DY / 2 {
            // Not parked — e.g. it was minimized (and hidden) at park
            // time; make sure it's shown either way.
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top - WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // See `park_window` — carry every owned window back too.
    for owned in groveshell_window_model::owned_windows_of(hwnd.0 as isize) {
        unpark_window(HWND(owned as *mut c_void));
    }
}

/// Commits to the workspace at `target_index`: parks the outgoing
/// workspace's windows off-screen and unparks the incoming ones (see
/// `park_window` for why this isn't a `ShowWindow` hide/show swap), so
/// the desktop actually reflects the new current workspace
/// immediately, even if the overview stays open — GNOME switches live
/// as soon as you land on a workspace in the overview too, rather than
/// waiting for it to close. No-op if `target_index` is already
/// current.
pub(crate) fn commit_workspace_switch(monitor: &str, target_index: usize) {
    let switch = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let tracker = state.workspaces.get_mut(monitor)?;
        let from_pinned = tracker.is_pinned(tracker.current_index());
        let to_pinned = tracker.is_pinned(target_index);
        let (from_id, to_id) = tracker.switch_to_index(target_index)?;
        // Two pinned workspaces are two physical monitors, both already
        // on screen — switching between them is purely a focus/indicator
        // change, nothing to hide or show. Any other pair shares screen
        // space (a dynamic workspace displays over the monitor being
        // left), so the outgoing windows must actually hide and the
        // incoming ones actually show — with a single monitor, its one
        // pinned workspace included, or switching does nothing visible.
        let both_pinned = from_pinned && to_pinned;
        let hide = if both_pinned { Vec::new() } else { tracker.windows_on(from_id) };
        let show = if both_pinned { Vec::new() } else { tracker.windows_on(to_id) };
        Some((hide, show))
    });
    let Some((hide, show)) = switch else {
        return;
    };

    for hwnd in &hide {
        park_window(HWND(*hwnd as *mut c_void));
    }
    for hwnd in &show {
        unpark_window(HWND(*hwnd as *mut c_void));
    }

    // Rebuild the overview's pages from scratch if it's open: a
    // workspace switch can change which page is current, how many
    // workspaces exist at all (dynamic growth/shrink), and which
    // windows live where, so patching the previous session's pages
    // piecemeal was a correctness trap — see `rebuild_open_overview_pages`.
    super::overview::rebuild_open_overview_pages(monitor);
    refresh_bar_indicator();
}

/// `Ctrl+Alt+Left/Right` — works whether or not the overview is open;
/// re-syncs the workspace tracker first (see `sync_workspaces`) so any
/// window opened since the last sync lands on the workspace being left
/// rather than silently following onto the new one.
///
/// The "active" monitor this switches is whichever one the cursor is
/// currently over (`monitor_key_at_point`, same convention
/// `movesize::toggle_overview`/hot corners already use) rather than the
/// foreground window's monitor: focus doesn't reliably follow the mouse
/// between monitors (clicking a window on monitor B makes it foreground,
/// but moving the mouse back to monitor A without clicking anything
/// doesn't change what's foreground), so keying off foreground window
/// made this hotkey silently act on whichever monitor was last clicked
/// into instead of the one the user is actually looking at/pointing at.
pub(crate) fn switch_workspace_relative(delta: i32) {
    sync_workspaces();
    let mut pt = windows::Win32::Foundation::POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_at_point(pt) else {
        return;
    };
    let (target, overview_open) = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| {
                let tracker = st.workspaces.get(&monitor)?;
                let overview_open = st
                    .overviews
                    .get(&monitor)
                    .map(|ov| matches!(ov.mode, OverviewMode::Open { .. }))
                    .unwrap_or(false);
                Some((tracker.clamped_relative_index(delta), overview_open))
            })
            .unwrap_or((0, false))
    });
    if overview_open {
        snap_carousel_to(&monitor, target, None);
    } else {
        commit_workspace_switch(&monitor, target);
    }
}

/// `Ctrl+Alt+Shift+Left/Right` — sends the foreground window away into
/// the dynamic (non-monitor) tail and parks it immediately (the user
/// stays on their current monitor; only the window leaves). `delta`'s
/// *sign* doesn't actually matter: a focused window can only ever be on
/// a pinned monitor workspace (it has to be visible to be focused, and
/// only monitor workspaces are ever really visible), and there's no
/// meaningful "direction" for leaving a monitor — it always lands on
/// the first dynamic workspace either way. No-op if nothing eligible is
/// focused (including any of this shell's own windows).
pub(crate) fn move_focused_window_relative(delta: i32) {
    let _ = delta;
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || role_of(fg) != Role::Other {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_of_window(fg) else {
        return;
    };
    sync_workspaces();
    let hwnd = fg.0 as isize;
    let moved = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let tracker = state.workspaces.get_mut(&monitor)?;
        let target = tracker.pinned_count();
        tracker.move_window_to_index(hwnd, target)
    });
    if moved.is_some() {
        park_window(fg);
    }
    refresh_bar_indicator();
}

thread_local! {
    /// Live WinEvent hook handles, kept only so `WM_DESTROY` can
    /// unhook them explicitly (the OS would also drop them at process
    /// exit; this keeps shutdown symmetrical with startup).
    static WIN_EVENT_HOOKS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

/// Live window tracking (Phase 1): out-of-context WinEvent hooks,
/// delivered as messages on this UI thread. Three hooks:
/// create/destroy/show/hide (one contiguous range), name changes, and
/// foreground changes. The first two just mark the window model dirty
/// (see `schedule_window_sync` — bursts coalesce into one re-sync);
/// foreground gets its own immediate handler so that activating a
/// parked window (taskbar-less Alt+Tab still exists) switches to its
/// workspace instead of focusing something invisible.
pub(crate) fn install_win_event_hooks() {
    const EVENT_OBJECT_CREATE: u32 = 0x8000;
    const EVENT_OBJECT_HIDE: u32 = 0x8003;
    const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;
    const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
    const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
    const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

    let ranges = [
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
    ];
    for (min, max) in ranges {
        // SAFETY: `win_event_proc` is a valid WINEVENTPROC for the
        // process lifetime; out-of-context hooks deliver on this
        // thread's message loop, so the callback shares thread-affine
        // state safely.
        let hook = unsafe {
            SetWinEventHook(
                min,
                max,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if !hook.is_invalid() {
            WIN_EVENT_HOOKS.with(|h| h.borrow_mut().push(hook.0 as isize));
        }
    }
}

pub(crate) fn uninstall_win_event_hooks() {
    let hooks = WIN_EVENT_HOOKS.with(|h| std::mem::take(&mut *h.borrow_mut()));
    for hook in hooks {
        // SAFETY: each handle came from `SetWinEventHook` above and is
        // unhooked at most once.
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook as *mut c_void));
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole-window events for real windows; child-object noise
    // (menus, scrollbars, accessibility subtrees) vastly outnumbers
    // them. OBJID_WINDOW == 0, CHILDID_SELF == 0.
    if hwnd.0.is_null() || id_object != 0 || id_child != 0 {
        return;
    }
    if event == 0x0003 {
        on_foreground_changed(hwnd);
    } else {
        schedule_window_sync();
    }
}

/// Marks the window model dirty; the actual re-sync runs once the
/// event burst quiets down (`SYNC_TIMER_ID` in `wndproc` — re-setting
/// an existing timer restarts it, which is the debounce).
fn schedule_window_sync() {
    let primary = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
    if let Some(primary) = primary {
        // SAFETY: `primary` is a valid, process-lifetime window.
        unsafe {
            SetTimer(primary, SYNC_TIMER_ID, SYNC_DEBOUNCE_MS, None);
        }
    }
}

/// The debounced sync itself: reconcile the tracker, refresh the bar's
/// workspace dots (a new window can grow the dynamic tail), and rebuild
/// the overview's pages if it's open.
pub(crate) fn on_window_sync_timer(bar_hwnd: HWND) {
    // SAFETY: `bar_hwnd` is the window whose timer just fired.
    unsafe {
        let _ = KillTimer(bar_hwnd, SYNC_TIMER_ID);
    }
    sync_workspaces();
    refresh_bar_indicator();
    let monitors: Vec<String> = STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.overviews.keys().cloned().collect()).unwrap_or_default()
    });
    for monitor in &monitors {
        rebuild_open_overview_pages(monitor);
    }
}

thread_local! {
    /// Set by [`suppress_foreground_follow`] to make `on_foreground_changed`
    /// ignore foreground-change events for a short window.
    static SUPPRESS_FOLLOW_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Ignores the next `duration` of foreground-change events entirely,
/// rather than just skipping the one we know is stale. Closing the
/// overview onto a workspace with nothing to focus explicitly hands
/// focus to the desktop (see `overview.rs`'s `Completion::Closed`) —
/// but that handoff isn't the end of it: confirmed live, Windows
/// reliably re-activates whatever real application window was legitimately
/// active before (e.g. the terminal the user is actually working in) a
/// couple of milliseconds later, on its own, as if the desktop never
/// really "stuck" as foreground. `on_foreground_changed` would then
/// dutifully follow *that* window back to its workspace, silently
/// undoing the switch the user just made. There's no single window
/// handle to filter out — it's whatever the OS decides to hand focus
/// back to — so the fix is a brief cooldown on the whole follow
/// behavior instead.
pub(crate) fn suppress_foreground_follow(duration: Duration) {
    SUPPRESS_FOLLOW_UNTIL.with(|c| c.set(Some(Instant::now() + duration)));
}

/// A window from another process just became foreground. If it lives
/// on a non-current workspace (still reachable through Alt+Tab), follow
/// it there — its parked position would otherwise leave the user
/// staring at a focused window they can't see.
fn on_foreground_changed(hwnd: HWND) {
    let suppressed =
        SUPPRESS_FOLLOW_UNTIL.with(|c| c.get().is_some_and(|until| Instant::now() < until));
    if suppressed {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_of_window(hwnd) else {
        return;
    };
    let target = STATE.with(|s| {
        let state_ref = s.borrow();
        let st = state_ref.as_ref()?;
        let tracker = st.workspaces.get(&monitor)?;
        let id = tracker.workspace_of(hwnd.0 as isize)?;
        let index = tracker.index_of(id)?;
        (index != tracker.current_index()).then_some(index)
    });
    if let Some(index) = target {
        commit_workspace_switch(&monitor, index);
    }
}

/// Re-syncs the workspace tracker against reality — assigns any
/// currently-visible-but-untracked window and drops assignments for
/// windows that no longer exist. WinEvent hooks keep this fresh in the
/// background (see `install_win_event_hooks`); workspace-changing
/// actions still re-sync inline first so they never act on a stale
/// model, even mid-debounce.
///
/// Assignment rule: a new window belongs to the workspace the user is
/// actually looking at. When the current workspace is a dynamic one,
/// that's simply "current" — matching it against monitor position
/// instead would pull it onto the monitor's pinned workspace, which is
/// exactly the bug where a window opened on workspace 2 "jumped" back
/// to workspace 0 at the next sync. Only when the current workspace is
/// pinned (so every visible workspace is a real monitor) does screen
/// position decide *which* monitor's workspace it belongs to.
/// Returns the live snapshot taken to do this, for callers that also
/// need it (building overview pages).
pub(crate) fn sync_workspaces() -> Vec<groveshell_window_model::WindowRecord> {
    let live = super::state::filter_ignored(groveshell_window_model::snapshot());
    let monitors = monitors_sorted_by_x();
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            for window in &live {
                // Identity first: a recycled HWND (same handle, new
                // process) must not inherit the dead window's
                // assignment or park-time snapshot.
                let (_, reused) = state.window_registry.observe(window.hwnd, window.pid);
                if reused {
                    // A recycled hwnd may have belonged to any monitor's
                    // tracker; `forget` on a tracker that never had it
                    // assigned is a documented no-op.
                    for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
                        if let Some(tracker) = state.workspaces.get_mut(&name) {
                            tracker.forget(window.hwnd);
                        }
                    }
                    drop_window_snapshot(window.hwnd);
                }
                let center_x = (window.rect.left + window.rect.right) / 2;
                let center_y = (window.rect.top + window.rect.bottom) / 2;
                let real_monitor = super::monitors::monitor_index_for_center(&monitors, center_x, center_y)
                    .and_then(|i| monitors.get(i))
                    .map(|m| m.device_name.clone());

                match (state.workspaces.monitor_of_window(window.hwnd), real_monitor) {
                    (Some(tracked_monitor), Some(real)) if tracked_monitor != real => {
                        // Win32 does not move an owned window (a dialog,
                        // color picker, etc.) when its owner is dragged
                        // to another monitor by its title bar — shift
                        // every currently-owned window by exactly the
                        // delta the owner itself moved since the last
                        // sync tick, preserving their relative offset.
                        if let Some(&old_rect) = state.window_rects.get(&window.hwnd) {
                            let dx = window.rect.left - old_rect.left;
                            let dy = window.rect.top - old_rect.top;
                            if dx != 0 || dy != 0 {
                                for owned in groveshell_window_model::owned_windows_of(window.hwnd) {
                                    // SAFETY: `owned` came from a live
                                    // `EnumWindows` pass moments ago; if
                                    // it's since closed, both calls below
                                    // documented-fail harmlessly.
                                    unsafe {
                                        let owned_hwnd = HWND(owned as *mut c_void);
                                        let mut owned_raw = windows::Win32::Foundation::RECT::default();
                                        if GetWindowRect(owned_hwnd, &mut owned_raw).is_err() {
                                            continue;
                                        }
                                        let shifted = shift_rect(
                                            groveshell_window_model::Rect::from(owned_raw),
                                            dx,
                                            dy,
                                        );
                                        let _ = SetWindowPos(
                                            owned_hwnd,
                                            HWND(std::ptr::null_mut()),
                                            shifted.left,
                                            shifted.top,
                                            0,
                                            0,
                                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                                        );
                                    }
                                }
                            }
                        }
                        // Physically dragged to a different monitor since the last
                        // sync — move it onto the new monitor's *current* workspace
                        // rather than leaving it assigned to a monitor it's no
                        // longer on.
                        if let Some(t) = state.workspaces.get_mut(&tracked_monitor) {
                            t.forget(window.hwnd);
                        }
                        if let Some(t) = state.workspaces.get_mut(&real) {
                            let index = t.current_index();
                            t.assign_to_index(window.hwnd, index);
                        }
                    }
                    (None, real) => {
                        if let Some((target_monitor, index)) = super::pending_launch::take_match(window.pid) {
                            if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                                tracker.assign_to_index(window.hwnd, index);
                            }
                        } else {
                            let target_monitor = real.unwrap_or_else(|| state.primary_monitor.clone());
                            if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                                let index = tracker.current_index();
                                tracker.assign_to_index(window.hwnd, index);
                            }
                        }
                    }
                    _ => {}
                }
                state.window_rects.insert(window.hwnd, window.rect);
            }
            for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
                if let Some(tracker) = state.workspaces.get_mut(&name) {
                    tracker.prune(groveshell_window_model::is_alive);
                }
            }
            state.window_registry.prune(groveshell_window_model::is_alive);
            state.window_rects.retain(|&hwnd, _| groveshell_window_model::is_alive(hwnd));
            let tracked = state.workspaces.all_tracked_windows();
            retain_window_snapshots(&tracked);
        }
    });
    live
}

/// Shifts every corner of `rect` by `(dx, dy)` — used to translate an
/// owned window's current position by exactly the delta its owner moved
/// between two `sync_workspaces` ticks, preserving their relative offset.
fn shift_rect(rect: groveshell_window_model::Rect, dx: i32, dy: i32) -> groveshell_window_model::Rect {
    groveshell_window_model::Rect {
        left: rect.left + dx,
        top: rect.top + dy,
        right: rect.right + dx,
        bottom: rect.bottom + dy,
    }
}

#[cfg(test)]
mod tests {
    use super::shift_rect;
    use groveshell_window_model::Rect;

    #[test]
    fn shift_rect_moves_every_corner_by_the_same_delta() {
        let rect = Rect { left: 100, top: 200, right: 300, bottom: 400 };
        let shifted = shift_rect(rect, 50, -20);
        assert_eq!(shifted, Rect { left: 150, top: 180, right: 350, bottom: 380 });
    }
}
