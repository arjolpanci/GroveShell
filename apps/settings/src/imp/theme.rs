//! Shared color palette and owner-drawn widgets for the settings window,
//! matching `apps/ui`'s established literal colors (bar/calendar/quick
//! settings) so this window doesn't look like a different app.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, Ellipse, GetStockObject, HOLLOW_BRUSH, PS_SOLID,
    RoundRect, SelectObject, HDC, NULL_PEN,
};

pub(crate) const TEXT: COLORREF = COLORREF(0x00E0E0E0);
pub(crate) const TEXT_MUTED: COLORREF = COLORREF(0x00A0A0A0);
pub(crate) const BG_WINDOW: u32 = 0x00202020;
pub(crate) const BG_PANEL: COLORREF = COLORREF(0x00262626);
pub(crate) const BG_NAV: COLORREF = COLORREF(0x00303030);
pub(crate) const ACCENT: COLORREF = COLORREF(0x00FFA860);
/// Hairline dividers/track backgrounds — same literal already used
/// throughout this file for slider/segmented-control tracks, promoted
/// to a named token now that the page header/card layout uses it too.
pub(crate) const DIVIDER: COLORREF = COLORREF(0x00383838);
pub(crate) const NAV_WIDTH: i32 = 180;
/// Height of the page-title band every page's content sits below (see
/// `window.rs`'s `header_rect`/`body_rect`).
pub(crate) const HEADER_HEIGHT: i32 = 60;
/// Gap between the content area's edges (nav divider, window edges,
/// header divider) and the grouping card every page paints its rows
/// onto — the Windows 11 Settings "grouped card" look, adapted to this
/// window's owner-drawn GDI rendering.
pub(crate) const PAGE_MARGIN: i32 = 20;
pub(crate) const CARD_RADIUS: i32 = 12;

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

/// A faint multi-ring shadow just outside `rect`'s edge, darkest near the
/// card and fading out — same technique `apps/ui`'s overview module uses
/// for its workspace cards, ported here (a separate binary crate can't
/// reach that `pub(crate)` function directly) so the settings window's
/// grouping cards read with the same subtle depth as the rest of the
/// shell's chrome instead of looking flat.
unsafe fn draw_shadow(hdc: HDC, rect: RECT, radius: i32, layers: i32) {
    let hollow_brush = GetStockObject(HOLLOW_BRUSH);
    let previous_brush = SelectObject(hdc, hollow_brush);
    for i in 0..layers {
        let spread = layers - i;
        let t = (i + 1) as f64 / layers as f64;
        let channel = (0x40 as f64 + (0x1C as f64 - 0x40 as f64) * t).round() as u32;
        let pen = CreatePen(PS_SOLID, 2, COLORREF(channel | (channel << 8) | (channel << 16)));
        let previous_pen = SelectObject(hdc, pen);
        let _ = RoundRect(
            hdc,
            rect.left - spread,
            rect.top - spread + 2,
            rect.right + spread,
            rect.bottom + spread + 2,
            (radius + spread) * 2,
            (radius + spread) * 2,
        );
        SelectObject(hdc, previous_pen);
        let _ = DeleteObject(pen);
    }
    SelectObject(hdc, previous_brush);
}

/// Draws one of the Windows-11-style "grouped card" backgrounds every
/// page's rows sit on: a soft shadow plus a `BG_PANEL`-filled rounded
/// rect at `rect`. Callers keep using their existing row math relative
/// to `rect`'s own top-left — this only paints behind it, it doesn't
/// change any layout.
pub(crate) unsafe fn draw_card(hdc: HDC, rect: RECT) {
    draw_shadow(hdc, rect, CARD_RADIUS, 5);
    fill_round_rect(hdc, rect, CARD_RADIUS, BG_PANEL);
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
