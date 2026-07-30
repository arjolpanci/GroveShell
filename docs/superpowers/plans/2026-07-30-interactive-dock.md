# Interactive Dock Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Activities overview's dock behave like GNOME's dash: GroveShell owns its own persisted pinned-app list (seeded once from the real taskbar, then independent), clicking a multi-window app cycles through its windows, right-click offers pin/unpin/open-new-window, and dragging a pinned icon reorders the dock or (dropped on a workspace card) launches the app assigned to that workspace.

**Architecture:** A new `dock_pins.rs` module owns the persisted pinned-path list (JSON file under the shared data directory, loaded once at startup into a `thread_local!`, mutated in place by pin/unpin/reorder and saved on every mutation). `dock.rs`'s existing `build_dock_apps` sources its pinned entries from this list instead of scanning the real taskbar's pin folder directly. A new `DockDrag` state (parallel to the existing `WindowDrag`/`CarouselDrag`) drives both reordering and drag-to-open, reusing the overview's established GDI/GPU dual-rendering fallback pattern throughout. A new `pending_launch.rs` module tracks "app just launched by drag, waiting to see its first window" so that window can be assigned to the workspace it was dropped on, hooked into the overview's existing debounced window-sync path.

**Tech Stack:** Rust, `windows` crate (Win32 `TrackPopupMenu`, `ShellExecuteExW`), `serde`/`serde_json` (new to `groveshell-ui`), existing `groveshell-window-model` workspace tracker.

## Global Constraints

- GroveShell's pinned list is independent of the real Windows taskbar from the moment it's first seeded — pinning/unpinning here never touches the real taskbar's pin folder, and the real taskbar is only ever read once, as the seed source when no persisted list exists yet.
- Reordering and drag-to-open apply only to **pinned** dock entries; running-but-unpinned entries are never reorderable and are not drag-to-open sources (per the approved spec's scope).
- Every `unsafe` Win32 call must carry a `// SAFETY:` comment, matching this codebase's existing convention.
- `cargo build --workspace` and `cargo clippy -p groveshell-ui --no-deps` must both be clean at the end of every task.
- No `Co-Authored-By` trailer on any commit (standing project rule).
- Pure logic (reorder computation, window-cycling, pending-launch match/expiry) gets real unit tests; Win32/UI integration (the context menu, the drag ghost, actual process launching) is manual-verification-only, consistent with the rest of this codebase's testing convention.

---

### Task 1: `dock_pins.rs` — persisted pinned-list storage

**Files:**
- Create: `apps/ui/src/imp/dock_pins.rs`
- Modify: `apps/ui/Cargo.toml` (add `serde`, `serde_json` to `[dependencies]`)
- Modify: `apps/ui/src/imp/mod.rs:1-30` (add `mod dock_pins;` alphabetically near the existing `mod dock;`)

**Interfaces:**
- Produces: `pub(crate) fn pins_file_path() -> Option<std::path::PathBuf>`, `pub(crate) fn load(path: &std::path::Path) -> Option<Vec<std::path::PathBuf>>`, `pub(crate) fn save(path: &std::path::Path, pins: &[std::path::PathBuf])`, `pub(crate) fn load_or_seed(path: &std::path::Path, seed: impl FnOnce() -> Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf>`, `pub(crate) fn reorder(pins: &[std::path::PathBuf], from: usize, to: usize) -> Vec<std::path::PathBuf>`.

- [ ] **Step 1: Add the dependencies**

In `apps/ui/Cargo.toml`, add to `[dependencies]` (both are already workspace dependencies per the root `Cargo.toml`, just not yet pulled into this crate):

```toml
[dependencies]
groveshell-common = { workspace = true }
groveshell-window-model = { workspace = true }
tracing = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 2: Write the failing tests for `reorder`**

Create `apps/ui/src/imp/dock_pins.rs`:

```rust
//! Persisted, independent pinned-dock-app list — GroveShell's own record
//! of what's pinned, seeded once from the real Windows taskbar's current
//! pins (see `dock::taskbar_pinned_shortcuts`) and never consulting it
//! again afterward. Deliberately not part of `groveshell-config`'s shared
//! `Config`: that file is user-editable settings shared (and potentially
//! written) across other processes with its own schema/versioning and
//! backup-on-save behavior tuned for occasional hand-edited changes, not
//! frequent UI-driven pin/unpin/reorder writes.

use std::path::{Path, PathBuf};

/// Reorders `pins` so the entry at `from` ends up at `to`. Out-of-range
/// indices or `from == to` return `pins` unchanged (cloned) rather than
/// panicking — a stale index from a race between a rebuild and a
/// still-in-flight drag should never crash the drag-drop handler.
pub(crate) fn reorder(pins: &[PathBuf], from: usize, to: usize) -> Vec<PathBuf> {
    if from >= pins.len() || to >= pins.len() || from == to {
        return pins.to_vec();
    }
    let mut result = pins.to_vec();
    let item = result.remove(from);
    result.insert(to, item);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_moves_the_entry_from_one_index_to_another() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let result = reorder(&pins, 0, 2);
        assert_eq!(result, vec![PathBuf::from("b"), PathBuf::from("c"), PathBuf::from("a")]);
    }

    #[test]
    fn reorder_moving_backward_shifts_the_entries_between() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let result = reorder(&pins, 2, 0);
        assert_eq!(result, vec![PathBuf::from("c"), PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn reorder_same_index_is_a_no_op() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(reorder(&pins, 1, 1), pins);
    }

    #[test]
    fn reorder_out_of_range_index_returns_pins_unchanged() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(reorder(&pins, 0, 5), pins);
        assert_eq!(reorder(&pins, 5, 0), pins);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail to compile initially, then pass**

Run: `cargo test -p groveshell-ui dock_pins::tests --lib`
Expected: 4 tests pass (the module only contains the pure function so far — no compile failures expected once written; if this is your first time running tests in this crate, confirm there's a `#[cfg(test)]`-gated unit test harness already wired for `lib`-style tests inside `apps/ui/src` — check `apps/ui/src/imp/overview.rs`'s existing `#[cfg(test)] mod tests` blocks for the established pattern, since `groveshell-ui` is a `[[bin]]`-only crate and its tests run via `cargo test -p groveshell-ui`, not `--lib`).

- [ ] **Step 4: Add file load/save/seed**

Append to `dock_pins.rs`:

```rust
/// The persisted pinned-list file's path: `<data_dir>/dock_pins.json`.
/// `None` only if the data directory itself can't be determined (see
/// `groveshell_common::paths::data_dir`).
pub(crate) fn pins_file_path() -> Option<PathBuf> {
    groveshell_common::paths::data_dir().ok().map(|d| d.join("dock_pins.json"))
}

/// Loads the persisted list. `None` if the file doesn't exist yet or
/// fails to parse — both cases mean "nothing persisted," which the
/// caller (`load_or_seed`) treats as "seed it."
pub(crate) fn load(path: &Path) -> Option<Vec<PathBuf>> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Overwrites the persisted list. Best-effort: a failed write (e.g. the
/// data directory briefly unwritable) just means the next pin/unpin/
/// reorder tries again — there's no in-memory state lost, since the
/// caller's own `thread_local!` copy is already updated regardless.
pub(crate) fn save(path: &Path, pins: &[PathBuf]) {
    let Ok(data) = serde_json::to_string_pretty(pins) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, data);
}

/// Loads the persisted list if it exists; otherwise calls `seed` (the
/// real taskbar's current pins, at startup) and persists that as the
/// starting point — from then on this file is authoritative and the
/// real taskbar is never consulted again.
pub(crate) fn load_or_seed(path: &Path, seed: impl FnOnce() -> Vec<PathBuf>) -> Vec<PathBuf> {
    if let Some(pins) = load(path) {
        return pins;
    }
    let seeded = seed();
    save(path, &seeded);
    seeded
}
```

- [ ] **Step 5: Register the module and build**

In `apps/ui/src/imp/mod.rs`, add `mod dock_pins;` near the existing `mod dock;` declaration.

Run: `cargo build --workspace`
Expected: clean (nothing calls these functions yet — that's Task 2).

Run: `cargo test -p groveshell-ui`
Expected: the 4 `dock_pins::tests` pass; no other test regressions.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/Cargo.toml apps/ui/src/imp/mod.rs apps/ui/src/imp/dock_pins.rs
git commit -m "feat: add persisted pinned-dock-app list storage"
```

---

### Task 2: Wire the persisted list into `build_dock_apps`

**Files:**
- Modify: `apps/ui/src/imp/dock.rs` (pinned-list `thread_local!`, `build_dock_apps`, new `pin`/`unpin` functions)
- Modify: `apps/ui/src/imp/mod.rs` (load the list at startup)

**Interfaces:**
- Consumes: `dock_pins::{pins_file_path, load_or_seed, save}` (Task 1), `dock::taskbar_pinned_shortcuts` (existing).
- Produces: `pub(crate) fn init_pinned_list()`, `pub(crate) fn pinned_paths() -> Vec<PathBuf>`, `pub(crate) fn pin_app(path: PathBuf)`, `pub(crate) fn unpin_app(path: &Path)`.

- [ ] **Step 1: Add the pinned-list `thread_local!` and accessors**

In `apps/ui/src/imp/dock.rs`, add near the existing `PINNED_ICON_CACHE` thread_local:

```rust
thread_local! {
    /// GroveShell's own pinned-app list — the authoritative source for
    /// `build_dock_apps`'s pinned entries from now on (see
    /// `dock_pins.rs`'s module doc for why this isn't the real
    /// taskbar's pin folder anymore). Loaded once at startup via
    /// `init_pinned_list`.
    static PINNED_PATHS: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
}

/// Loads the persisted pinned list (seeding it from the real taskbar's
/// current pins if this is the first run) into `PINNED_PATHS`. Must run
/// once, at startup, before the first `build_dock_apps` call.
pub(crate) fn init_pinned_list() {
    let Some(path) = super::dock_pins::pins_file_path() else { return };
    let pins = super::dock_pins::load_or_seed(&path, taskbar_pinned_shortcuts);
    PINNED_PATHS.with(|p| *p.borrow_mut() = pins);
}

/// The current pinned list, in order — read by `build_dock_apps`.
pub(crate) fn pinned_paths() -> Vec<PathBuf> {
    PINNED_PATHS.with(|p| p.borrow().clone())
}

fn persist_pinned_paths(pins: &[PathBuf]) {
    if let Some(path) = super::dock_pins::pins_file_path() {
        super::dock_pins::save(&path, pins);
    }
}

/// Adds `path` to the end of the pinned list (a no-op if already
/// pinned) and persists it.
pub(crate) fn pin_app(path: PathBuf) {
    PINNED_PATHS.with(|p| {
        let mut pins = p.borrow_mut();
        if !pins.contains(&path) {
            pins.push(path);
            persist_pinned_paths(&pins);
        }
    });
}

/// Removes `path` from the pinned list, if present, and persists it.
pub(crate) fn unpin_app(path: &Path) {
    PINNED_PATHS.with(|p| {
        let mut pins = p.borrow_mut();
        if let Some(i) = pins.iter().position(|p| p == path) {
            pins.remove(i);
            persist_pinned_paths(&pins);
        }
    });
}
```

- [ ] **Step 2: Source `build_dock_apps` from the persisted list**

In `apps/ui/src/imp/dock.rs`'s `build_dock_apps`, change:

```rust
    for lnk in taskbar_pinned_shortcuts() {
```
to
```rust
    for lnk in pinned_paths() {
```

(The rest of the loop body — resolving the target, matching windows, building the `DockApp` — is unchanged; it already works on whatever `PathBuf` it's handed.)

- [ ] **Step 3: Call `init_pinned_list` at startup**

In `apps/ui/src/imp/mod.rs`'s `main()`, add `dock::init_pinned_list();` right after the existing `gpu::init();` call (order doesn't matter relative to `gpu::init` specifically, but it must run before the first overview window's dock is built, which happens later in the same function).

- [ ] **Step 4: Build and verify**

Run: `cargo build --workspace`
Expected: clean.
Run: `cargo test -p groveshell-ui`
Expected: no regressions.
Run: `cargo clippy -p groveshell-ui --no-deps`
Expected: clean (aside from the one pre-existing, unrelated `sort_by` warning in `overview.rs`).

- [ ] **Step 5: Manual verification**

Delete (or rename) `%LOCALAPPDATA%\GroveShell\dock_pins.json` if it exists, run the app, open Activities: the dock should show the same pinned apps the real taskbar currently has. Confirm the file now exists at that path with the expected JSON array of paths.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/dock.rs apps/ui/src/imp/mod.rs
git commit -m "feat: source the dock's pinned apps from GroveShell's own persisted list"
```

---

### Task 3: Cycle through a multi-window app's windows on repeat click

**Files:**
- Modify: `apps/ui/src/imp/dock.rs` (`activate_dock_app`, new pure `next_window` function, new last-focused cache)

**Interfaces:**
- Produces: `pub(crate) fn next_window(windows: &[isize], last_focused: Option<isize>) -> Option<isize>`.

- [ ] **Step 1: Write the failing tests**

Add to `apps/ui/src/imp/dock.rs`'s existing test module (create one with `#[cfg(test)] mod tests { use super::*; ... }` at the end of the file if none exists yet — check first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_window_with_no_last_focused_returns_the_first() {
        assert_eq!(next_window(&[10, 20, 30], None), Some(10));
    }

    #[test]
    fn next_window_advances_past_the_last_focused() {
        assert_eq!(next_window(&[10, 20, 30], Some(10)), Some(20));
    }

    #[test]
    fn next_window_wraps_around_after_the_last_entry() {
        assert_eq!(next_window(&[10, 20, 30], Some(30)), Some(10));
    }

    #[test]
    fn next_window_falls_back_to_first_if_last_focused_is_gone() {
        assert_eq!(next_window(&[10, 20, 30], Some(999)), Some(10));
    }

    #[test]
    fn next_window_with_no_windows_returns_none() {
        assert_eq!(next_window(&[], Some(10)), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p groveshell-ui dock::tests::next_window`
Expected: FAIL — `next_window` not defined.

- [ ] **Step 3: Implement `next_window`**

Add above the test module:

```rust
/// The window to focus next in `windows`, given whichever one was last
/// focused (or `None` if this entry has never been clicked, or its
/// previously-focused window closed) — advances past `last_focused` and
/// wraps around; falls back to the first window if `last_focused` isn't
/// (or no longer is) in `windows`.
pub(crate) fn next_window(windows: &[isize], last_focused: Option<isize>) -> Option<isize> {
    if windows.is_empty() {
        return None;
    }
    let Some(last) = last_focused else {
        return Some(windows[0]);
    };
    match windows.iter().position(|&w| w == last) {
        Some(i) => Some(windows[(i + 1) % windows.len()]),
        None => Some(windows[0]),
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p groveshell-ui dock::tests::next_window`
Expected: 5 tests pass.

- [ ] **Step 5: Wire into `activate_dock_app`**

Add the last-focused cache and use it. In `apps/ui/src/imp/dock.rs`, add near `PINNED_ICON_CACHE`:

```rust
thread_local! {
    /// Last-focused window per dock entry, keyed by the entry's
    /// lowercased exe name (the same key `build_dock_apps` already uses
    /// to group windows into one entry, for both pinned and running-
    /// unpinned entries) — lets repeat clicks on a multi-window app
    /// cycle through its windows instead of always refocusing the
    /// first one. Cleared implicitly by just going stale (an exe key
    /// for an app that's no longer running simply never gets read
    /// again); no explicit eviction needed.
    static LAST_FOCUSED: RefCell<HashMap<String, isize>> = RefCell::new(HashMap::new());
}
```

Then find `activate_dock_app` and change the branch that currently does:

```rust
        if let Some(&hwnd) = app.windows.first() {
```

Instead, first compute an exe key for the entry (reusing the same lowercasing already done elsewhere in this file — the entry itself doesn't currently store its exe key, so derive it from its first window via `groveshell_window_model::describe`, matching the pattern `build_dock_apps` uses):

```rust
        if !app.windows.is_empty() {
            let exe_key = app
                .windows
                .first()
                .and_then(|&hwnd| groveshell_window_model::describe(hwnd))
                .and_then(|w| w.exe_name)
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            let last = LAST_FOCUSED.with(|m| m.borrow().get(&exe_key).copied());
            let hwnd = next_window(&app.windows, last).unwrap();
            LAST_FOCUSED.with(|m| { m.borrow_mut().insert(exe_key, hwnd); });
```

(This replaces the `if let Some(&hwnd) = app.windows.first() {` line; the rest of that branch's body — computing `tracker`/`id`/`page`/`current` and returning `Some((Some(hwnd), page, current, None))` — is unchanged, just now reads the cycled `hwnd` instead of always `windows[0]`.)

- [ ] **Step 6: Build and verify**

Run: `cargo build --workspace`
Expected: clean.
Run: `cargo test -p groveshell-ui`
Expected: all passing, including the 5 new `next_window` tests.

- [ ] **Step 7: Manual verification**

Open an app with two windows (e.g. two Explorer windows), open Activities, click its dock icon twice in a row (with a click elsewhere or a short overview-close/reopen between, since one click focuses+closes the overview) — confirm the second click focuses the other window, not the same one again.

- [ ] **Step 8: Commit**

```bash
git add apps/ui/src/imp/dock.rs
git commit -m "feat: cycle through a multi-window app's windows on repeat dock click"
```

---

### Task 4: `pending_launch.rs` — best-effort workspace assignment for a drag-launched app

**Files:**
- Create: `apps/ui/src/imp/pending_launch.rs`
- Modify: `apps/ui/src/imp/mod.rs` (register the module)
- Modify: `apps/ui/src/imp/workspaces.rs` (hook into `sync_workspaces`)

**Interfaces:**
- Produces: `pub(crate) struct PendingLaunch { pub(crate) process_id: u32, pub(crate) monitor: String, pub(crate) workspace_index: usize, pub(crate) expires_at: std::time::Instant }`, `pub(crate) fn register(pending: PendingLaunch)`, `pub(crate) fn take_match(pid: u32) -> Option<(String, usize)>`.
- Consumes (Task 6 will call `register`; this task only builds and wires `take_match`): `groveshell_window_model::WindowRecord.pid` (existing field).

- [ ] **Step 1: Write the failing tests**

Create `apps/ui/src/imp/pending_launch.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify they fail, then pass**

Run: `cargo test -p groveshell-ui pending_launch::tests`
Expected: FAIL first (nothing to compile against yet is untrue — the module is self-contained and should compile; if it fails, it's a typo, fix it), then run again and expect 4 passes.

- [ ] **Step 3: Add the thread-local registry and public wrappers**

Append to `pending_launch.rs`:

```rust
thread_local! {
    static PENDING: RefCell<Vec<PendingLaunch>> = RefCell::new(Vec::new());
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
```

- [ ] **Step 4: Register the module**

In `apps/ui/src/imp/mod.rs`, add `mod pending_launch;` alphabetically.

- [ ] **Step 5: Hook into `sync_workspaces`**

In `apps/ui/src/imp/workspaces.rs`'s `sync_workspaces`, find the branch handling a brand-new window:

```rust
                    (None, real) => {
                        let target_monitor = real.unwrap_or_else(|| state.primary_monitor.clone());
                        if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                            let index = tracker.current_index();
                            tracker.assign_to_index(window.hwnd, index);
                        }
                    }
```

Change it to check for a pending-launch match first, assigning to that target instead of the default (`real`/primary's current index) when one exists:

```rust
                    (None, real) => {
                        if let Some((target_monitor, index)) = super::pending_launch::take_match(window.pid) {
                            if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                                tracker.assign_to_index(window.hwnd, index);
                            }
                        } else {
                            let target_monitor = real.unwrap_or_else(|| state.primary_monitor.clone());
                            if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                                let index = tracker.current_index();
                                tracker.assign_to_index(window.hwnd, index);
                            }
                        }
                    }
```

- [ ] **Step 6: Build and verify**

Run: `cargo build --workspace`
Expected: clean.
Run: `cargo test -p groveshell-ui`
Expected: all passing, including the 4 new `pending_launch` tests.
Run: `cargo clippy -p groveshell-ui --no-deps`
Expected: clean (aside from the pre-existing unrelated warning).

- [ ] **Step 7: Commit**

```bash
git add apps/ui/src/imp/pending_launch.rs apps/ui/src/imp/mod.rs apps/ui/src/imp/workspaces.rs
git commit -m "feat: track drag-launched apps to assign their first window to the drop target"
```

---

### Task 5: Right-click context menu (pin/unpin, open new window)

**Files:**
- Modify: `apps/ui/src/imp/dock.rs` (menu-building/handling function)
- Modify: `apps/ui/src/imp/overview.rs` (new `on_overview_right_click` dispatch function)
- Modify: `apps/ui/src/imp/mod.rs` (route `WM_RBUTTONDOWN` for `Role::Overview`)

**Interfaces:**
- Consumes: `dock::{pin_app, unpin_app, dock_layout}` (existing/Task 2), `DockApp.launch_path` (existing).
- Produces: `pub(crate) fn on_overview_right_click(monitor: &str, x: i32, y: i32)` (in `overview.rs`, delegating to a new `dock::show_context_menu`).

- [ ] **Step 1: Write `dock::show_context_menu`**

Add to `apps/ui/src/imp/dock.rs`:

```rust
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenu,
    MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
};

const MENU_ID_UNPIN: u32 = 1;
const MENU_ID_PIN: u32 = 2;
const MENU_ID_OPEN_NEW_WINDOW: u32 = 3;

/// Shows the right-click context menu for the dock entry at `index`
/// (already resolved by the caller via `dock_layout`'s slot hit-test),
/// then performs whichever action was chosen. A running-but-unpinned
/// entry (no `launch_path`) gets no menu at all — right-clicking it is
/// a no-op, matching today's behavior — since there's no shortcut to
/// pin or relaunch from a bare running-window entry.
pub(crate) fn show_context_menu(monitor: &str, overview_hwnd: HWND, index: usize) {
    let Some(app_launch_path) = super::state::STATE.with(|s| {
        let state = s.borrow();
        let ov = state.as_ref()?.overviews.get(monitor)?;
        ov.dock_apps.get(index)?.launch_path.clone()
    }) else {
        return;
    };

    // SAFETY: every call here is a standard, synchronous Win32 popup-menu
    // sequence; `menu` is destroyed before returning on every path.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let is_pinned = pinned_paths().contains(&app_launch_path);
        let pin_label = if is_pinned { w!("Unpin from dock") } else { w!("Pin to dock") };
        let pin_id = if is_pinned { MENU_ID_UNPIN } else { MENU_ID_PIN };
        let _ = AppendMenuW(menu, MF_STRING, pin_id as usize, pin_label);
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_OPEN_NEW_WINDOW as usize, w!("Open new window"));

        let mut point = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(overview_hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            point.x,
            point.y,
            0,
            overview_hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as u32 {
            MENU_ID_UNPIN => unpin_app(&app_launch_path),
            MENU_ID_PIN => pin_app(app_launch_path),
            MENU_ID_OPEN_NEW_WINDOW => {
                let wide: Vec<u16> =
                    app_launch_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                let _ = ShellExecuteW(
                    HWND(std::ptr::null_mut()),
                    w!("open"),
                    PCWSTR(wide.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
            _ => {}
        }
    }
    super::overview::rebuild_open_overview_pages(monitor);
}
```

(`dock.rs` already has one `use windows::Win32::UI::WindowsAndMessaging::{HICON};` and a separate `use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;` — leave both as they are and add the new `use` statement above as a third, self-contained import from that module, matching this file's existing split-import style rather than merging them.)

- [ ] **Step 2: Add the dispatch function in `overview.rs`**

Add near `on_overview_click`:

```rust
/// A right-click while the overview is open: only dock icons have any
/// right-click behavior right now — hit-tests the same `dock_layout`
/// slots the plain click/hover paths already use, and shows the
/// context menu for whichever one (if any) was clicked.
pub(crate) fn on_overview_right_click(monitor: &str, x: i32, y: i32) {
    let hit = STATE.with(|s| {
        let state = s.borrow();
        let st = state.as_ref()?;
        let ov = st.overviews.get(monitor)?;
        if !matches!(ov.mode, OverviewMode::Open { .. }) {
            return None;
        }
        let (_, slots) = super::dock::dock_layout(monitor, ov.dock_apps.len());
        let index = slots.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)?;
        Some((ov.hwnd, index))
    });
    if let Some((overview_hwnd, index)) = hit {
        super::dock::show_context_menu(monitor, overview_hwnd, index);
    }
}
```

- [ ] **Step 3: Route `WM_RBUTTONDOWN` in `mod.rs`**

Find the existing `WM_LBUTTONDOWN =>` match arm in `apps/ui/src/imp/mod.rs`'s `wndproc` (it already has a `Role::Overview { monitor } => { ... on_overview_drag_start(&monitor, x, y); ... }` branch). Add a new top-level arm right after it:

```rust
        WM_RBUTTONDOWN => {
            if let Role::Overview { monitor } = role {
                let x = (lparam.0 & 0xFFFF) as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                on_overview_right_click(&monitor, x, y);
            }
            LRESULT(0)
        }
```

Add `WM_RBUTTONDOWN` to the existing `use windows::Win32::UI::WindowsAndMessaging::{...}` import list at the top of `mod.rs` (it's a plain `u32` constant, same import style as the already-imported `WM_LBUTTONDOWN`), and add `on_overview_right_click` to the existing `use super::overview::{...}` import list.

- [ ] **Step 4: Build and verify**

Run: `cargo build --workspace`
Expected: clean.
Run: `cargo clippy -p groveshell-ui --no-deps`
Expected: clean (aside from the pre-existing unrelated warning).

- [ ] **Step 5: Manual verification**

Open Activities, right-click a pinned dock icon: confirm the menu shows "Unpin from dock" and "Open new window," and each does what it says (unpinning removes it from the dock and persists across a restart; "Open new window" launches another instance). Right-click a running-but-unpinned icon: confirm nothing happens (no menu).

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/dock.rs apps/ui/src/imp/overview.rs apps/ui/src/imp/mod.rs
git commit -m "feat: add a right-click context menu to dock icons (pin/unpin, open new window)"
```

---

### Task 6: Drag-to-reorder and drag-to-open

**Files:**
- Modify: `apps/ui/src/imp/dock.rs` (`DockDrag` struct, reorder/launch-on-drop logic)
- Modify: `apps/ui/src/imp/overview.rs` (`OverviewInstance.dock_drag` field, drag start/move/end wiring, GDI ghost paint)
- Modify: `apps/ui/src/imp/overview_gpu.rs` (GPU ghost paint)
- Modify: `apps/ui/Cargo.toml` (add `Win32_System_Threading` feature, for `GetProcessId`)

**Interfaces:**
- Consumes: `dock_pins::reorder` (Task 1), `dock::{pin_app is not needed here, pinned_paths}` (Task 2), `pending_launch::{PendingLaunch, register, PENDING_LAUNCH_TIMEOUT}` (Task 4), `WindowDrag`/`CarouselDrag`'s existing sibling-pattern in `overview.rs` (existing).
- Produces: `pub(crate) struct DockDrag { pub(crate) start_x: i32, pub(crate) start_y: i32, pub(crate) cur_x: i32, pub(crate) cur_y: i32, pub(crate) max_delta: i32, pub(crate) from_index: usize, pub(crate) icon: Option<HICON> }` (in `dock.rs`), `pub(crate) fn on_dock_drag_end(monitor: &str, drag: DockDrag, x: i32, y: i32)` (in `dock.rs`).

- [ ] **Step 1: Add the `Win32_System_Threading` feature**

In `apps/ui/Cargo.toml`, add `"Win32_System_Threading",` to the `windows` feature list (alphabetically, after `"Win32_System_SystemServices"`).

- [ ] **Step 2: Add `DockDrag` and the reorder/launch-on-drop logic**

Add to `apps/ui/src/imp/dock.rs`:

```rust
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS};

/// An in-progress drag of a **pinned** dock icon — either reordering it
/// within the dock, or dragging it out onto a workspace card to open a
/// new instance there. Which of the two happens is decided purely by
/// where the pointer releases (see `on_dock_drag_end`), not by anything
/// decided at drag-start.
pub(crate) struct DockDrag {
    pub(crate) start_x: i32,
    pub(crate) start_y: i32,
    pub(crate) cur_x: i32,
    pub(crate) cur_y: i32,
    pub(crate) max_delta: i32,
    /// This pin's index among `pinned_paths()` at drag-start (stable for
    /// the duration of one drag — the pinned list isn't rebuilt mid-drag).
    pub(crate) from_index: usize,
    /// Ghost icon to draw at the cursor while dragging.
    pub(crate) icon: Option<HICON>,
}

/// Ends a drag that moved past the click threshold (the caller already
/// handled the below-threshold "was actually just a click" case, same
/// as `WindowDrag`/`CarouselDrag`'s own end handlers do). Reorders if
/// dropped within the dock bar; launches assigned to a workspace if
/// dropped on a card; cancels (no-op) otherwise.
pub(crate) fn on_dock_drag_end(monitor: &str, drag: DockDrag, x: i32, y: i32) {
    let pins = pinned_paths();
    // `dock_layout` must be called with the *total* displayed entry
    // count (pinned + running-unpinned), exactly like every other
    // hit-test in this file (`on_overview_click`, `on_overview_hover`)
    // — using only `pins.len()` would compute a narrower bar than what's
    // actually on screen (running-unpinned entries widen it), shifting
    // every slot's x position and silently breaking this hit-test.
    let total_count = super::state::STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.overviews.get(monitor))
            .map(|ov| ov.dock_apps.len())
    });
    let Some(total_count) = total_count else { return };
    let (bar_rect, slots) = dock_layout(monitor, total_count.max(1));
    if x >= bar_rect.left && x < bar_rect.right && y >= bar_rect.top && y < bar_rect.bottom {
        // Reorder: target index is whichever *pinned* slot's center is
        // closest to the drop point — only the first `pins.len()` slots
        // are pinned entries (`build_dock_apps` always places pinned
        // entries first), so running-unpinned slots (if any) are never
        // eligible reorder targets.
        let target = slots
            .iter()
            .take(pins.len())
            .enumerate()
            .min_by_key(|(_, r)| ((r.left + r.right) / 2 - x).abs())
            .map(|(i, _)| i)
            .unwrap_or(drag.from_index);
        let reordered = super::dock_pins::reorder(&pins, drag.from_index, target);
        PINNED_PATHS.with(|p| *p.borrow_mut() = reordered.clone());
        persist_pinned_paths(&reordered);
        super::overview::rebuild_open_overview_pages(monitor);
        return;
    }

    let (card_rect, pitch) = super::overview::card_layout(monitor);
    let carousel_offset = super::state::STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| st.overviews.get(monitor)).map(|ov| ov.carousel_offset)
    });
    let Some(carousel_offset) = carousel_offset else { return };
    if y < card_rect.top || y >= card_rect.bottom {
        return; // Released outside any card — cancel.
    }
    let card_center_x = (card_rect.left + card_rect.right) / 2;
    let approx_offset = carousel_offset + (x - card_center_x) as f64 / pitch as f64;
    let max_page = super::state::STATE
        .with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.workspaces.get(monitor))
                .map(|t| t.workspace_ids().len())
        })
        .unwrap_or(0)
        .saturating_sub(1);
    let target_page = approx_offset.round().clamp(0.0, max_page as f64) as usize;
    if (approx_offset - target_page as f64).abs() > 0.5 {
        return; // Not confidently over any specific card — cancel.
    }

    let Some(launch_path) = pins.get(drag.from_index).cloned() else { return };
    let wide: Vec<u16> = launch_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut info = windows::Win32::UI::Shell::SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<windows::Win32::UI::Shell::SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("open"),
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is a local, fully-initialized struct valid for the
    // duration of this call; `wide` outlives it. `info.hProcess`, on
    // success with `SEE_MASK_NOCLOSEPROCESS`, is a process handle this
    // call site owns and must close.
    unsafe {
        if ShellExecuteExW(&mut info).is_ok() && !info.hProcess.is_invalid() {
            let pid = GetProcessId(info.hProcess);
            super::pending_launch::register(super::pending_launch::PendingLaunch {
                process_id: pid,
                monitor: monitor.to_string(),
                workspace_index: target_page,
                expires_at: std::time::Instant::now() + super::pending_launch::PENDING_LAUNCH_TIMEOUT,
            });
            let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        }
    }
    super::overview::close_overview(monitor, None);
}
```

- [ ] **Step 3: Add `dock_drag` to `OverviewInstance` and wire drag-start**

In `apps/ui/src/imp/overview.rs`, add to the `OverviewInstance` struct (next to `window_drag`):

```rust
    pub(crate) dock_drag: Option<super::dock::DockDrag>,
```

Add `dock_drag: None,` to `OverviewInstance::new`'s constructor.

In `on_overview_drag_start`, change the dock-press branch (currently `if dock_slots.iter().any(...) { return None; }`) to start a `DockDrag` when the hit slot is a **pinned** entry, and keep today's behavior (no drag) otherwise:

```rust
        let (_, dock_slots) = super::dock::dock_layout(monitor, ov.dock_apps.len());
        let dock_hit = dock_slots.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
        if let Some(index) = dock_hit {
            let is_pinned = ov.dock_apps.get(index).is_some_and(|a| a.launch_path.is_some());
            if is_pinned {
                ov.hover_thumb = None;
                ov.dock_drag = Some(super::dock::DockDrag {
                    start_x: x,
                    start_y: y,
                    cur_x: x,
                    cur_y: y,
                    max_delta: 0,
                    from_index: index,
                    icon: ov.dock_apps.get(index).and_then(|a| a.icon),
                });
                return Some(ov.hwnd);
            }
            return None;
        }
```

(This replaces the existing two-line dock-exclusion check; everything below it — the thumb hit-test, `CarouselDrag`/`WindowDrag` construction — is unchanged.)

- [ ] **Step 4: Wire drag-move**

In `on_overview_drag_move`, add a new arm to the `MoveKind` enum: `Dock,` — and, in the `STATE.with` closure, check `ov.dock_drag.is_some()` first (before the existing `window_drag`/`carousel_drag` checks):

```rust
        if let Some(drag) = ov.dock_drag.as_mut() {
            drag.cur_x = x;
            drag.cur_y = y;
            let travel = (x - drag.start_x).abs().max((y - drag.start_y).abs());
            drag.max_delta = drag.max_delta.max(travel);
            return Some((ov.hwnd, MoveKind::Dock));
        }
```

In the second `STATE.with` block (the one calling `update_transforms`/`paint_root`), add a `MoveKind::Dock => { super::overview_gpu::paint_root(gpu, monitor, ov); }` arm alongside the existing `MoveKind::Window`/`MoveKind::Carousel` handling (`update_transforms` still runs unconditionally before the match, same as today, since the carousel position itself never changes during a dock drag — only the ghost needs repainting).

- [ ] **Step 5: Wire drag-end**

In `on_overview_drag_end`, add a new block at the very top (before the existing `window_drag` handling):

```rust
    let dock_drag = STATE.with(|s| {
        s.borrow_mut()
            .as_mut()
            .and_then(|st| st.overviews.get_mut(monitor))
            .and_then(|ov| ov.dock_drag.take())
    });
    if let Some(drag) = dock_drag {
        if drag.max_delta <= CAROUSEL_DRAG_CLICK_THRESHOLD_PX {
            on_overview_click(monitor, x, y);
        } else {
            super::dock::on_dock_drag_end(monitor, drag, x, y);
        }
        return;
    }
```

- [ ] **Step 6: Ghost rendering — GDI**

`paint_overview`'s `STATE.with` closure builds and returns a 9-element
tuple ending `Some((cards, snapshots, placeholders, icons, ghost,
hover_glow, thumb_hover_glow, dock, search))`, destructured back out at
the paint call site as `if let Some((cards, snapshots, placeholders,
icons, ghost, hover_glow, thumb_hover_glow, dock, search)) = content {`.
Add a 10th element, `dock_drag_ghost`, computed right before that
`Some((...))` line:

```rust
        let dock_drag_ghost = ov.dock_drag.as_ref().and_then(|drag| {
            drag.icon.map(|icon| (drag.cur_x, drag.cur_y, icon))
        });
        Some((cards, snapshots, placeholders, icons, ghost, hover_glow, thumb_hover_glow, dock, search, dock_drag_ghost))
```

(This replaces the existing `Some((cards, snapshots, placeholders, icons, ghost, hover_glow, thumb_hover_glow, dock, search))` line.)

At the paint call site, change the destructure to match:

```rust
        if let Some((cards, snapshots, placeholders, icons, ghost, hover_glow, thumb_hover_glow, dock, search, dock_drag_ghost)) = content {
```

Then, right after the existing window-drag ghost block (the one ending
`if let Some((cx, cy, base_w, base_h, drag_hwnd, scale)) = ghost { ... }`
inside this same `if let Some((...)) = content` body), add:

```rust
            // The dock-drag ghost: a plain icon following the cursor,
            // no pop-in/out animation (unlike the window-drag ghost) —
            // simpler by design, since a dock icon isn't a captured
            // window snapshot.
            if let Some((gx, gy, icon)) = dock_drag_ghost {
                let size = scaled(40, dpi);
                let _ = DrawIconEx(mem, gx - size / 2, gy - size / 2, icon, size, size, 0, None, DI_NORMAL);
            }
```

- [ ] **Step 7: Ghost rendering — GPU**

In `apps/ui/src/imp/overview_gpu.rs`'s `paint_root`, right after the existing window-drag ghost block (ending `if let Some((cx, cy, base_w, base_h, ghost_hwnd, scale)) = ghost { ... }`), add:

```rust
        if let Some(drag) = ov.dock_drag.as_ref() {
            if let Some(icon) = drag.icon {
                let size = scaled(40, dpi);
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        let rect = D2D_RECT_F {
                            left: (drag.cur_x - size / 2) as f32,
                            top: (drag.cur_y - size / 2) as f32,
                            right: (drag.cur_x + size / 2) as f32,
                            bottom: (drag.cur_y + size / 2) as f32,
                        };
                        gpu::draw_rounded_bitmap(ctx, rect, 0.0, &bitmap);
                    }
                    // SAFETY: created locally above, owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
            }
        }
```

- [ ] **Step 8: Build and verify**

Run: `cargo build --workspace`
Expected: clean — fix any borrow-checker issues that arise from the tuple-threading in Step 6 (this is the one step in this task most likely to need adjustment to fit `paint_overview`'s actual current tuple shape; read that function's current `content` construction closely before editing it).
Run: `cargo test -p groveshell-ui`
Expected: all passing, no regressions.
Run: `cargo clippy -p groveshell-ui --no-deps`
Expected: clean (aside from the pre-existing unrelated warning).

- [ ] **Step 9: Manual verification**

Drag a pinned dock icon to a different position within the dock: confirm it reorders and the new order persists after closing/reopening the overview and after restarting the app. Drag a pinned dock icon onto a different workspace card: confirm the app launches and its window ends up on that workspace once it appears (may take a moment for a slow-starting app). Drag a pinned icon and release on empty space: confirm nothing happens. Confirm dragging a running-but-unpinned icon still behaves exactly as before (no reorder, no drag-to-open — it's not a `DockDrag` at all, since only pinned entries start one).

- [ ] **Step 10: Commit**

```bash
git add apps/ui/Cargo.toml apps/ui/src/imp/dock.rs apps/ui/src/imp/overview.rs apps/ui/src/imp/overview_gpu.rs
git commit -m "feat: drag pinned dock icons to reorder or open onto a workspace"
```

---

### Self-Review Notes

- **Spec coverage:** §1 (persisted storage) → Task 1; §2 (`build_dock_apps` sourcing) → Task 2; §3 (window cycling) → Task 3; §4 (context menu) → Task 5; §5 (drag-to-reorder) and §6 (drag-to-open + pending assignment) → Task 6, with the pending-assignment machinery split out as Task 4 since it's independently testable pure logic the drag handler (Task 6) merely calls.
- **Type consistency:** `DockDrag`, `PendingLaunch`, `pinned_paths`/`pin_app`/`unpin_app`, `next_window`, `reorder`, `register`/`take_match` are used with the same names and signatures introduced in their originating task throughout every later task that consumes them.
- **Ordering:** Task 4 (pending-launch tracking) is placed before Task 6 (the drag handler that populates it) since Task 6 consumes its interface; Task 5 (context menu) is independent of both and could run in either order, placed after Task 4 only to keep pin/unpin (Task 5) adjacent to the reorder/drag-to-open logic (Task 6) that also touches pin state indirectly.
