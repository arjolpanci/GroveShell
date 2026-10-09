//! The Quick Settings flyout: a GNOME-style panel — Wi-Fi and dark-mode
//! toggle chips, a real draggable volume slider, and battery status.
//! Fully custom-painted and custom-hit-tested (no native child
//! controls), same approach as the Activities overview.
//!
//! The window is exactly the card. It is deliberately *not* layered:
//! `WS_EX_LAYERED` blocks the Windows 11 backdrop material, so the
//! color-key shaping and alpha fade it used to carry were replaced by
//! DWM-drawn round corners and shadow (`design::material`) and the
//! dropdown reveal in [`apply_reveal`] (spec §3.2/§3.4).
//!
//! Every rect comes from [`qs_layout`], which is a pure function of DPI
//! and derives from the fixed `QS_HEIGHT` rather than the client rect.
//! That is what makes the reveal cheap: the window can be any height
//! mid-animation and the content still lands where it belongs, clipped by
//! the window bounds.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DeleteObject, EndPaint, InvalidateRect, SelectObject, DT_SINGLELINE, DT_VCENTER,
    PAINTSTRUCT,
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
use super::icons::{battery_icon, volume_icon, Icon};
use super::overview::close_overview;
use super::state::{scaled, STATE};
use super::canvas::Canvas;
use std::cell::RefCell;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

pub(crate) const QS_WIDTH: i32 = 420;
/// Sized to the tallest page, which is Wi-Fi: its network list plus the
/// three links beneath it. Home needs less and leaves the remainder
/// empty — the panel is one window for every page, so the height is the
/// maximum, not the average. It came down from 396 with the chips.
pub(crate) const QS_HEIGHT: i32 = 360;
pub(crate) const QS_TIMER_ID: usize = 71;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
/// Was the breathing room the software drop-shadow was painted into,
/// back when the window was layered and color-keyed. DWM draws the
/// flyout's shadow and corners now (spec §3.1), so the window is exactly
/// the card and the margin is zero. Kept as a named constant rather than
/// deleted because the preview fixtures and `qs_layout` both still speak
/// in terms of it, and a future surface may want its own inset.
pub(crate) const QS_SHADOW_MARGIN: i32 = 0;
/// Opacity of the Quick Settings card on the Direct2D backend. Opaque
/// enough to keep text and chips legible over a busy wallpaper, sheer
/// enough that the Acrylic behind it is clearly doing something.
const QS_CARD_ALPHA: f32 = 0.78;

const QS_PADDING: i32 = 16;
const QS_CHIP_GAP: i32 = 12;
/// Measured off Windows 11's own quick settings: its chips are about
/// this tall. At 60 they dominated the panel as four saturated slabs.
const QS_CHIP_HEIGHT: i32 = 48;
const QS_CHIP_RADIUS: i32 = 6;
const QS_CARD_RADIUS: i32 = 8;
const QS_ROW_GAP: i32 = 18;
const QS_VOLUME_ROW_HEIGHT: i32 = 32;
const QS_BATTERY_ROW_HEIGHT: i32 = 28;
const QS_ICON_SIZE: i32 = 20;
/// The header band detail pages draw their back button and title into.
/// Home leaves it empty.
const QS_HEADER_HEIGHT: i32 = 8;
/// The header band on a detail page, which does carry a control.
const QS_DETAIL_HEADER_HEIGHT: i32 = 44;

/// The app's signature accent (the same light blue `draw_glow_border`
/// uses for hover glows elsewhere) — reused here for the volume fill
/// and the "on" chip state so Quick Settings reads as part of the same
/// design language instead of introducing a second accent color.

/// Below this battery percentage the glyph and text turn a warning red
/// rather than the normal foreground color.
const QS_LOW_BATTERY_PERCENT: u8 = 20;
/// Room the Wi-Fi page keeps at the bottom of the card for its three
/// links, measured up from the card's bottom edge.
const QS_WIFI_FOOTER_HEIGHT: i32 = 86;
/// Where the network list starts, below the back-button header.
const QS_NETWORK_ROWS_TOP: i32 = 96;
/// Row-to-row distance in the network list; each row is 36 tall.
const QS_NETWORK_ROW_PITCH: i32 = 40;
/// Segoe Fluent's padlock, marking a secured network.
const QS_LOCK_GLYPH: &str = "\u{E72E}";

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

    // Home has no title — Windows' quick settings does not label itself,
    // and the band was 40px of nothing. Detail pages still draw a header
    // in the same space, because a back button belongs there.
    let chips_top = pad + scaled(QS_HEADER_HEIGHT, dpi);
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

/// Paints the panel through the Direct2D backend, onto the window's
/// composition surface.
///
/// Same `render_panel` as the GDI path — only the `Canvas` differs. The
/// surface is cleared to alpha 0 first so the DWM Acrylic behind the
/// window shows through wherever the card does not paint, and the card
/// itself is drawn at `QS_CARD_ALPHA`.
pub(crate) fn paint_quick_settings_gpu(hwnd: HWND) {
    // SAFETY: plain query on a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
    let surface = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.quick_settings_gpu.as_ref().map(|g| g as *const super::gpu::GpuSurface))
    });
    let Some(surface) = surface else { return };
    // SAFETY: the surface lives in `AppState`, which outlives this call;
    // the raw pointer exists only so `STATE`'s borrow ends before
    // `redraw`, which re-enters code that borrows `STATE` again.
    let surface = unsafe { &*surface };
    super::gpu::redraw(surface, |ctx| {
        super::gpu::clear_transparent(ctx);
        render_panel(&mut super::canvas::D2DCanvas::new(ctx, dpi), dpi);
    });
}

/// Paints the panel through the GDI backend.
///
/// Only reached when the window has no composition surface; with one,
/// `paint_quick_settings_gpu` renders the same `render_panel` through the
/// Direct2D backend instead, which is the translucent path.
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
            render_panel(&mut super::canvas::GdiCanvas::new(target, dpi), dpi);
        } else {
            let old = SelectObject(buffer, bitmap);
            render_panel(&mut super::canvas::GdiCanvas::new(buffer, dpi), dpi);
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

fn render_panel(c: &mut dyn Canvas, dpi: u32) {
    // The panel draws one size larger than the bar's body text.
    c.set_font_size(super::design::typography::BODY_PX * 7 / 6);
    let layout = qs_layout(dpi);
    let snapshot = control_state::snapshot();

    // The whole window in the card color. This used to be the color key
    // that made the un-painted margin transparent, back when the window
    // was layered; DWM now rounds the window itself and draws its shadow,
    // so the window *is* the card and a plain fill is all it needs
    // (spec §3.2).
    let card_radius = scaled(QS_CARD_RADIUS, dpi);
    // No software shadow: DWM draws the flyout's shadow outside the
    // window now, and a painted one inside the window would read as a
    // dark band against the real one (spec §3.1).
    // The window *is* the card now (the shadow margin went away with the
    // color key), so this single fill is the whole background. Translucent
    // on the Direct2D backend so the DWM Acrylic behind the window reads
    // through it; the GDI backend ignores the alpha and fills opaque,
    // which is exactly the old appearance.
    c.fill_round_rect_alpha(
        layout.card,
        card_radius,
        COLORREF(super::design::color::surface_raised()),
        QS_CARD_ALPHA,
    );

    let text_color = COLORREF(super::design::color::text());
    let muted_text_color = COLORREF(super::design::color::text_muted());
    let accent = COLORREF(super::design::color::accent());

    let page = INTERACTION.with(|i| i.borrow().page);

    // Home carries no title. Windows' own quick settings does not label
    // itself, and a heading on a panel you opened on purpose is a line
    // of furniture. Detail pages do get one, because the back button
    // beside it is a control the user needs.
    if page != Page::Home {
        let header = RECT {
            left: layout.card.left + scaled(12, dpi),
            top: layout.card.top + scaled(8, dpi),
            right: layout.card.right - scaled(12, dpi),
            bottom: layout.card.top + scaled(QS_DETAIL_HEADER_HEIGHT, dpi),
        };
        let back = scaled(20, dpi);
        let mid = (header.top + header.bottom) / 2;
        c.set_text_color(text_color);
        c.glyph(
            RECT {
                left: header.left + scaled(4, dpi),
                top: mid - back / 2,
                right: header.left + scaled(4, dpi) + back,
                bottom: mid + back / 2,
            },
            super::glyph::CHEVRON_LEFT,
        );
        c.text(
            RECT { left: header.left + scaled(32, dpi), ..header },
            match page {
                Page::Wifi => "Wi-Fi",
                Page::Bluetooth => "Bluetooth",
                Page::Theme => "Appearance",
                Page::Airplane => "Radios",
                Page::Sound => "Sound output",
                Page::Home => "",
            },
            DT_SINGLELINE | DT_VCENTER,
        );
    }

    if page != Page::Home {
        paint_details(c, dpi, page);
        return;
    }

    let draw_chip = |c: &mut dyn Canvas, on: bool, available: bool, rect: RECT, icon_fn: &dyn Fn(&mut dyn Canvas), label: &str| {
        let bg = if on {
            COLORREF(super::design::color::accent())
        } else {
            COLORREF(super::design::color::surface_overlay())
        };
        let radius = scaled(QS_CHIP_RADIUS, dpi);
        c.fill_round_rect( rect, radius, bg);
        if on {
            // A thin accent ring around the active chip so "on"
            // reads as more than just a slightly different gray.
            c.stroke_round_rect(rect, radius, accent, 2.0);
        }

        c.set_text_color(
            if !available {
                muted_text_color
            } else if on {
                COLORREF(super::design::color::accent_text())
            } else {
                text_color
            },
        );
        icon_fn(c);
        c.text(
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
        c,
        wifi_on.unwrap_or(false),
        wifi_on.is_some(),
        layout.wifi_chip,
        &|c: &mut dyn Canvas| {
            let color = chip_foreground(wifi_on.unwrap_or(false), wifi_on.is_some());
            let icon = if wifi_on.unwrap_or(false) {
                Icon::Wifi
            } else {
                Icon::WifiOff
            };
            c.icon_colored( wifi_icon_rect, icon, color);
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
        c,
        light == Some(false),
        light.is_some(),
        layout.theme_chip,
        &|c: &mut dyn Canvas| {
            let color = chip_foreground(light == Some(false), light.is_some());
            let icon = if light == Some(false) {
                Icon::Moon
            } else {
                Icon::Sun
            };
            c.icon_colored( theme_icon_rect, icon, color);
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
        c,
        bluetooth_state.unwrap_or(false),
        bluetooth_state.is_some(),
        layout.bluetooth_chip,
        &|c: &mut dyn Canvas| {
            let color =
                chip_foreground(bluetooth_state.unwrap_or(false), bluetooth_state.is_some());
            let icon = if bluetooth_state.unwrap_or(false) {
                Icon::Bluetooth
            } else {
                Icon::BluetoothOff
            };
            c.icon_colored( bluetooth_icon_rect, icon, color);
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
        c,
        airplane_state.unwrap_or(false),
        airplane_state.is_some(),
        layout.airplane_chip,
        &|c: &mut dyn Canvas| {
            let color = chip_foreground(airplane_state.unwrap_or(false), airplane_state.is_some());
            c.icon_colored( airplane_icon_rect, Icon::Plane, color);
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
    c.set_text_color( text_color);
    c.icon_colored(
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
    c.fill_round_rect(
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
        c.fill_round_rect( fill_rect, track_h / 2, accent);
    }
    let thumb_r = scaled(7, dpi);
    let thumb_cy = (track.top + track.bottom) / 2;
    c.fill_ellipse(
        RECT {
            left: fill_right - thumb_r,
            top: thumb_cy - thumb_r,
            right: fill_right + thumb_r,
            bottom: thumb_cy + thumb_r,
        },
        COLORREF(super::design::color::text()),
    );

    c.set_text_color( text_color);
    c.text(
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
    c.icon_colored(
        battery_icon_rect,
        battery_icon(battery_pct, battery_charging),
        battery_color,
    );
    c.set_text_color( battery_color);
    c.text(
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
        c.set_text_color( chip_foreground(on, true));
        c.text(
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
        c.fill_rect(divider, chip_foreground(on, true));
    }
    let targets = targets(dpi, Page::Home);
    c.set_text_color( text_color);
    c.text(
        targets[9],
        "Sound output   ›",
        DT_SINGLELINE | DT_VCENTER,
    );
    c.text(
        targets[11],
        "Windows settings   ›",
        DT_SINGLELINE | DT_VCENTER,
    );
    draw_feedback(c, dpi);
    draw_focus(c, dpi, Page::Home);
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

/// The Segoe Fluent bar glyph for a signal strength percentage.
///
/// Windows shows bars, not a number: "97%" is a diagnostic, and reading
/// it costs more attention than glancing at four bars. Four buckets,
/// clamped, so anything out of range still resolves to a real glyph.
fn signal_glyph(percent: u8) -> &'static str {
    match percent {
        0..=24 => "\u{E904}",
        25..=49 => "\u{E905}",
        50..=74 => "\u{E906}",
        _ => "\u{E907}",
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
        let count = visible_networks(dpi).len();
        let mut rows = vec![RECT {
            left: l.card.left + scaled(12, dpi),
            top: l.card.top + scaled(8, dpi),
            right: l.card.right - scaled(12, dpi),
            bottom: l.card.top + scaled(44, dpi),
        }];
        for n in 0..count {
            let top =
                l.card.top + scaled(QS_NETWORK_ROWS_TOP + n as i32 * QS_NETWORK_ROW_PITCH, dpi);
            rows.push(RECT {
                left: l.card.left + scaled(16, dpi),
                top,
                right: l.card.right - scaled(16, dpi),
                bottom: top + scaled(36, dpi),
            });
        }
        // Anchored to the card's bottom edge, not a fixed offset from its
        // top: the old `card.top + 310` happened to fit the card height of
        // the day and escaped it the moment that changed.
        let top = l.card.bottom - scaled(QS_WIFI_FOOTER_HEIGHT, dpi);
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
            top: top + scaled(40, dpi),
            right: l.card.right - scaled(16, dpi),
            bottom: top + scaled(68, dpi),
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
    // Directly under "Sound output", not pinned to the card's bottom
    // edge: pinning it left a band of nothing between the two links.
    let sound_bottom = l.battery_row.bottom + scaled(32, dpi);
    rows.push(RECT {
        left: l.card.left + scaled(16, dpi),
        right: l.card.right - scaled(16, dpi),
        top: sound_bottom + scaled(4, dpi),
        bottom: sound_bottom + scaled(32, dpi),
    });
    rows.push(l.volume_track);
    rows
}

fn hit_target(dpi: u32, page: Page, x: i32, y: i32) -> Option<usize> {
    targets(dpi, page)
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

fn paint_details(c: &mut dyn Canvas, dpi: u32, page: Page) {
    if page == Page::Wifi {
        paint_networks(c, dpi);
        return;
    }
    // No explanatory paragraph: Windows does not explain this surface,
    // and the row labels already say where they go.
    for (rect, (label, _)) in targets(dpi, page)
        .into_iter()
        .skip(1)
        .zip(detail_links(page))
    {
        c.fill_round_rect(
            rect,
            scaled(6, dpi),
            COLORREF(super::design::color::surface_overlay()),
        );
        c.set_text_color(COLORREF(super::design::color::text()));
        let chevron = scaled(20, dpi);
        c.text(
            RECT {
                left: rect.left + scaled(12, dpi),
                right: rect.right - chevron - scaled(8, dpi),
                ..rect
            },
            label,
            DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS,
        );
        // The chevron is right-aligned in its own square, the way every
        // navigation row in Windows draws it, rather than trailing the
        // label wherever the text happens to end.
        let mid = (rect.top + rect.bottom) / 2;
        c.set_text_color(COLORREF(super::design::color::text_muted()));
        c.glyph(
            RECT {
                left: rect.right - chevron - scaled(8, dpi),
                top: mid - chevron / 2,
                right: rect.right - scaled(8, dpi),
                bottom: mid + chevron / 2,
            },
            super::glyph::CHEVRON_RIGHT,
        );
    }
    draw_focus(c, dpi, page);
}

fn draw_feedback(c: &mut dyn Canvas, dpi: u32) {
    let l = qs_layout(dpi);
    c.set_text_color( COLORREF(super::design::color::text_muted()));
    c.text(
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

fn draw_focus(c: &mut dyn Canvas, dpi: u32, page: Page) {
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
            c.stroke_round_rect(
                RECT { left: r.left + 1, top: r.top + 1, right: r.right - 1, bottom: r.bottom - 1 },
                scaled(8, dpi) / 2,
                COLORREF(color),
                scaled(1, dpi).max(1) as f32,
            );
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
    // SAFETY: plain query on a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
    if page == Page::Wifi {
        let networks = visible_networks(dpi);
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
        // Not `Some(0)`: seeding focus here drew the accent ring around
        // every detail page's back button whether or not the keyboard
        // was in use, which tells the user nothing. Tab sets it.
        i.focus = None;
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

/// How many network rows fit between the list's top and the links at
/// the bottom of the card.
///
/// Derived rather than fixed: it used to be a hardcoded 5, which fit the
/// card height of the day and started overlapping the links the moment
/// that height changed.
fn max_visible_networks(dpi: u32) -> usize {
    let l = qs_layout(dpi);
    let first_top = l.card.top + scaled(QS_NETWORK_ROWS_TOP, dpi);
    let footer_top = l.card.bottom - scaled(QS_WIFI_FOOTER_HEIGHT, dpi);
    let pitch = scaled(QS_NETWORK_ROW_PITCH, dpi).max(1);
    (((footer_top - first_top) / pitch).max(0)) as usize
}

fn visible_networks(dpi: u32) -> Vec<super::wifi::Network> {
    let max = max_visible_networks(dpi);
    let networks = control_state::snapshot().networks;
    let offset = INTERACTION
        .with(|i| i.borrow().network_offset)
        .min(networks.len().saturating_sub(max));
    networks.into_iter().skip(offset).take(max).collect()
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

fn paint_networks(c: &mut dyn Canvas, dpi: u32) {
    let snapshot = control_state::snapshot();
    let l = qs_layout(dpi);
    let networks = visible_networks(dpi);
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
        // Nothing else: the list is visibly a list, and telling the user
        // how to scroll it is a developer's note, not a label.
        String::new()
    };
    if !summary.is_empty() {
        c.set_text_color(COLORREF(super::design::color::text_muted()));
        c.text(
            RECT {
                left: l.card.left + scaled(16, dpi),
                top: l.card.top + scaled(52, dpi),
                right: l.card.right - scaled(16, dpi),
                bottom: l.card.top + scaled(92, dpi),
            },
            &summary,
            DT_SINGLELINE | DT_VCENTER,
        );
    }
    for (network, rect) in networks.iter().zip(rows.iter().skip(1)) {
        c.fill_round_rect(
            *rect,
            scaled(6, dpi),
            COLORREF(super::design::color::surface_overlay()),
        );
        // Signal as bars, and a padlock when the network is secured —
        // the two things Windows shows, in place of a "97%" readout.
        let glyph_box = scaled(16, dpi);
        let mid = (rect.top + rect.bottom) / 2;
        c.set_text_color(COLORREF(super::design::color::text()));
        c.glyph(
            RECT {
                left: rect.left + scaled(10, dpi),
                top: mid - glyph_box / 2,
                right: rect.left + scaled(10, dpi) + glyph_box,
                bottom: mid + glyph_box / 2,
            },
            signal_glyph(network.signal.min(255) as u8),
        );
        if network.secured {
            c.set_text_color(COLORREF(super::design::color::text_muted()));
            c.glyph(
                RECT {
                    left: rect.left + scaled(28, dpi),
                    top: mid - glyph_box / 2,
                    right: rect.left + scaled(28, dpi) + glyph_box,
                    bottom: mid + glyph_box / 2,
                },
                QS_LOCK_GLYPH,
            );
        }
        c.set_text_color(COLORREF(super::design::color::text()));
        let name = if network.name.is_empty() {
            "Hidden network"
        } else {
            &network.name
        };
        c.text(
            RECT {
                left: rect.left + scaled(48, dpi),
                right: rect.right - scaled(110, dpi),
                ..*rect
            },
            name,
            DT_SINGLELINE
                | DT_VCENTER
                | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS
                | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
        );
        // One word, one line. "Set up securely" wrapped and spilled into
        // the row below it; the padlock already says "secured".
        let detail = if network.connected {
            "Disconnect"
        } else if !network.profile.is_empty() && network.connectable {
            "Connect"
        } else {
            "Set up"
        };
        c.set_text_color(
            COLORREF(if network.connected {
                super::design::color::accent()
            } else {
                super::design::color::text_muted()
            }),
        );
        c.text(
            RECT {
                left: rect.right - scaled(110, dpi),
                right: rect.right - scaled(10, dpi),
                ..*rect
            },
            detail,
            DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_RIGHT,
        );
    }
    c.set_text_color( COLORREF(super::design::color::text()));
    for (rect, label) in rows.iter().skip(networks.len() + 1).zip([
        "Refresh",
        "Windows networks & passwords",
        "Manage saved networks",
    ]) {
        c.text( *rect, label, DT_SINGLELINE | DT_VCENTER);
    }
    draw_focus(c, dpi, Page::Wifi);
}

/// Resizes `hwnd` to the flyout's current unroll height (spec §3.4).
///
/// The window's top stays pinned under the bar and only its height moves,
/// so the content — laid out against the fixed `QS_HEIGHT` by
/// [`qs_layout`], never against the client rect — stays put while the
/// window grows over it. The existing double buffer in [`paint`] is sized
/// from `GetClientRect`, so during the unroll it is simply shorter and
/// clips the overflow for free; no per-pixel alpha and no layered window
/// are involved, which is what lets the backdrop material composite.
fn apply_reveal(hwnd: HWND, progress: f32) {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) };
    let full = scaled(QS_HEIGHT + QS_SHADOW_MARGIN * 2, dpi);
    let width = scaled(QS_WIDTH + QS_SHADOW_MARGIN * 2, dpi);
    let height =
        INTERACTION.with(|i| i.borrow().motion.reveal_extent(progress, full));
    // SAFETY: `hwnd` is a live, process-lifetime window. A zero height is
    // legal for `SetWindowPos`; the window is hidden by the caller once
    // the phase reaches `Hidden`.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height.max(0),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub(crate) fn mouse_leave(hwnd: HWND) {
    INTERACTION.with(|i| i.borrow_mut().hover = None);
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub(crate) fn tick(hwnd: HWND) {
    let changed = control_state::poll();
    let progress = INTERACTION.with(|i| {
        let mut i = i.borrow_mut();
        if !i.motion.is_animating() {
            return None;
        }
        Some(i.motion.tick(std::time::Instant::now()))
    });
    unsafe {
        if let Some(p) = progress {
            apply_reveal(hwnd, p);
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
            SetTimer(hwnd, QS_TIMER_ID, super::design::motion::FRAME_INTERVAL_MS, None);
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
        // Start the unroll at its first frame so the window is never shown
        // at full height for one frame before the animation takes over
        // (spec §3.4). With reduced motion the flyout is already `Open`,
        // so this is the full height immediately.
        apply_reveal(hwnd, 0.0);
        SetTimer(hwnd, QS_TIMER_ID, super::design::motion::FRAME_INTERVAL_MS, None);
        let _ = InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    const PAGES: [Page; 6] = [
        Page::Home,
        Page::Wifi,
        Page::Bluetooth,
        Page::Theme,
        Page::Airplane,
        Page::Sound,
    ];
    const DPIS: [u32; 4] = [96, 120, 144, 192];

    #[test]
    fn no_two_targets_on_a_page_overlap() {
        for dpi in DPIS {
            for page in PAGES {
                let rows = targets(dpi, page);
                for (i, a) in rows.iter().enumerate() {
                    for b in rows.iter().skip(i + 1) {
                        let disjoint = a.right <= b.left
                            || b.right <= a.left
                            || a.bottom <= b.top
                            || b.bottom <= a.top;
                        assert!(disjoint, "targets overlap at {dpi} dpi: {a:?} and {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_target_sits_inside_the_card() {
        for dpi in DPIS {
            for page in PAGES {
                let card = qs_layout(dpi).card;
                for r in targets(dpi, page) {
                    assert!(
                        r.left >= card.left && r.right <= card.right,
                        "target escapes the card horizontally at {dpi} dpi: {r:?} vs {card:?}"
                    );
                    assert!(
                        r.top >= card.top && r.bottom <= card.bottom,
                        "target escapes the card vertically at {dpi} dpi: {r:?} vs {card:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn hit_testing_agrees_with_the_layout_on_every_page() {
        for dpi in DPIS {
            for page in PAGES {
                for (index, r) in targets(dpi, page).into_iter().enumerate() {
                    let x = (r.left + r.right) / 2;
                    let y = (r.top + r.bottom) / 2;
                    assert_eq!(
                        hit_target(dpi, page, x, y),
                        Some(index),
                        "centre of target {index} on {page:?} did not hit it at {dpi} dpi"
                    );
                }
            }
        }
    }

    #[test]
    fn the_home_chips_form_two_even_rows() {
        for dpi in DPIS {
            let l = qs_layout(dpi);
            assert_eq!(l.wifi_chip.bottom - l.wifi_chip.top, l.theme_chip.bottom - l.theme_chip.top);
            assert_eq!(l.wifi_chip.top, l.theme_chip.top);
            assert_eq!(l.bluetooth_chip.top, l.airplane_chip.top);
            assert!(l.bluetooth_chip.top >= l.wifi_chip.bottom, "chip rows collide at {dpi} dpi");
            assert_eq!(
                l.wifi_chip.right - l.wifi_chip.left,
                l.theme_chip.right - l.theme_chip.left,
                "chips are uneven at {dpi} dpi"
            );
        }
    }

    /// A populated Wi-Fi page. The overlap this catches was invisible to
    /// the other layout tests, because with no snapshot the network list
    /// is empty and the rows that collide never exist.
    fn with_networks(count: usize) {
        let mut snapshot = control_state::Snapshot::default();
        snapshot.networks = (0..count)
            .map(|i| super::super::wifi::Network {
                name: format!("Network {i}"),
                signal: 80,
                secured: true,
                connected: false,
                connectable: true,
                profile: String::new(),
            })
            .collect();
        control_state::set_test_snapshot(snapshot);
    }

    #[test]
    fn a_full_network_list_never_collides_with_the_links_below_it() {
        with_networks(12);
        for dpi in DPIS {
            let rows = targets(dpi, Page::Wifi);
            for (i, a) in rows.iter().enumerate() {
                for b in rows.iter().skip(i + 1) {
                    let disjoint = a.right <= b.left
                        || b.right <= a.left
                        || a.bottom <= b.top
                        || b.bottom <= a.top;
                    assert!(disjoint, "Wi-Fi rows overlap at {dpi} dpi: {a:?} and {b:?}");
                }
            }
            let card = qs_layout(dpi).card;
            for r in rows {
                assert!(r.bottom <= card.bottom, "row {r:?} runs past the card at {dpi} dpi");
            }
        }
        control_state::set_test_snapshot(control_state::Snapshot::default());
    }

    #[test]
    fn the_visible_network_count_is_what_actually_fits() {
        for dpi in DPIS {
            let count = max_visible_networks(dpi);
            assert!(count > 0, "no room for any network row at {dpi} dpi");
            let l = qs_layout(dpi);
            let last_bottom = l.card.top
                + scaled(QS_NETWORK_ROWS_TOP + (count as i32 - 1) * QS_NETWORK_ROW_PITCH + 36, dpi);
            let footer_top = l.card.bottom - scaled(QS_WIFI_FOOTER_HEIGHT, dpi);
            assert!(
                last_bottom <= footer_top,
                "{count} rows reach {last_bottom} but the footer starts at {footer_top} at {dpi} dpi"
            );
        }
    }

    #[test]
    fn each_signal_bucket_has_its_own_glyph() {
        // One sample per bucket: all four must look different, or the
        // bars stop carrying information.
        let glyphs: Vec<&str> = [10, 35, 60, 90].iter().map(|p| signal_glyph(*p)).collect();
        for (i, a) in glyphs.iter().enumerate() {
            for b in glyphs.iter().skip(i + 1) {
                assert_ne!(a, b, "two buckets share a glyph: {glyphs:?}");
            }
        }
    }

    #[test]
    fn the_bucket_boundaries_fall_where_they_are_documented() {
        assert_eq!(signal_glyph(0), signal_glyph(24));
        assert_ne!(signal_glyph(24), signal_glyph(25));
        assert_eq!(signal_glyph(25), signal_glyph(49));
        assert_ne!(signal_glyph(49), signal_glyph(50));
        assert_eq!(signal_glyph(50), signal_glyph(74));
        assert_ne!(signal_glyph(74), signal_glyph(75));
    }

    #[test]
    fn an_out_of_range_strength_still_resolves() {
        assert_eq!(signal_glyph(100), signal_glyph(255));
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
        // The visible count is whatever fits above the links now, not a
        // fixed 5 — but a stale offset must still land on the last page
        // of entries rather than past the end.
        let fits = max_visible_networks(96);
        control_state::set_test_snapshot(example_snapshot());
        INTERACTION.with(|i| i.borrow_mut().network_offset = 100);
        assert_eq!(visible_networks(96).len(), fits);
        assert_eq!(visible_networks(96).last().unwrap().name, "Other network");
        control_state::set_test_snapshot(control_state::Snapshot::default());
        assert!(visible_networks(96).is_empty());
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
                    render_panel(&mut crate::imp::canvas::GdiCanvas::new(dc, dpi), dpi);
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
