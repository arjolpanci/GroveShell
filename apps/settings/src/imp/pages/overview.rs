//! Overview settings: blur, reduced motion, and animation speed.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_slider, draw_toggle, hit_toggle, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

pub(crate) struct OverviewPage;

impl OverviewPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn blur_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 24,
        }
    }

    fn reduced_motion_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }

    fn speed_slider_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + 24,
        }
    }
}

impl Page for OverviewPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            let blur_toggle = self.blur_toggle_rect(content_rect);
            draw_toggle(hdc, blur_toggle, config.appearance.overview_blur);
            draw_centered_text(hdc, RECT { left: blur_toggle.right + 12, top: blur_toggle.top, right: blur_toggle.right + 200, bottom: blur_toggle.bottom }, "Blur", TEXT_MUTED);

            let motion_toggle = self.reduced_motion_toggle_rect(content_rect);
            draw_toggle(hdc, motion_toggle, config.appearance.reduced_motion);
            draw_centered_text(hdc, RECT { left: motion_toggle.right + 12, top: motion_toggle.top, right: motion_toggle.right + 200, bottom: motion_toggle.bottom }, "Reduced motion", TEXT_MUTED);

            let label = format!("Animation speed: {:.1}x", config.appearance.animation_scale);
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 4, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 4 + 24 },
                &label,
                TEXT_MUTED,
            );
            // Drawn regardless of reduced_motion (so the last chosen value
            // stays visible), but clicks on it are ignored while reduced
            // motion is on — see on_click.
            draw_slider(hdc, self.speed_slider_rect(content_rect), config.appearance.animation_scale, 0.5, 2.0);
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let blur_toggle = self.blur_toggle_rect(content_rect);
        if hit_toggle(blur_toggle, x, y) {
            let current = config_store::current().appearance.overview_blur;
            config_store::update(|c| c.appearance.overview_blur = !current);
            return;
        }
        let motion_toggle = self.reduced_motion_toggle_rect(content_rect);
        if hit_toggle(motion_toggle, x, y) {
            let current = config_store::current().appearance.reduced_motion;
            config_store::update(|c| c.appearance.reduced_motion = !current);
            return;
        }
        if config_store::current().appearance.reduced_motion {
            return; // Slider ignores clicks while reduced motion is on.
        }
        let slider = self.speed_slider_rect(content_rect);
        if y >= slider.top - 8 && y < slider.bottom + 8 && x >= slider.left && x < slider.right {
            let value = value_from_slider_x(slider, x, 0.5, 2.0);
            config_store::update(|c| c.appearance.animation_scale = value);
        }
    }
}
