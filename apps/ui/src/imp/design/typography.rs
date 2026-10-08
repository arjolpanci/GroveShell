//! Type tokens (spec §5.1): the WinUI size ramp and the Windows 11 UI
//! typeface, with the pre-Windows-11 fallback.
//!
//! Sizes are **logical pixels at the 96-DPI reference**, not points —
//! `util::bar_font` passes them to `CreateFontW` as a negative character
//! height in logical device units, and callers scale them per-monitor with
//! the usual `state::scaled` helpers.

use std::cell::Cell;

/// Small, secondary text: workspace labels, tray captions.
pub(crate) const CAPTION_PX: i32 = 12;

/// Default UI text: bar labels, flyout rows. Windows 11's own body size.
pub(crate) const BODY_PX: i32 = 14;

/// Flyout and section headers.
pub(crate) const SUBTITLE_PX: i32 = 20;

/// The Windows 11 UI typeface. Ships only on Windows 11, hence the
/// fallback to the face every earlier release has.
const VARIABLE_FACE: &str = "Segoe UI Variable Text";
const FALLBACK_FACE: &str = "Segoe UI";

/// The icon typeface, for [`super::super::icons`]'s glyph path.
pub(crate) const ICON_FACE: &str = "Segoe Fluent Icons";

/// Which UI face to request, given whether the variable font is installed.
pub(crate) fn ui_face(variable_available: bool) -> &'static str {
    if variable_available {
        VARIABLE_FACE
    } else {
        FALLBACK_FACE
    }
}

thread_local! {
    /// Font availability, probed once and cached — enumerating fonts on
    /// every paint would be absurd, and the installed set cannot change
    /// without a restart of the shell in practice. `None` until first use.
    static VARIABLE_AVAILABLE: Cell<Option<bool>> = const { Cell::new(None) };
    static ICONS_AVAILABLE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The UI face to use on this machine, probing once on first call.
pub(crate) fn resolved_ui_face() -> &'static str {
    ui_face(VARIABLE_AVAILABLE.with(|c| match c.get() {
        Some(known) => known,
        None => {
            let found = font_installed(VARIABLE_FACE);
            c.set(Some(found));
            found
        }
    }))
}

/// Whether the Fluent icon font is installed, probed once. When it is not,
/// callers fall back to the bundled PNG icons.
pub(crate) fn icon_font_available() -> bool {
    ICONS_AVAILABLE.with(|c| match c.get() {
        Some(known) => known,
        None => {
            let found = font_installed(ICON_FACE);
            c.set(Some(found));
            found
        }
    })
}

/// Asks GDI for a face by name and checks what it actually got back.
///
/// `CreateFontW` never fails for a missing face — it silently substitutes
/// the closest match — so the only reliable test is to select the font and
/// read the resulting face name back with `GetTextFaceW`.
fn font_installed(face: &str) -> bool {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, DeleteObject, GetDC, GetTextFaceW, ReleaseDC, SelectObject,
        CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DEFAULT_QUALITY, OUT_DEFAULT_PRECIS,
    };

    let wanted = HSTRING::from(face);
    // SAFETY: a screen DC from `GetDC(None)` is valid until released
    // below; the font is selected, read back, deselected, and deleted on
    // every path, so neither object leaks.
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return false;
        }
        let font = CreateFontW(
            -12,
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
            DEFAULT_QUALITY.0.into(),
            DEFAULT_PITCH.0.into(),
            PCWSTR(wanted.as_ptr()),
        );
        let previous = SelectObject(hdc, font);
        let mut buffer = [0u16; 64];
        let len = GetTextFaceW(hdc, Some(&mut buffer));
        SelectObject(hdc, previous);
        let _ = DeleteObject(font);
        ReleaseDC(None, hdc);

        if len <= 0 {
            return false;
        }
        let actual = String::from_utf16_lossy(&buffer[..(len as usize).saturating_sub(1)]);
        actual.eq_ignore_ascii_case(face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_increases_from_caption_to_subtitle() {
        assert!(CAPTION_PX < BODY_PX);
        assert!(BODY_PX < SUBTITLE_PX);
    }

    #[test]
    fn body_matches_the_windows_ui_body_size() {
        // Windows 11 sets body text at 14epx; the bar used to hardcode 12,
        // which is what made it read as subtly not-native.
        assert_eq!(BODY_PX, 14);
    }

    #[test]
    fn ui_face_is_the_variable_font_when_present_and_segoe_ui_otherwise() {
        assert_eq!(ui_face(true), "Segoe UI Variable Text");
        assert_eq!(ui_face(false), "Segoe UI");
    }
}
