//! The GroveShell design system: one source of truth for color, shape, and
//! motion tokens (spec §3). Every shell surface draws from these instead of
//! hard-coding literals, so the look stays consistent and a single change
//! restyles everything. Folds in the Phase 6 `palette` module.

// A design-token module exposes a complete, coherent set of tokens; the
// remaining Phase 4 stages (surface migration, flyouts, tray, dock) wire up
// the ones not yet consumed. Allowed module-wide so each staged commit stays
// warning-clean rather than churning the token list task by task.
#![allow(dead_code)]

pub(crate) mod color;
pub(crate) mod metrics;
pub(crate) mod motion;
