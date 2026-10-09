//! What a click, a wheel notch and a Tab press mean, given a laid-out
//! page. Pure functions over [`super::layout::PageLayout`], so the rules
//! are testable without a window.

use super::layout::PageLayout;
use super::{Card, Control};

/// What the point under the cursor resolves to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// The row itself: activate it (toggle, invoke, follow the link).
    Row(u32),
    /// Inside a slider's track: `value` is what the cursor position maps
    /// to, already clamped to the slider's own range.
    SliderDrag { id: u32, value: f32 },
    None,
}

/// Resolves a click in *window* coordinates against a layout computed in
/// *content* coordinates. `scroll` is how far the content has been
/// scrolled up, so the point is pushed back down by it before testing —
/// without this, every click lands on whatever row used to be there.
pub fn hit_test(cards: &[Card], layout: &PageLayout, x: i32, y: i32, scroll: i32) -> Hit {
    let y = y + scroll;
    for (card_index, (_, row_rects)) in layout.cards.iter().enumerate() {
        let Some(card) = cards.get(card_index) else { continue };
        for (row_index, rects) in row_rects.iter().enumerate() {
            let Some(row) = card.rows.get(row_index) else { continue };
            let r = rects.row;
            if x < r.left || x >= r.right || y < r.top || y >= r.bottom {
                continue;
            }
            if !row.enabled {
                return Hit::None;
            }
            if let Control::Slider { min, max, .. } = row.control {
                let c = rects.control;
                if x >= c.left && x < c.right {
                    return Hit::SliderDrag { id: row.id, value: value_at(c.left, c.right, x, min, max) };
                }
            }
            return Hit::Row(row.id);
        }
    }
    Hit::None
}

/// Where `x` falls in `left..right`, mapped onto `min..=max`.
fn value_at(left: i32, right: i32, x: i32, min: f32, max: f32) -> f32 {
    let width = (right - left).max(1) as f32;
    let fraction = ((x - left) as f32 / width).clamp(0.0, 1.0);
    min + fraction * (max - min)
}

/// Keeps a scroll offset inside the range the content actually has.
/// Content shorter than the viewport has no scroll range at all, so this
/// returns 0 rather than a negative offset that would drag the page down.
pub fn clamp_scroll(scroll: i32, content_height: i32, viewport_height: i32) -> i32 {
    let max = (content_height - viewport_height).max(0);
    scroll.clamp(0, max)
}

/// The next row id Tab (or Shift+Tab) should land on, in visual order,
/// wrapping at both ends and skipping disabled rows.
pub fn next_focus(cards: &[Card], current: Option<u32>, forward: bool) -> Option<u32> {
    let order: Vec<u32> = cards
        .iter()
        .flat_map(|c| c.rows.iter())
        .filter(|r| r.enabled)
        .map(|r| r.id)
        .collect();
    if order.is_empty() {
        return None;
    }
    let Some(current) = current else {
        return Some(if forward { order[0] } else { order[order.len() - 1] });
    };
    match order.iter().position(|id| *id == current) {
        Some(index) => {
            let next = if forward {
                (index + 1) % order.len()
            } else {
                (index + order.len() - 1) % order.len()
            };
            Some(order[next])
        }
        // The focused row was disabled or removed since: start over.
        None => Some(order[0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::layout::layout_page;
    use crate::rows::{Card, Control, Row};
    use windows::Win32::Foundation::RECT;

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
    fn a_click_inside_a_row_hits_that_row() {
        let cards = vec![Card { caption: None, rows: vec![row(7, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let r = layout.cards[0].1[0].row;
        assert!(matches!(
            hit_test(&cards, &layout, r.left + 10, (r.top + r.bottom) / 2, 0),
            Hit::Row(7)
        ));
    }

    #[test]
    fn a_click_between_cards_hits_nothing() {
        let cards = vec![
            Card { caption: None, rows: vec![row(1, None)] },
            Card { caption: Some("Second".into()), rows: vec![row(2, None)] },
        ];
        let layout = layout_page(&cards, content(), 96);
        let gap_y = layout.cards[0].0.bottom + 1;
        assert!(matches!(hit_test(&cards, &layout, 100, gap_y, 0), Hit::None));
    }

    // Review Focus 4: hit-testing must account for the scroll offset.
    #[test]
    fn hit_testing_follows_the_scroll_offset() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let second = layout.cards[0].1[1].row;
        let scroll = 40;
        let on_screen_y = (second.top + second.bottom) / 2 - scroll;
        assert!(matches!(hit_test(&cards, &layout, 100, on_screen_y, scroll), Hit::Row(2)));
    }

    #[test]
    fn a_disabled_row_is_not_hit() {
        let mut r = row(5, None);
        r.enabled = false;
        let cards = vec![Card { caption: None, rows: vec![r] }];
        let layout = layout_page(&cards, content(), 96);
        let rect = layout.cards[0].1[0].row;
        assert!(matches!(
            hit_test(&cards, &layout, rect.left + 10, (rect.top + rect.bottom) / 2, 0),
            Hit::None
        ));
    }

    #[test]
    fn a_click_on_a_slider_reports_the_value_under_the_cursor() {
        let slider = Row::new(9, "Height")
            .with_control(Control::Slider { value: 24.0, min: 24.0, max: 48.0, unit: "px" });
        let cards = vec![Card { caption: None, rows: vec![slider] }];
        let layout = layout_page(&cards, content(), 96);
        let c = layout.cards[0].1[0].control;
        let middle = (c.left + c.right) / 2;
        match hit_test(&cards, &layout, middle, (c.top + c.bottom) / 2, 0) {
            Hit::SliderDrag { id, value } => {
                assert_eq!(id, 9);
                assert!((value - 36.0).abs() < 1.0, "midpoint should be about 36, got {value}");
            }
            other => panic!("expected a slider drag, got {other:?}"),
        }
    }

    #[test]
    fn scroll_clamps_at_both_ends() {
        assert_eq!(clamp_scroll(-50, 1000, 400), 0);
        assert_eq!(clamp_scroll(5000, 1000, 400), 600);
        assert_eq!(clamp_scroll(100, 1000, 400), 100);
    }

    #[test]
    fn content_shorter_than_the_viewport_never_scrolls() {
        assert_eq!(clamp_scroll(80, 200, 400), 0);
    }

    #[test]
    fn tab_order_matches_visual_order_and_wraps() {
        let cards = vec![
            Card { caption: None, rows: vec![row(1, None), row(2, None)] },
            Card { caption: None, rows: vec![row(3, None)] },
        ];
        assert_eq!(next_focus(&cards, None, true), Some(1));
        assert_eq!(next_focus(&cards, Some(1), true), Some(2));
        assert_eq!(next_focus(&cards, Some(3), true), Some(1));
        assert_eq!(next_focus(&cards, Some(1), false), Some(3));
    }

    #[test]
    fn focus_skips_a_disabled_row() {
        let mut middle = row(2, None);
        middle.enabled = false;
        let cards = vec![Card { caption: None, rows: vec![row(1, None), middle, row(3, None)] }];
        assert_eq!(next_focus(&cards, Some(1), true), Some(3));
    }
}
