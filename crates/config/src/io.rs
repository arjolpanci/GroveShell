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
    {
        use std::io::Write;
        // Write and fsync through the *same write handle*. Writing with
        // `fs::write` and then reopening via `fs::File::open` (read-only)
        // to `sync_all` fails on Windows with ERROR_ACCESS_DENIED: there,
        // `sync_all` calls `FlushFileBuffers`, which requires write access
        // on the handle, so flushing a read-only handle is rejected. That
        // made every `save` abort at the flush — before the rename — which
        // is why settings changes silently never persisted and a stray
        // `.tmp` was left behind with no `config.toml` written.
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}
