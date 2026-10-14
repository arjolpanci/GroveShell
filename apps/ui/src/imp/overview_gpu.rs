//! GPU-composited visual tree for one monitor's Activities overview —
//! Phase 2 of the animation-fluidity work (see
//! `docs/design/2026-07-29-overview-directcomposition-port-design.md`).
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
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, DI_NORMAL, HICON, WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

use groveshell_motion::{lerp, RectF};

use super::gpu::{self, GpuSurface};
use super::overview::{CardAnim, ThumbAnim, WINDOW_HOVER_GLOW_DURATION};
use super::overview_layout::{layout_for_progress, thumb_alpha, thumb_rect};
use super::state::{reference_dpi, scaled};
use super::util::{ease_out, progress_dur};

/// Attaches `child` as the topmost child of `parent`, detaching it first.
///
/// `AddVisual` is **not** a no-op for a visual that is already a child:
/// DirectComposition rejects it with `E_INVALIDARG`. Relying on it to
/// re-insert meant every rebuild after the first silently failed to
/// attach or re-raise anything, so attachment and z-order stopped being
/// deterministic — a preview that dropped out of one rebuild and came
/// back never rendered again. `RemoveVisual` on a visual that is not a
/// child *is* a documented no-op, so detach-then-attach is safe for both
/// the first attach and every re-raise.
unsafe fn raise_child(parent: &windows::Win32::Graphics::DirectComposition::IDCompositionVisual2, child: &windows::Win32::Graphics::DirectComposition::IDCompositionVisual2) {
    let _ = parent.RemoveVisual(child);
    // `insertAbove = false` with a null reference appends to the end of
    // the child list, which is what composites on top. Passing `true`
    // puts it at the *beginning*, i.e. the bottom — which is what hid
    // every window preview behind its card.
    let _ = parent.AddVisual(child, false, None);
}

/// What a preview's surface should actually show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewSource {
    /// The captured bitmap of the window.
    Snapshot,
    /// A rounded chip with the window's title.
    TitleChip,
}

/// Decides between them.
///
/// A minimized window is the case worth spelling out: DWM renders
/// nothing for one, so the capture taken for it is blank. Drawing it
/// anyway put a large grey gradient block where a preview should be —
/// and because a minimized window also gets no *live* DWM thumbnail
/// laid over the top, nothing ever corrected it.
pub(crate) fn preview_source(iconic: bool, has_snapshot: bool) -> PreviewSource {
    if has_snapshot && !iconic {
        PreviewSource::Snapshot
    } else {
        PreviewSource::TitleChip
    }
}

/// Takes the entry matching `key` out of `pool`, but **only** if it also
/// satisfies `still_usable`.
///
/// The obvious spelling — `position(..).map(|i| pool.remove(i)).filter(..)`
/// — removes the entry *before* the filter rejects it, so a stale-sized
/// visual is dropped on the floor instead of being left for the caller to
/// `RemoveVisual`. `GpuSurface` has no `Drop`, and the root still holds
/// its own reference from the earlier `AddVisual`, so that orphan stays
/// in the composition tree compositing frozen content for the rest of
/// the session while nothing ever transforms it again.
fn take_reusable<T>(
    pool: &mut Vec<T>,
    key: impl Fn(&T) -> bool,
    still_usable: impl Fn(&T) -> bool,
) -> Option<T> {
    let i = pool.iter().position(&key)?;
    if !still_usable(&pool[i]) {
        return None;
    }
    Some(pool.remove(i))
}

/// The transform that maps a surface drawn at `natural` onto `target`.
///
/// Every visual in this module is painted once at its natural size and
/// then only transformed, so this is the single piece of arithmetic a
/// frame does per visual — which is why frame cost no longer depends on
/// how many windows are open. A degenerate natural rect yields scale 1
/// rather than an infinity that would poison the composition tree.
fn surface_transform(natural: RectF, target: RectF) -> Matrix3x2 {
    let sx = if natural.width().abs() > f64::EPSILON {
        target.width() / natural.width()
    } else {
        1.0
    };
    let sy = if natural.height().abs() > f64::EPSILON {
        target.height() / natural.height()
    } else {
        1.0
    };
    Matrix3x2 {
        M11: sx as f32,
        M12: 0.0,
        M21: 0.0,
        M22: sy as f32,
        M31: target.left as f32,
        M32: target.top as f32,
    }
}

fn offset_rect(r: RectF, dx: f64, dy: f64) -> RectF {
    RectF::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy)
}

fn to_rect_f(r: RECT) -> RectF {
    RectF::new(r.left as f64, r.top as f64, r.right as f64, r.bottom as f64)
}

/// One window preview's own composited surface, painted once per
/// overview session at its *largest* size — its real on-screen rect, the
/// `p = 0` end of the flight, which is always the bigger one, so the
/// snapshot is never upscaled. Position, size and opacity are transforms
/// from there.
pub(crate) struct ThumbVisual {
    pub(crate) hwnd: isize,
    pub(crate) page: usize,
    pub(crate) natural: RectF,
    pub(crate) surface: GpuSurface,
}

/// One workspace card's own composited surface, at the card's natural
/// (unscrolled, unzoomed) size — carousel position/zoom is applied on
/// top via `SetTransform2`, never baked into what's drawn here.
pub(crate) struct CardVisual {
    pub(crate) page: usize,
    /// The rect this surface was allocated and painted at. Every frame
    /// maps it onto wherever the layout says the card goes.
    pub(crate) natural: RectF,
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
    /// One visual per window preview, parented to the *root* rather than
    /// to its card: `update_transforms` applies the card's own affine to
    /// each preview itself, so parenting them to the card would apply
    /// that mapping twice.
    pub(crate) thumbs: Vec<ThumbVisual>,
    pub(crate) chrome: GpuSurface,
    /// The dash, lifted out of `chrome` so it can slide on the
    /// adjustment independently of the rest of the chrome. Populated in
    /// the dock-binding task; `None` until then, which `update_transforms`
    /// already tolerates.
    pub(crate) dash: Option<GpuSurface>,
    pub(crate) dash_natural: RectF,
    /// The workspace thumbnail strip. Same arrangement as `dash`.
    pub(crate) strip: Option<GpuSurface>,
    pub(crate) strip_natural: RectF,
    /// The hover ring, drawn once at full strength and then *faded* by
    /// opacity rather than repainted.
    ///
    /// It used to be stroked onto the hovered preview's own surface,
    /// which meant re-stretching that window's capture on every frame of
    /// the 220ms ease — 5 to 20ms a tick against a 15ms budget, which is
    /// what made the overview feel heavy. Worse, a preview's surface is
    /// covered by its live DWM thumbnail, so the ring was invisible for
    /// every window that was not minimized.
    pub(crate) glow: Option<GpuSurface>,
    /// The slot the ring is currently painted for, so it is only
    /// repainted when the hovered preview actually changes.
    pub(crate) glow_slot: Option<(isize, RectF)>,
    /// `(page_count, current)` the strip was last painted for. A `Cell`
    /// so the gate works from the shared borrows the paint paths hold.
    pub(crate) strip_state: std::cell::Cell<Option<(usize, usize)>>,
    /// Whether the backdrop has been painted for this window yet.
    pub(crate) backdrop_painted: std::cell::Cell<bool>,
}

/// Creates the root/chrome surfaces for `hwnd` (both sized to the
/// overview window's full client area). Returns `None` if the
/// process-wide GPU setup isn't available, or if this specific
/// window's target/surface setup fails — either way the caller keeps
/// using GDI for this monitor's overview, unchanged.
/// Whether the overview's Direct2D renderer is used.
///
/// False from 2026-07-30 to 2026-10-13, for two stacked reasons. The
/// first was `create` asking for a second `IDCompositionTarget` on a
/// window that already had one, which always failed with
/// `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`; that was fixed by making
/// the extra surfaces children (see `gpu::create_child_surface`). The
/// second was underneath it: the tree built, `BeginDraw` succeeded,
/// `paint_backdrop` filled the root opaque and opacity reached 1.0, and
/// the window still composited fully transparent.
///
/// The cause was the window keeping its GDI redirection bitmap so that
/// the per-frame GDI fallback could still paint it, while
/// DirectComposition composed the same HWND — an opaque redirection
/// bitmap in front of composed content renders that content invisible.
/// The four surfaces that always worked (bar, desktop dock, calendar,
/// quick settings) all set `WS_EX_NOREDIRECTIONBITMAP` and paint only
/// through D2D. The overview does the same now, and GDI is what it falls
/// back to when the process has no GPU at all (see `overview_ex_style`),
/// not a per-frame alternative on a composed window.
const OVERVIEW_GPU_ENABLED: bool = true;

/// The overview window's extended style.
///
/// `translucent` must be exactly `gpu::is_enabled()` at creation time,
/// and the result must be re-checked against the per-window surface — see
/// `desktop_dock`'s `create` for the established pattern. With no
/// redirection bitmap there is nothing for GDI to paint into, so a
/// per-window DirectComposition failure after a successful global check
/// would leave an invisible overview rather than a merely slow one.
pub(crate) fn overview_ex_style(translucent: bool) -> WINDOW_EX_STYLE {
    if translucent {
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP
    } else {
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED
    }
}

/// Whether the overview will actually render through DirectComposition.
///
/// The window style has to agree with this: `WS_EX_NOREDIRECTIONBITMAP`
/// is only safe when something is composing the window, because a window
/// without a redirection bitmap cannot be painted by GDI *at all*. Set
/// one without the other and the overview is invisible rather than merely
/// slow — which is exactly what happened when this flag went in.
#[allow(dead_code, reason = "the window style deliberately keys off gpu::is_enabled instead; kept as the single place that answers this question")]
pub(crate) fn enabled() -> bool {
    OVERVIEW_GPU_ENABLED && super::gpu::is_enabled()
}

pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<OverviewGpuState> {
    if !OVERVIEW_GPU_ENABLED {
        return None;
    }
    let root = gpu::create_surface(hwnd, width, height)?;
    // A *child* surface: `root` already owns this window's one and only
    // composition target, and asking for a second would fail with
    // `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED` — which is exactly
    // what used to happen here, silently dropping every overview onto
    // the GDI fallback.
    let chrome = gpu::create_child_surface(width, height)?;
    // SAFETY: `root`/`chrome` were both just created above by this
    // module's own `gpu::create_surface` and are alive for as long as
    // the `OverviewGpuState` returned below is. `AddVisual` on a fresh,
    // never-attached visual simply attaches it.
    unsafe {
        let _ = root.visual().AddVisual(chrome.visual(), true, None);
    }
    // The dash gets its own full-window surface so it can slide on the
    // adjustment as a pure translation, without repainting anything.
    let dash = gpu::create_child_surface(width, height);
    if let Some(dash) = dash.as_ref() {
        // SAFETY: same device, same lifetime as `root`.
        unsafe {
            raise_child(root.visual(), dash.visual());
        }
    }
    // The workspace strip, on the same arrangement as the dash.
    let strip = gpu::create_child_surface(width, height);
    if let Some(strip) = strip.as_ref() {
        // SAFETY: same device, same lifetime as `root`.
        unsafe {
            raise_child(root.visual(), strip.visual());
        }
    }
    // Re-raise the chrome so it stays above the dash.
    // SAFETY: as above.
    unsafe {
        raise_child(root.visual(), chrome.visual());
    }
    Some(OverviewGpuState {
        root,
        cards: Vec::new(),
        thumbs: Vec::new(),
        chrome,
        dash,
        dash_natural: RectF::new(0.0, 0.0, width as f64, height as f64),
        strip,
        strip_natural: RectF::new(0.0, 0.0, width as f64, height as f64),
        glow: None,
        glow_slot: None,
        strip_state: std::cell::Cell::new(None),
        backdrop_painted: std::cell::Cell::new(false),
    })
}

/// Rebuilds `state.cards` to have exactly one entry per `cards`,
/// reusing each existing card visual's surface when a page's rect size
/// hasn't changed (avoids a needless surface recreation on every
/// `build_carousel_pages` call when only window contents changed, not
/// layout). Cards are recreated in `cards`' order, matching how
/// `on_animation_tick`'s per-tick transform pass below will iterate
/// them.
pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, cards: &[CardAnim]) {
    let mut old_cards = std::mem::take(&mut state.cards);
    let mut rebuilt = Vec::with_capacity(cards.len());
    for card in cards {
        let (w, h) = (card.rect.right - card.rect.left, card.rect.bottom - card.rect.top);
        let reused = take_reusable(
            &mut old_cards,
            |cv| cv.page == card.page,
            |cv| cv.surface.width() == w && cv.surface.height() == h,
        );
        let card_visual = match reused {
            Some(cv) => cv,
            None => match gpu::create_child_surface(w.max(1), h.max(1)) {
                Some(surface) => CardVisual {
                    page: card.page,
                    natural: to_rect_f(card.rect),
                    surface,
                },
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
    restack(state);
}

/// Width of the hover ring, in 96-DPI units.
pub(crate) const HOVER_RING_DP: i32 = 3;

/// Points the hover ring at `slot` for `hwnd`, painting it only when the
/// hovered preview has actually changed, and fading it with `intensity`.
///
/// `None` fades it out. The ring is drawn into a surface the size of the
/// slot, so the per-frame cost is one `SetOpacity` — no bitmap work at
/// all.
pub(crate) fn set_hover_ring(
    state: &mut OverviewGpuState,
    hovered: Option<(isize, RectF)>,
    intensity: f64,
) {
    let Some((hwnd, slot)) = hovered else {
        if let Some(glow) = state.glow.as_ref() {
            gpu::set_opacity(glow, 0.0);
        }
        state.glow_slot = None;
        return;
    };

    let (w, h) = (slot.width().round() as i32, slot.height().round() as i32);
    if w <= 0 || h <= 0 {
        return;
    }

    let changed = state.glow_slot != Some((hwnd, slot));
    if changed {
        let reusable = state
            .glow
            .as_ref()
            .is_some_and(|g| g.width() == w && g.height() == h);
        if !reusable {
            if let Some(old) = state.glow.as_ref() {
                // SAFETY: attached by an earlier call; `RemoveVisual` on
                // a detached visual is a documented no-op.
                unsafe {
                    let _ = state.root.visual().RemoveVisual(old.visual());
                }
            }
            state.glow = gpu::create_child_surface(w, h);
        }

        let Some(glow) = state.glow.as_ref() else { return };
        let dpi = reference_dpi();
        let radius = scaled(8, dpi) as f32; // THUMB_CORNER_RADIUS
        let stroke = scaled(HOVER_RING_DP, dpi) as f32;
        let full = D2D_RECT_F {
            left: stroke / 2.0,
            top: stroke / 2.0,
            right: w as f32 - stroke / 2.0,
            bottom: h as f32 - stroke / 2.0,
        };
        gpu::redraw(glow, |ctx: &ID2D1DeviceContext| {
            gpu::clear_transparent(ctx);
            // Full strength: the ease is carried by the visual's opacity.
            gpu::stroke_rounded_rect(ctx, full, radius, super::design::color::accent(), 1.0, stroke);
        });
        state.glow_slot = Some((hwnd, slot));
    }

    let Some(glow) = state.glow.as_ref() else { return };
    // SAFETY: same device and lifetime as `root`.
    unsafe {
        let _ = glow
            .visual()
            .SetTransform2(&surface_transform(RectF::new(0.0, 0.0, w as f64, h as f64), slot));
    }
    gpu::set_opacity(glow, intensity.clamp(0.0, 1.0) as f32);
    if changed {
        // SAFETY: as above.
        unsafe {
            raise_child(state.root.visual(), glow.visual());
        }
    }
}

/// Re-establishes the composition order, bottom to top: cards, then the
/// previews sitting on them, then the dash, the strip, and the chrome.
///
/// One function rather than a re-raise at the end of each rebuild,
/// because the order is a property of the whole tree and every partial
/// version of it has been wrong. `rebuild_cards` used to re-raise only
/// the cards, which lifted them above the previews and hid every one of
/// them behind an opaque card — visible only for windows that happened
/// to have a live DWM thumbnail painted over the top, so minimized
/// windows simply disappeared.
fn restack(state: &OverviewGpuState) {
    // SAFETY: every visual here belongs to the same device as `root` and
    // lives as long as `state`; `raise_child` detaches before attaching,
    // which is a no-op for a visual with no parent.
    unsafe {
        for card in &state.cards {
            raise_child(state.root.visual(), card.surface.visual());
        }
        for thumb in &state.thumbs {
            raise_child(state.root.visual(), thumb.surface.visual());
        }
        if let Some(glow) = state.glow.as_ref() {
            raise_child(state.root.visual(), glow.visual());
        }
        if let Some(dash) = state.dash.as_ref() {
            raise_child(state.root.visual(), dash.visual());
        }
        if let Some(strip) = state.strip.as_ref() {
            raise_child(state.root.visual(), strip.visual());
        }
        raise_child(state.root.visual(), state.chrome.visual());
    }
}

/// Rebuilds `state.thumbs` to one visual per preview. Reuses an existing
/// surface when the same window on the same page still wants the same
/// natural size.
///
/// A visual's natural size is its `p = 0` rect — where the real window
/// sits on screen — because that is the larger end of the flight, so the
/// snapshot is scaled down rather than up. A preview with no `home`
/// (parked or minimized) has no such rect and takes its slot size.
pub(crate) fn rebuild_thumbs(state: &mut OverviewGpuState, thumbs: &[ThumbAnim]) {
    let mut old = std::mem::take(&mut state.thumbs);
    let mut rebuilt = Vec::with_capacity(thumbs.len());

    for th in thumbs {
        let hwnd = th.hwnd.0 as isize;
        // `th.rect` is the preview's own grid slot — the same rect
        // hit-testing, the GDI painter and `update_transforms` all use.
        // A second geometry (`grid_slots`) used to size this fallback,
        // and because that one reserves no icon band and does not
        // aspect-fit, a parked preview was painted at cell aspect and
        // then non-uniformly scaled onto a window-aspect slot — visibly
        // stretched.
        let natural = match th.home {
            Some(home) => to_rect_f(home),
            None => to_rect_f(th.rect),
        };
        let (w, h) = (natural.width().round() as i32, natural.height().round() as i32);
        if w <= 0 || h <= 0 {
            continue;
        }

        let reused = take_reusable(
            &mut old,
            |tv| tv.hwnd == hwnd && tv.page == th.page,
            |tv| tv.surface.width() == w && tv.surface.height() == h,
        );

        let visual = match reused {
            Some(tv) => tv,
            None => match gpu::create_child_surface(w, h) {
                Some(surface) => {
                    let visual = ThumbVisual { hwnd, page: th.page, natural, surface };
                    paint_thumb(&visual, &th.title);
                    visual
                }
                None => continue,
            },
        };
        rebuilt.push(visual);
    }

    // Same reasoning as `rebuild_cards`: dropping the Rust wrapper only
    // releases our COM reference, so a stale child has to be detached
    // explicitly or it keeps compositing frozen content forever.
    for stale in &old {
        // SAFETY: `stale` was attached to the root by an earlier call;
        // `RemoveVisual` on a detached visual is a documented no-op.
        // Both outlive this call.
        unsafe {
            let _ = state.root.visual().RemoveVisual(stale.surface.visual());
        }
    }

    state.thumbs = rebuilt;
    restack(state);
}

/// Paints one preview's surface, once. Mirrors what `paint_card`'s
/// thumbnail loop used to draw into the card: the window's snapshot
/// rounded off, or a title chip when no pixels are available.
pub(crate) fn paint_thumb(thumb: &ThumbVisual, title: &str) {
    let dpi = reference_dpi();
    let radius = scaled(8, dpi) as f32; // THUMB_CORNER_RADIUS
    let full = D2D_RECT_F {
        left: 0.0,
        top: 0.0,
        right: thumb.natural.width() as f32,
        bottom: thumb.natural.height() as f32,
    };
    // SAFETY: a plain query; a stale handle reports false rather than
    // misbehaving.
    let iconic = unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsIconic(HWND(thumb.hwnd as *mut c_void)).as_bool()
    };
    let has_snapshot = super::overview::window_snapshot(thumb.hwnd).is_some();
    let source = preview_source(iconic, has_snapshot);

    gpu::redraw(&thumb.surface, |ctx: &ID2D1DeviceContext| {
        // The surface is rounded off, so its corners must start empty.
        gpu::clear_transparent(ctx);
        let snapshot = match source {
            PreviewSource::Snapshot => super::overview::slot_scaled_snapshot(
                thumb.hwnd,
                thumb.natural.width().round() as i32,
                thumb.natural.height().round() as i32,
            ),
            PreviewSource::TitleChip => None,
        };
        match snapshot.and_then(|h| gpu::bitmap_from_hbitmap(ctx, HBITMAP(h as *mut c_void))) {
            Some(bitmap) => gpu::draw_rounded_bitmap(ctx, full, radius, &bitmap),
            None => {
                gpu::fill_rounded_rect(ctx, full, radius, super::design::color::surface_overlay());
                gpu::draw_text(ctx, full, title, super::design::color::text(), 13.0, true);
            }
        }
    });
}


/// Paints the dash onto its own full-window surface.
///
/// Full-window rather than a tight band so the drawing keeps the window
/// coordinates `dock_layout` already returns — the slide is then a pure
/// translation of the whole surface, with no coordinate rewriting and no
/// risk of clipping a magnified icon against a tight band's edge.
pub(crate) fn paint_dash(gpu: &OverviewGpuState, monitor: &str, ov: &super::overview::OverviewInstance) {
    let Some(surface) = gpu.dash.as_ref() else { return };

    let dpi = reference_dpi();
    let dock_radius = scaled(super::dock::DOCK_CORNER_RADIUS, dpi) as f32;
    let thumb_radius = scaled(8, dpi) as f32;
    gpu::redraw(surface, |ctx: &ID2D1DeviceContext| {
        gpu::clear_transparent(ctx);
        let (dock_bar_rect, dock_slots, dock_divider) = super::dock::dock_layout(
            monitor,
            super::dock::pinned_count(&ov.dock_apps),
            ov.dock_apps.len(),
        );
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
    });
}


/// Paints the workspace strip, but only when what it shows has changed.
///
/// Each miniature scales its own wallpaper, so a full repaint costs
/// ~52ms — and it used to run on every hover change, where neither the
/// page count nor the current page can possibly have moved.
pub(crate) fn paint_strip_if_changed(
    gpu: &OverviewGpuState,
    monitor: &str,
    page_count: usize,
    current: usize,
) {
    if gpu.strip_state.get() == Some((page_count, current)) {
        return;
    }
    gpu.strip_state.set(Some((page_count, current)));
    paint_strip(gpu, monitor, page_count, current);
}

/// Paints the backdrop once per overview window.
///
/// It is a full-window blurred wallpaper plus a scrim — 62ms a call,
/// measured — and the wallpaper does not change while the shell runs, so
/// repainting it on every rebuild of the pages was pure cost. Rebuilds
/// happen while the overview is open (a window appearing, a workspace
/// re-sync), which is what made it stutter under the pointer.
pub(crate) fn paint_backdrop_once(gpu: &OverviewGpuState) {
    if gpu.backdrop_painted.get() {
        return;
    }
    gpu.backdrop_painted.set(true);
    paint_backdrop(gpu);
}

/// Forgets what the cached-once surfaces were painted for, so the next
/// paint call actually redraws them.
///
/// Both gates above assume their input is fixed for the life of the
/// window, which holds for the wallpaper but not for settings the user
/// can change while the shell runs: `overview_blur` decides whether the
/// backdrop is a blurred wallpaper or a flat fill, and the accent colour
/// outlines the strip's current workspace. A live config reload or an
/// accent change has to call this, or the overview keeps showing the old
/// one until it is next recreated.
pub(crate) fn invalidate_cached_paints(gpu: &OverviewGpuState) {
    gpu.backdrop_painted.set(false);
    gpu.strip_state.set(None);
}

/// Paints the workspace thumbnail strip: one wallpaper-filled miniature
/// per workspace, with the current one outlined in the accent colour.
///
/// Full-window surface for the same reason the dash has one — the slide
/// stays a pure translation.
pub(crate) fn paint_strip(gpu: &OverviewGpuState, monitor: &str, page_count: usize, current: usize) {
    let Some(surface) = gpu.strip.as_ref() else { return };
    let Some(input) = super::overview::layout_input(monitor) else { return };
    let layout = super::overview_layout::layout_for_state(
        super::overview_layout::ControlsState::WindowPicker,
        &input,
    );
    let slots = super::overview_layout::strip_slots(layout.strip, page_count, input.monitor_aspect);
    let dpi = reference_dpi();
    let radius = scaled(4, dpi) as f32;

    gpu::redraw(surface, |ctx: &ID2D1DeviceContext| {
        gpu::clear_transparent(ctx);
        for (page, slot) in slots.iter().enumerate() {
            let r = D2D_RECT_F {
                left: slot.left as f32,
                top: slot.top as f32,
                right: slot.right as f32,
                bottom: slot.bottom as f32,
            };
            let rect = RECT {
                left: slot.left.round() as i32,
                top: slot.top.round() as i32,
                right: slot.right.round() as i32,
                bottom: slot.bottom.round() as i32,
            };
            // `wallpaper_hbitmap_for` deliberately serves only the card's
            // exact size; a strip miniature needs its own small scale, which
            // is what `backdrop_small_wallpaper` already produces.
            let mut drew = false;
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if let Some(wallpaper) = super::overview::backdrop_small_wallpaper(w, h) {
                if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, wallpaper) {
                    gpu::draw_rounded_bitmap(ctx, r, radius, &bitmap);
                    drew = true;
                }
            }
            if !drew {
                gpu::fill_rounded_rect(ctx, r, radius, super::design::color::surface_overlay());
            }
            if page == current {
                gpu::stroke_rounded_rect(ctx, r, radius, super::design::color::accent(), 1.0, 2.0);
            }
        }
    });
}

/// Applies this tick's carousel position/zoom to every card visual —
/// the GPU-path replacement for `repaint_overview`'s full redraw.
/// `card_layout`/`displayed_rect`/`zoom_rect` are the exact same pure
/// functions `paint_overview` already used; only what happens with
/// their result changes (a transform instead of a redraw).
pub(crate) fn update_transforms(
    overview_hwnd: HWND,
    gpu_state: &OverviewGpuState,
    monitor: &str,
    carousel_offset: f64,
    progress: f64,
    thumbs: &[ThumbAnim],
    // The dragged window and where its ghost is right now, from
    // `OverviewInstance::ghost`. The live preview of that window is
    // moved to this rect instead of its grid slot, so what follows the
    // cursor is the actual window rather than a still of it.
    ghost: Option<(isize, RectF)>,
) {
    let Some(input) = super::overview::layout_input(monitor) else {
        // Monitor vanished mid-frame (hotplug race). Skip the frame
        // rather than transform against a layout we cannot measure.
        return;
    };
    let layout = layout_for_progress(progress, &input);
    // Live previews ride on the same rects the snapshots are transformed
    // into, so a preview tracks the flight frame for frame. Collected
    // here and handed to DWM once at the end.
    let mut live: Vec<(HWND, RECT)> = Vec::new();

    for card_visual in &gpu_state.cards {
        let dx = (card_visual.page as f64 - carousel_offset) * layout.pitch;
        // GNOME shrinks the off-centre workspaces slightly
        // (`WORKSPACE_INACTIVE_SCALE`, `workspacesView.js:25`) so the
        // focused one reads as the subject.
        let focus = 1.0 - (card_visual.page as f64 - carousel_offset).abs().min(1.0);
        let scale = lerp(super::overview_layout::WORKSPACE_INACTIVE_SCALE, 1.0, focus);

        // One affine for this page: shift by the carousel, then shrink
        // about the shifted card's centre. The card and every preview on
        // it go through *the same* mapping, which is what keeps a
        // preview glued to its card. Previews are parented to the root
        // rather than to the card precisely so this is applied once
        // instead of composing with the card's own transform — composing
        // them was what put every preview back at its real screen rect.
        let shifted = offset_rect(layout.card, dx, 0.0);
        let (ccx, ccy) = shifted.center();
        let apply = |r: RectF| {
            let o = offset_rect(r, dx, 0.0);
            RectF::new(
                ccx + (o.left - ccx) * scale,
                ccy + (o.top - ccy) * scale,
                ccx + (o.right - ccx) * scale,
                ccy + (o.bottom - ccy) * scale,
            )
        };

        // SAFETY: the visual outlives `gpu_state`; `SetTransform2` takes
        // a matrix pointer valid for this synchronous call.
        unsafe {
            let _ = card_visual
                .surface
                .visual()
                .SetTransform2(&surface_transform(card_visual.natural, apply(layout.card)));
        }

        for th in thumbs.iter().filter(|t| t.page == card_visual.page) {
            let hwnd = th.hwnd.0 as isize;
            let Some(visual) = gpu_state
                .thumbs
                .iter()
                .find(|tv| tv.hwnd == hwnd && tv.page == card_visual.page)
            else {
                continue;
            };
            // `th.rect` is the preview's grid slot, in the same
            // coordinate space as `layout.card` — `card_layout` and
            // `overview_layout` are one system now, so hit-testing and
            // rendering cannot disagree.
            let home = th.home.map(to_rect_f);
            let flown = thumb_rect(home, to_rect_f(th.rect), progress);
            // A dragged window is held by the cursor, not laid out in
            // the grid, so it skips the carousel/zoom mapping that puts
            // previews on their card — the ghost rect is already in
            // screen coordinates.
            let dragged = ghost.filter(|(h, _)| *h == hwnd).map(|(_, r)| r);
            let tgt = match dragged {
                Some(ghost_rect) => ghost_rect,
                None => apply(flown),
            };
            // SAFETY: as above.
            unsafe {
                let _ = visual
                    .surface
                    .visual()
                    .SetTransform2(&surface_transform(visual.natural, tgt));
            }
            gpu::set_opacity(&visual.surface, thumb_alpha(home, progress) as f32);

            if super::thumbnails::can_preview(th.hwnd) {
                // Inset by the hover ring's width. DWM composites a live
                // thumbnail above *all* of this window's own content, so
                // anything we draw inside the preview's bounds is hidden
                // by it — which is why the hover ring never appeared on
                // any window that was not minimized. Leaving this margin
                // is what gives the ring somewhere to show.
                let inset = scaled(HOVER_RING_DP, input.dpi) as f64;
                live.push((
                    th.hwnd,
                    RECT {
                        left: (tgt.left + inset).round() as i32,
                        top: (tgt.top + inset).round() as i32,
                        right: (tgt.right - inset).round() as i32,
                        bottom: (tgt.bottom - inset).round() as i32,
                    },
                ));
            }
        }
    }

    // Dash and strip slide on the same number. Both are `None` until
    // their own tasks create them.
    if let Some(dash) = &gpu_state.dash {
        // The surface was painted with the dash at its window-picker
        // position, so the slide is the offset from there to wherever the
        // adjustment currently puts it — a pure translation.
        let picker = super::overview_layout::layout_for_state(
            super::overview_layout::ControlsState::WindowPicker,
            &input,
        );
        let dy = layout.dash.top - picker.dash.top;
        let target = offset_rect(gpu_state.dash_natural, 0.0, dy);
        // SAFETY: as above.
        unsafe {
            let _ = dash.visual().SetTransform2(&surface_transform(gpu_state.dash_natural, target));
        }
    }
    if let Some(strip) = &gpu_state.strip {
        // Same arrangement as the dash: painted at its window-picker
        // position, slid by the offset to wherever the adjustment puts it.
        let picker = super::overview_layout::layout_for_state(
            super::overview_layout::ControlsState::WindowPicker,
            &input,
        );
        let dy = layout.strip.top - picker.strip.top;
        let target = offset_rect(gpu_state.strip_natural, 0.0, dy);
        // SAFETY: as above.
        unsafe {
            let _ = strip.visual().SetTransform2(&surface_transform(gpu_state.strip_natural, target));
        }
        gpu::set_opacity(strip, layout.strip_alpha as f32);
    }

    // DWM composites these above this window's own content, so the
    // painted snapshot stays underneath as the base layer and shows
    // through for anything DWM will not register (a window parked on
    // another workspace has no live contents).
    //
    // This used to be driven from `paint_overview`, the GDI painter.
    // Enabling the DirectComposition renderer took that function out of
    // the frame path, which silently turned every preview into a frozen
    // still — the renderer fix and this call belong together.
    super::thumbnails::sync(overview_hwnd, &live);
    gpu::commit();
}

fn rect_f_to_d2d(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
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
}

/// Paints one card's frame (wallpaper-filled rounded rect, drop-shadow
/// approximation), thumbnails (each clipped/rounded, WIC-bridged from
/// the existing `PrintWindow` capture cache), icon badges, placeholder
/// chips, and — when `hover` names this card or one of its thumbnails —
/// the light-blue hover-glow outline. Mirrors the corresponding subset
/// of `overview::paint_overview`'s GDI drawing, at the card's own
/// natural (unscrolled) size. `thumbs` must already be filtered to this
/// card's `page`.
/// Paints one workspace card's surface: wallpaper, rounded frame, and
/// the whole-card hover glow.
///
/// It no longer draws the window previews. Each preview is its own
/// composition visual now (see `rebuild_thumbs`), which is what lets a
/// frame be transforms only — the card is painted once per session
/// instead of whenever a preview moved.
pub(crate) fn paint_card(
    card_visual: &CardVisual,
    card_rect: RECT,
    monitor: &str,
    hover: Option<(HoverTarget, f64)>,
) {
    let dpi = reference_dpi();
    let card_radius = scaled(20, dpi) as f32; // CARD_CORNER_RADIUS
    let (origin_x, origin_y) = (card_rect.left, card_rect.top);
    let full = rect_to_d2d(card_rect, origin_x, origin_y);

    gpu::redraw(&card_visual.surface, |ctx: &ID2D1DeviceContext| {
        // The card is rounded, so its corners must start empty — with no
        // redirection bitmap there is nothing opaque behind them.
        gpu::clear_transparent(ctx);
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
                gpu::stroke_rounded_rect(
                    ctx,
                    full,
                    card_radius,
                    super::design::color::accent(),
                    *intensity as f32,
                    2.0,
                );
            }
        }
    });
}

pub(crate) fn paint_backdrop(gpu: &OverviewGpuState) {
    let (w, h) = (gpu.root.width().max(1), gpu.root.height().max(1));
    let full = D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 };
    let blur = super::state::overview_blur();

    gpu::redraw(&gpu.root, |ctx: &ID2D1DeviceContext| {
        // Without a redirection bitmap there is no opaque GDI surface
        // underneath, so anything not drawn here keeps whatever this
        // surface last held. Clearing first is what makes every
        // subsequent alpha fill composite against the desktop rather
        // than against the previous frame.
        gpu::clear_transparent(ctx);
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
            gpu::fill_rect_alpha(ctx, full, 0x0000_0000, super::overview_layout::BACKDROP_DIM_MAX);
        } else {
            // Blur off: a clean, solid dark backdrop.
            gpu::fill_rect(ctx, full, 0x0014_1418);
        }
    });
}

/// Paints the chrome: the search panel and the drag ghost.
///
/// It deliberately does **not** repaint the backdrop any more. The
/// backdrop is a full-window blurred wallpaper plus a scrim — 57ms a
/// call, measured — and it does not change while the overview is up, yet
/// this ran on every hover change. Call `paint_backdrop` when the
/// overview is built instead.
pub(crate) fn paint_root(gpu: &OverviewGpuState, monitor: &str, ov: &super::overview::OverviewInstance) {
    let dpi = reference_dpi();
    // Shared corner radius for the search panel and the drag ghost.
    let thumb_radius = scaled(8, dpi) as f32;

    gpu::redraw(&gpu.chrome, |ctx: &ID2D1DeviceContext| {
        // Nothing else clears this surface, and it is drawn on every
        // frame of a drag at a rect that moves with the cursor. Without
        // this, each frame's ghost stayed where it was painted and the
        // drag left a trail of them across the overview.
        gpu::clear_transparent(ctx);

        // The dash lives on its own visual now (see `paint_dash`) so it
        // can slide on the adjustment without repainting this surface.

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

        // The ghost: the window the user is holding. Where DWM can
        // preview the source live, the ghost *is* that live preview,
        // positioned by `update_transforms` — it composites above this
        // surface, so drawing a still here too would only sit behind it.
        //
        // This still is the fallback for the windows DWM will not
        // preview: parked on another workspace, or minimized. Those are
        // exactly the ones that have a park-time capture to draw.
        if let Some((ghost_hwnd, rect)) = ov.ghost() {
            let previewable =
                super::thumbnails::can_preview(HWND(ghost_hwnd as *mut c_void));
            if !previewable {
                let (gw, gh) = (rect.width().round() as i32, rect.height().round() as i32);
                if let Some(scaled_handle) =
                    super::overview::slot_scaled_snapshot(ghost_hwnd, gw, gh)
                {
                    if let Some(bitmap) =
                        gpu::bitmap_from_hbitmap(ctx, HBITMAP(scaled_handle as *mut c_void))
                    {
                        gpu::draw_rounded_bitmap(ctx, rect_f_to_d2d(rect), thumb_radius, &bitmap);
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
mod preview_source_tests {
    use super::*;

    #[test]
    fn an_on_screen_window_with_a_capture_shows_it() {
        assert_eq!(preview_source(false, true), PreviewSource::Snapshot);
    }

    /// A minimized window has no rendered content, so the capture taken
    /// for it is blank — it rendered as a large grey gradient block
    /// sitting where a preview should be.
    #[test]
    fn a_minimized_window_shows_its_title_not_its_blank_capture() {
        assert_eq!(preview_source(true, true), PreviewSource::TitleChip);
    }

    #[test]
    fn no_capture_means_the_title_whatever_the_window_is_doing() {
        assert_eq!(preview_source(false, false), PreviewSource::TitleChip);
        assert_eq!(preview_source(true, false), PreviewSource::TitleChip);
    }
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    #[test]
    fn a_matching_entry_is_taken_out_of_the_pool() {
        let mut pool = vec![(1usize, 100, 50), (2, 200, 60)];
        let got = take_reusable(&mut pool, |e| e.0 == 2, |e| (e.1, e.2) == (200, 60));
        assert_eq!(got, Some((2, 200, 60)));
        assert_eq!(pool, vec![(1, 100, 50)], "the taken entry is gone");
    }

    /// The bug this exists to prevent: an entry whose key matches but
    /// whose size does not must stay in the pool, so the caller still
    /// detaches it. Removing it first and then rejecting it dropped it
    /// on the floor — the composition visual was never detached, so it
    /// kept compositing frozen content for the rest of the session.
    #[test]
    fn a_key_match_with_a_stale_size_is_left_in_the_pool_to_be_detached() {
        let mut pool = vec![(1usize, 100, 50)];
        let got = take_reusable(&mut pool, |e| e.0 == 1, |e| (e.1, e.2) == (999, 999));
        assert_eq!(got, None);
        assert_eq!(pool, vec![(1, 100, 50)], "must remain for the caller to detach");
    }

    #[test]
    fn no_key_match_leaves_the_pool_untouched() {
        let mut pool = vec![(1usize, 100, 50)];
        let got = take_reusable(&mut pool, |e| e.0 == 7, |_| true);
        assert_eq!(got, None);
        assert_eq!(pool, vec![(1, 100, 50)]);
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;

    #[test]
    fn a_surface_transform_maps_its_natural_rect_onto_the_target_rect() {
        let natural = RectF::new(0.0, 0.0, 200.0, 100.0);
        let target = RectF::new(50.0, 25.0, 450.0, 325.0);
        let m = surface_transform(natural, target);
        assert!((m.M11 - 2.0).abs() < 1e-6, "x scale");
        assert!((m.M22 - 3.0).abs() < 1e-6, "y scale");
        assert!((m.M31 - 50.0).abs() < 1e-6, "x offset");
        assert!((m.M32 - 25.0).abs() < 1e-6, "y offset");
    }

    #[test]
    fn a_degenerate_natural_rect_does_not_produce_a_nan_transform() {
        let m = surface_transform(RectF::new(0.0, 0.0, 0.0, 0.0), RectF::new(0.0, 0.0, 10.0, 10.0));
        assert!(m.M11.is_finite() && m.M22.is_finite());
    }

    #[test]
    fn offsetting_moves_every_edge_by_the_same_delta() {
        let r = offset_rect(RectF::new(10.0, 20.0, 30.0, 40.0), 5.0, -5.0);
        assert_eq!(r, RectF::new(15.0, 15.0, 35.0, 35.0));
    }

    #[test]
    fn converting_a_win32_rect_preserves_every_edge() {
        let r = to_rect_f(RECT { left: 1, top: 2, right: 3, bottom: 4 });
        assert_eq!(r, RectF::new(1.0, 2.0, 3.0, 4.0));
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    #[test]
    fn translucent_style_drops_the_redirection_bitmap_and_layering() {
        let style = overview_ex_style(true);
        assert_eq!(style & WS_EX_NOREDIRECTIONBITMAP, WS_EX_NOREDIRECTIONBITMAP);
        assert_eq!(style & WS_EX_LAYERED, Default::default());
        assert_eq!(style & WS_EX_TOPMOST, WS_EX_TOPMOST);
        assert_eq!(style & WS_EX_TOOLWINDOW, WS_EX_TOOLWINDOW);
    }

    #[test]
    fn opaque_style_keeps_layering_for_the_gdi_fade() {
        let style = overview_ex_style(false);
        assert_eq!(style & WS_EX_LAYERED, WS_EX_LAYERED);
        assert_eq!(style & WS_EX_NOREDIRECTIONBITMAP, Default::default());
    }
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
