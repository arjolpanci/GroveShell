//! Every box, scale and opacity in the Activities overview, as a pure
//! function of one progress value — the port of GNOME Shell's
//! `ControlsManagerLayout` (`js/ui/overviewControls.js:39`).
//!
//! Pure on purpose: the whole feel of the transition is testable here
//! without a monitor, a GPU, or a running shell. The caller owns
//! everything platform-shaped — querying the work area, the DPI, and the
//! dash's measured height — and rounds to integers at the Win32/D2D
//! boundary.

use groveshell_motion::{lerp, lerp_rect, RectF};

/// Vertical spacing as a fraction of the work area's height
/// (`VERTICAL_SPACING_RATIO`).
const SPACING_RATIO: f64 = 0.02;
/// Cap on the dash's height (`DASH_MAX_HEIGHT_RATIO`).
const DASH_MAX_HEIGHT_RATIO: f64 = 0.16;
/// Cap on the workspace strip's height (`MAX_THUMBNAIL_SCALE`,
/// `workspaceThumbnail.js:19`).
const STRIP_MAX_SCALE: f64 = 0.05;
/// How the spacing around the strip is split above and below it
/// (`THUMBNAILS_SPACING_ADJUSTMENT_TOP`/`_BOTTOM`).
const STRIP_SPACING_TOP: f64 = 0.6;
const STRIP_SPACING_BOTTOM: f64 = 0.4;
/// Gap between neighbouring cards, as a fraction of a card's width. The
/// neighbours peek in at the screen edges rather than being hidden.
const CARD_GAP_RATIO: f64 = 0.06;
/// How dark the backdrop goes behind the cards at full overview. Read by
/// `overview_gpu::paint_backdrop`, which paints it once; the *animation*
/// of the dim comes from the root visual's opacity following progress,
/// not from repainting this.
pub(crate) const BACKDROP_DIM_MAX: f32 = 0.42;
/// A card's corner radius at full overview, at 96 DPI
/// (`BACKGROUND_CORNER_RADIUS_PIXELS`).
const CARD_RADIUS_DP: f64 = 20.0;
/// How much an off-centre workspace shrinks, so the focused one reads as
/// the subject (`WORKSPACE_INACTIVE_SCALE`, `workspacesView.js:25`).
/// Read by `overview_gpu::update_transforms`.
pub(crate) const WORKSPACE_INACTIVE_SCALE: f64 = 0.94;

/// The two states the overview interpolates between. GNOME has a third
/// (`APP_GRID`); GroveShell's type-to-search covers that ground, and the
/// adjustment's upper bound is the only thing that would need to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlsState {
    Hidden,
    WindowPicker,
}

/// Everything platform-shaped the layout needs, measured by the caller.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutInput {
    /// The whole monitor in overview-window client coordinates, top bar
    /// included. This is what a card is a miniature of.
    pub(crate) monitor: RectF,
    /// The usable area in overview-window client coordinates: the
    /// monitor's rect inset by the top bar.
    pub(crate) work_area: RectF,
    pub(crate) dpi: u32,
    /// The search entry's height, DPI-scaled.
    pub(crate) search_height: f64,
    /// The dash's natural height, DPI-scaled, before the max-height cap.
    pub(crate) dash_height: f64,
    /// width/height of the monitor, so a card is a faithful miniature.
    pub(crate) monitor_aspect: f64,
}

impl LayoutInput {
    fn scaled(&self, dp: f64) -> f64 {
        dp * self.dpi as f64 / 96.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverviewLayout {
    /// The band the focused card is centred in.
    pub(crate) workspace_box: RectF,
    /// The focused workspace's card: aspect-fit inside `workspace_box`.
    pub(crate) card: RectF,
    /// Page-to-page distance for the carousel.
    pub(crate) pitch: f64,
    pub(crate) strip: RectF,
    pub(crate) strip_alpha: f64,
    pub(crate) dash: RectF,
    pub(crate) search_entry: RectF,
    pub(crate) search_alpha: f64,
    /// 0..1 dim over the wallpaper behind the cards.
    pub(crate) backdrop_dim: f64,
    pub(crate) card_radius: f64,
}

/// Aspect-fits `aspect` inside `bounds`, centred.
fn aspect_fit(bounds: RectF, aspect: f64) -> RectF {
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    let (bw, bh) = (bounds.width().max(0.0), bounds.height().max(0.0));
    if bw <= 0.0 || bh <= 0.0 {
        let (cx, cy) = bounds.center();
        return RectF::new(cx, cy, cx, cy);
    }
    let (w, h) = if bw / bh > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
    let (cx, cy) = bounds.center();
    RectF::from_size(cx - w / 2.0, cy - h / 2.0, w, h)
}

pub(crate) fn layout_for_state(state: ControlsState, input: &LayoutInput) -> OverviewLayout {
    let work = input.work_area;
    let height = work.height().max(0.0);
    let spacing = (height * SPACING_RATIO).round();
    let dash_height = input.dash_height.min(height * DASH_MAX_HEIGHT_RATIO).max(0.0);
    let strip_height = (height * STRIP_MAX_SCALE).max(0.0);
    let search_height = input.search_height.max(0.0);

    match state {
        ControlsState::Hidden => {
            // The overview at rest is a picture of the desktop: the card
            // *is* the monitor, square-cornered, with the furniture
            // parked just off the edges it will slide in from.
            //
            // The monitor and not the work area, as in GNOME, where the
            // workspace runs under the panel. A card shows the desktop
            // as it is on screen, wallpaper behind the bar included, so
            // fitting that picture into the work area would squash it by
            // the bar's height and the first frame would visibly jump.
            let monitor = input.monitor;
            OverviewLayout {
                workspace_box: monitor,
                card: monitor,
                pitch: (monitor.width() * (1.0 + CARD_GAP_RATIO)).max(1.0),
                strip: RectF::from_size(
                    work.left,
                    work.top - strip_height,
                    work.width(),
                    strip_height,
                ),
                strip_alpha: 0.0,
                dash: RectF::from_size(work.left, work.bottom, work.width(), dash_height),
                search_entry: RectF::from_size(
                    work.left,
                    work.top - search_height,
                    work.width(),
                    search_height,
                ),
                search_alpha: 0.0,
                backdrop_dim: 0.0,
                card_radius: 0.0,
            }
        }
        ControlsState::WindowPicker => {
            let strip_top = work.top + search_height + (spacing * STRIP_SPACING_TOP).round();
            let box_top = strip_top + strip_height + (spacing * STRIP_SPACING_BOTTOM).round();
            // `max(0.0)` throughout: on a very short work area the
            // furniture can ask for more room than exists, and a
            // negative height would invert every rect downstream.
            let box_height = (height
                - search_height
                - (spacing * STRIP_SPACING_TOP).round()
                - strip_height
                - (spacing * STRIP_SPACING_BOTTOM).round()
                - dash_height
                - spacing)
                .max(0.0);
            let workspace_box = RectF::from_size(work.left, box_top, work.width(), box_height);
            let card = aspect_fit(workspace_box, input.monitor_aspect);

            OverviewLayout {
                workspace_box,
                card,
                pitch: (card.width() * (1.0 + CARD_GAP_RATIO)).max(1.0),
                strip: RectF::from_size(work.left, strip_top, work.width(), strip_height),
                strip_alpha: 1.0,
                dash: RectF::from_size(
                    work.left,
                    work.bottom - dash_height,
                    work.width(),
                    dash_height,
                ),
                search_entry: RectF::from_size(work.left, work.top, work.width(), search_height),
                search_alpha: 1.0,
                backdrop_dim: BACKDROP_DIM_MAX as f64,
                card_radius: input.scaled(CARD_RADIUS_DP),
            }
        }
    }
}

/// The layout at an arbitrary point between the two states — the whole
/// reason the transition is interruptible.
pub(crate) fn layout_for_progress(p: f64, input: &LayoutInput) -> OverviewLayout {
    let p = p.clamp(0.0, 1.0);
    let a = layout_for_state(ControlsState::Hidden, input);
    let b = layout_for_state(ControlsState::WindowPicker, input);
    OverviewLayout {
        workspace_box: lerp_rect(a.workspace_box, b.workspace_box, p),
        card: lerp_rect(a.card, b.card, p),
        pitch: lerp(a.pitch, b.pitch, p),
        strip: lerp_rect(a.strip, b.strip, p),
        strip_alpha: lerp(a.strip_alpha, b.strip_alpha, p),
        dash: lerp_rect(a.dash, b.dash, p),
        search_entry: lerp_rect(a.search_entry, b.search_entry, p),
        search_alpha: lerp(a.search_alpha, b.search_alpha, p),
        backdrop_dim: lerp(a.backdrop_dim, b.backdrop_dim, p),
        card_radius: lerp(a.card_radius, b.card_radius, p),
    }
}

/// Gap between strip thumbnails, as a fraction of one thumbnail's width.
const STRIP_GAP_RATIO: f64 = 0.08;

/// One slot per workspace in the strip, each a faithful miniature of the
/// monitor, laid out in page order and centred as a group.
pub(crate) fn strip_slots(strip: RectF, page_count: usize, aspect: f64) -> Vec<RectF> {
    if page_count == 0 {
        return Vec::new();
    }
    let h = strip.height().max(0.0);
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    let w = h * aspect;
    let gap = w * STRIP_GAP_RATIO;
    let total = w * page_count as f64 + gap * (page_count as f64 - 1.0);
    let start = strip.center().0 - total / 2.0;
    (0..page_count)
        .map(|i| RectF::from_size(start + i as f64 * (w + gap), strip.top, w, h))
        .collect()
}

/// Which page's strip slot contains `(x, y)`, if any. Gutters between
/// slots deliberately hit nothing rather than rounding to a neighbour —
/// a mis-click that switches workspace is worse than one that does
/// nothing.
pub(crate) fn strip_page_at(
    strip: RectF,
    page_count: usize,
    aspect: f64,
    x: f64,
    y: f64,
) -> Option<usize> {
    strip_slots(strip, page_count, aspect)
        .into_iter()
        .position(|s| x >= s.left && x < s.right && y >= s.top && y < s.bottom)
}

/// Where one window's preview sits at progress `p` — its real on-screen
/// rect at 0, its grid slot at 1. This is the port of
/// `workspace.js:732`, and it is what makes the overview's first frame
/// honest: at `p = 0` every preview covers the window it was captured
/// from, so the composed frame is indistinguishable from the desktop.
///
/// `home` of `None` means the window has no meaningful on-screen rect —
/// parked on another workspace, or minimized. Flying a preview out of an
/// empty patch of desktop would be a lie, so it stays at its slot and
/// fades instead (see [`thumb_alpha`]).
pub(crate) fn thumb_rect(home: Option<RectF>, slot: RectF, p: f64) -> RectF {
    let p = p.clamp(0.0, 1.0);
    match home {
        Some(home) => lerp_rect(home, slot, p),
        None => slot,
    }
}

/// Where a rect drawn on the card at rest in the picker (`picker_card`)
/// is while the card is at `card`: the same place on the card, at the
/// card's current size.
///
/// A preview with no window to fly from sits in its grid slot, and the
/// slot is a place on the card. Left at its picker coordinates it hung
/// still in the middle of the screen while the card shrank around it.
pub(crate) fn ride_card(r: RectF, picker_card: RectF, card: RectF) -> RectF {
    let sx = if picker_card.width() > 0.0 { card.width() / picker_card.width() } else { 1.0 };
    let sy = if picker_card.height() > 0.0 { card.height() / picker_card.height() } else { 1.0 };
    RectF::new(
        card.left + (r.left - picker_card.left) * sx,
        card.top + (r.top - picker_card.top) * sy,
        card.left + (r.right - picker_card.left) * sx,
        card.top + (r.bottom - picker_card.top) * sy,
    )
}

/// A preview's opacity at progress `p`. One that flies is opaque the
/// whole way — it is standing in for a real window that was already on
/// screen. One with no home fades in with the card, which is the honest
/// thing to show for a window that is not actually anywhere.
pub(crate) fn thumb_alpha(home: Option<RectF>, p: f64) -> f64 {
    match home {
        Some(_) => 1.0,
        None => p.clamp(0.0, 1.0),
    }
}

/// How far a live preview is inset from its rect at progress `p`, given
/// the inset it has at rest in the grid.
///
/// The inset exists to leave room for the hover ring, which only matters
/// in the grid. At `p = 0` the preview has to cover its window exactly,
/// or the first frame shows every window a few pixels smaller than it
/// was a moment ago.
pub(crate) fn live_inset(at_rest: f64, p: f64) -> f64 {
    at_rest * p.clamp(0.0, 1.0)
}

/// How much of its slot size a dragged window's ghost is drawn at.
///
/// Smaller than the slot so the ghost reads as "picked up and held"
/// rather than as the preview still sitting in the grid.
pub(crate) const GHOST_SCALE: f64 = 3.0 / 5.0;

/// Where a dragged window's ghost sits: centred on the cursor at
/// [`GHOST_SCALE`] of its slot size, shrunk further by `pop` (the
/// pickup/drop-out animation, 0.0..=1.0).
///
/// Shared so the live preview that follows the cursor and the still that
/// stands in for it land in exactly the same place — they used to be
/// computed separately, and a ghost drawn in one place while DWM
/// composited the live window in another is not a thing the user can
/// make sense of.
pub(crate) fn ghost_rect(cx: f64, cy: f64, base_w: f64, base_h: f64, pop: f64) -> RectF {
    let pop = pop.clamp(0.0, 1.0);
    let w = base_w * GHOST_SCALE * pop;
    let h = base_h * GHOST_SCALE * pop;
    RectF::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> LayoutInput {
        LayoutInput {
            monitor: RectF::new(0.0, 0.0, 1920.0, 1080.0),
            work_area: RectF::new(0.0, 24.0, 1920.0, 1080.0),
            dpi: 96,
            search_height: 40.0,
            dash_height: 72.0,
            monitor_aspect: 1920.0 / 1080.0,
        }
    }

    #[test]
    fn hidden_fills_the_monitor_with_square_corners() {
        let l = layout_for_state(ControlsState::Hidden, &input());
        assert_eq!(l.workspace_box, input().monitor);
        assert_eq!(l.card, input().monitor, "under the bar too, like the desktop it stands for");
        assert_eq!(l.card_radius, 0.0);
        assert_eq!(l.backdrop_dim, 0.0);
        assert_eq!(l.strip_alpha, 0.0);
        assert_eq!(l.search_alpha, 0.0);
    }

    #[test]
    fn hidden_parks_the_dash_entirely_below_the_work_area() {
        let l = layout_for_state(ControlsState::Hidden, &input());
        assert!(l.dash.top >= input().work_area.bottom);
    }

    #[test]
    fn hidden_parks_the_strip_entirely_above_the_work_area() {
        let l = layout_for_state(ControlsState::Hidden, &input());
        assert!(l.strip.bottom <= input().work_area.top);
    }

    #[test]
    fn the_window_picker_leaves_room_for_search_strip_and_dash() {
        let i = input();
        let l = layout_for_state(ControlsState::WindowPicker, &i);
        assert!(l.workspace_box.top > i.work_area.top + i.search_height);
        assert!(l.workspace_box.bottom < l.dash.top);
        assert!(l.strip.bottom <= l.workspace_box.top);
        assert!(l.dash.bottom <= i.work_area.bottom);
        assert!(l.card_radius > 0.0);
        assert!(l.backdrop_dim > 0.0);
        assert_eq!(l.strip_alpha, 1.0);
        assert_eq!(l.search_alpha, 1.0);
    }

    #[test]
    fn the_card_is_aspect_fit_inside_the_workspace_box() {
        let i = input();
        let l = layout_for_state(ControlsState::WindowPicker, &i);
        let card_aspect = l.card.width() / l.card.height();
        assert!((card_aspect - i.monitor_aspect).abs() < 0.01);
        assert!(l.card.width() <= l.workspace_box.width() + 0.01);
        assert!(l.card.height() <= l.workspace_box.height() + 0.01);
    }

    #[test]
    fn the_pitch_leaves_a_gap_between_neighbouring_cards() {
        let l = layout_for_state(ControlsState::WindowPicker, &input());
        assert!(l.pitch > l.card.width());
    }

    #[test]
    fn progress_zero_and_one_match_the_two_states() {
        let i = input();
        assert_eq!(
            layout_for_progress(0.0, &i).card,
            layout_for_state(ControlsState::Hidden, &i).card
        );
        assert_eq!(
            layout_for_progress(1.0, &i).card,
            layout_for_state(ControlsState::WindowPicker, &i).card
        );
    }

    #[test]
    fn progress_half_sits_between_the_two_states() {
        let i = input();
        let hidden = layout_for_state(ControlsState::Hidden, &i);
        let picker = layout_for_state(ControlsState::WindowPicker, &i);
        let mid = layout_for_progress(0.5, &i);

        assert!(mid.card.top > hidden.card.top && mid.card.top < picker.card.top);
        assert!(mid.dash.top < hidden.dash.top && mid.dash.top > picker.dash.top);
        assert!((mid.card_radius - picker.card_radius / 2.0).abs() < 0.01);
        assert!((mid.backdrop_dim - picker.backdrop_dim / 2.0).abs() < 0.01);
    }

    #[test]
    fn progress_is_clamped_to_the_two_states() {
        let i = input();
        assert_eq!(layout_for_progress(-1.0, &i).card, layout_for_progress(0.0, &i).card);
        assert_eq!(layout_for_progress(9.0, &i).card, layout_for_progress(1.0, &i).card);
    }

    /// Review Focus 4: a short or rotated display must not produce
    /// inverted boxes.
    #[test]
    fn a_work_area_too_short_for_its_furniture_still_yields_sane_boxes() {
        let i = LayoutInput {
            monitor: RectF::new(0.0, 0.0, 400.0, 120.0),
            work_area: RectF::new(0.0, 0.0, 400.0, 120.0),
            dpi: 96,
            search_height: 40.0,
            dash_height: 72.0,
            monitor_aspect: 400.0 / 120.0,
        };
        let l = layout_for_state(ControlsState::WindowPicker, &i);
        assert!(l.workspace_box.height() >= 0.0, "height must not invert");
        assert!(l.workspace_box.width() >= 0.0);
        assert!(l.card.height() >= 0.0);
        assert!(l.card.width() >= 0.0);
        assert!(l.pitch > 0.0, "a zero pitch would stack every page");
    }

    #[test]
    fn the_strip_lays_out_one_slot_per_page_centred_in_its_band() {
        let strip = RectF::new(0.0, 30.0, 1920.0, 84.0);
        let slots = strip_slots(strip, 3, 16.0 / 9.0);
        assert_eq!(slots.len(), 3);
        for s in &slots {
            assert!(s.top >= strip.top && s.bottom <= strip.bottom);
            assert!((s.width() / s.height() - 16.0 / 9.0).abs() < 0.01);
        }
        let span_center = (slots[0].left + slots[2].right) / 2.0;
        assert!((span_center - strip.center().0).abs() < 0.01);
        assert!(slots[0].right <= slots[1].left);
        assert!(slots[1].right <= slots[2].left);
    }

    #[test]
    fn an_empty_strip_has_no_slots() {
        assert!(strip_slots(RectF::new(0.0, 0.0, 100.0, 20.0), 0, 1.7).is_empty());
    }

    #[test]
    fn a_zero_height_strip_does_not_produce_inverted_slots() {
        let slots = strip_slots(RectF::new(0.0, 10.0, 1920.0, 10.0), 3, 1.7);
        for s in &slots {
            assert!(s.width() >= 0.0 && s.height() >= 0.0);
        }
    }

    #[test]
    fn hit_testing_the_strip_finds_the_page_under_the_pointer() {
        let strip = RectF::new(0.0, 30.0, 1920.0, 84.0);
        let slots = strip_slots(strip, 3, 16.0 / 9.0);
        for (i, s) in slots.iter().enumerate() {
            let (cx, cy) = s.center();
            assert_eq!(strip_page_at(strip, 3, 16.0 / 9.0, cx, cy), Some(i));
        }
        assert_eq!(strip_page_at(strip, 3, 16.0 / 9.0, 5.0, 35.0), None, "gutter hits nothing");
        assert_eq!(strip_page_at(strip, 3, 16.0 / 9.0, 960.0, 500.0), None, "below the strip");
    }

    #[test]
    fn a_preview_starts_on_its_real_window_and_lands_in_its_slot() {
        let home = RectF::new(100.0, 100.0, 900.0, 600.0);
        let slot = RectF::new(500.0, 400.0, 700.0, 520.0);
        assert_eq!(thumb_rect(Some(home), slot, 0.0), home);
        assert_eq!(thumb_rect(Some(home), slot, 1.0), slot);
    }

    #[test]
    fn a_preview_in_flight_is_between_its_two_ends() {
        let home = RectF::new(0.0, 0.0, 800.0, 600.0);
        let slot = RectF::new(400.0, 300.0, 600.0, 450.0);
        let mid = thumb_rect(Some(home), slot, 0.5);
        assert_eq!(mid, RectF::new(200.0, 150.0, 700.0, 525.0));
    }

    #[test]
    fn a_preview_is_fully_opaque_throughout_its_flight() {
        let home = Some(RectF::new(0.0, 0.0, 800.0, 600.0));
        for p in [0.0, 0.25, 0.5, 1.0] {
            assert_eq!(thumb_alpha(home, p), 1.0);
        }
    }

    /// Review Focus 2: a parked or minimized window has no real rect, so
    /// it must sit at its slot and fade rather than fly from nowhere.
    #[test]
    fn a_preview_without_a_home_sits_at_its_slot_and_fades_in() {
        let slot = RectF::new(500.0, 400.0, 700.0, 520.0);
        assert_eq!(thumb_rect(None, slot, 0.0), slot);
        assert_eq!(thumb_rect(None, slot, 0.5), slot);
        assert_eq!(thumb_rect(None, slot, 1.0), slot);

        assert_eq!(thumb_alpha(None, 0.0), 0.0);
        assert_eq!(thumb_alpha(None, 1.0), 1.0);
        assert!(thumb_alpha(None, 0.5) > 0.0 && thumb_alpha(None, 0.5) < 1.0);
    }

    #[test]
    fn a_live_preview_covers_its_window_exactly_at_rest_on_the_desktop() {
        assert_eq!(live_inset(6.0, 0.0), 0.0);
        assert_eq!(live_inset(6.0, 1.0), 6.0);
        assert_eq!(live_inset(6.0, 0.5), 3.0);
        assert_eq!(live_inset(6.0, -3.0), 0.0);
        assert_eq!(live_inset(6.0, 3.0), 6.0);
    }

    #[test]
    fn a_slot_keeps_its_place_on_the_card_as_the_card_changes_size() {
        let picker = RectF::new(100.0, 100.0, 500.0, 300.0);
        let slot = RectF::new(200.0, 150.0, 300.0, 200.0);
        assert_eq!(ride_card(slot, picker, picker), slot, "at rest it is where the grid put it");
        let big = RectF::new(0.0, 0.0, 800.0, 400.0);
        assert_eq!(ride_card(slot, picker, big), RectF::new(200.0, 100.0, 400.0, 200.0));
        let nothing = RectF::new(0.0, 0.0, 0.0, 0.0);
        let r = ride_card(slot, nothing, big);
        assert!(r.left.is_finite() && r.right.is_finite());
    }

    #[test]
    fn thumb_progress_is_clamped() {
        let home = RectF::new(0.0, 0.0, 100.0, 100.0);
        let slot = RectF::new(10.0, 10.0, 50.0, 50.0);
        assert_eq!(thumb_rect(Some(home), slot, -1.0), home);
        assert_eq!(thumb_rect(Some(home), slot, 2.0), slot);
        assert_eq!(thumb_alpha(None, -1.0), 0.0);
        assert_eq!(thumb_alpha(None, 7.0), 1.0);
    }

    #[test]
    fn a_ghost_is_centred_on_the_cursor() {
        let g = ghost_rect(500.0, 400.0, 400.0, 300.0, 1.0);
        let (cx, cy) = g.center();
        assert!((cx - 500.0).abs() < 1e-9, "centred horizontally, got {cx}");
        assert!((cy - 400.0).abs() < 1e-9, "centred vertically, got {cy}");
    }

    #[test]
    fn a_ghost_is_drawn_smaller_than_its_slot() {
        let g = ghost_rect(0.0, 0.0, 400.0, 300.0, 1.0);
        assert_eq!(g.width(), 400.0 * GHOST_SCALE);
        assert_eq!(g.height(), 300.0 * GHOST_SCALE);
        assert!(g.width() < 400.0);
    }

    /// The pickup pop grows the ghost from nothing, and the drop-out pop
    /// shrinks it back — both run through `pop`.
    #[test]
    fn the_pop_scales_the_ghost_about_the_cursor() {
        let half = ghost_rect(100.0, 100.0, 400.0, 300.0, 0.5);
        assert_eq!(half.width(), 400.0 * GHOST_SCALE * 0.5);
        let (cx, cy) = half.center();
        assert!((cx - 100.0).abs() < 1e-9, "still centred while popping");
        assert!((cy - 100.0).abs() < 1e-9);

        let none = ghost_rect(100.0, 100.0, 400.0, 300.0, 0.0);
        assert_eq!(none.width(), 0.0, "a fully popped-out ghost has no area");
        assert_eq!(none.height(), 0.0);
    }

    #[test]
    fn a_ghost_pop_outside_zero_to_one_is_clamped() {
        let over = ghost_rect(0.0, 0.0, 400.0, 300.0, 4.0);
        assert_eq!(over.width(), 400.0 * GHOST_SCALE, "never larger than full");
        let under = ghost_rect(0.0, 0.0, 400.0, 300.0, -2.0);
        assert_eq!(under.width(), 0.0, "never inverted");
    }
}
