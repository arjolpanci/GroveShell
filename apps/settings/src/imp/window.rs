//! The settings window: a left-hand nav list plus the currently selected
//! page's content pane, both owner-drawn.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, IsWindowVisible, LoadCursorW,
    RegisterClassW, SetForegroundWindow, ShowWindow, IDC_ARROW, SW_RESTORE, SW_SHOW, WM_DESTROY,
    WM_LBUTTONDOWN, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use super::nav::{nav_hit_test, nav_layout, NAV_ITEMS};
use super::pages::Page;
use super::pages::home::HomePage;
use super::theme::{BG_NAV, BG_WINDOW, NAV_WIDTH};
use super::util_text::draw_centered_text;

thread_local! {
    static WINDOW_HWND: RefCell<Option<HWND>> = const { RefCell::new(None) };
    static SELECTED_NAV: RefCell<usize> = const { RefCell::new(0) };
    static HOME_PAGE: RefCell<HomePage> = RefCell::new(HomePage::new());
}

const WINDOW_WIDTH: i32 = 780;
const WINDOW_HEIGHT: i32 = 520;

pub(crate) fn open_settings_window() {
    let existing = WINDOW_HWND.with(|w| *w.borrow());
    if let Some(hwnd) = existing {
        // SAFETY: `hwnd` is a still-registered class-level window for the
        // process lifetime; showing/foregrounding an existing window is a
        // plain, always-safe Win32 call.
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    // SAFETY: every call below either has its own safety comment or is a
    // plain value/query with no aliasing or lifetime requirements.
    unsafe {
        let hinstance = GetModuleHandleW(None).expect("own module handle always resolves");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellSettingsWindow"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(windows::Win32::Foundation::COLORREF(BG_WINDOW)),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("GroveShellSettingsWindow"),
            w!("GroveShell Settings"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            hinstance,
            None,
        );
        if let Ok(hwnd) = hwnd {
            WINDOW_HWND.with(|w| *w.borrow_mut() = Some(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, 1, 2000, None);
        }
    }
}

fn content_rect(client: RECT) -> RECT {
    RECT { left: NAV_WIDTH, top: 0, right: client.right, bottom: client.bottom }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);

            let nav_rect = RECT { left: 0, top: 0, right: NAV_WIDTH, bottom: client.bottom };
            let nav_brush = CreateSolidBrush(BG_NAV);
            FillRect(hdc, &nav_rect, nav_brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(nav_brush);

            let selected = SELECTED_NAV.with(|s| *s.borrow());
            for (i, rect) in nav_layout().into_iter().enumerate() {
                if i == selected {
                    super::theme::fill_round_rect(hdc, rect, 0, windows::Win32::Foundation::COLORREF(0x00404040));
                }
                draw_centered_text(hdc, rect, NAV_ITEMS[i], super::theme::TEXT);
            }

            let content = content_rect(client);
            if selected == 0 {
                HOME_PAGE.with(|p| p.borrow().paint(hdc, content));
            }
            // Tasks 15-18 add the remaining `selected == 1..=4` arms here.

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            if let Some(index) = nav_hit_test(x, y) {
                SELECTED_NAV.with(|s| *s.borrow_mut() = index);
            } else {
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let content = content_rect(client);
                let selected = SELECTED_NAV.with(|s| *s.borrow());
                if selected == 0 {
                    HOME_PAGE.with(|p| p.borrow_mut().on_click(x, y, content));
                }
                // Tasks 15-18 add the remaining `selected == 1..=4` arms here.
            }
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER => {
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW_HWND.with(|w| *w.borrow_mut() = None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub(crate) fn is_settings_window_open() -> bool {
    WINDOW_HWND.with(|w| {
        w.borrow()
            .map(|hwnd| unsafe { IsWindowVisible(hwnd) }.as_bool())
            .unwrap_or(false)
    })
}
