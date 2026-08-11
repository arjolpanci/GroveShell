#[cfg(windows)]
mod imp {
    use clap::{Parser, Subcommand};
    use groveshell_common::Result;
    use groveshell_ipc::{message_type, pipe, Envelope};

    const HOST_PIPE_NAME: &str = "groveshell-host";
    const WATCHDOG_PIPE_NAME: &str = "groveshell-watchdog";

    #[derive(Parser)]
    #[command(
        name = "groveshell-cli",
        about = "GroveShell diagnostics and automation CLI"
    )]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Sends a ping to a running groveshell-host and prints the round-trip time.
        Ping,
        /// Gracefully stops groveshell-host and groveshell-watchdog, in that order.
        Shutdown,
        /// Lists eligible top-level windows (the same set the shell manages).
        ListWindows,
        /// Lists connected monitors with their bounds, work areas, and DPI.
        ListMonitors,
        /// Prints a JSON snapshot of windows + monitors to stdout, honoring
        /// the configured privacy redaction of window titles.
        DumpState,
        /// Collects config, logs, and a state dump into a timestamped
        /// diagnostics bundle for attaching to a bug report.
        Diagnostics {
            /// Directory to write the bundle into (a timestamped subfolder
            /// is created inside it). Defaults to `<data_dir>/diagnostics`.
            #[arg(long)]
            out: Option<std::path::PathBuf>,
        },
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("cli")?;
        let cli = Cli::parse();

        match cli.command {
            Command::Ping => ping(),
            Command::Shutdown => shutdown(),
            Command::ListWindows => list_windows(),
            Command::ListMonitors => list_monitors(),
            Command::DumpState => dump_state(),
            Command::Diagnostics { out } => diagnostics(out),
        }
    }

    /// Enumerated directly in this process (no IPC round-trip to the shell
    /// needed — `EnumWindows` sees the same session state from anywhere),
    /// so this also works as a diagnostic when the shell isn't running.
    fn list_windows() -> Result<()> {
        groveshell_window_model::make_process_dpi_aware();
        let windows = groveshell_window_model::snapshot();
        println!("{:<12} {:<8} {:<24} {:<26} TITLE", "HWND", "PID", "EXE", "RECT");
        for w in &windows {
            let rect = format!(
                "({},{})-({},{})",
                w.rect.left, w.rect.top, w.rect.right, w.rect.bottom
            );
            println!(
                "{:<12} {:<8} {:<24} {:<26} {}",
                format!("{:#x}", w.hwnd),
                w.pid,
                w.exe_name.as_deref().unwrap_or("?"),
                rect,
                w.title
            );
        }
        println!("{} eligible top-level window(s)", windows.len());
        Ok(())
    }

    fn list_monitors() -> Result<()> {
        groveshell_window_model::make_process_dpi_aware();
        let monitors = groveshell_window_model::monitors();
        println!("{:<8} {:<26} {:<26} {:<10}", "PRIMARY", "BOUNDS", "WORK AREA", "DPI/SCALE");
        for m in &monitors {
            let bounds = format!(
                "({},{})-({},{})",
                m.rect.left, m.rect.top, m.rect.right, m.rect.bottom
            );
            let work = format!(
                "({},{})-({},{})",
                m.work.left, m.work.top, m.work.right, m.work.bottom
            );
            let dpi = format!("{} ({:.0}%)", m.dpi, groveshell_window_model::scale_for_dpi(m.dpi) * 100.0);
            println!(
                "{:<8} {:<26} {:<26} {:<10}",
                if m.is_primary { "yes" } else { "no" },
                bounds,
                work,
                dpi
            );
        }
        println!("{} monitor(s)", monitors.len());
        Ok(())
    }

    /// Whether window titles should be redacted anywhere this process
    /// writes them out, per the on-disk config's `[privacy]` section
    /// (defaults to redacting, so a missing/unreadable config is treated
    /// as the private choice).
    fn redact_titles() -> bool {
        match groveshell_common::paths::data_dir() {
            Ok(dir) => groveshell_config::load_or_default(&dir.join("config.toml"))
                .privacy
                .redact_window_titles,
            Err(_) => true,
        }
    }

    /// A JSON value describing the current window + monitor state, with
    /// window titles redacted when `redact` is set. Shared by `dump-state`
    /// (stdout) and `diagnostics` (bundle file) so both stay consistent.
    fn state_json(redact: bool) -> serde_json::Value {
        groveshell_window_model::make_process_dpi_aware();
        let windows: Vec<serde_json::Value> = groveshell_window_model::snapshot()
            .into_iter()
            .map(|w| {
                serde_json::json!({
                    "hwnd": format!("{:#x}", w.hwnd),
                    "pid": w.pid,
                    "exe": w.exe_name,
                    "class": w.class,
                    "title": if redact { "<redacted>".to_string() } else { w.title },
                    "rect": [w.rect.left, w.rect.top, w.rect.right, w.rect.bottom],
                })
            })
            .collect();
        let monitors: Vec<serde_json::Value> = groveshell_window_model::monitors()
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "primary": m.is_primary,
                    "dpi": m.dpi,
                    "scale": groveshell_window_model::scale_for_dpi(m.dpi),
                    "bounds": [m.rect.left, m.rect.top, m.rect.right, m.rect.bottom],
                    "work": [m.work.left, m.work.top, m.work.right, m.work.bottom],
                })
            })
            .collect();
        serde_json::json!({
            "windows": windows,
            "monitors": monitors,
            "titles_redacted": redact,
        })
    }

    fn dump_state() -> Result<()> {
        let state = state_json(redact_titles());
        println!("{}", serde_json::to_string_pretty(&state)?);
        Ok(())
    }

    /// Collects config (+ backup), all rotating log files, and a state dump
    /// into a fresh timestamped folder, then prints its path. Titles in the
    /// state dump follow the privacy setting; the raw log files are copied
    /// verbatim (they already avoid recording titles, per PROJECT_PLAN §12).
    fn diagnostics(out: Option<std::path::PathBuf>) -> Result<()> {
        use std::fs;

        let data_dir = groveshell_common::paths::data_dir()?;
        let base = out.unwrap_or_else(|| data_dir.join("diagnostics"));
        // Seconds since the epoch keeps the folder name sortable and needs
        // no date-formatting dependency.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bundle = base.join(format!("groveshell-diagnostics-{stamp}"));
        fs::create_dir_all(&bundle)?;

        // Config and its known-good backup, if present.
        for name in ["config.toml", "config.toml.bak"] {
            let src = data_dir.join(name);
            if src.exists() {
                let _ = fs::copy(&src, bundle.join(name));
            }
        }

        // Every rotating log file.
        let mut log_count = 0;
        if let Ok(log_dir) = groveshell_common::paths::log_dir() {
            if let Ok(entries) = fs::read_dir(&log_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_file() {
                        if let Some(name) = entry.path().file_name() {
                            if fs::copy(entry.path(), bundle.join(name)).is_ok() {
                                log_count += 1;
                            }
                        }
                    }
                }
            }
        }

        // State dump (privacy-aware).
        let redact = redact_titles();
        let state = serde_json::to_string_pretty(&state_json(redact))?;
        fs::write(bundle.join("state.json"), state)?;

        println!("diagnostics bundle written to {}", bundle.display());
        println!(
            "  config + backup, {log_count} log file(s), state.json (titles {})",
            if redact { "redacted" } else { "included" }
        );
        Ok(())
    }

    fn ping() -> Result<()> {
        let start = std::time::Instant::now();

        let mut conn = match pipe::connect(HOST_PIPE_NAME) {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("could not connect to groveshell-host: {e}");
                eprintln!("is groveshell-host running?");
                return Err(e);
            }
        };

        let request = Envelope::new("groveshell-cli", message_type::PING, serde_json::json!({}));
        groveshell_ipc::framing::write_envelope(&mut conn, &request)?;

        let response = groveshell_ipc::framing::read_envelope(&mut conn)?;
        println!(
            "pong from {} in {:?} (request_id {})",
            response.sender,
            start.elapsed(),
            response.request_id
        );
        Ok(())
    }

    fn shutdown() -> Result<()> {
        shutdown_one(HOST_PIPE_NAME, "groveshell-host", message_type::SHUTDOWN);
        // The host is asked to stop first: while it's still alive it keeps
        // sending heartbeats, which is harmless but pointless noise once
        // the watchdog is on its way down too.
        shutdown_one(
            WATCHDOG_PIPE_NAME,
            "groveshell-watchdog",
            message_type::WATCHDOG_SHUTDOWN,
        );
        Ok(())
    }

    /// Best-effort graceful stop of a single process over its pipe. A
    /// connection failure means the process isn't running at all, which is
    /// the desired end state either way, so it's reported rather than
    /// treated as an error.
    fn shutdown_one(pipe_name: &str, label: &str, message_type: &str) {
        let mut conn = match pipe::connect(pipe_name) {
            Ok(conn) => conn,
            Err(_) => {
                println!("{label}: not running");
                return;
            }
        };

        let request = Envelope::new("groveshell-cli", message_type, serde_json::json!({}));
        if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &request) {
            println!("{label}: failed to send shutdown request: {e}");
            return;
        }

        match groveshell_ipc::framing::read_envelope(&mut conn) {
            Ok(_) => println!("{label}: stopped"),
            Err(e) => println!("{label}: sent shutdown but did not get an acknowledgement: {e}"),
        }
    }
}

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-cli is Windows-only.");
    std::process::exit(1);
}
