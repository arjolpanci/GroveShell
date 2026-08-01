//! Input settings: the overview/move-resize trigger and per-corner hot
//! corner actions.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use groveshell_config::HotCornerConfig;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_segmented, segmented_hit, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

const MODIFIER_OPTIONS: [&str; 3] = ["Super", "Alt", "CtrlAlt"];
const CORNER_ACTION_OPTIONS: [&str; 2] = ["none", "activities"];
const CORNERS: [&str; 4] = ["top_left", "top_right", "bottom_left", "bottom_right"];

pub(crate) struct InputPage;

impl InputPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn modifier_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 32,
        }
    }

    // Each corner gets its own two-row slot: a label row followed by its
    // control row, mirroring the label-above-control pairing used by
    // `TopBarPage`/`OverviewPage`. The base offset advances by 2 rows per
    // corner (not 1) so no corner's control row coincides with the next
    // corner's label row.
    fn corner_label_rect(&self, content_rect: RECT, index: usize) -> RECT {
        let row = 2 + 2 * index as i32;
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * row,
            right: content_rect.right - PADDING,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * row + 24,
        }
    }

    fn corner_rect(&self, content_rect: RECT, index: usize) -> RECT {
        let row = 3 + 2 * index as i32;
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * row,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * row + 32,
        }
    }
}

impl Page for InputPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        let modifier_index = MODIFIER_OPTIONS
            .iter()
            .position(|m| *m == config.input.overview_modifier)
            .unwrap_or(0);

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                "Overview / move-resize trigger",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.modifier_rect(content_rect), &MODIFIER_OPTIONS, modifier_index);

            for (i, corner) in CORNERS.iter().enumerate() {
                let action = config.hot_corners.get(*corner).map(|c| c.action.clone()).unwrap_or_else(|| "none".to_string());
                let action_index = CORNER_ACTION_OPTIONS.iter().position(|a| *a == action).unwrap_or(0);
                draw_centered_text(hdc, self.corner_label_rect(content_rect, i), &corner_display_name(corner), TEXT_MUTED);
                draw_segmented(hdc, self.corner_rect(content_rect, i), &CORNER_ACTION_OPTIONS, action_index);
            }
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        if let Some(index) = segmented_hit(self.modifier_rect(content_rect), &MODIFIER_OPTIONS, x, y) {
            let modifier = MODIFIER_OPTIONS[index].to_string();
            config_store::update(|c| c.input.overview_modifier = modifier.clone());
            return;
        }
        for (i, corner) in CORNERS.iter().enumerate() {
            if let Some(index) = segmented_hit(self.corner_rect(content_rect, i), &CORNER_ACTION_OPTIONS, x, y) {
                let action = CORNER_ACTION_OPTIONS[index].to_string();
                let corner_name = corner.to_string();
                config_store::update(|c| {
                    let entry = c.hot_corners.entry(corner_name.clone()).or_insert_with(|| HotCornerConfig {
                        action: "none".to_string(),
                        delay_ms: 150,
                        disable_in_fullscreen: true,
                    });
                    entry.action = action.clone();
                });
                return;
            }
        }
    }
}

fn corner_display_name(corner: &str) -> String {
    match corner {
        "top_left" => "Top-left corner".to_string(),
        "top_right" => "Top-right corner".to_string(),
        "bottom_left" => "Bottom-left corner".to_string(),
        "bottom_right" => "Bottom-right corner".to_string(),
        other => other.to_string(),
    }
}
