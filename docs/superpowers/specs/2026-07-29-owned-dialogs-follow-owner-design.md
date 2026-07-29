# Owned dialogs follow their owner window

## Problem

Win32 does not automatically move an *owned* window (`GW_OWNER` set — a
modal dialog, color picker, file-save box, etc.) when its owner moves. This
is a real, visible bug in two places GroveShell already physically
repositions windows:

- **Workspace switch** (`park_window`/`unpark_window` in
  `apps/ui/src/imp/workspaces.rs`): the owner is shoved 20000px off-screen
  (or brought back) via `SetWindowPos`, but any currently-open owned dialog
  stays exactly where it was — stranded on the wrong workspace.
- **Cross-monitor drag** (`sync_workspaces`'s monitor-mismatch detection,
  added for auto-reassignment): when a user drags a window's title bar to
  another monitor, any owned dialog stays behind on the old monitor.

Owned windows are deliberately excluded from `WorkspaceTracker`/`AppState`
bookkeeping entirely (see `window-model`'s `inspect()`) — they are not
independent top-level windows to manage, they are visual passengers of
their owner. This feature makes them behave like passengers: whatever
happens to the owner, physically, happens to them too.

## Scope

1. Owned dialogs park/unpark together with their owner during a workspace
   switch (including the same mechanism reused by hotplug's
   orphan-window unpark and shutdown-time unpark, since both already call
   `park_window`/`unpark_window`).
2. Owned dialogs are repositioned to follow when their owner is physically
   dragged to a different monitor, detected by the existing ~250ms
   window-sync tick (`sync_workspaces`).

Out of scope: owned windows never become independently tracked in
`WorkspaceTracker`, never appear in the Activities overview, and this
feature does not attempt to handle same-monitor drags (Windows' own
default behavior there is not the bug being fixed).

## Design

### 1. `owned_windows_of` in `groveshell-window-model`

A new pure Win32 read, alongside `snapshot`/`describe`/`monitors`:

```rust
pub fn owned_windows_of(owner: isize) -> Vec<isize>
```

Implementation: one `EnumWindows` pass collecting every top-level window's
`(hwnd, GW_OWNER)` pair — unfiltered by visibility, title, or tool-window
status, since a hidden or not-yet-shown owned window should still follow
its owner. From that pair list, walk transitively from `owner` (a dialog
that itself owns a color picker, etc.) using a visited-set to guard against
any pathological owner cycle, returning the flat set of every window owned
directly or transitively.

No caching, no lifecycle tracking — same style as `snapshot()`. Called
on-demand at the two integration points below.

### 2. Workspace switch: `park_window`/`unpark_window`

After moving `hwnd` itself (existing logic unchanged), loop over
`owned_windows_of(hwnd.0 as isize)` and call the same function
(`park_window`/`unpark_window`) on each. Both functions are already
idempotent (guarded by the off-screen `rect.top` check), so calling them on
an already-parked/unparked window is a harmless no-op.

Because `park_window`/`unpark_window` are the *only* functions that ever
physically move a window off/on-screen for workspace purposes — including
hotplug's orphan-window reassignment (`hotplug.rs::remove_monitor` already
calls `unpark_window` on every orphaned window) and the shutdown-time
unpark loop in `mod.rs`'s `WM_DESTROY` handler — this single change
composes correctly everywhere a window is parked or unparked, with no
other call site needing to change.

### 3. Cross-monitor drag: `sync_workspaces`'s mismatch branch

Add one new field to `AppState` (`apps/ui/src/imp/state.rs`):

```rust
/// Last-observed on-screen rect per live window, used only to compute
/// how far a window moved between sync ticks when `sync_workspaces`
/// detects it's now on a different monitor than tracked — the delta is
/// then applied to any of its owned windows (dialogs) so they follow.
window_rects: HashMap<isize, groveshell_window_model::Rect>,
```

In `sync_workspaces`'s existing mismatch arm —
`(Some(tracked_monitor), Some(real)) if tracked_monitor != real`
(`apps/ui/src/imp/workspaces.rs`) — before applying the tracker
reassignment, look up `state.window_rects.get(&window.hwnd)` for the
owner's rect as of the previous tick. If present, compute:

```rust
let dx = window.rect.left - old_rect.left;
let dy = window.rect.top - old_rect.top;
```

then for every hwnd in `owned_windows_of(window.hwnd)`, read its current
rect and reposition it by `(dx, dy)` via `SetWindowPos` (`SWP_NOSIZE |
SWP_NOZORDER | SWP_NOACTIVATE`, matching the existing park/unpark style).

At the end of every `sync_workspaces` pass, update `state.window_rects`
with every live window's current rect (insert or overwrite), and drop
entries for hwnds no longer live (mirrors the existing dead-window pruning
already happening elsewhere in the same function).

This only ever fires for a genuinely on-screen, current-workspace owner:
`monitor_index_for_center` already returns `None` for a parked window's
off-screen rect (confirmed by the existing Task 11 code), so a parked
owner can never land in the mismatch arm — there is no ambiguity between
"parked" and "actually dragged to another monitor."

### Data flow summary

```
park_window(owner)/unpark_window(owner)
    -> owned_windows_of(owner)
    -> park_window/unpark_window each (idempotent, no new logic needed)

sync_workspaces tick, per live window:
    old_rect = window_rects.get(hwnd)
    if tracked_monitor != real_monitor (Task 11's existing check):
        if let Some(old_rect) = old_rect:
            dx, dy = new_rect - old_rect
            for owned in owned_windows_of(hwnd):
                shift owned's current rect by (dx, dy)
        ... existing tracker reassignment (unchanged) ...
    window_rects[hwnd] = new_rect   // every tick, every live window
```

### Testing

The rect-delta math is extracted as a pure function —
`fn shift_rect(rect: Rect, dx: i32, dy: i32) -> Rect` — unit-tested without
Win32 (e.g. "shifting a rect by (100, -50) moves both corners by exactly
that"). The `EnumWindows`/`GW_OWNER` walk and the actual `SetWindowPos`
repositioning are live Win32 integration with no automated coverage,
consistent with this codebase's established convention for this class of
behavior (Tasks 10/11 of the per-monitor workspaces plan) — manual
verification only: open a Save-As dialog from an app, switch away from and
back to its workspace (confirm the dialog follows), then drag the owner to
another monitor (confirm the dialog follows there too).
