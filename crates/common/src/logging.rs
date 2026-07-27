use crate::Result;
use tracing_appender::non_blocking::WorkerGuard;

/// Initializes structured tracing for a GroveShell process, writing to a
/// daily-rotating log file named `<component>.log` under the shared log
/// directory. Returns a guard that must be kept alive for the lifetime of
/// `main` — dropping it flushes and stops the background writer thread.
pub fn init(component: &str) -> Result<WorkerGuard> {
    let dir = crate::paths::log_dir()?;
    std::fs::create_dir_all(&dir)?;

    let file_appender = tracing_appender::rolling::daily(&dir, format!("{component}.log"));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .init();

    Ok(guard)
}
