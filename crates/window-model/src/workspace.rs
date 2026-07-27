//! Workspace domain model: window→workspace assignment plus the "dynamic
//! workspace" policy from `docs/PROJECT_PLAN.md` §8.3 and ADR-005's
//! `ManagedWorkspaceBackend`. Deliberately platform-independent — actually
//! parking windows on or off the real desktop (`ShowWindow`) and driving
//! the Activities overview carousel are `groveshell-ui`'s job; this module
//! only tracks *which* window belongs to *which* workspace, so it can be
//! exercised with plain unit tests instead of a live Windows session.
//!
//! Per the current MVP scope, all monitors share one set of workspaces
//! (§8.3: "Define whether workspaces are global or per-monitor; start
//! global for simplicity") — per-monitor workspace sets are a later,
//! explicitly out-of-scope enhancement.
//!
//! Dynamic workspaces only ever grow or shrink at the *tail*: a new empty
//! workspace appears once the last one is occupied, and redundant trailing
//! empties collapse back down to one. A workspace that becomes empty in
//! the middle of the list is left alone rather than auto-collapsed —
//! removing it would silently renumber every workspace after it, which is
//! more disorienting than a stray empty workspace is useful.

use std::collections::BTreeMap;

/// Never fewer than this many workspaces exist, even when all are empty —
/// GNOME-style "you always start with (at least) two."
pub const MIN_WORKSPACES: usize = 2;

/// Session-local workspace identity. Not a UUID (unlike the persisted
/// `WorkspaceId` sketched in `docs/PROJECT_PLAN.md` §6) because nothing
/// here is persisted across restarts yet — dynamic workspaces are
/// reconstructed fresh each session, matching Phase 3's recommended MVP
/// policy of only persisting meaningful named/pinned workspaces (none
/// exist yet).
pub type WorkspaceId = u32;

/// Ordered workspace list plus window→workspace assignments, with the
/// dynamic-workspace policy applied after every mutation.
#[derive(Debug, Clone)]
pub struct WorkspaceTracker {
    /// Ordered ids; position in this list is the workspace's carousel/index
    /// position.
    ids: Vec<WorkspaceId>,
    next_id: WorkspaceId,
    current: usize,
    /// hwnd -> workspace id. A window with no entry hasn't been observed
    /// yet (see [`Self::observe_active`]).
    assignments: BTreeMap<isize, WorkspaceId>,
}

impl Default for WorkspaceTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceTracker {
    pub fn new() -> Self {
        let ids: Vec<WorkspaceId> = (0..MIN_WORKSPACES as WorkspaceId).collect();
        Self {
            next_id: ids.len() as WorkspaceId,
            ids,
            current: 0,
            assignments: BTreeMap::new(),
        }
    }

    pub fn workspace_ids(&self) -> &[WorkspaceId] {
        &self.ids
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current_id(&self) -> WorkspaceId {
        self.ids[self.current]
    }

    pub fn index_of(&self, id: WorkspaceId) -> Option<usize> {
        self.ids.iter().position(|&x| x == id)
    }

    pub fn workspace_of(&self, hwnd: isize) -> Option<WorkspaceId> {
        self.assignments.get(&hwnd).copied()
    }

    /// Windows currently assigned to `id`. Order is by hwnd value, which is
    /// arbitrary but stable within a session — good enough for laying out
    /// an overview page.
    pub fn windows_on(&self, id: WorkspaceId) -> Vec<isize> {
        self.assignments
            .iter()
            .filter(|(_, &w)| w == id)
            .map(|(&hwnd, _)| hwnd)
            .collect()
    }

    fn is_occupied(&self, id: WorkspaceId) -> bool {
        self.assignments.values().any(|&w| w == id)
    }

    /// Assigns any of `hwnds` not yet tracked to the *current* workspace.
    /// Callers should only pass currently live/visible windows here (e.g.
    /// from `window_model::snapshot()`) — by construction those are always
    /// on the current workspace, since inactive-workspace windows are
    /// hidden — so a brand-new window lands wherever the user is actually
    /// looking, per Phase 3's "assign new windows to the current
    /// workspace."
    pub fn observe_active<I: IntoIterator<Item = isize>>(&mut self, hwnds: I) {
        let current = self.current_id();
        for hwnd in hwnds {
            self.assignments.entry(hwnd).or_insert(current);
        }
        self.compact();
    }

    /// Drops assignments for windows no longer alive (per the supplied
    /// predicate, typically `window_model::is_alive`), then re-applies the
    /// dynamic-workspace policy.
    pub fn prune<F: Fn(isize) -> bool>(&mut self, is_alive: F) {
        self.assignments.retain(|&hwnd, _| is_alive(hwnd));
        self.compact();
    }

    /// Switches to the workspace at `index` (clamped to the valid range).
    /// Returns `(from_id, to_id)`, or `None` if that's already current.
    pub fn switch_to_index(&mut self, index: usize) -> Option<(WorkspaceId, WorkspaceId)> {
        let index = index.min(self.ids.len() - 1);
        if index == self.current {
            return None;
        }
        let from = self.current_id();
        self.current = index;
        let to = self.current_id();
        self.compact();
        Some((from, to))
    }

    pub fn switch_relative(&mut self, delta: i32) -> Option<(WorkspaceId, WorkspaceId)> {
        self.switch_to_index(self.clamped_relative_index(delta))
    }

    /// `self.current_index()` offset by `delta`, clamped to the valid
    /// range (no wraparound — GNOME doesn't wrap either).
    pub fn clamped_relative_index(&self, delta: i32) -> usize {
        (self.current as i32 + delta).clamp(0, self.ids.len() as i32 - 1) as usize
    }

    /// Reassigns `hwnd` to the workspace at `index` (clamped). `None` if
    /// `hwnd` isn't currently tracked.
    pub fn move_window_to_index(&mut self, hwnd: isize, index: usize) -> Option<WorkspaceId> {
        if !self.assignments.contains_key(&hwnd) {
            return None;
        }
        let index = index.min(self.ids.len() - 1);
        let id = self.ids[index];
        self.assignments.insert(hwnd, id);
        self.compact();
        Some(id)
    }

    /// Moves `hwnd` to the workspace `delta` positions from wherever it
    /// currently is (clamped, no wraparound). `None` if `hwnd` isn't
    /// tracked.
    pub fn move_window_relative(&mut self, hwnd: isize, delta: i32) -> Option<WorkspaceId> {
        let current_idx = self.workspace_of(hwnd).and_then(|id| self.index_of(id))?;
        let target = (current_idx as i32 + delta).clamp(0, self.ids.len() as i32 - 1) as usize;
        self.move_window_to_index(hwnd, target)
    }

    /// Applies §8.3's dynamic-workspace policy at the tail only:
    /// - Trim redundant trailing empty workspaces (both the last and
    ///   second-last empty) down to one, never removing the current
    ///   workspace and never dropping below [`MIN_WORKSPACES`].
    /// - Append a new empty trailing workspace once the last one is
    ///   occupied.
    fn compact(&mut self) {
        while self.ids.len() > MIN_WORKSPACES {
            let last_idx = self.ids.len() - 1;
            let last_empty = !self.is_occupied(self.ids[last_idx]);
            let second_last_empty = !self.is_occupied(self.ids[last_idx - 1]);
            if last_empty && second_last_empty && last_idx != self.current {
                self.ids.pop();
            } else {
                break;
            }
        }

        let last_occupied = self
            .ids
            .last()
            .map(|&id| self.is_occupied(id))
            .unwrap_or(true);
        if last_occupied {
            self.ids.push(self.next_id);
            self.next_id += 1;
        }

        while self.ids.len() < MIN_WORKSPACES {
            self.ids.push(self.next_id);
            self.next_id += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_two_empty_workspaces() {
        let t = WorkspaceTracker::new();
        assert_eq!(t.workspace_ids().len(), 2);
        assert_eq!(t.current_index(), 0);
        assert!(t.windows_on(t.workspace_ids()[0]).is_empty());
        assert!(t.windows_on(t.workspace_ids()[1]).is_empty());
    }

    #[test]
    fn new_window_lands_on_current_workspace_without_growing() {
        let mut t = WorkspaceTracker::new();
        t.observe_active([100, 200]);
        let ws0 = t.workspace_ids()[0];
        assert_eq!(t.workspace_of(100), Some(ws0));
        assert_eq!(t.workspace_of(200), Some(ws0));
        // Filling a non-last workspace doesn't grow the list.
        assert_eq!(t.workspace_ids().len(), 2);
    }

    #[test]
    fn filling_the_trailing_workspace_grows_a_new_one() {
        let mut t = WorkspaceTracker::new();
        t.switch_to_index(1);
        t.observe_active([42]);
        assert_eq!(t.workspace_ids().len(), 3);
        assert_eq!(t.current_index(), 1);
        assert_eq!(t.windows_on(t.workspace_ids()[1]), vec![42]);
        assert!(t.windows_on(t.workspace_ids()[2]).is_empty());
    }

    #[test]
    fn current_empty_workspace_is_not_removed_out_from_under_the_user() {
        let mut t = WorkspaceTracker::new();
        t.switch_to_index(1);
        // Still empty here, but it's current, so it must survive.
        assert_eq!(t.current_index(), 1);
        assert_eq!(t.workspace_ids().len(), 2);
    }

    #[test]
    fn redundant_trailing_empties_trim_to_one() {
        let mut t = WorkspaceTracker::new();
        t.switch_to_index(1);
        t.observe_active([9]); // grows to [ws0, ws1(9), ws2]
        assert_eq!(t.workspace_ids().len(), 3);
        let ws0 = t.workspace_ids()[0];

        t.switch_to_index(0);
        // Move the only window off ws1 (now the middle workspace) onto
        // ws0: ws1 and ws2 are both empty, and ws2 (the true tail) isn't
        // current, so the redundant one trims away.
        t.move_window_relative(9, -1);
        assert_eq!(t.workspace_of(9), Some(ws0));
        assert_eq!(t.workspace_ids().len(), 2, "the extra trailing empty should collapse");
    }

    #[test]
    fn middle_empty_workspace_is_left_alone() {
        let mut t = WorkspaceTracker::new();
        t.switch_to_index(1);
        t.observe_active([9]); // grows to [ws0, ws1(9), ws2]
        t.switch_to_index(0);
        // ws1 -> ws2: ws1 is now empty but sits in the middle (not the
        // tail), and ws2 just became occupied so a new trailing empty
        // (ws3) appears — final shape: [ws0, ws1(empty), ws2(9), ws3(empty)].
        t.move_window_relative(9, 1);
        assert_eq!(t.workspace_ids().len(), 4, "ws1 isn't at the tail, so it stays");
        let ids = t.workspace_ids().to_vec();
        assert!(t.windows_on(ids[1]).is_empty(), "ws1 stays empty rather than collapsing");
        assert_eq!(t.windows_on(ids[2]), vec![9]);
        assert!(t.windows_on(ids[3]).is_empty(), "still exactly one trailing empty");
    }

    #[test]
    fn never_drops_below_minimum() {
        let mut t = WorkspaceTracker::new();
        t.observe_active([1, 2, 3]);
        t.prune(|_| false); // everything dies at once
        assert!(t.workspace_ids().len() >= MIN_WORKSPACES);
        assert_eq!(t.current_index(), 0);
    }

    #[test]
    fn prune_drops_dead_windows() {
        let mut t = WorkspaceTracker::new();
        t.observe_active([7]);
        t.prune(|hwnd| hwnd != 7);
        assert!(t.workspace_of(7).is_none());
    }

    #[test]
    fn move_window_relative_clamps_at_the_edges() {
        let mut t = WorkspaceTracker::new();
        let ws0 = t.workspace_ids()[0];
        let ws1 = t.workspace_ids()[1];
        t.observe_active([5]);
        assert_eq!(t.move_window_relative(5, -10), Some(ws0));
        assert_eq!(t.move_window_relative(5, 10), Some(ws1));
    }

    #[test]
    fn switch_relative_does_not_wrap() {
        let mut t = WorkspaceTracker::new();
        assert_eq!(t.switch_relative(-5), None, "already at index 0");
        assert_eq!(t.current_index(), 0);
        t.switch_relative(1);
        assert_eq!(t.current_index(), 1);
        t.switch_relative(1);
        assert_eq!(t.current_index(), 1, "only 2 workspaces exist yet, can't go past the last");
    }
}
