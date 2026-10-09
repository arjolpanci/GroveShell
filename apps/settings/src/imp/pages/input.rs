//! Input settings: the modifiers and buttons that drive window moves,
//! resizes and Activities, plus the hot corners.

use groveshell_config::HotCornerConfig;
use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row};

use super::{option_index, Page};
use crate::imp::config_store;

const ROW_OVERVIEW_MODIFIER: u32 = 1;
const ROW_MOVE_MODIFIER: u32 = 2;
const ROW_MOVE_BUTTON: u32 = 3;
const ROW_RESIZE_BUTTON: u32 = 4;
/// Hot-corner rows are `ROW_CORNER_BASE + index into CORNERS`.
const ROW_CORNER_BASE: u32 = 10;

const MODIFIER_OPTIONS: [&str; 3] = ["Super", "Alt", "CtrlAlt"];
const BUTTON_OPTIONS: [&str; 3] = ["Left", "Right", "Middle"];
const CORNER_ACTION_OPTIONS: [&str; 2] = ["none", "activities"];
const CORNERS: [&str; 4] = ["top_left", "top_right", "bottom_left", "bottom_right"];

/// The default a corner gets when the config has never mentioned it —
/// the same values `apps/ui` reads for an absent corner.
const DEFAULT_CORNER: HotCornerConfig = HotCornerConfig {
    action: String::new(),
    delay_ms: 150,
    disable_in_fullscreen: true,
};

pub(crate) struct InputPage;

impl InputPage {
    pub(crate) fn new() -> Self {
        Self
    }
}

fn corner_display_name(corner: &str) -> &'static str {
    match corner {
        "top_left" => "Top-left corner",
        "top_right" => "Top-right corner",
        "bottom_left" => "Bottom-left corner",
        "bottom_right" => "Bottom-right corner",
        _ => "Corner",
    }
}

impl Page for InputPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();

        let corners = CORNERS
            .iter()
            .enumerate()
            .map(|(index, corner)| {
                let action = config
                    .hot_corners
                    .get(*corner)
                    .map(|c| c.action.clone())
                    .unwrap_or_else(|| "none".to_string());
                Row::new(ROW_CORNER_BASE + index as u32, corner_display_name(corner))
                    .with_description("What happens when the pointer reaches this corner")
                    .with_glyph(glyph::OVERVIEW)
                    .with_control(Control::Choice {
                        options: &CORNER_ACTION_OPTIONS,
                        selected: option_index(&CORNER_ACTION_OPTIONS, &action, 0),
                    })
            })
            .collect();

        vec![
            Card::new(vec![
                Row::new(ROW_OVERVIEW_MODIFIER, "Activities modifier")
                    .with_description("Held with a tap to open the Activities overview")
                    .with_glyph(glyph::OVERVIEW)
                    .with_control(Control::Choice {
                        options: &MODIFIER_OPTIONS,
                        selected: option_index(&MODIFIER_OPTIONS, &config.input.overview_modifier, 0),
                    }),
                Row::new(ROW_MOVE_MODIFIER, "Move and resize modifier")
                    .with_description("Held to drag a window from anywhere inside it")
                    .with_glyph(glyph::INPUT)
                    .with_control(Control::Choice {
                        options: &MODIFIER_OPTIONS,
                        selected: option_index(&MODIFIER_OPTIONS, &config.input.move_modifier, 1),
                    }),
                Row::new(ROW_MOVE_BUTTON, "Move button")
                    .with_description("Pressed with the modifier to move a window")
                    .with_glyph(glyph::INPUT)
                    .with_control(Control::Choice {
                        options: &BUTTON_OPTIONS,
                        selected: option_index(&BUTTON_OPTIONS, &config.input.move_button, 0),
                    }),
                Row::new(ROW_RESIZE_BUTTON, "Resize button")
                    .with_description("Pressed with the modifier to resize a window")
                    .with_glyph(glyph::INPUT)
                    .with_control(Control::Choice {
                        options: &BUTTON_OPTIONS,
                        selected: option_index(&BUTTON_OPTIONS, &config.input.resize_button, 1),
                    }),
            ]),
            Card::with_caption("Hot corners", corners),
        ]
    }

    fn on_activate(&mut self, _id: u32) {}

    fn on_value(&mut self, id: u32, value: f32) {
        let index = value.max(0.0) as usize;
        match id {
            ROW_OVERVIEW_MODIFIER => {
                let choice = MODIFIER_OPTIONS.get(index).copied().unwrap_or("Super").to_string();
                config_store::update(|c| c.input.overview_modifier = choice.clone());
            }
            ROW_MOVE_MODIFIER => {
                let choice = MODIFIER_OPTIONS.get(index).copied().unwrap_or("Alt").to_string();
                config_store::update(|c| c.input.move_modifier = choice.clone());
            }
            ROW_MOVE_BUTTON => {
                let choice = BUTTON_OPTIONS.get(index).copied().unwrap_or("Left").to_string();
                config_store::update(|c| c.input.move_button = choice.clone());
            }
            ROW_RESIZE_BUTTON => {
                let choice = BUTTON_OPTIONS.get(index).copied().unwrap_or("Right").to_string();
                config_store::update(|c| c.input.resize_button = choice.clone());
            }
            _ => {
                let Some(corner) = id
                    .checked_sub(ROW_CORNER_BASE)
                    .and_then(|i| CORNERS.get(i as usize))
                    .map(|c| c.to_string())
                else {
                    return;
                };
                let action = CORNER_ACTION_OPTIONS.get(index).copied().unwrap_or("none").to_string();
                config_store::update(|c| {
                    let entry = c
                        .hot_corners
                        .entry(corner.clone())
                        .or_insert_with(|| HotCornerConfig { action: "none".to_string(), ..DEFAULT_CORNER });
                    entry.action = action.clone();
                });
            }
        }
    }
}
