//! The in-memory `Config` this process edits, plus the single choke point
//! (`update`) every settings control uses to persist a change: save to
//! disk (existing atomic-write/backup behavior in
//! `groveshell_config::save`, unchanged) and push `config.reload` to
//! `apps/ui` so it takes effect live (see Task 13).

use std::cell::RefCell;

use groveshell_config::Config;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

fn config_path() -> std::path::PathBuf {
    groveshell_common::paths::data_dir()
        .expect("data_dir should always resolve")
        .join("config.toml")
}

pub(crate) fn init() {
    let config = groveshell_config::load_or_default(&config_path());
    CONFIG.with(|c| *c.borrow_mut() = config);
}

pub(crate) fn current() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Applies `f` to a clone of the current config, saves it, and — if it
/// saved successfully — pushes `config.reload` to `apps/ui` (best-effort:
/// `apps/ui` might not be running, e.g. right after "Restore Explorer";
/// that's not an error, the setting still applies next time `ui` starts
/// and reads `config.toml` itself) and updates the in-memory copy.
pub(crate) fn update(f: impl FnOnce(&mut Config)) {
    let mut next = current();
    f(&mut next);
    match groveshell_config::save(&config_path(), &next) {
        Ok(()) => {
            CONFIG.with(|c| *c.borrow_mut() = next);
            push_reload();
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to save config");
        }
    }
}

fn push_reload() {
    let Ok(mut conn) = groveshell_ipc::pipe::connect("groveshell-ui") else {
        tracing::debug!("groveshell-ui not reachable; config.reload not pushed (will apply on its next start)");
        return;
    };
    let envelope = groveshell_ipc::Envelope::new(
        "groveshell-settings",
        groveshell_ipc::message_type::CONFIG_RELOAD,
        serde_json::json!({}),
    );
    if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &envelope) {
        tracing::warn!(error = ?e, "failed to push config.reload");
    }
    // Fire-and-forget: `apps/ui` doesn't send a response to `config.reload`,
    // so there is nothing to read back here.
}
