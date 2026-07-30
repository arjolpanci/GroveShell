//! GPU-composited visual tree for one monitor's Activities overview —
//! Phase 2 of the animation-fluidity work (see
//! `docs/superpowers/specs/2026-07-29-overview-directcomposition-port-design.md`).
//! One root visual (dock/search/ghost chrome) plus one child visual per
//! current `CardAnim` (frame/wallpaper/shadow/thumbnails/icons/chips).
//! `None`/absent anywhere in this module means "fall back to this
//! overview window's existing GDI painting, unchanged" — never a panic.

use windows::Win32::Foundation::HWND;

use super::gpu::{self, GpuSurface};
use super::overview::CardAnim;

/// One workspace card's own composited surface, at the card's natural
/// (unscrolled, unzoomed) size — carousel position/zoom is applied on
/// top via `SetTransform2`, never baked into what's drawn here.
pub(crate) struct CardVisual {
    #[allow(dead_code)] // read by rebuild_cards, which isn't called until a later task
    pub(crate) page: usize,
    #[allow(dead_code)] // read by rebuild_cards, which isn't called until a later task
    pub(crate) surface: GpuSurface,
}

/// One monitor overview's GPU state. `root`'s surface holds the dock
/// bar, search panel, and drag ghost; `cards` holds one entry per
/// current `CardAnim`, kept in sync by `rebuild_cards`.
pub(crate) struct OverviewGpuState {
    #[allow(dead_code)] // will be painted into by a later task
    pub(crate) root: GpuSurface,
    #[allow(dead_code)] // read/written by rebuild_cards, which isn't called until a later task
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
#[allow(dead_code)] // will be called by overview GPU state in a later task
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
}
