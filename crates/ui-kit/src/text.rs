//! Text drawing on the GDI backend: the UI font at a logical size, and a
//! `DrawTextW` wrapper. Both are used by `canvas::GdiCanvas` and by the
//! shell's remaining direct-GDI paint paths.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DrawTextW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DRAW_TEXT_FORMAT, HDC, HFONT, OUT_DEFAULT_PRECIS,
};

use crate::runtime::scaled;

/// The UI font at an explicit logical-pixel size (caller owns the handle
/// and must `DeleteObject` it after deselecting).
///
/// Uses Windows 11's `Segoe UI Variable Text`, falling back to `Segoe UI`
/// on older releases — see `design::typography`, which probes the
/// installed face once.
pub fn ui_font(size_px: i32, dpi: u32) -> HFONT {
    let face = windows::core::HSTRING::from(crate::design::typography::resolved_ui_face());
    // SAFETY: plain object creation; no aliasing or lifetime
    // preconditions.
    unsafe {
        CreateFontW(
            -scaled(size_px, dpi),
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
            windows::core::PCWSTR(face.as_ptr()),
        )
    }
}

/// SAFETY: `hdc` must be a valid device context obtained from
/// `BeginPaint` on the window currently handling `WM_PAINT`.
pub unsafe fn draw_text_in(hdc: HDC, rect: RECT, text: &str, format: DRAW_TEXT_FORMAT) {
    // An empty Vec has a dangling non-null pointer. Some DrawText paths
    // inspect the first character even with a zero character count.
    if text.is_empty() { return; }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, format | windows::Win32::Graphics::Gdi::DT_NOPREFIX);
}
