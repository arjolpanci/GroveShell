//! Text drawing, factored out of `theme.rs` so `draw_segmented` can use it
//! without a circular import — mirrors `apps/ui/src/imp/util.rs`'s
//! `draw_text_in`/`bar_font` pair.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, DrawTextW, SelectObject, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

pub(crate) fn ui_font() -> windows::Win32::Graphics::Gdi::HFONT {
    // SAFETY: plain object creation, no aliasing or lifetime preconditions.
    unsafe {
        CreateFontW(
            -14, 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET.0.into(),
            OUT_DEFAULT_PRECIS.0.into(),
            CLIP_DEFAULT_PRECIS.0.into(),
            CLEARTYPE_QUALITY.0.into(),
            DEFAULT_PITCH.0.into(),
            w!("Segoe UI"),
        )
    }
}

pub(crate) unsafe fn draw_centered_text(hdc: HDC, rect: RECT, text: &str, color: COLORREF) {
    let font = ui_font();
    let previous = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, color);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, previous);
    let _ = DeleteObject(font);
}
