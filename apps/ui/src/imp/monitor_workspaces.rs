use std::collections::HashMap;

use groveshell_window_model::workspace::WorkspaceTracker;

/// One independent `WorkspaceTracker` per currently-connected monitor,
/// keyed by that monitor's stable device name (see
/// `monitors::MonitorInfo::device_name`). Each tracker is fully
/// self-contained: its own pinned workspace, its own dynamic tail,
/// unaffected by any other monitor's switching/growth/shrinkage — see
/// `docs/superpowers/specs/2026-07-28-per-monitor-workspaces-design.md`
/// §B for why the crate's `WorkspaceTracker` itself needed no changes.
#[derive(Default)]
pub(crate) struct MonitorWorkspaces {
    trackers: HashMap<String, WorkspaceTracker>,
}

impl MonitorWorkspaces {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert_monitor(&mut self, device_name: String, tracker: WorkspaceTracker) {
        self.trackers.insert(device_name, tracker);
    }

    pub(crate) fn remove_monitor(&mut self, device_name: &str) -> Option<WorkspaceTracker> {
        self.trackers.remove(device_name)
    }

    pub(crate) fn get(&self, device_name: &str) -> Option<&WorkspaceTracker> {
        self.trackers.get(device_name)
    }

    pub(crate) fn get_mut(&mut self, device_name: &str) -> Option<&mut WorkspaceTracker> {
        self.trackers.get_mut(device_name)
    }

    /// Which monitor currently has `hwnd` assigned to one of its
    /// workspaces, if any — scans every tracker since a window only
    /// ever lives in exactly one.
    pub(crate) fn monitor_of_window(&self, hwnd: isize) -> Option<String> {
        self.trackers
            .iter()
            .find(|(_, t)| t.workspace_of(hwnd).is_some())
            .map(|(name, _)| name.clone())
    }

    pub(crate) fn device_names(&self) -> impl Iterator<Item = &str> {
        self.trackers.keys().map(String::as_str)
    }

    /// Every window tracked by any monitor's tracker — used at shutdown
    /// to unpark everything, mirroring what `mod.rs`'s `WM_DESTROY`
    /// handler did against the single global tracker before this
    /// change.
    pub(crate) fn all_tracked_windows(&self) -> Vec<isize> {
        self.trackers
            .values()
            .flat_map(|t| t.workspace_ids().to_vec().into_iter().flat_map(|id| t.windows_on(id)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groveshell_window_model::workspace::WorkspaceTracker;

    #[test]
    fn each_monitor_gets_an_independent_tracker() {
        let mut mw = MonitorWorkspaces::new();
        mw.insert_monitor("A".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.insert_monitor("B".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));

        mw.get_mut("A").unwrap().switch_to_index(1);
        assert_eq!(mw.get("A").unwrap().current_index(), 1);
        assert_eq!(mw.get("B").unwrap().current_index(), 0, "monitor B must be unaffected by A's switch");
    }

    #[test]
    fn monitor_of_window_scans_every_tracker() {
        let mut mw = MonitorWorkspaces::new();
        mw.insert_monitor("A".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.insert_monitor("B".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.get_mut("B").unwrap().assign_to_index(777, 0);

        assert_eq!(mw.monitor_of_window(777), Some("B".to_string()));
        assert_eq!(mw.monitor_of_window(999), None);
    }

    #[test]
    fn remove_monitor_returns_its_tracker_for_reassignment() {
        let mut mw = MonitorWorkspaces::new();
        mw.insert_monitor("A".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        let removed = mw.remove_monitor("A").unwrap();
        assert_eq!(removed.workspace_ids().len(), 2); // 1 pinned + 1 dynamic tail
        assert!(mw.get("A").is_none());
    }

    #[test]
    fn all_tracked_windows_covers_every_monitor() {
        let mut mw = MonitorWorkspaces::new();
        mw.insert_monitor("A".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.insert_monitor("B".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.get_mut("A").unwrap().assign_to_index(1, 0);
        mw.get_mut("B").unwrap().assign_to_index(2, 0);
        let mut all = mw.all_tracked_windows();
        all.sort();
        assert_eq!(all, vec![1, 2]);
    }

    #[test]
    fn reassigning_a_window_to_a_new_monitor_removes_it_from_the_old_one() {
        let mut mw = MonitorWorkspaces::new();
        mw.insert_monitor("A".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.insert_monitor("B".into(), WorkspaceTracker::with_monitor_workspaces(1, 0));
        mw.get_mut("A").unwrap().assign_to_index(42, 0);
        assert_eq!(mw.monitor_of_window(42), Some("A".to_string()));

        // Simulate what `sync_workspaces` will do: forget it on its old
        // monitor, assign it on the new one.
        mw.get_mut("A").unwrap().forget(42);
        mw.get_mut("B").unwrap().assign_to_index(42, 0);

        assert_eq!(mw.monitor_of_window(42), Some("B".to_string()));
    }
}
