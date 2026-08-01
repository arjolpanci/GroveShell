//! The settings window's left-hand vertical nav list.

use windows::Win32::Foundation::RECT;

pub(crate) const NAV_ITEMS: &[&str] = &["Home", "Dock", "Top Bar", "Overview", "Input"];
const NAV_ITEM_HEIGHT: i32 = 44;

/// One rect per nav item, top-to-bottom, each `NAV_ITEM_HEIGHT` tall and
/// `crate::imp::theme::NAV_WIDTH` wide — pure function of nothing but the
/// constants above, so painting and hit-testing can never disagree, same
/// pattern as `apps/ui`'s `card_layout`/`qs_layout`.
pub(crate) fn nav_layout() -> Vec<RECT> {
    (0..NAV_ITEMS.len())
        .map(|i| RECT {
            left: 0,
            top: i as i32 * NAV_ITEM_HEIGHT,
            right: super::theme::NAV_WIDTH,
            bottom: (i as i32 + 1) * NAV_ITEM_HEIGHT,
        })
        .collect()
}

pub(crate) fn nav_hit_test(x: i32, y: i32) -> Option<usize> {
    if x < 0 || x >= super::theme::NAV_WIDTH {
        return None;
    }
    nav_layout().iter().position(|r| y >= r.top && y < r.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_hit_test_finds_the_first_item_at_the_top() {
        assert_eq!(nav_hit_test(10, 5), Some(0));
    }

    #[test]
    fn nav_hit_test_finds_the_last_item() {
        let last_top = nav_layout().last().unwrap().top;
        assert_eq!(nav_hit_test(10, last_top + 5), Some(NAV_ITEMS.len() - 1));
    }

    #[test]
    fn nav_hit_test_outside_the_nav_width_returns_none() {
        assert_eq!(nav_hit_test(super::super::theme::NAV_WIDTH + 10, 5), None);
    }

    #[test]
    fn nav_hit_test_below_the_last_item_returns_none() {
        let below = nav_layout().last().unwrap().bottom + 100;
        assert_eq!(nav_hit_test(10, below), None);
    }
}
