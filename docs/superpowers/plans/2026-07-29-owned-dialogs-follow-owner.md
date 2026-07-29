# Owned Dialogs Follow Their Owner Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When an owner window is parked/unparked during a workspace switch, or physically dragged to a different monitor, any of its owned windows (modal dialogs, color pickers, etc.) move right along with it instead of being left behind.

**Architecture:** A new pure Win32 read (`owned_windows_of`) in `groveshell-window-model` finds every window owned, directly or transitively, by a given hwnd. Two existing GroveShell mechanisms that already physically move windows — `park_window`/`unpark_window` (workspace switch) and `sync_workspaces`'s monitor-mismatch detection (cross-monitor drag) — call it and apply the same treatment to whatever it finds.

**Tech Stack:** Rust, `windows` crate (Win32), existing `groveshell-window-model` and `groveshell-ui` crates.

## Global Constraints

- Owned windows never become independently tracked in `WorkspaceTracker` or `AppState` — they are purely visual passengers of their owner, per `docs/superpowers/specs/2026-07-29-owned-dialogs-follow-owner-design.md`.
- `owned_windows_of` returns every window owned directly or transitively (an owned dialog that itself owns a further window), unfiltered by visibility/title/tool-window status.
- The cross-monitor-drag follow logic only ever fires from `sync_workspaces`'s existing monitor-mismatch arm (`tracked_monitor != real`) — never for same-monitor moves, and never for a parked (off-screen) owner, since `monitor_index_for_center` already returns `None` for a parked rect.
- Reposition strategy for the cross-monitor case is "preserve relative offset": shift every owned window by the exact same (dx, dy) the owner itself moved between sync ticks, not re-centering.
- Pure logic (the transitive owner-walk, the rect-shift math) is unit-tested without Win32; the actual `EnumWindows`/`GetWindowRect`/`SetWindowPos` calls are live Win32 integration with no automated test, consistent with this codebase's established convention for this class of behavior — manual verification only.

---

### Task 1: `owned_windows_of` in `groveshell-window-model`

**Files:**
- Modify: `crates/window-model/src/lib.rs` (append near the end, after `exe_name_for_pid`)

**Interfaces:**
- Produces: `pub fn owned_windows_of(owner: isize) -> Vec<isize>` — every window owned by `owner`, directly or transitively, unfiltered by visibility/title/tool-window status.

- [ ] **Step 1: Write the failing tests for the pure transitive walk**

Add to `crates/window-model/src/lib.rs`, after `exe_name_for_pid`:

```rust
#[cfg(test)]
mod owned_windows_tests {
    use super::owned_windows_from_pairs;

    #[test]
    fn finds_direct_and_transitive_owned_windows() {
        // 1 owns 2; 2 owns 3 (transitive); 4 is owned by an unrelated
        // window (99) and must not appear.
        let pairs = vec![(2, 1), (3, 2), (4, 99)];
        let mut owned = owned_windows_from_pairs(1, &pairs);
        owned.sort();
        assert_eq!(owned, vec![2, 3]);
    }

    #[test]
    fn returns_empty_when_nothing_is_owned() {
        let pairs = vec![(2, 1)];
        assert_eq!(owned_windows_from_pairs(5, &pairs), Vec::<isize>::new());
    }

    #[test]
    fn an_owner_cycle_does_not_infinite_loop() {
        // Pathological: 1 owns 2 and 2 "owns" 1 back. Must terminate and
        // must not include the root itself in its own owned set.
        let pairs = vec![(1, 2), (2, 1)];
        let mut owned = owned_windows_from_pairs(1, &pairs);
        owned.sort();
        assert_eq!(owned, vec![2]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p groveshell-window-model owned_windows_tests`
Expected: FAIL with "cannot find function `owned_windows_from_pairs`" — it doesn't exist yet.

- [ ] **Step 3: Implement the pure walk and the Win32 wrapper**

Add to `crates/window-model/src/lib.rs`, right before the `owned_windows_tests` module you just added:

```rust
/// Returns every window (top-level or itself owned) that `owner` owns,
/// directly or transitively — e.g. a dialog that itself owns a color
/// picker — unfiltered by visibility, title, or tool-window status, since
/// a not-yet-shown or hidden owned window should still follow its owner.
/// Win32 does not move an owned window when its owner moves (unlike a
/// true child window), so callers use this to carry owned dialogs along
/// during a workspace switch or a cross-monitor drag.
pub fn owned_windows_of(owner: isize) -> Vec<isize> {
    let mut pairs: Vec<(isize, isize)> = Vec::new();
    // SAFETY: `pairs` is a local `Vec` whose address is passed through as
    // `lparam` and only ever read back by `enum_owner_pairs_proc` during
    // this call; `EnumWindows` is synchronous, so `pairs` is guaranteed to
    // outlive every callback invocation.
    unsafe {
        let _ = EnumWindows(
            Some(enum_owner_pairs_proc),
            LPARAM(&mut pairs as *mut Vec<(isize, isize)> as isize),
        );
    }
    owned_windows_from_pairs(owner, &pairs)
}

unsafe extern "system" fn enum_owner_pairs_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` was created from a live `&mut Vec<(isize, isize)>`
    // in `owned_windows_of` and this callback only runs synchronously
    // within that call's lifetime.
    let pairs = &mut *(lparam.0 as *mut Vec<(isize, isize)>);
    let owner = GetWindow(hwnd, GW_OWNER).map(|o| o.0 as isize).unwrap_or(0);
    if owner != 0 {
        pairs.push((hwnd.0 as isize, owner));
    }
    TRUE
}

/// Pure transitive walk: every hwnd in `pairs` whose owner is `root`,
/// directly or through a chain, using a visited set so a pathological
/// owner cycle can't loop forever or include `root` in its own result.
fn owned_windows_from_pairs(root: isize, pairs: &[(isize, isize)]) -> Vec<isize> {
    let mut result = Vec::new();
    let mut frontier = vec![root];
    let mut visited = std::collections::HashSet::new();
    visited.insert(root);
    while let Some(current) = frontier.pop() {
        for &(hwnd, owner) in pairs {
            if owner == current && visited.insert(hwnd) {
                result.push(hwnd);
                frontier.push(hwnd);
            }
        }
    }
    result
}
```

`EnumWindows`, `GetWindow`, `GW_OWNER`, `HWND`, `LPARAM`, `BOOL`, `TRUE` are all already imported at the top of this file (used by `snapshot`'s `enum_proc`) — no new imports needed.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p groveshell-window-model owned_windows_tests`
Expected: PASS, 3/3.

- [ ] **Step 5: Commit**

```bash
git add crates/window-model/src/lib.rs
git commit -m "feat: add owned_windows_of, finding a window's owned dialogs transitively"
```

---

### Task 2: `AppState.window_rects` field and the `shift_rect` pure helper

**Files:**
- Modify: `apps/ui/src/imp/state.rs` (add field to `AppState`)
- Modify: `apps/ui/src/imp/mod.rs:372-387` (`AppState` construction)
- Modify: `apps/ui/src/imp/workspaces.rs` (add `shift_rect` + its test module)

**Interfaces:**
- Consumes: `groveshell_window_model::Rect` (already `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, defined in `crates/window-model/src/lib.rs`)
- Produces: `AppState.window_rects: HashMap<isize, groveshell_window_model::Rect>` (Task 4 reads/writes it); `fn shift_rect(rect: groveshell_window_model::Rect, dx: i32, dy: i32) -> groveshell_window_model::Rect` (Task 4 calls it)

- [ ] **Step 1: Add the `window_rects` field to `AppState`**

In `apps/ui/src/imp/state.rs`, add near the top of the file (alongside the other `use` lines):

```rust
use groveshell_window_model::Rect;
```

Then add a field to the `AppState` struct (after `window_registry: WindowRegistry,`):

```rust
    /// Last-observed on-screen rect per live window, updated every
    /// `sync_workspaces` tick. Used only to compute how far a window
    /// moved since the previous tick when a monitor mismatch is detected,
    /// so the same delta can be applied to any of its owned windows
    /// (dialogs) — see `workspaces::sync_workspaces`.
    pub(crate) window_rects: std::collections::HashMap<isize, Rect>,
```

- [ ] **Step 2: Initialize the field where `AppState` is constructed**

In `apps/ui/src/imp/mod.rs`, find the `AppState { ... }` construction (around line 372) and add the new field, e.g. right after `window_registry: WindowRegistry::new(),`:

```rust
                window_registry: WindowRegistry::new(),
                window_rects: std::collections::HashMap::new(),
```

- [ ] **Step 3: Run the build to confirm the new field compiles**

Run: `cargo build -p groveshell-ui 2>&1 | tail -30`
Expected: clean (the field is unused so far — Task 4 reads/writes it — so this just confirms the struct literal and field declaration match).

- [ ] **Step 4: Write the failing test for `shift_rect`**

Add to `apps/ui/src/imp/workspaces.rs`, at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::shift_rect;
    use groveshell_window_model::Rect;

    #[test]
    fn shift_rect_moves_every_corner_by_the_same_delta() {
        let rect = Rect { left: 100, top: 200, right: 300, bottom: 400 };
        let shifted = shift_rect(rect, 50, -20);
        assert_eq!(shifted, Rect { left: 150, top: 180, right: 350, bottom: 380 });
    }
}
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p groveshell-ui shift_rect`
Expected: FAIL with "cannot find function `shift_rect`" — it doesn't exist yet.

- [ ] **Step 6: Implement `shift_rect`**

Add to `apps/ui/src/imp/workspaces.rs`, just above the `#[cfg(test)] mod tests` block you just added:

```rust
/// Shifts every corner of `rect` by `(dx, dy)` — used to translate an
/// owned window's current position by exactly the delta its owner moved
/// between two `sync_workspaces` ticks, preserving their relative offset.
fn shift_rect(rect: groveshell_window_model::Rect, dx: i32, dy: i32) -> groveshell_window_model::Rect {
    groveshell_window_model::Rect {
        left: rect.left + dx,
        top: rect.top + dy,
        right: rect.right + dx,
        bottom: rect.bottom + dy,
    }
}
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p groveshell-ui shift_rect`
Expected: PASS, 1/1.

- [ ] **Step 8: Commit**

```bash
git add apps/ui/src/imp/state.rs apps/ui/src/imp/mod.rs apps/ui/src/imp/workspaces.rs
git commit -m "feat: add AppState.window_rects and the shift_rect helper for owned-dialog following"
```

---

### Task 3: Workspace switch — `park_window`/`unpark_window` carry owned windows along

**Files:**
- Modify: `apps/ui/src/imp/workspaces.rs:59-113` (`park_window`, `unpark_window`)

**Interfaces:**
- Consumes: `groveshell_window_model::owned_windows_of` (Task 1)

- [ ] **Step 1: Modify `park_window` to also park every owned window**

In `apps/ui/src/imp/workspaces.rs`, change `park_window` from:

```rust
pub(crate) fn park_window(hwnd: HWND) {
    // SAFETY: `hwnd` was tracked as a real window; if it has since
    // closed every call here documented-fails harmlessly.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top >= WORKSPACE_PARK_DY / 2 {
            return;
        }
        capture_window_snapshot(hwnd);
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top + WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}
```

to:

```rust
pub(crate) fn park_window(hwnd: HWND) {
    // SAFETY: `hwnd` was tracked as a real window; if it has since
    // closed every call here documented-fails harmlessly.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top >= WORKSPACE_PARK_DY / 2 {
            return;
        }
        capture_window_snapshot(hwnd);
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top + WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // Win32 does not move an owned window (a dialog, color picker, etc.)
    // when its owner moves — carry every currently-live owned window
    // along so it doesn't strand on the wrong workspace. Recursing
    // through `park_window` itself (rather than inlining the move) means
    // a multi-level owner chain (a dialog that owns a further dialog)
    // parks correctly too; each recursive call's own early-return guards
    // make this cheap and safe even though `owned_windows_of` already
    // returns the full transitive set.
    for owned in groveshell_window_model::owned_windows_of(hwnd.0 as isize) {
        park_window(HWND(owned as *mut c_void));
    }
}
```

- [ ] **Step 2: Modify `unpark_window` the same way**

Change `unpark_window` from:

```rust
pub(crate) fn unpark_window(hwnd: HWND) {
    drop_window_snapshot(hwnd.0 as isize);
    // SAFETY: same as `park_window`.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top < WORKSPACE_PARK_DY / 2 {
            // Not parked — e.g. it was minimized (and hidden) at park
            // time; make sure it's shown either way.
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top - WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}
```

to:

```rust
pub(crate) fn unpark_window(hwnd: HWND) {
    drop_window_snapshot(hwnd.0 as isize);
    // SAFETY: same as `park_window`.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() || rect.top < WORKSPACE_PARK_DY / 2 {
            // Not parked — e.g. it was minimized (and hidden) at park
            // time; make sure it's shown either way.
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            rect.left,
            rect.top - WORKSPACE_PARK_DY,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // See `park_window` — carry every owned window back too.
    for owned in groveshell_window_model::owned_windows_of(hwnd.0 as isize) {
        unpark_window(HWND(owned as *mut c_void));
    }
}
```

Note: `unpark_window` recursing means `drop_window_snapshot` also runs for each owned hwnd — this is correct and harmless (owned windows are never captured by `capture_window_snapshot` independently, since only `park_window`'s own call on `hwnd` — now also invoked recursively for owned windows — captures one; `drop_window_snapshot` on an hwnd with no snapshot is a documented no-op elsewhere in this codebase's convention).

Because `park_window`/`unpark_window` are the only functions that ever physically move a window for workspace purposes — including hotplug's orphan-window reassignment (`hotplug.rs::remove_monitor`) and the shutdown-time unpark loop in `mod.rs`'s `WM_DESTROY` handler — this one change is sufficient; no other call site needs to change.

- [ ] **Step 3: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -30`
Expected: clean.

- [ ] **Step 4: Run the existing test suite**

Run: `cargo test -p groveshell-ui`
Expected: all existing tests still pass (this task adds no new automated tests of its own — the transitive-walk logic it relies on was already unit-tested in Task 1; the live `SetWindowPos` behavior is Win32 integration with no automated coverage, consistent with this codebase's convention).

- [ ] **Step 5: Manual verification**

Run `.\scripts\dev-start.ps1`. Open an app that shows an owned dialog (e.g. Notepad → File → Save As, or any app with a modal "About" box). With the dialog open, switch to a different workspace on that monitor (`Ctrl+Alt+→`), then switch back (`Ctrl+Alt+←`). Confirm the dialog disappears and reappears together with its owner, rather than staying visible on the wrong workspace or disappearing independently.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/workspaces.rs
git commit -m "feat: park_window/unpark_window carry owned dialogs along with their owner"
```

---

### Task 4: Cross-monitor drag — owned dialogs follow via `sync_workspaces`

**Files:**
- Modify: `apps/ui/src/imp/workspaces.rs:410-472` (`sync_workspaces`)

**Interfaces:**
- Consumes: `groveshell_window_model::owned_windows_of` (Task 1), `shift_rect` (Task 2), `AppState.window_rects` (Task 2)

- [ ] **Step 1: Apply the delta to owned windows in the monitor-mismatch arm, and maintain `window_rects`**

In `apps/ui/src/imp/workspaces.rs`, `sync_workspaces`'s `match` currently reads:

```rust
                match (state.workspaces.monitor_of_window(window.hwnd), real_monitor) {
                    (Some(tracked_monitor), Some(real)) if tracked_monitor != real => {
                        // Physically dragged to a different monitor since the last
                        // sync — move it onto the new monitor's *current* workspace
                        // rather than leaving it assigned to a monitor it's no
                        // longer on.
                        if let Some(t) = state.workspaces.get_mut(&tracked_monitor) {
                            t.forget(window.hwnd);
                        }
                        if let Some(t) = state.workspaces.get_mut(&real) {
                            let index = t.current_index();
                            t.assign_to_index(window.hwnd, index);
                        }
                    }
                    (None, real) => {
                        let target_monitor = real.unwrap_or_else(|| state.primary_monitor.clone());
                        if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                            let index = tracker.current_index();
                            tracker.assign_to_index(window.hwnd, index);
                        }
                    }
                    _ => {}
                }
```

Change the `(Some(tracked_monitor), Some(real)) if tracked_monitor != real` arm to also carry along any owned windows, using the rect recorded on the *previous* tick:

```rust
                match (state.workspaces.monitor_of_window(window.hwnd), real_monitor) {
                    (Some(tracked_monitor), Some(real)) if tracked_monitor != real => {
                        // Win32 does not move an owned window (a dialog,
                        // color picker, etc.) when its owner is dragged
                        // to another monitor by its title bar — shift
                        // every currently-owned window by exactly the
                        // delta the owner itself moved since the last
                        // sync tick, preserving their relative offset.
                        if let Some(&old_rect) = state.window_rects.get(&window.hwnd) {
                            let dx = window.rect.left - old_rect.left;
                            let dy = window.rect.top - old_rect.top;
                            if dx != 0 || dy != 0 {
                                for owned in groveshell_window_model::owned_windows_of(window.hwnd) {
                                    // SAFETY: `owned` came from a live
                                    // `EnumWindows` pass moments ago; if
                                    // it's since closed, both calls below
                                    // documented-fail harmlessly.
                                    unsafe {
                                        let owned_hwnd = HWND(owned as *mut c_void);
                                        let mut owned_raw = windows::Win32::Foundation::RECT::default();
                                        if GetWindowRect(owned_hwnd, &mut owned_raw).is_err() {
                                            continue;
                                        }
                                        let shifted = shift_rect(
                                            groveshell_window_model::Rect::from(owned_raw),
                                            dx,
                                            dy,
                                        );
                                        let _ = SetWindowPos(
                                            owned_hwnd,
                                            HWND(std::ptr::null_mut()),
                                            shifted.left,
                                            shifted.top,
                                            0,
                                            0,
                                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                                        );
                                    }
                                }
                            }
                        }
                        // Physically dragged to a different monitor since the last
                        // sync — move it onto the new monitor's *current* workspace
                        // rather than leaving it assigned to a monitor it's no
                        // longer on.
                        if let Some(t) = state.workspaces.get_mut(&tracked_monitor) {
                            t.forget(window.hwnd);
                        }
                        if let Some(t) = state.workspaces.get_mut(&real) {
                            let index = t.current_index();
                            t.assign_to_index(window.hwnd, index);
                        }
                    }
                    (None, real) => {
                        let target_monitor = real.unwrap_or_else(|| state.primary_monitor.clone());
                        if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                            let index = tracker.current_index();
                            tracker.assign_to_index(window.hwnd, index);
                        }
                    }
                    _ => {}
                }
                state.window_rects.insert(window.hwnd, window.rect);
```

The `state.window_rects.insert(window.hwnd, window.rect);` line runs for every live window, every tick, right after the `match` (still inside the same `for window in &live` loop) — this is what gives the *next* tick something to diff against.

- [ ] **Step 2: Prune `window_rects` for windows that are no longer alive**

Immediately below the existing pruning block in the same function:

```rust
            for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
                if let Some(tracker) = state.workspaces.get_mut(&name) {
                    tracker.prune(groveshell_window_model::is_alive);
                }
            }
            state.window_registry.prune(groveshell_window_model::is_alive);
```

add:

```rust
            state.window_rects.retain(|&hwnd, _| groveshell_window_model::is_alive(hwnd));
```

- [ ] **Step 3: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -30`
Expected: clean.

- [ ] **Step 4: Run the existing test suite**

Run: `cargo test -p groveshell-ui`
Expected: all existing tests still pass, including `shift_rect`'s test from Task 2 (the logic under test here — `owned_windows_of`'s transitive walk and `shift_rect`'s math — was already unit-tested; the live `GetWindowRect`/`SetWindowPos` calls and the tick-to-tick `window_rects` diffing are Win32 integration behavior with no automated coverage, consistent with this codebase's convention for this class of behavior).

- [ ] **Step 5: Manual verification**

With two monitors connected, run `.\scripts\dev-start.ps1`. Open an app with an owned dialog visible (e.g. Notepad's "About Notepad", which stays open without blocking). Drag the owner window's title bar from monitor A to monitor B. Within about a quarter second (the existing `SYNC_DEBOUNCE_MS`), confirm the dialog also moves to monitor B, landing at the same position relative to its owner that it had before the drag.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/workspaces.rs
git commit -m "feat: owned dialogs follow their owner across a cross-monitor drag"
```

---

### Task 5: Documentation and final verification

**Files:**
- Modify: `README.md:127-137` (Phase 3 roadmap)

**Interfaces:** none — verification and docs only.

- [ ] **Step 1: Flip the Phase 3 checkbox**

In `README.md`, change:

```markdown
- [ ] Owned dialogs following their owner window
```

to:

```markdown
- [x] Owned dialogs following their owner window: carried along by
  `park_window`/`unpark_window` during a workspace switch, and
  repositioned to preserve their relative offset when their owner is
  dragged to a different monitor
```

- [ ] **Step 2: Full workspace build**

Run: `cargo build --workspace 2>&1 | tail -60`
Expected: clean, no new warnings introduced by this feature.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace --no-fail-fast 2>&1 | tail -100`
Expected: all tests pass — the pre-existing `workspace.rs`/`monitor_workspaces.rs`/`monitors.rs` suites unchanged, plus this plan's new tests (`owned_windows_tests` in `window-model`, `shift_rect`'s test in `groveshell-ui`). The two pre-existing `groveshell-config` `PermissionDenied` test failures are a known environment issue predating this branch (verified via `git stash` against unmodified `main` in an earlier session) — not a regression, do not treat as a blocker.

- [ ] **Step 4: Clippy**

Run: `cargo clippy -p groveshell-ui --no-deps 2>&1 | tail -60`
Run: `cargo clippy -p groveshell-window-model --no-deps 2>&1 | tail -60`
Expected: no new warnings beyond whatever pre-existing lints already exist on `main` before this branch.

- [ ] **Step 5: Combined manual smoke test**

Run `.\scripts\dev-start.ps1` with two monitors connected. Repeat both manual checks from Tasks 3 and 4 back to back on the same dialog: open a dialog, switch workspaces on its monitor and confirm it follows, then drag the owner to the other monitor and confirm the dialog follows there too, ending up at the same relative offset it started with.

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs: mark owned dialogs following their owner as done in the Phase 3 roadmap"
```

(If Step 5's smoke test surfaces any fixes, make them, re-run Steps 2-4, and commit the fix separately before this docs commit.)
