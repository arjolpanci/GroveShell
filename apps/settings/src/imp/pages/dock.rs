//! Dock settings: alignment, icon size, and when the dock appears.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row};

use super::{option_index, Page};
use crate::imp::config_store;

const ROW_ALIGNMENT: u32 = 1;
const ROW_ICON_SIZE: u32 = 2;
const ROW_MODE: u32 = 3;

const ALIGNMENT_OPTIONS: [&str; 3] = ["left", "center", "right"];
const MODE_OPTIONS: [&str; 3] = ["overview", "always", "autohide"];

pub(crate) struct DockPage;

impl DockPage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for DockPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();
        vec![Card::new(vec![
            Row::new(ROW_MODE, "Show the dock")
                .with_description("On the desktop, only in Activities, or hidden until you reach for it")
                .with_glyph(glyph::DOCK)
                .with_control(Control::Choice {
                    options: &MODE_OPTIONS,
                    selected: option_index(&MODE_OPTIONS, &config.appearance.dock_mode, 0),
                }),
            Row::new(ROW_ALIGNMENT, "Alignment")
                .with_description("Where the dock sits along the screen edge")
                .with_glyph(glyph::DOCK)
                .with_control(Control::Choice {
                    options: &ALIGNMENT_OPTIONS,
                    selected: option_index(&ALIGNMENT_OPTIONS, &config.appearance.dock_alignment, 1),
                }),
            Row::new(ROW_ICON_SIZE, "Icon size")
                .with_description("How large the dock's icons are")
                .with_glyph(glyph::RESIZE)
                .with_control(Control::Slider {
                    value: config.appearance.dock_icon_size as f32,
                    min: 32.0,
                    max: 64.0,
                    unit: "px",
                }),
        ])]
    }

    fn on_activate(&mut self, _id: u32) {}

    fn on_value(&mut self, id: u32, value: f32) {
        match id {
            ROW_ICON_SIZE => {
                config_store::update(|c| c.appearance.dock_icon_size = value.round() as u32);
            }
            ROW_ALIGNMENT => {
                let choice = ALIGNMENT_OPTIONS
                    .get(value as usize)
                    .copied()
                    .unwrap_or("center")
                    .to_string();
                config_store::update(|c| c.appearance.dock_alignment = choice.clone());
            }
            ROW_MODE => {
                let choice = MODE_OPTIONS.get(value as usize).copied().unwrap_or("overview").to_string();
                config_store::update(|c| c.appearance.dock_mode = choice.clone());
            }
            _ => {}
        }
    }
}
