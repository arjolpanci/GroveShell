use crate::Config;
use std::fs;
use std::path::Path;
use groveshell_common::Result;

/// Loads and validates a config file. Returns an error if the file is
/// missing, unparsable, or fails validation — callers that want a
/// fall-back-to-defaults behavior should use [`load_or_default`] instead.
pub fn load(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&text)?;
    config.validate()?;
    Ok(config)
}

/// Loads a config file, falling back to [`Config::default`] if the file is
/// missing, unparsable, or invalid. Never fails — this is what
/// long-running processes like `groveshell-host` should call at startup so a
/// corrupt or absent config never prevents the process from starting.
pub fn load_or_default(path: &Path) -> Config {
    match load(path) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(error = ?e, path = ?path, "failed to load config, using defaults");
            Config::default()
        }
    }
}

/// Validates `config`, then writes it durably: serialize to a temp file in
/// the same directory, fsync, atomically rename over the target path. If a
/// file already exists at `path`, it is copied to `<path>.bak` first so one
/// previous-known-good backup is always available.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    config.validate()?;
    let text = toml::to_string_pretty(config)?;

    if path.exists() {
        let backup_path = path.with_extension("toml.bak");
        fs::copy(path, &backup_path)?;
    }

    let tmp_path = path.with_extension("toml.tmp");
    fs::write(&tmp_path, &text)?;
    {
        let f = fs::File::open(&tmp_path)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}
