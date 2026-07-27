#[cfg(windows)]
mod imp {
    use clap::{Parser, Subcommand};
    use groveshell_common::Result;
    use groveshell_ipc::{message_type, pipe, Envelope};

    const HOST_PIPE_NAME: &str = "groveshell-host";

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
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("cli")?;
        let cli = Cli::parse();

        match cli.command {
            Command::Ping => ping(),
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
