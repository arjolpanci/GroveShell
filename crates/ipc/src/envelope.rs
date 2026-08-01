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
    /// Pushed by `groveshell-settings` to the `groveshell-ui` pipe after
    /// every successful `config.toml` save, so `groveshell-ui` can reload
    /// and re-apply settings live without a restart. No payload; the
    /// receiver always re-reads the config file itself rather than
    /// trusting an embedded copy, so this message can never carry a
    /// version that's already stale by the time it's read.
    pub const CONFIG_RELOAD: &str = "config.reload";
    /// Sent to the `groveshell-settings` pipe to ask the already-running
    /// instance to open (or foreground) its settings window. Sent by a
    /// second `groveshell-settings.exe` launch that lost the
    /// single-instance mutex race (instead of just silently exiting), and
    /// by `apps/ui`'s top-bar settings button. No payload, no response
    /// expected — the sender doesn't wait for one.
    pub const SETTINGS_SHOW: &str = "settings.show";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reload_message_type_is_stable() {
        assert_eq!(message_type::CONFIG_RELOAD, "config.reload");
    }

    #[test]
    fn settings_show_message_type_is_stable() {
        assert_eq!(message_type::SETTINGS_SHOW, "settings.show");
    }
}
