//! Home: whether GroveShell is running, the per-process detail behind
//! that, and whether it starts with Windows.
//!
//! This page still reports what *this* process spawned, which is why it
//! can claim GroveShell is unhealthy while the shell is plainly running.
//! Fixing that is the next plan's job (see the spec's §8); this one only
//! moves the existing information onto the new rows.

use groveshell_ui_kit::glyph;
use groveshell_ui_kit::rows::{Card, Control, Row, Severity};

use super::Page;
use crate::imp::autostart;
use crate::imp::health::{host_ping_ok, sample_process};
use crate::imp::tray::toggle_groveshell;

const ROW_STATUS: u32 = 1;
const ROW_TOGGLE_SHELL: u32 = 2;
const ROW_AUTOSTART: u32 = 3;
const ROW_PROCESS_BASE: u32 = 10;

const PROCESSES: [&str; 3] = ["watchdog", "host", "ui"];

pub(crate) struct HomePage;

impl HomePage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for HomePage {
    fn cards(&self) -> Vec<Card> {
        let running = crate::imp::tray::is_ui_running();
        let health = health_summary();

        let status = match (&health, running) {
            (Ok(()), _) => Row::new(ROW_STATUS, "GroveShell is running")
                .with_description(format!("version {}", env!("CARGO_PKG_VERSION")))
                .with_glyph(glyph::HOME)
                .with_control(Control::Status { severity: Severity::Ok }),
            (Err(reason), _) => Row::new(ROW_STATUS, "GroveShell isn't fully running")
                .with_description(reason.clone())
                .with_glyph(glyph::HOME)
                .with_control(Control::Status { severity: Severity::Warning }),
        };

        let action = Row::new(
            ROW_TOGGLE_SHELL,
            if running { "Restore Explorer" } else { "Start GroveShell" },
        )
        .with_description(if running {
            "Stop the shell and give the Windows taskbar its screen space back"
        } else {
            "Start the shell and take the screen over from the Windows taskbar"
        })
        .with_glyph(glyph::STARTUP)
        .with_control(Control::Action {
            label: if running { "Restore" } else { "Start" },
        });

        let autostart_row = Row::new(ROW_AUTOSTART, "Start with Windows")
            .with_description("Launch GroveShell when you sign in")
            .with_glyph(glyph::STARTUP)
            .with_control(Control::Toggle { on: autostart::is_enabled() });

        let processes = PROCESSES
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let (description, severity) = process_detail(name);
                Row::new(ROW_PROCESS_BASE + index as u32, *name)
                    .with_description(description)
                    .with_glyph(glyph::ABOUT)
                    .with_control(Control::Status { severity })
            })
            .collect();

        vec![
            Card::new(vec![status, action, autostart_row]),
            Card::with_caption("Processes", processes),
        ]
    }

    fn on_activate(&mut self, id: u32) {
        match id {
            ROW_TOGGLE_SHELL => toggle_groveshell(),
            ROW_AUTOSTART => {
                let next = !autostart::is_enabled();
                autostart::set_enabled(next);
                crate::imp::config_store::update(|config| {
                    config.general.start_with_windows = next;
                });
            }
            _ => {}
        }
    }
}

/// One process's line: pid and resource use when it's alive, so the
/// detail a developer wants is here rather than in the headline.
fn process_detail(name: &str) -> (String, Severity) {
    match crate::imp::tray::pid_for(name) {
        Some(pid) => match sample_process(pid) {
            Some(sample) => (
                format!(
                    "running - pid {pid}, {:.1}% CPU, {:.0} MB",
                    sample.cpu_percent,
                    sample.working_set_bytes as f64 / (1024.0 * 1024.0)
                ),
                Severity::Ok,
            ),
            None => (format!("running - pid {pid}"), Severity::Ok),
        },
        None => ("not running".to_string(), Severity::Warning),
    }
}

fn health_summary() -> Result<(), String> {
    for name in PROCESSES {
        if crate::imp::tray::pid_for(name).is_none() {
            return Err(format!("{name} is not running"));
        }
    }
    if !host_ping_ok(std::time::Duration::from_millis(500)) {
        return Err("host did not respond to ping".to_string());
    }
    Ok(())
}
