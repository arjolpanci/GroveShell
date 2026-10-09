//! Windows 11 backdrop material for the shell's own windows (spec §3.1):
//! Mica behind the bar, Acrylic behind the flyouts, DWM-drawn round corners
//! and shadow on the flyouts, and the immersive dark-mode flag that tints
//! all of it to match the active theme.

use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DWMWCP_ROUND, DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE,
};

use crate::runtime as state;

/// Which kind of shell surface a window is, for material purposes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    /// The top bar: permanent chrome, full monitor width, flush against
    /// the screen's top edge.
    Bar,
    /// A transient pop-up anchored under the bar (Quick Settings, calendar).
    Flyout,
    /// An ordinary application window: Mica, rounded corners, and a
    /// title bar tinted to match the system theme. The settings window.
    Window,
}

/// Mica for the bar, Acrylic for flyouts — matching what Windows itself
/// puts behind its own equivalents.
pub fn backdrop_for(surface: Surface) -> DWM_SYSTEMBACKDROP_TYPE {
    match surface {
        Surface::Bar | Surface::Window => DWMSBT_MAINWINDOW,
        Surface::Flyout => DWMSBT_TRANSIENTWINDOW,
    }
}

/// Everything rounds except the bar, which is full-width and flush to
/// the top edge: rounding it would notch the screen corners.
pub fn corner_for(surface: Surface) -> DWM_WINDOW_CORNER_PREFERENCE {
    match surface {
        Surface::Bar => DWMWCP_DONOTROUND,
        Surface::Flyout | Surface::Window => DWMWCP_ROUND,
    }
}

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` wants "is dark", so it is the inverse
/// of the light-theme flag.
pub fn dark_mode_flag(light: bool) -> BOOL {
    BOOL::from(!light)
}

/// Applies the backdrop, corner preference, and dark-mode tint to `hwnd`.
///
/// Returns whether the backdrop was accepted. `DWMWA_SYSTEMBACKDROP_TYPE`
/// needs Windows 11 22621+ and `DWMWA_WINDOW_CORNER_PREFERENCE` needs
/// 22000+; on older builds `DwmSetWindowAttribute` simply fails, and that
/// failure *is* the feature detection — no version probe, no registry read
/// (spec §3.3). A `false` return tells the caller to fall back to the
/// legacy blur-behind path and keep painting its own rounded corners.
pub fn apply(hwnd: HWND, surface: Surface) -> bool {
    let dark = dark_mode_flag(state::light_theme());
    let backdrop = backdrop_for(surface);
    let corner = corner_for(surface);

    // SAFETY: each call passes a pointer to a live local of exactly the
    // size given, which is what `DwmSetWindowAttribute` reads. `hwnd` is a
    // window this process created. Failures are expected on older Windows
    // and are handled, not propagated.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const std::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const DWM_WINDOW_CORNER_PREFERENCE as *const std::ffi::c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const DWM_SYSTEMBACKDROP_TYPE as *const std::ffi::c_void,
            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_gets_mica_and_flyouts_get_acrylic() {
        // Mica (DWMSBT_MAINWINDOW = 2) is for long-lived chrome tied to
        // the desktop; Acrylic (DWMSBT_TRANSIENTWINDOW = 3) is what
        // Windows 11 uses behind its own transient flyouts.
        assert_eq!(backdrop_for(Surface::Bar).0, 2);
        assert_eq!(backdrop_for(Surface::Flyout).0, 3);
    }

    #[test]
    fn an_app_window_gets_mica_and_rounded_corners() {
        // The settings window is ordinary desktop chrome: the same Mica
        // the bar gets, but rounded like every other app window.
        assert_eq!(backdrop_for(Surface::Window).0, 2);
        assert_eq!(corner_for(Surface::Window).0, 2);
    }

    #[test]
    fn the_bar_alone_refuses_rounded_corners() {
        // The bar spans the full monitor width flush against the top
        // edge, so rounding it would cut visible notches out of the
        // screen corners. DWMWCP_DONOTROUND = 1, DWMWCP_ROUND = 2.
        assert_eq!(corner_for(Surface::Bar).0, 1);
        assert_eq!(corner_for(Surface::Flyout).0, 2);
    }

    #[test]
    fn dark_mode_flag_is_the_inverse_of_the_light_theme() {
        assert_eq!(dark_mode_flag(true).0, 0);
        assert_eq!(dark_mode_flag(false).0, 1);
    }
}
