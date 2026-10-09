//! The per-monitor top bar: painting, the AppBar work-area reservation,
//! and dispatching clicks on the primary bar's Activities/workspace-dots/
//! clock/Quick-Settings regions.

use std::cell::Cell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    Ellipse, FillRect, RoundRect, SelectObject, SetBkMode, SetTextColor, SRCCOPY,
    BeginPaint, EndPaint, PAINTSTRUCT, TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    GetStockObject, NULL_PEN,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE, SetWindowPos,
};

use super::icons::{battery_icon, draw_icon, volume_icon, Icon};
use super::quick_settings::{battery_status, get_mute, get_volume_percent, toggle_quick_settings};
use super::overview::OverviewMode;
use super::state::STATE;
use super::util::{bar_font, draw_text_in, blend_toward_white};
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
/// A `size`-square centered inside `rect`.
///
/// The settings and session *buttons* are bar-height tall so they have a
/// comfortable hit target, but their glyphs must match the status pill's
/// icons rather than fill the button — `draw_fluent_glyph` sizes the
/// glyph to the rect it is handed, so it gets this inset rect, not the
/// button.
pub(super) fn centered_square(rect: RECT, size: i32) -> RECT {
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let half = size / 2;
    RECT { left: cx - half, top: cy - half, right: cx - half + size, bottom: cy - half + size }
}

fn scaled(v: i32, dpi: u32) -> i32 {
    super::state::scaled((v as f64 * super::state::bar_content_scale()).round() as i32, dpi)
}

/// 96-DPI layout of the status pill (Wi-Fi/volume/battery glyphs) that
/// replaced the old plain "42% Quick Settings" text label — one click
/// target for the whole pill, same as GNOME's single combined status
/// menu rather than a separate flyout per icon.
const QS_PILL_HEIGHT: i32 = 20;
const QS_PILL_PADDING_X: i32 = 8;
pub(super) const QS_ICON_SIZE: i32 = 15;
const QS_ICON_GAP: i32 = 10;
pub(super) const QS_PILL_RADIUS: i32 = 10;
const QS_PILL_RIGHT_MARGIN: i32 = 10;

/// Opacity of the bar's hover plate on the Direct2D path. The GDI painter
/// draws it opaque because it has no choice; over the Mica backdrop a
/// translucent plate reads as a highlight rather than a patch.
pub(super) const BAR_HOVER_ALPHA: f32 = 0.9;

/// The settings-gear glyph that opens `groveshell-settings`'s settings
/// window — the only way to reach it once this bar has hidden the real
/// Windows taskbar (and, with it, the system tray `groveshell-settings`
/// would otherwise show its own icon in). Sits just left of the status
/// pill, same row.
/// Segoe Fluent Icons "Settings"; the `_FALLBACK` is the plain Unicode
/// gear used when that font is missing (pre-Windows 11).
pub(super) const SETTINGS_GLYPH: &str = "\u{E713}";
const SETTINGS_GLYPH_FALLBACK: &str = "\u{2699}";
const SETTINGS_BUTTON_WIDTH: i32 = 20;
const SETTINGS_BUTTON_GAP: i32 = 6;

/// The tray-overflow chevron, sitting just left of the settings gear. Only
/// painted/hit-tested when a real overflow window exists to host (see
/// `tray::overflow_available`).
pub(super) const TRAY_CHEVRON_GLYPH: &str = "\u{25BE}"; // U+25BE BLACK DOWN-POINTING SMALL TRIANGLE
const TRAY_CHEVRON_WIDTH: i32 = 18;
const TRAY_CHEVRON_GAP: i32 = 4;

/// The session/power button, just left of the settings gear. Opens the
/// session menu (lock, sleep, sign out, restart, shut down).
/// Segoe Fluent Icons "PowerButton"; the `_FALLBACK` is the plain
/// Unicode power symbol.
pub(super) const SESSION_GLYPH: &str = "\u{E7E8}";
const SESSION_GLYPH_FALLBACK: &str = "\u{23FB}";
const SESSION_BUTTON_WIDTH: i32 = 20;
const SESSION_BUTTON_GAP: i32 = 6;

/// The session button's rect, just left of the settings button — a pure
/// function of the settings rect so paint and hit-test agree.
pub(super) fn session_button_rect(settings_rect: RECT, dpi: u32, bar_h: i32) -> RECT {
    let w = scaled(SESSION_BUTTON_WIDTH, dpi);
    let gap = scaled(SESSION_BUTTON_GAP, dpi);
    RECT { left: settings_rect.left - gap - w, top: 0, right: settings_rect.left - gap, bottom: bar_h }
}

/// The tray chevron's rect, just left of the session button — a pure
/// function of the session rect so paint and hit-test agree.
pub(super) fn tray_chevron_rect(session_rect: RECT, dpi: u32, bar_h: i32) -> RECT {
    let w = scaled(TRAY_CHEVRON_WIDTH, dpi);
    let gap = scaled(TRAY_CHEVRON_GAP, dpi);
    RECT { left: session_rect.left - gap - w, top: 0, right: session_rect.left - gap, bottom: bar_h }
}

pub(super) fn tray_icon_rect(chevron: RECT, dpi: u32, index: usize) -> RECT {
    let size = scaled(20, dpi);
    let right = chevron.left - scaled(6, dpi) - index as i32 * scaled(26, dpi);
    let top = (chevron.bottom - size) / 2;
    RECT { left: right - size, top, right, bottom: top + size }
}

pub(crate) fn on_tray_context(hwnd: HWND, x: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let mut client = RECT::default();
    unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client); }
    let h = scaled(super::state::BAR_HEIGHT, dpi);
    if let Some(BarRegion::TrayIcon(index)) = region_at(x, dpi, client.right, h, true, 0) { super::tray_icons::invoke(index, true); }
}

/// The status pill's own rect and its three icon slots, in physical
/// pixels at `dpi` — a pure function of `bar_width`/`dpi` so painting
/// and hit-testing can never disagree, same pattern as the overview's
/// `card_layout`.
pub(super) fn qs_pill_layout(bar_width: i32, dpi: u32, bar_h: i32) -> (RECT, [RECT; 3]) {
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
pub(super) fn settings_button_rect(pill: RECT, dpi: u32, bar_h: i32) -> RECT {
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
    SessionButton,
    TrayChevron,
    TrayIcon(usize),
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
    let session_rect = session_button_rect(settings_rect, dpi, bar_h);
    if (session_rect.left..session_rect.right).contains(&x) {
        return Some(BarRegion::SessionButton);
    }
    {
        let chevron = tray_chevron_rect(session_rect, dpi, bar_h);
        if (chevron.left..chevron.right).contains(&x) {
            return Some(BarRegion::TrayChevron);
        }
        for index in 0..super::tray_icons::count() {
            let rect = tray_icon_rect(chevron, dpi, index);
            if (rect.left..rect.right).contains(&x) { return Some(BarRegion::TrayIcon(index)); }
        }
    }
    None
}

// A pop-up anchored to a bar button closes on the *mouse-down* of a
// click — Win32 deactivates a flyout, and `TrackPopupMenu` dismisses the
// session menu, before the button is ever released — while the bar
// dispatches its own clicks on `WM_LBUTTONUP` (see `on_bar_click`). With
// nothing remembering that dismissal, the release of the very click that
// closed a pop-up opens it straight back up, so a bar button can never
// read as a toggle.
//
// [`note_popup_click_dismissed`] records the anchor whose pop-up an
// in-progress click just closed; the `on_bar_click` that click ends in
// consumes the record and does nothing, leaving the pop-up closed.
//
// A `Cell` thread-local rather than a field on `STATE`: this is written
// from flyout `wndproc` paths that already borrow `STATE` elsewhere in
// the same call, and a nested `RefCell` borrow panics.
thread_local! {
    static CLICK_DISMISSED: Cell<Option<(BarRegion, Instant)>> = const { Cell::new(None) };
}

/// How long a recorded dismissal can still swallow a click. The release
/// it belongs to lands one click-hold later, and *any* bar click consumes
/// the record regardless — so this only bounds the single case where that
/// release never reaches a bar at all: press the button, drag off the
/// bar, release there.
const DISMISS_GRACE: Duration = Duration::from_secs(1);

/// Records that `anchor`'s pop-up was just dismissed by a click still in
/// progress: the left button is still down, and it went down on `anchor`
/// itself. Both conditions matter — a keyboard dismiss (Escape) or a
/// click that landed anywhere else must leave the next click free to open
/// the pop-up again.
pub(crate) fn note_popup_click_dismissed(anchor: BarRegion) {
    if !left_button_down() || region_under_cursor() != Some(anchor) {
        return;
    }
    CLICK_DISMISSED.with(|cell| cell.set(Some((anchor, Instant::now()))));
}

/// Whether a click on `region` is the release of the click that dismissed
/// that region's own pop-up. Pure, so the toggle rule is testable without
/// a message loop.
fn dismissal_swallows(
    record: Option<(BarRegion, Instant)>,
    region: BarRegion,
    now: Instant,
) -> bool {
    match record {
        Some((anchor, at)) => {
            anchor == region && now.saturating_duration_since(at) < DISMISS_GRACE
        }
        None => false,
    }
}

/// `GetAsyncKeyState`, not `GetKeyState`: the latter reports the input
/// state as of the last message this thread *retrieved*, and a pop-up is
/// deactivated before the bar's own `WM_LBUTTONDOWN` is ever dispatched —
/// so mid-click it still reads the button as up. The async state is the
/// physical one, which is what "a click is in progress" means here.
fn left_button_down() -> bool {
    // SAFETY: `GetAsyncKeyState` is a precondition-free system call.
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

/// The bar region under the mouse cursor right now, on whichever bar it
/// is over. `region_at` wants a bar-local x, and a dismissal is noticed
/// from a pop-up's own `wndproc` — which has no click coordinates of its
/// own — so the point comes from the cursor instead.
fn region_under_cursor() -> Option<BarRegion> {
    let mut pt = POINT::default();
    // SAFETY: `GetCursorPos` only writes through `pt` for this call.
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    let (hwnd, bar_left, bar_width, is_primary, monitor) = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            st.bars
                .iter()
                .find(|b| {
                    (b.rect.left..b.rect.right).contains(&pt.x)
                        && (b.rect.top..b.rect.bottom).contains(&pt.y)
                })
                .map(|b| {
                    (b.hwnd, b.rect.left, b.rect.right - b.rect.left, b.is_primary, b.monitor.clone())
                })
        })
    })?;
    // SAFETY: `hwnd` came from the live bar list a moment ago.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
    let workspace_count = STATE
        .with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.workspaces.get(monitor.as_str()))
                .map(|t| t.workspace_ids().len())
        })
        .unwrap_or(0);
    region_at(pt.x - bar_left, dpi, bar_width, bar_h, is_primary, workspace_count)
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
        let window_dc = BeginPaint(hwnd, &mut ps);

        // Paint into a back buffer, never straight to the window. The
        // clock timer invalidates this bar once a second, and the first
        // thing the paint does is fill the whole background; drawing that
        // directly to the window makes the fill visible as a flash before
        // the icons land on top of it — a flicker once a second, worst on
        // the status-pill icons, which is exactly what the `bErase: false`
        // at the clock-timer call site in `mod.rs` exists to avoid. Quick
        // Settings already buffers this way for the same reason.
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        let buffer = CreateCompatibleDC(window_dc);
        let buffer_bitmap = CreateCompatibleBitmap(window_dc, client.right, client.bottom);
        let previous_bitmap = SelectObject(buffer, buffer_bitmap);
        let hdc = buffer;

        let dpi = GetDpiForWindow(hwnd).max(96);
        let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
        let bar_width = STATE
            .with(|s| {
                s.borrow().as_ref().and_then(|st| {
                    st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
                })
            })
            .unwrap_or(0);

        // The bar paints its own background rather than leaning on the
        // window class's brush, which was a solid color fixed at
        // registration time and so could never follow a live theme
        // change. Filling here also replaces the class-brush erase as the
        // thing that clears the previous frame's hover highlight, which is
        // why the class is now registered with a null brush.
        //
        // This fill is also why the Mica backdrop set in
        // `design::material` is not visible on the bar: GDI has no alpha
        // channel, so every painted pixel is opaque and DWM's material
        // never shows through. Verified live. Making the material visible
        // needs the Direct2D port tracked as a follow-up in the spec.
        let background = CreateSolidBrush(COLORREF(super::design::color::surface_base()));
        FillRect(hdc, &RECT { left: 0, top: 0, right: bar_width, bottom: bar_h }, background);
        let _ = DeleteObject(background);

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(super::design::color::text()));
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
            let highlight = CreateSolidBrush(blend_toward_white(super::design::color::surface_base(), 0.15));
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
        let filled_brush = CreateSolidBrush(COLORREF(super::design::color::accent()));
        let empty_brush = CreateSolidBrush(COLORREF(super::design::color::text_muted()));
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

            let glyph_color = COLORREF(super::design::color::text());
            let wifi_icon = if super::control_state::snapshot().wifi.unwrap_or(false) { Icon::Wifi } else { Icon::WifiOff };
            draw_icon(hdc, slots[0], wifi_icon, glyph_color);
            let vol_icon = volume_icon(get_mute().unwrap_or(false), get_volume_percent().unwrap_or(0));
            draw_icon(hdc, slots[1], vol_icon, glyph_color);
            if let Some((pct, charging)) = battery_status() {
                draw_icon(hdc, slots[2], battery_icon(pct, charging), glyph_color);
            } else {
                draw_text_in(hdc, slots[2], "AC", format);
            }

            let settings_rect = settings_button_rect(pill, dpi, bar_h);
            if hovered_region == Some(BarRegion::SettingsGear) {
                draw_hover_highlight(hdc, settings_rect, scaled(6, dpi));
            }
            if !super::icons::draw_fluent_glyph(
                hdc,
                centered_square(settings_rect, scaled(QS_ICON_SIZE, dpi)),
                SETTINGS_GLYPH,
                COLORREF(super::design::color::text()),
            ) {
                draw_text_in(hdc, settings_rect, SETTINGS_GLYPH_FALLBACK, format);
            }

            let session_rect = session_button_rect(settings_rect, dpi, bar_h);
            if hovered_region == Some(BarRegion::SessionButton) {
                draw_hover_highlight(hdc, session_rect, scaled(6, dpi));
            }
            if !super::icons::draw_fluent_glyph(
                hdc,
                centered_square(session_rect, scaled(QS_ICON_SIZE, dpi)),
                SESSION_GLYPH,
                COLORREF(super::design::color::text()),
            ) {
                draw_text_in(hdc, session_rect, SESSION_GLYPH_FALLBACK, format);
            }

            {
                let chevron = tray_chevron_rect(session_rect, dpi, bar_h);
                if hovered_region == Some(BarRegion::TrayChevron) {
                    draw_hover_highlight(hdc, chevron, scaled(6, dpi));
                }
                draw_text_in(hdc, chevron, TRAY_CHEVRON_GLYPH, format);
                for index in 0..super::tray_icons::count() {
                    let rect = tray_icon_rect(chevron, dpi, index);
                    if hovered_region == Some(BarRegion::TrayIcon(index)) { draw_hover_highlight(hdc, rect, scaled(4, dpi)); }
                    super::tray_icons::paint(hdc, index, rect);
                }
            }
        }

        SelectObject(hdc, previous_font);
        let _ = DeleteObject(font);
        // One blit of the finished frame; nothing partially drawn is ever
        // on screen.
        let _ = BitBlt(
            window_dc,
            0,
            0,
            client.right,
            client.bottom,
            buffer,
            0,
            0,
            SRCCOPY,
        );
        SelectObject(buffer, previous_bitmap);
        let _ = DeleteObject(buffer_bitmap);
        let _ = DeleteDC(buffer);

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

    let region = region_at(x, dpi, bar_width, bar_h, is_primary, workspace_count);
    // Every bar click consumes a pending dismissal, so at most the one
    // click that closed a pop-up is ever swallowed.
    let dismissed = CLICK_DISMISSED.with(|cell| cell.take());
    if region.is_some_and(|r| dismissal_swallows(dismissed, r, Instant::now())) {
        return;
    }

    match region {
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
        Some(BarRegion::SessionButton) => super::session_menu::show(hwnd),
        Some(BarRegion::TrayIcon(index)) => super::tray_icons::invoke(index, false),
        Some(BarRegion::TrayChevron) => {
            // Host the real Windows overflow window under the chevron. Needs
            // the chevron's screen rect and this bar's monitor span, which
            // the bar's own rect provides.
            let (pill, _) = qs_pill_layout(bar_width, dpi, bar_h);
            let settings_rect = settings_button_rect(pill, dpi, bar_h);
            let session_rect = session_button_rect(settings_rect, dpi, bar_h);
            let chevron = tray_chevron_rect(session_rect, dpi, bar_h);
            if let Some((bar_left, bar_top, bar_right, bar_bottom)) = STATE.with(|s| {
                s.borrow().as_ref().and_then(|st| {
                    st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| (b.rect.left, b.rect.top, b.rect.right, b.rect.bottom))
                })
            }) {
                let screen = RECT {
                    left: bar_left + chevron.left,
                    top: bar_top,
                    right: bar_left + chevron.right,
                    bottom: bar_bottom,
                };
                if super::tray::overflow_available() { super::tray::toggle_overflow(screen, bar_left, bar_right); }
                else { super::tray_icons::invoke_overflow(); }
            }
        }
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

    let tip = match region {
        Some(BarRegion::Activities) => "Activities (Windows key)".into(),
        Some(BarRegion::Dots) => "Workspaces (Ctrl + Alt + Left / Right)".into(),
        Some(BarRegion::Clock) => "Open calendar".into(),
        Some(BarRegion::QsPill) => format!("Control center\nVolume: {}%{}", get_volume_percent().unwrap_or(0), if get_mute() == Some(true) { " (muted)" } else { "" }),
        Some(BarRegion::SettingsGear) => "GroveShell settings".into(),
        Some(BarRegion::SessionButton) => "Power and session".into(),
        Some(BarRegion::TrayChevron) => "Show hidden notification icons".into(),
        Some(BarRegion::TrayIcon(index)) => super::tray_icons::name(index),
        None => String::new(),
    };
    super::tooltips::update(hwnd, &tip);

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
        // `bErase: true` is required for *correctness*, not just cleanliness:
        // `paint_bar` draws with `SetBkMode(TRANSPARENT)` and never fills the
        // bar background itself, so the only thing that clears a previously
        // drawn hover highlight is the class-brush erase `BeginPaint` does
        // when the update region was invalidated with erase requested. With
        // `bErase: false` the old rounded highlight was left painted on the
        // bar after the pointer moved off a region (the reported "hover
        // never fades out" bug). Erasing only fires here on an actual
        // hover-state *change* (guarded by `changed`), not every pixel of
        // movement, so there's no continuous flicker.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
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
        // `WM_MOUSELEAVE`. `bErase: true` so the class-brush erase clears
        // the highlight that was under the pointer — see `on_bar_hover`.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_click_that_dismissed_a_popup_does_not_reopen_it() {
        let now = Instant::now();
        assert!(dismissal_swallows(Some((BarRegion::Clock, now)), BarRegion::Clock, now));
    }

    #[test]
    fn a_click_on_a_different_button_still_opens_it() {
        let now = Instant::now();
        assert!(!dismissal_swallows(Some((BarRegion::Clock, now)), BarRegion::QsPill, now));
    }

    #[test]
    fn a_stale_dismissal_never_swallows_a_later_click() {
        let now = Instant::now();
        let stale = now
            .checked_sub(DISMISS_GRACE + Duration::from_millis(1))
            .expect("machine has been up longer than the grace period");
        assert!(!dismissal_swallows(Some((BarRegion::Clock, stale)), BarRegion::Clock, now));
    }

    #[test]
    fn a_plain_click_with_nothing_dismissed_opens_the_popup() {
        assert!(!dismissal_swallows(None, BarRegion::SessionButton, Instant::now()));
    }
}
