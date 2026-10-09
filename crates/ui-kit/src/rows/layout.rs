//! Turning a page's declared cards into rectangles.
//!
//! Pure arithmetic: no Win32, no drawing, no state. Everything here is a
//! function of the cards, the content rectangle and the DPI, which is
//! what makes the layout rules testable rather than hopeful.
//!
//! The constants below were measured off a real Windows 11 Settings
//! window (Personalization > Taskbar, captured at 200% scale, so every
//! physical measurement is halved here), not guessed: card corner radius
//! 4, content column 1000 wide centred in the space beside the nav rail,
//! caption/header rows 67-68 tall, two-line rows 54-59, single-line 51.

use windows::Win32::Foundation::RECT;

use super::{Card, Control, Row};
use crate::runtime::scaled;

/// Below this the content column stops shrinking; the window refuses to
/// size smaller (see the settings window's `WM_GETMINMAXINFO`).
pub const MIN_CONTENT_WIDTH: i32 = 480;

/// Above this the column stops growing and centres instead, so settings
/// never stretch into an unreadable line on a wide monitor. Measured at
/// 1000 logical pixels in Windows 11 Settings.
pub const MAX_CONTENT_WIDTH: i32 = 1000;

/// A row with a title only.
const ROW_HEIGHT_SINGLE: i32 = 52;
/// A row with a title and a description.
const ROW_HEIGHT_DOUBLE: i32 = 60;
/// The caption line above a card.
const CAPTION_HEIGHT: i32 = 32;
/// Gap between one card and the next inside a page.
const CARD_GAP: i32 = 4;
/// Extra gap before a card that introduces a new captioned group.
const GROUP_GAP: i32 = 20;
/// Padding from the card's edge to its content.
pub(crate) const ROW_PAD_X: i32 = 16;
/// The icon column: a 20px glyph, 12px of air, then the text.
const ICON_SIZE: i32 = 20;
const ICON_GUTTER: i32 = ICON_SIZE + 12;
/// Breathing room between the text column and the control.
const TEXT_CONTROL_GAP: i32 = 16;
/// Top and bottom margin of the whole content column.
const PAGE_PAD_Y: i32 = 16;
/// Minimum air between the content column and the edges of the area it
/// sits in. Windows 11 Settings never runs a card flush against the
/// window frame, even on a narrow window where the column has stopped
/// growing and has no centring margin of its own.
const PAGE_PAD_X: i32 = 24;

/// Widths each control reserves on the right-hand side of a row.
const TOGGLE_WIDTH: i32 = 40;
/// The `On`/`Off` label Windows draws to the left of a switch.
const TOGGLE_LABEL_WIDTH: i32 = 28;
const SLIDER_WIDTH: i32 = 200;
/// Room reserved to the right of a slider's track for its value label
/// ("32px"). It sits outside the control rect on purpose: the control
/// rect is what a click is mapped across, so anything that is not the
/// draggable track must not be inside it.
pub(crate) const SLIDER_LABEL_WIDTH: i32 = 48;
const CHOICE_WIDTH: i32 = 160;
const ACTION_WIDTH: i32 = 120;

/// Where one row's parts ended up. Every consumer — paint, hit-testing,
/// focus — reads these rather than recomputing them.
#[derive(Clone, Copy, Debug)]
pub struct RowRects {
    pub row: RECT,
    pub glyph: RECT,
    pub text: RECT,
    pub control: RECT,
}

/// A laid-out page: one entry per card, holding the card's own rectangle
/// and its rows' rectangles, plus the total height for scrolling.
#[derive(Clone, Debug)]
pub struct PageLayout {
    pub cards: Vec<(RECT, Vec<RowRects>)>,
    /// Height of everything laid out, measured from `content.top`. The
    /// scroll range is this minus the viewport height.
    pub content_height: i32,
}

/// The width a control reserves at this DPI, label included.
fn control_width(control: &Control, dpi: u32) -> i32 {
    let logical = match control {
        Control::Toggle { .. } => TOGGLE_LABEL_WIDTH + TOGGLE_WIDTH,
        Control::Slider { .. } => SLIDER_WIDTH + SLIDER_LABEL_WIDTH,
        Control::Choice { .. } => CHOICE_WIDTH,
        Control::Action { .. } => ACTION_WIDTH,
        Control::Link | Control::Status { .. } | Control::None => 0,
    };
    scaled(logical, dpi)
}

fn row_height(row: &Row, dpi: u32) -> i32 {
    let logical = if row.description.is_some() { ROW_HEIGHT_DOUBLE } else { ROW_HEIGHT_SINGLE };
    scaled(logical, dpi)
}

/// Lays `cards` out inside `content`, which is the area beside the nav
/// rail. The column is centred and clamped to
/// `MIN_CONTENT_WIDTH..=MAX_CONTENT_WIDTH`.
pub fn layout_page(cards: &[Card], content: RECT, dpi: u32) -> PageLayout {
    let pad = scaled(PAGE_PAD_X, dpi);
    let available = (content.right - content.left - pad * 2).max(scaled(MIN_CONTENT_WIDTH, dpi) - pad * 2);
    let column_width = available.min(scaled(MAX_CONTENT_WIDTH, dpi));
    let left = content.left + pad + (available - column_width) / 2;
    let right = left + column_width;

    let pad_x = scaled(ROW_PAD_X, dpi);
    let icon_gutter = scaled(ICON_GUTTER, dpi);
    let icon_size = scaled(ICON_SIZE, dpi);
    let text_gap = scaled(TEXT_CONTROL_GAP, dpi);

    let mut y = content.top + scaled(PAGE_PAD_Y, dpi);
    let mut laid_out = Vec::with_capacity(cards.len());

    for (index, card) in cards.iter().enumerate() {
        if index > 0 {
            y += scaled(if card.caption.is_some() { GROUP_GAP } else { CARD_GAP }, dpi);
        }
        if card.caption.is_some() {
            y += scaled(CAPTION_HEIGHT, dpi);
        }

        let card_top = y;
        let mut row_rects = Vec::with_capacity(card.rows.len());
        for row in &card.rows {
            let height = row_height(row, dpi);
            let rect = RECT { left, top: y, right, bottom: y + height };

            let glyph_left = rect.left + pad_x;
            let glyph = RECT {
                left: glyph_left,
                top: rect.top + (height - icon_size) / 2,
                right: glyph_left + icon_size,
                bottom: rect.top + (height - icon_size) / 2 + icon_size,
            };

            let width = control_width(&row.control, dpi);
            let control_left = (rect.right - pad_x - width).max(rect.left + pad_x);
            // A slider's value label lives outside the control rect, so
            // the rect is exactly the track a click is mapped across.
            let control_right = rect.right
                - pad_x
                - match row.control {
                    Control::Slider { .. } => scaled(SLIDER_LABEL_WIDTH, dpi),
                    _ => 0,
                };
            let control = RECT {
                left: control_left,
                top: rect.top,
                right: control_right.max(control_left),
                bottom: rect.bottom,
            };

            // The text column takes what is left, and never reaches the
            // control: at the minimum width this clamp is the only thing
            // keeping a long description from being drawn underneath it.
            let text_left = rect.left + pad_x + if row.glyph.is_some() { icon_gutter } else { 0 };
            let text_right = (control.left - text_gap).max(text_left + 1);
            let text = RECT { left: text_left, top: rect.top, right: text_right, bottom: rect.bottom };

            row_rects.push(RowRects { row: rect, glyph, text, control });
            y += height;
        }

        laid_out.push((RECT { left, top: card_top, right, bottom: y }, row_rects));
    }

    let content_height = (y + scaled(PAGE_PAD_Y, dpi)) - content.top;
    PageLayout { cards: laid_out, content_height }
}

/// How a slider's value is written next to its track.
///
/// A range only a couple of units wide is meaningless as an integer:
/// the animation-speed slider runs 0.5x to 2.0x, so rounding would print
/// "1x" from 0.5 all the way to 1.4. Narrow ranges get a decimal.
pub fn format_slider_value(value: f32, min: f32, max: f32, unit: &str) -> String {
    if (max - min).abs() < 5.0 {
        format!("{value:.1}{unit}")
    } else {
        format!("{}{unit}", value.round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_slider_range_is_written_as_a_whole_number() {
        assert_eq!(format_slider_value(32.0, 24.0, 48.0, "px"), "32px");
        assert_eq!(format_slider_value(41.4, 32.0, 64.0, "px"), "41px");
    }

    #[test]
    fn a_narrow_slider_range_keeps_a_decimal() {
        // 0.5x..2.0x: rounding would read "1x" across most of the track.
        assert_eq!(format_slider_value(0.5, 0.5, 2.0, "x"), "0.5x");
        assert_eq!(format_slider_value(1.3, 0.5, 2.0, "x"), "1.3x");
        assert_eq!(format_slider_value(2.0, 0.5, 2.0, "x"), "2.0x");
    }

    #[test]
    fn every_step_of_a_narrow_range_reads_differently() {
        let labels: Vec<String> = (0..=15)
            .map(|i| format_slider_value(0.5 + i as f32 * 0.1, 0.5, 2.0, "x"))
            .collect();
        let unique: std::collections::BTreeSet<&String> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "distinct values share a label: {labels:?}");
    }

    fn row(id: u32, description: Option<&str>) -> Row {
        Row {
            id,
            glyph: Some("\u{E700}"),
            title: format!("Row {id}"),
            description: description.map(|d| d.to_string()),
            control: Control::Toggle { on: false },
            enabled: true,
        }
    }

    fn content() -> RECT {
        RECT { left: 0, top: 0, right: 900, bottom: 600 }
    }

    #[test]
    fn rows_stack_in_order_without_overlapping() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, None), row(3, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let rects = &layout.cards[0].1;
        for pair in rects.windows(2) {
            assert!(
                pair[0].row.bottom <= pair[1].row.top,
                "rows overlap: {:?} then {:?}",
                pair[0].row,
                pair[1].row
            );
        }
    }

    #[test]
    fn a_two_line_row_is_taller_than_a_one_line_row() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, Some("A description"))] }];
        let layout = layout_page(&cards, content(), 96);
        let rects = &layout.cards[0].1;
        let one_line = rects[0].row.bottom - rects[0].row.top;
        let two_line = rects[1].row.bottom - rects[1].row.top;
        assert!(two_line > one_line);
    }

    #[test]
    fn every_part_of_a_row_stays_inside_it() {
        let cards = vec![Card { caption: None, rows: vec![row(1, Some("desc"))] }];
        let layout = layout_page(&cards, content(), 96);
        let r = &layout.cards[0].1[0];
        for part in [r.glyph, r.text, r.control] {
            assert!(part.left >= r.row.left && part.right <= r.row.right, "{part:?} escapes {:?}", r.row);
            assert!(part.top >= r.row.top && part.bottom <= r.row.bottom);
        }
    }

    #[test]
    fn text_never_runs_under_the_control() {
        let cards = vec![Card {
            caption: None,
            rows: vec![row(
                1,
                Some("a very long description that would happily run the whole width of the row if nothing stopped it"),
            )],
        }];
        let layout = layout_page(&cards, content(), 96);
        let r = &layout.cards[0].1[0];
        assert!(r.text.right <= r.control.left);
    }

    // Review Focus 2: a window narrower than its content.
    #[test]
    fn a_narrow_content_area_still_produces_a_sane_row() {
        let narrow = RECT { left: 0, top: 0, right: MIN_CONTENT_WIDTH, bottom: 600 };
        let cards = vec![Card { caption: None, rows: vec![row(1, Some("desc"))] }];
        let layout = layout_page(&cards, narrow, 96);
        let r = &layout.cards[0].1[0];
        assert!(r.text.right <= r.control.left, "text and control collided at the minimum width");
        assert!(r.control.right <= narrow.right);
        assert!(r.text.left < r.text.right, "text column collapsed to nothing");
    }

    #[test]
    fn the_column_never_touches_the_edges_of_the_content_area() {
        // Windows 11 Settings always leaves air between the card and the
        // window edge; a card flush against the frame reads as broken.
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        for width in [MIN_CONTENT_WIDTH, 700, 900, 1200] {
            let area = RECT { left: 0, top: 0, right: width, bottom: 600 };
            let card = layout_page(&cards, area, 96).cards[0].0;
            assert!(card.left > area.left, "card touches the left edge at width {width}");
            assert!(card.right < area.right, "card touches the right edge at width {width}");
        }
    }

    #[test]
    fn the_content_column_stops_growing_on_a_very_wide_window() {
        let wide = RECT { left: 0, top: 0, right: 4000, bottom: 600 };
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        let layout = layout_page(&cards, wide, 96);
        let card = layout.cards[0].0;
        assert!(card.right - card.left <= MAX_CONTENT_WIDTH);
    }

    #[test]
    fn a_sliders_control_rect_is_the_track_the_user_actually_drags() {
        // `hit_test` maps a click across the control rect, and `paint`
        // draws the track inside it. If the two disagree the thumb lands
        // somewhere other than the cursor, so the rect must be the track
        // alone, with the value label reserved beyond its right edge.
        let slider = Row::new(1, "Height")
            .with_control(Control::Slider { value: 24.0, min: 24.0, max: 48.0, unit: "px" });
        let cards = vec![Card { caption: None, rows: vec![slider] }];
        let layout = layout_page(&cards, content(), 96);
        let r = &layout.cards[0].1[0];
        let label_room = r.row.right - r.control.right;
        assert!(
            label_room >= SLIDER_LABEL_WIDTH,
            "only {label_room}px left for the value label; the track would overlap it"
        );
    }

    #[test]
    fn layout_scales_with_dpi() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        let at96 = layout_page(&cards, content(), 96);
        let at192 = layout_page(&cards, content(), 192);
        let h96 = at96.cards[0].1[0].row.bottom - at96.cards[0].1[0].row.top;
        let h192 = at192.cards[0].1[0].row.bottom - at192.cards[0].1[0].row.top;
        assert_eq!(h192, h96 * 2);
    }

    #[test]
    fn content_height_covers_every_card() {
        let cards = vec![
            Card { caption: Some("One".into()), rows: vec![row(1, None), row(2, None)] },
            Card { caption: Some("Two".into()), rows: vec![row(3, None)] },
        ];
        let layout = layout_page(&cards, content(), 96);
        let last = layout.cards.last().unwrap().0;
        assert!(layout.content_height >= last.bottom - content().top);
    }
}
