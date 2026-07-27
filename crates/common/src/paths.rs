use crate::{Error, Result};
use std::path::PathBuf;

/// Per-user data directory: `%LOCALAPPDATA%\GroveShell` on Windows, and the
/// platform-appropriate equivalent elsewhere (used only so this crate's
/// tests can run on non-Windows hosts).
pub fn data_dir() -> Result<PathBuf> {
    // Deliberately not `directories::ProjectDirs`, whose
    // qualifier/organization/application scheme would resolve to
    // `%LOCALAPPDATA%\groveshell\GroveShell\data` on Windows. We want the
    // simple, documented `%LOCALAPPDATA%\GroveShell` path instead, so build
    // it directly from `BaseDirs`.
    let base = directories::BaseDirs::new()
        .ok_or_else(|| Error::InvalidConfig("could not determine data directory".into()))?;
    Ok(base.data_local_dir().join("GroveShell"))
}

/// Directory rotating log files are written into: `<data_dir>/logs`.
pub fn log_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_non_empty_and_ends_in_groveshell() {
        let dir = data_dir().expect("data_dir should resolve on any supported OS");
        assert!(!dir.as_os_str().is_empty());
        assert!(dir.to_string_lossy().to_lowercase().contains("groveshell"));
    }

    #[test]
    fn log_dir_is_data_dir_plus_logs() {
        let data = data_dir().unwrap();
        let logs = log_dir().unwrap();
        assert_eq!(logs, data.join("logs"));
    }
}
