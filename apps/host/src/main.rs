//! `groveshell-host`: the always-on shell process. Acquires a single-instance
//! lock, joins the shared shell job object (`docs/PROJECT_PLAN.md` §13.3),
//! loads configuration, then serves `host.ping` and `host.shutdown` on the
//! `groveshell-host` pipe and pushes `watchdog.heartbeat` to the
//! `groveshell-watchdog` pipe every [`imp::HEARTBEAT_INTERVAL`], per
//! `docs/PROJECT_PLAN.md` §13.2.

#[cfg(windows)]
mod imp {
    use std::time::Duration;
    use groveshell_common::jobobject::ShellJob;
    use groveshell_common::Result;
    use groveshell_ipc::{message_type, pipe, Envelope};

    const HOST_PIPE_NAME: &str = "groveshell-host";
    const WATCHDOG_PIPE_NAME: &str = "groveshell-watchdog";
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
    /// How long to wait before retrying after a transient
    /// `bind_and_accept` failure, matching the watchdog's own retry pace.
    const BIND_RETRY_INTERVAL: Duration = Duration::from_secs(1);

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("host")?;
        tracing::info!("groveshell-host starting");

        let _single_instance = acquire_single_instance_lock()?;
        tracing::info!("single-instance lock acquired");

        let _job = ShellJob::create_and_join()?;
        tracing::info!("joined shell job object");

        let config_path = groveshell_common::paths::data_dir()?.join("config.toml");
        let config = groveshell_config::load_or_default(&config_path);
        tracing::info!(?config, "configuration loaded");

        std::thread::spawn(heartbeat_loop);

        serve_ping()
    }

    /// Acquires a session-local named mutex so at most one `groveshell-host`
    /// runs at a time. Holds the handle for the lifetime of the process; the
    /// OS releases it automatically on exit or crash.
    fn acquire_single_instance_lock() -> Result<SingleInstanceGuard> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
        use windows::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = "Local\\GroveShell-Host-SingleInstance"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `name` is a valid, NUL-terminated UTF-16 buffer that outlives
        // this call (it's a local that isn't dropped until after the call
        // returns). No security attributes are supplied, so the handle gets
        // default access and is not inheritable. `true` requests initial
        // ownership of the mutex if this call creates it; if the mutex already
        // existed (another instance is running), initial ownership is not
        // granted and `GetLastError` reports `ERROR_ALREADY_EXISTS` below.
        let handle: HANDLE = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }
            .map_err(groveshell_common::Error::Windows)?;

        // SAFETY: no preconditions; `GetLastError` reads thread-local state set
        // by the immediately preceding `CreateMutexW` call above, which
        // succeeded (the `?` above would have returned on failure).
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            tracing::error!("another groveshell-host instance is already running");
            std::process::exit(1);
        }

        Ok(SingleInstanceGuard(handle))
    }

    struct SingleInstanceGuard(windows::Win32::Foundation::HANDLE);

    impl Drop for SingleInstanceGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` was created by `CreateMutexW` in
            // `acquire_single_instance_lock` and is owned exclusively by this
            // guard; nothing else closes it, and this `Drop` impl runs at most
            // once per guard.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }

    /// Pushes a `watchdog.heartbeat` message to the watchdog's pipe every
    /// [`HEARTBEAT_INTERVAL`]. Each heartbeat opens a short-lived connection
    /// rather than holding one open, so a watchdog restart doesn't require the
    /// host to reconnect any special-cased logic.
    fn heartbeat_loop() {
        let pid = std::process::id();
        loop {
            match pipe::connect(WATCHDOG_PIPE_NAME) {
                Ok(mut conn) => {
                    let envelope = Envelope::new(
                        "groveshell-host",
                        message_type::HEARTBEAT,
                        serde_json::json!({ "pid": pid }),
                    );
                    if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &envelope) {
                        tracing::warn!(error = ?e, "failed to send heartbeat");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "could not connect to watchdog pipe");
                }
            }
            std::thread::sleep(HEARTBEAT_INTERVAL);
        }
    }

    /// Serves `host.ping` and `host.shutdown` requests on the
    /// `groveshell-host` pipe forever, answering the former with `host.pong`
    /// and the latter with `host.shutdown_ack` before exiting. This is the
    /// Phase 0 manual smoke test surface (`groveshell-cli ping`/`shutdown`).
    ///
    /// Each accepted connection is handled on its own short-lived thread
    /// rather than inline, so a client that connects and then never writes
    /// anything can't wedge the accept loop and block every other client
    /// (finding: "no read timeout on the single-connection servers").
    fn serve_ping() -> Result<()> {
        loop {
            let conn = match pipe::bind_and_accept(HOST_PIPE_NAME) {
                Ok(conn) => conn,
                Err(e) => {
                    // A transient bind/accept failure must not take the
                    // whole host process down (and trigger watchdog
                    // recovery) — log and retry, matching the watchdog's
                    // own `heartbeat_server` handling of the same error.
                    tracing::error!(error = ?e, "failed to bind host pipe; retrying");
                    std::thread::sleep(BIND_RETRY_INTERVAL);
                    continue;
                }
            };
            std::thread::spawn(move || handle_connection(conn));
        }
    }

    fn handle_connection(mut conn: std::fs::File) {
        match groveshell_ipc::framing::read_envelope(&mut conn) {
            Ok(request) if request.message_type == message_type::PING => {
                let response = Envelope::new(
                    "groveshell-host",
                    message_type::PONG,
                    serde_json::json!({ "echo_of": request.request_id }),
                );
                if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &response) {
                    tracing::warn!(error = ?e, "failed to respond to ping");
                    return;
                }
                // `write_envelope`'s `flush()` is a no-op for `std::fs::File`
                // (it doesn't call `FlushFileBuffers`). Without this, the
                // server can close its handle to the pipe before the client
                // has read the response, and Windows discards unread data
                // on server-side disconnect. Block here until the client has
                // had the chance to read it.
                if let Err(e) = conn.sync_all() {
                    tracing::warn!(error = ?e, "failed to flush ping response to client");
                }
            }
            Ok(request) if request.message_type == message_type::SHUTDOWN => {
                tracing::info!("shutdown requested; exiting");
                let response = Envelope::new(
                    "groveshell-host",
                    message_type::SHUTDOWN_ACK,
                    serde_json::json!({ "echo_of": request.request_id }),
                );
                if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &response) {
                    tracing::warn!(error = ?e, "failed to acknowledge shutdown");
                }
                // See the sync_all() note above the PING arm — the client
                // must have a chance to read the ack before the process
                // exits and the pipe handle disappears out from under it.
                let _ = conn.sync_all();
                std::process::exit(0);
            }
            Ok(other) => {
                tracing::warn!(message_type = %other.message_type, "unexpected message on host pipe");
            }
            Err(e) => {
                tracing::warn!(error = ?e, "failed to read request on host pipe");
            }
        }
    }
}

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-host is Windows-only.");
    std::process::exit(1);
}
