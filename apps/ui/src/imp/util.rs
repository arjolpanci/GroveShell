//! Small painting/animation helpers shared across bar, calendar, quick
//! settings, and overview rendering.

use windows::core::w;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DrawTextW, DRAW_TEXT_FORMAT, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, HDC, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::Foundation::RECT;

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
