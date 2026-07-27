pub mod error;
pub mod logging;
pub mod paths;

#[cfg(windows)]
pub mod jobobject;

pub use error::{Error, Result};
