//! The per-monitor top bar: painting, the AppBar work-area reservation,
//! and dispatching clicks on the primary bar's Activities/workspace-dots/
//! clock/Quick-Settings regions.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, Ellipse, RoundRect, SelectObject, SetBkMode, SetTextColor,
    BeginPaint, EndPaint, PAINTSTRUCT, TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    GetStockObject, NULL_PEN,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE, SetWindowPos,
};

use super::icons::{battery_icon, draw_icon, volume_icon, Icon};
use super::quick_settings::{battery_status, get_mute, get_volume_percent, toggle_quick_settings};
use super::overview::OverviewMode;
use super::state::STATE;
use super::util::{bar_font, draw_text_in, blend_toward_white};
use super::wifi::wifi_radio_on;
use super::calendar::clock_text;
use super::calendar::toggle_calendar;

/// This file's own DPI-scaling entry point, used instead of
/// `state::scaled` directly: every 96-DPI constant in this file
/// (including `state::BAR_HEIGHT` itself, used below for `bar_h`) was
/// tuned against the bar's *original* fixed height, so scaling by DPI
/// alone left every icon/glyph/dot the same size when the user grew the
/// bar via the Top Bar settings page's height slider — only the window
/// grew, recentring the same-size contents in extra empty space. Scaling
/// every constant by `bar_content_scale()` first (the ratio between the
/// configured height and the tuned baseline) before the usual DPI scale
/// makes the bar's contents actually grow with it.
fn scaled(v: i32, dpi: u32) -> i32 {
    super::state::scaled((v as f64 * super::state::bar_content_scale()).round() as i32, dpi)
}

/// 96-DPI layout of the status pill (Wi-Fi/volume/battery glyphs) that
/// replaced the old plain "42% Quick Settings" text label — one click
/// target for the whole pill, same as GNOME's single combined status
/// menu rather than a separate flyout per icon.
const QS_PILL_HEIGHT: i32 = 20;
const QS_PILL_PADDING_X: i32 = 8;
const QS_ICON_SIZE: i32 = 15;
const QS_ICON_GAP: i32 = 10;
const QS_PILL_RADIUS: i32 = 10;
const QS_PILL_RIGHT_MARGIN: i32 = 10;

/// The settings-gear glyph that opens `groveshell-settings`'s settings
/// window — the only way to reach it once this bar has hidden the real
/// Windows taskbar (and, with it, the system tray `groveshell-settings`
/// would otherwise show its own icon in). Sits just left of the status
/// pill, same row.
const SETTINGS_GLYPH: &str = "\u{2699}"; // U+2699 GEAR
const SETTINGS_BUTTON_WIDTH: i32 = 20;
const SETTINGS_BUTTON_GAP: i32 = 6;

/// The status pill's own rect and its three icon slots, in physical
/// pixels at `dpi` — a pure function of `bar_width`/`dpi` so painting
/// and hit-testing can never disagree, same pattern as the overview's
/// `card_layout`.
fn qs_pill_layout(bar_width: i32, dpi: u32, bar_h: i32) -> (RECT, [RECT; 3]) {
    let icon = scaled(QS_ICON_SIZE, dpi);
    let gap = scaled(QS_ICON_GAP, dpi);
    let pad_x = scaled(QS_PILL_PADDING_X, dpi);
    let pill_h = scaled(QS_PILL_HEIGHT, dpi);
    let pill_w = pad_x * 2 + icon * 3 + gap * 2;
    let right_margin = scaled(QS_PILL_RIGHT_MARGIN, dpi);
    let pill = RECT {
        left: bar_width - right_margin - pill_w,
        top: (bar_h - pill_h) / 2,
        right: bar_width - right_margin,
        bottom: (bar_h - pill_h) / 2 + pill_h,
    };
    let icon_top = pill.top + (pill_h - icon) / 2;
    let mut slots = [RECT::default(); 3];
    for (i, slot) in slots.iter_mut().enumerate() {
        let left = pill.left + pad_x + i as i32 * (icon + gap);
        *slot = RECT { left, top: icon_top, right: left + icon, bottom: icon_top + icon };
    }
    (pill, slots)
}

/// The settings button's rect, just left of the status pill — a pure
/// function of the pill's own rect so painting and hit-testing can
/// never disagree, same pattern as `qs_pill_layout`.
fn settings_button_rect(pill: RECT, dpi: u32, bar_h: i32) -> RECT {
    let w = scaled(SETTINGS_BUTTON_WIDTH, dpi);
    let gap = scaled(SETTINGS_BUTTON_GAP, dpi);
    RECT {
        left: pill.left - gap - w,
        top: 0,
        right: pill.left - gap,
        bottom: bar_h,
    }
}

/// Hit-test region for the painted (not native controls — there isn't
/// enough vertical room in the bar for real button chrome) bar labels.
pub(crate) const ACTIVITIES_LABEL_X: i32 = 8;
pub(crate) const ACTIVITIES_LABEL_WIDTH: i32 = 72;
pub(crate) const CLOCK_LABEL_WIDTH: i32 = 130;
pub(crate) const QS_LABEL_MARGIN: i32 = 8;

/// Bar-side workspace indicator: a row of small dots to the right of
/// "Activities," current one filled, the rest outlined.
pub(crate) const WS_DOTS_X: i32 = ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH + 8;
pub(crate) const WS_DOT_SLOT_WIDTH: i32 = 14;
pub(crate) const WS_DOT_RADIUS: i32 = 3;

/// A bar's clickable regions — shared between `on_bar_click` (dispatch),
/// `on_bar_hover` (hover highlight + hand cursor), and `paint_bar` (the
/// highlight itself), so all three agree on where a click target
/// actually is. `Activities`/`Dots` exist on every monitor's bar; the
/// rest are primary-bar-only (see `region_at`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarRegion {
    Activities,
    Dots,
    Clock,
    QsPill,
    SettingsGear,
}

/// Which clickable region (if any) `x` falls under, given this bar's
/// width/dpi/primary-ness and current workspace count — a pure function
/// of the same inputs `paint_bar` lays out from, so painting, hit-
/// testing, and hover can never disagree.
fn region_at(x: i32, dpi: u32, bar_width: i32, bar_h: i32, is_primary: bool, workspace_count: usize) -> Option<BarRegion> {
    if (scaled(ACTIVITIES_LABEL_X, dpi)..scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi)).contains(&x) {
        return Some(BarRegion::Activities);
    }
    let dots_x = scaled(WS_DOTS_X, dpi);
    let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
    let dots_width = workspace_count as i32 * dot_slot_w;
    if (dots_x..dots_x + dots_width).contains(&x) {
        return Some(BarRegion::Dots);
    }
    if !is_primary {
        return None;
    }
    let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
    let clock_x = bar_width / 2 - clock_w / 2;
    if (clock_x..clock_x + clock_w).contains(&x) {
        return Some(BarRegion::Clock);
    }
    let (pill, _) = qs_pill_layout(bar_width, dpi, bar_h);
    if (pill.left..pill.right).contains(&x) {
        return Some(BarRegion::QsPill);
    }
    let settings_rect = settings_button_rect(pill, dpi, bar_h);
    if (settings_rect.left..settings_rect.right).contains(&x) {
        return Some(BarRegion::SettingsGear);
    }
    None
}

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

/// Paints a bar. Activities + workspace dots now paint on every
/// monitor's bar, each reading its own monitor's `WorkspaceTracker` via
/// `st.workspaces.get(monitor)`; the clock and Quick Settings status
/// pill remain primary-bar-only (per `docs/PROJECT_PLAN.md` §10.1).
/// There are no native `BUTTON` controls for any of these — at this bar
/// height a real push button's chrome leaves no room for legible text,
/// so this is flat painted text hit-tested in `WM_LBUTTONUP` instead
/// (see `on_bar_click`).
pub(crate) fn paint_bar(hwnd: HWND, is_primary: bool, monitor: &str) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`, so
    // it's guaranteed valid for the duration of this call; `ps` is a
    // local that outlives the paired `BeginPaint`/`EndPaint` call.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);

        let dpi = GetDpiForWindow(hwnd).max(96);
        let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
        let bar_width = STATE
            .with(|s| {
                s.borrow().as_ref().and_then(|st| {
                    st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
                })
            })
            .unwrap_or(0);

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(super::palette::text()));
        // The DC's default font is the fixed-size legacy "System"
        // font, which neither scales with DPI nor matches the rest
        // of the OS — use Segoe UI sized to the bar's monitor.
        let font = bar_font(dpi);
        let previous_font = SelectObject(hdc, font);
        let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;

        let hovered_region = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.hovered_bar_region)
                .filter(|(hover_hwnd, _)| *hover_hwnd == hwnd)
                .map(|(_, region)| region)
        });
        let draw_hover_highlight = |hdc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, radius: i32| {
            let highlight = CreateSolidBrush(blend_toward_white(super::palette::background(), 0.15));
            let previous_brush = SelectObject(hdc, highlight);
            let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
            let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius * 2, radius * 2);
            SelectObject(hdc, previous_pen);
            SelectObject(hdc, previous_brush);
            let _ = DeleteObject(highlight);
        };

        // Activities button + workspace dots: every monitor's bar now,
        // each reading its own monitor's tracker.
        let activities_rect = RECT {
            left: scaled(ACTIVITIES_LABEL_X, dpi),
            top: 0,
            right: scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi),
            bottom: bar_h,
        };
        if hovered_region == Some(BarRegion::Activities) {
            draw_hover_highlight(hdc, activities_rect, scaled(6, dpi));
        }
        draw_text_in(hdc, activities_rect, "Activities", format);

        let (workspace_count, current_index) = STATE
            .with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|st| st.workspaces.get(monitor))
                    .map(|t| (t.workspace_ids().len(), t.current_index()))
            })
            .unwrap_or((0, 0));
        if hovered_region == Some(BarRegion::Dots) && workspace_count > 0 {
            let dots_rect = RECT {
                left: scaled(WS_DOTS_X, dpi),
                top: 0,
                right: scaled(WS_DOTS_X, dpi) + workspace_count as i32 * scaled(WS_DOT_SLOT_WIDTH, dpi),
                bottom: bar_h,
            };
            draw_hover_highlight(hdc, dots_rect, scaled(6, dpi));
        }
        let dot_mid_y = bar_h / 2;
        let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
        let dot_radius = scaled(WS_DOT_RADIUS, dpi);
        let filled_brush = CreateSolidBrush(COLORREF(super::palette::accent()));
        let empty_brush = CreateSolidBrush(COLORREF(super::palette::text_muted()));
        for i in 0..workspace_count {
            let cx = scaled(WS_DOTS_X, dpi) + i as i32 * dot_slot_w + dot_slot_w / 2;
            let brush = if i == current_index { filled_brush } else { empty_brush };
            let previous = SelectObject(hdc, brush);
            let _ = Ellipse(hdc, cx - dot_radius, dot_mid_y - dot_radius, cx + dot_radius, dot_mid_y + dot_radius);
            SelectObject(hdc, previous);
        }
        let _ = DeleteObject(filled_brush);
        let _ = DeleteObject(empty_brush);

        if is_primary {
            let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
            let clock_x = bar_width / 2 - clock_w / 2;
            let clock_rect = RECT { left: clock_x, top: 0, right: clock_x + clock_w, bottom: bar_h };
            if hovered_region == Some(BarRegion::Clock) {
                draw_hover_highlight(hdc, clock_rect, scaled(6, dpi));
            }
            draw_text_in(hdc, clock_rect, &clock_text(), format);

            let (pill, slots) = qs_pill_layout(bar_width, dpi, bar_h);
            if hovered_region == Some(BarRegion::QsPill) {
                draw_hover_highlight(hdc, pill, scaled(QS_PILL_RADIUS, dpi));
            }

            let glyph_color = COLORREF(super::palette::text());
            let wifi_icon = if wifi_radio_on().unwrap_or(false) { Icon::Wifi } else { Icon::WifiOff };
            draw_icon(hdc, slots[0], wifi_icon, glyph_color);
            let vol_icon = volume_icon(get_mute().unwrap_or(false), get_volume_percent().unwrap_or(0));
            draw_icon(hdc, slots[1], vol_icon, glyph_color);
            let (pct, charging) = battery_status().unwrap_or((100, false));
            draw_icon(hdc, slots[2], battery_icon(pct, charging), glyph_color);

            let settings_rect = settings_button_rect(pill, dpi, bar_h);
            if hovered_region == Some(BarRegion::SettingsGear) {
                draw_hover_highlight(hdc, settings_rect, scaled(6, dpi));
            }
            draw_text_in(hdc, settings_rect, SETTINGS_GLYPH, format);
        }

        SelectObject(hdc, previous_font);
        let _ = DeleteObject(font);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Dispatches a click on a bar to whichever painted region it landed
/// in (see `paint_bar` for the same layout, including the DPI scaling
/// both must agree on). Activities + workspace dots are handled on
/// every monitor's bar; the clock and Quick Settings pill remain
/// primary-bar-only.
pub(crate) fn on_bar_click(hwnd: HWND, x: i32, is_primary: bool, monitor: &str) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let bar_width = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
        })
    });
    let Some(bar_width) = bar_width else {
        return;
    };
    let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
    let workspace_count = STATE
        .with(|s| s.borrow().as_ref().and_then(|st| st.workspaces.get(monitor)).map(|t| t.workspace_ids().len()))
        .unwrap_or(0);

    match region_at(x, dpi, bar_width, bar_h, is_primary, workspace_count) {
        Some(BarRegion::Activities) => super::overview::toggle_overview_for(monitor),
        Some(BarRegion::Dots) => {
            let dots_x = scaled(WS_DOTS_X, dpi);
            let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
            let index = ((x - dots_x) / dot_slot_w) as usize;
            let overview_open = STATE
                .with(|s| {
                    s.borrow().as_ref().and_then(|st| st.overviews.get(monitor))
                        .map(|ov| matches!(ov.mode, OverviewMode::Open { .. }))
                })
                .unwrap_or(false);
            if overview_open {
                super::overview::snap_carousel_to(monitor, index, None);
            } else {
                super::workspaces::commit_workspace_switch(monitor, index);
            }
        }
        Some(BarRegion::Clock) => toggle_calendar(),
        Some(BarRegion::QsPill) => toggle_quick_settings(),
        Some(BarRegion::SettingsGear) => open_settings_app(),
        None => {}
    }
}

/// Opens `groveshell-settings`'s settings window: asks an already-running
/// instance to show it over IPC, or launches a fresh one if none is
/// running. A freshly launched instance detects `ui` is already up (see
/// `apps/settings/src/imp/process.rs`'s `groveshell_already_running`) and
/// skips spawning a duplicate watchdog/host/ui trio — so this is safe to
/// call regardless of whether GroveShell was originally started via
/// `groveshell-settings.exe` or `scripts/dev-start.ps1`.
fn open_settings_app() {
    if let Ok(mut conn) = groveshell_ipc::pipe::connect("groveshell-settings") {
        let envelope = groveshell_ipc::Envelope::new(
            "groveshell-ui",
            groveshell_ipc::message_type::SETTINGS_SHOW,
            serde_json::json!({}),
        );
        if groveshell_ipc::framing::write_envelope(&mut conn, &envelope).is_ok() {
            return;
        }
    }

    let Ok(mut exe) = std::env::current_exe() else {
        tracing::error!("could not resolve current_exe to find groveshell-settings.exe");
        return;
    };
    exe.pop();
    exe.push("groveshell-settings.exe");
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        tracing::error!(error = ?e, path = ?exe, "failed to launch groveshell-settings.exe");
    }
}

/// Bar-only mouse tracking: whether the pointer sits over the status
/// pill, for the hover highlight in `paint_bar`. `TrackMouseEvent`
/// arms a one-shot `WM_MOUSELEAVE` so the highlight clears the instant
/// the pointer leaves the bar entirely, not just when it moves to
/// another spot on the bar. The status pill only exists on the primary
/// bar (Quick Settings stays primary-only), so non-primary bars early
/// return.
pub(crate) fn on_bar_hover(hwnd: HWND, x: i32, is_primary: bool, monitor: &str) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let bar_width = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
        })
    });
    let Some(bar_width) = bar_width else {
        return;
    };
    let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
    let workspace_count = STATE
        .with(|s| s.borrow().as_ref().and_then(|st| st.workspaces.get(monitor)).map(|t| t.workspace_ids().len()))
        .unwrap_or(0);
    let region = region_at(x, dpi, bar_width, bar_h, is_primary, workspace_count);

    let changed = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return false;
        };
        let new_value = region.map(|r| (hwnd, r));
        if state.hovered_bar_region == new_value {
            return false;
        }
        state.hovered_bar_region = new_value;
        true
    });
    if changed {
        // SAFETY: `hwnd` is the bar window currently handling this move.
        // `bErase: false` — only the hover highlight changed, not the
        // bar's static background, so there's nothing to erase; erasing
        // anyway forces a visible clear-then-redraw flash on every
        // hover-state change.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
        }
    }
    // Always re-arm: Windows only fires one `WM_MOUSELEAVE` per
    // `TrackMouseEvent` call, so this needs to run on every move within
    // the bar (regardless of which region, if any, is hovered) to keep
    // tracking active for whenever the pointer actually leaves.
    // SAFETY: a plain, fully-initialized local struct passed by pointer
    // only for the duration of this call.
    unsafe {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tme);
    }
}

/// Clears the hover highlight once the pointer actually leaves the bar
/// (see `on_bar_hover`).
pub(crate) fn on_bar_mouse_leave(hwnd: HWND) {
    let changed = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return false;
        };
        if state.hovered_bar_region.map_or(true, |(h, _)| h != hwnd) {
            return false;
        }
        state.hovered_bar_region = None;
        true
    });
    if changed {
        // SAFETY: `hwnd` is the bar window that just received
        // `WM_MOUSELEAVE`. `bErase: false` — see `on_bar_hover`.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
        }
    }
}

/// Repaints every monitor's bar so its workspace dots pick up the change.
/// Every monitor paints its own Activities/dots from its own monitor's
/// `STATE.workspaces` entry (see this file's top-of-file doc comment), so
/// invalidating only `primary_bar_hwnd` left non-primary bars showing
/// stale dots until something else happened to repaint them.
pub(crate) fn refresh_bar_indicator() {
    let bar_hwnds: Vec<HWND> = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| st.bars.iter().map(|b| b.hwnd).collect())
            .unwrap_or_default()
    });
    // SAFETY: every bar hwnd is a valid, process-lifetime window.
    // `bErase: false` — only the workspace dots changed, not the bar's
    // static background; erasing anyway flickers on every switch.
    unsafe {
        for bar_hwnd in bar_hwnds {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(bar_hwnd, None, false);
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
