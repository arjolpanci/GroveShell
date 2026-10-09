//! The GroveShell UI kit: the design tokens, icon set, Direct2D device
//! layer and painter shared by `apps/ui` and `apps/settings`.
//!
//! This exists because both binaries draw the same shell: the settings
//! window is not a different product with a different look, and the one
//! time it had its own palette it drifted into something that matched
//! neither Windows nor the shell.

pub mod design;
pub mod runtime;
pub mod theme;
