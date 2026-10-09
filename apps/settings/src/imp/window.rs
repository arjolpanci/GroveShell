//! The settings window: a navigation rail and a scrolling column of
//! setting rows, drawn through the shell's own design system.
//!
//! The window owns no layout maths of its own. Each page hands over a
//! list of cards (see `pages::Page`), `rows::layout_page` turns that into
//! rectangles, and paint, hit-testing, hover and keyboard traversal all
//! read those same rectangles — so what you see, what you can click and
//! what Tab reaches cannot drift apart.

use std::cell::RefCell;

use groveshell_ui_kit::canvas::Canvas;
use groveshell_ui_kit::design::{color, material, typography};
use groveshell_ui_kit::gpu::GpuSurface;
use groveshell_ui_kit::rows::input::{clamp_scroll, hit_test, next_focus, Hit};
use groveshell_ui_kit::rows::layout::{layout_page, PageLayout, MIN_CONTENT_WIDTH};
use groveshell_ui_kit::rows::{paint::paint_page, Card, Control};
use groveshell_ui_kit::runtime::scaled;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE, VK_DOWN, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, LoadCursorW, RegisterClassW,
    SetForegroundWindow, SetWindowPos, ShowWindow, IDC_ARROW, MINMAXINFO, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOW, WNDCLASSW, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE,
};

use super::nav::{nav_hit_test, nav_layout, next_nav, NAV_ITEMS, NAV_WIDTH};
use super::pages::accessibility::AccessibilityPage;
use super::pages::dock::DockPage;
use super::pages::home::HomePage;
use super::pages::input::InputPage;
use super::pages::overview::OverviewPage;
use super::pages::top_bar::TopBarPage;
use super::pages::Page;
use super::surface;

/// Everything the window draws from. A plain thread-local struct rather
/// than a `RefCell` consulted from inside paint: the message handlers
/// below take what they need and drop the borrow before calling any Win32
/// function that could re-enter `wndproc`.
struct WindowState {
    hwnd: HWND,
    surface: Option<GpuSurface>,
    selected_nav: usize,
    scroll: i32,
    focused_row: Option<u32>,
    hovered_row: Option<u32>,
    tracking_mouse: bool,
}

thread_local! {
    static STATE: RefCell<Option<WindowState>> = const { RefCell::new(None) };
    static HOME_PAGE: RefCell<HomePage> = RefCell::new(HomePage::new());
    static DOCK_PAGE: RefCell<DockPage> = RefCell::new(DockPage::new());
    static TOP_BAR_PAGE: RefCell<TopBarPage> = RefCell::new(TopBarPage::new());
    static OVERVIEW_PAGE: RefCell<OverviewPage> = RefCell::new(OverviewPage::new());
    static INPUT_PAGE: RefCell<InputPage> = RefCell::new(InputPage::new());
    static ACCESSIBILITY_PAGE: RefCell<AccessibilityPage> = RefCell::new(AccessibilityPage::new());
}

/// Default size, in logical pixels: wide enough for the rail plus a full
/// content column, tall enough for a page of rows without scrolling.
const WINDOW_WIDTH: i32 = 1000;
const WINDOW_HEIGHT: i32 = 700;
/// Below this the window refuses to shrink: the content column has a
/// minimum of its own, and the rail does not collapse.
const MIN_WINDOW_HEIGHT: i32 = 400;
/// The page title band above the content column.
const HEADER_HEIGHT: i32 = 56;

/// Reads the current page's cards without holding a borrow across
/// anything that could re-enter.
fn cards_for(index: usize) -> Vec<Card> {
    match index {
        0 => HOME_PAGE.with(|p| p.borrow().cards()),
        1 => DOCK_PAGE.with(|p| p.borrow().cards()),
        2 => TOP_BAR_PAGE.with(|p| p.borrow().cards()),
        3 => OVERVIEW_PAGE.with(|p| p.borrow().cards()),
        4 => INPUT_PAGE.with(|p| p.borrow().cards()),
        5 => ACCESSIBILITY_PAGE.with(|p| p.borrow().cards()),
        _ => Vec::new(),
    }
}

fn activate(index: usize, id: u32) {
    match index {
        0 => HOME_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        1 => DOCK_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        2 => TOP_BAR_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        3 => OVERVIEW_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        4 => INPUT_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        5 => ACCESSIBILITY_PAGE.with(|p| p.borrow_mut().on_activate(id)),
        _ => {}
    }
}

fn set_value(index: usize, id: u32, value: f32) {
    match index {
        0 => HOME_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        1 => DOCK_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        2 => TOP_BAR_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        3 => OVERVIEW_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        4 => INPUT_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        5 => ACCESSIBILITY_PAGE.with(|p| p.borrow_mut().on_value(id, value)),
        _ => {}
    }
}

/// The area the content column lives in: right of the rail, below the
/// page title.
fn content_rect(client: RECT, dpi: u32) -> RECT {
    RECT {
        left: scaled(NAV_WIDTH, dpi),
        top: scaled(HEADER_HEIGHT, dpi),
        right: client.right,
        bottom: client.bottom,
    }
}

fn page_layout(client: RECT, dpi: u32, index: usize) -> (Vec<Card>, PageLayout) {
    let cards = cards_for(index);
    let layout = layout_page(&cards, content_rect(client, dpi), dpi);
    (cards, layout)
}

pub(crate) fn open_settings_window() {
    let existing = STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd));
    if let Some(hwnd) = existing {
        // SAFETY: `hwnd` is a live, process-lifetime window.
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    // SAFETY: every call below is either a plain query or has its own
    // safety comment; the class name and strings are static literals.
    unsafe {
        let hinstance = GetModuleHandleW(None).expect("own module handle always resolves");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellSettingsWindow"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // No background brush: the window paints every pixel itself,
            // and a class brush would fill the Mica backdrop with a flat
            // color before the first frame.
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let (x, y, width, height) = spawn_rect();
        // `WS_EX_NOREDIRECTIONBITMAP` drops the opaque GDI redirection
        // surface so the DirectComposition content — and the Mica behind
        // it — is what reaches the screen. Deliberately not a tool window:
        // this should behave like any other application window, including
        // in the shell's own Activities overview.
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("GroveShellSettingsWindow"),
            w!("GroveShell Settings"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
            width,
            height,
            None,
            None,
            hinstance,
            None,
        );
        let Ok(hwnd) = hwnd else { return };

        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);

        STATE.with(|s| {
            *s.borrow_mut() = Some(WindowState {
                hwnd,
                surface: surface::create(hwnd, client.right, client.bottom),
                selected_nav: 0,
                scroll: 0,
                focused_row: None,
                hovered_row: None,
                tracking_mouse: false,
            });
        });

        color::refresh_theme();
        color::refresh_accent();
        material::apply(hwnd, material::Surface::Window);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = scaled(0, dpi); // dpi is read again per paint; this keeps the call honest
    }
}

/// Centred on whichever monitor the cursor is on, so opening Settings
/// while working on a second monitor doesn't throw the window onto the
/// primary one.
fn spawn_rect() -> (i32, i32, i32, i32) {
    // SAFETY: plain geometry queries, no preconditions.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            let width = WINDOW_WIDTH.min(work.right - work.left);
            let height = WINDOW_HEIGHT.min(work.bottom - work.top);
            let x = work.left + ((work.right - work.left) - width) / 2;
            let y = work.top + ((work.bottom - work.top) - height) / 2;
            (x, y, width, height)
        } else {
            (200, 200, WINDOW_WIDTH, WINDOW_HEIGHT)
        }
    }
}

fn repaint(hwnd: HWND) {
    // SAFETY: `hwnd` is a live, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn paint_window(hwnd: HWND) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let mut client = RECT::default();
    // SAFETY: `hwnd` is the window currently handling `WM_PAINT`.
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }

    let (selected, scroll, focused, hovered) = STATE.with(|s| {
        let state = s.borrow();
        let Some(st) = state.as_ref() else { return (0, 0, None, None) };
        (st.selected_nav, st.scroll, st.focused_row, st.hovered_row)
    });

    let (cards, layout) = page_layout(client, dpi, selected);
    let scroll = clamp_scroll(scroll, layout.content_height, client.bottom - content_rect(client, dpi).top);

    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.surface.take().map(|surface| {
                s_restore(st, surface);
            });
        }
    });

    let surface_taken = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.surface.take()));
    surface::paint(hwnd, surface_taken.as_ref(), client, dpi, |canvas, client| {
        paint_chrome(canvas, client, dpi, selected);
        paint_page(canvas, &cards, &layout, focused, hovered, scroll, dpi);
    });
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.surface = surface_taken;
            st.scroll = scroll;
        }
    });
}

/// Puts a surface back after a paint that didn't happen.
fn s_restore(state: &mut WindowState, surface: GpuSurface) {
    state.surface = Some(surface);
}

/// The window's own chrome: the backdrop fill, the nav rail and the page
/// title.
fn paint_chrome(canvas: &mut dyn Canvas, client: RECT, dpi: u32, selected: usize) {
    // On the Direct2D path the surface was cleared to alpha 0, so this
    // fill is what gives the content area its tint over the Mica; the
    // rail is left unpainted so the backdrop shows through it, the way
    // Windows 11 Settings does.
    canvas.fill_rect(
        RECT { left: scaled(NAV_WIDTH, dpi), ..client },
        COLORREF(color::surface_base()),
    );

    for (index, rect) in nav_layout(dpi).into_iter().enumerate() {
        let item = &NAV_ITEMS[index];
        if index == selected {
            let pill = RECT {
                left: rect.left + scaled(8, dpi),
                top: rect.top + scaled(2, dpi),
                right: rect.right - scaled(8, dpi),
                bottom: rect.bottom - scaled(2, dpi),
            };
            canvas.fill_round_rect(pill, scaled(4, dpi), COLORREF(color::surface_overlay()));
            // The accent bar that marks the current page in every
            // Windows 11 nav rail.
            canvas.fill_round_rect(
                RECT {
                    left: rect.left + scaled(2, dpi),
                    top: rect.top + scaled(10, dpi),
                    right: rect.left + scaled(5, dpi),
                    bottom: rect.bottom - scaled(10, dpi),
                },
                scaled(2, dpi),
                COLORREF(color::accent()),
            );
        }

        canvas.set_text_color(COLORREF(color::text()));
        let glyph_size = scaled(16, dpi);
        canvas.glyph(
            RECT {
                left: rect.left + scaled(20, dpi),
                top: (rect.top + rect.bottom) / 2 - glyph_size / 2,
                right: rect.left + scaled(20, dpi) + glyph_size,
                bottom: (rect.top + rect.bottom) / 2 + glyph_size / 2,
            },
            item.glyph,
        );
        canvas.set_font_size(typography::BODY_PX);
        canvas.text(
            RECT { left: rect.left + scaled(48, dpi), ..rect },
            item.label,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
    }

    canvas.set_text_color(COLORREF(color::text()));
    canvas.set_font_size(typography::SUBTITLE_PX);
    canvas.text(
        RECT {
            left: scaled(NAV_WIDTH, dpi) + scaled(24, dpi),
            top: scaled(12, dpi),
            right: client.right,
            bottom: scaled(HEADER_HEIGHT, dpi),
        },
        NAV_ITEMS[selected.min(NAV_ITEMS.len() - 1)].label,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The window paints every pixel of its client area itself.
        WM_ERASEBKGND => LRESULT(1),

        WM_PAINT => {
            paint_window(hwnd);
            LRESULT(0)
        }

        WM_GETMINMAXINFO => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let info = lparam.0 as *mut MINMAXINFO;
            if !info.is_null() {
                (*info).ptMinTrackSize.x = scaled(NAV_WIDTH + MIN_CONTENT_WIDTH, dpi);
                (*info).ptMinTrackSize.y = scaled(MIN_WINDOW_HEIGHT, dpi);
            }
            LRESULT(0)
        }

        WM_SIZE => {
            let width = (lparam.0 & 0xFFFF) as i32;
            let height = ((lparam.0 >> 16) & 0xFFFF) as i32;
            if width > 0 && height > 0 {
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        // Recreated rather than resized: a composition
                        // surface is created at a fixed size.
                        st.surface = surface::create(hwnd, width, height);
                    }
                });
            }
            repaint(hwnd);
            LRESULT(0)
        }

        WM_DPICHANGED => {
            // lParam carries the rect Windows wants the window moved to;
            // taking it is what keeps the window the same physical size
            // across a monitor with a different scale factor.
            let suggested = lparam.0 as *const RECT;
            if !suggested.is_null() {
                let r = *suggested;
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            repaint(hwnd);
            LRESULT(0)
        }

        WM_MOUSEWHEEL => {
            let notches = ((wparam.0 >> 16) as u16 as i16) as i32 / 120;
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let selected = STATE.with(|s| s.borrow().as_ref().map_or(0, |st| st.selected_nav));
            let (_, layout) = page_layout(client, dpi, selected);
            let viewport = client.bottom - content_rect(client, dpi).top;
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.scroll = clamp_scroll(
                        st.scroll - notches * scaled(48, dpi),
                        layout.content_height,
                        viewport,
                    );
                }
            });
            repaint(hwnd);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let (selected, scroll) =
                STATE.with(|s| s.borrow().as_ref().map_or((0, 0), |st| (st.selected_nav, st.scroll)));
            let (cards, layout) = page_layout(client, dpi, selected);
            let hovered = match hit_test(&cards, &layout, x, y, scroll) {
                Hit::Row(id) | Hit::SliderDrag { id, .. } => Some(id),
                Hit::None => None,
            };

            let changed = STATE.with(|s| {
                let mut state = s.borrow_mut();
                let Some(st) = state.as_mut() else { return false };
                if !st.tracking_mouse {
                    st.tracking_mouse = true;
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                }
                if st.hovered_row == hovered {
                    return false;
                }
                st.hovered_row = hovered;
                true
            });
            if changed {
                repaint(hwnd);
            }
            LRESULT(0)
        }

        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.tracking_mouse = false;
                    st.hovered_row = None;
                }
            });
            repaint(hwnd);
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let dpi = GetDpiForWindow(hwnd).max(96);

            if let Some(index) = nav_hit_test(x, y, dpi) {
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.selected_nav = index;
                        st.scroll = 0;
                        st.focused_row = None;
                    }
                });
                repaint(hwnd);
                return LRESULT(0);
            }

            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let (selected, scroll) =
                STATE.with(|s| s.borrow().as_ref().map_or((0, 0), |st| (st.selected_nav, st.scroll)));
            let (cards, layout) = page_layout(client, dpi, selected);

            match hit_test(&cards, &layout, x, y, scroll) {
                Hit::SliderDrag { id, value } => {
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.focused_row = Some(id);
                        }
                    });
                    set_value(selected, id, value);
                }
                Hit::Row(id) => {
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.focused_row = Some(id);
                        }
                    });
                    // A choice row cycles to its next option on click,
                    // which is the whole interaction until the dropdown
                    // itself exists.
                    match control_of(&cards, id) {
                        Some(Control::Choice { options, selected: current }) => {
                            let next = (current + 1) % options.len().max(1);
                            set_value(selected, id, next as f32);
                        }
                        _ => activate(selected, id),
                    }
                }
                Hit::None => {}
            }
            repaint(hwnd);
            LRESULT(0)
        }

        WM_KEYDOWN => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let (selected, focused) = STATE
                .with(|s| s.borrow().as_ref().map_or((0, None), |st| (st.selected_nav, st.focused_row)));
            let cards = cards_for(selected);
            let key = wparam.0 as u16;

            if key == VK_TAB.0 {
                let shift = GetKeyState(
                    VK_SHIFT.0 as i32,
                ) as u16
                    & 0x8000
                    != 0;
                let next = next_focus(&cards, focused, !shift);
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.focused_row = next;
                    }
                });
                repaint(hwnd);
                return LRESULT(0);
            }

            if key == VK_UP.0 || key == VK_DOWN.0 {
                let delta = if key == VK_UP.0 { -1 } else { 1 };
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.selected_nav = next_nav(st.selected_nav, delta);
                        st.scroll = 0;
                        st.focused_row = None;
                    }
                });
                repaint(hwnd);
                return LRESULT(0);
            }

            if let Some(id) = focused {
                if key == VK_SPACE.0 || key == VK_RETURN.0 {
                    match control_of(&cards, id) {
                        Some(Control::Choice { options, selected: current }) => {
                            let next = (current + 1) % options.len().max(1);
                            set_value(selected, id, next as f32);
                        }
                        _ => activate(selected, id),
                    }
                    repaint(hwnd);
                    return LRESULT(0);
                }
                if key == VK_LEFT.0 || key == VK_RIGHT.0 {
                    if let Some(Control::Slider { value, min, max, .. }) = control_of(&cards, id) {
                        let step = (max - min) / 20.0;
                        let delta = if key == VK_LEFT.0 { -step } else { step };
                        set_value(selected, id, (value + delta).clamp(min, max));
                        repaint(hwnd);
                        return LRESULT(0);
                    }
                }
            }
            let _ = (dpi, client);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        // Review Focus 3: the system theme or accent changed while the
        // window is open. Re-read both and repaint, the same way
        // `apps/ui` reacts to the broadcast, so an open window never
        // keeps a stale palette.
        WM_SETTINGCHANGE => {
            color::refresh_theme();
            color::refresh_accent();
            material::apply(hwnd, material::Surface::Window);
            repaint(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        WM_DESTROY => {
            STATE.with(|s| *s.borrow_mut() = None);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The control belonging to `id` on the current page, cloned out so no
/// borrow is held while it is acted on.
fn control_of(cards: &[Card], id: u32) -> Option<Control> {
    cards
        .iter()
        .flat_map(|c| c.rows.iter())
        .find(|r| r.id == id)
        .map(|r| r.control.clone())
}
