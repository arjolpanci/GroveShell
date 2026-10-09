//! Process-wide values the design tokens read: the live Windows accent,
//! the light/dark and high-contrast flags, the animation config, and DPI
//! scaling.
//!
//! Thread-local `Cell`s rather than a struct threaded through every
//! call: these are read from inside paint paths that already hold other
//! borrows, and a nested `RefCell` borrow panics (see
//! `apps/ui/src/imp/state.rs`, where this pattern started).

use std::cell::Cell;

thread_local! {
    static ACCENT: Cell<u32> = const { Cell::new(0x00B1_6300) };
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
