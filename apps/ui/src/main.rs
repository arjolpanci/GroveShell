//! `groveshell-ui`: first-iteration shell UI. Per `docs/PROJECT_PLAN.md`
//! §10.1/§10.2, creates a single top bar (Activities affordance only —
//! clock, workspace indicator, and system indicators are later scope) and
//! an Activities overview listing currently open top-level windows, sourced
//! from `groveshell-window-model::snapshot()`. The bar reserves its strip
//! of the work area via the AppBar API (`SHAppBarMessage`), the same
//! mechanism the Windows taskbar uses, so maximized windows and desktop
//! icon layout respect it instead of being covered. No hot corners,
//! hotkeys, or animations yet — those are deliberately deferred to keep
//! this slice small.

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;

    use groveshell_common::{Error, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, InvalidateRect, SetBkMode,
        SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::Shell::{
        SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    const ID_OVERVIEW_LISTBOX: i32 = 1002;
    const ID_OVERVIEW_CLOSE: i32 = 1003;
    /// Half of the original 32px guess — the Windows taskbar itself is
    /// ~40px, but this shell is meant to feel closer to GNOME's slim bar.
    const BAR_HEIGHT: i32 = 16;
    /// Hit-test region for the painted (not a native control — there isn't
    /// enough vertical room in a 16px bar for one) "Activities" label.
    const ACTIVITIES_LABEL_X: i32 = 8;
    const ACTIVITIES_LABEL_WIDTH: i32 = 72;

    /// All mutable UI state lives here, on the single UI thread that owns
    /// every window created below. `thread_local!` (rather than a `static`)
    /// makes that single-thread assumption explicit and avoids `unsafe`
    /// globals for what is, in a classic Win32 app, thread-affine state
    /// anyway (window procedures for these windows only ever run on the
    /// thread that created them).
    struct AppState {
        bar_hwnd: HWND,
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

            // Register the bar as a top-edge AppBar (the same mechanism the
            // Windows taskbar uses) so it reserves its strip of the work
            // area instead of just floating on top of maximized windows and
            // desktop icons.
            let bar_rect = register_appbar(bar_hwnd, screen_w, BAR_HEIGHT);
            let _ = MoveWindow(
                bar_hwnd,
                bar_rect.left,
                bar_rect.top,
                bar_rect.right - bar_rect.left,
                bar_rect.bottom - bar_rect.top,
                true,
            );

            // Overview: covers everything below the bar rather than the
            // whole screen, so the bar (and its Activities label) stays put
            // and visible while the overview is open — matching GNOME,
            // where the top bar is never covered by the overview it opens.
            let overview_y = bar_rect.bottom;
            let overview_h = screen_h - overview_y;
            let overview_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellOverview"),
                w!("GroveShell Activities"),
                WS_POPUP,
                0,
                overview_y,
                screen_w,
                overview_h,
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
                overview_h - 120,
                overview_hwnd,
                HMENU(ID_OVERVIEW_LISTBOX as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            STATE.with(|s| {
                *s.borrow_mut() = Some(AppState {
                    bar_hwnd,
                    overview_hwnd,
                    listbox_hwnd,
                    overview_visible: false,
                    listed_hwnds: Vec::new(),
                });
            });

            // The `MoveWindow` above may have already triggered and
            // consumed a `WM_PAINT` for the bar before `STATE` existed, in
            // which case `wndproc` fell back to `DefWindowProcW` and the
            // Activities label never actually got drawn. Force one more
            // repaint now that `STATE` is ready so the label always shows
            // up, regardless of how that first paint landed.
            let _ = InvalidateRect(bar_hwnd, None, true);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        Ok(())
    }

    /// Registers `bar_hwnd` as a top-edge AppBar and reserves a
    /// `bar_height`-tall strip of the primary monitor for it, returning the
    /// rect the system assigned (per `ABM_SETPOS` semantics, this is what
    /// the caller should actually move/resize the window to). Every other
    /// top-level window's maximize/work-area layout is recalculated by the
    /// system as a side effect, exactly as it is for the real taskbar.
    ///
    /// SAFETY: `bar_hwnd` must be a live window for the duration of this
    /// call; `SHAppBarMessage` only reads/writes through the `APPBARDATA`
    /// pointer for the duration of each call.
    unsafe fn register_appbar(bar_hwnd: HWND, screen_w: i32, bar_height: i32) -> RECT {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: bar_hwnd,
            ..Default::default()
        };
        SHAppBarMessage(ABM_NEW, &mut abd);

        abd.uEdge = ABE_TOP;
        abd.rc = RECT {
            left: 0,
            top: 0,
            right: screen_w,
            bottom: bar_height,
        };
        // ABM_QUERYPOS lets other appbars adjust the proposed rect (e.g. if
        // the Windows taskbar already sits at the top); our height is
        // fixed regardless, so only `bottom` is reasserted afterward.
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.bottom = abd.rc.top + bar_height;

        SHAppBarMessage(ABM_SETPOS, &mut abd);
        abd.rc
    }

    /// SAFETY: `bar_hwnd` was previously registered by [`register_appbar`];
    /// calling this after that registration is gone (e.g. twice) is a
    /// documented no-op on the shell's side, not undefined behavior.
    unsafe fn unregister_appbar(bar_hwnd: HWND) {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: bar_hwnd,
            ..Default::default()
        };
        SHAppBarMessage(ABM_REMOVE, &mut abd);
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

    /// Paints the "Activities" label directly onto the bar. There's no
    /// native `BUTTON` control here (unlike the overview's Close button) —
    /// at a 16px bar height a real push button's chrome leaves no room for
    /// legible text, so this is flat painted text hit-tested in
    /// `WM_LBUTTONUP` instead.
    fn paint_bar(hwnd: HWND) {
        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`, so
        // it's guaranteed valid for the duration of this call; `ps` is a
        // local that outlives the paired `BeginPaint`/`EndPaint` call.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00E0E0E0));
            let mut text: Vec<u16> = "Activities".encode_utf16().collect();
            let mut rect = RECT {
                left: ACTIVITIES_LABEL_X,
                top: 0,
                right: ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH,
                bottom: BAR_HEIGHT,
            };
            DrawTextW(
                hdc,
                &mut text,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_CENTER,
            );
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// Sets `overview_visible` with a short-lived borrow that never
    /// overlaps a Win32 call — see the note on [`toggle_overview`] about
    /// why that matters.
    fn set_overview_visible(visible: bool) {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.overview_visible = visible;
            }
        });
    }

    /// Shows the overview with a fresh window snapshot, or hides it if
    /// already visible — the Activities label is a toggle.
    ///
    /// No `STATE` borrow is ever held while calling `ShowWindow`,
    /// `SetForegroundWindow`, or `SetFocus`: those can synchronously
    /// deliver activation/paint messages back into `wndproc` on this same
    /// thread before returning (confirmed by a real crash during manual
    /// testing — `RefCell` panics on a nested borrow while its
    /// `borrow_mut()` from here was still outstanding). Every Win32 call
    /// below happens after the relevant borrow has already been dropped.
    fn toggle_overview() {
        let Some((overview_hwnd, listbox_hwnd, was_visible)) = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| (st.overview_hwnd, st.listbox_hwnd, st.overview_visible))
        }) else {
            return;
        };

        if was_visible {
            set_overview_visible(false);
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
            unsafe {
                let _ = ShowWindow(overview_hwnd, SW_HIDE);
            }
            return;
        }

        // SAFETY: `listbox_hwnd` is a valid, process-lifetime window.
        // `LB_RESETCONTENT`/`LB_ADDSTRING` go to the system-provided
        // ListBox window procedure, not ours, so they don't reenter
        // `wndproc`.
        unsafe {
            let _ = SendMessageW(listbox_hwnd, LB_RESETCONTENT, None, None);
        }

        let mut listed_hwnds = Vec::new();
        for window in groveshell_window_model::snapshot() {
            let label = match &window.exe_name {
                Some(exe) => format!("{exe} \u{2014} {}", window.title),
                None => window.title.clone(),
            };
            let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer for
            // the duration of this synchronous call; `LB_ADDSTRING` copies
            // the string internally before returning.
            unsafe {
                let _ = SendMessageW(
                    listbox_hwnd,
                    LB_ADDSTRING,
                    None,
                    LPARAM(wide.as_ptr() as isize),
                );
            }
            listed_hwnds.push(window.hwnd);
        }

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.listed_hwnds = listed_hwnds;
                state.overview_visible = true;
            }
        });

        // SAFETY: `overview_hwnd`/`listbox_hwnd` are valid, process-
        // lifetime windows.
        unsafe {
            let _ = ShowWindow(overview_hwnd, SW_SHOW);
            let _ = SetForegroundWindow(overview_hwnd);
            let _ = SetFocus(listbox_hwnd);
        }
    }

    /// Restores (if minimized) and focuses the window currently selected in
    /// the overview listbox, then closes the overview — the overview's
    /// entire reason to exist is "pick a window to switch to." Same
    /// borrow-then-release-then-call-Win32 shape as [`toggle_overview`],
    /// and for the same reason: `SetForegroundWindow(target)` deactivates
    /// whatever was previously foreground (our own overview window) via a
    /// synchronous message back into `wndproc`.
    fn activate_selected() {
        let Some((listbox_hwnd, overview_hwnd, listed_hwnds)) = STATE.with(|s| {
            s.borrow().as_ref().map(|st| {
                (
                    st.listbox_hwnd,
                    st.overview_hwnd,
                    st.listed_hwnds.clone(),
                )
            })
        }) else {
            return;
        };

        // SAFETY: `listbox_hwnd` is a valid, process-lifetime window.
        let selection = unsafe { SendMessageW(listbox_hwnd, LB_GETCURSEL, None, None) };
        if selection.0 < 0 {
            return;
        }

        let Some(&raw_hwnd) = listed_hwnds.get(selection.0 as usize) else {
            return;
        };
        let target = HWND(raw_hwnd as *mut c_void);

        // SAFETY: `target` was enumerated moments ago by `EnumWindows` in
        // `window-model::snapshot`; it may already have been destroyed by
        // the time we get here (the user picked it from a stale
        // snapshot), in which case these calls are documented
        // no-ops/failures rather than undefined behavior.
        unsafe {
            if IsIconic(target).as_bool() {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let _ = SetForegroundWindow(target);
        }

        set_overview_visible(false);
        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = ShowWindow(overview_hwnd, SW_HIDE);
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let is_bar =
            STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.bar_hwnd == hwnd));

        match msg {
            WM_PAINT if is_bar => {
                paint_bar(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP if is_bar => {
                let x = (lparam.0 & 0xFFFF) as i32;
                if (ACTIVITIES_LABEL_X..ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH).contains(&x) {
                    toggle_overview();
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let control_id = (wparam.0 & 0xFFFF) as i32;
                let notify_code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                match (control_id, notify_code) {
                    (ID_OVERVIEW_CLOSE, BN_CLICKED) => toggle_overview(),
                    (ID_OVERVIEW_LISTBOX, LBN_DBLCLK) => activate_selected(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                if is_bar {
                    unregister_appbar(hwnd);
                }
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
