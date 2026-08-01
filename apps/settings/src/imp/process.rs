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
    ///
    /// Idempotent: if GroveShell already appears to be running (started
    /// some other way — most commonly `scripts/dev-start.ps1` during
    /// development, which this process didn't spawn and so wouldn't
    /// otherwise know about), this does nothing instead of spawning a
    /// second, duplicate watchdog/host/ui trio on top of the first. This
    /// matters because `apps/ui`'s top-bar settings button launches this
    /// exe unconditionally to reach the settings window, regardless of
    /// how GroveShell was originally started.
    ///
    /// Known limitation: when GroveShell was already running before this
    /// process started, its `watchdog`/`host`/`ui` children aren't
    /// tracked by this `ManagedProcesses` (this process never spawned
    /// them), so `pid_of`/`is_ui_running` will under-report them as "not
    /// running" even though they are — the Home page's health display and
    /// the tray's "Restore Explorer"/"Start GroveShell" label can be
    /// stale in that case. Not attempted here: doing better would need
    /// discovering and adopting pre-existing processes by pid, which is
    /// out of scope for "don't spawn duplicates."
    pub fn spawn_all(&mut self) {
        if groveshell_already_running() {
            tracing::info!("groveshell already running (started elsewhere); not spawning a duplicate set");
            return;
        }
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

    pub fn pid_of(&mut self, name: &str) -> Option<u32> {
        let child = match name {
            "watchdog" => self.watchdog.as_mut(),
            "host" => self.host.as_mut(),
            "ui" => self.ui.as_mut(),
            _ => None,
        }?;
        match child.try_wait() {
            Ok(None) => Some(child.id()),
            _ => None,
        }
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

/// Best-effort check for "is GroveShell already up, however it was
/// started." A live `GroveShellBar` window is the most direct signal
/// (`apps/ui` only creates one once it's fully running); a reachable
/// `host.ping` is a fallback in case `ui` is mid-startup and hasn't
/// created its bar yet. Either signal is enough — `watchdog`/`host`/`ui`
/// are always started together (by this process or by
/// `scripts/dev-start.ps1`), so there's no realistic case where exactly
/// one of them is up on its own.
fn groveshell_already_running() -> bool {
    find_any_window_by_class("GroveShellBar") || super::health::host_ping_ok(Duration::from_millis(300))
}

/// `EnumWindows` pass matching only a class name, ignoring which process
/// owns it — used to detect "is GroveShell's bar up at all," unlike
/// `find_window_by_class_and_pid`'s more specific "is *this* pid's
/// window up," which only works for processes this `ManagedProcesses`
/// itself spawned.
fn find_any_window_by_class(class_name: &str) -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW};

    struct SearchState<'a> {
        class_name: &'a str,
        found: bool,
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
            state.found = true;
            return BOOL(0); // stop enumerating
        }
        TRUE
    }

    let mut state = SearchState { class_name, found: false };
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
