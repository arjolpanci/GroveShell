# Per-monitor workspaces, overview, and hotplug — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each monitor an independent workspace context — its own pinned
workspace + dynamic tail, its own Activities button and workspace dots on its
bar, its own Activities overview scoped only to its own workspaces — with
live monitor hotplug support, replacing today's single global workspace
tracker and virtual-screen-wide overview.

**Architecture:** Key monitors by a stable device name (`MONITORINFOEXW.szDevice`)
instead of position. Replace the single `WorkspaceTracker` with a
`MonitorWorkspaces` map keyed by that device name, each entry an independent
tracker (1 pinned + its own dynamic tail). Replace the single overview window
with one `HWND` per monitor, each with its own `OverviewInstance` holding
everything that today lives flat on `AppState` (mode, carousel state, dock,
search). `Role::Overview`/`Role::Bar` carry the owning monitor's device name
so `wndproc` can resolve "which monitor" per message. A `WM_DISPLAYCHANGE`
handler reconciles the monitor set live: new monitors get a tracker/bar/
overview, removed ones get torn down with their windows reassigned to the
primary monitor.

**Tech Stack:** Rust, `windows` crate (Win32 GDI/AppBar/HiDPI/Accessibility),
existing `groveshell-window-model` crate (`WorkspaceTracker` unchanged).

## Global Constraints

- No changes to `crates/window-model/src/workspace.rs`'s public API — its
  existing "1 pinned + dynamic tail" shape (`with_monitor_workspaces(1, 0)`)
  already covers the per-monitor case.
- Monitor identity is the device name string (e.g. `\\.\DISPLAY1`) from
  `MONITORINFOEXW`, not `HMONITOR` (which isn't guaranteed stable across
  hotplug) and not position (which changes when a monitor is added/removed).
- Session persistence across restarts stays explicitly out of scope (already
  a separate open roadmap item) — nothing here needs a *persisted* monitor
  identity, only a stable one within a single running session.
- Clock and the Quick Settings status pill (Wi-Fi/volume/battery) remain
  primary-bar-only; every bar's Activities button and workspace dots become
  per-monitor.
- Follow this codebase's existing conventions throughout: `unsafe` blocks
  carry a `// SAFETY:` comment explaining the precondition; 96-DPI layout
  constants are converted via `scaled(v, dpi)` using the *specific monitor's*
  DPI, never a global/primary one, for anything drawn on that monitor.

---

### Task 1: Stable monitor identity (`device_name`)

**Files:**
- Modify: `apps/ui/src/imp/monitors.rs`
- Test: `apps/ui/src/imp/monitors.rs` (inline `#[cfg(test)]` module — this
  file has none yet; add one)

**Interfaces:**
- Produces: `MonitorInfo.device_name: String`, `monitor_key_of_window(hwnd: HWND) -> Option<String>`,
  `monitor_key_at_point(pt: POINT) -> Option<String>`, `monitor_by_device_name<'a>(monitors: &'a [MonitorInfo], device_name: &str) -> Option<&'a MonitorInfo>`

- [ ] **Step 1: Switch to `MONITORINFOEXW` and add `device_name` to `MonitorInfo`**

Edit `apps/ui/src/imp/monitors.rs`. Replace the `MONITORINFO` import/usage
with `MONITORINFOEXW`, and add the field:

```rust
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
```

```rust
#[derive(Clone)]
pub(crate) struct MonitorInfo {
    pub(crate) rect: RECT,
    pub(crate) work: RECT,
    pub(crate) is_primary: bool,
    pub(crate) dpi: u32,
    /// Stable identity for this monitor within the current session (e.g.
    /// `\\.\DISPLAY1`), from `MONITORINFOEXW::szDevice`. Used to key
    /// per-monitor workspace/overview state instead of `HMONITOR` (not
    /// guaranteed stable across hotplug) or screen position (changes
    /// when a monitor is added/removed to the left of another).
    pub(crate) device_name: String,
}
```

`MonitorInfo` drops its `Copy` derive (`String` isn't `Copy`) — `Clone` only.
Fix the one call site that relied on `Copy`: `monitors_sorted_by_x()` already
takes ownership from `enumerate_monitors()` so `.clone()` isn't needed there,
but check every other caller in later tasks for an implicit copy (there are
none yet outside this file — `MonitorInfo` is only otherwise read by value in
loops that already iterate `&Vec<MonitorInfo>`).

- [ ] **Step 2: Capture `szDevice` in `monitor_enum_proc`**

```rust
unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

    let mut info = MONITORINFOEXW {
        monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let device_name = String::from_utf16_lossy(
            &info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len())],
        );
        monitors.push(MonitorInfo {
            rect: info.monitorInfo.rcMonitor,
            work: info.monitorInfo.rcWork,
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            dpi: dpi_x.max(96),
            device_name,
        });
    }
    TRUE
}
```

Update the empty-enumeration fallback in `enumerate_monitors()` to supply a
synthetic device name:

```rust
monitors.push(MonitorInfo {
    rect,
    work: rect,
    is_primary: true,
    dpi: 96,
    device_name: "\\\\.\\DISPLAY-FALLBACK".to_string(),
});
```

- [ ] **Step 3: Add `monitor_key_of_window` / `monitor_key_at_point` helpers**

These resolve "which monitor" directly via `MonitorFromWindow`/`MonitorFromPoint`
plus a single `GetMonitorInfoW` call, without needing to loop through
`enumerate_monitors()` — used later by hotkey routing (Task 6) and hot corners
(Task 9).

```rust
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};

fn device_name_of(hmonitor: HMONITOR) -> Option<String> {
    // SAFETY: `hmonitor` came from `MonitorFromWindow`/`MonitorFromPoint`,
    // which always return a valid handle (falling back to the nearest
    // real monitor per `MONITOR_DEFAULTTONEAREST`).
    unsafe {
        let mut info = MONITORINFOEXW {
            monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
            return None;
        }
        let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        Some(String::from_utf16_lossy(&info.szDevice[..len]))
    }
}

/// The device name of the monitor `hwnd` is currently on (nearest match
/// if it straddles more than one) — used to route a keyboard shortcut
/// triggered while that window is focused.
pub(crate) fn monitor_key_of_window(hwnd: HWND) -> Option<String> {
    // SAFETY: `hwnd` is a live window; a plain geometry query.
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    device_name_of(hmonitor)
}

/// The device name of the monitor the given screen point is on — used to
/// route a hot-corner trigger or Win-key tap to whichever monitor the
/// cursor is actually over.
pub(crate) fn monitor_key_at_point(pt: POINT) -> Option<String> {
    // SAFETY: plain geometry query, no preconditions.
    let hmonitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    device_name_of(hmonitor)
}

/// The `MonitorInfo` matching `device_name`, if it's still connected.
pub(crate) fn monitor_by_device_name<'a>(
    monitors: &'a [MonitorInfo],
    device_name: &str,
) -> Option<&'a MonitorInfo> {
    monitors.iter().find(|m| m.device_name == device_name)
}
```

- [ ] **Step 4: Write and run a unit test for `monitor_by_device_name`**

Add at the bottom of `monitors.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fake(device_name: &str, left: i32) -> MonitorInfo {
        MonitorInfo {
            rect: RECT { left, top: 0, right: left + 1920, bottom: 1080 },
            work: RECT { left, top: 0, right: left + 1920, bottom: 1080 },
            is_primary: left == 0,
            dpi: 96,
            device_name: device_name.to_string(),
        }
    }

    #[test]
    fn finds_monitor_by_device_name_regardless_of_order() {
        let monitors = vec![fake("\\\\.\\DISPLAY2", 1920), fake("\\\\.\\DISPLAY1", 0)];
        let found = monitor_by_device_name(&monitors, "\\\\.\\DISPLAY1").unwrap();
        assert!(found.is_primary);
        assert_eq!(found.rect.left, 0);
    }

    #[test]
    fn missing_device_name_returns_none() {
        let monitors = vec![fake("\\\\.\\DISPLAY1", 0)];
        assert!(monitor_by_device_name(&monitors, "\\\\.\\DISPLAY9").is_none());
    }
}
```

Run: `cargo test -p groveshell-ui monitors::tests`
Expected: both tests PASS.

- [ ] **Step 5: Full workspace build check**

Run: `cargo check --workspace`
Expected: clean (this task only adds a field/functions; nothing consumes
`device_name` yet, so no other file needs changes yet — `cargo check` will
warn about the unused `monitor_key_of_window`/`monitor_key_at_point` until
Task 6/9 use them; suppress with `#[allow(dead_code)]` on those two functions
for now, removed once they're wired up).

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/monitors.rs
git commit -m "feat: add stable device-name identity to MonitorInfo"
```

---

### Task 2: `MonitorWorkspaces` — per-monitor tracker map

**Files:**
- Create: `apps/ui/src/imp/monitor_workspaces.rs`
- Modify: `apps/ui/src/imp/mod.rs:5-19` (add `mod monitor_workspaces;`)

**Interfaces:**
- Consumes: `groveshell_window_model::workspace::{WorkspaceTracker, WorkspaceId}`
  (existing crate, unchanged)
- Produces: `MonitorWorkspaces` with methods `new()`, `insert_monitor(device_name: String, tracker: WorkspaceTracker)`,
  `remove_monitor(device_name: &str) -> Option<WorkspaceTracker>`,
  `get(&self, device_name: &str) -> Option<&WorkspaceTracker>`,
  `get_mut(&mut self, device_name: &str) -> Option<&mut WorkspaceTracker>`,
  `monitor_of_window(&self, hwnd: isize) -> Option<String>`,
  `device_names(&self) -> impl Iterator<Item = &str>`,
  `all_tracked_windows(&self) -> Vec<isize>`

This type is pure logic (no Win32), fully unit-testable.

- [ ] **Step 1: Write the failing tests**

```rust
// apps/ui/src/imp/monitor_workspaces.rs
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
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p groveshell-ui monitor_workspaces:: 2>&1 | head -30`
Expected: FAIL — `error[E0433]: failed to resolve: use of undeclared module` /
module not registered yet (Step 3 below).

- [ ] **Step 3: Register the module**

Edit `apps/ui/src/imp/mod.rs`, in the `mod` list (`mod.rs:5-19`), add:

```rust
mod monitor_workspaces;
```

alphabetically between `mod monitors;` and `mod movesize;` — i.e.:

```rust
mod monitors;
mod monitor_workspaces;
mod movesize;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p groveshell-ui monitor_workspaces::`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/monitor_workspaces.rs apps/ui/src/imp/mod.rs
git commit -m "feat: add MonitorWorkspaces, a per-monitor WorkspaceTracker map"
```

---

### Task 3: `OverviewInstance` and `AppState` restructure

**Files:**
- Modify: `apps/ui/src/imp/state.rs` (whole file rewritten below)
- Modify: `apps/ui/src/imp/overview.rs:140-237` (struct definitions move, no
  behavior change yet — behavior changes in Task 7)

**Interfaces:**
- Consumes: `CarouselAnim`, `CarouselDrag`, `OverviewMode`, `WindowDrag`,
  `WindowPopAnim` (existing types in `overview.rs`, unchanged)
- Produces: `OverviewInstance` struct, `AppState.overviews: HashMap<String, OverviewInstance>`,
  `AppState.workspaces: MonitorWorkspaces` (was `WorkspaceTracker`),
  `BarWindow.monitor: String` (new field),
  `Role::Bar { is_primary: bool, monitor: String }`,
  `Role::Overview { monitor: String }`,
  `role_of(hwnd: HWND) -> Role` (same signature, new variant shapes)

This task only changes the *shape* of state — every function that reads the
old flat fields still needs updating, which is Tasks 5-8. Expect
`cargo check` to show many errors after this task alone; that's expected and
resolved incrementally by the following tasks. To keep the workspace
buildable at each commit, do this task and Tasks 4 through 8 as one
uninterrupted sequence before running the full test suite (Task 8's last
step is the first point everything compiles again).

- [ ] **Step 1: Add `OverviewInstance` to `overview.rs`**

Add just above the existing `OverviewMode` enum (`overview.rs:163`):

```rust
/// Everything one monitor's Activities overview needs that isn't just
/// "the window handle" — mirrors what used to be flat fields on
/// `AppState` before each monitor got its own overview. One of these
/// exists per currently-connected monitor, keyed by device name in
/// `AppState.overviews`.
pub(crate) struct OverviewInstance {
    pub(crate) hwnd: HWND,
    pub(crate) mode: OverviewMode,
    /// Current horizontal scroll position through *this monitor's*
    /// carousel, in page units. Only meaningful while `mode` isn't
    /// `Closed`.
    pub(crate) carousel_offset: f64,
    pub(crate) carousel_drag: Option<CarouselDrag>,
    pub(crate) carousel_anim: Option<CarouselAnim>,
    pub(crate) carousel_close_after: Option<HWND>,
    pub(crate) window_drag: Option<WindowDrag>,
    pub(crate) window_pop_anim: Option<WindowPopAnim>,
    pub(crate) hover_thumb: Option<(isize, std::time::Instant)>,
    pub(crate) dock_apps: Vec<super::dock::DockApp>,
    pub(crate) dock_hover: Option<(usize, std::time::Instant)>,
    pub(crate) search_query: String,
}

impl OverviewInstance {
    pub(crate) fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            mode: OverviewMode::Closed,
            carousel_offset: 0.0,
            carousel_drag: None,
            carousel_anim: None,
            carousel_close_after: None,
            window_drag: None,
            window_pop_anim: None,
            hover_thumb: None,
            dock_apps: Vec::new(),
            dock_hover: None,
            search_query: String::new(),
        }
    }
}
```

- [ ] **Step 2: Rewrite `state.rs`**

Replace the whole file with:

```rust
//! Shared UI state: the single `AppState` every other module reads and
//! mutates through the thread-local `STATE` cell, plus the `Role` lookup
//! used to dispatch messages in `wndproc`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, RECT};

use groveshell_window_model::registry::WindowRegistry;

use super::dock::DockApp;
use super::monitor_workspaces::MonitorWorkspaces;
use super::overview::OverviewInstance;

pub(crate) const BAR_HEIGHT: i32 = 24;
pub(crate) const BAR_CORNER_RADIUS: i32 = 10;

pub(crate) const ANIM_DURATION: std::time::Duration = std::time::Duration::from_millis(250);
pub(crate) const ANIM_TIMER_ID: usize = 1;
pub(crate) const ANIM_TIMER_INTERVAL_MS: u32 = 16;
pub(crate) const CLOCK_TIMER_ID: usize = 2;

pub(crate) fn scaled(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

/// Effective DPI of the primary monitor — used only where a value must
/// be primary-anchored on purpose (the clock/Quick-Settings pill).
/// Per-monitor overview/bar code must use *that monitor's own* DPI
/// instead (see `monitors::MonitorInfo::dpi`), never this.
pub(crate) fn reference_dpi() -> u32 {
    let monitors = super::monitors::monitors_sorted_by_x();
    monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first())
        .map(|m| m.dpi)
        .unwrap_or(96)
}

/// One monitor's top-level bar window plus the rect the AppBar system
/// actually assigned it and which monitor (by device name) it belongs
/// to.
pub(crate) struct BarWindow {
    pub(crate) hwnd: HWND,
    pub(crate) rect: RECT,
    pub(crate) is_primary: bool,
    pub(crate) monitor: String,
}

pub(crate) struct AppState {
    pub(crate) bars: Vec<BarWindow>,
    pub(crate) primary_bar_hwnd: HWND,
    pub(crate) primary_bar_rect: RECT,
    /// The primary monitor's device name — the fallback target for
    /// windows/workspaces orphaned by a monitor unplug (Task 10).
    pub(crate) primary_monitor: String,
    pub(crate) calendar_hwnd: HWND,
    pub(crate) quick_settings_hwnd: HWND,
    pub(crate) calendar_open: bool,
    pub(crate) quick_settings_open: bool,
    pub(crate) previous_foreground: HWND,
    /// Per-monitor window->workspace assignment (see
    /// `monitor_workspaces::MonitorWorkspaces`).
    pub(crate) workspaces: MonitorWorkspaces,
    /// Per-monitor Activities overview state, keyed by device name.
    pub(crate) overviews: HashMap<String, OverviewInstance>,
    pub(crate) window_registry: WindowRegistry,
    pub(crate) qs_pill_hover: bool,
    pub(crate) qs_volume_dragging: bool,
}

thread_local! {
    pub(crate) static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Role {
    Bar { is_primary: bool, monitor: String },
    Overview { monitor: String },
    Calendar,
    QuickSettings,
    Other,
}

pub(crate) fn role_of(hwnd: HWND) -> Role {
    STATE.with(|s| {
        let state = s.borrow();
        let Some(st) = state.as_ref() else {
            return Role::Other;
        };
        if let Some(bar) = st.bars.iter().find(|b| b.hwnd == hwnd) {
            return Role::Bar {
                is_primary: bar.is_primary,
                monitor: bar.monitor.clone(),
            };
        }
        if let Some((monitor, _)) = st.overviews.iter().find(|(_, ov)| ov.hwnd == hwnd) {
            return Role::Overview { monitor: monitor.clone() };
        }
        if hwnd == st.calendar_hwnd {
            Role::Calendar
        } else if hwnd == st.quick_settings_hwnd {
            Role::QuickSettings
        } else {
            Role::Other
        }
    })
}
```

- [ ] **Step 3: Confirm the expected compile fallout**

Run: `cargo check --workspace 2>&1 | head -80`
Expected: many errors in `mod.rs`, `bar.rs`, `overview.rs`, `workspaces.rs`,
`movesize.rs` — all references to the now-removed flat fields
(`st.overview`, `st.overview_hwnd`, `st.carousel_offset`, `st.workspaces`
used as a bare `WorkspaceTracker`, `Role::Overview` used as a unit variant,
`Role::Bar { is_primary }` missing the new `monitor` field, `MonitorInfo`
`Copy` reliance). This is expected — Tasks 4 through 8 resolve every one of
these. Do not attempt to fix them all in this task.

- [ ] **Step 4: Commit (compiles or not — this is a checkpoint, not a release)**

```bash
git add apps/ui/src/imp/state.rs apps/ui/src/imp/overview.rs
git commit -m "refactor: introduce OverviewInstance and per-monitor AppState shape (WIP, does not build yet)"
```

---

### Task 4: `mod.rs` startup — one tracker + one overview per monitor

**Files:**
- Modify: `apps/ui/src/imp/mod.rs:41` (imports), `:155-232` (bar creation
  loop), `:245-267` (overview creation), `:330-376` (workspace seeding +
  `AppState` construction), `:639-677` (`WM_DESTROY`)

**Interfaces:**
- Consumes: `MonitorInfo.device_name`, `MonitorWorkspaces`, `OverviewInstance`
  (Tasks 1-3)
- Produces: one `overview_hwnd` per monitor registered in
  `AppState.overviews`, `AppState.workspaces: MonitorWorkspaces` populated
  with one tracker per monitor

- [ ] **Step 1: Extend the bar-creation loop to also build a `MonitorWorkspaces` map**

In `mod.rs`, the bar-creation loop (`:179-232`) already iterates `&monitors`
and pushes a `BarWindow`. Add the `monitor` field and, right after building
`bars`, build the workspace map from the *same* monitor list order used for
pinning today (`monitors_sorted_by_x()` — keep using it for initial index
assignment, but key the map by device name, not position):

```rust
bars.push(BarWindow {
    hwnd: bar_hwnd,
    rect: bar_rect,
    is_primary: monitor.is_primary,
    monitor: monitor.device_name.clone(),
});
```

- [ ] **Step 2: Create one overview window per monitor instead of one virtual-screen-wide window**

Replace the single overview-creation block (`:245-267`) with a loop creating
one overview per monitor, sized to that monitor's own rect:

```rust
// One Activities overview per monitor now, sized to that monitor's
// own rect (not the virtual screen) — see the design doc §D. Created
// eagerly at startup (not lazily) since there's no live-resize path
// for an already-open overview window yet; lazy creation is only
// needed for hotplug (Task 10), which creates one on demand there.
let mut overviews: std::collections::HashMap<String, overview::OverviewInstance> =
    std::collections::HashMap::new();
for monitor in &monitors {
    let width = monitor.rect.right - monitor.rect.left;
    let height = monitor.rect.bottom - monitor.rect.top;
    let overview_hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
        w!("GroveShellOverview"),
        w!("GroveShell Activities"),
        WS_POPUP,
        monitor.rect.left,
        monitor.rect.top,
        width,
        height,
        None,
        None,
        hinstance,
        None,
    )
    .map_err(Error::Windows)?;
    overviews.insert(monitor.device_name.clone(), overview::OverviewInstance::new(overview_hwnd));
}
```

Remove the now-unused `virtual_x`/`virtual_y`/`virtual_w`/`virtual_h`
(`SM_XVIRTUALSCREEN` etc.) lines — no window spans the virtual screen
anymore.

- [ ] **Step 3: Build `MonitorWorkspaces` instead of a single `WorkspaceTracker`**

Replace the workspace-seeding block (`:330-347`):

```rust
let mut workspaces = monitor_workspaces::MonitorWorkspaces::new();
for monitor in &monitors {
    workspaces.insert_monitor(
        monitor.device_name.clone(),
        WorkspaceTracker::with_monitor_workspaces(1, 0),
    );
}
for window in groveshell_window_model::snapshot() {
    let center_x = (window.rect.left + window.rect.right) / 2;
    let center_y = (window.rect.top + window.rect.bottom) / 2;
    let target_monitor = monitor_index_for_center(&monitors, center_x, center_y)
        .and_then(|i| monitors.get(i))
        .or_else(|| monitors.iter().find(|m| m.is_primary))
        .map(|m| m.device_name.clone());
    if let Some(device_name) = target_monitor {
        if let Some(tracker) = workspaces.get_mut(&device_name) {
            tracker.assign_to_index(window.hwnd, 0);
        }
    }
}
```

Add `use monitor_workspaces;` (module path, not an item import — since it's
referenced as `monitor_workspaces::MonitorWorkspaces` above) alongside the
existing `mod monitor_workspaces;` declaration, and remove the now-unused
`WorkspaceTracker` import if nothing else in this file constructs one
directly (it's still needed for `WorkspaceTracker::with_monitor_workspaces`
above, so keep the import).

- [ ] **Step 4: Update `AppState` construction**

Replace the `STATE.with(...)` block (`:349-376`):

```rust
let primary_monitor = monitors.iter().find(|m| m.is_primary)
    .map(|m| m.device_name.clone())
    .unwrap_or_else(|| monitors[0].device_name.clone());

STATE.with(|s| {
    *s.borrow_mut() = Some(AppState {
        bars,
        primary_bar_hwnd,
        primary_bar_rect,
        primary_monitor,
        calendar_hwnd,
        quick_settings_hwnd,
        calendar_open: false,
        quick_settings_open: false,
        previous_foreground: HWND(std::ptr::null_mut()),
        workspaces,
        overviews,
        window_registry: WindowRegistry::new(),
        qs_pill_hover: false,
        qs_volume_dragging: false,
    });
});
```

- [ ] **Step 5: Fix `WM_DESTROY`'s shutdown unpark loop**

`mod.rs:655-667` builds `tracked` from `st.workspaces.workspace_ids()...` —
that method no longer exists on `MonitorWorkspaces` directly. Replace with:

```rust
let tracked: Vec<isize> = STATE.with(|s| {
    s.borrow()
        .as_ref()
        .map(|st| st.workspaces.all_tracked_windows())
        .unwrap_or_default()
});
```

- [ ] **Step 6: `cargo check` — confirm `mod.rs`'s own errors are gone**

Run: `cargo check -p groveshell-ui 2>&1 | grep "mod.rs"`
Expected: no remaining errors specifically in `mod.rs` (errors in `bar.rs`,
`overview.rs`, `workspaces.rs`, `movesize.rs` are expected and fixed in
Tasks 5-8).

- [ ] **Step 7: Commit**

```bash
git add apps/ui/src/imp/mod.rs
git commit -m "feat: create one bar/tracker/overview per monitor at startup (WIP)"
```

---

### Task 5: Bar — Activities + dots on every monitor

**Files:**
- Modify: `apps/ui/src/imp/bar.rs` (whole file's `is_primary`-gated section
  becomes per-monitor; `on_bar_click`/`on_bar_hover`/`on_bar_mouse_leave`
  gain a `monitor: &str` parameter)
- Modify: `apps/ui/src/imp/mod.rs` (call sites passing `is_primary`/`monitor`
  through from `Role::Bar`)

**Interfaces:**
- Consumes: `MonitorWorkspaces::get(&self, device_name: &str) -> Option<&WorkspaceTracker>` (Task 2),
  `Role::Bar { is_primary: bool, monitor: String }` (Task 3)
- Produces: `paint_bar(hwnd: HWND, is_primary: bool, monitor: &str)`,
  `on_bar_click(hwnd: HWND, x: i32, is_primary: bool, monitor: &str)`,
  `on_bar_hover(hwnd: HWND, x: i32, y: i32, is_primary: bool, monitor: &str)`
  (unchanged: `on_bar_mouse_leave`, `refresh_bar_indicator`, `raise_bars_topmost`,
  `register_appbar`, `unregister_appbar`)

- [ ] **Step 1: Split `paint_bar`'s per-monitor section out of the `is_primary` gate**

Activities + dots move outside `if is_primary`; clock + Quick Settings pill
stay inside it. Replace `paint_bar`'s body (`bar.rs:138-247`):

```rust
pub(crate) fn paint_bar(hwnd: HWND, is_primary: bool, monitor: &str) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);

        let dpi = GetDpiForWindow(hwnd).max(96);
        let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
        let bar_width = STATE
            .with(|s| {
                s.borrow().as_ref().and_then(|st| {
                    st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
                })
            })
            .unwrap_or(0);

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00E0E0E0));
        let font = bar_font(dpi);
        let previous_font = SelectObject(hdc, font);
        let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;

        // Activities button + workspace dots: every monitor's bar now,
        // each reading its own monitor's tracker.
        draw_text_in(
            hdc,
            RECT {
                left: scaled(ACTIVITIES_LABEL_X, dpi),
                top: 0,
                right: scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi),
                bottom: bar_h,
            },
            "Activities",
            format,
        );

        let (workspace_count, current_index) = STATE
            .with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|st| st.workspaces.get(monitor))
                    .map(|t| (t.workspace_ids().len(), t.current_index()))
            })
            .unwrap_or((0, 0));
        let dot_mid_y = bar_h / 2;
        let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
        let dot_radius = scaled(WS_DOT_RADIUS, dpi);
        let filled_brush = CreateSolidBrush(COLORREF(0x00E0E0E0));
        let empty_brush = CreateSolidBrush(COLORREF(0x00606060));
        for i in 0..workspace_count {
            let cx = scaled(WS_DOTS_X, dpi) + i as i32 * dot_slot_w + dot_slot_w / 2;
            let brush = if i == current_index { filled_brush } else { empty_brush };
            let previous = SelectObject(hdc, brush);
            let _ = Ellipse(hdc, cx - dot_radius, dot_mid_y - dot_radius, cx + dot_radius, dot_mid_y + dot_radius);
            SelectObject(hdc, previous);
        }
        let _ = DeleteObject(filled_brush);
        let _ = DeleteObject(empty_brush);

        if is_primary {
            let clock_x = bar_width / 2 - scaled(CLOCK_LABEL_WIDTH, dpi) / 2;
            draw_text_in(
                hdc,
                RECT { left: clock_x, top: 0, right: clock_x + scaled(CLOCK_LABEL_WIDTH, dpi), bottom: bar_h },
                &clock_text(),
                format,
            );

            let (pill, slots) = qs_pill_layout(bar_width, dpi, bar_h);
            let hovered = STATE.with(|s| s.borrow().as_ref().map(|st| st.qs_pill_hover)).unwrap_or(false);
            if hovered {
                let highlight = CreateSolidBrush(blend_toward_white(0x00202020, 0.15));
                let previous_brush = SelectObject(hdc, highlight);
                SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
                let radius = scaled(QS_PILL_RADIUS, dpi);
                let _ = RoundRect(hdc, pill.left, pill.top, pill.right, pill.bottom, radius * 2, radius * 2);
                SelectObject(hdc, previous_brush);
                let _ = DeleteObject(highlight);
            }

            let glyph_color = COLORREF(0x00E0E0E0);
            let wifi_icon = if wifi_radio_on().unwrap_or(false) { Icon::Wifi } else { Icon::WifiOff };
            draw_icon(hdc, slots[0], wifi_icon, glyph_color);
            let vol_icon = volume_icon(get_mute().unwrap_or(false), get_volume_percent().unwrap_or(0));
            draw_icon(hdc, slots[1], vol_icon, glyph_color);
            let (pct, charging) = battery_status().unwrap_or((100, false));
            draw_icon(hdc, slots[2], battery_icon(pct, charging), glyph_color);
        }

        SelectObject(hdc, previous_font);
        let _ = DeleteObject(font);
        let _ = EndPaint(hwnd, &ps);
    }
}
```

- [ ] **Step 2: `on_bar_click` — every bar handles Activities/dots; clock/QS pill stay primary-gated**

```rust
pub(crate) fn on_bar_click(hwnd: HWND, x: i32, is_primary: bool, monitor: &str) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let bar_width = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            st.bars.iter().find(|b| b.hwnd == hwnd).map(|b| b.rect.right - b.rect.left)
        })
    });
    let Some(bar_width) = bar_width else {
        return;
    };

    if (scaled(ACTIVITIES_LABEL_X, dpi)..scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi)).contains(&x) {
        super::overview::toggle_overview_for(monitor);
        return;
    }

    let workspace_count = STATE
        .with(|s| s.borrow().as_ref().and_then(|st| st.workspaces.get(monitor)).map(|t| t.workspace_ids().len()))
        .unwrap_or(0);
    let dots_x = scaled(WS_DOTS_X, dpi);
    let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
    let dots_width = workspace_count as i32 * dot_slot_w;
    if (dots_x..dots_x + dots_width).contains(&x) {
        let index = ((x - dots_x) / dot_slot_w) as usize;
        let overview_open = STATE
            .with(|s| {
                s.borrow().as_ref().and_then(|st| st.overviews.get(monitor))
                    .map(|ov| matches!(ov.mode, OverviewMode::Open { .. }))
            })
            .unwrap_or(false);
        if overview_open {
            super::overview::snap_carousel_to(monitor, index, None);
        } else {
            super::workspaces::commit_workspace_switch(monitor, index);
        }
        return;
    }

    if !is_primary {
        return;
    }

    let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
    let clock_x = bar_width / 2 - clock_w / 2;
    if (clock_x..clock_x + clock_w).contains(&x) {
        toggle_calendar();
        return;
    }

    let bar_h = scaled(super::state::BAR_HEIGHT, dpi);
    let (pill, _) = qs_pill_layout(bar_width, dpi, bar_h);
    if (pill.left..pill.right).contains(&x) {
        toggle_quick_settings();
    }
}
```

This introduces `overview::toggle_overview_for(monitor: &str)` and changes
`snap_carousel_to`/`commit_workspace_switch` to take a leading `monitor: &str`
— both defined in Tasks 6-7. `import` line at the top of `bar.rs` for
`open_overview`/`close_overview` from `super::overview` is removed (replaced
by `toggle_overview_for`); keep `OverviewMode` imported.

- [ ] **Step 3: `on_bar_hover`/`on_bar_mouse_leave` — gate the QS-pill highlight to the primary bar only**

The status-pill hover highlight only exists on the primary bar (Quick
Settings stays primary-only). Change the signature but keep the early return
for non-primary bars:

```rust
pub(crate) fn on_bar_hover(hwnd: HWND, x: i32, _y: i32, is_primary: bool) {
    if !is_primary {
        return;
    }
    // ...unchanged body below this point...
}
```

`on_bar_mouse_leave` is unchanged (it already only clears `qs_pill_hover`,
which is meaningless on non-primary bars since it's never set there).

- [ ] **Step 4: Commit**

```bash
git add apps/ui/src/imp/bar.rs
git commit -m "feat: Activities button and workspace dots on every monitor's bar (WIP)"
```

---

### Task 6: Workspace switching/moving resolves the acting monitor

**Files:**
- Modify: `apps/ui/src/imp/workspaces.rs` (whole file — every function
  touching `state.workspaces` as a bare tracker now takes/resolves a
  `monitor: &str`)

**Interfaces:**
- Consumes: `MonitorWorkspaces` (Task 2), `monitors::monitor_key_of_window` (Task 1)
- Produces: `commit_workspace_switch(monitor: &str, target_index: usize)`,
  `switch_workspace_relative(delta: i32)` (unchanged signature — resolves
  the monitor internally from the focused window),
  `move_focused_window_relative(delta: i32)` (unchanged signature, same
  reason), `sync_workspaces() -> Vec<WindowRecord>` (unchanged signature —
  iterates every monitor's tracker internally now)

- [ ] **Step 1: `commit_workspace_switch` takes the target monitor explicitly**

```rust
pub(crate) fn commit_workspace_switch(monitor: &str, target_index: usize) {
    let switch = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let tracker = state.workspaces.get_mut(monitor)?;
        let from_pinned = tracker.is_pinned(tracker.current_index());
        let to_pinned = tracker.is_pinned(target_index);
        let (from_id, to_id) = tracker.switch_to_index(target_index)?;
        let both_pinned = from_pinned && to_pinned;
        let hide = if both_pinned { Vec::new() } else { tracker.windows_on(from_id) };
        let show = if both_pinned { Vec::new() } else { tracker.windows_on(to_id) };
        Some((hide, show))
    });
    let Some((hide, show)) = switch else {
        return;
    };

    for hwnd in &hide {
        park_window(HWND(*hwnd as *mut c_void));
    }
    for hwnd in &show {
        unpark_window(HWND(*hwnd as *mut c_void));
    }

    super::overview::rebuild_open_overview_pages(monitor);
    refresh_bar_indicator();
}
```

(`both_pinned` for a single-monitor tracker with `pinned_count() == 1` is
only ever true when `target_index == current_index`, which `switch_to_index`
already short-circuits to `None` for — so the "two pinned monitors, nothing
to hide/show" case from the old global model can no longer occur here; the
guard is harmless dead logic kept only because `is_pinned`/`windows_on` still
need calling either way. No behavior change: parking/unparking always runs
for a real per-monitor switch now, matching what already happened for any
dynamic-workspace switch before this change.)

- [ ] **Step 2: `switch_workspace_relative`/`move_focused_window_relative` resolve their monitor from the focused window**

```rust
pub(crate) fn switch_workspace_relative(delta: i32) {
    sync_workspaces();
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    let Some(monitor) = super::monitors::monitor_key_of_window(fg) else {
        return;
    };
    let (target, overview_open) = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| {
                let tracker = st.workspaces.get(&monitor)?;
                let overview_open = st.overviews.get(&monitor)
                    .map(|ov| matches!(ov.mode, OverviewMode::Open { .. }))
                    .unwrap_or(false);
                Some((tracker.clamped_relative_index(delta), overview_open))
            })
            .unwrap_or((0, false))
    });
    if overview_open {
        super::overview::snap_carousel_to(&monitor, target, None);
    } else {
        commit_workspace_switch(&monitor, target);
    }
}

pub(crate) fn move_focused_window_relative(delta: i32) {
    let _ = delta;
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || role_of(fg) != Role::Other {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_of_window(fg) else {
        return;
    };
    sync_workspaces();
    let hwnd = fg.0 as isize;
    let moved = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let tracker = state.workspaces.get_mut(&monitor)?;
        let target = tracker.pinned_count();
        tracker.move_window_to_index(hwnd, target)
    });
    if moved.is_some() {
        park_window(fg);
    }
    refresh_bar_indicator();
}
```

Note: `monitor_key_of_window` uses `MonitorFromWindow`'s *nearest* match, so
even a fully-parked (off-screen, `top >= WORKSPACE_PARK_DY / 2`) foreground
window still resolves to *some* monitor rather than `None` — real screen
coordinates always map to the nearest real monitor, off-screen or not. This
matches the resolution the design's Section E called for ("whichever monitor
currently owns keyboard focus").

- [ ] **Step 3: `on_foreground_changed` resolves the window's own monitor, not a global one**

```rust
fn on_foreground_changed(hwnd: HWND) {
    let suppressed =
        SUPPRESS_FOLLOW_UNTIL.with(|c| c.get().is_some_and(|until| Instant::now() < until));
    if suppressed {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_of_window(hwnd) else {
        return;
    };
    let target = STATE.with(|s| {
        let state_ref = s.borrow();
        let st = state_ref.as_ref()?;
        let tracker = st.workspaces.get(&monitor)?;
        let id = tracker.workspace_of(hwnd.0 as isize)?;
        let index = tracker.index_of(id)?;
        (index != tracker.current_index()).then_some(index)
    });
    if let Some(index) = target {
        commit_workspace_switch(&monitor, index);
    }
}
```

- [ ] **Step 4: `sync_workspaces` iterates every monitor's tracker**

This is the one function that genuinely needs to touch *all* monitors at
once (a live re-sync has to assign every untracked window somewhere, and it
doesn't yet know which monitor "current" means without first checking where
each window physically is). Replace the body:

```rust
pub(crate) fn sync_workspaces() -> Vec<groveshell_window_model::WindowRecord> {
    let live = groveshell_window_model::snapshot();
    let monitors = monitors_sorted_by_x();
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            for window in &live {
                let (_, reused) = state.window_registry.observe(window.hwnd, window.pid);
                if reused {
                    // A recycled hwnd may have belonged to any monitor's
                    // tracker; `forget` on a tracker that never had it
                    // assigned is a documented no-op.
                    for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
                        if let Some(tracker) = state.workspaces.get_mut(&name) {
                            tracker.forget(window.hwnd);
                        }
                    }
                    drop_window_snapshot(window.hwnd);
                }
                if state.workspaces.monitor_of_window(window.hwnd).is_some() {
                    continue;
                }
                let center_x = (window.rect.left + window.rect.right) / 2;
                let center_y = (window.rect.top + window.rect.bottom) / 2;
                let target_monitor = super::monitors::monitor_index_for_center(&monitors, center_x, center_y)
                    .and_then(|i| monitors.get(i))
                    .map(|m| m.device_name.clone())
                    .unwrap_or_else(|| state.primary_monitor.clone());
                if let Some(tracker) = state.workspaces.get_mut(&target_monitor) {
                    let index = tracker.current_index();
                    tracker.assign_to_index(window.hwnd, index);
                }
            }
            for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
                if let Some(tracker) = state.workspaces.get_mut(&name) {
                    tracker.prune(groveshell_window_model::is_alive);
                }
            }
            state.window_registry.prune(groveshell_window_model::is_alive);
            let tracked = state.workspaces.all_tracked_windows();
            retain_window_snapshots(&tracked);
        }
    });
    live
}
```

(Task 11 extends this same loop to also *reassign* a window whose current
monitor no longer matches its real on-screen monitor — physical
cross-monitor drag. Left as a `TODO`-free no-op here on purpose: this task's
job is only to make the existing behavior monitor-scoped, not yet to add the
new reassignment behavior, which needs its own test coverage in Task 11.)

- [ ] **Step 5: `on_window_sync_timer` and `install_win_event_hooks`/`schedule_window_sync` — no signature changes needed**

These already operate process-wide (they don't reference `state.workspaces`
directly) — `rebuild_open_overview_pages()` inside `on_window_sync_timer`
does need updating for the new per-monitor signature; change its one call
site to loop over every open overview:

```rust
pub(crate) fn on_window_sync_timer(bar_hwnd: HWND) {
    unsafe {
        let _ = KillTimer(bar_hwnd, SYNC_TIMER_ID);
    }
    sync_workspaces();
    refresh_bar_indicator();
    let monitors: Vec<String> = STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.overviews.keys().cloned().collect()).unwrap_or_default()
    });
    for monitor in &monitors {
        super::overview::rebuild_open_overview_pages(monitor);
    }
}
```

- [ ] **Step 6: `cargo check` — confirm `workspaces.rs`'s own errors are gone**

Run: `cargo check -p groveshell-ui 2>&1 | grep "workspaces.rs"`
Expected: no remaining errors in `workspaces.rs` (errors in `overview.rs`,
`mod.rs`, `movesize.rs` calling the now-changed function signatures are
expected — Tasks 7-9 fix those call sites).

- [ ] **Step 7: Commit**

```bash
git add apps/ui/src/imp/workspaces.rs
git commit -m "feat: workspace switch/move/sync resolve the acting monitor (WIP)"
```

---

### Task 7: Overview — scope every function to one monitor

This is the largest task. `overview.rs` has ~25 `pub(crate)` functions that
today read/write the single flat `st.overview`/`st.overview_hwnd`/
`st.carousel_*`/`st.dock_*`/`st.search_query` fields. Every one of them needs
the same mechanical transformation: add a `monitor: &str` parameter, and
replace `st.overview` / `st.overview_hwnd` / `st.carousel_offset` / etc. with
a lookup into `st.overviews.get(monitor)` / `.get_mut(monitor)`, propagating
`None` (early-return / no-op) if that monitor's overview instance is ever
missing (shouldn't normally happen, but a monitor mid-teardown during
hotplog, Task 10, is exactly when it can).

**Files:**
- Modify: `apps/ui/src/imp/overview.rs` (all functions listed below)
- Modify: `apps/ui/src/imp/mod.rs` (call sites — Task 8 handles the dispatch
  side specifically; this task's own call sites within `overview.rs` itself
  also need updating since these functions call each other)

**Interfaces:**
- Produces (all gain a leading `monitor: &str` parameter; return types
  unchanged): `card_layout(monitor: &str) -> (RECT, i32)`,
  `search_layout(monitor: &str, dpi: u32, result_count: usize) -> (RECT, Vec<RECT>)`,
  `build_carousel_pages(monitor: &str) -> (Vec<CardAnim>, Vec<ThumbAnim>, usize, Vec<DockApp>)`,
  `open_overview(monitor: &str)`, `close_overview(monitor: &str, focus_after: Option<HWND>)`,
  `rebuild_open_overview_pages(monitor: &str)`, `snap_carousel_to(monitor: &str, target_index: usize, close_after: Option<HWND>)`,
  `toggle_overview_for(monitor: &str)` (new — replaces the old parameterless `toggle_overview`),
  `on_overview_char(monitor: &str, ch: u32)`, `on_overview_arrow(monitor: &str, delta: i32)`,
  `on_overview_hover(monitor: &str, x: i32, y: i32)`, `on_overview_drag_start(monitor: &str, x: i32, y: i32)`,
  `on_overview_drag_move(monitor: &str, x: i32, y: i32)`, `on_overview_drag_end(monitor: &str, x: i32, y: i32)`,
  `paint_overview(hwnd: HWND, monitor: &str)`, `repaint_overview(hwnd: HWND)` (unchanged — already just calls `InvalidateRect`, doesn't touch `STATE`), `on_animation_tick(monitor: &str)`

- [ ] **Step 1: `card_layout` and `search_layout` — anchor to the given monitor, not "the primary"**

This is also the fix for the search-bar DPI/visibility bug from the design
doc. Replace `card_layout` (`overview.rs:266-300`):

```rust
pub(crate) fn card_layout(monitor: &str) -> (RECT, i32) {
    let monitors = monitors_sorted_by_x();
    let Some(this_monitor) = super::monitors::monitor_by_device_name(&monitors, monitor) else {
        // Monitor vanished mid-frame (hotplug race) — degrade to a
        // primary-anchored guess rather than panicking; the overview
        // is about to be torn down for this monitor anyway (Task 10).
        return card_layout_fallback(&monitors);
    };

    let origin_x = this_monitor.rect.left;
    let client_h = this_monitor.rect.bottom - this_monitor.rect.top;
    let dpi = this_monitor.dpi;
    let w = (this_monitor.rect.right - this_monitor.rect.left) as f64;
    let h = (this_monitor.rect.bottom - this_monitor.rect.top).max(1) as f64;
    let ref_aspect = w / h;
    let ref_center_x_abs = (this_monitor.rect.left + this_monitor.rect.right) / 2;

    let card_w = (w * CARD_WIDTH_FRACTION).round() as i32;
    let max_card_h = (client_h - scaled(BAR_HEIGHT + CARD_MARGIN_TOP + CARD_MARGIN_BOTTOM, dpi)).max(1);
    let card_h = ((card_w as f64 / ref_aspect).round() as i32).min(max_card_h).max(1);

    let card_top = scaled(BAR_HEIGHT + CARD_MARGIN_TOP, dpi) + (max_card_h - card_h) / 2;
    let card_left = (ref_center_x_abs - origin_x) - card_w / 2;
    let rect = RECT {
        left: card_left,
        top: card_top,
        right: card_left + card_w,
        bottom: card_top + card_h,
    };
    (rect, card_w + scaled(CARD_GAP, dpi))
}

fn card_layout_fallback(monitors: &[super::monitors::MonitorInfo]) -> (RECT, i32) {
    let reference = monitors.iter().find(|m| m.is_primary).or_else(|| monitors.first());
    match reference {
        Some(m) => {
            let device_name = m.device_name.clone();
            card_layout(&device_name)
        }
        None => (RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, 1920),
    }
}
```

Note the key simplification versus the old version: since this overview
window is now sized to exactly its own monitor's rect (Task 4, Step 2), the
window's own top-left *is* that monitor's top-left — so `origin_x`/`client_h`
come straight from `this_monitor.rect`, not `GetSystemMetrics(SM_XVIRTUALSCREEN...)`.
That `GetSystemMetrics` import can be removed from this function (check
whether `layout_grid`/other functions in the file still need it before
deleting the `use` line entirely).

Replace `search_layout` (`overview.rs:1295-1316`):

```rust
fn search_layout(monitor: &str, dpi: u32, result_count: usize) -> (RECT, Vec<RECT>) {
    let (card, _) = card_layout(monitor);
    let width = scaled(SEARCH_PANEL_WIDTH, dpi);
    let row_h = scaled(SEARCH_ROW_HEIGHT, dpi);
    let left = (card.left + card.right) / 2 - width / 2;
    let top = scaled(BAR_HEIGHT + SEARCH_PANEL_GAP, dpi);
    let rows: Vec<RECT> = (0..result_count + 1)
        .map(|i| RECT { left, top: top + row_h * i as i32, right: left + width, bottom: top + row_h * (i as i32 + 1) })
        .collect();
    let panel = RECT { left, top, right: left + width, bottom: top + row_h * (result_count as i32 + 1) };
    (panel, rows)
}
```

- [ ] **Step 2: `build_carousel_pages`, `open_overview`, `rebuild_open_overview_pages`, `close_overview`**

```rust
pub(crate) fn build_carousel_pages(monitor: &str) -> (Vec<CardAnim>, Vec<ThumbAnim>, usize, Vec<super::dock::DockApp>) {
    let live = super::workspaces::sync_workspaces();

    let (workspace_ids, current_pos) = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.workspaces.get(monitor))
            .map(|t| (t.workspace_ids().to_vec(), t.current_index()))
            .unwrap_or_default()
    });

    let (card_rect, _) = card_layout(monitor);
    let mut cards = Vec::new();
    let mut thumbs = Vec::new();
    let mut all_windows: Vec<groveshell_window_model::WindowRecord> = Vec::new();

    for (page, &ws_id) in workspace_ids.iter().enumerate() {
        cards.push(CardAnim { page, rect: card_rect });

        let assigned = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.workspaces.get(monitor))
                .map(|t| t.windows_on(ws_id))
                .unwrap_or_default()
        });
        let windows: Vec<groveshell_window_model::WindowRecord> = assigned
            .into_iter()
            .filter_map(|hwnd| live.iter().find(|w| w.hwnd == hwnd).cloned().or_else(|| groveshell_window_model::describe(hwnd)))
            .collect();
        all_windows.extend(windows.iter().cloned());

        for (slot_rect, icon_rect, window) in layout_grid(card_rect, windows) {
            let source = HWND(window.hwnd as *mut c_void);
            let icon = window_icon(source);
            if window_snapshot(window.hwnd).is_none() {
                capture_window_snapshot(source);
            }
            thumbs.push(ThumbAnim { hwnd: source, title: window.title, icon, page, rect: slot_rect, icon_rect });
        }
    }

    let dock_apps = super::dock::build_dock_apps(&all_windows);
    (cards, thumbs, current_pos, dock_apps)
}

pub(crate) fn open_overview(monitor: &str) {
    let overview_hwnd = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.overviews.get(monitor))
            .filter(|ov| matches!(ov.mode, OverviewMode::Closed))
            .map(|ov| ov.hwnd)
    });
    let Some(overview_hwnd) = overview_hwnd else {
        return;
    };

    hide_calendar(false);
    hide_quick_settings(false);

    let (cards, thumbs, current_pos, dock_apps) = build_carousel_pages(monitor);

    // SAFETY: no preconditions.
    let previous_foreground = unsafe { GetForegroundWindow() };

    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.previous_foreground = previous_foreground;
            if let Some(ov) = state.overviews.get_mut(monitor) {
                ov.carousel_offset = current_pos as f64;
                ov.carousel_drag = None;
                ov.carousel_anim = None;
                ov.carousel_close_after = None;
                ov.dock_apps = dock_apps;
                ov.dock_hover = None;
                ov.mode = OverviewMode::Opening { started: Instant::now(), thumbs, cards };
            }
        }
    });

    // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), 0, LWA_ALPHA);
        let _ = ShowWindow(overview_hwnd, SW_SHOW);
        let _ = SetForegroundWindow(overview_hwnd);
        let _ = SetFocus(overview_hwnd);
        raise_bars_topmost();
        SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
    }
}

pub(crate) fn rebuild_open_overview_pages(monitor: &str) {
    let overview_hwnd = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.overviews.get(monitor))
            .filter(|ov| matches!(ov.mode, OverviewMode::Open { .. }))
            .map(|ov| ov.hwnd)
    });
    let Some(overview_hwnd) = overview_hwnd else {
        return;
    };

    let (cards, thumbs, _current_pos, dock_apps) = build_carousel_pages(monitor);

    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if let Some(ov) = state.overviews.get_mut(monitor) {
                if matches!(ov.mode, OverviewMode::Open { .. }) {
                    ov.mode = OverviewMode::Open { thumbs, cards };
                    ov.dock_apps = dock_apps;
                }
            }
        }
    });

    repaint_overview(overview_hwnd);
}

pub(crate) fn close_overview(monitor: &str, focus_after: Option<HWND>) {
    let overview_hwnd = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let ov = state.overviews.get_mut(monitor)?;
        let mode = std::mem::replace(&mut ov.mode, OverviewMode::Closed);
        match mode {
            OverviewMode::Open { thumbs, cards } | OverviewMode::Opening { thumbs, cards, .. } => {
                ov.mode = OverviewMode::Closing { started: Instant::now(), thumbs, cards, focus_after };
                Some(ov.hwnd)
            }
            other => {
                ov.mode = other;
                None
            }
        }
    });
    let Some(overview_hwnd) = overview_hwnd else {
        return;
    };
    // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
    unsafe {
        SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
    }
}
```

(The last `match` arm above restores `ov.mode` unchanged when it wasn't
`Open`/`Opening` — the original code's early-`None`-on-`Closed`/`Closing`
behavior, just expressed through the `Option` monitor lookup instead of a
second `STATE.with` pass. Apply the same restore-on-no-op pattern to every
other function in this task that does a `std::mem::replace` on `ov.mode`.)

- [ ] **Step 3: `toggle_overview_for` — replaces `movesize.rs`'s old `toggle_overview`**

Add to `overview.rs` (this becomes the single place both the bar's Activities
click, the Win-key tap, and hot corners call through):

```rust
pub(crate) fn toggle_overview_for(monitor: &str) {
    let is_closed = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.overviews.get(monitor))
            .map(|ov| matches!(ov.mode, OverviewMode::Closed))
    });
    match is_closed {
        Some(true) => open_overview(monitor),
        Some(false) => close_overview(monitor, None),
        None => {}
    }
}
```

- [ ] **Step 4: `snap_carousel_to`, `on_overview_char`, `on_overview_arrow`, `on_overview_hover`, drag start/move/end, `activate_search_result`**

Apply the same transformation to each: add `monitor: &str` as the first
parameter, replace every `state.overview`/`st.overview_hwnd`/`state.search_query`/
`state.carousel_*`/`state.window_drag`/`state.window_pop_anim`/`state.hover_thumb`/
`state.dock_apps`/`state.dock_hover` reference with `state.overviews.get(monitor)`
or `.get_mut(monitor)` then `.field`, propagating `None`/early-return exactly
as the pre-existing code did for a missing/wrong-mode `AppState`. These
functions are currently at:
  - `snap_carousel_to` — `overview.rs:1154` (leading parameter becomes
    `monitor: &str, target_index: usize, close_after: Option<HWND>`)
  - `on_overview_char` — `overview.rs:1364` (`monitor: &str, ch: u32`) — its
    internal call to `activate_search_result` becomes
    `activate_search_result(monitor, result)`
  - `activate_search_result` — near `overview.rs:1318` (`monitor: &str, result: SearchResult`) —
    its internal calls to `snap_carousel_to`/`close_overview` gain the
    `monitor` argument
  - `on_overview_arrow` — search for `fn on_overview_arrow` (`monitor: &str, delta: i32`)
  - `on_overview_hover` — search for `fn on_overview_hover` (`monitor: &str, x: i32, y: i32`)
  - `on_overview_drag_start` / `on_overview_drag_move` / `on_overview_drag_end` —
    search for each (`monitor: &str` leading parameter, same trailing
    `x`/`y` as today)
  - `on_animation_tick` — around `overview.rs:2316` (`monitor: &str`) — this
    one is driven by a `WM_TIMER` on a *specific* overview's `hwnd`, so its
    caller (Task 8) already knows which monitor to pass; no ambiguity here.

For each, grep the function body for `STATE.with` and change every
`st.overview` / `state.overview` (mode field), `.overview_hwnd`,
`.carousel_offset`, `.carousel_drag`, `.carousel_anim`, `.carousel_close_after`,
`.window_drag`, `.window_pop_anim`, `.hover_thumb`, `.dock_apps`, `.dock_hover`,
`.search_query` to go through `.overviews.get(monitor)` /
`.overviews.get_mut(monitor)` first, same as the worked examples in Steps 1-3
above.

- [ ] **Step 5: `paint_overview` resolves its monitor from its own `hwnd`**

`paint_overview` is called from `mod.rs`'s `WM_PAINT` dispatch, which already
has `role_of(hwnd)` available (Task 8 wires this). Change its signature to:

```rust
pub(crate) fn paint_overview(hwnd: HWND, monitor: &str) {
    // ...body: replace every `st.overview`/`state.overview` read with
    // `st.overviews.get(monitor)`/`.get_mut(monitor)`, same pattern as
    // above. `card_layout()` calls inside this function become
    // `card_layout(monitor)`; `search_layout(dpi, n)` calls become
    // `search_layout(monitor, dpi, n)`.
}
```

- [ ] **Step 6: `cargo check` — confirm `overview.rs`'s own internal call sites are consistent**

Run: `cargo check -p groveshell-ui 2>&1 | grep "overview.rs"`
Expected: remaining errors should only be about *callers outside*
`overview.rs` (i.e. `mod.rs`, `bar.rs`, `movesize.rs`, `workspaces.rs`) not
yet passing the new `monitor` argument — Task 8 (and the parts of Tasks 5/6
already done) resolve those. Any error still inside `overview.rs` itself at
this point is a real bug in this task's transformation and must be fixed
before continuing.

- [ ] **Step 7: Commit**

```bash
git add apps/ui/src/imp/overview.rs
git commit -m "feat: scope every Activities overview function to one monitor (WIP)"
```

---

### Task 8: `mod.rs` dispatch — resolve and pass the monitor through

**Files:**
- Modify: `apps/ui/src/imp/mod.rs:453-680` (`wndproc`)

**Interfaces:**
- Consumes: `Role::Bar { is_primary, monitor }`, `Role::Overview { monitor }` (Task 3),
  every changed function signature from Tasks 5-7

- [ ] **Step 1: Fix the two `role == Role::Overview` equality checks**

`Role::Overview` is now a fielded variant, so bare-variant equality no longer
compiles. Change:

```rust
WM_ERASEBKGND if role == Role::Overview || role == Role::QuickSettings => LRESULT(1),
```
to:
```rust
WM_ERASEBKGND if matches!(role, Role::Overview { .. }) || role == Role::QuickSettings => LRESULT(1),
```

and:
```rust
WM_CHAR if role == Role::Overview => {
```
to:
```rust
WM_CHAR if matches!(role, Role::Overview { .. }) => {
```
(then destructure `monitor` inside that arm's body — see Step 3).

- [ ] **Step 2: `WM_PAINT`**

```rust
WM_PAINT => match role {
    Role::Bar { is_primary, monitor } => {
        paint_bar(hwnd, is_primary, &monitor);
        LRESULT(0)
    }
    Role::Overview { monitor } => {
        paint_overview(hwnd, &monitor);
        LRESULT(0)
    }
    Role::Calendar => {
        paint_calendar(hwnd);
        LRESULT(0)
    }
    Role::QuickSettings => {
        paint_quick_settings(hwnd);
        LRESULT(0)
    }
    Role::Other => DefWindowProcW(hwnd, msg, wparam, lparam),
},
```

- [ ] **Step 3: `WM_LBUTTONDOWN`, `WM_MOUSEMOVE`, `WM_MOUSELEAVE`, `WM_LBUTTONUP`, `WM_CHAR`, `WM_KEYDOWN`**

```rust
WM_LBUTTONDOWN => {
    match role {
        Role::Overview { monitor } => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            on_overview_drag_start(&monitor, x, y);
            LRESULT(0)
        }
        Role::QuickSettings => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            on_quick_settings_mouse_down(hwnd, x, y);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
WM_MOUSEMOVE => {
    match role {
        Role::Overview { monitor } => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            if wparam.0 & (MK_LBUTTON.0 as usize) != 0 {
                on_overview_drag_move(&monitor, x, y);
            } else {
                on_overview_hover(&monitor, x, y);
            }
        }
        Role::QuickSettings => {
            let x = (lparam.0 & 0xFFFF) as i32;
            on_quick_settings_mouse_move(hwnd, x);
        }
        Role::Bar { is_primary: true, .. } => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            on_bar_hover(hwnd, x, y, true);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
WM_MOUSELEAVE => {
    if let Role::Bar { is_primary: true, .. } = role {
        on_bar_mouse_leave(hwnd);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
WM_LBUTTONUP => {
    match role {
        Role::Bar { is_primary, monitor } => {
            let x = (lparam.0 & 0xFFFF) as i32;
            on_bar_click(hwnd, x, is_primary, &monitor);
        }
        Role::Overview { monitor } => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            on_overview_drag_end(&monitor, x, y);
        }
        Role::QuickSettings => {
            on_quick_settings_mouse_up();
        }
        _ => {}
    }
    LRESULT(0)
}
WM_CHAR if matches!(role, Role::Overview { .. }) => {
    if let Role::Overview { monitor } = role {
        on_overview_char(&monitor, wparam.0 as u32);
    }
    LRESULT(0)
}
```

`on_bar_click`/`on_bar_hover` previously only fired for `Role::Bar { is_primary: true }`
(non-primary bars had no clickable regions at all). Since Task 5 makes
Activities+dots clickable on *every* bar, the `WM_LBUTTONUP` arm above now
matches `Role::Bar { is_primary, monitor }` unconditionally (not gated to
`is_primary: true`) — `on_bar_click` itself still internally gates the
clock/QS-pill hit tests behind `is_primary` (Task 5, Step 2). `WM_MOUSEMOVE`'s
hover highlight, by contrast, stays gated to `is_primary: true` in the match
arm itself, since only the primary bar has anything to highlight on hover
(Task 5, Step 3) — leave that arm's guard as `Role::Bar { is_primary: true, .. }`.

Update `WM_KEYDOWN`'s `Role::Overview` arm similarly:

```rust
WM_KEYDOWN => {
    if wparam.0 == VK_ESCAPE.0 as usize {
        match role {
            Role::Overview { monitor } => {
                let searching = STATE.with(|s| {
                    s.borrow_mut()
                        .as_mut()
                        .and_then(|st| st.overviews.get_mut(&monitor))
                        .map(|ov| {
                            let searching = !ov.search_query.is_empty();
                            ov.search_query.clear();
                            searching
                        })
                        .unwrap_or(false)
                });
                if searching {
                    repaint_overview(hwnd);
                } else {
                    close_overview(&monitor, None);
                }
            }
            Role::Calendar => hide_calendar(true),
            Role::QuickSettings => hide_quick_settings(true),
            _ => {}
        }
    } else if let Role::Overview { monitor } = &role {
        let searching = STATE
            .with(|s| {
                s.borrow().as_ref().and_then(|st| st.overviews.get(monitor.as_str()))
                    .map(|ov| !ov.search_query.is_empty())
            })
            .unwrap_or(false);
        if !searching {
            if wparam.0 == VK_LEFT.0 as usize {
                on_overview_arrow(monitor, -1);
            } else if wparam.0 == VK_RIGHT.0 as usize {
                on_overview_arrow(monitor, 1);
            }
        }
    }
    LRESULT(0)
}
```

- [ ] **Step 4: `WM_TIMER`'s `ANIM_TIMER_ID` — resolve the monitor from `hwnd` via `role_of`**

Each overview window has its own independent `ANIM_TIMER_ID` timer (armed
per-`hwnd` in `open_overview`/`close_overview`), so `WM_TIMER` already
delivers to the correct specific overview window — just resolve which one:

```rust
WM_TIMER => {
    match wparam.0 {
        ANIM_TIMER_ID => {
            if let Role::Overview { monitor } = role {
                on_animation_tick(&monitor);
            }
        }
        SYNC_TIMER_ID => on_window_sync_timer(hwnd),
        HOTCORNER_TIMER_ID => check_hot_corners(),
        DRAG_TIMER_ID => on_drag_timer(),
        CLOCK_TIMER_ID => { /* unchanged */ }
        _ => {}
    }
    LRESULT(0)
}
```

- [ ] **Step 5: `WM_DESTROY`'s hotkey/unpark cleanup — no monitor-specific change needed here**

The primary bar still owns the process-wide hotkeys/hooks; only the tracked-
windows unpark loop changed (already done in Task 4, Step 5). No further
change needed in this handler.

- [ ] **Step 6: `main()`'s hotkey registration is unaffected**

`RegisterHotKey`/`WM_HOTKEY` stay registered on `primary_bar_hwnd` exactly as
today — `switch_workspace_relative`/`move_focused_window_relative` (Task 6)
already resolve their *own* target monitor internally from the focused
window, so the `WM_HOTKEY` handler itself (`mod.rs:604-613`) needs no change.

- [ ] **Step 7: `cargo build --workspace`**

Run: `cargo build --workspace 2>&1 | tail -60`
Expected: clean build. This is the first point since Task 3 where the whole
workspace compiles again. If there are remaining errors, they're almost
certainly leftover call sites in `overview.rs` (Task 7, Step 4's checklist)
that were missed — fix those, not new code here.

- [ ] **Step 8: `cargo test --workspace`**

Run: `cargo test --workspace`
Expected: all existing tests pass (the `workspace.rs` suite, this plan's new
`monitors.rs`/`monitor_workspaces.rs` tests), no regressions.

- [ ] **Step 9: Commit**

```bash
git add apps/ui/src/imp/mod.rs
git commit -m "feat: wndproc resolves and threads the acting monitor through overview/bar handlers"
```

---

### Task 9: Hot corners and Win-key tap resolve the monitor under the cursor

**Files:**
- Modify: `apps/ui/src/imp/movesize.rs:170-181` (`toggle_overview`), `:371-395`
  (`check_hot_corners`)

**Interfaces:**
- Consumes: `monitors::monitor_key_at_point` (Task 1), `overview::toggle_overview_for`/`open_overview` (Task 7)

- [ ] **Step 1: `toggle_overview` (Win-key tap) resolves the monitor under the cursor**

```rust
fn toggle_overview() {
    let mut pt = POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let Some(monitor) = super::monitors::monitor_key_at_point(pt) else {
        return;
    };
    super::overview::toggle_overview_for(&monitor);
}
```

Remove the now-unused `is_closed`/`STATE` lookup from this function (folded
into `toggle_overview_for`) and drop the `open_overview`/`close_overview`
imports from this file's `use super::overview::{...}` if nothing else in
`movesize.rs` still calls them directly (check `check_hot_corners` below
first).

- [ ] **Step 2: `check_hot_corners` opens the specific monitor's overview**

```rust
pub(crate) fn check_hot_corners() {
    let mut pt = POINT::default();
    // SAFETY: plain query, no preconditions.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let monitors = monitors_sorted_by_x();
    let corner_monitor = monitors.iter().find(|m| {
        pt.x >= m.rect.left
            && pt.x < m.rect.left + HOTCORNER_ZONE
            && pt.y >= m.rect.top
            && pt.y < m.rect.top + HOTCORNER_ZONE
    });
    let in_corner = corner_monitor.is_some();
    let was_in_corner = IN_HOT_CORNER.with(|c| c.replace(in_corner));
    if in_corner && !was_in_corner {
        if let Some(m) = corner_monitor {
            super::overview::open_overview(&m.device_name);
        }
    }
}
```

- [ ] **Step 3: `cargo build -p groveshell-ui`**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add apps/ui/src/imp/movesize.rs
git commit -m "feat: hot corners and Win-key tap open the overview for whichever monitor the cursor is on"
```

---

### Task 10: Monitor hotplug — live connect/disconnect

**Files:**
- Create: `apps/ui/src/imp/hotplug.rs`
- Modify: `apps/ui/src/imp/mod.rs` (register `mod hotplug;`, add
  `WM_DISPLAYCHANGE` to `wndproc`, extend `WM_DESTROY`'s teardown loop to
  iterate every monitor's overview, not just a single one)

**Interfaces:**
- Consumes: `enumerate_monitors`, `MonitorWorkspaces`, `OverviewInstance`,
  `register_appbar`/`unregister_appbar`, `park_window`/`unpark_window`
- Produces: `reconcile_monitors(hinstance: HINSTANCE) -> Result<()>`, called
  from `wndproc`'s new `WM_DISPLAYCHANGE` arm

- [ ] **Step 1: Write `reconcile_monitors`**

```rust
//! Live monitor hotplug: reconciles `AppState`'s per-monitor bars,
//! workspace trackers, and overview windows against the real, current
//! monitor topology whenever Windows reports a `WM_DISPLAYCHANGE`. See
//! `docs/superpowers/specs/2026-07-28-per-monitor-workspaces-design.md`
//! §A/§F.

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, RGN_OR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, MoveWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use groveshell_common::{Error, Result};
use groveshell_window_model::workspace::WorkspaceTracker;

use super::bar::{register_appbar, unregister_appbar};
use super::monitors::enumerate_monitors;
use super::overview::OverviewInstance;
use super::state::{scaled, BarWindow, BAR_CORNER_RADIUS, BAR_HEIGHT, STATE};
use super::workspaces::{park_window, unpark_window};

/// Re-runs monitor enumeration and diffs it against `AppState.bars` by
/// device name: any newly-connected monitor gets a bar, a workspace
/// tracker, and an overview window; any monitor that's now missing has
/// its windows reassigned to the primary monitor's current workspace
/// and its bar/tracker/overview torn down.
pub(crate) fn reconcile_monitors(hinstance: HINSTANCE) -> Result<()> {
    let monitors = enumerate_monitors();
    let current_names: Vec<String> = monitors.iter().map(|m| m.device_name.clone()).collect();

    let existing_names: Vec<String> = STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.bars.iter().map(|b| b.monitor.clone()).collect()).unwrap_or_default()
    });

    // Disconnected: anything tracked that's no longer in the live list.
    for removed in existing_names.iter().filter(|n| !current_names.contains(n)) {
        remove_monitor(removed);
    }

    // Connected: anything live that isn't tracked yet.
    for monitor in monitors.iter().filter(|m| !existing_names.contains(&m.device_name)) {
        add_monitor(hinstance, monitor)?;
    }

    // A surviving monitor's primary status or geometry may have
    // changed (e.g. the old primary was unplugged and Windows promoted
    // another one) — refresh `primary_monitor`/`primary_bar_hwnd` to
    // match reality.
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
                state.primary_monitor = primary.device_name.clone();
                if let Some(bar) = state.bars.iter().find(|b| b.monitor == primary.device_name) {
                    state.primary_bar_hwnd = bar.hwnd;
                    state.primary_bar_rect = bar.rect;
                }
            }
        }
    });

    Ok(())
}

fn add_monitor(hinstance: HINSTANCE, monitor: &super::monitors::MonitorInfo) -> Result<()> {
    // SAFETY: mirrors the startup bar-creation loop in `mod.rs::main`
    // exactly (same window class, same AppBar registration, same
    // rounded-corner region), just for one monitor after the fact.
    unsafe {
        let width = monitor.rect.right - monitor.rect.left;
        let bar_height = scaled(BAR_HEIGHT, monitor.dpi);
        let bar_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("GroveShellBar"),
            w!("GroveShell"),
            WS_POPUP | WS_VISIBLE,
            monitor.rect.left,
            monitor.rect.top,
            width,
            bar_height,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(Error::Windows)?;

        let bar_rect = register_appbar(bar_hwnd, monitor.rect.left, monitor.rect.top, width, bar_height);
        let _ = MoveWindow(bar_hwnd, bar_rect.left, bar_rect.top, bar_rect.right - bar_rect.left, bar_rect.bottom - bar_rect.top, true);

        let radius = scaled(BAR_CORNER_RADIUS, monitor.dpi);
        let region_w = bar_rect.right - bar_rect.left;
        let region_h = bar_rect.bottom - bar_rect.top;
        let region = CreateRoundRectRgn(0, 0, region_w + 1, region_h + 1, radius * 2, radius * 2);
        let top_square = CreateRectRgn(0, 0, region_w + 1, (region_h - radius).max(0));
        CombineRgn(region, region, top_square, RGN_OR);
        let _ = DeleteObject(top_square);
        SetWindowRgn(bar_hwnd, region, true);

        let overview_width = monitor.rect.right - monitor.rect.left;
        let overview_height = monitor.rect.bottom - monitor.rect.top;
        let overview_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("GroveShellOverview"),
            w!("GroveShell Activities"),
            WS_POPUP,
            monitor.rect.left,
            monitor.rect.top,
            overview_width,
            overview_height,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(Error::Windows)?;

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.bars.push(BarWindow {
                    hwnd: bar_hwnd,
                    rect: bar_rect,
                    is_primary: monitor.is_primary,
                    monitor: monitor.device_name.clone(),
                });
                state.workspaces.insert_monitor(monitor.device_name.clone(), WorkspaceTracker::with_monitor_workspaces(1, 0));
                state.overviews.insert(monitor.device_name.clone(), OverviewInstance::new(overview_hwnd));
            }
        });
        let _ = InvalidateRect(bar_hwnd, None, true);
    }
    Ok(())
}

fn remove_monitor(device_name: &str) {
    let (bar_hwnd, overview_hwnd, orphaned_windows, primary) = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return (None, None, Vec::new(), String::new());
        };
        let bar_hwnd = state.bars.iter().position(|b| b.monitor == device_name).map(|i| state.bars.remove(i).hwnd);
        let overview_hwnd = state.overviews.remove(device_name).map(|ov| ov.hwnd);
        let orphaned = state.workspaces.remove_monitor(device_name)
            .map(|t| t.workspace_ids().to_vec().into_iter().flat_map(|id| t.windows_on(id)).collect())
            .unwrap_or_default();
        (bar_hwnd, overview_hwnd, orphaned, state.primary_monitor.clone())
    });

    // Reassign every orphaned window onto the primary monitor's
    // current workspace, un-parking any that were parked on a
    // background workspace of the removed monitor (they must become
    // visible now — there's no monitor left to hide them off of).
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if let Some(tracker) = state.workspaces.get_mut(&primary) {
                let target = tracker.current_index();
                for hwnd in &orphaned_windows {
                    tracker.assign_to_index(*hwnd, target);
                }
            }
        }
    });
    for hwnd in orphaned_windows {
        unpark_window(HWND(hwnd as *mut std::ffi::c_void));
    }

    // SAFETY: both handles, if present, were valid windows created by
    // this process; destroying an already-torn-down window would be a
    // caller bug, not the case here since each is removed from
    // `AppState` exactly once, right before this call.
    unsafe {
        if let Some(hwnd) = overview_hwnd {
            let _ = DestroyWindow(hwnd);
        }
        if let Some(hwnd) = bar_hwnd {
            unregister_appbar(hwnd);
            let _ = DestroyWindow(hwnd);
        }
    }
}
```

`park_window` is imported but unused in this file directly (only
`unpark_window` is called) — remove that half of the `use` line if `cargo
check` flags it.

- [ ] **Step 2: Register the module and wire `WM_DISPLAYCHANGE`**

In `mod.rs`'s `mod` list, add `mod hotplug;` (alphabetically after
`mod dock;` and before `mod icons;`).

In `wndproc`, add a new arm (anywhere among the other `WM_*` arms, e.g. right
after `WM_HOTKEY`):

```rust
WM_DISPLAYCHANGE => {
    // SAFETY: `GetModuleHandleW(None)` is a plain, idempotent query;
    // `hinstance` is only used to create new windows for newly
    // connected monitors, exactly as at startup.
    if let Ok(module) = unsafe { GetModuleHandleW(None) } {
        let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);
        let _ = hotplug::reconcile_monitors(hinstance);
    }
    LRESULT(0)
}
```

Add `hotplug` to the module list and nothing else needs importing at the top
of `mod.rs` (the arm above calls `hotplug::reconcile_monitors` via its full
path).

- [ ] **Step 3: `cargo build -p groveshell-ui`**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean.

- [ ] **Step 4: Manual verification**

No automated UI test exists for live hotplug in this codebase (consistent
with this project's established practice of manual verification for
Win32-integration behavior — see `docs/superpowers/specs/2026-07-28-per-monitor-workspaces-design.md`'s
Testing section). Run `.\scripts\dev-start.ps1`, then:
1. Plug in a second monitor. Confirm a new bar and a working Activities
   button/overview appear on it within a second or two, without restarting
   GroveShell.
2. Switch workspaces independently on each monitor (`Ctrl+Alt+Left/Right`
   with focus on each monitor in turn) and confirm they don't affect each
   other.
3. Unplug the second monitor while it has a window open on a background
   (non-current) workspace. Confirm that window reappears, unparked, on the
   primary monitor, and that the second monitor's bar/overview are gone
   (check Task Manager or Spy++ if unsure whether the windows were actually
   destroyed, not just hidden).

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/hotplug.rs apps/ui/src/imp/mod.rs
git commit -m "feat: live monitor hotplug — create/destroy bar, tracker, and overview on connect/disconnect"
```

---

### Task 11: Auto-reassign a window when it physically moves to another monitor

**Files:**
- Modify: `apps/ui/src/imp/workspaces.rs` (`sync_workspaces`, extending
  Task 6 Step 4's version)

**Interfaces:**
- Consumes: `monitor_index_for_center`, `MonitorWorkspaces::monitor_of_window`/`get_mut`

- [ ] **Step 1: Write the failing test**

This behavior is a pure function of "window rect -> which monitor" plus
tracker reassignment, both already unit-testable without Win32. Add to
`apps/ui/src/imp/monitor_workspaces.rs`'s test module:

```rust
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
```

Run: `cargo test -p groveshell-ui reassigning_a_window_to_a_new_monitor`
Expected: PASS already (this test only exercises Task 2's existing API —
it's here to lock in the exact sequence `sync_workspaces` needs to perform,
written first per TDD before wiring it into the live sync path below).

- [ ] **Step 2: Extend `sync_workspaces` to detect a monitor mismatch for already-tracked windows**

In `workspaces.rs`'s `sync_workspaces` (Task 6, Step 4), the existing loop
does `if state.workspaces.monitor_of_window(window.hwnd).is_some() { continue; }`
— skipping any window already assigned somewhere. Change that to also check
*whether* its current monitor still matches its real on-screen monitor, and
reassign if not:

```rust
for window in &live {
    let (_, reused) = state.window_registry.observe(window.hwnd, window.pid);
    if reused {
        for name in state.workspaces.device_names().map(str::to_string).collect::<Vec<_>>() {
            if let Some(tracker) = state.workspaces.get_mut(&name) {
                tracker.forget(window.hwnd);
            }
        }
        drop_window_snapshot(window.hwnd);
    }

    let center_x = (window.rect.left + window.rect.right) / 2;
    let center_y = (window.rect.top + window.rect.bottom) / 2;
    let real_monitor = super::monitors::monitor_index_for_center(&monitors, center_x, center_y)
        .and_then(|i| monitors.get(i))
        .map(|m| m.device_name.clone());

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
}
```

This replaces the `if ... { continue; }` early-skip from Task 6, Step 4 with
a `match` that also handles the "already tracked, but on the wrong monitor
now" case. Note this only fires for windows on a *currently visible*
(on-screen, not parked) real position — a parked window's `rect` reads its
off-screen coordinates (`top >= WORKSPACE_PARK_DY / 2`), which
`monitor_index_for_center` would resolve to whatever monitor happens to sit
at that far-below-everything y-coordinate (usually none, since it's
`20000px` below any real monitor) — `monitor_index_for_center` returns
`None` in that case, and the `match`'s `(Some(tracked), None)` combination
isn't handled above, meaning it silently falls through doing nothing
(correct: a parked window's monitor assignment shouldn't change just because
it's temporarily off-screen).

- [ ] **Step 3: Run the full test suite and the manual check**

Run: `cargo test --workspace`
Expected: all pass, including the new test from Step 1.

Manual check (no automated coverage for real window drag, same rationale as
Task 10, Step 4): with two monitors, drag a window from monitor A to monitor
B by its title bar. Confirm it now shows up in monitor B's overview/dots
rather than A's on the next sync (within ~250ms, the existing
`SYNC_DEBOUNCE_MS`).

- [ ] **Step 4: Commit**

```bash
git add apps/ui/src/imp/workspaces.rs apps/ui/src/imp/monitor_workspaces.rs
git commit -m "feat: auto-reassign a window's workspace when it's physically moved to another monitor"
```

---

### Task 12: Documentation

**Files:**
- Modify: `README.md` (`What works today`, `Roadmap` §Phase 3/4)
- Modify: `docs/PROJECT_PLAN.md` §16 if it separately describes the old
  global-workspace/single-overview model (check for a section describing
  "one `WorkspaceTracker`"/"virtual-screen-wide overview" and update it to
  match; if no such section exists, skip this file)

- [ ] **Step 1: Update `README.md`'s "What works today" workspace bullet**

Replace the existing bullet (`README.md:53-57`, "GNOME-style workspaces...")
with:

```markdown
- GNOME-style workspaces, independent per monitor. Each monitor has its own
  pinned workspace and its own dynamic tail (a spare empty workspace always
  waiting at the end), and its own Activities overview — switching
  workspaces on one monitor never affects any other. `Ctrl+Alt+←/→` switches
  the workspace on whichever monitor currently has keyboard focus,
  `Ctrl+Alt+Shift+←/→` sends the focused window away on that same monitor.
  Windows on inactive workspaces are parked off-screen rather than hidden,
  with a snapshot taken at park time so previews don't go blank when apps
  stop rendering. Monitors can be connected or disconnected while GroveShell
  is running: a new monitor gets its own bar, workspace set, and overview
  within a second or two, and unplugging one hands its windows back to the
  primary monitor's current workspace rather than stranding them.
```

Update the top bar bullet (`README.md:30-33`) to mention every bar now
carries Activities + workspace dots, not just the primary one:

```markdown
- A top bar on every monitor: Activities button and workspace dots (each
  reflecting that monitor's own independent workspace set), plus — on the
  primary monitor only, since these reflect machine-wide state rather than
  anything per-monitor — a clock with a calendar flyout and a Wi-Fi/volume/
  battery status pill. Every bar is per-monitor DPI aware and reserves its
  strip through the same AppBar mechanism the real taskbar uses.
```

- [ ] **Step 2: Update the "Activities overview" bullet**

Replace `README.md:63-71`'s opening sentence to reflect one overview per
monitor:

```markdown
- The Activities overview: one per monitor, each scoped to only that
  monitor's own workspaces — fixed-size workspace cards in a draggable
  carousel. ...
```//! (keep the rest of the bullet's content — carousel/search/drag
   description — unchanged after that opening clause)
```

- [ ] **Step 3: Update the Phase 3/4 roadmap checkboxes**

In `README.md`'s roadmap section, Phase 3 (`:117-124`):

```markdown
### Phase 3: Managed workspaces (partial)
- [x] Workspace domain model (pure, unit-tested) with the dynamic empty-tail policy
- [x] Independent per-monitor workspace sets: one pinned workspace and one
  dynamic tail per monitor, with live hotplug (new monitors get their own
  set; disconnected ones hand their windows to the primary monitor)
- [x] Keyboard switching and move-window shortcuts, resolved to whichever
  monitor currently has keyboard focus
- [x] Park/unpark instead of hide/show, with crash recovery on the next start
- [ ] Session persistence across restarts
- [ ] Owned dialogs following their owner window
```

And Phase 5 (`:136-142`), first bullet:

```markdown
- [x] One Activities overview per monitor, each with its own carousel
  layout engine and focused-card scaling, scoped to only that monitor's
  own workspaces
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: describe per-monitor workspaces, overview, and hotplug"
```

---

### Task 13: Final full verification

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `cargo build --workspace`
Expected: clean, no warnings introduced by this feature (pre-existing
`overview.rs` clippy lints noted in earlier sessions — the let-else and
`sort_by_key` ones — are unrelated and may remain).

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: all tests pass — the pre-existing `workspace.rs` suite (unchanged,
20 tests per the last recorded run), plus this plan's new tests in
`monitors.rs` (2) and `monitor_workspaces.rs` (5, including Task 11's).

- [ ] **Step 3: Clippy**

Run: `cargo clippy -p groveshell-ui --no-deps 2>&1 | tail -60`
Expected: no new warnings beyond the two pre-existing ones already known
(`overview.rs`'s let-else-question-mark and `sort_by_key` lints).

- [ ] **Step 4: Manual smoke test on the actual dev machine**

Run `.\scripts\dev-start.ps1` with two monitors connected from the start.
Confirm:
- Both monitors show a bar with a working Activities button and workspace
  dots.
- Only the primary monitor's bar shows the clock and Quick Settings pill.
- Switching workspaces on one monitor doesn't move the other.
- Each monitor's hot corner opens that monitor's own overview.
- The search bar inside each monitor's overview is fully visible and
  correctly scaled (the original bug report this whole feature grew out of).
- Dragging a window from one monitor to the other moves it into the target
  monitor's current workspace.
- Unplugging one monitor mid-session brings its windows back onto the
  primary monitor without a restart, and reconnecting it creates a fresh
  bar/workspace/overview for it.

- [ ] **Step 5: Final commit (if the smoke test surfaced any fixes)**

```bash
git add -A
git commit -m "fix: address issues found in full per-monitor workspaces smoke test"
```

(Skip this step entirely if the smoke test found nothing to fix.)
