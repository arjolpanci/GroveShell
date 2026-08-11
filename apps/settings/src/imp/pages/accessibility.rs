//! Accessibility & privacy settings: high-contrast palette and whether
//! window titles are redacted from diagnostics bundles. Phase 6
//! (PROJECT_PLAN §16).

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_toggle, hit_toggle, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;

pub(crate) struct AccessibilityPage;

impl AccessibilityPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn high_contrast_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 24,
        }
    }

    fn redact_titles_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }
}

impl Page for AccessibilityPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            let hc = self.high_contrast_toggle_rect(content_rect);
            draw_toggle(hdc, hc, config.appearance.high_contrast);
            draw_centered_text(
                hdc,
                RECT { left: hc.right + 12, top: hc.top, right: hc.right + 320, bottom: hc.bottom },
                "High contrast (black / white / yellow shell)",
                TEXT_MUTED,
            );

            let redact = self.redact_titles_toggle_rect(content_rect);
            draw_toggle(hdc, redact, config.privacy.redact_window_titles);
            draw_centered_text(
                hdc,
                RECT { left: redact.right + 12, top: redact.top, right: redact.right + 340, bottom: redact.bottom },
                "Redact window titles from diagnostics bundles",
                TEXT_MUTED,
            );
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let hc = self.high_contrast_toggle_rect(content_rect);
        if hit_toggle(hc, x, y) {
            let current = config_store::current().appearance.high_contrast;
            config_store::update(|c| c.appearance.high_contrast = !current);
            return;
        }
        let redact = self.redact_titles_toggle_rect(content_rect);
        if hit_toggle(redact, x, y) {
            let current = config_store::current().privacy.redact_window_titles;
            config_store::update(|c| c.privacy.redact_window_titles = !current);
        }
    }
}
