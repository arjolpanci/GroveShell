//! Timing and physics for the shell's motion: the single progress
//! `Adjustment` every overview surface derives from, the swipe-gesture
//! physics ported from GNOME Shell's `js/ui/swipeTracker.js`, and the
//! interpolation helpers both need.
//!
//! Deliberately free of `windows` and of any painting: this is the
//! timing kit, `ui-kit::design` is the painting kit. Everything here is
//! a pure function or a plain state machine, so the feel of the shell is
//! unit-testable without a monitor.

pub mod lerp;

pub use lerp::{lerp, lerp_rect, Easing, RectF};
