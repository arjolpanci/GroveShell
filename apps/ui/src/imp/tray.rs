//! Where the chevron's hidden-icons panel goes. The panel itself is
//! `tray_overflow`; the icons in it and in the bar come from
//! `tray_icons`, which hosts the notification area.

use windows::Win32::Foundation::RECT;

/// Top-left position for the panel so its right edge sits under the
/// chevron's right edge, dropped just below the bar, clamped to the
/// monitor. Pure so it can be unit-tested without any window present.
pub(crate) fn overflow_position(
    chevron: RECT,
    size_w: i32,
    monitor_left: i32,
    monitor_right: i32,
    below_y: i32,
) -> (i32, i32) {
    let x = (chevron.right - size_w).clamp(monitor_left, (monitor_right - size_w).max(monitor_left));
    (x, below_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: r, bottom: b }
    }

    #[test]
    fn overflow_right_aligns_under_chevron() {
        // chevron right at 500, overflow 200 wide -> left at 300.
        assert_eq!(overflow_position(rect(480, 0, 500, 24), 200, 0, 1000, 30), (300, 30));
    }

    #[test]
    fn overflow_clamps_to_monitor_left() {
        // chevron near the left edge can't push the window off-screen.
        assert_eq!(overflow_position(rect(30, 0, 50, 24), 200, 0, 1000, 30).0, 0);
    }

    #[test]
    fn overflow_clamps_to_monitor_right() {
        // A chevron past the right edge clamps so the window stays on-screen.
        assert_eq!(overflow_position(rect(1180, 0, 1200, 24), 200, 0, 1000, 30).0, 800);
    }
}
