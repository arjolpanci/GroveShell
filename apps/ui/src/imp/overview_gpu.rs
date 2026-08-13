//! GPU-composited visual tree for one monitor's Activities overview —
//! Phase 2 of the animation-fluidity work (see
//! `docs/superpowers/specs/2026-07-29-overview-directcomposition-port-design.md`).
//! One root visual (dock/search/ghost chrome) plus one child visual per
//! current `CardAnim` (frame/wallpaper/shadow/thumbnails/icons/chips).
//! `None`/absent anywhere in this module means "fall back to this
//! overview window's existing GDI painting, unchanged" — never a panic.

use std::ffi::c_void;

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL, HICON};

use super::gpu::{self, GpuSurface};
use super::overview::{
    card_layout, displayed_rect, zoom_rect, CardAnim, ThumbAnim, WINDOW_HOVER_GLOW_DURATION, WINDOW_POP_DURATION,
};
use super::state::{reference_dpi, scaled};
use super::util::{ease_out, progress_dur};

/// One workspace card's own composited surface, at the card's natural
/// (unscrolled, unzoomed) size — carousel position/zoom is applied on
/// top via `SetTransform2`, never baked into what's drawn here.
pub(crate) struct CardVisual {
    pub(crate) page: usize,
    pub(crate) surface: GpuSurface,
}

/// One monitor overview's GPU state. `root`'s own surface is left
/// blank — it exists only as the composition-tree parent that carries
/// the open/close opacity fade (`gpu::set_opacity`) down to its
/// children. `cards` holds one entry per current `CardAnim`, kept in
/// sync by `rebuild_cards`. `chrome` holds the dock bar, search panel,
/// and drag ghost, and is kept re-inserted above every card visual (see
/// `rebuild_cards`) so it always composites on top — a card is a full,
/// opaque rounded rect, and without this the drag ghost and search
/// panel would render underneath whichever card visual they overlap.
pub(crate) struct OverviewGpuState {
    pub(crate) root: GpuSurface,
    pub(crate) cards: Vec<CardVisual>,
    pub(crate) chrome: GpuSurface,
}

/// Creates the root/chrome surfaces for `hwnd` (both sized to the
/// overview window's full client area). Returns `None` if the
/// process-wide GPU setup isn't available, or if this specific
/// window's target/surface setup fails — either way the caller keeps
/// using GDI for this monitor's overview, unchanged.
pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<OverviewGpuState> {
    let root = gpu::create_surface(hwnd, width, height)?;
    let chrome = gpu::create_surface(hwnd, width, height)?;
    // SAFETY: `root`/`chrome` were both just created above by this
    // module's own `gpu::create_surface` and are alive for as long as
    // the `OverviewGpuState` returned below is. `AddVisual` on a fresh,
    // never-attached visual simply attaches it.
    unsafe {
        let _ = root.visual().AddVisual(chrome.visual(), true, None);
    }
    Some(OverviewGpuState { root, cards: Vec::new(), chrome })
}

/// Rebuilds `state.cards` to have exactly one entry per `cards`,
/// reusing each existing card visual's surface when a page's rect size
/// hasn't changed (avoids a needless surface recreation on every
/// `build_carousel_pages` call when only window contents changed, not
/// layout). Cards are recreated in `cards`' order, matching how
/// `on_animation_tick`'s per-tick transform pass below will iterate
/// them.
pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, hwnd: HWND, cards: &[CardAnim]) {
    let mut old_cards = std::mem::take(&mut state.cards);
    let mut rebuilt = Vec::with_capacity(cards.len());
    for card in cards {
        let (w, h) = (card.rect.right - card.rect.left, card.rect.bottom - card.rect.top);
        let reused = old_cards
            .iter()
            .position(|cv| cv.page == card.page)
            .map(|i| old_cards.remove(i))
            .filter(|cv| cv.surface.width() == w && cv.surface.height() == h);
        let card_visual = match reused {
            Some(cv) => cv,
            None => match gpu::create_surface(hwnd, w.max(1), h.max(1)) {
                Some(surface) => CardVisual { page: card.page, surface },
                None => continue,
            },
        };
        rebuilt.push(card_visual);
    }
    // Anything left in `old_cards` here is a page that no longer
    // exists, or one whose surface size changed and so got rejected by
    // the `filter` above — either way it's about to be dropped. Dropping
    // the Rust wrapper only releases *our* COM reference; `state.root`'s
    // visual still holds its own separate reference from the `AddVisual`
    // call below (a previous invocation of this same function), so
    // without an explicit `RemoveVisual` the now-orphaned child visual
    // would never leave the composition tree — it'd leak for the rest of
    // this monitor's overview lifetime and keep rendering stale, frozen
    // content that no longer tracks the carousel.
    for stale in &old_cards {
        // SAFETY: `stale.surface.visual()` was attached to
        // `state.root.visual()` by an earlier call to this function (or
        // never attached, in which case `RemoveVisual` is a documented
        // no-op). `state.root` is alive for as long as `state` is.
        unsafe {
            let _ = state.root.visual().RemoveVisual(stale.surface.visual());
        }
    }
    state.cards = rebuilt;
    for card_visual in &state.cards {
        // SAFETY: both visuals belong to the same `IDCompositionDesktopDevice`;
        // `root.visual()`/`card_visual.surface.visual()` are alive for as
        // long as `state` is. `AddVisual` on an already-attached visual is
        // a harmless no-op.
        unsafe {
            let _ = state.root.visual().AddVisual(card_visual.surface.visual(), true, None);
        }
    }
    // Re-insert `chrome` above every card just attached above — each
    // `AddVisual(_, true, None)` call inserts its visual at the very top
    // of `root`'s children, so whichever visual is added last ends up on
    // top; this keeps the dock/search/drag-ghost chrome above every card,
    // regardless of card add/remove order this call.
    // SAFETY: `state.chrome.visual()` was created alongside `state.root`
    // in `create` and is alive for as long as `state` is.
    unsafe {
        let _ = state.root.visual().AddVisual(state.chrome.visual(), true, None);
    }
}

/// Applies this tick's carousel position/zoom to every card visual —
/// the GPU-path replacement for `repaint_overview`'s full redraw.
/// `card_layout`/`displayed_rect`/`zoom_rect` are the exact same pure
/// functions `paint_overview` already used; only what happens with
/// their result changes (a transform instead of a redraw).
pub(crate) fn update_transforms(
    gpu: &OverviewGpuState,
    monitor: &str,
    carousel_offset: f64,
    zoom: f64,
    anchor: (f64, f64),
) {
    let (card_rect, pitch) = card_layout(monitor);
    let (anchor_x, anchor_y) = anchor;
    for card_visual in &gpu.cards {
        // The card's surface was created at its own natural rect's
        // top-left; `displayed_rect`/`zoom_rect` operate in the
        // overview window's coordinate space, so the transform must
        // carry both the translate-to-displayed-position *and* the
        // uniform scale, in that surface-local-origin frame.
        let base = RECT {
            left: card_rect.left,
            top: card_rect.top,
            right: card_rect.right,
            bottom: card_rect.bottom,
        };
        let displayed = displayed_rect(base, card_visual.page, carousel_offset, pitch, card_rect);
        let zoomed = zoom_rect(displayed, anchor_x, anchor_y, zoom);
        let (base_w, base_h) = ((base.right - base.left).max(1) as f32, (base.bottom - base.top).max(1) as f32);
        let scale_x = (zoomed.right - zoomed.left) as f32 / base_w;
        let scale_y = (zoomed.bottom - zoomed.top) as f32 / base_h;
        let matrix = Matrix3x2 {
            M11: scale_x,
            M12: 0.0,
            M21: 0.0,
            M22: scale_y,
            M31: zoomed.left as f32,
            M32: zoomed.top as f32,
        };
        // SAFETY: `card_visual.surface.visual()` is alive for as long
        // as `gpu` is; `SetTransform2` takes a plain matrix pointer,
        // valid for the duration of this synchronous call.
        unsafe {
            let _ = card_visual.surface.visual().SetTransform2(&matrix);
        }
    }
    gpu::commit();
}

fn rect_to_d2d(r: RECT, origin_x: i32, origin_y: i32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: (r.left - origin_x) as f32,
        top: (r.top - origin_y) as f32,
        right: (r.right - origin_x) as f32,
        bottom: (r.bottom - origin_y) as f32,
    }
}

/// Renders an `HICON` to a small premultiplied-BGRA `HBITMAP` so it can
/// go through the same WIC bridge as window thumbnails and the wallpaper
/// — icons have no separate Direct2D-native path.
///
/// Uses a zeroed 32-bit top-down DIB section rather than a
/// `CreateCompatibleBitmap`: a compatible bitmap has no alpha channel, so
/// the icon's transparent areas came out as the DC's uninitialised gray
/// and the WIC bridge (told the source is premultiplied) then drew every
/// icon as an opaque gray square — the dock's "gray background behind the
/// icons". Drawing `DI_NORMAL` onto a transparent 32-bit DIB alpha-blends
/// a modern icon straight to premultiplied BGRA, which is exactly what
/// `bitmap_from_hbitmap` expects, so the icon reads as true transparent
/// art and only the icon (never a plate) rides the magnification wave.
pub(crate) fn icon_to_hbitmap(icon: HICON, size: i32) -> Option<HBITMAP> {
    let size = size.max(1);
    // SAFETY: standard create-select-draw-restore GDI sequence on locally
    // created handles. The DIB's pixel buffer (`bits`) stays valid for as
    // long as `bitmap` is alive, and we only touch it while `bitmap` is
    // selected out of `mem` and before handing ownership to the caller.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // negative => top-down (row 0 at the top)
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        ReleaseDC(None, screen);
        let Some(bitmap) = bitmap else {
            let _ = DeleteDC(mem);
            return None;
        };
        if bits.is_null() {
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem);
            return None;
        }

        let previous = SelectObject(mem, bitmap);
        // The DIB starts fully transparent (zeroed). `DI_NORMAL`
        // alpha-blends the icon; a modern 32-bit icon writes its own
        // premultiplied per-pixel alpha here.
        let ok = DrawIconEx(mem, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok();
        let _ = GdiFlush(); // flush GDI writes before reading the bits back
        SelectObject(mem, previous);
        let _ = DeleteDC(mem);
        if !ok {
            let _ = DeleteObject(bitmap);
            return None;
        }

        // Legacy icons carry a 1-bit AND mask instead of an alpha channel,
        // so `DI_NORMAL` leaves alpha zero everywhere (the icon would be
        // fully transparent). Detect that and treat every painted pixel as
        // opaque so such icons still show. Modern 32-bit icons never take
        // this branch.
        let count = (size as usize) * (size as usize);
        let px = std::slice::from_raw_parts_mut(bits as *mut [u8; 4], count);
        if !px.iter().any(|p| p[3] != 0) {
            for p in px.iter_mut() {
                if p[0] != 0 || p[1] != 0 || p[2] != 0 {
                    p[3] = 0xFF;
                }
            }
        }
        Some(bitmap)
    }
}

/// Which piece of hover-glow state (if any) `paint_card` should render
/// on this call — a whole-card border (a real window drag hovering
/// this card, matched by page) or a single-thumbnail border (plain
/// browsing, matched by hwnd). Hover glow lives on card content, not
/// root, per this task's design scoping — mutually exclusive in
/// practice (starting a window drag clears `hover_thumb`, and
/// `on_overview_hover` is a no-op while a drag is active).
pub(crate) enum HoverTarget {
    Card(usize),
    Thumb(isize),
}

/// Paints one card's frame (wallpaper-filled rounded rect, drop-shadow
/// approximation), thumbnails (each clipped/rounded, WIC-bridged from
/// the existing `PrintWindow` capture cache), icon badges, placeholder
/// chips, and — when `hover` names this card or one of its thumbnails —
/// the light-blue hover-glow outline. Mirrors the corresponding subset
/// of `overview::paint_overview`'s GDI drawing, at the card's own
/// natural (unscrolled) size. `thumbs` must already be filtered to this
/// card's `page`.
pub(crate) fn paint_card(
    card_visual: &CardVisual,
    card_rect: RECT,
    thumbs: &[&ThumbAnim],
    monitor: &str,
    hover: Option<(HoverTarget, f64)>,
) {
    let dpi = reference_dpi();
    let card_radius = scaled(20, dpi) as f32; // CARD_CORNER_RADIUS
    let thumb_radius = scaled(8, dpi) as f32; // THUMB_CORNER_RADIUS
    let (origin_x, origin_y) = (card_rect.left, card_rect.top);
    let full = rect_to_d2d(card_rect, origin_x, origin_y);

    gpu::redraw(&card_visual.surface, |ctx: &ID2D1DeviceContext| {
        // Flat fallback fill (shows through if the wallpaper bridge
        // below fails), then the wallpaper itself.
        gpu::fill_rounded_rect(ctx, full, card_radius, 0x00302010);
        if let Some(wallpaper) = super::overview::wallpaper_hbitmap_for(monitor, card_rect) {
            if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, wallpaper) {
                gpu::draw_rounded_bitmap(ctx, full, card_radius, &bitmap);
            }
        }

        // Whole-card hover glow: this card is the one a real window
        // drag currently sits over.
        if let Some((HoverTarget::Card(page), intensity)) = &hover {
            if *page == card_visual.page && *intensity > 0.001 {
                gpu::stroke_rounded_rect(ctx, full, card_radius, super::design::color::accent(), *intensity as f32, 2.0);
            }
        }

        for th in thumbs {
            let rect = rect_to_d2d(th.rect, origin_x, origin_y);
            let (base_w, base_h) = (th.rect.right - th.rect.left, th.rect.bottom - th.rect.top);
            let hwnd = th.hwnd.0 as isize;
            if super::overview::window_snapshot(hwnd).is_some() {
                if let Some(scaled_handle) = super::overview::slot_scaled_snapshot(hwnd, base_w, base_h) {
                    if let Some(bitmap) =
                        gpu::bitmap_from_hbitmap(ctx, HBITMAP(scaled_handle as *mut c_void))
                    {
                        gpu::draw_rounded_bitmap(ctx, rect, thumb_radius, &bitmap);
                    }
                }
            } else {
                gpu::fill_rounded_rect(ctx, rect, thumb_radius, super::design::color::surface_overlay());
                gpu::draw_text(ctx, rect, &th.title, super::design::color::text(), 13.0, true);
            }
            if let Some(icon) = th.icon {
                let icon_rect = rect_to_d2d(th.icon_rect, origin_x, origin_y);
                let size = (th.icon_rect.right - th.icon_rect.left).max(1);
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        gpu::draw_rounded_bitmap(ctx, icon_rect, 0.0, &bitmap);
                    }
                    // SAFETY: `icon_bitmap` was created locally above by
                    // `icon_to_hbitmap` and is owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
            }
            // Plain per-thumbnail hover glow: just browsing, not
            // dragging anything.
            if let Some((HoverTarget::Thumb(hovered_hwnd), intensity)) = &hover {
                if *hovered_hwnd == hwnd && *intensity > 0.001 {
                    gpu::stroke_rounded_rect(ctx, rect, thumb_radius, super::design::color::accent(), *intensity as f32, 2.0);
                }
            }
        }
    });
}

/// Paints the chrome surface: the dock bar/icons, the search
/// panel/rows, and the dragged-window ghost. Mirrors the corresponding
/// slice of `overview::paint_overview`'s GDI drawing. Triggered only by
/// the same state changes that already invalidated this content under
/// GDI (dock/search/drag changes) — never by the per-tick
/// carousel-position path (see `update_transforms`). Drawn onto
/// `gpu.chrome`, not `gpu.root`, because `chrome`'s visual is kept
/// re-inserted above every card visual (see `rebuild_cards`) — the
/// drag ghost and search panel both need to render above cards, and
/// the dock, though it never overlaps a card today, is painted here
/// too rather than split onto `root`, since a future layout change to
/// either could silently reintroduce the occlusion bug this fixes.
/// Paints the overview's full-screen backdrop onto `gpu.root` — the
/// surface every card and the chrome composite on top of. With
/// `overview_blur` on this is a soft, dimmed blur of the desktop
/// wallpaper (the gorgeous GNOME-style look): a tiny, smoothly-downscaled
/// wallpaper (see `overview::backdrop_small_wallpaper`) upscaled to fill
/// the screen with linear interpolation — the upscale *is* the blur — then
/// a translucent black scrim so the light cards and text stay legible over
/// any wallpaper. With blur off it's a clean, solid dark fill. Cheap
/// enough (one stretched small bitmap + one scrim rect) to redraw whenever
/// `paint_root` runs; the downscaled source itself is cached.
pub(crate) fn paint_backdrop(gpu: &OverviewGpuState) {
    let (w, h) = (gpu.root.width().max(1), gpu.root.height().max(1));
    let full = D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 };
    let blur = super::state::overview_blur();

    gpu::redraw(&gpu.root, |ctx: &ID2D1DeviceContext| {
        if blur {
            // Downscale target: small enough for a heavy, even blur, but
            // not so small the wallpaper loses all sense of place. ~1/22
            // of the screen width, clamped, with height carrying the
            // screen's aspect so the upscale doesn't distort it.
            let small_w = (w / 22).clamp(24, 90);
            let small_h = ((small_w as i64 * h as i64) / w as i64).max(1) as i32;
            let mut drew_wallpaper = false;
            if let Some(hbitmap) = super::overview::backdrop_small_wallpaper(small_w, small_h) {
                if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, hbitmap) {
                    gpu::draw_bitmap_stretched(ctx, full, &bitmap);
                    drew_wallpaper = true;
                }
            }
            if !drew_wallpaper {
                gpu::fill_rect(ctx, full, 0x0014_1418);
            }
            // Translucent scrim to darken and unify — keeps the bright
            // workspace cards and the "Type to search" chrome readable
            // over any wallpaper, and gives the overview its focused,
            // recessed feel.
            gpu::fill_rect_alpha(ctx, full, 0x0000_0000, 0.42);
        } else {
            // Blur off: a clean, solid dark backdrop.
            gpu::fill_rect(ctx, full, 0x0014_1418);
        }
    });
}

pub(crate) fn paint_root(gpu: &OverviewGpuState, monitor: &str, ov: &super::overview::OverviewInstance) {
    paint_backdrop(gpu);
    let dpi = reference_dpi();
    let dock_radius = scaled(super::dock::DOCK_CORNER_RADIUS, dpi) as f32;
    let thumb_radius = scaled(8, dpi) as f32;

    gpu::redraw(&gpu.chrome, |ctx: &ID2D1DeviceContext| {
        let (dock_bar_rect, dock_slots, dock_divider) =
            super::dock::dock_layout(monitor, super::dock::pinned_count(&ov.dock_apps), ov.dock_apps.len());
        if !ov.dock_apps.is_empty() {
            let bar = rect_to_d2d(dock_bar_rect, 0, 0);
            gpu::fill_rounded_rect(ctx, bar, dock_radius, super::design::color::surface_raised());

            // GNOME/macOS-dash-style divider between the pinned section
            // (left) and the running-but-unpinned section (right), if
            // there's one to draw.
            if let Some(divider) = dock_divider {
                gpu::fill_rounded_rect(ctx, rect_to_d2d(divider, 0, 0), 0.0, super::design::color::stroke());
            }

            let running_dot_radius = super::dock::dock_running_dot_radius();
            let running_dot_gap = scaled(super::dock::DOCK_RUNNING_DOT_GAP, dpi);
            // Wave magnification: each icon grows with its nearness to the
            // pointer, bottom-anchored so it rises out of the bar. The base
            // slots are still what got hit-tested (see `on_overview_hover`);
            // these are the drawn rects only.
            let draw_slots = super::dock::wave_slots(&dock_slots, ov.dock_cursor_x, 1.0);
            for (i, app) in ov.dock_apps.iter().enumerate() {
                let (Some(slot), Some(base)) = (draw_slots.get(i), dock_slots.get(i)) else {
                    continue;
                };
                if let Some(icon) = app.icon {
                    let size = (slot.right - slot.left).max(1);
                    if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                        if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                            gpu::draw_rounded_bitmap(ctx, rect_to_d2d(*slot, 0, 0), 0.0, &bitmap);
                        }
                        // SAFETY: created locally above, owned exclusively here.
                        unsafe {
                            let _ = DeleteObject(icon_bitmap);
                        }
                    }
                }
                // One small dot per open window (capped — see
                // `running_dot_rects`), GNOME/macOS-dash-style — anchored
                // to the *base* slot so the row of dots stays put while the
                // icons ride the wave above it.
                for dot in super::dock::running_dot_rects(*base, app.windows.len(), running_dot_radius, running_dot_gap) {
                    gpu::fill_rounded_rect(ctx, rect_to_d2d(dot, 0, 0), running_dot_radius as f32, super::design::color::text());
                }
            }
            // The dock's own hover glow — whichever slot the pointer
            // currently sits on, if any, easing in the same way as the
            // card/thumbnail glows. Drawn on the magnified rect so it hugs
            // the icon as it grows.
            if let Some((index, started)) = ov.dock_hover {
                if let Some(slot) = draw_slots.get(index) {
                    let intensity = ease_out(progress_dur(started, WINDOW_HOVER_GLOW_DURATION)) as f32;
                    if intensity > 0.001 {
                        gpu::stroke_rounded_rect(
                            ctx,
                            rect_to_d2d(*slot, 0, 0),
                            thumb_radius,
                            super::design::color::accent(),
                            intensity,
                            2.0,
                        );
                    }
                }
            }
        }

        let results = if ov.search_query.is_empty() {
            Vec::new()
        } else {
            super::overview::search_results(monitor, &ov.search_query)
        };
        let (panel, rows) = super::overview::search_layout(monitor, dpi, results.len());
        gpu::fill_rounded_rect(ctx, rect_to_d2d(panel, 0, 0), thumb_radius, super::design::color::surface_raised());
        let header_text = if ov.search_query.is_empty() {
            "Type to search".to_string()
        } else {
            format!("Search: {}", ov.search_query)
        };
        gpu::draw_text(ctx, rect_to_d2d(rows[0], 0, 0), &header_text, super::design::color::text_muted(), 14.0, false);
        let icon_size = scaled(super::overview::SEARCH_ICON_SIZE, dpi);
        let icon_gap = scaled(super::overview::SEARCH_ICON_TEXT_GAP, dpi);
        for (i, result) in results.iter().enumerate() {
            let label = match result {
                super::overview::SearchResult::Window { title, .. } => title.clone(),
                super::overview::SearchResult::App { name, .. } => format!("{name}  (launch)"),
            };
            let mut row = rows[i + 1];
            if let Some(icon) = super::overview::search_result_icon(result) {
                let icon_y = row.top + ((row.bottom - row.top) - icon_size) / 2;
                let icon_rect = RECT {
                    left: row.left,
                    top: icon_y,
                    right: row.left + icon_size,
                    bottom: icon_y + icon_size,
                };
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, icon_size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        gpu::draw_rounded_bitmap(ctx, rect_to_d2d(icon_rect, 0, 0), 0.0, &bitmap);
                    }
                    // SAFETY: `icon_bitmap` was created locally above by
                    // `icon_to_hbitmap` and is owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
                row.left += icon_size + icon_gap;
            }
            gpu::draw_text(ctx, rect_to_d2d(row, 0, 0), &label, super::design::color::text(), 14.0, false);
        }

        // The ghost itself: a live drag follows the cursor at full size
        // unless a pickup pop is still growing in (then it scales up
        // from nothing); once the drag ends, a drop-out pop shrinks it
        // back to nothing at the frozen release point — mirrors
        // `overview::paint_overview`'s `ghost` computation exactly.
        let ghost = if let Some(drag) = ov.window_drag.as_ref() {
            let scale = match &ov.window_pop_anim {
                Some(p) if p.growing && p.hwnd == drag.hwnd => {
                    ease_out(progress_dur(p.started, WINDOW_POP_DURATION))
                }
                _ => 1.0,
            };
            Some((drag.cur_x, drag.cur_y, drag.base_w, drag.base_h, drag.hwnd, scale))
        } else {
            ov.window_pop_anim.as_ref().filter(|p| !p.growing).and_then(|p| {
                let (x, y) = p.at?;
                let scale = 1.0 - ease_out(progress_dur(p.started, WINDOW_POP_DURATION));
                Some((x, y, p.base_w, p.base_h, p.hwnd, scale))
            })
        };
        if let Some((cx, cy, base_w, base_h, ghost_hwnd, scale)) = ghost {
            if scale > 0.001 {
                if let Some(scaled_handle) = super::overview::slot_scaled_snapshot(ghost_hwnd, base_w, base_h) {
                    let gw = ((base_w * 3 / 5) as f64 * scale).round() as i32;
                    let gh = ((base_h * 3 / 5) as f64 * scale).round() as i32;
                    let rect = D2D_RECT_F {
                        left: (cx - gw / 2) as f32,
                        top: (cy - gh / 2) as f32,
                        right: (cx + gw / 2) as f32,
                        bottom: (cy + gh / 2) as f32,
                    };
                    if let Some(bitmap) =
                        gpu::bitmap_from_hbitmap(ctx, HBITMAP(scaled_handle as *mut c_void))
                    {
                        gpu::draw_rounded_bitmap(ctx, rect, thumb_radius, &bitmap);
                    }
                }
            }
        }

        if let Some(drag) = ov.dock_drag.as_ref() {
            if let Some(icon) = drag.icon {
                let size = scaled(40, dpi);
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        let rect = D2D_RECT_F {
                            left: (drag.cur_x - size / 2) as f32,
                            top: (drag.cur_y - size / 2) as f32,
                            right: (drag.cur_x + size / 2) as f32,
                            bottom: (drag.cur_y + size / 2) as f32,
                        };
                        gpu::draw_rounded_bitmap(ctx, rect, 0.0, &bitmap);
                    }
                    // SAFETY: created locally above, owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod icon_tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{
        GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION};

    /// Reads back the produced bitmap's pixels and asserts the icon fix
    /// yields a real alpha channel: transparent corners (the system app
    /// icon has empty corners) and opaque art. Guards against a regression
    /// back to the alpha-less `CreateCompatibleBitmap` that drew every icon
    /// as an opaque gray square.
    #[test]
    fn icon_to_hbitmap_produces_transparent_and_opaque_pixels() {
        let size = 32i32;
        // SAFETY: `IDI_APPLICATION` is a stock system icon always present;
        // the read-back sequence uses locally-owned handles.
        unsafe {
            let icon = LoadIconW(None, IDI_APPLICATION).expect("stock app icon");
            let hbmp = super::icon_to_hbitmap(icon, size).expect("icon_to_hbitmap");

            let mut buf = vec![0u8; (size * size * 4) as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size,
                    biHeight: -size,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0 as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let screen = GetDC(None);
            let scanned = GetDIBits(
                screen,
                hbmp,
                0,
                size as u32,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                &mut info,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, screen);
            let _ = DeleteObject(hbmp);
            assert!(scanned > 0, "GetDIBits should read scanlines");

            let has_transparent = buf.chunks_exact(4).any(|p| p[3] == 0);
            let has_opaque = buf.chunks_exact(4).any(|p| p[3] == 0xFF);
            assert!(has_transparent, "icon must have transparent pixels (empty corners)");
            assert!(has_opaque, "icon must have opaque pixels (the art)");
        }
    }
}
