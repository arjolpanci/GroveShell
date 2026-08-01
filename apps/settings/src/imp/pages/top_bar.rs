//! Top Bar settings: height and blur.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_slider, draw_toggle, hit_toggle, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

pub(crate) struct TopBarPage;

impl TopBarPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn height_slider_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 24,
        }
    }

    fn blur_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }
}

impl Page for TopBarPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                &format!("Height: {}px", config.appearance.top_bar_height),
                TEXT_MUTED,
            );
            draw_slider(hdc, self.height_slider_rect(content_rect), config.appearance.top_bar_height as f32, 24.0, 48.0);

            let toggle = self.blur_toggle_rect(content_rect);
            draw_toggle(hdc, toggle, config.appearance.top_bar_blur);
            draw_centered_text(
                hdc,
                RECT { left: toggle.right + 12, top: toggle.top, right: toggle.right + 200, bottom: toggle.bottom },
                "Blur",
                TEXT_MUTED,
            );
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let slider = self.height_slider_rect(content_rect);
        if y >= slider.top - 8 && y < slider.bottom + 8 && x >= slider.left && x < slider.right {
            let value = value_from_slider_x(slider, x, 24.0, 48.0).round() as u32;
            config_store::update(|c| c.appearance.top_bar_height = value);
            return;
        }
        let toggle = self.blur_toggle_rect(content_rect);
        if hit_toggle(toggle, x, y) {
            let current = config_store::current().appearance.top_bar_blur;
            config_store::update(|c| c.appearance.top_bar_blur = !current);
        }
    }
}
