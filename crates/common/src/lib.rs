pub mod error;
pub mod logging;
pub mod paths;
pub mod taskbar_state;

#[cfg(windows)]
pub mod jobobject;

pub use error::{Error, Result};
