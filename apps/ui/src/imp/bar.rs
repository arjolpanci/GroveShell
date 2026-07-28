//! The per-monitor top bar: painting, the AppBar work-area reservation,
//! and dispatching clicks on the primary bar's Activities/workspace-dots/
//! clock/Quick-Settings regions.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, Ellipse, SelectObject, SetBkMode, SetTextColor,
    BeginPaint, EndPaint, PAINTSTRUCT, TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE, SetWindowPos};

use super::overview::{close_overview, open_overview, snap_carousel_to};
use super::quick_settings::{quick_settings_label_text, toggle_quick_settings};
use super::overview::OverviewMode;
use super::state::{scaled, STATE};
use super::util::{bar_font, draw_text_in};
use super::workspaces::commit_workspace_switch;
use super::calendar::clock_text;
use super::calendar::toggle_calendar;

/// Hit-test region for the painted (not native controls — there isn't
/// enough vertical room in the bar for real button chrome) bar labels.
pub(crate) const ACTIVITIES_LABEL_X: i32 = 8;
pub(crate) const ACTIVITIES_LABEL_WIDTH: i32 = 72;
pub(crate) const CLOCK_LABEL_WIDTH: i32 = 130;
pub(crate) const QS_LABEL_WIDTH: i32 = 170;
pub(crate) const QS_LABEL_MARGIN: i32 = 8;

/// Bar-side workspace indicator: a row of small dots to the right of
/// "Activities," current one filled, the rest outlined.
pub(crate) const WS_DOTS_X: i32 = ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH + 8;
pub(crate) const WS_DOT_SLOT_WIDTH: i32 = 14;
pub(crate) const WS_DOT_RADIUS: i32 = 3;

/// Registers `bar_hwnd` as a top-edge AppBar and reserves a
/// `bar_height`-tall strip of the monitor at `(x, y)` for it, returning
/// the rect the system assigned (per `ABM_SETPOS` semantics, this is
/// what the caller should actually move/resize the window to). Every
/// other top-level window's maximize/work-area layout on that monitor
/// is recalculated by the system as a side effect, exactly as it is
/// for the real taskbar.
///
/// SAFETY: `bar_hwnd` must be a live window for the duration of this
/// call; `SHAppBarMessage` only reads/writes through the `APPBARDATA`
/// pointer for the duration of each call.
pub(crate) unsafe fn register_appbar(bar_hwnd: HWND, x: i32, y: i32, width: i32, bar_height: i32) -> RECT {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: bar_hwnd,
        ..Default::default()
    };
    SHAppBarMessage(ABM_NEW, &mut abd);

    abd.uEdge = ABE_TOP;
    abd.rc = RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + bar_height,
    };
    // ABM_QUERYPOS lets other appbars adjust the proposed rect (e.g. if
    // the Windows taskbar already sits at the top of this monitor);
    // our height is fixed regardless, so only `bottom` is reasserted
    // afterward.
    SHAppBarMessage(ABM_QUERYPOS, &mut abd);
    abd.rc.bottom = abd.rc.top + bar_height;

    SHAppBarMessage(ABM_SETPOS, &mut abd);
    abd.rc
}

/// SAFETY: `bar_hwnd` was previously registered by [`register_appbar`];
/// calling this after that registration is gone (e.g. twice) is a
/// documented no-op on the shell's side, not undefined behavior.
pub(crate) unsafe fn unregister_appbar(bar_hwnd: HWND) {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: bar_hwnd,
        ..Default::default()
    };
    SHAppBarMessage(ABM_REMOVE, &mut abd);
}

/// Paints a bar. Non-primary bars are just the plain class-brush
/// background (still validated via `BeginPaint`/`EndPaint` so Windows
/// doesn't keep re-queuing `WM_PAINT`) — only the primary monitor
/// carries Activities/clock/Quick Settings (per
/// `docs/PROJECT_PLAN.md` §10.1). There are no native `BUTTON`
/// controls for these — at this bar height a real push button's chrome
/// leaves no room for legible text, so this is flat painted text
/// hit-tested in `WM_LBUTTONUP` instead (see `on_bar_click`).
pub(crate) fn paint_bar(hwnd: HWND, is_primary: bool) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`, so
    // it's guaranteed valid for the duration of this call; `ps` is a
    // local that outlives the paired `BeginPaint`/`EndPaint` call.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);

        if is_primary {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
            let bar_width = STATE
                .with(|s| {
                    s.borrow()
                        .as_ref()
                        .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
                })
                .unwrap_or(0);

            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00E0E0E0));
            // The DC's default font is the fixed-size legacy "System"
            // font, which neither scales with DPI nor matches the rest
            // of the OS — use Segoe UI sized to the bar's monitor.
            let font = bar_font(dpi);
            let previous_font = SelectObject(hdc, font);

            let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;
            draw_text_in(
                hdc,
                RECT {
                    left: scaled(ACTIVITIES_LABEL_X, dpi),
                    top: 0,
                    right: scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi),
                    bottom: bar_h,
                },
                "Activities",
                format,
            );

            let (workspace_count, current_index) = STATE
                .with(|s| {
                    s.borrow()
                        .as_ref()
                        .map(|st| (st.workspaces.workspace_ids().len(), st.workspaces.current_index()))
                })
                .unwrap_or((0, 0));
            let dot_mid_y = bar_h / 2;
            let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
            let dot_radius = scaled(WS_DOT_RADIUS, dpi);
            let filled_brush = CreateSolidBrush(COLORREF(0x00E0E0E0));
            let empty_brush = CreateSolidBrush(COLORREF(0x00606060));
            for i in 0..workspace_count {
                let cx = scaled(WS_DOTS_X, dpi) + i as i32 * dot_slot_w + dot_slot_w / 2;
                let brush = if i == current_index { filled_brush } else { empty_brush };
                let previous = SelectObject(hdc, brush);
                let _ = Ellipse(
                    hdc,
                    cx - dot_radius,
                    dot_mid_y - dot_radius,
                    cx + dot_radius,
                    dot_mid_y + dot_radius,
                );
                SelectObject(hdc, previous);
            }
            let _ = DeleteObject(filled_brush);
            let _ = DeleteObject(empty_brush);

            let clock_x = bar_width / 2 - scaled(CLOCK_LABEL_WIDTH, dpi) / 2;
            draw_text_in(
                hdc,
                RECT {
                    left: clock_x,
                    top: 0,
                    right: clock_x + scaled(CLOCK_LABEL_WIDTH, dpi),
                    bottom: bar_h,
                },
                &clock_text(),
                format,
            );

            let qs_x = bar_width - scaled(QS_LABEL_WIDTH + QS_LABEL_MARGIN, dpi);
            draw_text_in(
                hdc,
                RECT {
                    left: qs_x,
                    top: 0,
                    right: qs_x + scaled(QS_LABEL_WIDTH, dpi),
                    bottom: bar_h,
                },
                &quick_settings_label_text(),
                format,
            );

            SelectObject(hdc, previous_font);
            let _ = DeleteObject(font);
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Dispatches a click on the primary bar to whichever of its three
/// painted regions it landed in (see `paint_bar` for the same layout,
/// including the DPI scaling both must agree on).
pub(crate) fn on_bar_click(hwnd: HWND, x: i32) {
    // SAFETY: `hwnd` is the bar window currently handling this click.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let bar_width = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
    });
    let Some(bar_width) = bar_width else {
        return;
    };

    if (scaled(ACTIVITIES_LABEL_X, dpi)..scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi))
        .contains(&x)
    {
        let is_closed = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| matches!(st.overview, OverviewMode::Closed))
        });
        match is_closed {
            Some(true) => open_overview(),
            Some(false) => close_overview(None),
            None => {}
        }
        return;
    }

    let workspace_count = STATE
        .with(|s| s.borrow().as_ref().map(|st| st.workspaces.workspace_ids().len()))
        .unwrap_or(0);
    let dots_x = scaled(WS_DOTS_X, dpi);
    let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
    let dots_width = workspace_count as i32 * dot_slot_w;
    if (dots_x..dots_x + dots_width).contains(&x) {
        let index = ((x - dots_x) / dot_slot_w) as usize;
        let overview_open = STATE
            .with(|s| s.borrow().as_ref().map(|st| matches!(st.overview, OverviewMode::Open { .. })))
            .unwrap_or(false);
        if overview_open {
            snap_carousel_to(index, None);
        } else {
            commit_workspace_switch(index);
        }
        return;
    }

    let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
    let clock_x = bar_width / 2 - clock_w / 2;
    if (clock_x..clock_x + clock_w).contains(&x) {
        toggle_calendar();
        return;
    }

    let qs_x = bar_width - scaled(QS_LABEL_WIDTH + QS_LABEL_MARGIN, dpi);
    if (qs_x..bar_width - scaled(QS_LABEL_MARGIN, dpi)).contains(&x) {
        toggle_quick_settings();
    }
}

pub(crate) fn refresh_bar_indicator() {
    let primary = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
    if let Some(primary) = primary {
        // SAFETY: `primary` is a valid, process-lifetime window.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(primary, None, true);
        }
    }
}

/// Puts every bar back above the overview within the topmost band.
/// Needed once at open (the overview is shown and activated *after*
/// the bars) and again every time the overview is clicked — mouse
/// activation re-raises the overview above its topmost siblings, which
/// is exactly how the bar "stopped rendering" the moment a drag
/// started.
pub(crate) fn raise_bars_topmost() {
    let bar_hwnds: Vec<HWND> = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| st.bars.iter().map(|b| b.hwnd).collect())
            .unwrap_or_default()
    });
    // SAFETY: every bar hwnd is a valid, process-lifetime window.
    unsafe {
        for bar_hwnd in bar_hwnds {
            let _ = SetWindowPos(
                bar_hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}
