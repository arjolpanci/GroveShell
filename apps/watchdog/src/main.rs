//! `groveshell-watchdog`: listens for `watchdog.heartbeat` messages from the
//! host on the `groveshell-watchdog` pipe, and if the host goes quiet, marks
//! it unhealthy and eventually recovers by terminating the shared shell job
//! object, ensuring `explorer.exe` is running, and recording a crash
//! marker. See `docs/PROJECT_PLAN.md` §13.2 for the timing protocol this
//! implements (6s unhealthy threshold, 2s grace period, then recovery).

#[cfg(windows)]
mod imp {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use groveshell_common::{jobobject::ShellJob, Result};
    use groveshell_ipc::{message_type, pipe, Envelope};

    const WATCHDOG_PIPE_NAME: &str = "groveshell-watchdog";
    const UNHEALTHY_AFTER: Duration = Duration::from_secs(6);
    const RECOVER_AFTER: Duration = Duration::from_secs(8); // unhealthy + 2s grace, per §13.2
    const POLL_INTERVAL: Duration = Duration::from_secs(1);

    /// Crash-loop guard: if recovery fires this many times within
    /// [`CRASH_LOOP_WINDOW`], automatic recovery is disabled and the human
    /// fallback (`recover.ps1`, per `docs/PROJECT_PLAN.md` §13.1: "Crash
    /// loops disable shell mode automatically after a threshold") takes
    /// over. The heartbeat listener keeps running either way.
    const CRASH_LOOP_THRESHOLD: usize = 3;
    const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60);

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("watchdog")?;
        tracing::info!("groveshell-watchdog starting");

        // `None` means "no heartbeat has ever been observed", which is
        // distinct from "the host went quiet" — treating process start
        // identically to a stale/dead host would spuriously trigger
        // recovery against a host that just hasn't started yet.
        let last_heartbeat: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

        let listener_state = last_heartbeat.clone();
        std::thread::spawn(move || heartbeat_server(listener_state));

        monitor_loop(last_heartbeat);
        Ok(())
    }

    /// Accepts heartbeat connections on the `groveshell-watchdog` pipe forever,
    /// updating `last_heartbeat` on every `watchdog.heartbeat` message
    /// received.
    ///
    /// Each connection is handled on its own short-lived thread rather than
    /// inline, so a client that connects and never writes anything can't
    /// wedge the accept loop — while wedged inline, no further heartbeats
    /// would be consumed and `monitor_loop` could fire spurious recovery
    /// even though the real host is fine.
    fn heartbeat_server(last_heartbeat: Arc<Mutex<Option<Instant>>>) {
        loop {
            let conn = match pipe::bind_and_accept(WATCHDOG_PIPE_NAME) {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::error!(error = ?e, "failed to bind watchdog pipe; retrying");
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
            };

            let state = last_heartbeat.clone();
            std::thread::spawn(move || handle_heartbeat_connection(conn, state));
        }
    }

    fn handle_heartbeat_connection(
        mut conn: std::fs::File,
        last_heartbeat: Arc<Mutex<Option<Instant>>>,
    ) {
        match groveshell_ipc::framing::read_envelope(&mut conn) {
            Ok(envelope) if envelope.message_type == message_type::HEARTBEAT => {
                *last_heartbeat.lock().expect("heartbeat mutex poisoned") = Some(Instant::now());
                tracing::debug!("heartbeat received");
            }
            Ok(request) if request.message_type == message_type::WATCHDOG_SHUTDOWN => {
                tracing::info!("shutdown requested; exiting");
                let response = Envelope::new(
                    "groveshell-watchdog",
                    message_type::WATCHDOG_SHUTDOWN_ACK,
                    serde_json::json!({ "echo_of": request.request_id }),
                );
                if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &response) {
                    tracing::warn!(error = ?e, "failed to acknowledge shutdown");
                }
                // See the sync_all() note in the host's shutdown handler —
                // the client needs a chance to read the ack before this
                // process exits and the pipe handle goes away.
                let _ = conn.sync_all();
                std::process::exit(0);
            }
            Ok(other) => {
                tracing::warn!(message_type = %other.message_type, "unexpected message on watchdog pipe");
            }
            Err(e) => {
                tracing::warn!(error = ?e, "failed to read heartbeat");
            }
        }
    }

    /// Polls heartbeat age once per second. Logs unhealthy at
    /// [`UNHEALTHY_AFTER`] and runs recovery at [`RECOVER_AFTER`], matching
    /// the "unhealthy after 6s, recover if no response after 2 more seconds"
    /// protocol from `docs/PROJECT_PLAN.md` §13.2. Does nothing until the
    /// first heartbeat has ever been observed.
    fn monitor_loop(last_heartbeat: Arc<Mutex<Option<Instant>>>) {
        let mut already_warned_unhealthy = false;
        let mut recovery_history: Vec<Instant> = Vec::new();
        let mut crash_loop_disabled = false;

        loop {
            std::thread::sleep(POLL_INTERVAL);

            let Some(elapsed) = last_heartbeat
                .lock()
                .expect("heartbeat mutex poisoned")
                .map(|t| t.elapsed())
            else {
                // No heartbeat has arrived yet; nothing to evaluate.
                continue;
            };

            if crash_loop_disabled {
                // Automatic recovery is disabled; keep polling (so logs
                // still show state) but never call `recover()` again.
                continue;
            }

            if elapsed >= RECOVER_AFTER {
                tracing::error!(
                    elapsed_secs = elapsed.as_secs(),
                    "host unresponsive, recovering"
                );

                let now = Instant::now();
                recovery_history.retain(|t| now.duration_since(*t) <= CRASH_LOOP_WINDOW);
                recovery_history.push(now);

                if recovery_history.len() > CRASH_LOOP_THRESHOLD {
                    tracing::error!(
                        recoveries = recovery_history.len(),
                        window_secs = CRASH_LOOP_WINDOW.as_secs(),
                        "crash loop detected: automatic recovery disabled; run recover.ps1 manually"
                    );
                    crash_loop_disabled = true;
                    continue;
                }

                recover();
                *last_heartbeat.lock().expect("heartbeat mutex poisoned") = Some(Instant::now());
                already_warned_unhealthy = false;
            } else if elapsed >= UNHEALTHY_AFTER && !already_warned_unhealthy {
                tracing::warn!(elapsed_secs = elapsed.as_secs(), "host marked unhealthy");
                already_warned_unhealthy = true;
            } else if elapsed < UNHEALTHY_AFTER {
                already_warned_unhealthy = false;
            }
        }
    }

    /// Terminates the shared shell job object (killing the host, if it's still
    /// alive), makes sure `explorer.exe` is running, and appends a
    /// crash-recovery marker to the on-disk crash log.
    fn recover() {
        if let Err(e) = ShellJob::terminate_by_name() {
            tracing::warn!(error = ?e, "failed to terminate shell job (it may not exist yet)");
        }

        if !is_explorer_running() {
            match std::process::Command::new("explorer.exe").spawn() {
                Ok(_) => tracing::info!("explorer.exe restarted"),
                Err(e) => tracing::error!(error = ?e, "failed to restart explorer.exe"),
            }
        } else {
            tracing::info!("explorer.exe already running, no restart needed");
        }

        write_crash_marker();
    }

    /// Walks a `TH32CS_SNAPPROCESS` snapshot of all running processes looking
    /// for `explorer.exe`. `szExeFile` is a fixed-size `[u16; MAX_PATH]`
    /// buffer, not a Rust string, so the NUL terminator has to be located
    /// before slicing and decoding it (a shorter name still leaves the rest of
    /// the buffer as leftover/garbage bytes from a previous entry).
    fn is_explorer_running() -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };

        let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!(error = ?e, "could not snapshot processes; assuming explorer is down");
                return false;
            }
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut found = false;
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while has_entry {
            let nul_pos = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let exe_name = String::from_utf16_lossy(&entry.szExeFile[..nul_pos]);
            if exe_name.eq_ignore_ascii_case("explorer.exe") {
                found = true;
                break;
            }
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }

        unsafe {
            let _ = CloseHandle(snapshot);
        }
        found
    }

    fn write_crash_marker() {
        let Ok(data_dir) = groveshell_common::paths::data_dir() else {
            tracing::error!("could not resolve data dir; skipping crash marker");
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            tracing::error!(error = ?e, "could not create data dir; skipping crash marker");
            return;
        }
        let marker_path = data_dir.join("crash-log.txt");
        let line = format!("{:?} recovery triggered\n", std::time::SystemTime::now());
        if let Err(e) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&marker_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()))
        {
            tracing::error!(error = ?e, "failed to write crash marker");
        }
    }
}

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-watchdog is Windows-only.");
    std::process::exit(1);
}
