//! The top bar's Direct2D / DirectComposition painter.
//!
//! `bar.rs` owns the bar's layout, hit-testing, and the GDI painter that
//! remains the fallback on machines without the GPU path. This module is
//! the second painter, and it exists for one reason: GDI has no alpha
//! channel, so every pixel it writes is opaque and the Mica backdrop
//! behind the bar can never show through. Painting the same bar into a
//! DirectComposition surface — which is premultiplied-alpha — lets the bar
//! clear itself to transparent and let the system material *be* its
//! background.
//!
//! Both painters read the same layout helpers and the same hit-testing in
//! `bar::region_at`, so they can never disagree about where anything is.
//! Only the drawing calls differ.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

use super::bar::{
    centered_square, qs_pill_layout, session_button_rect, settings_button_rect, tray_chevron_rect,
    tray_icon_rect, BarRegion, ACTIVITIES_LABEL_WIDTH, ACTIVITIES_LABEL_X, BAR_HOVER_ALPHA,
    CLOCK_LABEL_WIDTH, QS_ICON_SIZE, QS_PILL_RADIUS, SESSION_GLYPH, SETTINGS_GLYPH,
    TRAY_CHEVRON_GLYPH, WS_DOTS_X, WS_DOT_RADIUS, WS_DOT_SLOT_WIDTH,
};
use super::calendar::clock_text;
use super::icons::{battery_icon, fluent_glyph, volume_icon, Icon};
use super::quick_settings::{battery_status, get_mute, get_volume_percent};
use super::state::{scaled, STATE};
use super::util::blend_toward_white;

fn to_d2d(r: RECT) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}

/// Whether the Direct2D bar painter can be used.
///
/// Needs the GPU path, and needs the Segoe Fluent Icons face: this painter
/// draws every bar icon as a glyph rather than decoding the bundled PNGs,
/// so without that face it would render tofu. Every icon the bar actually
/// shows (wifi, volume, battery) has a verified glyph, so this is only
/// ever false on pre-Windows-11 machines, which also lack the backdrop.
pub(crate) fn available() -> bool {
    super::gpu::is_enabled() && super::design::typography::icon_font_available()
}

/// Everything the painter needs out of `STATE`, read in one borrow.
///
/// `gpu::redraw` runs its closure while the surface is mapped and
/// re-enters code that borrows `STATE` again; a nested borrow panics, and
/// that class of crash is a recurring one in this project. So the snapshot
/// is taken first and the borrow released before drawing starts.
struct BarSnapshot {
    width: i32,
    hovered: Option<BarRegion>,
    workspace_count: usize,
    current_index: usize,
}

/// Paints one bar into its DirectComposition surface.
pub(crate) fn paint(hwnd: HWND, is_primary: bool, monitor: &str) {
    // SAFETY: plain query on a live, process-lifetime window.
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
    let bar_h = scaled(super::state::BAR_HEIGHT, dpi);

    let snapshot = STATE.with(|s| {
        let state = s.borrow();
        let st = state.as_ref()?;
        let bar = st.bars.iter().find(|b| b.hwnd == hwnd)?;
        let (workspace_count, current_index) = st
            .workspaces
            .get(monitor)
            .map(|t| (t.workspace_ids().len(), t.current_index()))
            .unwrap_or((0, 0));
        Some(BarSnapshot {
            width: bar.rect.right - bar.rect.left,
            hovered: st
                .hovered_bar_region
                .filter(|(hover_hwnd, _)| *hover_hwnd == hwnd)
                .map(|(_, region)| region),
            workspace_count,
            current_index,
        })
    });
    let Some(snapshot) = snapshot else { return };

    // Indicator reads also touch COM/registry and must not happen inside
    // the draw closure either.
    let clock = is_primary.then(clock_text);
    let indicators = is_primary.then(|| {
        let wifi = if super::control_state::snapshot().wifi.unwrap_or(false) {
            Icon::Wifi
        } else {
            Icon::WifiOff
        };
        let volume = volume_icon(get_mute().unwrap_or(false), get_volume_percent().unwrap_or(0));
        let battery = battery_status().map(|(pct, charging)| battery_icon(pct, charging));
        (wifi, volume, battery)
    });

    let surface = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.bars.iter().find(|b| b.hwnd == hwnd))
            .and_then(|b| b.gpu.as_ref().map(|g| g as *const super::gpu::GpuSurface))
    });
    let Some(surface) = surface else { return };
    // SAFETY: the surface lives in `AppState`, which outlives this call —
    // a bar is only dropped during monitor teardown, which cannot run
    // while this paint is on the stack. The raw pointer exists purely so
    // `STATE`'s borrow ends before `redraw` re-borrows it.
    let surface = unsafe { &*surface };

    super::gpu::redraw(surface, |ctx| {
        // Clear to alpha 0 rather than filling a background: this is what
        // lets the Mica backdrop be the bar's background. The GDI painter
        // has to fill, which is exactly why it cannot be translucent.
        super::gpu::clear_transparent(ctx);

        let text = super::design::color::text();
        let body = scaled(super::design::typography::BODY_PX, dpi) as f32;
        let face = super::design::typography::resolved_ui_face();
        let icon_face = super::design::typography::ICON_FACE;

        let hover = |ctx: &ID2D1DeviceContext, rect: RECT, radius: i32| {
            super::gpu::fill_rounded_rect_alpha(
                ctx,
                to_d2d(rect),
                radius as f32,
                blend_toward_white(super::design::color::surface_base(), 0.15).0,
                BAR_HOVER_ALPHA,
            );
        };
        let glyph = |ctx: &ID2D1DeviceContext, rect: RECT, s: &str| {
            let size = (rect.bottom - rect.top).max(1) as f32;
            super::gpu::draw_text_in_font(ctx, to_d2d(rect), s, text, size, true, icon_face);
        };

        let activities_rect = RECT {
            left: scaled(ACTIVITIES_LABEL_X, dpi),
            top: 0,
            right: scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi),
            bottom: bar_h,
        };
        if snapshot.hovered == Some(BarRegion::Activities) {
            hover(ctx, activities_rect, scaled(6, dpi));
        }
        super::gpu::draw_text_in_font(
            ctx,
            to_d2d(activities_rect),
            "Activities",
            text,
            body,
            true,
            face,
        );

        if snapshot.hovered == Some(BarRegion::Dots) && snapshot.workspace_count > 0 {
            let dots_rect = RECT {
                left: scaled(WS_DOTS_X, dpi),
                top: 0,
                right: scaled(WS_DOTS_X, dpi)
                    + snapshot.workspace_count as i32 * scaled(WS_DOT_SLOT_WIDTH, dpi),
                bottom: bar_h,
            };
            hover(ctx, dots_rect, scaled(6, dpi));
        }
        let dot_mid_y = bar_h / 2;
        let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
        let dot_radius = scaled(WS_DOT_RADIUS, dpi);
        for i in 0..snapshot.workspace_count {
            let cx = scaled(WS_DOTS_X, dpi) + i as i32 * dot_slot_w + dot_slot_w / 2;
            let color = if i == snapshot.current_index {
                super::design::color::accent()
            } else {
                super::design::color::text_muted()
            };
            // A circle is a rounded rect whose radius is half its side, so
            // no separate ellipse primitive is needed.
            let dot = RECT {
                left: cx - dot_radius,
                top: dot_mid_y - dot_radius,
                right: cx + dot_radius,
                bottom: dot_mid_y + dot_radius,
            };
            super::gpu::fill_rounded_rect(ctx, to_d2d(dot), dot_radius as f32, color);
        }

        let (Some(clock), Some((wifi_icon, vol_icon, battery))) = (clock.as_deref(), indicators)
        else {
            return;
        };

        let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
        let clock_x = snapshot.width / 2 - clock_w / 2;
        let clock_rect = RECT { left: clock_x, top: 0, right: clock_x + clock_w, bottom: bar_h };
        if snapshot.hovered == Some(BarRegion::Clock) {
            hover(ctx, clock_rect, scaled(6, dpi));
        }
        super::gpu::draw_text_in_font(ctx, to_d2d(clock_rect), clock, text, body, true, face);

        let (pill, slots) = qs_pill_layout(snapshot.width, dpi, bar_h);
        if snapshot.hovered == Some(BarRegion::QsPill) {
            hover(ctx, pill, scaled(QS_PILL_RADIUS, dpi));
        }

        if let Some(g) = fluent_glyph(wifi_icon) {
            glyph(ctx, slots[0], g);
        }
        if let Some(g) = fluent_glyph(vol_icon) {
            glyph(ctx, slots[1], g);
        }
        match battery.and_then(fluent_glyph) {
            Some(g) => glyph(ctx, slots[2], g),
            None => {
                super::gpu::draw_text_in_font(ctx, to_d2d(slots[2]), "AC", text, body, true, face)
            }
        }

        let settings_rect = settings_button_rect(pill, dpi, bar_h);
        if snapshot.hovered == Some(BarRegion::SettingsGear) {
            hover(ctx, settings_rect, scaled(6, dpi));
        }
        glyph(ctx, centered_square(settings_rect, scaled(QS_ICON_SIZE, dpi)), SETTINGS_GLYPH);

        let session_rect = session_button_rect(settings_rect, dpi, bar_h);
        if snapshot.hovered == Some(BarRegion::SessionButton) {
            hover(ctx, session_rect, scaled(6, dpi));
        }
        glyph(ctx, centered_square(session_rect, scaled(QS_ICON_SIZE, dpi)), SESSION_GLYPH);

        let chevron = tray_chevron_rect(session_rect, dpi, bar_h);
        if snapshot.hovered == Some(BarRegion::TrayChevron) {
            hover(ctx, chevron, scaled(6, dpi));
        }
        super::gpu::draw_text_in_font(
            ctx,
            to_d2d(chevron),
            TRAY_CHEVRON_GLYPH,
            text,
            body,
            true,
            face,
        );
        for index in 0..super::tray_icons::count() {
            let rect = tray_icon_rect(chevron, dpi, index);
            if snapshot.hovered == Some(BarRegion::TrayIcon(index)) {
                hover(ctx, rect, scaled(4, dpi));
            }
            super::tray_icons::with_pixels(index, |pixels, size| {
                if let Some(bitmap) = super::gpu::bitmap_from_bgra(ctx, pixels, size) {
                    super::gpu::draw_bitmap_stretched(ctx, to_d2d(rect), &bitmap);
                }
            });
        }
    });
}

/// Creates one bar window, with a composition surface when the GPU path
/// is available, and applies the Mica backdrop.
///
/// `WS_EX_NOREDIRECTIONBITMAP` is what makes the backdrop visible: it
/// drops the window's opaque GDI redirection bitmap, which DWM would
/// otherwise composite over the material. It is only safe once a surface
/// actually exists — a window without a redirection bitmap cannot be
/// painted by GDI at all, so a bar that failed to get a surface would be
/// invisible rather than merely opaque. The per-window surface is
/// therefore re-checked, and the window rebuilt opaque if it failed.
pub(crate) fn create_bar_window(
    hinstance: windows::Win32::Foundation::HINSTANCE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> windows::core::Result<(HWND, Option<super::gpu::GpuSurface>)> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    let make = |translucent: bool| {
        let ex = if translucent {
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP
        } else {
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
        };
        // SAFETY: standard top-most tool-window creation; `hinstance` is
        // the process module handle.
        unsafe {
            CreateWindowExW(
                ex,
                w!("GroveShellBar"),
                w!("GroveShell Top Bar"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                width,
                height,
                None,
                None,
                hinstance,
                None,
            )
        }
    };

    let translucent = available();
    let mut hwnd = make(translucent)?;
    let mut surface = if translucent {
        super::gpu::create_surface(hwnd, width, height)
    } else {
        None
    };
    if translucent && surface.is_none() {
        tracing::warn!("bar DirectComposition surface failed; rebuilding the window opaque");
        // SAFETY: `hwnd` was created just above and has no references yet.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        hwnd = make(false)?;
        surface = None;
    }

    // Mica, plus the immersive dark-mode tint. Harmless when the window
    // is opaque: the attribute is simply not visible there.
    super::design::material::apply(hwnd, super::design::material::Surface::Bar);
    Ok((hwnd, surface))
}
