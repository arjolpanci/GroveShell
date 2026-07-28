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
        /// Lists connected monitors with their bounds and work areas.
        ListMonitors,
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("cli")?;
        let cli = Cli::parse();

        match cli.command {
            Command::Ping => ping(),
            Command::Shutdown => shutdown(),
            Command::ListWindows => list_windows(),
            Command::ListMonitors => list_monitors(),
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
        println!("{:<8} {:<26} {:<26}", "PRIMARY", "BOUNDS", "WORK AREA");
        for m in &monitors {
            let bounds = format!(
                "({},{})-({},{})",
                m.rect.left, m.rect.top, m.rect.right, m.rect.bottom
            );
            let work = format!(
                "({},{})-({},{})",
                m.work.left, m.work.top, m.work.right, m.work.bottom
            );
            println!("{:<8} {:<26} {:<26}", if m.is_primary { "yes" } else { "no" }, bounds, work);
        }
        println!("{} monitor(s)", monitors.len());
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
