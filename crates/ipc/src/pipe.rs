#![cfg(windows)]

//! Minimal named-pipe transport. Each GroveShell pipe instance serves one
//! client connection at a time: a server calls [`bind_and_accept`] in a
//! loop to get the next connection. Callers that want to keep accepting
//! new connections while a slow/stuck client is still being read from
//! (e.g. `groveshell-host` and `groveshell-watchdog`) hand each connection off
//! to its own short-lived thread rather than blocking the accept loop on
//! it — see the `serve_ping`/`heartbeat_server` callers.

use std::fs::{File, OpenOptions};
use std::os::windows::io::FromRawHandle;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use groveshell_common::{Error, Result};

/// Win32 `ERROR_PIPE_CONNECTED`: a client connected between
/// `CreateNamedPipeW` and `ConnectNamedPipe`, which is a documented
/// success case for `ConnectNamedPipe`, not a failure.
const ERROR_PIPE_CONNECTED: i32 = 535;

/// Session-scoping suffix appended to every pipe name so two different
/// users on the same machine (e.g. fast user switching or RDP, both of
/// which share the machine-wide pipe namespace) don't collide on the same
/// pipe name. `USERNAME` is simple and always set for an interactive
/// session; it is computed here once so `groveshell-host`, `groveshell-watchdog`,
/// and `groveshell-cli` automatically agree on the scoping without each
/// needing to duplicate the logic. A full security-descriptor DACL
/// restricting the pipe to the creating user's SID (per
/// `docs/PROJECT_PLAN.md` §12, "restrictive pipe security descriptors") is
/// a more complete fix and is left as a documented follow-up.
fn session_scope() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string())
}

fn pipe_path(name: &str) -> String {
    format!(r"\\.\pipe\{name}-{}", session_scope())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Creates (or re-creates, for the next client) a named pipe instance and
/// blocks until a client connects, then returns a `File` for that single
/// connection. Call this again after the returned `File` is dropped to
/// serve the next client.
pub fn bind_and_accept(name: &str) -> Result<File> {
    let wide_path = to_wide(&pipe_path(name));

    // SAFETY: `wide_path` is a valid, NUL-terminated UTF-16 buffer that
    // outlives this call (it's a local that isn't dropped until after the
    // call returns). No security attributes are supplied, so the handle
    // gets default access and is not inheritable. All other arguments are
    // plain values with no aliasing or lifetime requirements.
    let handle: HANDLE = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide_path.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }

    // SAFETY: `handle` was just created above by `CreateNamedPipeW` and is
    // a valid, unconnected named pipe server instance owned by this call
    // frame. No overlapped I/O structure is supplied, so this call blocks
    // synchronously until a client connects or an error occurs.
    let connect_result = unsafe { ConnectNamedPipe(handle, None) };
    if connect_result.is_err() {
        let err = std::io::Error::last_os_error();
        // A client racing in between CreateNamedPipeW and ConnectNamedPipe
        // is reported as this specific error code, and it means success,
        // not failure.
        if err.raw_os_error() != Some(ERROR_PIPE_CONNECTED) {
            // SAFETY: `handle` is still owned by this frame; no `File` has
            // taken ownership of it yet, so it must be closed here to
            // avoid leaking the pipe instance on the error path.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            return Err(Error::Io(err));
        }
    }

    // SAFETY: `handle` is a valid, connected named pipe instance handle
    // that nothing else holds a reference to at this point. `File` takes
    // ownership of the raw handle and will close it via `CloseHandle` when
    // dropped, so it must not be closed anywhere else past this line.
    Ok(unsafe { File::from_raw_handle(handle.0 as *mut _) })
}

/// Connects to an existing named pipe as a client.
pub fn connect(name: &str) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_path(name))
        .map_err(Error::Io)
}
