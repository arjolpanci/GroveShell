//! Shell surface palette, with a high-contrast alternative (PROJECT_PLAN
//! §16 accessibility). Every accessor returns a Win32 `COLORREF`-style
//! `0x00BBGGRR` value and branches on [`state::high_contrast`], so a single
//! config toggle (`appearance.high_contrast`) restyles every surface that
//! draws through these instead of a hard-coded literal.
//!
//! The default (non-high-contrast) values are the established dark-grey
//! palette the bar, calendar, and overview were built with — kept here so
//! there is one home for "what colour is shell text" rather than the same
//! `0x00E0E0E0` repeated across files. High-contrast mode swaps to pure
//! black on white-adjacent foregrounds with a saturated yellow accent, the
//! convention Windows' own high-contrast themes use for maximum legibility.

use super::state;

/// Primary foreground: labels, glyphs, clock text.
pub(crate) fn text() -> u32 {
    if state::high_contrast() {
        0x00FFFFFF // white
    } else {
        0x00E0E0E0
    }
}

/// Secondary/de-emphasised foreground: inactive workspace dots, hints.
pub(crate) fn text_muted() -> u32 {
    if state::high_contrast() {
        0x00C0C0C0 // light grey, still well above the 4.5:1 bar on black
    } else {
        0x00606060
    }
}

/// The bar / panel background fill.
pub(crate) fn background() -> u32 {
    if state::high_contrast() {
        0x00000000 // black
    } else {
        0x00202020
    }
}

/// A raised panel / hovered chip fill sitting on top of [`background`].
pub(crate) fn panel() -> u32 {
    if state::high_contrast() {
        0x00202020 // near-black so it reads as a distinct raised surface
    } else {
        0x00303030
    }
}

/// Accent used for active/selected emphasis (active workspace dot, focus
/// ring). Saturated yellow in high contrast; the established light grey
/// otherwise (callers that want a specific brand colour still pass their
/// own literal — this is only for elements that should track the theme).
pub(crate) fn accent() -> u32 {
    if state::high_contrast() {
        0x0000FFFF // yellow (R255 G255 B0 in 0x00BBGGRR)
    } else {
        0x00E0E0E0
    }
}
