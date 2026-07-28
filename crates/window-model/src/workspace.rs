//! Workspace domain model: window→workspace assignment plus the "dynamic
//! workspace" policy from `docs/PROJECT_PLAN.md` §8.3 and ADR-005's
//! `ManagedWorkspaceBackend`. Deliberately platform-independent — actually
//! parking windows on or off the real desktop (`ShowWindow`) and driving
//! the Activities overview carousel are `groveshell-ui`'s job; this module
//! only tracks *which* window belongs to *which* workspace, so it can be
//! exercised with plain unit tests instead of a live Windows session.
//!
//! Per the user's guidance, each currently-connected *monitor* gets its own
//! fixed, always-present workspace slot — the "pinned prefix" of the list,
//! `[0, pinned_prefix)` — plus the usual GNOME-style dynamic tail beyond
//! it for genuinely virtual (hide/show-managed) workspaces. A monitor's
//! workspace is never removed by the dynamic-workspace policy even if it's
//! empty, since it represents real, always-visible screen space, not a
//! virtual one. `groveshell-ui` is responsible for deciding *which* pinned
//! index a window belongs to (by real screen position) and for only ever
//! hiding/showing windows assigned to the tail (index >= pinned_prefix) —
//! this module just protects the pinned range from the tail's
//! grow/shrink/collapse bookkeeping. Full per-monitor *virtual* workspace
//! sets (each monitor getting its own independent dynamic tail) are a
//! later, explicitly out-of-scope enhancement; for now every monitor
//! shares the one dynamic tail.
//!
//! Dynamic workspaces only ever grow or shrink at the *tail*: a new empty
//! workspace appears once the last one is occupied, and redundant trailing
//! empties collapse back down to one. A tail workspace that becomes empty
//! in the middle of the list is left alone rather than auto-collapsed —
//! removing it would silently renumber every workspace after it, which is
//! more disorienting than a stray empty workspace is useful.

use std::collections::BTreeMap;

/// Never fewer than this many workspaces exist in total, even with no
/// monitors seeded (`WorkspaceTracker::new`) and all of them empty —
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
    /// position. `ids[..pinned_prefix]` are monitor workspaces and never
    /// removed; anything beyond is the dynamic virtual tail.
    ids: Vec<WorkspaceId>,
    next_id: WorkspaceId,
    current: usize,
    pinned_prefix: usize,
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
    /// No monitor information available: falls back to the original
    /// "just two dynamic workspaces" shape (`pinned_prefix` 0).
    pub fn new() -> Self {
        Self::with_monitor_workspaces(0, 0)
    }

    /// One pinned, always-present workspace per currently-connected
    /// monitor (`monitor_count`, ordered however the caller likes — in
    /// practice left-to-right by screen position), plus the standard
    /// dynamic virtual tail. `current_index` (typically the primary
    /// monitor's position) is clamped into range.
    pub fn with_monitor_workspaces(monitor_count: usize, current_index: usize) -> Self {
        let ids: Vec<WorkspaceId> = (0..monitor_count as WorkspaceId).collect();
        let mut tracker = Self {
            next_id: ids.len() as WorkspaceId,
            ids,
            current: current_index.min(monitor_count.saturating_sub(1)),
            pinned_prefix: monitor_count,
            assignments: BTreeMap::new(),
        };
        tracker.compact();
        tracker
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

    /// Whether the workspace at `index` is a pinned monitor workspace
    /// (always present, never hidden/shown, never removed) rather than a
    /// dynamic virtual one.
    pub fn is_pinned(&self, index: usize) -> bool {
        index < self.pinned_prefix
    }

    pub fn pinned_count(&self) -> usize {
        self.pinned_prefix
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
    /// Only meaningful for the dynamic tail — callers seeding pinned
    /// monitor workspaces should assign windows directly via
    /// [`Self::move_window_to_index`] (called on a not-yet-tracked hwnd,
    /// which also works as an initial assignment) based on real screen
    /// position instead, since "current" isn't a meaningful home for a
    /// window that's already visible on some other monitor. Re-applies the
    /// dynamic-workspace policy afterward.
    pub fn observe_active<I: IntoIterator<Item = isize>>(&mut self, hwnds: I) {
        let current = self.current_id();
        for hwnd in hwnds {
            self.assignments.entry(hwnd).or_insert(current);
        }
        self.compact();
    }

    /// Assigns `hwnd` to the workspace at `index` outright, tracked or not
    /// — unlike [`Self::move_window_to_index`], this doesn't require the
    /// window to already be tracked. Used for initial per-monitor seeding.
    pub fn assign_to_index(&mut self, hwnd: isize, index: usize) {
        let index = index.min(self.ids.len().saturating_sub(1));
        if let Some(&id) = self.ids.get(index) {
            self.assignments.insert(hwnd, id);
            self.compact();
        }
    }

    /// Drops assignments for windows no longer alive (per the supplied
    /// predicate, typically `window_model::is_alive`), then re-applies the
    /// dynamic-workspace policy.
    pub fn prune<F: Fn(isize) -> bool>(&mut self, is_alive: F) {
        self.assignments.retain(|&hwnd, _| is_alive(hwnd));
        self.compact();
    }

    /// Drops a single window's assignment outright — used when the identity
    /// registry detects that `hwnd` was recycled for an unrelated window
    /// (see `registry::WindowRegistry::observe`), whose assignment must not
    /// be inherited. The handle can be re-assigned as a new window after.
    pub fn forget(&mut self, hwnd: isize) {
        if self.assignments.remove(&hwnd).is_some() {
            self.compact();
        }
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

    /// Applies §8.3's dynamic-workspace policy to the tail beyond
    /// `pinned_prefix` (pinned monitor workspaces are never touched):
    /// - Trim redundant trailing empty workspaces (both the last and
    ///   second-last empty) down to one, never removing the current
    ///   workspace.
    /// - Always keep at least one workspace beyond the pinned monitor
    ///   slots — the "extra one that's always there" — appending a new
    ///   empty one if the list currently ends exactly at the pinned
    ///   prefix or its last entry is occupied.
    /// - Never drop below [`MIN_WORKSPACES`] in total (only relevant when
    ///   `pinned_prefix` is 0 — [`Self::new`]'s fallback shape).
    fn compact(&mut self) {
        let floor = self.pinned_prefix.max(MIN_WORKSPACES);

        while self.ids.len() > floor {
            let last_idx = self.ids.len() - 1;
            if last_idx < self.pinned_prefix {
                break;
            }
            let last_empty = !self.is_occupied(self.ids[last_idx]);
            let second_last_empty = !self.is_occupied(self.ids[last_idx - 1]);
            if last_empty && second_last_empty && last_idx != self.current {
                self.ids.pop();
            } else {
                break;
            }
        }

        let needs_extra_tail = self.ids.len() <= self.pinned_prefix;
        let last_occupied = self
            .ids
            .last()
            .map(|&id| self.is_occupied(id))
            .unwrap_or(true);
        if needs_extra_tail || last_occupied {
            self.ids.push(self.next_id);
            self.next_id += 1;
        }

        while self.ids.len() < floor {
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

    #[test]
    fn monitor_workspaces_start_with_one_extra_trailing_empty() {
        // 2 monitors -> [monitor0, monitor1, extra-empty], matching the
        // user's "laptop, external (current), extra" 3-workspace layout.
        let t = WorkspaceTracker::with_monitor_workspaces(2, 1);
        assert_eq!(t.workspace_ids().len(), 3);
        assert_eq!(t.current_index(), 1);
        assert!(t.is_pinned(0));
        assert!(t.is_pinned(1));
        assert!(!t.is_pinned(2));
        assert_eq!(t.pinned_count(), 2);
    }

    #[test]
    fn pinned_monitor_workspace_is_never_removed_even_if_empty() {
        let mut t = WorkspaceTracker::with_monitor_workspaces(2, 0);
        // Fill the extra tail, switch away from monitor 1, prune — a
        // regular (non-pinned) empty middle workspace would be eligible
        // for removal consideration, but a pinned one never is.
        t.switch_to_index(2);
        t.observe_active([1]);
        assert_eq!(t.workspace_ids().len(), 4); // [m0, m1, ws(1), extra]
        t.switch_to_index(0);
        t.prune(|_| true);
        assert!(t.is_pinned(1));
        assert_eq!(t.workspace_ids().len(), 4, "monitor 1 stays even though it's empty");
    }

    #[test]
    fn assign_to_index_seeds_a_window_directly() {
        let mut t = WorkspaceTracker::with_monitor_workspaces(2, 0);
        let monitor1_id = t.workspace_ids()[1];
        t.assign_to_index(555, 1);
        assert_eq!(t.workspace_of(555), Some(monitor1_id));
    }

    #[test]
    fn forget_drops_one_assignment_and_reapplies_the_tail_policy() {
        let mut t = WorkspaceTracker::new();
        // Occupy the trailing workspace so an extra one grows after it.
        t.assign_to_index(10, 1);
        let grown = t.workspace_ids().len();
        assert!(grown > MIN_WORKSPACES);
        t.forget(10);
        assert_eq!(t.workspace_of(10), None);
        assert!(t.workspace_ids().len() < grown);
    }

    #[test]
    fn fallback_new_has_no_pinned_workspaces() {
        let t = WorkspaceTracker::new();
        assert_eq!(t.pinned_count(), 0);
        assert!(!t.is_pinned(0));
    }
}
