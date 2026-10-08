//! Color tokens — the single source of truth for every shell surface's
//! palette (spec §3.1). Tokens branch three ways —
//! `high_contrast` -> `light` -> `dark` — so the Phase 6 high-contrast mode
//! still overrides everything while the rest follows Windows' own
//! light/dark apps preference. The accent token is the *live* Windows
//! accent read from the DWM registry.
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

fn light() -> bool {
    state::light_theme()
}

/// Each surface/text token is split into a pure `*_for(hc, light)` core and
/// a thin live getter. The split keeps the three-way precedence
/// (`high_contrast` -> `light` -> `dark`) unit-testable without touching the
/// thread-local mirrors, exactly as `motion::effective_ms_with` does for
/// durations. High contrast is checked first so it keeps overriding both
/// themes, as it did before the light palette existed.
///
/// Light values follow the WinUI common-resource ramp; the dark values are
/// unchanged from the Phase 4 palette.
macro_rules! theme_token {
    ($live:ident, $pure:ident, $hc:expr, $light:expr, $dark:expr, $doc:expr) => {
        #[doc = $doc]
        pub(crate) fn $live() -> u32 {
            $pure(hc(), light())
        }

        #[doc = $doc]
        pub(crate) fn $pure(high_contrast: bool, light: bool) -> u32 {
            if high_contrast {
                rgb($hc)
            } else if light {
                rgb($light)
            } else {
                rgb($dark)
            }
        }
    };
}

theme_token!(
    surface_base,
    surface_base_for,
    0x0000_0000,
    0x00F3_F3F3,
    0x001E_1E1E,
    "Bar fill, under the backdrop material."
);

theme_token!(
    surface_raised,
    surface_raised_for,
    0x0000_0000,
    0x00FB_FBFB,
    0x0026_2626,
    "Raised flyout/card fill."
);

theme_token!(
    surface_overlay,
    surface_overlay_for,
    0x001A_1A1A,
    0x00FF_FFFF,
    0x002E_2E2E,
    "Menu / hovered-chip fill sitting above a card."
);

theme_token!(
    text,
    text_for,
    0x00FF_FFFF,
    0x001A_1A1A,
    0x00E8_E8E8,
    "Primary foreground."
);

theme_token!(
    text_muted,
    text_muted_for,
    0x00C8_C8C8,
    0x005F_5F5F,
    0x009A_9A9A,
    "Secondary foreground."
);

theme_token!(
    stroke,
    stroke_for,
    0x00FF_FFFF,
    0x00E5_E5E5,
    0x003A_3A3A,
    "Hairline borders/dividers."
);

/// Accent for active/selected/focus emphasis: the live Windows accent in
/// normal mode, yellow in high contrast (matching the Phase 6 palette).
pub(crate) fn accent() -> u32 {
    if hc() { rgb(0x00FF_FF00) } else { state::accent() }
}

/// Text drawn on top of an [`accent`] fill.
pub(crate) fn accent_text() -> u32 {
    contrasting_text(accent())
}

fn contrasting_text(background: u32) -> u32 {
    let linear = |channel: u32| {
        let c = channel as f64 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = 0.2126 * linear(background & 255)
        + 0.7152 * linear((background >> 8) & 255)
        + 0.0722 * linear((background >> 16) & 255);
    if luminance > 0.179 { rgb(0x0000_0000) } else { rgb(0x00FF_FFFF) }
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

/// Re-reads Windows' light/dark apps preference and updates the mirror in
/// `state`, so every token above follows the system theme. Safe to call at
/// startup and from the `WM_SETTINGCHANGE`/`ImmersiveColorSet` handler.
///
/// A `None` from the registry means the key has never been written (a fresh
/// account, or pre-1809 Windows); that maps to dark, which is the shell's
/// historical appearance.
pub(crate) fn refresh_theme() {
    state::set_light_theme(super::super::theme::apps_use_light_theme().unwrap_or(false));
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
    fn accent_foreground_stays_legible_on_light_and_dark_accents() {
        assert_eq!(contrasting_text(accent_fallback()), rgb(0));
        assert_eq!(contrasting_text(rgb(0xFFFFFF)), rgb(0));
        assert_eq!(contrasting_text(rgb(0x102060)), rgb(0xFFFFFF));
    }

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

    /// Relative luminance of a `COLORREF`, for the ordering assertions
    /// below — brighter surface in light theme, brighter text in dark.
    fn luma(colorref: u32) -> f64 {
        let b = (colorref >> 16) & 255;
        let g = (colorref >> 8) & 255;
        let r = colorref & 255;
        0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64
    }

    #[test]
    fn high_contrast_wins_over_light_theme() {
        // Precedence is high_contrast -> light -> dark, so the
        // high-contrast palette must be identical whichever theme is
        // underneath it.
        for token in [surface_base_for, surface_raised_for, surface_overlay_for, text_for, stroke_for]
        {
            assert_eq!(token(true, true), token(true, false));
        }
    }

    #[test]
    fn light_theme_surfaces_are_brighter_than_dark() {
        assert!(luma(surface_base_for(false, true)) > luma(surface_base_for(false, false)));
        assert!(luma(surface_raised_for(false, true)) > luma(surface_raised_for(false, false)));
        assert!(luma(surface_overlay_for(false, true)) > luma(surface_overlay_for(false, false)));
    }

    #[test]
    fn light_theme_text_is_darker_than_dark_theme_text() {
        assert!(luma(text_for(false, true)) < luma(text_for(false, false)));
        assert!(luma(text_muted_for(false, true)) < luma(text_muted_for(false, false)));
    }

    #[test]
    fn muted_text_is_lower_contrast_than_primary_in_both_themes() {
        // Muted sits between the primary text and its surface, in both
        // directions: darker than primary on dark, lighter on light.
        assert!(luma(text_muted_for(false, false)) < luma(text_for(false, false)));
        assert!(luma(text_muted_for(false, true)) > luma(text_for(false, true)));
    }

    #[test]
    fn light_theme_values_match_the_winui_ramp() {
        assert_eq!(surface_base_for(false, true), rgb(0x00F3_F3F3));
        assert_eq!(surface_raised_for(false, true), rgb(0x00FB_FBFB));
        assert_eq!(surface_overlay_for(false, true), rgb(0x00FF_FFFF));
        assert_eq!(text_for(false, true), rgb(0x001A_1A1A));
        assert_eq!(text_muted_for(false, true), rgb(0x005F_5F5F));
        assert_eq!(stroke_for(false, true), rgb(0x00E5_E5E5));
    }

    #[test]
    fn dark_theme_values_are_unchanged_from_before_the_light_pass() {
        assert_eq!(surface_base_for(false, false), rgb(0x001E_1E1E));
        assert_eq!(surface_raised_for(false, false), rgb(0x0026_2626));
        assert_eq!(surface_overlay_for(false, false), rgb(0x002E_2E2E));
        assert_eq!(text_for(false, false), rgb(0x00E8_E8E8));
        assert_eq!(text_muted_for(false, false), rgb(0x009A_9A9A));
        assert_eq!(stroke_for(false, false), rgb(0x003A_3A3A));
    }
}
