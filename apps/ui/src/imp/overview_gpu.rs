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
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    HBITMAP,
};
use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL, HICON};

use super::gpu::{self, GpuSurface};
use super::overview::{card_layout, displayed_rect, zoom_rect, CardAnim, ThumbAnim};
use super::state::{reference_dpi, scaled};

/// One workspace card's own composited surface, at the card's natural
/// (unscrolled, unzoomed) size — carousel position/zoom is applied on
/// top via `SetTransform2`, never baked into what's drawn here.
pub(crate) struct CardVisual {
    pub(crate) page: usize,
    pub(crate) surface: GpuSurface,
}

/// One monitor overview's GPU state. `root`'s surface holds the dock
/// bar, search panel, and drag ghost; `cards` holds one entry per
/// current `CardAnim`, kept in sync by `rebuild_cards`.
pub(crate) struct OverviewGpuState {
    pub(crate) root: GpuSurface,
    pub(crate) cards: Vec<CardVisual>,
}

/// Creates the root surface for `hwnd` (sized to the overview window's
/// full client area). Returns `None` if the process-wide GPU setup
/// isn't available, or if this specific window's target/surface setup
/// fails — either way the caller keeps using GDI for this monitor's
/// overview, unchanged.
pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<OverviewGpuState> {
    let root = gpu::create_surface(hwnd, width, height)?;
    Some(OverviewGpuState { root, cards: Vec::new() })
}

/// Rebuilds `state.cards` to have exactly one entry per `cards`,
/// reusing each existing card visual's surface when a page's rect size
/// hasn't changed (avoids a needless surface recreation on every
/// `build_carousel_pages` call when only window contents changed, not
/// layout). Cards are recreated in `cards`' order, matching how
/// `on_animation_tick`'s per-tick transform pass below will iterate
/// them.
pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, hwnd: HWND, cards: &[CardAnim]) {
    let mut rebuilt = Vec::with_capacity(cards.len());
    for card in cards {
        let (w, h) = (card.rect.right - card.rect.left, card.rect.bottom - card.rect.top);
        let reused = state
            .cards
            .iter()
            .position(|cv| cv.page == card.page)
            .map(|i| state.cards.remove(i))
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
}

/// Applies this tick's carousel position/zoom to every card visual —
/// the GPU-path replacement for `repaint_overview`'s full redraw.
/// `card_layout`/`displayed_rect`/`zoom_rect` are the exact same pure
/// functions `paint_overview` already used; only what happens with
/// their result changes (a transform instead of a redraw).
pub(crate) fn update_transforms(gpu: &OverviewGpuState, monitor: &str, carousel_offset: f64, zoom: f64) {
    let (card_rect, pitch) = card_layout(monitor);
    let anchor_x = (card_rect.left + card_rect.right) as f64 / 2.0;
    let anchor_y = (card_rect.top + card_rect.bottom) as f64 / 2.0;
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

/// Renders an `HICON` to a small ARGB `HBITMAP` so it can go through
/// the same WIC bridge as window thumbnails and the wallpaper — icons
/// have no separate Direct2D-native path.
fn icon_to_hbitmap(icon: HICON, size: i32) -> Option<HBITMAP> {
    // SAFETY: standard create-select-draw-restore GDI sequence on
    // locally created handles; `bitmap`'s ownership moves to the
    // caller on success.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, size, size);
        let previous = SelectObject(mem, bitmap);
        let ok = DrawIconEx(mem, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok();
        SelectObject(mem, previous);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            let _ = DeleteObject(bitmap);
            return None;
        }
        Some(bitmap)
    }
}

/// Paints one card's frame (wallpaper-filled rounded rect, drop-shadow
/// approximation), thumbnails (each clipped/rounded, WIC-bridged from
/// the existing `PrintWindow` capture cache), icon badges, and
/// placeholder chips — mirrors the corresponding subset of
/// `overview::paint_overview`'s GDI drawing, at the card's own natural
/// (unscrolled) size. `thumbs` must already be filtered to this card's
/// `page`.
pub(crate) fn paint_card(card_visual: &CardVisual, card_rect: RECT, thumbs: &[&ThumbAnim], monitor: &str) {
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
                gpu::fill_rounded_rect(ctx, rect, thumb_radius, 0x00303030);
                gpu::draw_text(ctx, rect, &th.title, 0x00E0E0E0, 13.0, true);
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
        }
    });
}
