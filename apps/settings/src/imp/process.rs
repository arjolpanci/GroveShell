//! Spawns and supervises `groveshell-watchdog`, `groveshell-host`, and
//! `groveshell-ui` — the same order `scripts/dev-start.ps1` uses for
//! development, now owned by this tray app for real end-user launches.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Resolves a sibling executable's path: the same directory this process's
/// own `.exe` lives in, joined with `name`. All four GroveShell binaries
/// are always built into the same `target/<profile>` directory and, in a
/// real install, would ship in the same install directory — there is no
/// scenario in this codebase where they live apart.
fn sibling_exe_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe should always resolve");
    path.pop();
    path.push(name);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_exe_path_is_alongside_the_current_exe() {
        let current = std::env::current_exe().unwrap();
        let expected_dir = current.parent().unwrap().to_path_buf();
        let resolved = sibling_exe_path("groveshell-ui.exe");
        assert_eq!(resolved.parent().unwrap(), expected_dir);
        assert_eq!(resolved.file_name().unwrap(), "groveshell-ui.exe");
    }
}

pub struct ManagedProcesses {
    watchdog: Option<Child>,
    host: Option<Child>,
    ui: Option<Child>,
}

impl ManagedProcesses {
    pub fn new() -> Self {
        Self { watchdog: None, host: None, ui: None }
    }

    /// Starts whichever of watchdog, host and ui is not running, in that
    /// order, the same sequence `scripts/dev-start.ps1` uses. Best-effort:
    /// a spawn failure is logged, not fatal, so a missing
    /// `groveshell-ui.exe` (a partial dev build) doesn't take the tray app
    /// down with it.
    ///
    /// Each process is checked on its own rather than the shell as a
    /// whole. The shell is usually already up when this runs, started by
    /// a script or an earlier Settings, and must not be started twice;
    /// but when only the UI has died, the host still answering used to
    /// count as "already running" and Start did nothing at all.
    pub fn spawn_all(&mut self) {
        if self.pid_of("watchdog").is_none() {
            self.watchdog = spawn_hidden("groveshell-watchdog.exe");
            std::thread::sleep(Duration::from_secs(1));
        }
        if self.pid_of("host").is_none() {
            self.host = spawn_hidden("groveshell-host.exe");
        }
        if self.pid_of("ui").is_none() {
            self.ui = spawn_hidden("groveshell-ui.exe");
        }
    }

    /// Alias used by the "Start GroveShell" tray/Home-page action — spawns
    /// fresh children regardless of any previous (now-exited) ones.
    pub fn start_all(&mut self) {
        self.spawn_all();
    }

    /// Whether the shell's UI is up, whoever started it.
    pub fn is_ui_running(&mut self) -> bool {
        self.pid_of("ui").is_some()
    }

    /// The process id of `name` (`"watchdog"`, `"host"` or `"ui"`), if
    /// it is running.
    ///
    /// A child this process spawned is asked directly. Otherwise the
    /// process list is searched by name: the shell is often already
    /// running when Settings starts, launched by a script or a previous
    /// Settings, and reporting it as stopped left the Home page offering
    /// to "Start" a shell that was plainly on screen, with no way to
    /// stop it.
    pub fn pid_of(&mut self, name: &str) -> Option<u32> {
        let child = match name {
            "watchdog" => self.watchdog.as_mut(),
            "host" => self.host.as_mut(),
            "ui" => self.ui.as_mut(),
            _ => return None,
        };
        if let Some(child) = child {
            if let Ok(None) = child.try_wait() {
                return Some(child.id());
            }
        }
        groveshell_common::processes::find(&format!("groveshell-{name}.exe"))
    }

    /// Stops `ui` gracefully (so it restores the real taskbar/work areas
    /// in its own `WM_DESTROY` handler), then asks `host`/`watchdog` to
    /// shut down over IPC, force-killing anything still alive after a
    /// short grace period. See `apps/ui/src/imp/mod.rs`'s `WM_DESTROY`
    /// handler and `scripts/dev-start.ps1`'s `Stop-UiGracefully` for the
    /// precedent this mirrors.
    pub fn stop_all(&mut self) {
        // The watchdog first. Left running, it sees the host go quiet
        // and "recovers" a shell that was being shut down on purpose.
        stop_via_ipc_or_kill(
            "groveshell-watchdog",
            self.watchdog.take(),
            groveshell_ipc::message_type::WATCHDOG_SHUTDOWN,
        );
        stop_ui_gracefully(self.ui.take());
        stop_via_ipc_or_kill("groveshell-host", self.host.take(), groveshell_ipc::message_type::SHUTDOWN);
    }
}

/// `EnumWindows` pass matching only a class name, returning the window
/// handle instead of just a bool (unlike `find_any_window_by_class`) — used
/// where the caller needs to post a message to the window it found.
fn find_any_window_by_class_hwnd(class_name: &str) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW};

    struct SearchState<'a> {
        class_name: &'a str,
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` was created from a live `&mut SearchState` in
        // the call below and this callback only runs synchronously within
        // that call's lifetime.
        let state = &mut *(lparam.0 as *mut SearchState);
        let mut buf = [0u16; 256];
        // SAFETY: `buf` outlives this call and is large enough for any
        // real window class name.
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        if name == state.class_name {
            state.found = Some(hwnd);
            return BOOL(0); // stop enumerating
        }
        TRUE
    }

    let mut state = SearchState { class_name, found: None };
    // SAFETY: `state`'s address is passed as `lparam` and only read back
    // by `enum_proc`, synchronously, within this call's lifetime.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut SearchState as isize));
    }
    state.found
}

fn spawn_hidden(exe_name: &str) -> Option<Child> {
    let path = sibling_exe_path(exe_name);
    match Command::new(&path).spawn() {
        Ok(child) => {
            tracing::info!(exe = exe_name, pid = child.id(), "spawned");
            Some(child)
        }
        Err(e) => {
            tracing::error!(exe = exe_name, error = ?e, "failed to spawn");
            None
        }
    }
}

/// Posts `WM_CLOSE` to the `GroveShellBar`-classed window (triggering
/// `ui`'s own taskbar-restore `WM_DESTROY` logic), then, if this process
/// tracks `ui` as a `Child`, waits up to 3 seconds for graceful exit before
/// force-killing it.
///
/// The window is found by class name alone rather than by `ui_child`'s pid:
/// `ui_child` is `None` whenever this `ManagedProcesses` instance adopted an
/// already-running shell instead of spawning it (see `spawn_all`'s doc
/// comment), and in that case there is no pid to match against — but the
/// `WM_CLOSE` still needs to reach the real running bar so the taskbar gets
/// restored. There is at most one `GroveShellBar` window system-wide, so
/// matching on class name alone is unambiguous.
fn stop_ui_gracefully(ui_child: Option<Child>) {
    if let Some(bar_hwnd) = find_any_window_by_class_hwnd("GroveShellBar") {
        // SAFETY: `bar_hwnd` was just found via `EnumWindows` and is a
        // plain message post with no ownership implications.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                bar_hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let Some(mut child) = ui_child else {
        // Not ours to wait on by handle, but the host must not be told
        // to go until the UI has finished putting the taskbar back.
        while std::time::Instant::now() < deadline
            && groveshell_common::processes::find("groveshell-ui.exe").is_some()
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        return;
    };
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Sends the shutdown message over `pipe_name` regardless of whether this
/// process tracks `child` — the IPC connection reaches whatever process is
/// actually listening on that named pipe, tracked or adopted (see
/// `stop_ui_gracefully`'s doc comment for why `child` can be `None` even
/// when the real process is alive). Only waits/force-kills when a tracked
/// `Child` is available to wait on.
fn stop_via_ipc_or_kill(pipe_name: &str, child: Option<Child>, shutdown_message_type: &str) {
    if let Ok(mut conn) = groveshell_ipc::pipe::connect(pipe_name) {
        let envelope = groveshell_ipc::Envelope::new("groveshell-settings", shutdown_message_type, serde_json::json!({}));
        let _ = groveshell_ipc::framing::write_envelope(&mut conn, &envelope);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let Some(mut child) = child else {
        // Not ours to wait on by handle. Wait by name instead, so the
        // caller's next look at what is running sees the result rather
        // than a process halfway out of the door.
        let exe = format!("{pipe_name}.exe");
        while std::time::Instant::now() < deadline && groveshell_common::processes::find(&exe).is_some() {
            std::thread::sleep(Duration::from_millis(100));
        }
        return;
    };
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

