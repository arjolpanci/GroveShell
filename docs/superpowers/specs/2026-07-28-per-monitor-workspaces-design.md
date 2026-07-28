# Per-monitor workspaces, overview, and hotplug

## Problem

Workspaces, the Activities overview, and workspace switching are currently
global: one `WorkspaceTracker` shared across every monitor, one overview
window sized to the whole virtual screen, and one active-workspace index for
the entire machine. Switching workspaces on one monitor moves every monitor
at once, which fights against using separate monitors for separate,
independent workflows. The overview also has a live bug from this design:
`card_layout()` and `search_layout()` always anchor to the primary monitor's
rect and DPI, so on a non-primary or differently-scaled external monitor the
search panel renders in the wrong place — sometimes off the visible card
entirely.

Separately, GroveShell does not react to monitors being connected or
disconnected while running at all; picking up a new monitor requires a
restart.

## Goal

Each monitor becomes an independent workspace context: its own pinned
workspace plus its own dynamic tail, its own Activities button and workspace
dots on its bar, and its own Activities overview scoped to only that
monitor's workspaces. Live monitor hotplug is supported so this works
without restarting GroveShell.

## Out of scope

- Session persistence across restarts (already a separate open roadmap item).
- Dragging a window between two different monitors' overview windows as a
  first-class overview interaction (physical window drag/move is covered;
  see "Cross-monitor window movement" below).
- A dedicated keybinding to send a window to a specific other monitor.

## Design

### A. Monitor identity & hotplug detection

`MonitorInfo` (`apps/ui/src/imp/monitors.rs:14`) currently carries only
`rect`, `work`, `is_primary`, `dpi` — no stable identity, since monitors are
only ever ordered by `rect.left`. That's insufficient once monitors can be
added/removed at runtime: plugging in a monitor to the left of an existing
one would otherwise silently swap which workspace set belongs to which
physical screen.

- Add `device_name: String` to `MonitorInfo`, captured via `GetMonitorInfoW`
  using the `MONITORINFOEXW` variant (`szDevice`, e.g. `\\.\DISPLAY1`)
  instead of the plain `MONITORINFO` used today.
- Add a `WM_DISPLAYCHANGE` handler on the primary bar's window procedure
  (that HWND already lives for the app's whole lifetime) that triggers a
  reconciliation pass: re-run `enumerate_monitors()`, diff the result by
  `device_name` against the currently tracked set.
  - A new `device_name` creates that monitor's bar, workspace tracker, and
    AppBar registration (its overview window is created lazily on first
    use, same as at startup).
  - A missing `device_name` tears that monitor down (see "Monitor
    unplug/replug" below).

### B. Per-monitor workspace data model

`WorkspaceTracker` (`crates/window-model/src/workspace.rs`) already supports
"1 pinned workspace + a dynamic tail" as a special case of its general
model — today it's just always invoked with `monitor_count` = the full
monitor count. The crate itself needs no changes; only how `apps/ui` uses it
changes.

- Replace the single `STATE.workspaces: WorkspaceTracker` field
  (`apps/ui/src/imp/state.rs:93`) with a new wrapper type owned by
  `apps/ui` (not the crate):
  ```rust
  struct MonitorWorkspaces {
      trackers: HashMap<String, WorkspaceTracker>, // keyed by device_name
  }
  ```
- Each monitor's tracker is created via
  `WorkspaceTracker::with_monitor_workspaces(1, 0)` — one pinned workspace
  plus its own independent dynamic tail, fully decoupled from every other
  monitor's tracker.
- `MonitorWorkspaces` methods: `tracker_for(&self, device_name)` /
  `tracker_for_mut`, and `monitor_of_window(&self, hwnd) -> Option<&str>`
  (scans all trackers — needed for hotkey routing and unplug reassignment).
- `apps/ui/src/imp/workspaces.rs`'s switch/send-window functions take a
  `device_name` parameter instead of implicitly touching one global tracker.

### C. Bar changes

Every monitor's bar becomes fully functional for its own workspace context,
not just chrome on non-primary monitors:

- `Role::Bar { is_primary }` gains a `monitor: String` (device name), so
  each bar's window procedure knows which tracker to read.
- The Activities button on every bar opens that monitor's own overview
  window (see below), not a single global one.
- Workspace dots render on every bar now, sourced from that monitor's own
  tracker (`workspace_ids().len()`, `current_index()`) instead of today's
  primary-only read of the single global tracker
  (`apps/ui/src/imp/bar.rs:178-202`).
- Clock and the Quick Settings status pill (Wi-Fi/volume/battery) stay
  primary-only — those reflect machine-wide state, not per-monitor state.
  No change needed beyond the existing `is_primary` gate.

### D. Overview: one per monitor

Today there is a single overview `HWND` sized to the full virtual screen
(`apps/ui/src/imp/mod.rs:253`), with `card_layout()` and `search_layout()`
(`apps/ui/src/imp/overview.rs:266`, `:1295`) always anchoring to
`monitors.iter().find(|m| m.is_primary)` regardless of which monitor's
workspace is actually being shown. That's the direct cause of the search bar
being invisible on a non-primary external monitor.

- Each monitor gets its own overview `HWND`, sized to that monitor's rect
  only (not the virtual screen), created lazily on first open (hot corner
  or Activities click) and reused after that.
- `card_layout()` / `search_layout()` stop hardcoding the primary monitor
  and instead use the specific monitor this overview instance belongs to,
  for both position and DPI. This fixes the visibility/DPI bug as a side
  effect of the redesign rather than as a separate patch.
- The carousel inside one monitor's overview only pages through that
  monitor's own workspace list. Dragging a window preview onto another card
  still works exactly as today, but only within the same monitor's
  overview.
- Each monitor's overview opens/closes independently of every other
  monitor's.

### E. Hotkey routing & hot corners

- `Ctrl+Alt+Left/Right` (switch) and `Ctrl+Alt+Shift+Left/Right` (send
  window) stay registered as global hotkeys
  (`apps/ui/src/imp/mod.rs:395-396`), but the `WM_HOTKEY` handler resolves
  which monitor to act on per keypress: `GetForegroundWindow()` →
  `MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)` → match to that
  monitor's `device_name` → operate on that monitor's tracker only. Only
  that monitor's bar is invalidated/repainted for its dots.
- `check_hot_corners()` (`apps/ui/src/imp/movesize.rs:371`) already detects
  each monitor's own top-left corner geometrically (it checks
  `monitors.iter().any(...)`). The only change is the action: instead of
  always calling one global `open_overview()`, it opens the specific
  monitor's own overview window.

### F. Cross-monitor window movement & monitor unplug/replug

- **Auto-reassign on physical move**: when a window's monitor (via
  `MonitorFromWindow`) changes — detected through the existing window
  live-tracking path — it's moved into the new monitor's currently active
  workspace and removed from its old monitor's tracker.
- **Monitor unplug**: on a `WM_DISPLAYCHANGE` reconciliation that finds a
  tracked `device_name` missing, gather every window across all of that
  monitor's workspaces, reassign them all to the primary monitor's current
  workspace (un-parking any that were parked on a background workspace,
  since they must become visible now), then destroy that monitor's bar,
  overview (if it had been created), and tracker, and unregister its
  AppBar.
- **Monitor replug / new monitor**: create a fresh tracker
  (`with_monitor_workspaces(1, 0)`), bar, and AppBar registration; overview
  window created lazily on first use, same as any monitor at startup.

## Testing

- `crates/window-model`: no new unit tests needed for `WorkspaceTracker`
  itself (unchanged); add tests for `MonitorWorkspaces`' reassignment logic
  (`monitor_of_window`, moving a window from one tracker to another) as pure
  logic, independent of any Win32 calls.
- Manual verification (per this project's established practice — no
  automated UI testing exists): plug in a second monitor mid-session,
  confirm a bar/overview/workspace set appears for it without a restart;
  switch workspaces independently on each monitor; drag a window from one
  monitor to another and confirm it lands in the target's active workspace;
  unplug a monitor with windows on a background workspace and confirm they
  reappear, unparked, on the primary monitor.
