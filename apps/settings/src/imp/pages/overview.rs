//! Activities settings: the overview's backdrop and its motion.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row};

use super::Page;
use crate::imp::config_store;

const ROW_BLUR: u32 = 1;
const ROW_REDUCED_MOTION: u32 = 2;
const ROW_SPEED: u32 = 3;

pub(crate) struct OverviewPage;

impl OverviewPage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for OverviewPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();
        vec![
            Card::new(vec![Row::new(ROW_BLUR, "Blur the background")
                .with_description("Blur the desktop behind the Activities overview")
                .with_glyph(glyph::BACKDROP)
                .with_control(Control::Toggle { on: config.appearance.overview_blur })]),
            Card::with_caption(
                "Motion",
                vec![
                    Row::new(ROW_REDUCED_MOTION, "Reduced motion")
                        .with_description("Resolve every transition instantly instead of animating it")
                        .with_glyph(glyph::ACCESSIBILITY)
                        .with_control(Control::Toggle { on: config.appearance.reduced_motion }),
                    Row::new(ROW_SPEED, "Animation speed")
                        .with_description("How fast transitions play, as a multiple of their normal duration")
                        .with_glyph(glyph::OVERVIEW)
                        .with_control(Control::Slider {
                            value: config.appearance.animation_scale,
                            min: 0.5,
                            max: 2.0,
                            unit: "x",
                        }),
                ],
            ),
        ]
    }

    fn on_activate(&mut self, id: u32) {
        match id {
            ROW_BLUR => {
                let current = config_store::current().appearance.overview_blur;
                config_store::update(|c| c.appearance.overview_blur = !current);
            }
            ROW_REDUCED_MOTION => {
                let current = config_store::current().appearance.reduced_motion;
                config_store::update(|c| c.appearance.reduced_motion = !current);
            }
            _ => {}
        }
    }

    fn on_value(&mut self, id: u32, value: f32) {
        if id == ROW_SPEED {
            // One decimal: the slider is continuous but the setting reads
            // as "1.3x", and a stored 1.2999999 would print as that.
            let rounded = (value * 10.0).round() / 10.0;
            config_store::update(|c| c.appearance.animation_scale = rounded);
        }
    }
}
