//! Text drawing, factored out of `theme.rs` so `draw_segmented` can use it
//! without a circular import — mirrors `apps/ui/src/imp/util.rs`'s
//! `draw_text_in`/`bar_font` pair.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, DrawTextW, SelectObject, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

fn font(point_size: i32, weight: i32) -> windows::Win32::Graphics::Gdi::HFONT {
    // SAFETY: plain object creation, no aliasing or lifetime preconditions.
    unsafe {
        CreateFontW(
            -point_size, 0, 0, 0, weight, 0, 0, 0,
            DEFAULT_CHARSET.0.into(),
            OUT_DEFAULT_PRECIS.0.into(),
            CLIP_DEFAULT_PRECIS.0.into(),
            CLEARTYPE_QUALITY.0.into(),
            DEFAULT_PITCH.0.into(),
            w!("Segoe UI"),
        )
    }
}

pub(crate) fn ui_font() -> windows::Win32::Graphics::Gdi::HFONT {
    font(14, 400)
}

/// The page-title band's font (see `theme::HEADER_HEIGHT`) — bold and
/// noticeably larger than body text so the nav selection's page name
/// reads as a real heading, not just another row.
pub(crate) fn title_font() -> windows::Win32::Graphics::Gdi::HFONT {
    font(20, 600)
}

pub(crate) unsafe fn draw_centered_text(hdc: HDC, rect: RECT, text: &str, color: COLORREF) {
    draw_text_in(hdc, rect, text, color, ui_font(), DT_CENTER | DT_SINGLELINE | DT_VCENTER);
}

/// Left-aligned, vertically centered — used for the page title and
/// anywhere else text needs to read start-to-end rather than centered
/// in its whole row (Windows Settings' own convention for headings and
/// group labels).
pub(crate) unsafe fn draw_left_text(hdc: HDC, rect: RECT, text: &str, color: COLORREF) {
    draw_text_in(hdc, rect, text, color, ui_font(), DT_LEFT | DT_SINGLELINE | DT_VCENTER);
}

/// Left-aligned using `title_font()` instead of the body font — the
/// page heading specifically.
pub(crate) unsafe fn draw_title_text(hdc: HDC, rect: RECT, text: &str, color: COLORREF) {
    draw_text_in(hdc, rect, text, color, title_font(), DT_LEFT | DT_SINGLELINE | DT_VCENTER);
}

unsafe fn draw_text_in(
    hdc: HDC,
    rect: RECT,
    text: &str,
    color: COLORREF,
    font: windows::Win32::Graphics::Gdi::HFONT,
    format: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let previous = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, color);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, format);
    SelectObject(hdc, previous);
    let _ = DeleteObject(font);
}
