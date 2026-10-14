//! Where the user's own taskbar state is remembered while the shell has
//! the screen.
//!
//! The shell switches the Windows taskbar to auto-hide so Explorer
//! releases its work-area reservation, and puts it back at shutdown. The
//! "put it back" half only works if the original state survives, and an
//! in-memory copy does not: kill the shell instead of shutting it down
//! and the taskbar is left auto-hiding. Worse, the next run would read
//! that auto-hide — which the shell itself set — and record it as the
//! user's own, destroying the real setting for good.
//!
//! So it lives in a file next to the config and logs. Both the shell and
//! `groveshell-cli restore-taskbar` read it, which is why the format
//! lives here rather than in either of them.

use std::path::{Path, PathBuf};

/// `<data_dir>/taskbar-state`.
pub fn path() -> crate::Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("taskbar-state"))
}

/// The remembered state, or `None` if nothing is saved or the file is
/// unreadable/corrupt — treated the same way, since both mean "we have
/// nothing trustworthy to restore".
pub fn read(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

pub fn write(path: &Path, state: u32) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, state.to_string()) {
        tracing::warn!(error = ?e, "could not persist the taskbar's pre-takeover state");
    }
}

pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// What to record as the user's own state, given what is already saved
/// and what the taskbar reports right now.
///
/// A saved value always wins: its presence means an earlier run took the
/// taskbar over and never gave it back, so the live state is the shell's
/// own auto-hide rather than anything the user chose.
pub fn state_to_remember(saved: Option<u32>, current: u32) -> u32 {
    saved.unwrap_or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-tbstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("taskbar-state")
    }

    #[test]
    fn the_first_takeover_remembers_what_the_taskbar_reports() {
        assert_eq!(state_to_remember(None, 2), 2);
        assert_eq!(state_to_remember(None, 0), 0);
    }

    /// The bug that made auto-hide sticky: after an unclean exit the
    /// taskbar is still auto-hiding because the shell put it there, and a
    /// run that trusted the live state recorded that as the user's.
    #[test]
    fn a_later_takeover_keeps_the_originally_saved_state() {
        assert_eq!(state_to_remember(Some(2), 1), 2);
        assert_eq!(state_to_remember(Some(0), 1), 0);
    }

    #[test]
    fn someone_who_really_uses_autohide_keeps_it() {
        assert_eq!(state_to_remember(Some(1), 1), 1);
    }

    #[test]
    fn the_state_round_trips_through_its_file() {
        let p = temp("roundtrip");
        assert_eq!(read(&p), None, "nothing saved yet");
        write(&p, 2);
        assert_eq!(read(&p), Some(2));
        clear(&p);
        assert_eq!(read(&p), None, "cleared once restored");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn a_corrupt_file_reads_as_nothing_saved() {
        let p = temp("corrupt");
        std::fs::write(&p, b"not a number").unwrap();
        assert_eq!(read(&p), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn clearing_a_file_that_is_not_there_is_harmless() {
        let p = temp("absent");
        clear(&p);
        clear(&p);
        assert_eq!(read(&p), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
