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
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, DT_SINGLELINE, DT_VCENTER,
    HOLLOW_BRUSH, NULL_PEN, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
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
use super::control_state::{self, Action};
use super::icons::{battery_icon, draw_icon, volume_icon, Icon};
use super::overview::{close_overview, draw_shadow};
use super::state::{scaled, STATE};
use super::util::{bar_font, draw_text_in};
use std::cell::RefCell;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetLayeredWindowAttributes, SetTimer, LWA_ALPHA, LWA_COLORKEY,
};

pub(crate) const QS_WIDTH: i32 = 420;
pub(crate) const QS_HEIGHT: i32 = 396;
pub(crate) const QS_TIMER_ID: usize = 71;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Wifi,
    Bluetooth,
    Theme,
    Airplane,
    Sound,
}

struct Interaction {
    page: Page,
    hover: Option<usize>,
    focus: Option<usize>,
    motion: super::flyout::Flyout,
    network_offset: usize,
}
thread_local! {
    static INTERACTION: RefCell<Interaction> = RefCell::new(Interaction {
        page: Page::Home, hover: None, focus: None, motion: super::flyout::Flyout::new(), network_offset: 0,
    });
}
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
const QS_CHIP_RADIUS: i32 = 6;
const QS_CARD_RADIUS: i32 = 8;
const QS_ROW_GAP: i32 = 18;
const QS_VOLUME_ROW_HEIGHT: i32 = 32;
const QS_BATTERY_ROW_HEIGHT: i32 = 28;
const QS_ICON_SIZE: i32 = 20;

/// The app's signature accent (the same light blue `draw_glow_border`
/// uses for hover glows elsewhere) — reused here for the volume fill
/// and the "on" chip state so Quick Settings reads as part of the same
/// design language instead of introducing a second accent color.

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
    let card = RECT {
        left: margin,
        top: margin,
        right: card_right,
        bottom: card_bottom,
    };

    let chip_gap = scaled(QS_CHIP_GAP, dpi);
    let chip_h = scaled(QS_CHIP_HEIGHT, dpi);
    let row_gap = scaled(QS_ROW_GAP, dpi);
    let volume_h = scaled(QS_VOLUME_ROW_HEIGHT, dpi);
    let battery_h = scaled(QS_BATTERY_ROW_HEIGHT, dpi);
    let icon = scaled(QS_ICON_SIZE, dpi);

    let chips_top = pad + scaled(40, dpi);
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

    QsLayout {
        card,
        wifi_chip,
        theme_chip,
        bluetooth_chip,
        airplane_chip,
        mute_button,
        volume_track,
        battery_row,
    }
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
unsafe fn fill_round_rect(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    radius: i32,
    color: COLORREF,
) {
    let brush = CreateSolidBrush(color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(
        hdc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius * 2,
        radius * 2,
    );
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
        let target = BeginPaint(hwnd, &mut ps);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, SRCCOPY,
        };
        let buffer = CreateCompatibleDC(target);
        let bitmap = CreateCompatibleBitmap(target, client.right, client.bottom);
        if buffer.0.is_null() || bitmap.0.is_null() {
            render_panel(target, dpi);
        } else {
            let old = SelectObject(buffer, bitmap);
            render_panel(buffer, dpi);
            let _ = BitBlt(
                target,
                0,
                0,
                client.right,
                client.bottom,
                buffer,
                0,
                0,
                SRCCOPY,
            );
            SelectObject(buffer, old);
        }
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(buffer);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe fn render_panel(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32) {
    SetBkMode(hdc, TRANSPARENT);
    let font = bar_font(dpi * 7 / 6);
    let old_font = SelectObject(hdc, font);
    let layout = qs_layout(dpi);
    let snapshot = control_state::snapshot();

    // The whole window first, in the color key — anything left
    // this color after painting the card stays fully transparent
    // (see the module docs), which is what makes the card's
    // rounded corners and the shadow around it actually visible
    // against the real desktop instead of a hard rectangle.
    let key_brush = CreateSolidBrush(COLORREF(QS_COLOR_KEY));
    let client = RECT {
        left: 0,
        top: 0,
        right: scaled(QS_WIDTH + QS_SHADOW_MARGIN * 2, dpi),
        bottom: scaled(QS_HEIGHT + QS_SHADOW_MARGIN * 2, dpi),
    };
    windows::Win32::Graphics::Gdi::FillRect(hdc, &client, key_brush);
    let _ = DeleteObject(key_brush);

    let card_radius = scaled(QS_CARD_RADIUS, dpi);
    draw_shadow(hdc, layout.card, card_radius, 6);
    fill_round_rect(
        hdc,
        layout.card,
        card_radius,
        COLORREF(super::design::color::surface_raised()),
    );

    let text_color = COLORREF(super::design::color::text());
    let muted_text_color = COLORREF(super::design::color::text_muted());
    let accent = COLORREF(super::design::color::accent());
    let hollow = GetStockObject(HOLLOW_BRUSH);

    let page = INTERACTION.with(|i| i.borrow().page);
    SetTextColor(hdc, text_color);
    draw_text_in(
        hdc,
        RECT {
            left: layout.card.left + scaled(16, dpi),
            top: layout.card.top + scaled(10, dpi),
            right: layout.card.right - scaled(16, dpi),
            bottom: layout.card.top + scaled(44, dpi),
        },
        match page {
            Page::Home => "Control center",
            Page::Wifi => "‹   Wi-Fi",
            Page::Bluetooth => "‹   Bluetooth",
            Page::Theme => "‹   Appearance",
            Page::Airplane => "‹   Radios",
            Page::Sound => "‹   Sound output",
        },
        DT_SINGLELINE | DT_VCENTER,
    );
    if page != Page::Home {
        paint_details(hdc, dpi, page);
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
        return;
    }

    let draw_chip = |on: bool, available: bool, rect: RECT, icon_fn: &dyn Fn(), label: &str| {
        let bg = if on {
            COLORREF(super::design::color::accent())
        } else {
            COLORREF(super::design::color::surface_overlay())
        };
        let radius = scaled(QS_CHIP_RADIUS, dpi);
        fill_round_rect(hdc, rect, radius, bg);
        if on {
            // A thin accent ring around the active chip so "on"
            // reads as more than just a slightly different gray.
            let pen = CreatePen(PS_SOLID, 2, accent);
            let previous_pen = SelectObject(hdc, pen);
            SelectObject(hdc, hollow);
            let _ = RoundRect(
                hdc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                radius * 2,
                radius * 2,
            );
            SelectObject(hdc, previous_pen);
            let _ = DeleteObject(pen);
        }

        SetTextColor(
            hdc,
            if !available {
                muted_text_color
            } else if on {
                COLORREF(super::design::color::accent_text())
            } else {
                text_color
            },
        );
        icon_fn();
        draw_text_in(
            hdc,
            RECT {
                left: rect.left + scaled(44, dpi),
                top: rect.top,
                right: rect.right - scaled(34, dpi),
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
        RECT {
            left: chip.left + inset,
            top: mid - size / 2,
            right: chip.left + inset + size,
            bottom: mid + size / 2,
        }
    };

    let wifi_on = snapshot.wifi;
    let wifi_icon_rect = icon_rect_in(layout.wifi_chip);
    draw_chip(
        wifi_on.unwrap_or(false),
        wifi_on.is_some(),
        layout.wifi_chip,
        &|| {
            let color = chip_foreground(wifi_on.unwrap_or(false), wifi_on.is_some());
            let icon = if wifi_on.unwrap_or(false) {
                Icon::Wifi
            } else {
                Icon::WifiOff
            };
            draw_icon(hdc, wifi_icon_rect, icon, color);
        },
        match wifi_on {
            Some(true) => "Wi-Fi",
            Some(false) => "Wi-Fi Off",
            None => "No Adapter",
        },
    );

    let light = snapshot.light;
    let theme_icon_rect = icon_rect_in(layout.theme_chip);
    draw_chip(
        light == Some(false),
        light.is_some(),
        layout.theme_chip,
        &|| {
            let color = chip_foreground(light == Some(false), light.is_some());
            let icon = if light == Some(false) {
                Icon::Moon
            } else {
                Icon::Sun
            };
            draw_icon(hdc, theme_icon_rect, icon, color);
        },
        match light {
            Some(false) => "Dark Mode",
            Some(true) => "Light Mode",
            None => "Theme N/A",
        },
    );

    let bluetooth_state = snapshot.bluetooth;
    let bluetooth_icon_rect = icon_rect_in(layout.bluetooth_chip);
    draw_chip(
        bluetooth_state.unwrap_or(false),
        bluetooth_state.is_some(),
        layout.bluetooth_chip,
        &|| {
            let color =
                chip_foreground(bluetooth_state.unwrap_or(false), bluetooth_state.is_some());
            let icon = if bluetooth_state.unwrap_or(false) {
                Icon::Bluetooth
            } else {
                Icon::BluetoothOff
            };
            draw_icon(hdc, bluetooth_icon_rect, icon, color);
        },
        match bluetooth_state {
            Some(true) => "Bluetooth",
            Some(false) => "Bluetooth",
            None => "No Bluetooth",
        },
    );

    let airplane_state = snapshot.airplane;
    let airplane_icon_rect = icon_rect_in(layout.airplane_chip);
    draw_chip(
        airplane_state.unwrap_or(false),
        airplane_state.is_some(),
        layout.airplane_chip,
        &|| {
            let color = chip_foreground(airplane_state.unwrap_or(false), airplane_state.is_some());
            draw_icon(hdc, airplane_icon_rect, Icon::Plane, color);
        },
        match airplane_state {
            Some(true) => "Radios off",
            Some(false) => "Radios on",
            None => "Unavailable",
        },
    );

    // Volume: mute glyph as a toggle button, a slider track with
    // the accent-filled portion and a white thumb, and the
    // percentage spelled out (nothing else in the row implies a
    // number, so leaving it out was genuinely ambiguous).
    let muted = get_mute().unwrap_or(false);
    SetTextColor(hdc, text_color);
    draw_icon(
        hdc,
        layout.mute_button,
        volume_icon(muted, get_volume_percent().unwrap_or(0)),
        text_color,
    );

    let track_h = scaled(6, dpi);
    let percent_label_w = scaled(40, dpi);
    let track = RECT {
        left: layout.volume_track.left,
        top: (layout.volume_track.top + layout.volume_track.bottom) / 2 - track_h / 2,
        right: layout.volume_track.right - percent_label_w,
        bottom: (layout.volume_track.top + layout.volume_track.bottom) / 2 + track_h / 2,
    };
    fill_round_rect(
        hdc,
        track,
        track_h / 2,
        COLORREF(super::design::color::surface_overlay()),
    );

    let percent = get_volume_percent().unwrap_or(0);
    let fill_right =
        track.left + ((track.right - track.left) as f64 * percent as f64 / 100.0).round() as i32;
    if fill_right > track.left {
        let fill_rect = RECT {
            left: track.left,
            top: track.top,
            right: fill_right.max(track.left + track_h),
            bottom: track.bottom,
        };
        fill_round_rect(hdc, fill_rect, track_h / 2, accent);
    }
    let thumb_r = scaled(7, dpi);
    let thumb_cy = (track.top + track.bottom) / 2;
    fill_ellipse(
        hdc,
        RECT {
            left: fill_right - thumb_r,
            top: thumb_cy - thumb_r,
            right: fill_right + thumb_r,
            bottom: thumb_cy + thumb_r,
        },
        COLORREF(super::design::color::text()),
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
    draw_icon(
        hdc,
        battery_icon_rect,
        battery_icon(battery_pct, battery_charging),
        battery_color,
    );
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

    // Split buttons have distinct toggle and options targets.
    for (chip, on) in [
        (layout.wifi_chip, wifi_on == Some(true)),
        (layout.theme_chip, light == Some(false)),
        (layout.bluetooth_chip, bluetooth_state == Some(true)),
        (layout.airplane_chip, airplane_state == Some(true)),
    ] {
        let arrow = split_arrow(chip, dpi);
        SetTextColor(hdc, chip_foreground(on, true));
        draw_text_in(
            hdc,
            arrow,
            "›",
            DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_CENTER,
        );
        let divider = RECT {
            left: arrow.left,
            right: arrow.left + scaled(1, dpi),
            top: arrow.top + scaled(14, dpi),
            bottom: arrow.bottom - scaled(14, dpi),
        };
        let brush = CreateSolidBrush(chip_foreground(on, true));
        windows::Win32::Graphics::Gdi::FillRect(hdc, &divider, brush);
        let _ = DeleteObject(brush);
    }
    let targets = targets(dpi, Page::Home);
    SetTextColor(hdc, text_color);
    draw_text_in(
        hdc,
        targets[9],
        "Sound output   ›",
        DT_SINGLELINE | DT_VCENTER,
    );
    draw_text_in(
        hdc,
        targets[11],
        "Windows settings   ›",
        DT_SINGLELINE | DT_VCENTER,
    );
    draw_feedback(hdc, dpi);
    draw_focus(hdc, dpi, Page::Home);
    SelectObject(hdc, old_font);
    let _ = DeleteObject(font);
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

fn split_arrow(chip: RECT, dpi: u32) -> RECT {
    RECT {
        left: chip.right - scaled(32, dpi),
        ..chip
    }
}

fn chip_foreground(on: bool, available: bool) -> COLORREF {
    COLORREF(if !available {
        super::design::color::text_muted()
    } else if on {
        super::design::color::accent_text()
    } else {
        super::design::color::text()
    })
}

fn detail_links(page: Page) -> Vec<(&'static str, &'static str)> {
    match page {
        Page::Wifi => vec![
            ("Available networks and connect…", "ms-availablenetworks:"),
            ("Manage saved networks", "ms-settings:network-wifisettings"),
            ("Wi-Fi settings", "ms-settings:network-wifi"),
        ],
        Page::Bluetooth => vec![
            ("Connect or pair a device…", "ms-settings:bluetooth"),
            ("Manage connected devices", "ms-settings:connecteddevices"),
        ],
        Page::Theme => vec![
            (
                "Colors and light / dark mode",
                "ms-settings:personalization-colors",
            ),
            ("Choose a theme", "ms-settings:themes"),
            (
                "Desktop background",
                "ms-settings:personalization-background",
            ),
        ],
        Page::Airplane => vec![
            ("Windows airplane mode", "ms-settings:network-airplanemode"),
            ("Wi-Fi settings", "ms-settings:network-wifi"),
            ("Bluetooth settings", "ms-settings:bluetooth"),
        ],
        Page::Sound => vec![
            ("Choose an output or input device", "ms-settings:sound"),
            ("Volume mixer and app routing", "ms-settings:apps-volume"),
        ],
        Page::Home => vec![],
    }
}

fn targets(dpi: u32, page: Page) -> Vec<RECT> {
    let l = qs_layout(dpi);
    if page == Page::Wifi {
        let count = visible_networks().len();
        let mut rows = vec![RECT {
            left: l.card.left + scaled(12, dpi),
            top: l.card.top + scaled(8, dpi),
            right: l.card.right - scaled(12, dpi),
            bottom: l.card.top + scaled(44, dpi),
        }];
        for n in 0..count {
            let top = l.card.top + scaled(96 + n as i32 * 40, dpi);
            rows.push(RECT {
                left: l.card.left + scaled(16, dpi),
                top,
                right: l.card.right - scaled(16, dpi),
                bottom: top + scaled(36, dpi),
            });
        }
        let top = l.card.top + scaled(310, dpi);
        rows.push(RECT {
            left: l.card.left + scaled(16, dpi),
            top,
            right: l.card.left + scaled(96, dpi),
            bottom: top + scaled(32, dpi),
        });
        rows.push(RECT {
            left: l.card.left + scaled(104, dpi),
            top,
            right: l.card.right - scaled(16, dpi),
            bottom: top + scaled(32, dpi),
        });
        rows.push(RECT {
            left: l.card.left + scaled(16, dpi),
            top: l.card.top + scaled(354, dpi),
            right: l.card.right - scaled(16, dpi),
            bottom: l.card.top + scaled(382, dpi),
        });
        return rows;
    }
    if page != Page::Home {
        let mut rows = vec![RECT {
            left: l.card.left + scaled(12, dpi),
            top: l.card.top + scaled(8, dpi),
            right: l.card.right - scaled(12, dpi),
            bottom: l.card.top + scaled(44, dpi),
        }];
        for index in 0..detail_links(page).len() {
            let top = l.card.top + scaled(112 + index as i32 * 52, dpi);
            rows.push(RECT {
                left: l.card.left + scaled(16, dpi),
                top,
                right: l.card.right - scaled(16, dpi),
                bottom: top + scaled(44, dpi),
            });
        }
        return rows;
    }
    let mut rows = Vec::new();
    for chip in [l.wifi_chip, l.theme_chip, l.bluetooth_chip, l.airplane_chip] {
        let arrow = split_arrow(chip, dpi);
        rows.push(RECT {
            right: arrow.left,
            ..chip
        });
        rows.push(arrow);
    }
    rows.push(RECT {
        left: l.mute_button.left - scaled(4, dpi),
        right: l.mute_button.right + scaled(4, dpi),
        top: l.volume_track.top,
        bottom: l.volume_track.bottom,
    });
    rows.push(RECT {
        left: l.card.left + scaled(16, dpi),
        right: l.card.right - scaled(16, dpi),
        top: l.battery_row.bottom + scaled(4, dpi),
        bottom: l.battery_row.bottom + scaled(32, dpi),
    });
    rows.push(l.battery_row);
    rows.push(RECT {
        left: l.card.left + scaled(16, dpi),
        right: l.card.right - scaled(16, dpi),
        top: l.card.bottom - scaled(40, dpi),
        bottom: l.card.bottom - scaled(12, dpi),
    });
    rows.push(l.volume_track);
    rows
}

fn hit_target(dpi: u32, page: Page, x: i32, y: i32) -> Option<usize> {
    targets(dpi, page)
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

unsafe fn paint_details(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32, page: Page) {
    if page == Page::Wifi {
        paint_networks(hdc, dpi);
        return;
    }
    let l = qs_layout(dpi);
    let message = match page {
        Page::Wifi => "Connect to nearby Wi-Fi networks or manage your saved connections.",
        Page::Bluetooth => "Find and pair headphones, keyboards and other nearby devices.",
        Page::Theme => "Personalize your desktop with Windows colors, themes and backgrounds.",
        Page::Airplane => "The tile switches radios together. Use Windows airplane mode for the system airplane-mode setting.",
        Page::Sound => "Select speakers or headphones and set the volume for individual apps.",
        Page::Home => "",
    };
    SetTextColor(hdc, COLORREF(super::design::color::text_muted()));
    draw_text_in(
        hdc,
        RECT {
            left: l.card.left + scaled(16, dpi),
            top: l.card.top + scaled(52, dpi),
            right: l.card.right - scaled(16, dpi),
            bottom: l.card.top + scaled(104, dpi),
        },
        message,
        windows::Win32::Graphics::Gdi::DT_WORDBREAK,
    );
    for (rect, (label, _)) in targets(dpi, page)
        .into_iter()
        .skip(1)
        .zip(detail_links(page))
    {
        fill_round_rect(
            hdc,
            rect,
            scaled(6, dpi),
            COLORREF(super::design::color::surface_overlay()),
        );
        SetTextColor(hdc, COLORREF(super::design::color::text()));
        draw_text_in(
            hdc,
            RECT {
                left: rect.left + scaled(12, dpi),
                right: rect.right - scaled(12, dpi),
                ..rect
            },
            &format!("{label}   ›"),
            DT_SINGLELINE | DT_VCENTER,
        );
    }
    draw_focus(hdc, dpi, page);
}

unsafe fn draw_feedback(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32) {
    let l = qs_layout(dpi);
    SetTextColor(hdc, COLORREF(super::design::color::text_muted()));
    draw_text_in(
        hdc,
        RECT {
            left: l.card.left + scaled(16, dpi),
            top: l.card.bottom - scaled(72, dpi),
            right: l.card.right - scaled(16, dpi),
            bottom: l.card.bottom - scaled(42, dpi),
        },
        &control_state::snapshot().message,
        windows::Win32::Graphics::Gdi::DT_WORDBREAK,
    );
}

unsafe fn draw_focus(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32, page: Page) {
    let (hover, focus) = INTERACTION.with(|i| {
        let i = i.borrow();
        (i.hover, i.focus)
    });
    let rows = targets(dpi, page);
    for (index, color) in [
        (hover, super::design::color::stroke()),
        (focus, super::design::color::accent()),
    ] {
        if let Some(r) = index.and_then(|n| rows.get(n)) {
            let pen = CreatePen(PS_SOLID, scaled(1, dpi).max(1), COLORREF(color));
            let old = SelectObject(hdc, pen);
            let brush = SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
            let _ = RoundRect(
                hdc,
                r.left + 1,
                r.top + 1,
                r.right - 1,
                r.bottom - 1,
                scaled(8, dpi),
                scaled(8, dpi),
            );
            SelectObject(hdc, brush);
            SelectObject(hdc, old);
            let _ = DeleteObject(pen);
        }
    }
}

fn open_windows(hwnd: HWND, uri: &str) {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    // Windows owns credential entry; GroveShell never reads or stores passwords.
    hide_quick_settings(false);
    let result =
        unsafe { ShellExecuteW(hwnd, w!("open"), &HSTRING::from(uri), None, None, SW_SHOW) };
    if result.0 as isize <= 32 {
        tracing::warn!(
            uri,
            code = result.0 as isize,
            "Could not open Windows settings"
        );
        if uri == "ms-availablenetworks:" {
            unsafe {
                ShellExecuteW(
                    hwnd,
                    w!("open"),
                    w!("ms-settings:network-wifi"),
                    None,
                    None,
                    SW_SHOW,
                );
            }
        }
    }
}

fn activate(hwnd: HWND, page: Page, index: usize) {
    if page == Page::Wifi {
        let networks = visible_networks();
        if index == 0 {
            change_page(hwnd, Page::Home);
        } else if let Some(network) = networks.get(index - 1) {
            if network.connected {
                control_state::request(Action::Disconnect);
            } else if !network.profile.is_empty() && network.connectable {
                control_state::request(Action::Connect(network.profile.clone()));
            } else {
                open_windows(hwnd, "ms-availablenetworks:");
            }
        } else {
            match index - networks.len() {
                1 => control_state::request(Action::Scan),
                2 => open_windows(hwnd, "ms-availablenetworks:"),
                3 => open_windows(hwnd, "ms-settings:network-wifisettings"),
                _ => {}
            }
        }
        return;
    }
    if page != Page::Home {
        if index == 0 {
            change_page(hwnd, Page::Home);
        } else if let Some((_, uri)) = detail_links(page).get(index - 1) {
            open_windows(hwnd, uri);
        }
        return;
    }
    let s = control_state::snapshot();
    match index {
        0 => {
            if let Some(on) = s.wifi {
                control_state::request(Action::Wifi(!on));
            }
        }
        1 => change_page(hwnd, Page::Wifi),
        2 => {
            if s.light.is_some() {
                control_state::request(Action::Theme);
            }
        }
        3 => change_page(hwnd, Page::Theme),
        4 => {
            if let Some(on) = s.bluetooth {
                control_state::request(Action::Bluetooth(!on));
            }
        }
        5 => change_page(hwnd, Page::Bluetooth),
        6 => {
            if let Some(on) = s.airplane {
                control_state::request(Action::Airplane(!on));
            }
        }
        7 => change_page(hwnd, Page::Airplane),
        8 => toggle_mute(),
        9 => change_page(hwnd, Page::Sound),
        10 => open_windows(hwnd, "ms-settings:powersleep"),
        11 => open_windows(hwnd, "ms-settings:"),
        _ => {}
    }
}

fn change_page(hwnd: HWND, page: Page) {
    INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        i.page = page;
        i.hover = None;
        i.focus = Some(0);
        i.motion.open();
    });
    if page == Page::Wifi {
        control_state::request(Action::Scan);
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub(crate) fn on_key(hwnd: HWND, key: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
    let (page, focus) = INTERACTION.with(|i| {
        let i = i.borrow();
        (i.page, i.focus)
    });
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    match key {
        33 | 34 if page == Page::Wifi => scroll_networks(hwnd, if key == 33 { -5 } else { 5 }),
        9 => {
            let count = targets(dpi, page).len();
            let backwards = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            let next = match focus {
                None => {
                    if backwards {
                        count - 1
                    } else {
                        0
                    }
                }
                Some(n) => (n + if backwards { count - 1 } else { 1 }) % count,
            };
            INTERACTION.with(|i| i.borrow_mut().focus = Some(next));
        }
        13 | 32 => {
            if let Some(index) = focus {
                activate(hwnd, page, index);
            }
        }
        37 | 39 | 36 | 35 if page == Page::Home && focus == Some(12) => {
            let value = get_volume_percent().unwrap_or(0) as i32;
            set_volume_percent(match key {
                36 => 0,
                35 => 100,
                37 => (value - 2).max(0) as u32,
                _ => (value + 2).min(100) as u32,
            });
        }
        27 | 8 if page != Page::Home => change_page(hwnd, Page::Home),
        _ => return false,
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
    true
}

fn visible_networks() -> Vec<super::wifi::Network> {
    let networks = control_state::snapshot().networks;
    let offset = INTERACTION
        .with(|i| i.borrow().network_offset)
        .min(networks.len().saturating_sub(5));
    networks.into_iter().skip(offset).take(5).collect()
}

pub(crate) fn scroll_networks(hwnd: HWND, delta: i32) {
    let count = control_state::snapshot().networks.len();
    INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        if i.page == Page::Wifi {
            i.network_offset =
                (i.network_offset as i32 + delta).clamp(0, count.saturating_sub(5) as i32) as usize;
            i.hover = None;
            i.focus = Some(0);
        }
    });
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn paint_networks(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32) {
    let snapshot = control_state::snapshot();
    let l = qs_layout(dpi);
    let networks = visible_networks();
    let rows = targets(dpi, Page::Wifi);
    let summary = if snapshot.network_error == Some(5) {
        "Windows requires location access to list networks. Use Windows networks below.".into()
    } else if snapshot.wifi == Some(false) {
        "Wi-Fi is off. Turn it on from the control center.".into()
    } else if snapshot.network_error.is_some() {
        "Networks unavailable. Open Windows networks to check your adapter.".into()
    } else if !snapshot.message.is_empty() {
        snapshot.message.clone()
    } else if networks.is_empty() {
        "No networks found. Refresh to scan again.".into()
    } else {
        format!(
            "{} nearby networks · scroll or Page Up / Down for more",
            snapshot.networks.len()
        )
    };
    SetTextColor(hdc, COLORREF(super::design::color::text_muted()));
    draw_text_in(
        hdc,
        RECT {
            left: l.card.left + scaled(16, dpi),
            top: l.card.top + scaled(52, dpi),
            right: l.card.right - scaled(16, dpi),
            bottom: l.card.top + scaled(92, dpi),
        },
        &summary,
        windows::Win32::Graphics::Gdi::DT_WORDBREAK,
    );
    for (network, rect) in networks.iter().zip(rows.iter().skip(1)) {
        fill_round_rect(
            hdc,
            *rect,
            scaled(6, dpi),
            COLORREF(super::design::color::surface_overlay()),
        );
        SetTextColor(hdc, COLORREF(super::design::color::text()));
        let name = if network.name.is_empty() {
            "Hidden network"
        } else {
            &network.name
        };
        draw_text_in(
            hdc,
            RECT {
                left: rect.left + scaled(10, dpi),
                right: rect.right - scaled(160, dpi),
                ..*rect
            },
            name,
            DT_SINGLELINE
                | DT_VCENTER
                | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS
                | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
        );
        let detail = if network.connected {
            "Disconnect"
        } else if !network.profile.is_empty() && network.connectable {
            "Connect"
        } else if network.secured {
            "Set up securely ↗"
        } else {
            "Set up ↗"
        };
        SetTextColor(
            hdc,
            COLORREF(if network.connected {
                super::design::color::accent()
            } else {
                super::design::color::text_muted()
            }),
        );
        draw_text_in(
            hdc,
            RECT {
                left: rect.right - scaled(160, dpi),
                right: rect.right - scaled(8, dpi),
                ..*rect
            },
            &format!("{}%  {detail}", network.signal),
            DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_RIGHT,
        );
    }
    SetTextColor(hdc, COLORREF(super::design::color::text()));
    for (rect, label) in rows.iter().skip(networks.len() + 1).zip([
        "Refresh",
        "Windows networks & passwords ↗",
        "Manage saved networks ↗",
    ]) {
        draw_text_in(hdc, *rect, label, DT_SINGLELINE | DT_VCENTER);
    }
    draw_focus(hdc, dpi, Page::Wifi);
}

pub(crate) fn mouse_leave(hwnd: HWND) {
    INTERACTION.with(|i| i.borrow_mut().hover = None);
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub(crate) fn tick(hwnd: HWND) {
    let changed = control_state::poll();
    let opacity = INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        if !i.motion.is_animating() {
            return None;
        }
        let p = i.motion.tick(std::time::Instant::now());
        Some((i.motion.scale_opacity(p).1 * 255.0).round() as u8)
    });
    unsafe {
        if let Some(alpha) = opacity {
            let _ = SetLayeredWindowAttributes(
                hwnd,
                COLORREF(QS_COLOR_KEY),
                alpha,
                LWA_COLORKEY | LWA_ALPHA,
            );
        }
        if INTERACTION.with(|i| !i.borrow().motion.is_visible()) {
            let _ = KillTimer(hwnd, QS_TIMER_ID);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        if changed {
            let _ = InvalidateRect(hwnd, None, false);
            super::bar::refresh_bar_indicator();
        }
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
    let page = INTERACTION.with(|i| i.borrow().page);
    let Some(index) = hit_target(dpi, page, x, y) else {
        return;
    };
    INTERACTION.with(|i| i.borrow_mut().focus = Some(index));
    if page == Page::Home && index == 12 {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.qs_volume_dragging = true;
            }
        });
        unsafe {
            SetCapture(hwnd);
        }
        apply_volume_drag(layout.volume_track, dpi, x);
    } else {
        activate(hwnd, page, index);
    }
    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

pub(crate) fn on_quick_settings_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let page = INTERACTION.with(|i| i.borrow().page);
    let next = hit_target(dpi, page, x, y);
    INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        if i.hover != next {
            i.hover = next;
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
    });
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
        };
        let mut tracking = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tracking);
    }
    let dragging = STATE
        .with(|s| s.borrow().as_ref().map(|st| st.qs_volume_dragging))
        .unwrap_or(false);
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
    unsafe {
        let _ = ReleaseCapture();
    }
}

/// Mirrors the percentage-label reservation `paint_quick_settings`
/// carves out of `volume_track` so a drag can't set a value past where
/// the visible track (and thumb) actually stop.
fn apply_volume_drag(volume_track: RECT, dpi: u32, x: i32) {
    let percent_label_w = scaled(40, dpi);
    let track_right = volume_track.right - percent_label_w;
    let width = (track_right - volume_track.left).max(1);
    let percent = ((x - volume_track.left) as f64 / width as f64 * 100.0)
        .round()
        .clamp(0.0, 100.0) as u32;
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
        let _ = ReleaseCapture();
        let animating = INTERACTION.with(|i| {
            let mut i = i.borrow_mut();
            i.motion.close();
            i.motion.is_animating()
        });
        if animating {
            SetTimer(hwnd, QS_TIMER_ID, 16, None);
        } else {
            let _ = KillTimer(hwnd, QS_TIMER_ID);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
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
    control_state::request(Action::Refresh);
    INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        i.page = Page::Home;
        i.network_offset = 0;
        i.focus = None;
        i.hover = None;
        i.motion.open();
    });
    unsafe {
        let opacity = INTERACTION.with(|i| {
            if i.borrow().motion.is_animating() {
                0
            } else {
                255
            }
        });
        let _ = SetLayeredWindowAttributes(
            hwnd,
            COLORREF(QS_COLOR_KEY),
            opacity,
            LWA_COLORKEY | LWA_ALPHA,
        );
        SetTimer(hwnd, QS_TIMER_ID, 16, None);
        let _ = InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_snapshot() -> control_state::Snapshot {
        control_state::Snapshot {
            wifi: Some(true),
            bluetooth: Some(false),
            airplane: Some(false),
            light: Some(false),
            networks: (0..7)
                .map(|index| super::super::wifi::Network {
                    name: [
                        "Home network",
                        "Cafe & bakery",
                        "Guest Wi-Fi",
                        "Office",
                        "Studio",
                        "Workshop",
                        "Other network",
                    ][index]
                        .into(),
                    profile: if index == 0 {
                        "Home network".into()
                    } else {
                        String::new()
                    },
                    connected: index == 0,
                    signal: 90 - index as u32 * 10,
                    secured: index != 2,
                    connectable: true,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn controls_do_not_overlap_and_hit_their_centers_at_each_dpi() {
        control_state::set_test_snapshot(example_snapshot());
        for dpi in [96, 120, 144, 192, 240] {
            for page in [
                Page::Home,
                Page::Wifi,
                Page::Bluetooth,
                Page::Theme,
                Page::Airplane,
                Page::Sound,
            ] {
                let card = qs_layout(dpi).card;
                let rows = targets(dpi, page);
                for (index, r) in rows.iter().enumerate() {
                    assert!(
                        r.left >= card.left
                            && r.top >= card.top
                            && r.right <= card.right
                            && r.bottom <= card.bottom
                    );
                    assert_eq!(
                        hit_target(dpi, page, (r.left + r.right) / 2, (r.top + r.bottom) / 2),
                        Some(index)
                    );
                    for other in rows.iter().skip(index + 1) {
                        assert!(
                            r.right <= other.left
                                || other.right <= r.left
                                || r.bottom <= other.top
                                || other.bottom <= r.top,
                            "overlapping targets at {dpi} DPI"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn network_list_clamps_when_a_scan_removes_entries() {
        control_state::set_test_snapshot(example_snapshot());
        INTERACTION.with(|i| i.borrow_mut().network_offset = 100);
        assert_eq!(visible_networks().len(), 5);
        assert_eq!(visible_networks().last().unwrap().name, "Other network");
        control_state::set_test_snapshot(control_state::Snapshot::default());
        assert!(visible_networks().is_empty());
        assert_eq!(targets(96, Page::Wifi).len(), 4);
    }

    #[test]
    fn options_do_not_toggle_and_outside_clicks_have_no_target() {
        for dpi in [96, 144, 192] {
            let l = qs_layout(dpi);
            assert_eq!(
                hit_target(dpi, Page::Home, l.wifi_chip.right - 1, l.wifi_chip.top + 1),
                Some(1)
            );
            assert_eq!(
                hit_target(dpi, Page::Home, l.wifi_chip.left, l.wifi_chip.top),
                Some(0)
            );
            assert_eq!(hit_target(dpi, Page::Home, -10, -10), None);
            assert_eq!(
                hit_target(
                    dpi,
                    Page::Home,
                    l.volume_track.right + 1,
                    l.volume_track.top
                ),
                None
            );
        }
    }

    #[test]
    #[ignore = "Exports real GDI renderings for visual inspection; no shell or system-setting changes"]
    fn render_control_center_previews() {
        use windows::Win32::Graphics::{Gdi::*, GdiPlus::*};
        control_state::set_test_snapshot(example_snapshot());
        // SAFETY: this fixture owns its memory DC and bitmap, keeps the
        // selected bitmap alive through rendering/copying, then deselects it.
        unsafe {
            let mut token = 0;
            let input = GdiplusStartupInput {
                GdiplusVersion: 1,
                ..Default::default()
            };
            GdiplusStartup(&mut token, &input, std::ptr::null_mut());
            for (name, page) in [
                ("home", Page::Home),
                ("wifi", Page::Wifi),
                ("bluetooth", Page::Bluetooth),
                ("appearance", Page::Theme),
                ("radios", Page::Airplane),
                ("sound", Page::Sound),
            ] {
                INTERACTION.with(|i| i.borrow_mut().page = page);
                for dpi in [96, 144, 192] {
                    let width = scaled(QS_WIDTH + QS_SHADOW_MARGIN * 2, dpi);
                    let height = scaled(QS_HEIGHT + QS_SHADOW_MARGIN * 2, dpi);
                    let info = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: 40,
                            biWidth: width,
                            biHeight: -height,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let dc = CreateCompatibleDC(None);
                    let mut bits = std::ptr::null_mut();
                    let bitmap =
                        CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
                    let old = SelectObject(dc, bitmap);
                    render_panel(dc, dpi);
                    let _ = GdiFlush();
                    let size = (width * height * 4) as usize;
                    let mut bytes = Vec::new();
                    bytes.extend_from_slice(b"BM");
                    bytes.extend_from_slice(&((54 + size) as u32).to_le_bytes());
                    bytes.extend_from_slice(&[0; 4]);
                    bytes.extend_from_slice(&54u32.to_le_bytes());
                    bytes.extend_from_slice(std::slice::from_raw_parts(
                        &info.bmiHeader as *const _ as *const u8,
                        40,
                    ));
                    bytes.extend_from_slice(std::slice::from_raw_parts(bits as *const u8, size));
                    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join(format!("../../target/control-center-{name}-{dpi}.bmp"));
                    std::fs::write(path, bytes).unwrap();
                    SelectObject(dc, old);
                    let _ = DeleteObject(bitmap);
                    let _ = DeleteDC(dc);
                }
            }
        }
    }
}
