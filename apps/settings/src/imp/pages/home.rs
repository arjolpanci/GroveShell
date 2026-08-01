//! Home/Status page: overall health, per-process CPU/RAM, the
//! Restore-Explorer/Start-GroveShell button, and Start-with-Windows.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::autostart;
use crate::imp::health::{host_ping_ok, sample_process};
use crate::imp::theme::{draw_toggle, hit_toggle, ACCENT, TEXT, TEXT_MUTED};
use crate::imp::tray::toggle_groveshell;
use crate::imp::util_text::draw_centered_text;

const ROW_HEIGHT: i32 = 32;
const PADDING: i32 = 24;
const BUTTON_HEIGHT: i32 = 36;
const BUTTON_WIDTH: i32 = 220;
const TOGGLE_WIDTH: i32 = 44;
const TOGGLE_HEIGHT: i32 = 24;

pub(crate) struct HomePage;

impl HomePage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn restore_button_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + BUTTON_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + BUTTON_HEIGHT,
        }
    }

    fn autostart_toggle_rect(&self, content_rect: RECT) -> RECT {
        let button = self.restore_button_rect(content_rect);
        RECT {
            left: content_rect.left + PADDING,
            top: button.bottom + PADDING,
            right: content_rect.left + PADDING + TOGGLE_WIDTH,
            bottom: button.bottom + PADDING + TOGGLE_HEIGHT,
        }
    }
}

impl Page for HomePage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let healthy = health_summary();
        let (status_text, status_color) = match &healthy {
            Ok(()) => ("Healthy".to_string(), COLORREF(0x0060C060)),
            Err(reason) => (format!("Unhealthy: {reason}"), COLORREF(0x004040FF)),
        };

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT },
                &status_text,
                status_color,
            );

            for (i, name) in ["watchdog", "host", "ui"].iter().enumerate() {
                let row = RECT {
                    left: content_rect.left + PADDING,
                    top: content_rect.top + PADDING + ROW_HEIGHT * (i as i32 + 1),
                    right: content_rect.right - PADDING,
                    bottom: content_rect.top + PADDING + ROW_HEIGHT * (i as i32 + 2),
                };
                let line = process_status_line(name);
                draw_centered_text(hdc, row, &line, TEXT);
            }

            let button = self.restore_button_rect(content_rect);
            let running = crate::imp::tray::is_ui_running();
            let label = if running { "Restore Explorer" } else { "Start GroveShell" };
            super::super::theme::fill_round_rect(hdc, button, 8, ACCENT);
            draw_centered_text(hdc, button, label, COLORREF(0x00202020));

            let toggle = self.autostart_toggle_rect(content_rect);
            draw_toggle(hdc, toggle, autostart::is_enabled());
            draw_centered_text(
                hdc,
                RECT { left: toggle.right + 12, top: toggle.top, right: toggle.right + 260, bottom: toggle.bottom },
                "Start with Windows",
                TEXT_MUTED,
            );
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let button = self.restore_button_rect(content_rect);
        if x >= button.left && x < button.right && y >= button.top && y < button.bottom {
            toggle_groveshell();
            return;
        }
        let toggle = self.autostart_toggle_rect(content_rect);
        if hit_toggle(toggle, x, y) {
            let new_state = !autostart::is_enabled();
            autostart::set_enabled(new_state);
            crate::imp::config_store::update(|config| {
                config.general.start_with_windows = new_state;
            });
        }
    }
}

fn process_status_line(name: &str) -> String {
    // Best-effort: pid lookup for display purposes only reads whatever
    // `ManagedProcesses` tracked; a fuller implementation would expose pid
    // accessors, but for this page's read-only display, a "known name,
    // sampled if alive" line is enough context for a health screen.
    match crate::imp::tray::pid_for(name) {
        Some(pid) => match sample_process(pid) {
            Some(sample) => format!(
                "{name}: running (pid {pid}, {:.1}% CPU, {:.0} MB)",
                sample.cpu_percent,
                sample.working_set_bytes as f64 / (1024.0 * 1024.0)
            ),
            None => format!("{name}: running (pid {pid})"),
        },
        None => format!("{name}: not running"),
    }
}

fn health_summary() -> Result<(), String> {
    for name in ["watchdog", "host", "ui"] {
        if crate::imp::tray::pid_for(name).is_none() {
            return Err(format!("{name} is not running"));
        }
    }
    if !host_ping_ok(std::time::Duration::from_millis(500)) {
        return Err("host did not respond to ping".to_string());
    }
    Ok(())
}
