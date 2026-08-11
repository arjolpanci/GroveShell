//! The bar's session / power menu (spec §7). A native `TrackPopupMenu`
//! rather than a custom-painted flyout: power actions are exactly the kind
//! of small, rarely-used list a native menu handles reliably, and it keeps
//! this to correct-by-inspection code instead of a bespoke window. The
//! destructive actions (sign out, restart, shut down) route through a
//! `MessageBox` confirm first.
//!
//! Settings is deliberately absent — the bar's gear already opens it.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, MessageBoxW, SetForegroundWindow, TrackPopupMenu,
    IDYES, MB_ICONWARNING, MB_YESNO, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionAction {
    Lock,
    SignOut,
    Sleep,
    Restart,
    ShutDown,
}

/// Whether an action needs an "are you sure?" confirm before running — the
/// ones that close the user's apps or power the machine off.
pub(crate) fn needs_confirm(action: SessionAction) -> bool {
    matches!(
        action,
        SessionAction::SignOut | SessionAction::Restart | SessionAction::ShutDown
    )
}

fn action_from_id(id: u32) -> Option<SessionAction> {
    match id {
        1 => Some(SessionAction::Lock),
        2 => Some(SessionAction::Sleep),
        3 => Some(SessionAction::SignOut),
        4 => Some(SessionAction::Restart),
        5 => Some(SessionAction::ShutDown),
        _ => None,
    }
}

fn confirm_label(action: SessionAction) -> PCWSTR {
    match action {
        SessionAction::SignOut => w!("Sign out now? Unsaved work in open apps may be lost."),
        SessionAction::Restart => w!("Restart now? Unsaved work in open apps may be lost."),
        SessionAction::ShutDown => w!("Shut down now? Unsaved work in open apps may be lost."),
        _ => w!(""),
    }
}

/// Pops the session menu at the cursor, anchored to `owner`, and runs the
/// chosen action (with a confirm for destructive ones).
pub(crate) fn show(owner: HWND) {
    // SAFETY: standard menu construction + teardown; `owner` is a live
    // shell window. `TrackPopupMenu` with `TPM_RETURNCMD` runs its own
    // modal loop and returns the selected command id (0 if dismissed).
    unsafe {
        let mut pt = POINT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);

        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(_) => return,
        };
        let _ = AppendMenuW(menu, MF_STRING, 1, w!("Lock"));
        let _ = AppendMenuW(menu, MF_STRING, 2, w!("Sleep"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("Sign out"));
        let _ = AppendMenuW(menu, MF_STRING, 4, w!("Restart"));
        let _ = AppendMenuW(menu, MF_STRING, 5, w!("Shut down"));

        // The menu needs the foreground so it dismisses correctly on an
        // outside click (documented TrackPopupMenu requirement).
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);

        let Some(action) = action_from_id(chosen.0 as u32) else {
            return;
        };
        if needs_confirm(action) {
            let answer = MessageBoxW(owner, confirm_label(action), w!("GroveShell"), MB_YESNO | MB_ICONWARNING);
            if answer != IDYES {
                return;
            }
        }
        execute(action);
    }
}

/// Runs a session action. Errors are logged, never silently swallowed.
pub(crate) fn execute(action: SessionAction) {
    match action {
        SessionAction::Lock => {
            // SAFETY: no preconditions.
            unsafe {
                let _ = windows::Win32::System::Shutdown::LockWorkStation();
            }
        }
        SessionAction::Sleep => {
            // SAFETY: plain power request; `false` args = sleep (not
            // hibernate), don't force, not wakeup-only.
            unsafe {
                let ok = windows::Win32::System::Power::SetSuspendState(false, false, false);
                if !ok.as_bool() {
                    tracing::warn!("SetSuspendState (sleep) failed");
                }
            }
        }
        SessionAction::SignOut => exit_windows(windows::Win32::System::Shutdown::EWX_LOGOFF),
        SessionAction::Restart => {
            if acquire_shutdown_privilege() {
                exit_windows(windows::Win32::System::Shutdown::EWX_REBOOT);
            }
        }
        SessionAction::ShutDown => {
            if acquire_shutdown_privilege() {
                exit_windows(windows::Win32::System::Shutdown::EWX_SHUTDOWN);
            }
        }
    }
}

fn exit_windows(flags: windows::Win32::System::Shutdown::EXIT_WINDOWS_FLAGS) {
    use windows::Win32::System::Shutdown::{ExitWindowsEx, SHUTDOWN_REASON};
    // SAFETY: `ExitWindowsEx` initiates logoff/shutdown for the session;
    // no pointers involved. Failure (e.g. a veto by another app) is logged.
    unsafe {
        if ExitWindowsEx(flags, SHUTDOWN_REASON(0)).is_err() {
            tracing::warn!(?flags, "ExitWindowsEx failed");
        }
    }
}

/// Enables `SE_SHUTDOWN_NAME` on this process's token, required before
/// `ExitWindowsEx` may reboot or power off. Returns whether it succeeded.
fn acquire_shutdown_privilege() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: opens this process's own token; `token` is written by the
    // call and closed on every path below.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return false;
        }

        let mut luid = LUID::default();
        let ok = LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid).is_ok();
        let mut applied = false;
        if ok {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            applied = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok();
        }
        let _ = CloseHandle(token);
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_actions_need_confirm() {
        assert!(needs_confirm(SessionAction::ShutDown));
        assert!(needs_confirm(SessionAction::Restart));
        assert!(needs_confirm(SessionAction::SignOut));
    }

    #[test]
    fn safe_actions_do_not_need_confirm() {
        assert!(!needs_confirm(SessionAction::Lock));
        assert!(!needs_confirm(SessionAction::Sleep));
    }

    #[test]
    fn menu_ids_map_back_to_their_actions() {
        assert_eq!(action_from_id(1), Some(SessionAction::Lock));
        assert_eq!(action_from_id(2), Some(SessionAction::Sleep));
        assert_eq!(action_from_id(3), Some(SessionAction::SignOut));
        assert_eq!(action_from_id(4), Some(SessionAction::Restart));
        assert_eq!(action_from_id(5), Some(SessionAction::ShutDown));
        assert_eq!(action_from_id(0), None);
        assert_eq!(action_from_id(99), None);
    }
}
