//! `groveshell-ui`: first-iteration shell UI. Per `docs/PROJECT_PLAN.md`
//! §10.1/§10.2, creates a single top bar (Activities button only — clock,
//! workspace indicator, and system indicators are later scope) and an
//! Activities overview listing currently open top-level windows, sourced
//! from `groveshell-window-model::snapshot()`. No hot corners, hotkeys,
//! work-area reservation (AppBar), or animations yet — those are
//! deliberately deferred to keep this slice small.

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;

    use groveshell_common::{Error, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const ID_ACTIVITIES_BUTTON: i32 = 1001;
    const ID_OVERVIEW_LISTBOX: i32 = 1002;
    const ID_OVERVIEW_CLOSE: i32 = 1003;
    const BAR_HEIGHT: i32 = 32;

    /// All mutable UI state lives here, on the single UI thread that owns
    /// every window created below. `thread_local!` (rather than a `static`)
    /// makes that single-thread assumption explicit and avoids `unsafe`
    /// globals for what is, in a classic Win32 app, thread-affine state
    /// anyway (window procedures for these windows only ever run on the
    /// thread that created them).
    struct AppState {
        overview_hwnd: HWND,
        listbox_hwnd: HWND,
        overview_visible: bool,
        /// Index-aligned with the listbox items currently shown, so
        /// `LB_GETCURSEL`'s result can be turned back into an `HWND`.
        listed_hwnds: Vec<isize>,
    }

    thread_local! {
        static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("ui")?;
        tracing::info!("groveshell-ui starting");

        let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
        tracing::info!("joined shell job object");

        // SAFETY: every Win32 call below either has a call-site safety
        // comment or is a plain value/query with no aliasing or lifetime
        // requirements (e.g. `GetSystemMetrics`).
        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(Error::Windows)?;
            let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

            register_class(hinstance, w!("GroveShellBar"), Some(wndproc))?;
            register_class(hinstance, w!("GroveShellOverview"), Some(wndproc))?;

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);

            // Bar: WS_EX_TOOLWINDOW keeps it out of the taskbar/alt-tab and
            // (as a side effect) out of its own Activities listing, since
            // `window-model::snapshot` excludes tool windows.
            let bar_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellBar"),
                w!("GroveShell"),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                screen_w,
                BAR_HEIGHT,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Activities"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                8,
                4,
                96,
                BAR_HEIGHT - 8,
                bar_hwnd,
                HMENU(ID_ACTIVITIES_BUTTON as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            // Overview: same class of popup window, but starts hidden and
            // covers the whole primary monitor. Multi-monitor support is
            // out of scope for this first iteration (per §9's phasing,
            // that hardening comes later).
            let overview_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellOverview"),
                w!("GroveShell Activities"),
                WS_POPUP,
                0,
                0,
                screen_w,
                screen_h,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Close"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                screen_w - 96,
                16,
                80,
                28,
                overview_hwnd,
                HMENU(ID_OVERVIEW_CLOSE as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            let listbox_hwnd = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("LISTBOX"),
                PCWSTR::null(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_VSCROLL
                    | WINDOW_STYLE((LBS_NOTIFY | LBS_HASSTRINGS) as u32),
                40,
                64,
                screen_w - 80,
                screen_h - 120,
                overview_hwnd,
                HMENU(ID_OVERVIEW_LISTBOX as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            STATE.with(|s| {
                *s.borrow_mut() = Some(AppState {
                    overview_hwnd,
                    listbox_hwnd,
                    overview_visible: false,
                    listed_hwnds: Vec::new(),
                });
            });

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        Ok(())
    }

    /// SAFETY: `wndproc` must be a valid `WNDPROC`-compatible function
    /// pointer for the lifetime of the registered class, which holds for
    /// the whole process lifetime here (classes are never unregistered).
    unsafe fn register_class(
        hinstance: windows::Win32::Foundation::HINSTANCE,
        class_name: PCWSTR,
        wndproc: WNDPROC,
    ) -> Result<()> {
        let class = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: hinstance,
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(Error::Windows)?,
            hbrBackground: CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00202020)),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(Error::Windows(windows::core::Error::from_win32()));
        }
        Ok(())
    }

    /// Shows the overview with a fresh window snapshot, or hides it if
    /// already visible — the Activities button is a toggle.
    fn toggle_overview() {
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };

            if state.overview_visible {
                hide_overview(state);
                return;
            }

            // SAFETY: `listbox_hwnd` and `overview_hwnd` were created in
            // `main` and live for the process lifetime.
            unsafe {
                let _ = SendMessageW(state.listbox_hwnd, LB_RESETCONTENT, None, None);
            }
            state.listed_hwnds.clear();

            for window in groveshell_window_model::snapshot() {
                let label = match &window.exe_name {
                    Some(exe) => format!("{exe} \u{2014} {}", window.title),
                    None => window.title.clone(),
                };
                let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer
                // for the duration of this synchronous call; `LB_ADDSTRING`
                // copies the string internally before returning.
                unsafe {
                    let _ = SendMessageW(
                        state.listbox_hwnd,
                        LB_ADDSTRING,
                        None,
                        LPARAM(wide.as_ptr() as isize),
                    );
                }
                state.listed_hwnds.push(window.hwnd);
            }

            // SAFETY: `overview_hwnd`/`listbox_hwnd` are valid, process-
            // lifetime windows.
            unsafe {
                let _ = ShowWindow(state.overview_hwnd, SW_SHOW);
                let _ = SetForegroundWindow(state.overview_hwnd);
                let _ = SetFocus(state.listbox_hwnd);
            }
            state.overview_visible = true;
        });
    }

    fn hide_overview(state: &mut AppState) {
        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = ShowWindow(state.overview_hwnd, SW_HIDE);
        }
        state.overview_visible = false;
    }

    /// Restores (if minimized) and focuses the window currently selected in
    /// the overview listbox, then closes the overview — the overview's
    /// entire reason to exist is "pick a window to switch to."
    fn activate_selected() {
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };

            // SAFETY: `listbox_hwnd` is a valid, process-lifetime window.
            let selection = unsafe { SendMessageW(state.listbox_hwnd, LB_GETCURSEL, None, None) };
            if selection.0 < 0 {
                return;
            }

            let Some(&raw_hwnd) = state.listed_hwnds.get(selection.0 as usize) else {
                return;
            };
            let target = HWND(raw_hwnd as *mut c_void);

            // SAFETY: `target` was enumerated moments ago by `EnumWindows`
            // in `window-model::snapshot`; it may already have been
            // destroyed by the time we get here (the user picked it from a
            // stale snapshot), in which case these calls are documented
            // no-ops/failures rather than undefined behavior.
            unsafe {
                if IsIconic(target).as_bool() {
                    let _ = ShowWindow(target, SW_RESTORE);
                }
                let _ = SetForegroundWindow(target);
            }

            hide_overview(state);
        });
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COMMAND => {
                let control_id = (wparam.0 & 0xFFFF) as i32;
                let notify_code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                match (control_id, notify_code) {
                    (ID_ACTIVITIES_BUTTON, BN_CLICKED) => toggle_overview(),
                    (ID_OVERVIEW_CLOSE, BN_CLICKED) => toggle_overview(),
                    (ID_OVERVIEW_LISTBOX, LBN_DBLCLK) => activate_selected(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-ui is Windows-only.");
    std::process::exit(1);
}
