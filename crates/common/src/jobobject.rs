#![cfg(windows)]

//! A single named Windows job object that the host process joins at
//! startup and the watchdog can terminate by name from a different
//! process, without needing a shared handle. See `docs/PROJECT_PLAN.md`
//! §13.3 — this intentionally does not set `KILL_ON_JOB_CLOSE` yet.

use crate::{Error, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, OpenJobObjectW, TerminateJobObject,
    JOB_OBJECT_ALL_ACCESS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// Session-global job object name. Processes across the GroveShell shell
/// share this single job so the watchdog can find and terminate the whole
/// tree without holding a live handle from the host's process.
const JOB_NAME: &str = "Local\\GroveShell-ShellJob";

/// Handle to a job object this process has joined. Dropping it does not
/// terminate the job (no `KILL_ON_JOB_CLOSE`); it only closes this
/// process's handle to it.
pub struct ShellJob(HANDLE);

impl ShellJob {
    /// Creates (or opens, if it already exists) the shared shell job
    /// object and assigns the *current* process to it. Call this once,
    /// early in `main`, before spawning any child processes that should
    /// share the same crash-recovery boundary.
    pub fn create_and_join() -> Result<Self> {
        let name = to_wide(JOB_NAME);

        // SAFETY: `name` is a valid, NUL-terminated UTF-16 buffer that
        // outlives this call (it's a local that isn't dropped until after
        // the call returns). No security attributes are supplied, so the
        // handle gets default access and is not inheritable.
        let handle: HANDLE =
            unsafe { CreateJobObjectW(None, PCWSTR(name.as_ptr())) }.map_err(Error::Windows)?;

        // SAFETY: `handle` was just created above and is a valid job
        // object handle owned by this call frame; `GetCurrentProcess()`
        // returns the pseudo-handle for the calling process, which is
        // always valid and requires no cleanup.
        unsafe { AssignProcessToJobObject(handle, GetCurrentProcess()) }.map_err(Error::Windows)?;

        Ok(Self(handle))
    }

    /// Opens the shared shell job object by name from any process (e.g.
    /// the watchdog, which never joins it itself) and terminates every
    /// process currently in it. Used as the watchdog's recovery hammer
    /// when the host stops responding.
    pub fn terminate_by_name() -> Result<()> {
        let name = to_wide(JOB_NAME);

        // SAFETY: `name` is a valid, NUL-terminated UTF-16 buffer that
        // outlives this call. `JOB_OBJECT_ALL_ACCESS` requests full access
        // to an existing, already-named job object; `false` means the
        // returned handle is not inheritable by child processes.
        let handle: HANDLE =
            unsafe { OpenJobObjectW(JOB_OBJECT_ALL_ACCESS.0, false, PCWSTR(name.as_ptr())) }
                .map_err(Error::Windows)?;

        // SAFETY: `handle` was just opened above with terminate access
        // (implied by `JOB_OBJECT_ALL_ACCESS`) and is valid for the
        // duration of this call.
        let result = unsafe { TerminateJobObject(handle, 1) }.map_err(Error::Windows);

        // SAFETY: `handle` was opened by this function and is not used
        // after this point on any path; it must be closed here to avoid
        // leaking a handle on every call (e.g. every watchdog recovery).
        unsafe {
            let _ = CloseHandle(handle);
        }

        result?;
        Ok(())
    }
}

impl Drop for ShellJob {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `CreateJobObjectW` in
        // `create_and_join` and is owned exclusively by this `ShellJob`;
        // nothing else closes it, and this `Drop` impl runs at most once
        // per instance. Closing this process's handle does not terminate
        // the job (no `KILL_ON_JOB_CLOSE` is set) — see the struct doc
        // comment.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
