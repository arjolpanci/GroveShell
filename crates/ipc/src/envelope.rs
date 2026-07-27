use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

/// The IPC message envelope shared by every GroveShell process, matching
/// `docs/PROJECT_PLAN.md` §11.1 exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub sender: String,
    pub message_type: String,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(
        sender: impl Into<String>,
        message_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            sender: sender.into(),
            message_type: message_type.into(),
            payload,
        }
    }
}

/// Well-known `message_type` values used in Phase 0.
pub mod message_type {
    pub const PING: &str = "host.ping";
    pub const PONG: &str = "host.pong";
    pub const HEARTBEAT: &str = "watchdog.heartbeat";
    /// Requests a graceful exit. Sent to either the host or the watchdog
    /// pipe; the receiver answers with the matching `*_ack` and then calls
    /// `std::process::exit`, so this is a normal stop, not the crash-loop
    /// recovery path in `docs/PROJECT_PLAN.md` §13.2.
    pub const SHUTDOWN: &str = "host.shutdown";
    pub const SHUTDOWN_ACK: &str = "host.shutdown_ack";
    pub const WATCHDOG_SHUTDOWN: &str = "watchdog.shutdown";
    pub const WATCHDOG_SHUTDOWN_ACK: &str = "watchdog.shutdown_ack";
}
