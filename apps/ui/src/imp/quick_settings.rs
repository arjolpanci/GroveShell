//! The Quick Settings flyout: a GNOME-style panel — Wi-Fi and dark-mode
//! toggle chips, a real draggable volume slider, and battery status.
//! Fully custom-painted and custom-hit-tested (no native child
//! controls), same approach as the Activities overview.
//!
//! The window itself is larger than the visible card by
//! `QS_SHADOW_MARGIN` on every side and layered with a color-key: the
//! margin is painted in that key color (so it's fully transparent) and
//! the drop shadow + rounded card are drawn inside it. That margin is
//! also what makes the *window's* corners look rounded — there's no
//! `SetWindowRgn` involved, the rectangular frame is simply invisible
//! outside the card shape.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, GetStockObject,
    RoundRect, SelectObject, SetBkMode, SetTextColor, PAINTSTRUCT, PS_SOLID, DT_SINGLELINE,
    DT_VCENTER, HOLLOW_BRUSH, InvalidateRect, NULL_PEN, TRANSPARENT,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
};

use super::calendar::hide_calendar;
use super::icons::{battery_icon, draw_icon, volume_icon, Icon};
use super::overview::{close_overview, draw_shadow};
use super::radios::{
    airplane_mode_on, bluetooth_on, set_airplane_mode_on, set_bluetooth_on,
};
use super::state::{scaled, STATE};
use super::theme::{apps_use_light_theme, toggle_theme};
use super::util::draw_text_in;
use super::wifi::{set_wifi_radio_on, wifi_radio_on};

pub(crate) const QS_WIDTH: i32 = 320;
pub(crate) const QS_HEIGHT: i32 = 316;
/// Room around the visible card for the drop shadow (see the module
/// docs) — comfortably more than `draw_shadow`'s 6-layer spread plus
/// its downward bias needs.
pub(crate) const QS_SHADOW_MARGIN: i32 = 24;
/// The chroma-key color: fully transparent everywhere it appears,
/// never used by anything actually drawn in the panel.
pub(crate) const QS_COLOR_KEY: u32 = 0x00FF00FF;

const QS_PADDING: i32 = 16;
const QS_CHIP_GAP: i32 = 12;
const QS_CHIP_HEIGHT: i32 = 60;
const QS_CHIP_RADIUS: i32 = 14;
const QS_CARD_RADIUS: i32 = 18;
const QS_ROW_GAP: i32 = 18;
const QS_VOLUME_ROW_HEIGHT: i32 = 32;
const QS_BATTERY_ROW_HEIGHT: i32 = 28;
const QS_ICON_SIZE: i32 = 20;

/// The app's signature accent (the same light blue `draw_glow_border`
/// uses for hover glows elsewhere) — reused here for the volume fill
/// and the "on" chip state so Quick Settings reads as part of the same
/// design language instead of introducing a second accent color.
const QS_ACCENT: u32 = 0x00FFA860;

/// Below this battery percentage the glyph and text turn a warning red
/// rather than the normal foreground color.
const QS_LOW_BATTERY_PERCENT: u8 = 20;

struct QsLayout {
    card: RECT,
    wifi_chip: RECT,
    theme_chip: RECT,
    bluetooth_chip: RECT,
    airplane_chip: RECT,
    mute_button: RECT,
    volume_track: RECT,
    battery_row: RECT,
}

/// Pure function of `dpi` — painting and hit-testing both call this so
/// they can never disagree, same pattern as the overview's `card_layout`.
/// Every rect is in *window* client coordinates, already offset by the
/// shadow margin.
fn qs_layout(dpi: u32) -> QsLayout {
    let margin = scaled(QS_SHADOW_MARGIN, dpi);
    let inner_pad = scaled(QS_PADDING, dpi);
    let pad = margin + inner_pad;
    let card_right = margin + scaled(QS_WIDTH, dpi);
    let card_bottom = margin + scaled(QS_HEIGHT, dpi);
    let content_right = card_right - inner_pad;
    let card = RECT { left: margin, top: margin, right: card_right, bottom: card_bottom };

    let chip_gap = scaled(QS_CHIP_GAP, dpi);
    let chip_h = scaled(QS_CHIP_HEIGHT, dpi);
    let row_gap = scaled(QS_ROW_GAP, dpi);
    let volume_h = scaled(QS_VOLUME_ROW_HEIGHT, dpi);
    let battery_h = scaled(QS_BATTERY_ROW_HEIGHT, dpi);
    let icon = scaled(QS_ICON_SIZE, dpi);

    let chips_top = pad;
    let chip_w = (content_right - pad - chip_gap) / 2;
    let wifi_chip = RECT {
        left: pad,
        top: chips_top,
        right: pad + chip_w,
        bottom: chips_top + chip_h,
    };
    let theme_chip = RECT {
        left: wifi_chip.right + chip_gap,
        top: chips_top,
        right: content_right,
        bottom: chips_top + chip_h,
    };

    let chips_row2_top = wifi_chip.bottom + chip_gap;
    let bluetooth_chip = RECT {
        left: pad,
        top: chips_row2_top,
        right: pad + chip_w,
        bottom: chips_row2_top + chip_h,
    };
    let airplane_chip = RECT {
        left: bluetooth_chip.right + chip_gap,
        top: chips_row2_top,
        right: content_right,
        bottom: chips_row2_top + chip_h,
    };

    let volume_top = bluetooth_chip.bottom + row_gap;
    let mute_button = RECT {
        left: pad,
        top: volume_top + (volume_h - icon) / 2,
        right: pad + icon,
        bottom: volume_top + (volume_h - icon) / 2 + icon,
    };
    let volume_track = RECT {
        left: mute_button.right + scaled(12, dpi),
        top: volume_top,
        right: content_right,
        bottom: volume_top + volume_h,
    };

    let battery_top = volume_track.bottom + row_gap;
    let battery_row = RECT {
        left: pad,
        top: battery_top,
        right: content_right,
        bottom: battery_top + battery_h,
    };

    QsLayout { card, wifi_chip, theme_chip, bluetooth_chip, airplane_chip, mute_button, volume_track, battery_row }
}

/// `None` when there's no battery to report (desktop on AC) — the
/// battery row falls back to "On AC power" in that case. `bool` is
/// "charging," from `SYSTEM_POWER_STATUS::BatteryFlag`'s charging bit.
pub(crate) fn battery_status() -> Option<(u8, bool)> {
    // SAFETY: `status` is a local, zeroed `SYSTEM_POWER_STATUS` that
    // outlives this synchronous call.
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        GetSystemPowerStatus(&mut status).ok()?;
        (status.BatteryLifePercent != 255)
            .then_some((status.BatteryLifePercent, status.BatteryFlag & 0x08 != 0))
    }
}

/// Fills `rect` with `color`, no border — the correct GDI idiom is
/// `NULL_PEN` (no stroke) plus a solid brush (the fill); selecting
/// `HOLLOW_BRUSH` instead, as an earlier version of this file did
/// almost everywhere, draws *no fill at all*, just an outline in
/// whatever pen happened to be active. That bug was why the volume
/// track/fill/thumb and the chip backgrounds all looked flat and
/// colorless.
unsafe fn fill_round_rect(hdc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, radius: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius * 2, radius * 2);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

unsafe fn fill_ellipse(hdc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

pub(crate) fn paint_quick_settings(hwnd: HWND) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        SetBkMode(hdc, TRANSPARENT);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let layout = qs_layout(dpi);

        // The whole window first, in the color key — anything left
        // this color after painting the card stays fully transparent
        // (see the module docs), which is what makes the card's
        // rounded corners and the shadow around it actually visible
        // against the real desktop instead of a hard rectangle.
        let key_brush = CreateSolidBrush(COLORREF(QS_COLOR_KEY));
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        windows::Win32::Graphics::Gdi::FillRect(hdc, &client, key_brush);
        let _ = DeleteObject(key_brush);

        let card_radius = scaled(QS_CARD_RADIUS, dpi);
        draw_shadow(hdc, layout.card, card_radius, 6);
        fill_round_rect(hdc, layout.card, card_radius, COLORREF(0x00262626));

        let text_color = COLORREF(0x00E0E0E0);
        let muted_text_color = COLORREF(0x00A0A0A0);
        let accent = COLORREF(QS_ACCENT);
        let hollow = GetStockObject(HOLLOW_BRUSH);

        let draw_chip = |on: bool, available: bool, rect: RECT, icon_fn: &dyn Fn(), label: &str| {
            let bg = if on { COLORREF(0x00203A52) } else { COLORREF(0x00383838) };
            let radius = scaled(QS_CHIP_RADIUS, dpi);
            fill_round_rect(hdc, rect, radius, bg);
            if on {
                // A thin accent ring around the active chip so "on"
                // reads as more than just a slightly different gray.
                let pen = CreatePen(PS_SOLID, 2, accent);
                let previous_pen = SelectObject(hdc, pen);
                SelectObject(hdc, hollow);
                let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius * 2, radius * 2);
                SelectObject(hdc, previous_pen);
                let _ = DeleteObject(pen);
            }

            SetTextColor(hdc, if available { text_color } else { muted_text_color });
            icon_fn();
            draw_text_in(
                hdc,
                RECT {
                    left: rect.left + scaled(44, dpi),
                    top: rect.top,
                    right: rect.right - scaled(8, dpi),
                    bottom: rect.bottom,
                },
                label,
                DT_SINGLELINE | DT_VCENTER,
            );
        };

        let icon_rect_in = |chip: RECT| -> RECT {
            let size = scaled(QS_ICON_SIZE, dpi);
            let inset = scaled(14, dpi);
            let mid = (chip.top + chip.bottom) / 2;
            RECT { left: chip.left + inset, top: mid - size / 2, right: chip.left + inset + size, bottom: mid + size / 2 }
        };

        let wifi_on = wifi_radio_on();
        let wifi_icon_rect = icon_rect_in(layout.wifi_chip);
        draw_chip(
            wifi_on.unwrap_or(false),
            wifi_on.is_some(),
            layout.wifi_chip,
            &|| {
                let color = if wifi_on.is_some() { text_color } else { muted_text_color };
                let icon = if wifi_on.unwrap_or(false) { Icon::Wifi } else { Icon::WifiOff };
                draw_icon(hdc, wifi_icon_rect, icon, color);
            },
            match wifi_on {
                Some(true) => "Wi-Fi",
                Some(false) => "Wi-Fi Off",
                None => "No Adapter",
            },
        );

        let light = apps_use_light_theme();
        let theme_icon_rect = icon_rect_in(layout.theme_chip);
        draw_chip(
            light == Some(false),
            light.is_some(),
            layout.theme_chip,
            &|| {
                let color = if light.is_some() { text_color } else { muted_text_color };
                let icon = if light == Some(false) { Icon::Moon } else { Icon::Sun };
                draw_icon(hdc, theme_icon_rect, icon, color);
            },
            match light {
                Some(false) => "Dark Mode",
                Some(true) => "Light Mode",
                None => "Theme N/A",
            },
        );

        let bluetooth_state = bluetooth_on();
        let bluetooth_icon_rect = icon_rect_in(layout.bluetooth_chip);
        draw_chip(
            bluetooth_state.unwrap_or(false),
            bluetooth_state.is_some(),
            layout.bluetooth_chip,
            &|| {
                let color = if bluetooth_state.is_some() { text_color } else { muted_text_color };
                let icon = if bluetooth_state.unwrap_or(false) { Icon::Bluetooth } else { Icon::BluetoothOff };
                draw_icon(hdc, bluetooth_icon_rect, icon, color);
            },
            match bluetooth_state {
                Some(true) => "Bluetooth",
                Some(false) => "Bluetooth Off",
                None => "No Bluetooth",
            },
        );

        let airplane_state = airplane_mode_on();
        let airplane_icon_rect = icon_rect_in(layout.airplane_chip);
        draw_chip(
            airplane_state.unwrap_or(false),
            airplane_state.is_some(),
            layout.airplane_chip,
            &|| {
                let color = if airplane_state.is_some() { text_color } else { muted_text_color };
                draw_icon(hdc, airplane_icon_rect, Icon::Plane, color);
            },
            match airplane_state {
                Some(true) => "Airplane Mode",
                Some(false) => "Airplane Mode Off",
                None => "Unavailable",
            },
        );

        // Volume: mute glyph as a toggle button, a slider track with
        // the accent-filled portion and a white thumb, and the
        // percentage spelled out (nothing else in the row implies a
        // number, so leaving it out was genuinely ambiguous).
        let muted = get_mute().unwrap_or(false);
        SetTextColor(hdc, text_color);
        draw_icon(hdc, layout.mute_button, volume_icon(muted, get_volume_percent().unwrap_or(0)), text_color);

        let track_h = scaled(6, dpi);
        let percent_label_w = scaled(40, dpi);
        let track = RECT {
            left: layout.volume_track.left,
            top: (layout.volume_track.top + layout.volume_track.bottom) / 2 - track_h / 2,
            right: layout.volume_track.right - percent_label_w,
            bottom: (layout.volume_track.top + layout.volume_track.bottom) / 2 + track_h / 2,
        };
        fill_round_rect(hdc, track, track_h / 2, COLORREF(0x00383838));

        let percent = get_volume_percent().unwrap_or(0);
        let fill_right = track.left + ((track.right - track.left) as f64 * percent as f64 / 100.0).round() as i32;
        if fill_right > track.left {
            let fill_rect = RECT { left: track.left, top: track.top, right: fill_right.max(track.left + track_h), bottom: track.bottom };
            fill_round_rect(hdc, fill_rect, track_h / 2, accent);
        }
        let thumb_r = scaled(7, dpi);
        let thumb_cy = (track.top + track.bottom) / 2;
        fill_ellipse(
            hdc,
            RECT { left: fill_right - thumb_r, top: thumb_cy - thumb_r, right: fill_right + thumb_r, bottom: thumb_cy + thumb_r },
            COLORREF(0x00FFFFFF),
        );

        SetTextColor(hdc, text_color);
        draw_text_in(
            hdc,
            RECT {
                left: track.right + scaled(8, dpi),
                top: layout.volume_track.top,
                right: layout.volume_track.right,
                bottom: layout.volume_track.bottom,
            },
            &format!("{percent}%"),
            DT_SINGLELINE | DT_VCENTER,
        );

        // Battery row.
        let (battery_color, battery_text) = match battery_status() {
            Some((pct, true)) => (text_color, format!("{pct}% \u{2022} Charging")),
            Some((pct, false)) if pct <= QS_LOW_BATTERY_PERCENT => {
                (COLORREF(0x004040FF), format!("{pct}% \u{2022} Low battery"))
            }
            Some((pct, false)) => (text_color, format!("{pct}%")),
            None => (muted_text_color, "On AC power".to_string()),
        };
        let battery_icon_rect = RECT {
            left: layout.battery_row.left,
            top: layout.battery_row.top,
            right: layout.battery_row.left + scaled(QS_ICON_SIZE, dpi),
            bottom: layout.battery_row.top + scaled(QS_ICON_SIZE, dpi),
        };
        let (battery_pct, battery_charging) = battery_status().unwrap_or((100, false));
        draw_icon(hdc, battery_icon_rect, battery_icon(battery_pct, battery_charging), battery_color);
        SetTextColor(hdc, battery_color);
        draw_text_in(
            hdc,
            RECT {
                left: battery_icon_rect.right + scaled(10, dpi),
                top: layout.battery_row.top,
                right: layout.battery_row.right,
                bottom: layout.battery_row.bottom,
            },
            &battery_text,
            DT_SINGLELINE | DT_VCENTER,
        );

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Acquires the default audio endpoint's volume control fresh for each
/// call rather than caching it — simpler and more robust against the
/// default device changing than holding a long-lived COM object, at
/// the cost of a little overhead per volume interaction (negligible;
/// this only ever runs in response to a user opening/dragging the
/// panel).
fn with_volume<R>(f: impl FnOnce(&IAudioEndpointVolume) -> windows::core::Result<R>) -> Option<R> {
    // SAFETY: `CoInitializeEx` was called once at process startup on
    // this same thread; every call here is synchronous and its result
    // fully consumed before returning.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        f(&volume).ok()
    }
}

pub(crate) fn get_volume_percent() -> Option<u32> {
    with_volume(|v| unsafe { v.GetMasterVolumeLevelScalar() })
        .map(|scalar| (scalar * 100.0).round() as u32)
}

pub(crate) fn get_mute() -> Option<bool> {
    with_volume(|v| unsafe { v.GetMute() }).map(|b| b.as_bool())
}

/// Sets the absolute volume level — the slider drags to a position,
/// not a delta.
pub(crate) fn set_volume_percent(percent: u32) {
    let next = percent.min(100) as f32 / 100.0;
    // SAFETY: no preconditions beyond `with_volume`'s own.
    let _ = with_volume(|v| unsafe { v.SetMasterVolumeLevelScalar(next, std::ptr::null()) });
}

pub(crate) fn toggle_mute() {
    let Some(muted) = get_mute() else {
        return;
    };
    // SAFETY: no preconditions beyond `with_volume`'s own.
    let _ = with_volume(|v| unsafe { v.SetMute(!muted, std::ptr::null()) });
}

/// A press inside the panel: chip toggles fire immediately; a press on
/// the mute button toggles mute; a press anywhere in the volume row
/// (not just the thin track — the whole row height is a much easier
/// target) starts a slider drag, cleared on release regardless of
/// where the pointer ends up (standard slider feel).
pub(crate) fn on_quick_settings_mouse_down(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: `hwnd` is the window currently handling this click.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let layout = qs_layout(dpi);
    let hit = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;

    if hit(layout.wifi_chip) {
        if let Some(on) = wifi_radio_on() {
            set_wifi_radio_on(!on);
        }
    } else if hit(layout.theme_chip) {
        toggle_theme();
    } else if hit(layout.bluetooth_chip) {
        if let Some(on) = bluetooth_on() {
            set_bluetooth_on(!on);
        }
    } else if hit(layout.airplane_chip) {
        if let Some(on) = airplane_mode_on() {
            set_airplane_mode_on(!on);
        }
    } else if hit(layout.mute_button) {
        toggle_mute();
    } else if y >= layout.volume_track.top - scaled(8, dpi) && y < layout.volume_track.bottom + scaled(8, dpi)
        && x >= layout.volume_track.left
    {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.qs_volume_dragging = true;
            }
        });
        apply_volume_drag(layout.volume_track, dpi, x);
    } else {
        return;
    }
    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

pub(crate) fn on_quick_settings_mouse_move(hwnd: HWND, x: i32) {
    let dragging = STATE.with(|s| s.borrow().as_ref().map(|st| st.qs_volume_dragging)).unwrap_or(false);
    if !dragging {
        return;
    }
    // SAFETY: `hwnd` is the window currently handling this move.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    apply_volume_drag(qs_layout(dpi).volume_track, dpi, x);
    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

pub(crate) fn on_quick_settings_mouse_up() {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.qs_volume_dragging = false;
        }
    });
}

/// Mirrors the percentage-label reservation `paint_quick_settings`
/// carves out of `volume_track` so a drag can't set a value past where
/// the visible track (and thumb) actually stop.
fn apply_volume_drag(volume_track: RECT, dpi: u32, x: i32) {
    let percent_label_w = scaled(40, dpi);
    let track_right = volume_track.right - percent_label_w;
    let width = (track_right - volume_track.left).max(1);
    let percent = ((x - volume_track.left) as f64 / width as f64 * 100.0).round().clamp(0.0, 100.0) as u32;
    set_volume_percent(percent);
}

/// Mirrors [`hide_calendar`] for the Quick Settings flyout.
pub(crate) fn hide_quick_settings(restore_focus: bool) {
    let result = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        if !state.quick_settings_open {
            return None;
        }
        state.quick_settings_open = false;
        state.qs_volume_dragging = false;
        Some((state.quick_settings_hwnd, state.previous_foreground))
    });
    let Some((hwnd, previous)) = result else {
        return;
    };
    // SAFETY: see `hide_calendar`.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
        if restore_focus && !previous.0.is_null() {
            let _ = SetForegroundWindow(previous);
        }
    }
}

pub(crate) fn toggle_quick_settings() {
    let info = STATE.with(|s| {
        s.borrow().as_ref().map(|st| {
            (
                st.quick_settings_hwnd,
                st.quick_settings_open,
                st.primary_monitor.clone(),
            )
        })
    });
    let Some((hwnd, is_open, primary_monitor)) = info else {
        return;
    };

    if is_open {
        hide_quick_settings(true);
        return;
    }

    hide_calendar(false);
    close_overview(&primary_monitor, None);

    // SAFETY: no preconditions.
    let previous_foreground = unsafe { GetForegroundWindow() };
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.previous_foreground = previous_foreground;
            state.quick_settings_open = true;
        }
    });

    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}
