//! Shape, spacing, and elevation tokens (spec §3.2). One home for the
//! rounded-corner radii, the 8px spacing grid, the hairline stroke width,
//! and the single flyout/card shadow spec, so every surface shares the
//! same geometry instead of re-deriving it.
//!
//! All values are logical (96-DPI reference) pixels; scale them with the
//! same `state::scaled` / per-monitor DPI helpers the rest of the UI uses.

/// Corner radius for chips, buttons, hover highlights.
pub(crate) const RADIUS_CHIP: i32 = 8;

/// Corner radius for cards and flyouts.
pub(crate) const RADIUS_CARD: i32 = 12;

/// Base spacing unit; layouts step in multiples of this.
pub(crate) const SPACING: i32 = 8;

/// Hairline border/divider width.
pub(crate) const STROKE_WIDTH: i32 = 1;

/// The single drop-shadow spec for elevated surfaces (flyouts, cards).
#[derive(Clone, Copy)]
pub(crate) struct Shadow {
    pub blur: i32,
    pub dx: i32,
    pub dy: i32,
    /// Shadow color as a `COLORREF` (opaque black; callers apply their own
    /// alpha/spread when compositing).
    pub color: u32,
}

/// The shared elevation shadow: soft, slightly downward.
pub(crate) fn shadow() -> Shadow {
    Shadow { blur: 18, dx: 0, dy: 6, color: 0x0000_0000 }
}
