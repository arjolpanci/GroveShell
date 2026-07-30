# Interactive dock: GNOME-dash-style pin/unpin, reorder, drag-to-open

## Problem

The Activities overview's dock (`imp/dock.rs`) currently just mirrors the
real Windows taskbar's pinned-shortcut folder read-only: no pin/unpin, no
reordering, no way to launch a new instance onto a specific workspace by
dragging. The user wants it to behave like GNOME's dash: right-click to
pin/unpin, drag to reorder pinned icons, and drag a pinned icon onto a
workspace card to open it there.

This document covers only that interactive-dock feature. Three smaller,
already-agreed visual fixes (moving the dock closer to the screen's bottom
edge, sourcing pinned icons from the shortcut's resolved target instead of
the `.lnk` file to drop the shell's shortcut-arrow overlay, and adding app
icons to search results) are simple enough to implement directly without a
spec and are not covered here.

## Scope

**In scope:**
- GroveShell owns its own persisted pinned-app list from now on, seeded
  once (on first run that finds no persisted list) from the real Windows
  taskbar's current pins, then fully independent of it — unpinning here
  never touches the real taskbar, and pinning here never re-pins there.
- One dock entry per app regardless of how many windows it has open;
  clicking an already-focused running app's entry again cycles to its next
  window (GNOME-style), matching today's "click focuses the first tracked
  window" behavior extended to actually cycle on repeat clicks.
- Right-click (or long-press touch equivalent — out of scope, no touch
  input exists in this codebase) on a dock icon opens a small native
  context menu: "Pin to dock" / "Unpin from dock" (whichever applies) plus
  "Open new window" (enabled only when the entry has a launch path, i.e.
  pinned entries — a running-but-unpinned entry has no shortcut to launch
  from, so it gets neither "Open new window" nor a way to pin itself
  in this pass — see Out of scope).
- Drag-to-reorder: dragging a **pinned** icon within the dock bar and
  dropping it at a different position among the pinned icons reorders the
  persisted pinned list. Running-but-unpinned entries always render after
  every pinned entry, in current tracking order, and are not reorderable.
- Drag-to-open: dragging a **pinned** icon out of the dock bar and
  dropping it on a workspace card launches that shortcut, then does a
  best-effort watch (bounded by a timeout) to assign the new process's
  next top-level window to the target workspace once it appears.

**Explicitly out of scope for this pass:**
- Pinning a running-but-unpinned app directly from its dock icon (only the
  right-click "Pin to dock" affordance on entries that already have a
  `launch_path` is covered; a running app's *own* icon has no shortcut to
  remember as a pin target, since it currently derives its icon straight
  from the window itself, not a resolvable launch command — adding this
  would mean guessing a launch command from a running process, which is
  unreliable and a separate concern from this pass's scope).
- Drag-to-reorder or drag-to-open for running-but-unpinned icons (per the
  earlier scoping conversation).
- Any settings UI to manage pins outside the dock itself.
- Multi-monitor-specific pin ordering (the pinned list is one global,
  per-user ordering, shared across every monitor's dock, same as today's
  taskbar-mirroring behavior).

## Design

### 1. Persisted pinned-list storage

A new small file, `%LOCALAPPDATA%\GroveShell\dock_pins.json`, holding an
ordered `Vec<PathBuf>` of pinned shortcut/launch-target paths. This is
**not** part of `groveshell-config`'s shared `Config` struct: that file is
user-editable settings shared (and potentially written) across multiple
processes (`host`, `cli`, future settings UI), with its own schema
versioning and its own backup-on-save behavior tuned for occasional
hand-edited changes — piggybacking frequent, UI-driven pin/unpin/reorder
writes onto it risks write contention with those other processes and
backup-file spam neither of those characteristics is designed for. A
dedicated file owned exclusively by `groveshell-ui` avoids both, at the
cost of one more small file under the shared data directory (an existing,
already-established pattern — `logs/`, and this initiative's earlier
config schema, both already live under `%LOCALAPPDATA%\GroveShell`).

Loaded once at startup (alongside the existing `taskbar_pinned_shortcuts`
call site, which becomes the one-time seed source): if `dock_pins.json`
doesn't exist yet, seed it from `taskbar_pinned_shortcuts()`'s current
result and save it immediately, so a fresh install still shows the user's
existing real-taskbar pins on first run. If it exists, load it and never
consult the real taskbar's pin folder again.

Saved (full rewrite, small file, no backup-on-save needed given how
small/low-stakes this data is compared to `groveshell-config`'s settings)
on every pin, unpin, and completed reorder-drag drop — never on every
drag-move tick, only on drop.

### 2. `build_dock_apps` sourcing change

`build_dock_apps` (`imp/dock.rs`) currently calls
`taskbar_pinned_shortcuts()` directly as its pinned-entry source. It
changes to take the persisted pinned list (an in-memory `Vec<PathBuf>`,
loaded once at startup into a `thread_local!`, same caching shape as the
existing `PINNED_ICON_CACHE`) as its pinned-entry source instead — the
rest of its logic (resolving each shortcut's target for window-matching,
building each entry's `DockApp`, appending unclaimed running apps after
the pinned ones) is unchanged.

### 3. Cycling through an app's windows on repeat click

`activate_dock_app` currently always focuses `app.windows.first()`. It
changes to track, per dock entry, which of its windows was last focused by
a dock click — a small `HashMap<String, isize>` cache keyed by the
entry's lowercased exe name (already computed by `build_dock_apps` for
grouping windows into an entry, for both pinned and running-unpinned
entries) mapping to the last-focused window's `hwnd`, cleared whenever the
dock rebuilds since the underlying window list can change. On each click:
find the clicked entry's `hwnd` in its own `windows` list, focus the next
one after it (wrapping around to the first), and record that as the new
last-focused `hwnd` for that exe key; if there's no cached last-focused
`hwnd` yet, or it's no longer in `windows`, fall back to
`windows.first()`.

### 4. Right-click context menu

A dock-icon right-click (`WM_RBUTTONDOWN` on the overview window, hit-
tested against the same `dock_layout` slot rects hover/click already use)
opens a native `TrackPopupMenu` at the cursor, built fresh each time from
the target entry's current state:

- If the entry has a `launch_path` (pinned): "Unpin from dock", "Open new
  window".
- If the entry has no `launch_path` (running-but-unpinned): no menu items
  are offered in this pass (see Out of scope) — right-clicking such an
  icon is a no-op, same as it is today.

Selecting "Unpin from dock" removes that path from the persisted pinned
list and saves it, then rebuilds the dock (same rebuild path
`rebuild_open_overview_pages`/`build_dock_apps` already use elsewhere).
Selecting "Open new window" calls the same `ShellExecuteW` launch path
`activate_dock_app` already uses for a cold launch, regardless of whether
the app is currently running.

### 5. Drag-to-reorder

A new drag state, `DockDrag { start_index: usize, cur_x: i32, cur_y: i32 }`,
started on a press over a **pinned** dock icon (mirroring
`WindowDrag`'s existing start/move/end shape and click-vs-drag threshold).
While active, the icon's ghost follows the cursor (reusing the existing
window-drag ghost rendering machinery — ghost ownership just switches to
"whichever kind of drag is active," same branching `paint_root`/GDI
`paint_overview` already do today for `window_pop_anim`/`window_drag`).

On release:
- If still within the dock bar's rect: reorders the persisted pinned list
  so the dragged entry lands at the dropped-at index among pinned entries
  (running-but-unpinned entries are never included in this reorder, and
  the dock rebuilds immediately after).
- If released on a workspace card (outside the dock bar, within a card's
  rect): treated as drag-to-open instead (§6) — the two are
  distinguished purely by where the pointer released, not by anything
  decided at drag-start time.
- If released anywhere else (empty space, outside any card): cancelled,
  same as a cancelled window-drag today — nothing changes.

### 6. Drag-to-open with best-effort workspace assignment

On a pinned-icon drag ending over a workspace card: launch that pin's
`launch_path` via `ShellExecuteExW` (not the simpler `ShellExecuteW`
already used elsewhere — `ShellExecuteExW` with `SEE_MASK_NOCLOSEPROCESS`
is the one variant that hands back a process handle, needed to identify
*which* process's window to watch for). Record a pending assignment —
`{ process_id: u32, target_workspace_index: usize, monitor: String,
expires_at: Instant }` — in a small `Vec` on `AppState` (bounded: a
fixed, generous timeout, e.g. 15 seconds, covers slow-launching apps
without leaking indefinitely if a launch never produces a matching
window).

The existing debounced window-sync path (`on_window_sync_timer`, which
already runs on every `EVENT_OBJECT_CREATE` burst) gains one more step:
for each newly-seen window (one not in the tracker before this sync),
check it against the pending-assignments list by matching
`GetWindowThreadProcessId` against `process_id`; on a match, call
`tracker.assign_to_index(hwnd, target_workspace_index)` for the matching
monitor's tracker *before* whatever default-assignment the sync would
otherwise have applied, then remove that pending entry. Expired entries
(past `expires_at`) are dropped the next time this check runs, without
needing their own timer.

## Testing

- Pure logic gets real unit tests: the pinned-list reorder computation
  (given a list and a from/to index, produce the reordered list), the
  "next window after last-focused, wrapping, falling back to first"
  cycling logic, and the pending-assignment expiry/match logic (given a
  set of pending assignments and a "newly seen window's process id,"
  determine which one it matches, if any, and that expired ones are
  correctly ignored) are all pure functions over plain data, independent
  of any Win32 call, and are tested exactly like `shift_rect`/
  `owned_windows_from_pairs` were in earlier phases of this project.
- Everything else (the actual context menu, the drag ghost, real
  `ShellExecuteExW`/window-creation-detection integration) is manual-
  verification-only, consistent with the rest of this codebase's Win32-
  integration testing convention. Manual verification checklist: pin/
  unpin persists across a restart; a fresh profile (no `dock_pins.json`)
  seeds correctly from the real taskbar once and never re-reads it after;
  repeat-clicking a multi-window app's dock icon cycles through its
  windows; dragging a pinned icon to reorder it persists after a rebuild;
  dragging a pinned icon onto a different workspace card launches the app
  and its window lands on that workspace once it appears; a drag dropped
  on empty space cancels cleanly; the pending-assignment timeout doesn't
  leak or misfire if the launched app never produces a matching window
  within 15 seconds.
