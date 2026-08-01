//! `groveshell-settings`: the tray-resident launcher/settings app. Spawns
//! and supervises `groveshell-watchdog`, `groveshell-host`, and
//! `groveshell-ui`; shows their health; offers a one-click Explorer-restore
//! toggle; hosts the dock/top-bar/overview/input settings window. See
//! `docs/superpowers/specs/2026-07-30-tray-settings-app-design.md`.

#[cfg(windows)]
mod imp;

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-settings is Windows-only.");
    std::process::exit(1);
}
