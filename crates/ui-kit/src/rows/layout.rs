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
const ROW_PAD_X: i32 = 16;
/// The icon column: a 20px glyph, 12px of air, then the text.
const ICON_SIZE: i32 = 20;
const ICON_GUTTER: i32 = ICON_SIZE + 12;
/// Breathing room between the text column and the control.
const TEXT_CONTROL_GAP: i32 = 16;
/// Top and bottom margin of the whole content column.
const PAGE_PAD_Y: i32 = 16;

/// Widths each control reserves on the right-hand side of a row.
const TOGGLE_WIDTH: i32 = 40;
/// The `On`/`Off` label Windows draws to the left of a switch.
const TOGGLE_LABEL_WIDTH: i32 = 28;
const SLIDER_WIDTH: i32 = 200;
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
        Control::Slider { .. } => SLIDER_WIDTH,
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
    let available = (content.right - content.left).max(scaled(MIN_CONTENT_WIDTH, dpi));
    let column_width = available.min(scaled(MAX_CONTENT_WIDTH, dpi));
    let left = content.left + (available - column_width) / 2;
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
            let control = RECT {
                left: control_left,
                top: rect.top,
                right: rect.right - pad_x,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_content_column_stops_growing_on_a_very_wide_window() {
        let wide = RECT { left: 0, top: 0, right: 4000, bottom: 600 };
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        let layout = layout_page(&cards, wide, 96);
        let card = layout.cards[0].0;
        assert!(card.right - card.left <= MAX_CONTENT_WIDTH);
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
