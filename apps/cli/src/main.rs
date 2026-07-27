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
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("cli")?;
        let cli = Cli::parse();

        match cli.command {
            Command::Ping => ping(),
            Command::Shutdown => shutdown(),
        }
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
