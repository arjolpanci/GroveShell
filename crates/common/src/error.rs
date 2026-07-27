use thiserror::Error;

/// Shared error type for all GroveShell crates. Crate-specific error cases
/// that don't fit here should wrap this type rather than inventing a
/// parallel hierarchy, so callers only ever handle one `Result` type
/// across crate boundaries.
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("ipc protocol error: {0}")]
    Protocol(String),

    #[cfg(windows)]
    #[error("windows API error: {0}")]
    Windows(#[from] windows::core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
