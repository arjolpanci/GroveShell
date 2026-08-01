//! Shared color palette and owner-drawn widgets for the settings window,
//! matching `apps/ui`'s established literal colors (bar/calendar/quick
//! settings) so this window doesn't look like a different app.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, Ellipse, GetStockObject, RoundRect, SelectObject, HDC,
    NULL_PEN,
};

pub(crate) const TEXT: COLORREF = COLORREF(0x00E0E0E0);
pub(crate) const TEXT_MUTED: COLORREF = COLORREF(0x00A0A0A0);
pub(crate) const BG_WINDOW: u32 = 0x00202020;
pub(crate) const BG_PANEL: COLORREF = COLORREF(0x00262626);
pub(crate) const BG_NAV: COLORREF = COLORREF(0x00303030);
pub(crate) const ACCENT: COLORREF = COLORREF(0x00FFA860);
pub(crate) const NAV_WIDTH: i32 = 180;

/// Fills `rect` with `color`, no border — same idiom as
/// `apps/ui/src/imp/quick_settings.rs`'s `fill_round_rect`.
pub(crate) unsafe fn fill_round_rect(hdc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius * 2, radius * 2);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

/// A toggle switch: a pill-shaped track plus a circular thumb, filled with
/// the accent color when `on`.
pub(crate) unsafe fn draw_toggle(hdc: HDC, rect: RECT, on: bool) {
    let track_color = if on { ACCENT } else { COLORREF(0x00505050) };
    fill_round_rect(hdc, rect, (rect.bottom - rect.top) / 2, track_color);
    let thumb_d = rect.bottom - rect.top - 4;
    let thumb_x = if on { rect.right - thumb_d - 2 } else { rect.left + 2 };
    let thumb_rect = RECT { left: thumb_x, top: rect.top + 2, right: thumb_x + thumb_d, bottom: rect.bottom - 2 };
    let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, thumb_rect.left, thumb_rect.top, thumb_rect.right, thumb_rect.bottom);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

pub(crate) fn hit_toggle(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// A slider: a track plus an accent-filled portion up to `value` (clamped
/// into `[min, max]`) and a round thumb — visually the same shape as
/// `apps/ui/src/imp/quick_settings.rs`'s volume slider.
pub(crate) unsafe fn draw_slider(hdc: HDC, rect: RECT, value: f32, min: f32, max: f32) {
    let track_h = (rect.bottom - rect.top).min(6);
    let track = RECT {
        left: rect.left,
        top: (rect.top + rect.bottom) / 2 - track_h / 2,
        right: rect.right,
        bottom: (rect.top + rect.bottom) / 2 + track_h / 2,
    };
    fill_round_rect(hdc, track, track_h / 2, COLORREF(0x00383838));
    let fraction = ((value - min) / (max - min).max(f32::EPSILON)).clamp(0.0, 1.0);
    let fill_right = track.left + ((track.right - track.left) as f32 * fraction).round() as i32;
    if fill_right > track.left {
        let fill = RECT { left: track.left, right: fill_right.max(track.left + track_h), ..track };
        fill_round_rect(hdc, fill, track_h / 2, ACCENT);
    }
    let thumb_r = 7;
    let thumb_cy = (track.top + track.bottom) / 2;
    let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, fill_right - thumb_r, thumb_cy - thumb_r, fill_right + thumb_r, thumb_cy + thumb_r);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

pub(crate) fn value_from_slider_x(rect: RECT, x: i32, min: f32, max: f32) -> f32 {
    let width = (rect.right - rect.left).max(1) as f32;
    let fraction = ((x - rect.left) as f32 / width).clamp(0.0, 1.0);
    min + fraction * (max - min)
}

/// A three-(or-fewer)-way segmented control: equal-width pill segments,
/// the selected one filled with the accent color.
pub(crate) unsafe fn draw_segmented(hdc: HDC, rect: RECT, options: &[&str], selected: usize) {
    use super::util_text::draw_centered_text;
    let n = options.len().max(1) as i32;
    let seg_w = (rect.right - rect.left) / n;
    let radius = (rect.bottom - rect.top) / 2;
    fill_round_rect(hdc, rect, radius, COLORREF(0x00383838));
    for (i, label) in options.iter().enumerate() {
        let seg_rect = RECT {
            left: rect.left + i as i32 * seg_w,
            top: rect.top,
            right: rect.left + (i as i32 + 1) * seg_w,
            bottom: rect.bottom,
        };
        if i == selected {
            fill_round_rect(hdc, seg_rect, radius, ACCENT);
        }
        draw_centered_text(hdc, seg_rect, label, if i == selected { COLORREF(0x00202020) } else { TEXT });
    }
}

pub(crate) fn segmented_hit(rect: RECT, options: &[&str], x: i32, y: i32) -> Option<usize> {
    if y < rect.top || y >= rect.bottom || x < rect.left || x >= rect.right {
        return None;
    }
    let n = options.len().max(1) as i32;
    let seg_w = (rect.right - rect.left) / n;
    Some((((x - rect.left) / seg_w).clamp(0, n - 1)) as usize)
}
