//! Stable window identity across HWND reuse, per `docs/PROJECT_PLAN.md`
//! §6: an HWND is just a recycled handle value, so a window that closes
//! can be followed by an unrelated window with the same handle. Keying
//! long-lived state (workspace assignments, snapshots) on the raw handle
//! alone would let the newcomer silently inherit the dead window's state.
//!
//! This registry hands out a [`WindowId`] per *observed window*, not per
//! handle: the same `(hwnd, pid)` pair keeps its id for as long as it's
//! alive, and a known hwnd showing up with a different owning process is
//! detected as reuse and gets a fresh id. Pid is the discriminator because
//! it's cheap, already collected by every snapshot, and a same-process
//! same-handle recycle within one observation interval is rare enough to
//! be out of scope for now (noted in the module docs of `lib.rs`).
//!
//! Pure bookkeeping, no Windows calls — callers feed it observations.

use std::collections::BTreeMap;

/// Process-unique, never-reused identity for one observed window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

#[derive(Debug, Clone, Copy)]
struct Entry {
    id: WindowId,
    pid: u32,
}

#[derive(Debug, Default)]
pub struct WindowRegistry {
    entries: BTreeMap<isize, Entry>,
    next: u64,
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `hwnd` currently belongs to `pid` and returns its
    /// identity, plus whether this observation revealed the handle was
    /// *reused* (same hwnd, different process) since last seen — in which
    /// case the returned id is fresh and any state keyed to this hwnd
    /// belongs to the previous window and should be dropped by the caller.
    pub fn observe(&mut self, hwnd: isize, pid: u32) -> (WindowId, bool) {
        match self.entries.get(&hwnd) {
            Some(entry) if entry.pid == pid => (entry.id, false),
            previous => {
                let reused = previous.is_some();
                let id = WindowId(self.next);
                self.next += 1;
                self.entries.insert(hwnd, Entry { id, pid });
                (id, reused)
            }
        }
    }

    pub fn id_of(&self, hwnd: isize) -> Option<WindowId> {
        self.entries.get(&hwnd).map(|e| e.id)
    }

    /// Drops entries for handles that no longer refer to live windows,
    /// mirroring `WorkspaceTracker::prune`.
    pub fn prune<F: Fn(isize) -> bool>(&mut self, is_alive: F) {
        self.entries.retain(|&hwnd, _| is_alive(hwnd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_window_keeps_its_id() {
        let mut reg = WindowRegistry::new();
        let (a, reused_a) = reg.observe(100, 42);
        let (b, reused_b) = reg.observe(100, 42);
        assert_eq!(a, b);
        assert!(!reused_a);
        assert!(!reused_b);
    }

    #[test]
    fn different_windows_get_different_ids() {
        let mut reg = WindowRegistry::new();
        let (a, _) = reg.observe(100, 42);
        let (b, _) = reg.observe(200, 42);
        assert_ne!(a, b);
    }

    #[test]
    fn hwnd_reuse_by_another_process_is_detected_with_a_fresh_id() {
        let mut reg = WindowRegistry::new();
        let (old, _) = reg.observe(100, 42);
        let (new, reused) = reg.observe(100, 43);
        assert!(reused);
        assert_ne!(old, new);
        assert_eq!(reg.id_of(100), Some(new));
    }

    #[test]
    fn no_window_id_is_ever_handed_out_twice() {
        let mut reg = WindowRegistry::new();
        let (a, _) = reg.observe(100, 1);
        let (b, _) = reg.observe(100, 2); // reuse
        reg.prune(|_| false); // everything dies
        let (c, _) = reg.observe(100, 3); // same handle again, later
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn prune_drops_dead_handles_but_preserves_live_ones() {
        let mut reg = WindowRegistry::new();
        let (a, _) = reg.observe(100, 1);
        reg.observe(200, 2);
        reg.prune(|hwnd| hwnd == 100);
        assert_eq!(reg.id_of(100), Some(a));
        assert_eq!(reg.id_of(200), None);
    }
}
