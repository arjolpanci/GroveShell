//! The hidden-icons panel under the bar's chevron.
//!
//! The bar shows the notification icons the user has switched on in
//! Windows Settings. Every other icon an application has registered is
//! here: a small grid that drops from the chevron, where each icon takes
//! the same clicks it would in the bar.
//!
//! This used to show Explorer's own overflow window instead, moved under
//! the chevron. On current Windows 11 that window is only the shell of a
//! XAML flyout, and showing it shows nothing: Explorer fills it in when
//! its own chevron is pressed, and its taskbar is hidden. The icons are
//! all in `tray_icons` already, since the shell hosts the notification
//! area, so the panel is drawn from there.

use std::cell::Cell;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, RoundRect,
    SelectObject, SetBkMode, SetTextColor, GetStockObject, NULL_PEN, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW,
    SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SW_HIDE, WA_INACTIVE, WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use super::state::scaled;

/// Not in the `windows` crate's `WindowsAndMessaging` module.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// One icon's cell, the icon drawn in it, and the panel's inner margin,
/// at 96 DPI.
const CELL_DP: i32 = 40;
const ICON_DP: i32 = 20;
const PADDING_DP: i32 = 8;
/// Icons per row before the panel grows downward instead.
const MAX_COLUMNS: usize = 5;
/// The panel's width when it has only a line of text to show.
const EMPTY_WIDTH_DP: i32 = 200;

/// While open, how often the panel checks whether its icons have changed.
const REFRESH_TIMER: usize = 1;
const REFRESH_MS: u32 = 500;

/// A click on the chevron while the panel is open first takes the
/// foreground away, which closes the panel, and then arrives at the bar
/// as a click on the chevron, which would open it again. A toggle this
/// soon after such a close is that same click.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

thread_local! {
    static WINDOW: Cell<isize> = const { Cell::new(0) };
    static HOVER: Cell<Option<usize>> = const { Cell::new(None) };
    static SHOWN_COUNT: Cell<usize> = const { Cell::new(0) };
    static CLOSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Where the panel hangs from: the chevron's screen rect and the
    /// monitor's left and right edges.
    static ANCHOR: Cell<(RECT, i32, i32)> = const {
        Cell::new((RECT { left: 0, top: 0, right: 0, bottom: 0 }, 0, 0))
    };
}

/// Columns and rows for `count` icons.
pub(crate) fn grid(count: usize) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let columns = count.min(MAX_COLUMNS);
    (columns, count.div_ceil(columns))
}

/// The panel's size in pixels for `count` icons at `dpi`.
pub(crate) fn panel_size(count: usize, dpi: u32) -> (i32, i32) {
    let padding = scaled(PADDING_DP, dpi);
    let cell = scaled(CELL_DP, dpi);
    let (columns, rows) = grid(count);
    if count == 0 {
        return (scaled(EMPTY_WIDTH_DP, dpi), cell + 2 * padding);
    }
    (columns as i32 * cell + 2 * padding, rows as i32 * cell + 2 * padding)
}

/// Icon `index`'s cell in the panel's client coordinates.
pub(crate) fn cell_rect(index: usize, count: usize, dpi: u32) -> RECT {
    let padding = scaled(PADDING_DP, dpi);
    let cell = scaled(CELL_DP, dpi);
    let (columns, _) = grid(count);
    let columns = columns.max(1);
    let (column, row) = ((index % columns) as i32, (index / columns) as i32);
    RECT {
        left: padding + column * cell,
        top: padding + row * cell,
        right: padding + (column + 1) * cell,
        bottom: padding + (row + 1) * cell,
    }
}

/// Which icon's cell holds `(x, y)`, if any.
pub(crate) fn cell_at(x: i32, y: i32, count: usize, dpi: u32) -> Option<usize> {
    (0..count).find(|&i| {
        let r = cell_rect(i, count, dpi);
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    })
}

fn window() -> Option<HWND> {
    let existing = WINDOW.with(Cell::get);
    if existing != 0 {
        return Some(HWND(existing as *mut std::ffi::c_void));
    }
    // SAFETY: registers a class and creates a hidden pop-up owned by
    // this thread; every pointer passed is a static or a live local.
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellTrayOverflow"),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
            ..Default::default()
        };
        // Zero for a class that already exists is fine: there is only
        // ever this one registration, and creation below then succeeds.
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("GroveShellTrayOverflow"),
            w!("Hidden icons"),
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
        .ok()?;
        // Rounded corners and a shadow from DWM, as on the other flyouts.
        let _ = super::design::material::apply(hwnd, super::design::material::Surface::Flyout);
        WINDOW.with(|w| w.set(hwnd.0 as isize));
        Some(hwnd)
    }
}

/// Sizes the panel for the icons there are now and puts it under the
/// chevron.
fn place(hwnd: HWND) {
    let count = super::tray_icons::overflow_count();
    SHOWN_COUNT.with(|c| c.set(count));
    let (chevron, monitor_left, monitor_right) = ANCHOR.with(Cell::get);
    // SAFETY: `hwnd` is this module's own window.
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let (width, height) = panel_size(count, dpi);
        let gap = scaled(6, dpi);
        let (x, y) =
            super::tray::overflow_position(chevron, width, monitor_left, monitor_right, chevron.bottom + gap);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn hide(hwnd: HWND) {
    // SAFETY: `hwnd` is this module's own window; killing a timer that
    // is not set is a documented no-op.
    unsafe {
        let _ = KillTimer(hwnd, REFRESH_TIMER);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    HOVER.with(|h| h.set(None));
    super::tooltips::update(hwnd, "");
}

/// Opens the panel under `chevron` (screen coordinates), or closes it if
/// it is open.
pub(crate) fn toggle(chevron: RECT, monitor_left: i32, monitor_right: i32) {
    let Some(hwnd) = window() else { return };
    // SAFETY: a plain query on this module's own window.
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        hide(hwnd);
        return;
    }
    if CLOSED_AT.with(Cell::get).is_some_and(|at| at.elapsed() < REOPEN_GUARD) {
        CLOSED_AT.with(|c| c.set(None));
        return;
    }
    ANCHOR.with(|a| a.set((chevron, monitor_left, monitor_right)));
    place(hwnd);
    // Foreground, so that clicking anywhere else closes it.
    super::util::force_foreground(hwnd);
    // SAFETY: `hwnd` is this module's own window.
    unsafe {
        SetTimer(hwnd, REFRESH_TIMER, REFRESH_MS, None);
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let count = SHOWN_COUNT.with(Cell::get);
    let (width, height) = panel_size(count, dpi);
    let full = RECT { left: 0, top: 0, right: width, bottom: height };

    let background = CreateSolidBrush(COLORREF(super::design::color::surface_raised()));
    FillRect(hdc, &full, background);
    let _ = DeleteObject(background);

    if count == 0 {
        let font = super::util::bar_font(dpi);
        let previous = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(super::design::color::text_muted()));
        let mut text: Vec<u16> = "No hidden icons".encode_utf16().collect();
        let mut rect = full;
        windows::Win32::Graphics::Gdi::DrawTextW(
            hdc,
            &mut text,
            &mut rect,
            windows::Win32::Graphics::Gdi::DT_CENTER
                | windows::Win32::Graphics::Gdi::DT_VCENTER
                | windows::Win32::Graphics::Gdi::DT_SINGLELINE,
        );
        SelectObject(hdc, previous);
        let _ = DeleteObject(font);
    }

    let hover = HOVER.with(Cell::get);
    let icon = scaled(ICON_DP, dpi);
    for index in 0..count {
        let cell = cell_rect(index, count, dpi);
        if hover == Some(index) {
            let brush = CreateSolidBrush(COLORREF(super::design::color::surface_overlay()));
            let previous_brush = SelectObject(hdc, brush);
            let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
            let inset = scaled(2, dpi);
            let radius = scaled(6, dpi);
            let _ = RoundRect(
                hdc,
                cell.left + inset,
                cell.top + inset,
                cell.right - inset,
                cell.bottom - inset,
                radius,
                radius,
            );
            SelectObject(hdc, previous_pen);
            SelectObject(hdc, previous_brush);
            let _ = DeleteObject(brush);
        }
        let left = cell.left + (cell.right - cell.left - icon) / 2;
        let top = cell.top + (cell.bottom - cell.top - icon) / 2;
        super::tray_icons::overflow_paint(
            hdc,
            index,
            RECT { left, top, right: left + icon, bottom: top + icon },
        );
    }
    let _ = EndPaint(hwnd, &ps);
}

fn point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let (x, y) = point(lparam);
            let over = cell_at(x, y, SHOWN_COUNT.with(Cell::get), dpi);
            if HOVER.with(|h| h.replace(over)) != over {
                let _ = InvalidateRect(hwnd, None, false);
                let tip = over.map(super::tray_icons::overflow_name).unwrap_or_default();
                super::tooltips::update(hwnd, &tip);
            }
            let mut track = TRACKMOUSEEVENT {
                cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut track);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if HOVER.with(|h| h.replace(None)).is_some() {
                let _ = InvalidateRect(hwnd, None, false);
                super::tooltips::update(hwnd, "");
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let (x, y) = point(lparam);
            if let Some(index) = cell_at(x, y, SHOWN_COUNT.with(Cell::get), dpi) {
                // On release, like the bar's own icons: an application
                // that opens a menu on the press never sees the release
                // and the menu then cannot be dismissed.
                super::tray_icons::overflow_invoke(index, msg == WM_RBUTTONUP);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            hide(hwnd);
            LRESULT(0)
        }
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // Something else was clicked, or the icon just clicked opened
            // its own window or menu. Either way the panel is done.
            if IsWindowVisible(hwnd).as_bool() {
                CLOSED_AT.with(|c| c.set(Some(Instant::now())));
                hide(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == REFRESH_TIMER => {
            // An icon arriving or leaving changes the grid's shape.
            if super::tray_icons::overflow_count() != SHOWN_COUNT.with(Cell::get) {
                place(hwnd);
            } else {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_icons_sit_in_one_row() {
        assert_eq!(grid(1), (1, 1));
        assert_eq!(grid(5), (5, 1));
    }

    #[test]
    fn more_than_a_row_wraps_downward() {
        assert_eq!(grid(6), (5, 2));
        assert_eq!(grid(11), (5, 3));
    }

    #[test]
    fn the_panel_is_as_large_as_its_grid_plus_the_margin() {
        assert_eq!(panel_size(3, 96), (3 * CELL_DP + 2 * PADDING_DP, CELL_DP + 2 * PADDING_DP));
        assert_eq!(panel_size(6, 192).1, 2 * (2 * CELL_DP + 2 * PADDING_DP));
    }

    #[test]
    fn an_empty_panel_still_has_room_to_say_so() {
        let (w, h) = panel_size(0, 96);
        assert!(w >= EMPTY_WIDTH_DP && h > 0);
        assert_eq!(cell_at(10, 10, 0, 96), None);
    }

    #[test]
    fn every_cell_is_found_by_a_point_inside_it_and_the_margin_hits_nothing() {
        for count in [1usize, 5, 7] {
            for index in 0..count {
                let r = cell_rect(index, count, 144);
                assert_eq!(cell_at((r.left + r.right) / 2, (r.top + r.bottom) / 2, count, 144), Some(index));
            }
        }
        assert_eq!(cell_at(1, 1, 5, 96), None);
        // The unused cells of a ragged last row are not icons.
        let last = cell_rect(6, 7, 96);
        assert_eq!(cell_at(last.right + 5, last.top + 5, 7, 96), None);
    }
}
