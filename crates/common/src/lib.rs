pub mod error;
pub mod logging;
pub mod paths;
pub mod processes;
pub mod taskbar_state;
pub mod touchpad_gestures;

#[cfg(windows)]
pub mod jobobject;

pub use error::{Error, Result};
