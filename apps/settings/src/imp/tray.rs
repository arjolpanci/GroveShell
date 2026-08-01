//! The system tray icon and its right-click context menu.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadCursorW, LoadImageW, PostQuitMessage,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, IDC_ARROW, IMAGE_ICON, LR_DEFAULTSIZE,
    MF_STRING, MSG, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, WM_APP, WM_DESTROY,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::process::ManagedProcesses;

pub(crate) const WM_TRAYICON: u32 = WM_APP + 1;
const MENU_ID_OPEN: u32 = 1;
const MENU_ID_TOGGLE: u32 = 2;
const MENU_ID_EXIT: u32 = 3;

thread_local! {
    static PROCESSES: RefCell<Option<ManagedProcesses>> = const { RefCell::new(None) };
}

/// Loads the icon this exe embedded as resource ID 1 (see `build.rs`) at
/// the small size appropriate for a tray icon / window class icon.
fn load_app_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    // SAFETY: `GetModuleHandleW(None)` returns this process's own module
    // handle; resource ID 1 was embedded by this exe's own `build.rs`.
    unsafe {
        let hinstance = GetModuleHandleW(None).expect("own module handle always resolves");
        let hinstance: windows::Win32::Foundation::HINSTANCE = hinstance.into();
        let handle = LoadImageW(
            hinstance,
            windows::core::PCWSTR(1 as *const u16),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE,
        )
        .unwrap_or_default();
        windows::Win32::UI::WindowsAndMessaging::HICON(handle.0)
    }
}

/// Creates the tray icon and settings-window shell, then runs the message
/// loop for the process's lifetime — analogous to `apps/ui`'s own `main()`
/// message loop, but for this single hidden window plus the settings
/// window Task 8 creates alongside it.
pub fn run_message_loop(processes: ManagedProcesses) -> groveshell_common::Result<()> {
    PROCESSES.with(|p| *p.borrow_mut() = Some(processes));

    // SAFETY: every call below either has its own safety comment or is a
    // plain value/query with no aliasing or lifetime requirements.
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(groveshell_common::Error::Windows)?;
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellSettingsMain"),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(groveshell_common::Error::Windows)?,
            hbrBackground: CreateSolidBrush(COLORREF(0x00202020)),
            ..Default::default()
        };
        if windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&class) == 0 {
            return Err(groveshell_common::Error::Windows(windows::core::Error::from_win32()));
        }

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("GroveShellSettingsMain"),
            w!("GroveShell Settings"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(groveshell_common::Error::Windows)?;

        add_tray_icon(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn add_tray_icon(hwnd: HWND) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: load_app_icon(),
        ..Default::default()
    };
    let tip = "GroveShell\0".encode_utf16().collect::<Vec<_>>();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    // SAFETY: `data` is a fully-initialized, valid `NOTIFYICONDATAW` for
    // the duration of this synchronous call.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    // SAFETY: same contract as `add_tray_icon`.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn is_ui_running() -> bool {
    PROCESSES.with(|p| p.borrow_mut().as_mut().map(|proc| proc.is_ui_running()).unwrap_or(false))
}

/// Restore-Explorer (if `ui` is running) or Start-GroveShell (if it isn't)
/// — shared by the tray menu's "Toggle" item and Task 9's Home-page
/// button, both of which call this same function rather than duplicating
/// the stop/start logic or reposting a synthetic menu command.
pub(crate) fn toggle_groveshell() {
    let running = is_ui_running();
    PROCESSES.with(|p| {
        if let Some(proc) = p.borrow_mut().as_mut() {
            if running {
                proc.stop_all();
            } else {
                proc.start_all();
            }
        }
    });
}

fn show_context_menu(hwnd: HWND) {
    // SAFETY: standard synchronous popup-menu sequence, same shape as
    // `apps/ui/src/imp/dock.rs`'s `show_context_menu`.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_OPEN as usize, w!("Open GroveShell Settings"));
        let toggle_label = if is_ui_running() { w!("Restore Explorer") } else { w!("Start GroveShell") };
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_TOGGLE as usize, toggle_label);
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_EXIT as usize, w!("Exit GroveShell"));

        let mut point = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            point.x,
            point.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as u32 {
            MENU_ID_OPEN => super::window::open_settings_window(),
            MENU_ID_TOGGLE => toggle_groveshell(),
            MENU_ID_EXIT => {
                // Always stop, never start: `stop_all` is a safe no-op when
                // nothing is running (its callees early-return on a `None`
                // tracked child), unlike `toggle_groveshell`, which would
                // spawn fresh, now-unsupervised processes if GroveShell were
                // already stopped.
                PROCESSES.with(|p| {
                    if let Some(proc) = p.borrow_mut().as_mut() {
                        proc.stop_all();
                    }
                });
                PostQuitMessage(0);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam.0 as u32;
            if event == WM_LBUTTONUP {
                super::window::open_settings_window();
            } else if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
