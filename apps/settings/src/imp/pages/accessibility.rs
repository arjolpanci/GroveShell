//! Accessibility and privacy settings.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row};

use super::Page;
use crate::imp::config_store;

const ROW_HIGH_CONTRAST: u32 = 1;
const ROW_REDACT_TITLES: u32 = 2;

pub(crate) struct AccessibilityPage;

impl AccessibilityPage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for AccessibilityPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();
        vec![
            Card::new(vec![Row::new(ROW_HIGH_CONTRAST, "High contrast")
                .with_description("Use the high-contrast palette everywhere in the shell")
                .with_glyph(glyph::ACCESSIBILITY)
                .with_control(Control::Toggle { on: config.appearance.high_contrast })]),
            Card::with_caption(
                "Privacy",
                vec![Row::new(ROW_REDACT_TITLES, "Hide window titles in logs")
                    .with_description("Keep the text of window titles out of GroveShell's log files")
                    .with_glyph(glyph::ABOUT)
                    .with_control(Control::Toggle { on: config.privacy.redact_window_titles })],
            ),
        ]
    }

    fn on_activate(&mut self, id: u32) {
        match id {
            ROW_HIGH_CONTRAST => {
                let current = config_store::current().appearance.high_contrast;
                config_store::update(|c| c.appearance.high_contrast = !current);
            }
            ROW_REDACT_TITLES => {
                let current = config_store::current().privacy.redact_window_titles;
                config_store::update(|c| c.privacy.redact_window_titles = !current);
            }
            _ => {}
        }
    }
}
