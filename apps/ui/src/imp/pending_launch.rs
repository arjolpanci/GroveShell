//! Tracks "an app was just launched by dragging a dock icon onto a
//! workspace card, waiting to see its first window" so that window can
//! be assigned to the workspace it was dropped on once it appears —
//! bounded by a timeout so a launch that never produces a matching
//! window (a slow-starting app, or one that never opens a top-level
//! window at all) doesn't wait forever.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// A launch still waiting to be matched to its first window.
pub(crate) struct PendingLaunch {
    pub(crate) process_id: u32,
    pub(crate) monitor: String,
    pub(crate) workspace_index: usize,
    pub(crate) expires_at: Instant,
}

/// How long a pending launch is kept before being given up on.
pub(crate) const PENDING_LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Pure: drops every expired entry, then finds (and removes) the first
/// entry matching `pid`, returning its target `(monitor, workspace_index)`.
/// Expiry is checked against `now` (passed in, not read internally) so
/// this is testable without waiting on a real clock.
fn take_match_at(pending: &mut Vec<PendingLaunch>, pid: u32, now: Instant) -> Option<(String, usize)> {
    pending.retain(|p| p.expires_at > now);
    let i = pending.iter().position(|p| p.process_id == pid)?;
    let p = pending.remove(i);
    Some((p.monitor, p.workspace_index))
}

thread_local! {
    static PENDING: RefCell<Vec<PendingLaunch>> = const { RefCell::new(Vec::new()) };
}

/// Registers a newly-launched process to watch for. Called right after
/// a drag-to-open launch (see `dock.rs`'s drag-end handling).
pub(crate) fn register(pending: PendingLaunch) {
    PENDING.with(|p| p.borrow_mut().push(pending));
}

/// Checks the pending list for a launch matching `pid` — called for
/// every newly-seen window during the overview's existing debounced
/// window-sync pass (see `workspaces::sync_workspaces`). Removes and
/// returns the match, if any, and drops any expired entries in the same
/// pass.
pub(crate) fn take_match(pid: u32) -> Option<(String, usize)> {
    PENDING.with(|p| take_match_at(&mut p.borrow_mut(), pid, Instant::now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, monitor: &str, index: usize, expires_at: Instant) -> PendingLaunch {
        PendingLaunch { process_id: pid, monitor: monitor.to_string(), workspace_index: index, expires_at }
    }

    #[test]
    fn take_match_at_finds_and_removes_the_matching_entry() {
        let now = Instant::now();
        let mut pending = vec![entry(111, "MonitorA", 2, now + Duration::from_secs(10))];
        let result = take_match_at(&mut pending, 111, now);
        assert_eq!(result, Some(("MonitorA".to_string(), 2)));
        assert!(pending.is_empty());
    }

    #[test]
    fn take_match_at_returns_none_for_no_matching_pid() {
        let now = Instant::now();
        let mut pending = vec![entry(111, "MonitorA", 2, now + Duration::from_secs(10))];
        assert_eq!(take_match_at(&mut pending, 999, now), None);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn take_match_at_ignores_an_expired_entry() {
        let now = Instant::now();
        let past = now.checked_sub(Duration::from_secs(1)).unwrap();
        let mut pending = vec![entry(111, "MonitorA", 2, past)];
        assert_eq!(take_match_at(&mut pending, 111, now), None);
        assert!(pending.is_empty(), "expired entries are dropped even if not matched");
    }

    #[test]
    fn take_match_at_matches_the_first_of_several_by_pid() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(10);
        let mut pending = vec![entry(1, "A", 0, soon), entry(2, "B", 1, soon)];
        assert_eq!(take_match_at(&mut pending, 2, now), Some(("B".to_string(), 1)));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].process_id, 1);
    }
}
