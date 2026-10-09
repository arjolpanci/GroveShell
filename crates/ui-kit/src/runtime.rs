//! Process-wide values the design tokens read: the live Windows accent,
//! the light/dark and high-contrast flags, the animation config, and DPI
//! scaling.
//!
//! Thread-local `Cell`s rather than fields on the host app's state, and
//! that shape is not an accident. These values are read from deep inside
//! paint and animation code that already holds `apps/ui`'s `STATE`
//! `RefCell` borrow for the whole duration of its work; a second, nested
//! `STATE.with(|s| s.borrow())` panics with "RefCell already mutably
//! borrowed". Both known cases were confirmed live, as process aborts:
//! the animation config on the first `WM_TIMER` tick after opening
//! Activities, and the dock metrics the moment the mouse moved over an
//! open overview. A separate `Cell` has no aliasing relationship with
//! that `RefCell`, so a token accessor can read it under any borrow.
//!
//! Whoever owns the config pushes each value here when it is loaded or
//! reloaded; nothing in the kit reads a config file itself.

use std::cell::Cell;

thread_local! {
    /// Defaults to the shell's fallback accent (`#4CC2FF`) until
    /// `design::color::refresh_accent` has read the real one, matching
    /// what `apps/ui`'s mirror has always started at.
    static ACCENT: Cell<u32> = const { Cell::new(0x00FF_C24C) };
    static LIGHT_THEME: Cell<bool> = const { Cell::new(false) };
    static HIGH_CONTRAST: Cell<bool> = const { Cell::new(false) };
    static ANIMATION_SCALE: Cell<f32> = const { Cell::new(1.0) };
    static REDUCED_MOTION: Cell<bool> = const { Cell::new(false) };
}

/// Scales a logical (96-DPI reference) pixel value to `dpi`, rounding to
/// nearest.
pub fn scaled(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// The live Windows accent color as a `COLORREF`-shaped `u32`.
pub fn accent() -> u32 {
    ACCENT.with(|c| c.get())
}

pub fn set_accent(value: u32) {
    ACCENT.with(|c| c.set(value));
}

/// Whether Windows is in its light apps theme.
pub fn light_theme() -> bool {
    LIGHT_THEME.with(|c| c.get())
}

pub fn set_light_theme(light: bool) {
    LIGHT_THEME.with(|c| c.set(light));
}

/// Whether the high-contrast palette is in force; it overrides both themes.
pub fn high_contrast() -> bool {
    HIGH_CONTRAST.with(|c| c.get())
}

pub fn set_high_contrast(on: bool) {
    HIGH_CONTRAST.with(|c| c.set(on));
}

/// `(animation_scale, reduced_motion)` — the pair `design::motion` needs.
pub fn animation_config() -> (f32, bool) {
    (ANIMATION_SCALE.with(|c| c.get()), REDUCED_MOTION.with(|c| c.get()))
}

pub fn set_animation_config(scale: f32, reduced: bool) {
    ANIMATION_SCALE.with(|c| c.set(scale));
    REDUCED_MOTION.with(|c| c.set(reduced));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_rounds_to_nearest_at_fractional_dpi() {
        assert_eq!(scaled(24, 96), 24);
        assert_eq!(scaled(24, 144), 36);
        assert_eq!(scaled(15, 120), 19);
    }

    #[test]
    fn accent_round_trips_through_the_mirror() {
        set_accent(0x00B1_6300);
        assert_eq!(accent(), 0x00B1_6300);
    }

    #[test]
    fn theme_and_contrast_flags_round_trip() {
        set_light_theme(true);
        assert!(light_theme());
        set_light_theme(false);
        assert!(!light_theme());
        set_high_contrast(true);
        assert!(high_contrast());
        set_high_contrast(false);
    }

    #[test]
    fn animation_config_round_trips() {
        set_animation_config(0.5, true);
        assert_eq!(animation_config(), (0.5, true));
    }
}
