//! Color tokens — the single source of truth for every shell surface's
//! palette (spec §3.1). Tokens branch on `state::high_contrast()` so the
//! Phase 6 high-contrast mode restyles everything, and the accent token is
//! the *live* Windows accent read from the DWM registry.
//!
//! Colors are written here in conventional `#RRGGBB` via [`rgb`], which
//! byte-swaps to the Win32 `COLORREF` layout (`0x00BBGGRR`) every drawing
//! call expects. Keeping one conversion site means the rest of the code
//! never hand-swaps bytes.

use super::super::state;

/// Convert a `0x00RRGGBB` web-order value to a Win32 `COLORREF`
/// (`0x00BBGGRR`) by swapping the red and blue bytes.
pub(crate) fn rgb(hex: u32) -> u32 {
    let r = (hex >> 16) & 0xFF;
    let g = (hex >> 8) & 0xFF;
    let b = hex & 0xFF;
    (b << 16) | (g << 8) | r
}

/// The DWM `AccentColor` registry value is a `DWORD` in `0xAABBGGRR` (ABGR)
/// order — already blue/green/red like a `COLORREF`, just with an alpha
/// byte on top. So dropping the alpha yields a `COLORREF` directly, no
/// swap. (`ColorizationColor`, used only as a fallback, is ARGB instead and
/// is swapped through [`rgb`] at its read site.)
pub(crate) fn accent_from_dword(abgr: u32) -> u32 {
    abgr & 0x00FF_FFFF
}

/// The fallback accent used when the registry can't be read: a modern
/// Win11-ish blue (`#4CC2FF`).
pub(crate) fn accent_fallback() -> u32 {
    rgb(0x004C_C2FF)
}

fn hc() -> bool {
    state::high_contrast()
}

/// Bar fill (under the Mica backdrop). `#1E1E1E` / black in high contrast.
pub(crate) fn surface_base() -> u32 {
    if hc() { rgb(0x0000_0000) } else { rgb(0x001E_1E1E) }
}

/// Raised flyout/card fill. `#262626` / black in high contrast.
pub(crate) fn surface_raised() -> u32 {
    if hc() { rgb(0x0000_0000) } else { rgb(0x0026_2626) }
}

/// Menu / hovered-chip fill sitting above a card. `#2E2E2E` in high contrast a hair off black.
pub(crate) fn surface_overlay() -> u32 {
    if hc() { rgb(0x001A_1A1A) } else { rgb(0x002E_2E2E) }
}

/// Primary foreground. `#E8E8E8` / white in high contrast.
pub(crate) fn text() -> u32 {
    if hc() { rgb(0x00FF_FFFF) } else { rgb(0x00E8_E8E8) }
}

/// Secondary foreground. `#9A9A9A` / bright grey in high contrast.
pub(crate) fn text_muted() -> u32 {
    if hc() { rgb(0x00C8_C8C8) } else { rgb(0x009A_9A9A) }
}

/// Hairline borders/dividers. `#3A3A3A` / white in high contrast.
pub(crate) fn stroke() -> u32 {
    if hc() { rgb(0x00FF_FFFF) } else { rgb(0x003A_3A3A) }
}

/// Accent for active/selected/focus emphasis: the live Windows accent in
/// normal mode, yellow in high contrast (matching the Phase 6 palette).
pub(crate) fn accent() -> u32 {
    if hc() { rgb(0x00FF_FF00) } else { state::accent() }
}

/// Text drawn on top of an [`accent`] fill.
pub(crate) fn accent_text() -> u32 {
    if hc() { rgb(0x0000_0000) } else { rgb(0x00FF_FFFF) }
}

/// Re-reads the Windows accent color from the DWM registry key and updates
/// the cross-thread mirror in `state`. Tries `AccentColor` (ABGR) first,
/// then `ColorizationColor` (ARGB), then falls back to [`accent_fallback`].
/// Safe to call from the UI thread at startup and on a colorization-change
/// broadcast.
pub(crate) fn refresh_accent() {
    let value = read_dwm_dword("AccentColor")
        .map(accent_from_dword)
        .or_else(|| read_dwm_dword("ColorizationColor").map(|argb| rgb(argb & 0x00FF_FFFF)))
        .unwrap_or_else(accent_fallback);
    state::set_accent(value);
}

fn read_dwm_dword(value_name: &str) -> Option<u32> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let subkey = HSTRING::from(r"Software\Microsoft\Windows\DWM");
    let name = HSTRING::from(value_name);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `subkey`/`name` are live wide strings for the call; `data`
    // and `size` are locals sized exactly for a REG_DWORD, matching what
    // `RegGetValueW` writes. No handle is opened (HKCU is a predefined key).
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_swaps_red_and_blue_to_colorref() {
        // #1E2A3C: R=1E G=2A B=3C  ->  COLORREF 0x003C2A1E
        assert_eq!(rgb(0x001E_2A3C), 0x003C_2A1E);
    }

    #[test]
    fn rgb_is_its_own_inverse() {
        assert_eq!(rgb(rgb(0x0012_3456)), 0x0012_3456);
    }

    #[test]
    fn accent_from_dword_drops_alpha_and_keeps_bgr() {
        // DWM AccentColor 0xAABBGGRR -> COLORREF 0x00BBGGRR.
        assert_eq!(accent_from_dword(0xFF3C_2A1E), 0x003C_2A1E);
        assert_eq!(accent_from_dword(0x0000_0000), 0x0000_0000);
    }
}
