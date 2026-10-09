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
use groveshell_ui_kit::rows::dropdown;
use groveshell_ui_kit::rows::input::{clamp_scroll, hit_test, next_focus, Hit};
use groveshell_ui_kit::rows::layout::{layout_page, PageLayout, MIN_CONTENT_WIDTH};
use groveshell_ui_kit::rows::{paint::paint_dropdown, paint::paint_page, Card, Control};
use groveshell_ui_kit::runtime::scaled;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE, VK_DOWN,
    VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, LoadCursorW,
    RegisterClassW,
    SetForegroundWindow, SetWindowPos, ShowWindow, IDC_ARROW, MINMAXINFO, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOW, WNDCLASSW, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_CAPTURECHANGED, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WS_EX_NOREDIRECTIONBITMAP,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
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
    /// Whether the focus ring should be drawn. Windows shows it for
    /// keyboard focus only: a ring that appears under every mouse click
    /// is noise, and its absence is how a pointer user can tell the
    /// keyboard is not driving.
    focus_visible: bool,
    hovered_row: Option<u32>,
    /// The choice row whose dropdown is open, if any. While this is set
    /// the list owns the mouse: it is hit-tested before the rows beneath
    /// it, and a click anywhere else closes it.
    open_choice: Option<u32>,
    hovered_option: Option<usize>,
    /// The slider row currently being dragged, while the mouse is
    /// captured. `Hit::SliderDrag` names a drag; without this it was
    /// only ever a single click at the press point.
    dragging_slider: Option<u32>,
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
        // bitmap so the DirectComposition content — and the Mica behind
        // it — is what reaches the screen. It is only safe once a surface
        // actually exists: a window without a redirection bitmap cannot
        // be painted by GDI at all, so a window that failed to get a
        // surface would be invisible rather than merely opaque. So build
        // it translucent, check, and rebuild it opaque if the surface
        // failed — the same two-attempt shape as `bar_gpu`'s bar window.
        // Deliberately not a tool window: this should behave like any
        // other application window, including in the shell's own
        // Activities overview.
        let make = |translucent: bool| {
            let ex = if translucent { WS_EX_NOREDIRECTIONBITMAP } else { Default::default() };
            CreateWindowExW(
                ex,
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
            )
        };

        let Ok(mut hwnd) = make(true) else { return };
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let mut surface = surface::create(hwnd, client.right, client.bottom);
        if surface.is_none() {
            tracing::warn!("settings window surface unavailable; rebuilding the window opaque");
            let _ = DestroyWindow(hwnd);
            let Ok(opaque) = make(false) else { return };
            hwnd = opaque;
            let _ = GetClientRect(hwnd, &mut client);
            surface = surface::create(hwnd, client.right, client.bottom);
        }

        STATE.with(|s| {
            *s.borrow_mut() = Some(WindowState {
                hwnd,
                surface,
                selected_nav: 0,
                scroll: 0,
                focused_row: None,
                focus_visible: false,
                hovered_row: None,
                open_choice: None,
                hovered_option: None,
                dragging_slider: None,
                tracking_mouse: false,
            });
        });

        refresh_system_appearance();
        material::apply(hwnd, material::Surface::Window);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Re-reads everything the design tokens resolve against.
///
/// The high-contrast flag is config, not a system setting, and nothing
/// else in this process pushes it: without this the shell would repaint
/// in the high-contrast palette while the window holding that very
/// switch stayed in light/dark.
fn refresh_system_appearance() {
    color::refresh_theme();
    color::refresh_accent();
    groveshell_ui_kit::runtime::set_high_contrast(
        super::config_store::current().appearance.high_contrast,
    );
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
        // `CreateWindowExW` takes physical pixels, and this process is
        // per-monitor DPI aware, so the logical default has to be scaled
        // for the monitor it will open on. Without this the window opens
        // at half size on a 200% display — small enough that
        // `WM_GETMINMAXINFO` clamps it to the minimum.
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x.max(96);

        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            let width = scaled(WINDOW_WIDTH, dpi).min(work.right - work.left);
            let height = scaled(WINDOW_HEIGHT, dpi).min(work.bottom - work.top);
            let x = work.left + ((work.right - work.left) - width) / 2;
            let y = work.top + ((work.bottom - work.top) - height) / 2;
            (x, y, width, height)
        } else {
            (200, 200, scaled(WINDOW_WIDTH, dpi), scaled(WINDOW_HEIGHT, dpi))
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

    let (selected, scroll, focused, hovered, open_choice, hovered_option) = STATE.with(|s| {
        let state = s.borrow();
        let Some(st) = state.as_ref() else { return (0, 0, None, None, None, None) };
        (
            st.selected_nav,
            st.scroll,
            st.focus_visible.then_some(st.focused_row).flatten(),
            st.hovered_row,
            st.open_choice,
            st.hovered_option,
        )
    });

    let (cards, layout) = page_layout(client, dpi, selected);
    let scroll = clamp_scroll(scroll, layout.content_height, client.bottom - content_rect(client, dpi).top);

    // A surface can go missing mid-session: a resize whose recreate
    // failed, or device loss. Without this the window would stay blank
    // for the rest of the session.
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            if st.surface.is_none() && groveshell_ui_kit::gpu::is_enabled() {
                st.surface = surface::create(hwnd, client.right, client.bottom);
                if st.surface.is_some() {
                    tracing::info!("settings DirectComposition surface recovered");
                }
            }
        }
    });

    // The surface is moved out for the duration of the paint and put
    // back afterwards: `surface::paint` runs a closure that reaches back
    // into the page code, which may read `STATE`, and holding the borrow
    // across that would panic.
    let surface = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.surface.take()));
    let open_popup = open_choice.and_then(|id| {
        let options = match control_of(&cards, id) {
            Some(Control::Choice { options, selected }) => (options, selected),
            _ => return None,
        };
        let control = control_rect_of(&cards, &layout, id)?;
        let popup = dropdown::popup_rect(shift_down(control, scroll), client, options.0.len(), dpi);
        Some((options.0, options.1, popup))
    });

    surface::paint(hwnd, surface.as_ref(), client, dpi, |canvas, client| {
        paint_chrome(canvas, client, dpi, selected);
        paint_page(canvas, &cards, &layout, content_rect(client, dpi), focused, hovered, scroll, dpi);
        if let Some((options, chosen, popup)) = open_popup {
            paint_dropdown(canvas, options, chosen, hovered_option, popup, dpi);
        }
    });
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.surface = surface;
            st.scroll = scroll;
        }
    });
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
                        //
                        // Drop the old surface and its target FIRST. A
                        // window can hold only one DirectComposition
                        // target, and Rust evaluates the right-hand side
                        // before dropping the old value, so assigning
                        // straight over this would ask for a second
                        // target on the same HWND, fail, and leave the
                        // window permanently unpainted — with
                        // `WS_EX_NOREDIRECTIONBITMAP`, invisible.
                        // `apps/ui`'s desktop dock hit exactly this.
                        st.surface = None;
                        st.surface = surface::create(hwnd, width, height);
                        if st.surface.is_none() {
                            tracing::warn!(
                                "settings surface recreate failed after resize; retrying on next paint"
                            );
                        }
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
                    // A list anchored to a row that is about to move must
                    // not stay open over the wrong one.
                    st.open_choice = None;
                    st.hovered_option = None;
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

            // A drag in progress owns the mouse: track the value under
            // the cursor even when it has left the track's rectangle.
            let dragging = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.dragging_slider));
            if let Some(id) = dragging {
                let (selected, scroll) = STATE
                    .with(|s| s.borrow().as_ref().map_or((0, 0), |st| (st.selected_nav, st.scroll)));
                let (cards, layout) = page_layout(client, dpi, selected);
                if let (Some(track), Some(Control::Slider { min, max, .. })) =
                    (control_rect_of(&cards, &layout, id), control_of(&cards, id))
                {
                    let track = shift_down(track, scroll);
                    let width = (track.right - track.left).max(1) as f32;
                    let fraction = ((x - track.left) as f32 / width).clamp(0.0, 1.0);
                    set_value(selected, id, min + fraction * (max - min));
                    repaint(hwnd);
                }
                return LRESULT(0);
            }
            let (selected, scroll) =
                STATE.with(|s| s.borrow().as_ref().map_or((0, 0), |st| (st.selected_nav, st.scroll)));
            let (cards, layout) = page_layout(client, dpi, selected);
            // While a list is open the cursor highlights its options,
            // not the rows underneath.
            let open = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.open_choice));
            if let Some(id) = open {
                let option = match (
                    open_popup_rect(hwnd, &cards, &layout, id, scroll, dpi),
                    control_of(&cards, id),
                ) {
                    (Some(popup), Some(Control::Choice { options, .. })) => {
                        dropdown::option_at(popup, options.len(), x, y, dpi)
                    }
                    _ => None,
                };
                let changed = STATE.with(|s| {
                    let mut state = s.borrow_mut();
                    let Some(st) = state.as_mut() else { return false };
                    if st.hovered_option == option {
                        return false;
                    }
                    st.hovered_option = option;
                    true
                });
                if changed {
                    repaint(hwnd);
                }
                return LRESULT(0);
            }

            let hovered = if y < content_rect(client, dpi).top {
                None
            } else {
                match hit_test(&cards, &layout, x, y, scroll) {
                    Hit::Row(id) | Hit::SliderDrag { id, .. } => Some(id),
                    Hit::None => None,
                }
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

        WM_LBUTTONUP => {
            let was_dragging = STATE.with(|s| {
                s.borrow_mut().as_mut().and_then(|st| st.dragging_slider.take()).is_some()
            });
            if was_dragging {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }

        // Capture can be taken away without a button-up (an alt-tab, a
        // system dialog); the drag must end with it rather than persist
        // and swallow every later mouse move.
        WM_CAPTURECHANGED => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.dragging_slider = None;
                }
            });
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

            // An open dropdown is hit-tested before anything else: it
            // is drawn over the rows, so it must take the clicks that
            // land on it, and a click anywhere else dismisses it.
            let open = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.open_choice));
            if let Some(id) = open {
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let (selected, scroll) = STATE
                    .with(|s| s.borrow().as_ref().map_or((0, 0), |st| (st.selected_nav, st.scroll)));
                let (cards, layout) = page_layout(client, dpi, selected);
                if let (Some(popup), Some(Control::Choice { options, .. })) =
                    (open_popup_rect(hwnd, &cards, &layout, id, scroll, dpi), control_of(&cards, id))
                {
                    if let Some(index) = dropdown::option_at(popup, options.len(), x, y, dpi) {
                        set_value(selected, id, index as f32);
                    }
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.open_choice = None;
                            st.hovered_option = None;
                        }
                    });
                    repaint(hwnd);
                    // The click is spent either way: on the option it
                    // picked, or on dismissing the list. It must not also
                    // act on whatever row sits underneath.
                    return LRESULT(0);
                }
            }

            if let Some(index) = nav_hit_test(x, y, dpi) {
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.selected_nav = index;
                        st.scroll = 0;
                        st.focused_row = None;
                        st.open_choice = None;
                        st.hovered_option = None;
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

            // Rows scrolled up behind the page-title band are still in
            // the layout; a click up there must not reach them.
            if y < content_rect(client, dpi).top {
                return LRESULT(0);
            }

            match hit_test(&cards, &layout, x, y, scroll) {
                Hit::SliderDrag { id, value } => {
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.focused_row = Some(id);
                            st.focus_visible = false;
                            st.dragging_slider = Some(id);
                        }
                    });
                    SetCapture(hwnd);
                    set_value(selected, id, value);
                }
                Hit::Row(id) => {
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.focused_row = Some(id);
                            st.focus_visible = false;
                        }
                    });
                    // A choice row opens its list; every other row
                    // acts immediately.
                    match control_of(&cards, id) {
                        Some(Control::Choice { .. }) => {
                            STATE.with(|s| {
                                if let Some(st) = s.borrow_mut().as_mut() {
                                    st.open_choice = Some(id);
                                    st.hovered_option = None;
                                }
                            });
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
            let (selected, focused) = STATE
                .with(|s| s.borrow().as_ref().map_or((0, None), |st| (st.selected_nav, st.focused_row)));
            let cards = cards_for(selected);
            let key = wparam.0 as u16;

            let open = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.open_choice));
            if let Some(id) = open {
                if key == VK_ESCAPE.0 {
                    STATE.with(|s| {
                        if let Some(st) = s.borrow_mut().as_mut() {
                            st.open_choice = None;
                            st.hovered_option = None;
                        }
                    });
                    repaint(hwnd);
                    return LRESULT(0);
                }
                if let Some(Control::Choice { options, selected: current }) = control_of(&cards, id) {
                    if key == VK_UP.0 || key == VK_DOWN.0 {
                        let count = options.len().max(1);
                        let delta = if key == VK_UP.0 { count - 1 } else { 1 };
                        set_value(selected, id, ((current + delta) % count) as f32);
                        repaint(hwnd);
                        return LRESULT(0);
                    }
                    if key == VK_RETURN.0 || key == VK_SPACE.0 {
                        STATE.with(|s| {
                            if let Some(st) = s.borrow_mut().as_mut() {
                                st.open_choice = None;
                                st.hovered_option = None;
                            }
                        });
                        repaint(hwnd);
                        return LRESULT(0);
                    }
                }
            }

            if key == VK_TAB.0 {
                let shift = GetKeyState(
                    VK_SHIFT.0 as i32,
                ) as u16
                    & 0x8000
                    != 0;
                let next = next_focus(&cards, focused, !shift);
                focus_row(hwnd, selected, next);
                return LRESULT(0);
            }

            if key == VK_UP.0 || key == VK_DOWN.0 {
                // With a row focused the arrows walk the rows, which is
                // where the keyboard already is. With nothing focused
                // they move between pages.
                if focused.is_some() {
                    let next = next_focus(&cards, focused, key == VK_DOWN.0);
                    focus_row(hwnd, selected, next);
                    return LRESULT(0);
                }
                let delta = if key == VK_UP.0 { -1 } else { 1 };
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.selected_nav = next_nav(st.selected_nav, delta);
                        st.scroll = 0;
                        st.focused_row = None;
                        st.open_choice = None;
                    }
                });
                repaint(hwnd);
                return LRESULT(0);
            }

            if let Some(id) = focused {
                if key == VK_SPACE.0 || key == VK_RETURN.0 {
                    match control_of(&cards, id) {
                        Some(Control::Choice { .. }) => {
                            STATE.with(|s| {
                                if let Some(st) = s.borrow_mut().as_mut() {
                                    st.open_choice = Some(id);
                                    st.hovered_option = None;
                                }
                            });
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
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        // Review Focus 3: the system theme or accent changed while the
        // window is open. Re-read both and repaint, the same way
        // `apps/ui` reacts to the broadcast, so an open window never
        // keeps a stale palette.
        WM_SETTINGCHANGE => {
            refresh_system_appearance();
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

/// Moves focus to `row`, scrolling it into view.
///
/// Focus that lands below the fold with no scroll is focus the user
/// cannot see — the focus rectangle would be drawn off-screen.
fn focus_row(hwnd: HWND, page: usize, row: Option<u32>) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let mut client = RECT::default();
    // SAFETY: `hwnd` is a live, process-lifetime window.
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let (cards, layout) = page_layout(client, dpi, page);
    let content = content_rect(client, dpi);
    let viewport = client.bottom - content.top;

    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(st) = state.as_mut() else { return };
        st.focused_row = row;
        st.focus_visible = true;
        st.open_choice = None;
        st.hovered_option = None;

        let Some(id) = row else { return };
        let Some(rect) = control_rect_of(&cards, &layout, id) else { return };
        // `rect` is in content coordinates; the visible band is
        // `scroll .. scroll + viewport` measured from `content.top`.
        let top = rect.top - content.top;
        let bottom = rect.bottom - content.top;
        if top < st.scroll {
            st.scroll = top;
        } else if bottom > st.scroll + viewport {
            st.scroll = bottom - viewport;
        }
        st.scroll = clamp_scroll(st.scroll, layout.content_height, viewport);
    });
    repaint(hwnd);
}

/// A rect moved into window coordinates: the layout is computed in
/// content coordinates, which the scroll offset shifts up.
fn shift_down(rect: RECT, scroll: i32) -> RECT {
    RECT { top: rect.top - scroll, bottom: rect.bottom - scroll, ..rect }
}

/// The on-screen control rect of `id`, in content coordinates.
fn control_rect_of(cards: &[Card], layout: &PageLayout, id: u32) -> Option<RECT> {
    for (card_index, (_, row_rects)) in layout.cards.iter().enumerate() {
        let card = cards.get(card_index)?;
        for (row_index, rects) in row_rects.iter().enumerate() {
            if card.rows.get(row_index).is_some_and(|r| r.id == id) {
                return Some(rects.control);
            }
        }
    }
    None
}

/// The popup rect for the currently open choice, if one is open.
fn open_popup_rect(hwnd: HWND, cards: &[Card], layout: &PageLayout, id: u32, scroll: i32, dpi: u32) -> Option<RECT> {
    let Some(Control::Choice { options, .. }) = control_of(cards, id) else { return None };
    let control = control_rect_of(cards, layout, id)?;
    let mut client = RECT::default();
    // SAFETY: `hwnd` is a live, process-lifetime window.
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    Some(dropdown::popup_rect(shift_down(control, scroll), client, options.len(), dpi))
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
