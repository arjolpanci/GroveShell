//! Phase 2: global window move/resize and hot corners.
//!
//! GNOME binds these to the Super key, not Alt — Alt+drag is a
//! per-application convention (GIMP, some window managers) that varies,
//! while Super+drag is the actual GNOME/desktop-environment-wide
//! behavior this shell is modeled on. Getting the real Windows key to
//! do it means swallowing every `LWIN`/`RWIN` key event globally (a
//! low-level keyboard hook that returns non-zero) so Windows never sees
//! them and never pops the Start Menu — the same trick PowerToys-style
//! "remap this key" tools use. That leaves the key free to reassign: a
//! plain tap toggles the Activities overview (`Role`'s "Activities"
//! button, just from the keyboard), and holding it down while
//! left/right-dragging moves or resizes whatever window is under the
//! cursor, tracked through a second low-level hook on the mouse.
//!
//! Both hooks are `_LL` (low-level): they run synchronously, on this
//! thread, for *every* keyboard/mouse event system-wide, blocking
//! further system-wide input dispatch until they return. That rules out
//! calling `SetWindowPos` directly from the mouse hook's `WM_MOUSEMOVE`
//! handler — it sends `WM_WINDOWPOSCHANGING`/`WM_WINDOWPOSCHANGED`
//! *synchronously* to the target window's own thread, and blocking on a
//! foreign process's thread from inside a system-wide input hook is
//! exactly the kind of thing that causes the whole desktop's mouse input
//! to stutter and back up — which is what an earlier version of this
//! module did, and which showed up as the drag visibly lagging behind
//! the cursor and "catching up" in jumps. The hook itself only ever
//! flips a `Cell`; the actual `SetWindowPos` work happens on a plain
//! timer tick (`DRAG_TIMER_ID`) running on this thread's normal message
//! loop, decoupled from the input pipeline entirely.
//!
//! One more consequence of that: the mouse hook must *never* swallow
//! `WM_MOUSEMOVE`. Returning non-zero from a `WH_MOUSE_LL` callback
//! discards the raw input event before Windows updates the system
//! cursor position from it — not just before any window sees a message.
//! Swallowing every move while dragging (to stop the app under the
//! cursor from reacting to it) sounded harmless but actually froze the
//! on-screen cursor and `GetCursorPos` in place for as long as the drag
//! lasted, confirmed live via the timer's own logged ticks reporting the
//! exact same point for a full second of real mouse movement. Letting
//! `WM_MOUSEMOVE` fall through unconditionally costs nothing — nobody
//! but this module's own `DRAG_TIMER_ID` poll needs to react to it.

use std::cell::Cell;
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetCursorPos, GetWindowRect, IsZoomed, KillTimer, SetTimer,
    SetWindowPos, SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, GA_ROOT, HHOOK,
    KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::state::{role_of, Role, STATE};

const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// Polls the cursor for hot-corner dwell — see `check_hot_corners`. Its
/// own timer (rather than reusing the 1s clock timer) because a corner
/// trigger needs to feel instant, not laggy.
pub(crate) const HOTCORNER_TIMER_ID: usize = 4;
pub(crate) const HOTCORNER_INTERVAL_MS: u32 = 100;

/// Drives the in-progress move/resize's `SetWindowPos` calls — see the
/// module docs on why this can't happen directly in the mouse hook.
/// Only runs while a drag is active (started in `start_drag`, stopped
/// in `end_drag`); ~60fps, same cadence as the overview's own animation
/// timer.
pub(crate) const DRAG_TIMER_ID: usize = 5;
const DRAG_TIMER_INTERVAL_MS: u32 = 16;

/// Below this, a resize drag is refused rather than shrinking a window
/// into nothing — a 96-DPI value, scaled at the drag's own monitor DPI
/// same as everything else in this codebase.
const MIN_WINDOW_SIZE: i32 = 120;

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    /// Which edges follow the cursor; the opposite edges stay put. Set
    /// once, from which quadrant of the window the drag started in.
    Resize { grow_right: bool, grow_bottom: bool },
}

#[derive(Clone, Copy)]
struct MoveSizeDrag {
    target: HWND,
    mode: DragMode,
    start_cursor: POINT,
    start_rect: RECT,
}

thread_local! {
    static KEYBOARD_HOOK: Cell<isize> = const { Cell::new(0) };
    static MOUSE_HOOK: Cell<isize> = const { Cell::new(0) };
    static WIN_HELD: Cell<bool> = const { Cell::new(false) };
    /// Set as soon as the held Win key is used for anything (a drag) —
    /// suppresses the "just a tap" Activities toggle on key-up.
    static WIN_USED: Cell<bool> = const { Cell::new(false) };
    static DRAG: Cell<Option<MoveSizeDrag>> = const { Cell::new(None) };
}

/// Installs the two low-level hooks. Best-effort like the rest of this
/// module's global input hooks (`WinEventHook`, `RegisterHotKey`):
/// another process holding an incompatible hook chain is rare but not
/// fatal to the rest of the shell.
pub(crate) fn install_move_size_hooks() {
    // SAFETY: both callbacks are valid `HOOKPROC`s for the process
    // lifetime; `hmod: None` + `thread_id: 0` is the documented way to
    // hook system-wide from a GUI thread with a message loop, which
    // `main`'s `GetMessageW` loop provides.
    unsafe {
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0);
        if let Ok(kb) = kb {
            KEYBOARD_HOOK.with(|h| h.set(kb.0 as isize));
        }
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0);
        if let Ok(mouse) = mouse {
            MOUSE_HOOK.with(|h| h.set(mouse.0 as isize));
        }
    }
}

pub(crate) fn uninstall_move_size_hooks() {
    // SAFETY: each handle came from `SetWindowsHookExW` above and is
    // unhooked at most once.
    unsafe {
        let kb = KEYBOARD_HOOK.with(|h| h.replace(0));
        if kb != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(kb as *mut c_void));
        }
        let mouse = MOUSE_HOOK.with(|h| h.replace(0));
        if mouse != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(mouse as *mut c_void));
        }
    }
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    // SAFETY: a low-level keyboard hook's `lparam` always points to a
    // live `KBDLLHOOKSTRUCT` for the duration of this call.
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if info.vkCode == VK_LWIN || info.vkCode == VK_RWIN {
        let msg = wparam.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            if !WIN_HELD.with(|h| h.get()) {
                WIN_HELD.with(|h| h.set(true));
                WIN_USED.with(|u| u.set(false));
            }
            return LRESULT(1);
        }
        if msg == WM_KEYUP || msg == WM_SYSKEYUP {
            WIN_HELD.with(|h| h.set(false));
            let used = WIN_USED.with(|u| u.get());
            if !used {
                toggle_overview();
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn toggle_overview() {
    let mut pt = POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_at_point(pt) else {
        return;
    };
    super::overview::toggle_overview_for(&monitor);
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || !WIN_HELD.with(|h| h.get()) {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    // SAFETY: a low-level mouse hook's `lparam` always points to a live
    // `MSLLHOOKSTRUCT` for the duration of this call.
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let msg = wparam.0 as u32;

    match msg {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN if start_drag(info.pt, msg == WM_RBUTTONDOWN) => {
            WIN_USED.with(|u| u.set(true));
            return LRESULT(1);
        }
        WM_LBUTTONUP | WM_RBUTTONUP if end_drag() => return LRESULT(1),
        _ => {}
    }
    // `WM_MOUSEMOVE` always falls through — see the module docs on why
    // swallowing it froze the cursor. `DRAG_TIMER_ID` reads the live
    // position itself instead of reacting to individual move events.
    CallNextHookEx(None, code, wparam, lparam)
}

/// Finds the top-level window under `pt` and starts a move or resize
/// drag on it, unless it's one of this shell's own windows or not an
/// "ordinary application window" by the same eligibility policy the
/// Activities overview itself uses. Returns whether a drag started (and
/// therefore whether the triggering button-down should be swallowed).
fn start_drag(pt: POINT, is_resize: bool) -> bool {
    if DRAG.with(|d| d.get().is_some()) {
        return false;
    }
    // SAFETY: `pt` came from the mouse hook's own hook struct; a plain
    // point-to-window query has no further preconditions.
    let under_cursor = unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(pt) };
    if under_cursor.0.is_null() {
        return false;
    }
    // SAFETY: `under_cursor` was just returned live from `WindowFromPoint`.
    let target = unsafe { GetAncestor(under_cursor, GA_ROOT) };
    if target.0.is_null() || role_of(target) != Role::Other {
        return false;
    }
    if groveshell_window_model::describe(target.0 as isize).is_none() {
        return false;
    }
    let mut rect = RECT::default();
    // SAFETY: `target` is a live top-level window (just checked above).
    if unsafe { GetWindowRect(target, &mut rect) }.is_err() {
        return false;
    }

    // A maximized window keeps re-asserting its maximized bounds against
    // plain `SetWindowPos` calls (the same reason a real titlebar drag
    // restores it first) — left alone, the drag looks like it's fighting
    // the cursor or not moving at all. Restore it first, the same as a
    // native titlebar drag would, keeping the cursor over the same
    // relative point in the window so it doesn't jump.
    if unsafe { IsZoomed(target) }.as_bool() {
        let rel_x = (pt.x - rect.left) as f64 / (rect.right - rect.left).max(1) as f64;
        let rel_y = (pt.y - rect.top) as f64 / (rect.bottom - rect.top).max(1) as f64;
        // SAFETY: `target` is a live top-level window.
        unsafe {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        let mut restored = RECT::default();
        // SAFETY: `target` is still the same live window just restored.
        if unsafe { GetWindowRect(target, &mut restored) }.is_err() {
            return false;
        }
        let w = restored.right - restored.left;
        let h = restored.bottom - restored.top;
        let new_left = pt.x - (rel_x * w as f64).round() as i32;
        let new_top = pt.y - (rel_y * h as f64).round() as i32;
        // SAFETY: `target` is still the same live window.
        unsafe {
            let _ = SetWindowPos(target, None, new_left, new_top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        rect = RECT { left: new_left, top: new_top, right: new_left + w, bottom: new_top + h };
    }

    let mode = if is_resize {
        let mid_x = (rect.left + rect.right) / 2;
        let mid_y = (rect.top + rect.bottom) / 2;
        DragMode::Resize {
            grow_right: pt.x >= mid_x,
            grow_bottom: pt.y >= mid_y,
        }
    } else {
        DragMode::Move
    };

    DRAG.with(|d| {
        d.set(Some(MoveSizeDrag {
            target,
            mode,
            start_cursor: pt,
            start_rect: rect,
        }))
    });
    let primary = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
    if let Some(primary) = primary {
        // SAFETY: `primary` is a valid, process-lifetime window.
        unsafe {
            SetTimer(primary, DRAG_TIMER_ID, DRAG_TIMER_INTERVAL_MS, None);
        }
    }
    true
}

/// Ticks `DRAG_TIMER_ID`: applies the in-progress drag's effect for
/// wherever the cursor actually is right now. Polling `GetCursorPos`
/// here (rather than trusting the mouse hook's per-event `pt`) is
/// deliberate — see the module docs on why the hook itself does no
/// positioning work, only records that a drag is active.
pub(crate) fn on_drag_timer() {
    let Some(drag) = DRAG.with(|d| d.get()) else {
        return;
    };
    let mut pt = POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let dx = pt.x - drag.start_cursor.x;
    let dy = pt.y - drag.start_cursor.y;

    let (x, y, w, h) = match drag.mode {
        DragMode::Move => (
            drag.start_rect.left + dx,
            drag.start_rect.top + dy,
            drag.start_rect.right - drag.start_rect.left,
            drag.start_rect.bottom - drag.start_rect.top,
        ),
        DragMode::Resize { grow_right, grow_bottom } => {
            let (left, right) = if grow_right {
                (drag.start_rect.left, (drag.start_rect.right + dx).max(drag.start_rect.left + MIN_WINDOW_SIZE))
            } else {
                ((drag.start_rect.left + dx).min(drag.start_rect.right - MIN_WINDOW_SIZE), drag.start_rect.right)
            };
            let (top, bottom) = if grow_bottom {
                (drag.start_rect.top, (drag.start_rect.bottom + dy).max(drag.start_rect.top + MIN_WINDOW_SIZE))
            } else {
                ((drag.start_rect.top + dy).min(drag.start_rect.bottom - MIN_WINDOW_SIZE), drag.start_rect.bottom)
            };
            (left, top, right - left, bottom - top)
        }
    };

    // SAFETY: `drag.target` was validated live when the drag started; if
    // it closed mid-drag this documented-fails harmlessly and the next
    // button-up just ends a drag with no further effect.
    unsafe {
        let _ = SetWindowPos(drag.target, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Ends the in-progress drag, if any, and stops `DRAG_TIMER_ID`. Returns
/// whether one was active (and therefore whether this button-up event
/// should be swallowed).
fn end_drag() -> bool {
    let had = DRAG.with(|d| d.take()).is_some();
    if had {
        let primary = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
        if let Some(primary) = primary {
            // SAFETY: `primary` is a valid, process-lifetime window.
            unsafe {
                let _ = KillTimer(primary, DRAG_TIMER_ID);
            }
        }
    }
    had
}

/// Hot corners (Phase 2): dwelling in a monitor's top-left pixel opens
/// the Activities overview, same trigger as GNOME's default corner.
/// Polled off the existing 1s clock timer would be too coarse to feel
/// responsive, so this runs on its own fast timer instead — see
/// `HOTCORNER_TIMER_ID` in `mod.rs`. Edge-triggered (only fires the
/// instant the cursor arrives, not on every tick while it sits there),
/// so leaving the mouse parked in the corner doesn't reopen the
/// overview right after closing it.
const HOTCORNER_ZONE: i32 = 3;

thread_local! {
    static IN_HOT_CORNER: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn check_hot_corners() {
    let mut pt = POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let monitors = super::monitors::monitors_sorted_by_x();
    let corner_monitor = monitors.iter().find(|m| {
        pt.x >= m.rect.left
            && pt.x < m.rect.left + HOTCORNER_ZONE
            && pt.y >= m.rect.top
            && pt.y < m.rect.top + HOTCORNER_ZONE
    });
    let in_corner = corner_monitor.is_some();
    let was_in_corner = IN_HOT_CORNER.with(|c| c.replace(in_corner));
    if in_corner && !was_in_corner {
        if let Some(m) = corner_monitor {
            super::overview::open_overview(&m.device_name);
        }
    }
}
