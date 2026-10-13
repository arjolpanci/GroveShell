//! Windows input to gesture progress: the bridge between wheel and
//! precision-touchpad scroll events and `groveshell_motion`'s
//! `SwipeTracker`.
//!
//! Two-finger touchpad scroll arrives as `WM_MOUSEWHEEL` with
//! *fractional* notch deltas, which is exactly the continuous signal a
//! swipe tracker wants — so the same code path serves a real touchpad
//! gesture and a mouse wheel, with the physics smoothing the wheel's
//! coarse notches.

use std::cell::RefCell;
use std::time::Instant;

use groveshell_motion::{swipe::SCROLL_MULTIPLIER, InputKind, SwipeTracker};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::state::{ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, STATE};

/// Timer that ends a gesture when scroll events stop arriving. A wheel
/// has no finger-lift, so idleness is the only available release signal;
/// GNOME's own scroll path does the same.
pub(crate) const GESTURE_IDLE_TIMER_ID: usize = 7;
pub(crate) const GESTURE_END_IDLE_MS: u32 = 50;

/// `WHEEL_DELTA`: one notch of a traditional wheel.
const WHEEL_DELTA: f64 = 120.0;

/// Which surface the live gesture is driving.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Overview,
    Carousel,
}

thread_local! {
    /// The live gesture. One at a time: a second axis arriving mid-swipe
    /// is folded into the one already running rather than starting a
    /// rival that would fight it for the same adjustment.
    static GESTURE: RefCell<Option<(String, Target, SwipeTracker)>> = const { RefCell::new(None) };
}

/// Gesture pixels for one `WM_MOUSEWHEEL` delta. Deliberately *not*
/// rounded to whole notches: a precision touchpad reports fractions, and
/// rounding them away is what turns a smooth two-finger drag into a
/// staircase.
pub(crate) fn scroll_delta_px(wheel_delta: i16) -> f64 {
    wheel_delta as f64 / WHEEL_DELTA * SCROLL_MULTIPLIER
}

/// How many wheel notches of travel make one full state change, and one
/// full workspace page.
///
/// GNOME uses the screen height as its swipe distance
/// (`overviewControls.js:758`), but that is a *touchpad swipe* measured
/// in touchpad pixels — GNOME deliberately does not drive the overview
/// from scroll at all (`overview.js:231` passes `allowScroll: false`).
/// Our gesture is scroll-driven, so its distance has to be in scroll
/// units or it is unreachable: at ten pixels per notch, a screen-height
/// distance would need ~186 notches to open the overview once.
const NOTCHES_PER_STATE: f64 = 6.0;
const NOTCHES_PER_PAGE: f64 = 4.0;

/// Gesture distance for a full `0 -> 1` overview transition, in the same
/// pixel units [`scroll_delta_px`] produces.
pub(crate) fn gesture_distance() -> f64 {
    NOTCHES_PER_STATE * SCROLL_MULTIPLIER
}

/// Gesture distance for one workspace page.
pub(crate) fn carousel_distance() -> f64 {
    NOTCHES_PER_PAGE * SCROLL_MULTIPLIER
}

/// Packs a wheel delta and its axis into one `WPARAM`-sized word.
///
/// The delta goes in as *unsigned* 16 bits. Sign-extending it instead
/// would let a negative scroll set the axis bit, so scrolling up would
/// read back as a horizontal gesture and switch workspace.
pub(crate) fn pack_scroll(wheel_delta: i16, horizontal: bool) -> usize {
    (wheel_delta as u16 as usize) | ((horizontal as usize) << 16)
}

pub(crate) fn unpack_scroll(packed: usize) -> (i16, bool) {
    ((packed & 0xFFFF) as u16 as i16, (packed >> 16) & 1 == 1)
}

/// Whether a scroll event should be allowed to start or continue a
/// shell gesture, given whether this monitor's overview is already up
/// and the root window class **under the pointer**.
///
/// The low-level hook sees **every** wheel event in the system, so
/// without this gate scrolling a web page pulls Activities open and
/// steals focus, and a horizontal scroll through a wide spreadsheet
/// switches workspace with nothing on screen to explain it.
///
/// Keyed on the pointer rather than on focus, deliberately. "Scrolling
/// on the desktop" means the pointer is over the desktop, whatever holds
/// focus — and scrolling over an application is exactly the case where
/// the scroll belongs to that application. A focus-keyed gate read the
/// same situation backwards often enough to make the gesture unreachable
/// in ordinary use, because the desktop rarely holds focus.
///
/// Once the overview is live it owns the screen and takes the scroll
/// regardless, which is what lets a scroll reverse an open.
pub(crate) fn gesture_allowed(overview_live: bool, pointer_class: &str) -> bool {
    if overview_live {
        return true;
    }
    matches!(
        pointer_class,
        // The desktop: `Progman` normally, `WorkerW` when a wallpaper
        // host has been slid underneath it.
        "Progman" | "WorkerW"
    ) || pointer_class.starts_with("GroveShell")
}

/// The class of the **root** window under the pointer, for
/// [`gesture_allowed`].
///
/// `GA_ROOT` matters: the desktop's icon list is a `SysListView32`
/// inside a `SHELLDLL_DefView` inside `Progman`, and hit-testing returns
/// the deepest child. Without walking to the root, a pointer over the
/// desktop reads as a list view and the gesture is refused.
fn pointer_class() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClassNameW, GetCursorPos, WindowFromPoint, GA_ROOT,
    };
    // SAFETY: plain queries; an empty name is returned rather than
    // misbehaving if the pointer is over nothing.
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return String::new();
        }
        let hwnd = WindowFromPoint(pt);
        if hwnd.0.is_null() {
            return String::new();
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        let target = if root.0.is_null() { hwnd } else { root };
        let mut buf = [0u16; 256];
        let n = GetClassNameW(target, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Handles one scroll event on the UI thread.
///
/// Everything here — deciding the target, arming the tracker, driving
/// the adjustment — runs here rather than in the hook, which must do
/// nothing but enqueue (see `movesize::mouse_hook_proc`).
pub(crate) fn on_scroll(wheel_delta: i16, horizontal: bool, time_ms: u64, monitor: &str) {
    let delta = scroll_delta_px(wheel_delta);
    if delta == 0.0 {
        return;
    }

    // Read what we need and drop the borrow before touching Win32.
    let Some((overview_hwnd, open, progress, carousel, transition_target)) = STATE.with(|s| {
        let state = s.borrow();
        let ov = state.as_ref()?.overviews.get(monitor)?;
        let open = !matches!(ov.mode, super::overview::OverviewMode::Closed);
        Some((
            ov.hwnd,
            open,
            ov.adjustment.value(),
            ov.carousel.value(),
            ov.adjustment.transition_target(),
        ))
    }) else {
        return;
    };

    // C1/C3: the hook sees every wheel event in the system. Nothing may
    // start from a scroll aimed at an ordinary application.
    if !gesture_allowed(open, &pointer_class()) {
        return;
    }

    let (pages, _current) = super::overview::strip_pages(monitor);
    // Vertical drives the overview transition, horizontal the carousel —
    // one axis each, both ways.
    //
    // The plan routed vertical-while-open to the carousel, following
    // GNOME, where scrolling over the overview switches workspace. But
    // GNOME's overview gesture is a three-finger swipe that reverses on
    // itself, and scroll is merely a second, separate input there
    // (`overview.js:231` passes `allowScroll: false` to the overview's
    // own tracker). Here scroll *is* the gesture, so routing it away the
    // moment the overview settles would mean a user could scroll down to
    // open and then have no way to scroll back out — the one thing this
    // whole port exists to make possible. Workspace switching keeps the
    // horizontal axis, which precision touchpads produce natively, plus
    // the strip and Ctrl+Alt+Arrows.
    let target = if horizontal { Target::Carousel } else { Target::Overview };

    // Scrolling down pulls the overview open; the wheel's sign is the
    // opposite of the gesture's, same as GNOME's `_getGestureDirFactor`.
    let signed = -delta;

    // Build the overview *before* the tracker runs: `open_overview_at`
    // parks the adjustment at its starting value, which would otherwise
    // wipe the first event's progress and make the gesture feel like it
    // swallowed a frame.
    if !open && target == Target::Overview {
        super::overview::open_overview_at(monitor, 0.0);
    }

    // A live gesture on a different monitor or axis is *ended*, never
    // silently replaced: abandoning it strands `gesture_in_progress` on
    // the overview's adjustment, and nothing else ever clears it — the
    // overview can then neither open nor close, and sits as a
    // full-screen topmost window swallowing every click.
    let stale = GESTURE.with(|g| {
        let slot = g.borrow();
        match slot.as_ref() {
            Some((m, t, tracker)) if m != monitor || *t != target || !tracker.is_scrolling() => {
                Some(m.clone())
            }
            _ => None,
        }
    });
    if let Some(stale_monitor) = stale {
        release_gesture(&stale_monitor);
    }

    GESTURE.with(|g| {
        let mut slot = g.borrow_mut();
        let fresh = match slot.as_ref() {
            Some((m, t, tracker)) => m != monitor || *t != target || !tracker.is_scrolling(),
            None => true,
        };
        if fresh {
            // `Touch` rather than `Touchpad`: its lower velocity threshold
            // (0.3 vs 0.6 px/ms) suits scroll units, where a brisk flick
            // is a few notches rather than a continuous glide.
            let mut tracker = SwipeTracker::new(InputKind::Touch);
            match target {
                Target::Overview => {
                    // A gesture interrupting a running transition cancels
                    // to that transition's target, not to the live value
                    // — otherwise letting go without moving snaps
                    // backwards.
                    let cancel = transition_target.unwrap_or_else(|| progress.round());
                    tracker.confirm_swipe(gesture_distance(), vec![0.0, 1.0], progress, cancel);
                }
                Target::Carousel => {
                    tracker.confirm_swipe(
                        carousel_distance(),
                        super::overview::carousel_snap_points(pages),
                        carousel,
                        carousel.round(),
                    );
                }
            }
            *slot = Some((monitor.to_string(), target, tracker));
        }

        let Some((_, _, tracker)) = slot.as_mut() else { return };
        let value = tracker.append_event(time_ms, signed);

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                if let Some(ov) = state.overviews.get_mut(monitor) {
                    match target {
                        Target::Overview => {
                            ov.adjustment.set_gesture_in_progress(true);
                            ov.adjustment.set_value(value);
                        }
                        Target::Carousel => ov.carousel.set_value(value),
                    }
                }
            }
        });
    });

    // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
    unsafe {
        SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
        SetTimer(overview_hwnd, GESTURE_IDLE_TIMER_ID, GESTURE_END_IDLE_MS, None);
    }
}

/// Drops any gesture state for `monitor` without committing it: clears
/// the adjustment's gesture flag and kills the idle timer.
///
/// Every exit path goes through here. `gesture_in_progress` left set is
/// not a cosmetic leak — `on_animation_tick` gates its completion check
/// on it, so the overview can never finish opening or closing.
fn release_gesture(monitor: &str) {
    GESTURE.with(|g| {
        let mut slot = g.borrow_mut();
        if slot.as_ref().is_some_and(|(m, _, _)| m == monitor) {
            *slot = None;
        }
    });
    let hwnd = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let ov = state.overviews.get_mut(monitor)?;
        ov.adjustment.set_gesture_in_progress(false);
        Some(ov.hwnd)
    });
    if let Some(hwnd) = hwnd {
        // SAFETY: a valid, process-lifetime window; killing a timer that
        // is not set is a documented no-op.
        unsafe {
            let _ = KillTimer(hwnd, GESTURE_IDLE_TIMER_ID);
        }
    }
}

/// Cancels any live gesture on `monitor` — the overview being torn down
/// under it, or the monitor going away mid-swipe.
pub(crate) fn interrupt(monitor: &str) {
    release_gesture(monitor);
}

/// No scroll event for `GESTURE_END_IDLE_MS`: the gesture is over.
/// Releases it at the velocity its recent history implies, which is what
/// turns a flick into a fling.
pub(crate) fn on_idle_timeout(monitor: &str) {
    let now_ms = Instant::now();
    let _ = now_ms;

    let finished = GESTURE.with(|g| {
        let mut slot = g.borrow_mut();
        let (m, target, tracker) = slot.as_mut()?;
        if m != monitor {
            // The timer belongs to a monitor the gesture has since left.
            // Falling through without killing it leaves a 20 Hz timer
            // running on that overview window for the life of the
            // process.
            return None;
        }
        let target = *target;
        // The same clock the hook stamped events with.
        let end = tracker.end_from_history(tick_ms());
        *slot = None;
        Some((target, end))
    });

    let Some((target, end)) = finished else {
        // No gesture of ours to finish: make sure the timer that woke us
        // does not fire again.
        let hwnd = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| st.overviews.get(monitor)).map(|ov| ov.hwnd)
        });
        if let Some(hwnd) = hwnd {
            // SAFETY: as above.
            unsafe {
                let _ = KillTimer(hwnd, GESTURE_IDLE_TIMER_ID);
            }
        }
        return;
    };

    let overview_hwnd = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let ov = state.overviews.get_mut(monitor)?;
        let now = Instant::now();
        match target {
            Target::Overview => {
                ov.adjustment.set_gesture_in_progress(false);
                ov.adjustment
                    .ease_to_at(end.end_progress, end.duration_ms, end.easing, now);
                if end.end_progress <= f64::EPSILON {
                    ov.close_after = None;
                }
            }
            Target::Carousel => {
                ov.carousel
                    .ease_to_at(end.end_progress, end.duration_ms, end.easing, now);
                ov.carousel_close_after = None;
            }
        }
        Some(ov.hwnd)
    });

    if let Some(overview_hwnd) = overview_hwnd {
        // SAFETY: a valid, process-lifetime window.
        unsafe {
            let _ = KillTimer(overview_hwnd, GESTURE_IDLE_TIMER_ID);
            SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
        }
    }

    // A carousel fling that lands on a different page has to commit the
    // workspace switch, same as a released pointer drag does.
    if target == Target::Carousel {
        let page = end.end_progress.round().max(0.0) as usize;
        super::overview::snap_carousel_to(monitor, page, None);
    }
}

/// Milliseconds on the same clock `MSLLHOOKSTRUCT::time` uses, so
/// gesture velocities are computed from one consistent timebase.
fn tick_ms() -> u64 {
    // SAFETY: no preconditions; returns a tick count.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

thread_local! {
    /// The window scroll events are posted to, set when the hooks are
    /// installed. A thread-local is the right channel: a low-level mouse
    /// hook's callback runs on the thread that installed it.
    static GESTURE_TARGET: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn set_gesture_target(hwnd: HWND) {
    GESTURE_TARGET.with(|t| t.set(hwnd.0 as isize));
}

pub(crate) fn gesture_target() -> Option<HWND> {
    GESTURE_TARGET.with(|t| {
        let h = t.get();
        (h != 0).then_some(HWND(h as *mut std::ffi::c_void))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scroll_with_the_pointer_over_an_application_never_starts_a_gesture() {
        // The hook sees every wheel event in the system. Without a gate,
        // scrolling a web page opens Activities and steals focus.
        assert!(!gesture_allowed(false, "Chrome_WidgetWin_1"));
        assert!(!gesture_allowed(false, "CabinetWClass"));
        assert!(!gesture_allowed(false, "XLMAIN"));
    }

    #[test]
    fn a_scroll_over_the_desktop_starts_a_gesture() {
        assert!(gesture_allowed(false, "Progman"));
        assert!(gesture_allowed(false, "WorkerW"));
    }

    #[test]
    fn a_scroll_starts_a_gesture_over_our_own_surfaces() {
        assert!(gesture_allowed(false, "GroveShellBar"));
        assert!(gesture_allowed(false, "GroveShellOverview"));
    }

    #[test]
    fn an_open_overview_accepts_a_scroll_wherever_the_pointer_is() {
        // Once it is up it owns the screen, so what is under the pointer
        // is irrelevant — and this is what lets a scroll reverse an open.
        assert!(gesture_allowed(true, "Chrome_WidgetWin_1"));
        assert!(gesture_allowed(true, ""));
    }

    #[test]
    fn a_full_notch_becomes_one_scroll_unit_of_pixels() {
        assert!((scroll_delta_px(120) - SCROLL_MULTIPLIER).abs() < 1e-9);
        assert!((scroll_delta_px(-120) + SCROLL_MULTIPLIER).abs() < 1e-9);
    }

    #[test]
    fn a_precision_touchpad_sub_notch_delta_is_proportional() {
        // Precision touchpads report fractions of a notch; that is the
        // continuous signal the tracker needs, so it must not be rounded
        // up to a whole notch.
        let quarter = scroll_delta_px(30);
        assert!((quarter - SCROLL_MULTIPLIER / 4.0).abs() < 1e-9);
        assert!(quarter > 0.0);
    }

    #[test]
    fn a_zero_delta_is_no_movement() {
        assert_eq!(scroll_delta_px(0), 0.0);
    }

    /// The real-units version: plain notches, no scaling fudge. An
    /// earlier draft of this test multiplied the delta by ten to make it
    /// pass, which hid that a wheel could not move the gesture at all.
    #[test]
    fn a_handful_of_real_notches_opens_the_overview() {
        let mut t = SwipeTracker::new(InputKind::Touch);
        t.confirm_swipe(gesture_distance(), vec![0.0, 1.0], 0.0, 0.0);
        for i in 0..6 {
            t.append_event(1000 + i * 20, scroll_delta_px(120));
        }
        assert!((t.progress() - 1.0).abs() < 1e-9, "six notches is a full open");
    }

    #[test]
    fn two_notches_alone_do_not_open_it() {
        let mut t = SwipeTracker::new(InputKind::Touch);
        t.confirm_swipe(gesture_distance(), vec![0.0, 1.0], 0.0, 0.0);
        for i in 0..2 {
            t.append_event(1000 + i * 200, scroll_delta_px(120));
        }
        // Slow and short: snaps back rather than committing.
        assert_eq!(t.end_from_history(1400).end_progress, 0.0);
    }

    #[test]
    fn a_brisk_flick_of_a_few_notches_commits_the_open() {
        let mut t = SwipeTracker::new(InputKind::Touch);
        t.confirm_swipe(gesture_distance(), vec![0.0, 1.0], 0.0, 0.0);
        for i in 0..3 {
            t.append_event(1000 + i * 15, scroll_delta_px(120));
        }
        assert_eq!(t.end_from_history(1030).end_progress, 1.0, "momentum finishes it");
    }

    #[test]
    fn the_gesture_distances_are_whole_numbers_of_notches() {
        assert_eq!(gesture_distance(), NOTCHES_PER_STATE * SCROLL_MULTIPLIER);
        assert_eq!(carousel_distance(), NOTCHES_PER_PAGE * SCROLL_MULTIPLIER);
        assert!(gesture_distance() > 0.0 && carousel_distance() > 0.0);
    }

    #[test]
    fn the_wheel_message_packing_round_trips_both_fields() {
        for (delta, horizontal) in [(120i16, false), (-120, false), (30, true), (-7, true)] {
            let packed = pack_scroll(delta, horizontal);
            assert_eq!(unpack_scroll(packed), (delta, horizontal));
        }
    }

    #[test]
    fn a_negative_delta_does_not_leak_into_the_axis_bit() {
        // Sign-extending the delta into the packed word would make
        // scrolling up read back as a horizontal gesture.
        let (delta, horizontal) = unpack_scroll(pack_scroll(-120, false));
        assert_eq!(delta, -120);
        assert!(!horizontal, "a negative vertical scroll must stay vertical");
    }
}
