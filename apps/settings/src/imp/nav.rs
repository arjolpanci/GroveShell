//! The settings window's left-hand navigation rail.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::runtime::scaled;
use windows::Win32::Foundation::RECT;

/// One rail entry. The glyph is a Segoe Fluent Icons code point, drawn
/// left of the label the way Windows 11 Settings draws its own rail.
pub(crate) struct NavItem {
    pub(crate) label: &'static str,
    pub(crate) glyph: &'static str,
}

pub(crate) const NAV_ITEMS: &[NavItem] = &[
    NavItem { label: "Home", glyph: glyph::HOME },
    NavItem { label: "Dock", glyph: glyph::DOCK },
    NavItem { label: "Top Bar", glyph: glyph::RESIZE },
    NavItem { label: "Activities", glyph: glyph::OVERVIEW },
    NavItem { label: "Input", glyph: glyph::INPUT },
    NavItem { label: "Accessibility", glyph: glyph::ACCESSIBILITY },
];

/// Logical metrics, scaled at use. Measured against Windows 11 Settings'
/// own rail: a 40px row with the selection pill inset inside it.
pub(crate) const NAV_WIDTH: i32 = 220;
const NAV_ITEM_HEIGHT: i32 = 40;
const NAV_TOP_INSET: i32 = 12;

/// One rect per nav item, top to bottom — a pure function of the DPI and
/// the constants above, so painting and hit-testing cannot disagree.
pub(crate) fn nav_layout(dpi: u32) -> Vec<RECT> {
    let height = scaled(NAV_ITEM_HEIGHT, dpi);
    let top = scaled(NAV_TOP_INSET, dpi);
    (0..NAV_ITEMS.len())
        .map(|i| RECT {
            left: 0,
            top: top + i as i32 * height,
            right: scaled(NAV_WIDTH, dpi),
            bottom: top + (i as i32 + 1) * height,
        })
        .collect()
}

pub(crate) fn nav_hit_test(x: i32, y: i32, dpi: u32) -> Option<usize> {
    if x < 0 || x >= scaled(NAV_WIDTH, dpi) {
        return None;
    }
    nav_layout(dpi).iter().position(|r| y >= r.top && y < r.bottom)
}

/// Keyboard navigation in the rail: up and down move one item and stop
/// at the ends rather than wrapping, matching how a Windows nav view
/// behaves under the arrow keys.
pub(crate) fn next_nav(current: usize, delta: i32) -> usize {
    let last = NAV_ITEMS.len() - 1;
    (current as i32 + delta).clamp(0, last as i32) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_nav_item_has_a_glyph() {
        for item in NAV_ITEMS {
            assert!(!item.glyph.is_empty(), "{} has no icon", item.label);
        }
    }

    #[test]
    fn keyboard_navigation_moves_and_stops_at_the_ends() {
        assert_eq!(next_nav(0, 1), 1);
        assert_eq!(next_nav(0, -1), 0);
        assert_eq!(next_nav(NAV_ITEMS.len() - 1, 1), NAV_ITEMS.len() - 1);
    }

    #[test]
    fn nav_hit_test_finds_the_first_item() {
        let first = nav_layout(96)[0];
        assert_eq!(nav_hit_test(10, first.top + 2, 96), Some(0));
    }

    #[test]
    fn nav_hit_test_finds_the_last_item() {
        let last = *nav_layout(96).last().unwrap();
        assert_eq!(nav_hit_test(10, last.top + 2, 96), Some(NAV_ITEMS.len() - 1));
    }

    #[test]
    fn nav_hit_test_outside_the_rail_returns_none() {
        assert_eq!(nav_hit_test(scaled(NAV_WIDTH, 96) + 10, 20, 96), None);
    }

    #[test]
    fn nav_hit_test_below_the_last_item_returns_none() {
        let below = nav_layout(96).last().unwrap().bottom + 100;
        assert_eq!(nav_hit_test(10, below, 96), None);
    }

    #[test]
    fn nav_rows_do_not_overlap_at_any_dpi() {
        for dpi in [96, 120, 144, 192] {
            for pair in nav_layout(dpi).windows(2) {
                assert!(pair[0].bottom <= pair[1].top, "nav rows overlap at {dpi} dpi");
            }
        }
    }
}
