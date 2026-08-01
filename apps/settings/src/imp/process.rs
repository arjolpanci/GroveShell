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

    /// Spawns watchdog -> host -> ui in order, same sequence
    /// `scripts/dev-start.ps1` uses. Best-effort: a spawn failure is
    /// logged, not fatal, so e.g. a missing `groveshell-ui.exe` (a
    /// dev-only partial build) doesn't crash the tray app itself.
    pub fn spawn_all(&mut self) {
        self.watchdog = spawn_hidden("groveshell-watchdog.exe");
        std::thread::sleep(Duration::from_secs(1));
        self.host = spawn_hidden("groveshell-host.exe");
        self.ui = spawn_hidden("groveshell-ui.exe");
    }

    /// Alias used by the "Start GroveShell" tray/Home-page action — spawns
    /// fresh children regardless of any previous (now-exited) ones.
    pub fn start_all(&mut self) {
        self.spawn_all();
    }

    pub fn is_ui_running(&mut self) -> bool {
        matches!(self.ui.as_mut().map(|c| c.try_wait()), Some(Ok(None)))
    }

    pub fn pid_of(&self, name: &str) -> Option<u32> {
        let child = match name {
            "watchdog" => self.watchdog.as_ref(),
            "host" => self.host.as_ref(),
            "ui" => self.ui.as_ref(),
            _ => None,
        }?;
        Some(child.id())
    }

    /// Stops `ui` gracefully (so it restores the real taskbar/work areas
    /// in its own `WM_DESTROY` handler), then asks `host`/`watchdog` to
    /// shut down over IPC, force-killing anything still alive after a
    /// short grace period. See `apps/ui/src/imp/mod.rs`'s `WM_DESTROY`
    /// handler and `scripts/dev-start.ps1`'s `Stop-UiGracefully` for the
    /// precedent this mirrors.
    pub fn stop_all(&mut self) {
        stop_ui_gracefully(self.ui.take());
        stop_via_ipc_or_kill("groveshell-host", self.host.take(), groveshell_ipc::message_type::SHUTDOWN);
        stop_via_ipc_or_kill(
            "groveshell-watchdog",
            self.watchdog.take(),
            groveshell_ipc::message_type::WATCHDOG_SHUTDOWN,
        );
    }
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

/// Posts `WM_CLOSE` to the `GroveShellBar`-classed window belonging to
/// `ui_child`'s pid (triggering `ui`'s own taskbar-restore `WM_DESTROY`
/// logic), waits up to 3 seconds for graceful exit, then force-kills.
fn stop_ui_gracefully(ui_child: Option<Child>) {
    let Some(mut child) = ui_child else { return };
    let pid = child.id();

    if let Some(bar_hwnd) = find_window_by_class_and_pid("GroveShellBar", pid) {
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

fn stop_via_ipc_or_kill(pipe_name: &str, child: Option<Child>, shutdown_message_type: &str) {
    let Some(mut child) = child else { return };

    if let Ok(mut conn) = groveshell_ipc::pipe::connect(pipe_name) {
        let envelope = groveshell_ipc::Envelope::new("groveshell-settings", shutdown_message_type, serde_json::json!({}));
        let _ = groveshell_ipc::framing::write_envelope(&mut conn, &envelope);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
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

/// `EnumWindows` pass matching both class name and owning pid — the same
/// two-part match `scripts/dev-start.ps1`'s `Stop-UiGracefully` performs
/// via .NET interop, ported to a direct Win32 call here.
fn find_window_by_class_and_pid(class_name: &str, pid: u32) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId};

    struct SearchState<'a> {
        class_name: &'a str,
        pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` was created from a live `&mut SearchState` in
        // the call below and this callback only runs synchronously within
        // that call's lifetime.
        let state = &mut *(lparam.0 as *mut SearchState);
        let mut window_pid = 0u32;
        // SAFETY: `hwnd` is supplied live by `EnumWindows`.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
        if window_pid != state.pid {
            return TRUE;
        }
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

    let mut state = SearchState { class_name, pid, found: None };
    // SAFETY: `state`'s address is passed as `lparam` and only read back by
    // `enum_proc`, synchronously, within this call's lifetime.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut SearchState as isize));
    }
    state.found
}
