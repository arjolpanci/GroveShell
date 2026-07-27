pub mod envelope;
pub mod framing;

#[cfg(windows)]
pub mod pipe;

pub use envelope::{message_type, Envelope, PROTOCOL_VERSION};
