//! The popup a [`super::Control::Choice`] row opens.
//!
//! Drawn into the settings window's own surface rather than a second
//! top-level window: a settings dropdown never needs to escape the
//! window it belongs to, and an overlay avoids a popup HWND's activation
//! and dismissal problems entirely.
//!
//! Geometry only in this module, so where the list lands — and which
//! option a click picks — is testable without a window. The one rule
//! that matters: the list is placed so it is always fully on screen, and
//! `option_at` inverts `option_rect` exactly, so the option under the
//! cursor is the option the user sees highlighted.

use windows::Win32::Foundation::RECT;

use crate::runtime::scaled;

/// Height of one option in the list.
pub const OPTION_HEIGHT: i32 = 36;
/// Padding above and below the list's options.
const POPUP_PAD_Y: i32 = 4;

/// Where the option list goes for a choice whose control sits at
/// `control`, inside a window whose client area is `client`.
///
/// Opens downward from the control's bottom edge, and flips to open
/// upward when that would run past the bottom of the window — the
/// behavior every native dropdown has near a screen edge.
pub fn popup_rect(control: RECT, client: RECT, option_count: usize, dpi: u32) -> RECT {
    let height = scaled(POPUP_PAD_Y, dpi) * 2 + scaled(OPTION_HEIGHT, dpi) * option_count as i32;
    let below_top = control.bottom;
    if below_top + height <= client.bottom {
        return RECT { left: control.left, top: below_top, right: control.right, bottom: below_top + height };
    }
    let above_bottom = control.top;
    let top = (above_bottom - height).max(client.top);
    RECT { left: control.left, top, right: control.right, bottom: top + height }
}

/// The rectangle of one option inside `popup`.
pub fn option_rect(popup: RECT, index: usize, dpi: u32) -> RECT {
    let height = scaled(OPTION_HEIGHT, dpi);
    let top = popup.top + scaled(POPUP_PAD_Y, dpi) + index as i32 * height;
    RECT { left: popup.left, top, right: popup.right, bottom: top + height }
}

/// Which option a point falls on, if any.
pub fn option_at(popup: RECT, option_count: usize, x: i32, y: i32, dpi: u32) -> Option<usize> {
    if x < popup.left || x >= popup.right {
        return None;
    }
    (0..option_count).find(|index| {
        let r = option_rect(popup, *index, dpi);
        y >= r.top && y < r.bottom
    })
}

/// Whether a point is anywhere inside the popup. A click outside closes
/// the dropdown without changing the value, so the caller needs to tell
/// "missed every option" from "clicked the padding".
pub fn contains(popup: RECT, x: i32, y: i32) -> bool {
    x >= popup.left && x < popup.right && y >= popup.top && y < popup.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> RECT {
        RECT { left: 0, top: 0, right: 900, bottom: 600 }
    }

    fn control() -> RECT {
        RECT { left: 600, top: 100, right: 860, bottom: 160 }
    }

    #[test]
    fn the_list_opens_below_the_control_when_there_is_room() {
        let popup = popup_rect(control(), client(), 3, 96);
        assert_eq!(popup.top, control().bottom);
        assert!(popup.bottom <= client().bottom);
    }

    #[test]
    fn the_list_aligns_with_the_control_it_belongs_to() {
        let popup = popup_rect(control(), client(), 3, 96);
        assert_eq!(popup.left, control().left);
        assert_eq!(popup.right, control().right);
    }

    #[test]
    fn the_list_flips_above_the_control_near_the_bottom_edge() {
        let low = RECT { top: 520, bottom: 570, ..control() };
        let popup = popup_rect(low, client(), 4, 96);
        assert!(popup.bottom <= low.top, "list should open upward, got {popup:?}");
        assert!(popup.top >= client().top);
    }

    #[test]
    fn every_option_sits_inside_the_list_without_overlapping() {
        let popup = popup_rect(control(), client(), 4, 96);
        let rects: Vec<RECT> = (0..4).map(|i| option_rect(popup, i, 96)).collect();
        for r in &rects {
            assert!(r.top >= popup.top && r.bottom <= popup.bottom, "{r:?} escapes {popup:?}");
        }
        for pair in rects.windows(2) {
            assert!(pair[0].bottom <= pair[1].top, "options overlap");
        }
    }

    #[test]
    fn a_click_picks_the_option_it_lands_on() {
        let popup = popup_rect(control(), client(), 3, 96);
        for index in 0..3 {
            let r = option_rect(popup, index, 96);
            let hit = option_at(popup, 3, r.left + 5, (r.top + r.bottom) / 2, 96);
            assert_eq!(hit, Some(index), "option {index} not picked at its own centre");
        }
    }

    #[test]
    fn a_click_in_the_lists_padding_picks_nothing() {
        let popup = popup_rect(control(), client(), 3, 96);
        assert_eq!(option_at(popup, 3, popup.left + 5, popup.top + 1, 96), None);
        assert!(contains(popup, popup.left + 5, popup.top + 1));
    }

    #[test]
    fn a_click_outside_the_list_is_outside() {
        let popup = popup_rect(control(), client(), 3, 96);
        assert!(!contains(popup, popup.left - 10, popup.top + 10));
        assert!(!contains(popup, popup.left + 10, popup.bottom + 10));
    }

    #[test]
    fn the_list_scales_with_dpi() {
        let at96 = popup_rect(control(), client(), 3, 96);
        let at192 = popup_rect(
            RECT { left: 600, top: 100, right: 860, bottom: 160 },
            RECT { left: 0, top: 0, right: 1800, bottom: 1200 },
            3,
            192,
        );
        assert_eq!(at192.bottom - at192.top, (at96.bottom - at96.top) * 2);
    }
}
