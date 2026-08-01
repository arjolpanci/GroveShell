//! Dock settings: horizontal alignment, icon size, and visibility mode.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_segmented, draw_slider, segmented_hit, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

const ALIGNMENT_OPTIONS: [&str; 3] = ["left", "center", "right"];
const MODE_OPTIONS: [&str; 3] = ["overview", "always", "autohide"];

pub(crate) struct DockPage;

impl DockPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn alignment_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 32,
        }
    }

    fn icon_size_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }

    fn mode_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + 32,
        }
    }
}

impl Page for DockPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        let alignment_index = ALIGNMENT_OPTIONS
            .iter()
            .position(|a| *a == config.appearance.dock_alignment)
            .unwrap_or(1);
        let mode_index = MODE_OPTIONS
            .iter()
            .position(|m| *m == config.appearance.dock_mode)
            .unwrap_or(0);

        // SAFETY: `hdc` is a valid device context supplied by the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                "Alignment",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.alignment_rect(content_rect), &ALIGNMENT_OPTIONS, alignment_index);

            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 2, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 2 + 24 },
                &format!("Icon size: {}px", config.appearance.dock_icon_size),
                TEXT_MUTED,
            );
            draw_slider(hdc, self.icon_size_rect(content_rect), config.appearance.dock_icon_size as f32, 32.0, 64.0);

            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 4, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 4 + 24 },
                "Mode",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.mode_rect(content_rect), &MODE_OPTIONS, mode_index);
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        if let Some(index) = segmented_hit(self.alignment_rect(content_rect), &ALIGNMENT_OPTIONS, x, y) {
            let alignment = ALIGNMENT_OPTIONS[index].to_string();
            config_store::update(|c| c.appearance.dock_alignment = alignment.clone());
            return;
        }
        let slider_rect = self.icon_size_rect(content_rect);
        if y >= slider_rect.top - 8 && y < slider_rect.bottom + 8 && x >= slider_rect.left && x < slider_rect.right {
            let value = value_from_slider_x(slider_rect, x, 32.0, 64.0).round() as u32;
            config_store::update(|c| c.appearance.dock_icon_size = value);
            return;
        }
        if let Some(index) = segmented_hit(self.mode_rect(content_rect), &MODE_OPTIONS, x, y) {
            let mode = MODE_OPTIONS[index].to_string();
            config_store::update(|c| c.appearance.dock_mode = mode.clone());
        }
    }
}
