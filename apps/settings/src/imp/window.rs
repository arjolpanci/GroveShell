//! The settings window: a left-hand nav list plus the currently selected
//! page's content pane, both owner-drawn.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect,
    MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, IsWindowVisible, LoadCursorW,
    RegisterClassW, SetForegroundWindow, ShowWindow, IDC_ARROW, SW_RESTORE, SW_SHOW, WM_DESTROY,
    WM_LBUTTONDOWN, WM_PAINT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use super::nav::{nav_hit_test, nav_layout, NAV_ITEMS};
use super::pages::Page;
use super::pages::dock::DockPage;
use super::pages::home::HomePage;
use super::pages::input::InputPage;
use super::pages::overview::OverviewPage;
use super::pages::top_bar::TopBarPage;
use super::theme::{ACCENT, BG_NAV, BG_WINDOW, DIVIDER, HEADER_HEIGHT, NAV_WIDTH, PAGE_MARGIN, TEXT};
use super::util_text::{draw_left_text, draw_title_text};

thread_local! {
    static WINDOW_HWND: RefCell<Option<HWND>> = const { RefCell::new(None) };
    static SELECTED_NAV: RefCell<usize> = const { RefCell::new(0) };
    static HOME_PAGE: RefCell<HomePage> = RefCell::new(HomePage::new());
    static DOCK_PAGE: RefCell<DockPage> = RefCell::new(DockPage::new());
    static TOP_BAR_PAGE: RefCell<TopBarPage> = RefCell::new(TopBarPage::new());
    static OVERVIEW_PAGE: RefCell<OverviewPage> = RefCell::new(OverviewPage::new());
    static INPUT_PAGE: RefCell<InputPage> = RefCell::new(InputPage::new());
}

const WINDOW_WIDTH: i32 = 780;
/// The header band and card margins (see `card_rect`) eat into the space
/// every page's own row math was tuned against before this window grew a
/// title/card layout — grown by exactly that much so no page's rows (the
/// Input page's four hot-corner rows in particular, tuned to just fit the
/// previous 520px) get pushed past the bottom edge.
const WINDOW_HEIGHT: i32 = 520 + HEADER_HEIGHT + PAGE_MARGIN * 2;

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

        // No `WS_EX_TOOLWINDOW`: that style makes `groveshell_window_model`
        // exclude this window from tracking entirely (see
        // `crates/window-model/src/lib.rs`'s `inspect`), so it never got
        // assigned to a workspace (workspace switches didn't park/unpark
        // it — it just sat wherever it was, on top of whatever workspace
        // happened to be current) and never appeared as a card in the
        // Activities overview. A plain overlapped window is what real
        // application windows use and is what this should behave like.
        let (x, y) = spawn_position();
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("GroveShellSettingsWindow"),
            w!("GroveShell Settings"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
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

/// Top-left corner to create the window at: centered on whichever
/// monitor the cursor is currently over, not a fixed absolute-screen
/// point. A fixed `(200, 200)` is always on the *primary* monitor's
/// virtual-screen origin regardless of where the user actually is —
/// opening Settings while working on a second monitor made it appear to
/// jump to the other monitor entirely. Falls back to the primary
/// monitor if the cursor position can't be read (documented-never-fails
/// in practice, but `GetCursorPos` is technically fallible).
fn spawn_position() -> (i32, i32) {
    // SAFETY: plain geometry queries, no preconditions.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let hmonitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            let work = info.rcWork;
            let x = work.left + ((work.right - work.left) - WINDOW_WIDTH).max(0) / 2;
            let y = work.top + ((work.bottom - work.top) - WINDOW_HEIGHT).max(0) / 2;
            (x, y)
        } else {
            (200, 200)
        }
    }
}

fn content_rect(client: RECT) -> RECT {
    RECT { left: NAV_WIDTH, top: 0, right: client.right, bottom: client.bottom }
}

/// The page-title band at the top of `content` (see `theme::HEADER_HEIGHT`).
fn header_rect(content: RECT) -> RECT {
    RECT { left: content.left, top: content.top, right: content.right, bottom: content.top + HEADER_HEIGHT }
}

/// The grouped card every page paints its rows onto — `content` below the
/// title band, inset by `PAGE_MARGIN` on every side. Passed to `Page::paint`/
/// `Page::on_click` as their `content_rect`, so a page's own row math (each
/// already starts with its own internal padding) lands inside the card
/// without any page needing to know about the header or margin itself.
fn card_rect(content: RECT) -> RECT {
    RECT {
        left: content.left + PAGE_MARGIN,
        top: content.top + HEADER_HEIGHT + PAGE_MARGIN,
        right: content.right - PAGE_MARGIN,
        bottom: content.bottom - PAGE_MARGIN,
    }
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
            // Hairline separating the nav rail from the content area —
            // otherwise the two flat fills abut with no visible edge.
            let nav_divider = RECT { left: NAV_WIDTH, top: 0, right: NAV_WIDTH + 1, bottom: client.bottom };
            let divider_brush = CreateSolidBrush(DIVIDER);
            FillRect(hdc, &nav_divider, divider_brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(divider_brush);

            let selected = SELECTED_NAV.with(|s| *s.borrow());
            for (i, rect) in nav_layout().into_iter().enumerate() {
                if i == selected {
                    // An inset selection pill plus a left accent bar —
                    // the same "current page" language Windows 11's own
                    // nav rail uses, instead of a flat full-width fill
                    // that reads as a hover state rather than "current".
                    let pill = RECT { left: rect.left + 8, top: rect.top + 3, right: rect.right - 8, bottom: rect.bottom - 3 };
                    super::theme::fill_round_rect(hdc, pill, 8, windows::Win32::Foundation::COLORREF(0x00203A52));
                    let accent_bar = RECT { left: 0, top: rect.top + 8, right: 3, bottom: rect.bottom - 8 };
                    let accent_brush = CreateSolidBrush(ACCENT);
                    FillRect(hdc, &accent_bar, accent_brush);
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(accent_brush);
                }
                let label_rect = RECT { left: rect.left + 20, top: rect.top, right: rect.right - 8, bottom: rect.bottom };
                draw_left_text(hdc, label_rect, NAV_ITEMS[i], TEXT);
            }

            let content = content_rect(client);
            let header = header_rect(content);
            let title_rect = RECT { left: header.left + PAGE_MARGIN, top: header.top, right: header.right - PAGE_MARGIN, bottom: header.bottom };
            draw_title_text(hdc, title_rect, NAV_ITEMS[selected], TEXT);

            let card = card_rect(content);
            super::theme::draw_card(hdc, card);
            match selected {
                0 => HOME_PAGE.with(|p| p.borrow().paint(hdc, card)),
                1 => DOCK_PAGE.with(|p| p.borrow().paint(hdc, card)),
                2 => TOP_BAR_PAGE.with(|p| p.borrow().paint(hdc, card)),
                3 => OVERVIEW_PAGE.with(|p| p.borrow().paint(hdc, card)),
                4 => INPUT_PAGE.with(|p| p.borrow().paint(hdc, card)),
                _ => {}
            }

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
                let card = card_rect(content_rect(client));
                let selected = SELECTED_NAV.with(|s| *s.borrow());
                match selected {
                    0 => HOME_PAGE.with(|p| p.borrow_mut().on_click(x, y, card)),
                    1 => DOCK_PAGE.with(|p| p.borrow_mut().on_click(x, y, card)),
                    2 => TOP_BAR_PAGE.with(|p| p.borrow_mut().on_click(x, y, card)),
                    3 => OVERVIEW_PAGE.with(|p| p.borrow_mut().on_click(x, y, card)),
                    4 => INPUT_PAGE.with(|p| p.borrow_mut().on_click(x, y, card)),
                    _ => {}
                }
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
