//! Small painting/animation helpers shared across bar, calendar, quick
//! settings, and overview rendering.

use windows::core::w;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DrawTextW, DRAW_TEXT_FORMAT, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, HDC, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};

use super::state::scaled;

/// A Segoe UI font sized for the bar at `dpi` (caller owns the handle
/// and must `DeleteObject` it after deselecting).
pub(crate) fn bar_font(dpi: u32) -> HFONT {
    // SAFETY: plain object creation; no aliasing or lifetime
    // preconditions.
    unsafe {
        CreateFontW(
            -scaled(12, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET.0.into(),
            OUT_DEFAULT_PRECIS.0.into(),
            CLIP_DEFAULT_PRECIS.0.into(),
            CLEARTYPE_QUALITY.0.into(),
            DEFAULT_PITCH.0.into(),
            w!("Segoe UI"),
        )
    }
}

/// SAFETY: `hdc` must be a valid device context obtained from
/// `BeginPaint` on the window currently handling `WM_PAINT`.
pub(crate) unsafe fn draw_text_in(hdc: HDC, rect: RECT, text: &str, format: DRAW_TEXT_FORMAT) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, format);
}

pub(crate) fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

pub(crate) fn progress_dur(started: std::time::Instant, duration: std::time::Duration) -> f64 {
    (started.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0)
}

pub(crate) fn progress(started: std::time::Instant) -> f64 {
    progress_dur(started, super::state::ANIM_DURATION)
}

/// `SetForegroundWindow`, but reliable even when called from a context
/// Windows doesn't consider a direct result of user input — notably
/// from `WM_TIMER` (an animation completing, e.g. a carousel slide
/// landing on the workspace a clicked window lives on). A plain
/// `SetForegroundWindow` there is silently denied by Windows' anti
/// focus-stealing lock (confirmed live: it returns `false` and nothing
/// happens), even though the whole chain started from a real click a
/// moment earlier — and confirmed live again that the textbook
/// `AttachThreadInput` workaround *alone* isn't always enough either.
/// What reliably works, verified live in this exact scenario, is a
/// synthetic Alt tap first: it resets Windows' internal "was there
/// recent input" heuristic that the lock is actually gated on, after
/// which a plain `SetForegroundWindow` succeeds on its own.
/// `AttachThreadInput` is kept as a second fallback in case some other
/// process's foreground lock is stricter still.
pub(crate) fn force_foreground(target: HWND) {
    // SAFETY: every call here is synchronous; the synthetic Alt tap is
    // a documented, harmless nudge (Windows Explorer and various
    // launchers use the same trick), and `target`'s validity is the
    // caller's documented precondition, same as a direct
    // `SetForegroundWindow` call.
    unsafe {
        if SetForegroundWindow(target).as_bool() {
            return;
        }

        keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        if SetForegroundWindow(target).as_bool() {
            return;
        }

        let current_fg = GetForegroundWindow();
        let mut current_pid = 0u32;
        let current_thread = GetWindowThreadProcessId(current_fg, Some(&mut current_pid));
        let this_thread = GetCurrentThreadId();
        if current_thread != 0 && current_thread != this_thread {
            let _ = AttachThreadInput(this_thread, current_thread, true);
            let _ = SetForegroundWindow(target);
            let _ = AttachThreadInput(this_thread, current_thread, false);
        }
    }
}

/// Simulates "50%-opacity white" over a *known, flat* background color
/// without real alpha blending — plain GDI has no per-pixel alpha, so
/// this just precomputes what that blend would look like and draws a
/// solid rect in that color instead. Only valid where the background
/// really is flat and known, which is true everywhere it's used here
/// (bar background, quick-settings card background).
pub(crate) fn blend_toward_white(bg: u32, amount: f64) -> COLORREF {
    let mix = |channel: u32| -> u32 {
        (channel as f64 + (255.0 - channel as f64) * amount).round() as u32
    };
    let r = mix(bg & 0xFF);
    let g = mix((bg >> 8) & 0xFF);
    let b = mix((bg >> 16) & 0xFF);
    COLORREF(r | (g << 8) | (b << 16))
}
