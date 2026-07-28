//! Small painting/animation helpers shared across bar, calendar, quick
//! settings, and overview rendering.

use windows::core::w;
use windows::Win32::Graphics::Gdi::{
    Arc, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, GetStockObject,
    Polygon, SelectObject, DRAW_TEXT_FORMAT, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, HDC, HFONT, HOLLOW_BRUSH, NULL_PEN, OUT_DEFAULT_PRECIS,
    PS_SOLID,
};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
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

/// A small filled speaker glyph — body plus a widening cone, in `rect`.
/// Sound-wave arcs to the right when `muted` is false; a short diagonal
/// slash through the cone instead when muted. `rect` should be roughly
/// square; the glyph is derived entirely from its fractional corners so
/// it scales cleanly with whatever DPI the caller already applied.
///
/// SAFETY: `hdc` must be a valid device context currently being painted
/// into.
pub(crate) unsafe fn draw_volume_glyph(hdc: HDC, rect: RECT, color: COLORREF, muted: bool) {
    let w = (rect.right - rect.left) as f64;
    let h = (rect.bottom - rect.top) as f64;
    let x = rect.left as f64;
    let y = rect.top as f64;
    let pt = |fx: f64, fy: f64| POINT { x: (x + w * fx).round() as i32, y: (y + h * fy).round() as i32 };

    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, pen);

    let speaker = [
        pt(0.0, 0.35),
        pt(0.35, 0.35),
        pt(0.65, 0.05),
        pt(0.65, 0.95),
        pt(0.35, 0.65),
        pt(0.0, 0.65),
    ];
    let _ = Polygon(hdc, &speaker);

    if muted {
        SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
        let a = pt(0.68, 0.28);
        let b = pt(0.98, 0.72);
        let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, a.x, a.y, None);
        let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, b.x, b.y);
    } else {
        SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
        let inner = RECT {
            left: pt(0.55, 0.20).x,
            top: pt(0.55, 0.20).y,
            right: pt(0.95, 0.80).x,
            bottom: pt(0.95, 0.80).y,
        };
        let _ = Arc(hdc, inner.left, inner.top, inner.right, inner.bottom, inner.right, inner.top, inner.right, inner.bottom);
        let outer = RECT {
            left: pt(0.55, 0.02).x,
            top: pt(0.55, 0.02).y,
            right: pt(1.05, 0.98).x,
            bottom: pt(1.05, 0.98).y,
        };
        let _ = Arc(hdc, outer.left, outer.top, outer.right, outer.bottom, outer.right, outer.top, outer.right, outer.bottom);
    }

    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(pen);
}

/// A small Wi-Fi glyph — three nested signal arcs plus a base dot, or
/// (when `enabled` is `false`) just the dot with a diagonal slash
/// through the whole glyph.
///
/// SAFETY: `hdc` must be a valid device context currently being painted
/// into.
pub(crate) unsafe fn draw_wifi_glyph(hdc: HDC, rect: RECT, color: COLORREF, enabled: bool) {
    let w = (rect.right - rect.left) as f64;
    let h = (rect.bottom - rect.top) as f64;
    let x = rect.left as f64;
    let y = rect.top as f64;
    let pt = |fx: f64, fy: f64| POINT { x: (x + w * fx).round() as i32, y: (y + h * fy).round() as i32 };

    let pen = CreatePen(PS_SOLID, 2, color);
    let previous_pen = SelectObject(hdc, pen);
    SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));

    let dot_r = (w * 0.08).max(1.0) as i32;
    let dot_c = pt(0.5, 0.85);
    let _ = Ellipse(hdc, dot_c.x - dot_r, dot_c.y - dot_r, dot_c.x + dot_r, dot_c.y + dot_r);

    if enabled {
        for band in [0.35, 0.6, 0.85] {
            let top = pt(0.5 - band, 0.85 - band * 1.7);
            let bottom = pt(0.5 + band, 0.95);
            let _ = Arc(hdc, top.x, top.y, bottom.x, bottom.y, bottom.x, dot_c.y, top.x, dot_c.y);
        }
    } else {
        let brush = CreateSolidBrush(color);
        let dot_brush = SelectObject(hdc, brush);
        let _ = Ellipse(hdc, dot_c.x - dot_r, dot_c.y - dot_r, dot_c.x + dot_r, dot_c.y + dot_r);
        SelectObject(hdc, dot_brush);
        let _ = DeleteObject(brush);
        let a = pt(0.05, 0.1);
        let b = pt(0.95, 0.9);
        let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, a.x, a.y, None);
        let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, b.x, b.y);
    }

    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(pen);
}

/// A small battery glyph — rounded body outline, a small nub on the
/// right, and an interior fill proportional to `percent`. `color` is
/// the outline/fill color; callers pick it (e.g. red under a low-charge
/// threshold) since this glyph has no opinion on thresholds itself.
///
/// SAFETY: `hdc` must be a valid device context currently being painted
/// into.
pub(crate) unsafe fn draw_battery_glyph(hdc: HDC, rect: RECT, color: COLORREF, percent: u8) {
    let w = (rect.right - rect.left) as f64;
    let h = (rect.bottom - rect.top) as f64;
    let x = rect.left as f64;
    let y = rect.top as f64;
    let pt = |fx: f64, fy: f64| POINT { x: (x + w * fx).round() as i32, y: (y + h * fy).round() as i32 };

    let pen = CreatePen(PS_SOLID, 1, color);
    let previous_pen = SelectObject(hdc, pen);
    SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));

    let body = RECT {
        left: pt(0.0, 0.15).x,
        top: pt(0.0, 0.15).y,
        right: pt(0.82, 0.85).x,
        bottom: pt(0.82, 0.85).y,
    };
    let radius = ((body.bottom - body.top) as f64 * 0.3) as i32;
    let _ = windows::Win32::Graphics::Gdi::RoundRect(
        hdc, body.left, body.top, body.right, body.bottom, radius, radius,
    );
    let nub = pt(0.82, 0.35);
    let nub_end = pt(0.98, 0.65);
    let _ = windows::Win32::Graphics::Gdi::Rectangle(hdc, nub.x, nub.y, nub_end.x, nub_end.y);

    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(pen);

    let fill_frac = (percent as f64 / 100.0).clamp(0.0, 1.0);
    if fill_frac > 0.02 {
        let pad = 2;
        let inner_left = body.left + pad;
        let inner_right = body.right - pad;
        let fill_right = inner_left + ((inner_right - inner_left) as f64 * fill_frac).round() as i32;
        let brush = CreateSolidBrush(color);
        let previous_brush = SelectObject(hdc, brush);
        SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = windows::Win32::Graphics::Gdi::Rectangle(
            hdc, inner_left, body.top + pad, fill_right, body.bottom - pad,
        );
        SelectObject(hdc, previous_brush);
        let _ = DeleteObject(brush);
    }
}
