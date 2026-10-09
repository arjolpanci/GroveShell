//! Top Bar settings: height and blur.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row};

use super::Page;
use crate::imp::config_store;

const ROW_HEIGHT: u32 = 1;
const ROW_BLUR: u32 = 2;

pub(crate) struct TopBarPage;

impl TopBarPage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for TopBarPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();
        vec![Card::new(vec![
            Row::new(ROW_HEIGHT, "Height")
                .with_description("How tall the top bar is")
                .with_glyph(glyph::RESIZE)
                .with_control(Control::Slider {
                    value: config.appearance.top_bar_height as f32,
                    min: 24.0,
                    max: 48.0,
                    unit: "px",
                }),
            Row::new(ROW_BLUR, "Blur")
                .with_description("Let the desktop show through the bar")
                .with_glyph(glyph::BACKDROP)
                .with_control(Control::Toggle { on: config.appearance.top_bar_blur }),
        ])]
    }

    fn on_activate(&mut self, id: u32) {
        if id == ROW_BLUR {
            let current = config_store::current().appearance.top_bar_blur;
            config_store::update(|c| c.appearance.top_bar_blur = !current);
        }
    }

    fn on_value(&mut self, id: u32, value: f32) {
        if id == ROW_HEIGHT {
            config_store::update(|c| c.appearance.top_bar_height = value.round() as u32);
        }
    }
}
