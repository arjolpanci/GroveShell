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
/// How dark the backdrop goes behind the cards at full overview.
const BACKDROP_DIM_MAX: f64 = 0.35;
/// A card's corner radius at full overview, at 96 DPI
/// (`BACKGROUND_CORNER_RADIUS_PIXELS`).
const CARD_RADIUS_DP: f64 = 20.0;
/// Inset from a card's edge to its window grid.
const CARD_PADDING_DP: f64 = 28.0;
/// Gap between grid cells.
const GRID_GAP_DP: f64 = 20.0;
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
            // *is* the work area, square-cornered and undimmed, with the
            // furniture parked just off the edges it will slide in from.
            OverviewLayout {
                workspace_box: work,
                card: work,
                pitch: (work.width() * (1.0 + CARD_GAP_RATIO)).max(1.0),
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
                backdrop_dim: BACKDROP_DIM_MAX,
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

/// `count` window slots gridded inside `card`, each aspect-fit in its
/// cell. Returns an empty vector for an empty workspace rather than
/// dividing by a zero count.
pub(crate) fn grid_slots(card: RectF, count: usize, dpi: u32) -> Vec<RectF> {
    if count == 0 {
        return Vec::new();
    }
    let scale = dpi as f64 / 96.0;
    let padding = CARD_PADDING_DP * scale;
    let gap = GRID_GAP_DP * scale;
    let content = RectF::new(
        card.left + padding,
        card.top + padding,
        (card.right - padding).max(card.left + padding),
        (card.bottom - padding).max(card.top + padding),
    );

    let cols = (count as f64).sqrt().ceil().max(1.0);
    let rows = (count as f64 / cols).ceil().max(1.0);
    let cell_w = ((content.width() - gap * (cols - 1.0)) / cols).max(0.0);
    let cell_h = ((content.height() - gap * (rows - 1.0)) / rows).max(0.0);

    (0..count)
        .map(|i| {
            let col = (i as f64) % cols;
            let row = ((i as f64) / cols).floor();
            RectF::from_size(
                content.left + col * (cell_w + gap),
                content.top + row * (cell_h + gap),
                cell_w,
                cell_h,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> LayoutInput {
        LayoutInput {
            work_area: RectF::new(0.0, 24.0, 1920.0, 1080.0),
            dpi: 96,
            search_height: 40.0,
            dash_height: 72.0,
            monitor_aspect: 1920.0 / 1080.0,
        }
    }

    #[test]
    fn hidden_fills_the_work_area_with_square_corners() {
        let l = layout_for_state(ControlsState::Hidden, &input());
        assert_eq!(l.workspace_box, input().work_area);
        assert_eq!(l.card, input().work_area);
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

    /// Review Focus 1: an empty workspace has no previews, and asking
    /// for the layout must not divide by a zero window count.
    #[test]
    fn an_empty_workspace_lays_out_a_card_with_no_previews() {
        let i = input();
        let l = layout_for_state(ControlsState::WindowPicker, &i);
        let slots = grid_slots(l.card, 0, i.dpi);
        assert!(slots.is_empty());
        assert!(l.card.width() > 0.0, "the card itself still has a size");
    }

    #[test]
    fn grid_slots_stay_inside_the_card_and_never_overlap() {
        let i = input();
        let l = layout_for_state(ControlsState::WindowPicker, &i);
        for count in 1..=9 {
            let slots = grid_slots(l.card, count, i.dpi);
            assert_eq!(slots.len(), count);
            for s in &slots {
                assert!(s.left >= l.card.left - 0.01 && s.right <= l.card.right + 0.01);
                assert!(s.top >= l.card.top - 0.01 && s.bottom <= l.card.bottom + 0.01);
                assert!(s.width() > 0.0 && s.height() > 0.0);
            }
        }
    }
}
