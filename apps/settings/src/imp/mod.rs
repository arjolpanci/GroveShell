//! Process bootstrap for `groveshell-settings`.

mod health;
mod nav;
mod pages;
mod process;
mod theme;
mod tray;
mod util_text;
mod window;

use groveshell_common::Result;
use process::ManagedProcesses;

pub fn run() -> Result<()> {
    let _log_guard = groveshell_common::logging::init("settings")?;
    tracing::info!("groveshell-settings starting");

    let _single_instance = acquire_single_instance_lock()?;
    tracing::info!("single-instance lock acquired");

    let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
    tracing::info!("joined shell job object");

    let config_path = groveshell_common::paths::data_dir()?.join("config.toml");
    let config = groveshell_config::load_or_default(&config_path);
    tracing::info!(?config, "configuration loaded");

    let mut processes = ManagedProcesses::new();
    processes.spawn_all();

    tray::run_message_loop(processes)
}

/// Acquires a session-local named mutex so at most one `groveshell-settings`
/// runs at a time — same pattern as `apps/host`'s own single-instance lock.
fn acquire_single_instance_lock() -> Result<SingleInstanceGuard> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\GroveShell-Settings-SingleInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `name` is a valid, NUL-terminated UTF-16 buffer that outlives
    // this call. No security attributes are supplied, so the handle gets
    // default access and is not inheritable.
    let handle: HANDLE = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }
        .map_err(groveshell_common::Error::Windows)?;

    // SAFETY: `GetLastError` reads thread-local state set by the
    // immediately preceding `CreateMutexW` call above, which succeeded.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        tracing::info!("another groveshell-settings instance is already running; exiting");
        std::process::exit(0);
    }

    Ok(SingleInstanceGuard(handle))
}

struct SingleInstanceGuard(windows::Win32::Foundation::HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `CreateMutexW` above and is owned
        // exclusively by this guard.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
