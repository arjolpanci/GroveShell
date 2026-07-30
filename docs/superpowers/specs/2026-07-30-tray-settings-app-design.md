# Phase 4 finisher: `groveshell-settings` tray/launcher app

## Problem

Phase 4 (`docs/PROJECT_PLAN.md` section 16) calls for the top bar and dock,
both now implemented, plus "settings for bar height, dock mode, and
keybindings." There is currently no such settings surface, and more
fundamentally no single "main app" a user runs to turn GroveShell on: a
developer starts `groveshell-watchdog`, `groveshell-host`, and
`groveshell-ui` via `scripts/dev-start.ps1`, and there is no tray presence,
no live health/stats view, and no way to customize anything short of
hand-editing `config.toml` (which today's `apps/ui` does not even read).

This spec covers a new tray-resident app that becomes GroveShell's real
entry point: it launches and supervises the other three processes, shows
their health, offers a one-click "restore Explorer" toggle, and hosts a
settings window covering dock, top-bar, overview, and input customization.

## Scope

**In scope:**
- A new binary crate `apps/settings` (`groveshell-settings.exe`) that is
  the process a user actually starts (manually or via Windows autostart).
- Tray icon (`Shell_NotifyIconW`) with a context menu: open settings,
  restore-Explorer/start-GroveShell toggle, exit.
- Process lifecycle ownership: spawns `watchdog` → `host` → `ui` in order,
  tracks their handles, and can gracefully stop/restart them.
- A settings window (native Win32 + GDI/GDI+, Fluent-styled to match the
  existing bar/calendar look) with: health/stats, dock customization,
  top-bar customization, overview customization, input rebinding, and
  Start-with-Windows.
- New `groveshell-config` fields backing all of the above, plus the first
  real consumption of `groveshell-config` by `apps/ui` (today it reads
  nothing from config at all).
- A `config.reload` IPC message so `apps/ui` picks up settings changes
  live, without a restart.
- Generating an app icon (`.ico`) from `media/logo.png` for the exe and
  tray icon.

**Explicitly out of scope for this pass:**
- A true vertical/side-docked dock layout. `dock.rs`'s geometry assumes a
  single horizontal row anchored to the bottom edge; "dock position" here
  means horizontal alignment (left/center/right) along that same bottom
  edge, not moving the dock to a screen side. A real side-dock is a
  layout-engine rework, not a settings-screen change.
- Arbitrary key-combo rebinding (a full key-capture UI). The overview/
  move-resize trigger is exposed as a small preset list (Super / Alt /
  Ctrl+Alt), not an arbitrary recorder.
- Mica/acrylic backdrop material (`DWM_SYSTEMBACKDROP_TYPE`). Blur toggles
  use `DwmEnableBlurBehindWindow` (simple on/off, works on any supported
  Windows version) instead.
- WinUI 3 / Windows App SDK / XAML Islands. Considered and rejected: it
  would require a runtime dependency and COM/WinRT interop with no
  precedent anywhere in this codebase, for a settings screen that doesn't
  need it.
- Any change to `apps/host`'s or `apps/watchdog`'s own responsibilities
  beyond adding the new `config.reload`-adjacent IPC message type constant
  to `groveshell-ipc`. Host still doesn't spawn `ui`; the settings app
  does, replacing that part of `dev-start.ps1`'s job for real usage.
- Removing or rewriting `scripts/dev-start.ps1` (it stays useful for
  developers who want the console dashboard and don't want a tray icon
  running); `scripts/recover.ps1` is unaffected.

## Design

### 1. Crate layout and process role

`apps/settings/` is added to the workspace (picked up automatically via the
root `Cargo.toml`'s `apps/*` glob member pattern). It depends on
`groveshell-common`, `groveshell-config`, `groveshell-ipc`, and the
`windows` crate (`Win32_UI_Shell` for `Shell_NotifyIconW`/`NOTIFYICONDATAW`,
`Win32_System_Threading`/`Win32_System_ProcessStatus` for CPU/RAM sampling,
plus the same GDI/GDI+ features `apps/ui` already uses for its window).

On launch:
- Acquires a single-instance named mutex (`Local\GroveShell-Settings-SingleInstance`),
  same pattern as `apps/host`'s `Local\GroveShell-Host-SingleInstance`. A
  second launch (e.g. from a Start Menu shortcut when it's already running)
  just focuses the existing tray icon's window instead of starting a
  second copy.
- Loads `config.toml` via `groveshell_config::load_or_default`.
- Spawns `groveshell-watchdog.exe`, waits briefly, spawns
  `groveshell-host.exe`, then `groveshell-ui.exe` — same order and
  same "hidden window, redirect stderr to a log file" shape as
  `dev-start.ps1`'s `Start-Watchdog`/`Start-HostProcess`/`Start-Ui`,
  tracking each as a `std::process::Child`.
- Registers the tray icon (`Shell_NotifyIconW` with `NIM_ADD`), using the
  generated `.ico` (§6), tooltip "GroveShell".
- Creates (but does not initially show) the settings window.

Tray icon interaction:
- **Left click**: show/focus the settings window (create if not yet
  created, restore+foreground if minimized/hidden).
- **Right click**: native `TrackPopupMenu` with:
  - "Open GroveShell Settings"
  - "Restore Explorer" (when GroveShell's UI is running) / "Start GroveShell"
    (when it isn't) — see below.
  - "Exit GroveShell"

**Restore Explorer / Start GroveShell** (also reachable as a button on the
settings window's Home/Status page):
- *Restore Explorer* (stop path): finds the `GroveShellBar`-classed window
  belonging to the tracked `ui` child and posts `WM_CLOSE` to it (triggering
  `ui`'s existing `WM_DESTROY` handler, which restores the real taskbar and
  work areas — see `apps/ui/src/imp/mod.rs` and `imp/taskbar.rs`), waits up
  to 3 seconds, then force-kills if it hasn't exited. Then sends
  `host.shutdown` and `watchdog.shutdown` over their existing IPC pipes
  (falling back to `TerminateProcess` after a short timeout each). The
  settings app itself and its tray icon keep running. The tray menu label
  and Home page now read "Start GroveShell."
- *Start GroveShell* (start path): re-runs the same spawn sequence used at
  launch (watchdog → host → ui).
- **Exit GroveShell**: runs the stop path above, then removes the tray icon
  (`NIM_DELETE`) and exits the process.

**Autostart**: the settings window's Start-with-Windows checkbox writes
(or removes) a `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
pointing at `groveshell-settings.exe` (not `host` — this is the process a
user wants launched at login, since it launches everything else) and
updates `config.toml`'s `general.start_with_windows` to match, so the
checkbox reflects reality even if the registry was edited by hand. This is
the first code that ever acts on that config field.

### 2. Health & stats

No new bidirectional stats protocol. While the settings window is open, a
timer (every 2 seconds) does, per tracked process (`watchdog`, `host`,
`ui`, found via the `Child` handles this app itself owns, or by process
name if the settings app was restarted independently and is reattaching):
- CPU%: two `GetProcessTimes` samples 2 seconds apart, standard
  kernel+user delta over wall-clock delta calculation.
- Memory: `GetProcessMemoryInfo`'s `WorkingSetSize`.
- Liveness: is the PID still present (`OpenProcess`/`GetExitCodeProcess`).

Overall health = all three processes alive **and** a `host.ping` round trip
(reusing `groveshell-ipc`'s existing envelope framing and the pipe `host`
already serves) completes within a short timeout (e.g. 500ms). "Healthy"/
"Unhealthy" is shown with the specific reason when unhealthy (which
process is down, or the ping timed out).

### 3. Config schema additions

In `crates/config/src/model.rs`:

```rust
pub struct AppearanceConfig {
    pub top_bar_height: u32,      // existing
    pub dock_mode: String,        // existing: "overview" | "always" | "autohide"
    pub animation_scale: f32,     // existing
    pub dock_icon_size: u32,      // new, default 44
    pub dock_alignment: String,   // new, default "center": "left" | "center" | "right"
    pub top_bar_blur: bool,       // new, default false
    pub overview_blur: bool,      // new, default false
    pub reduced_motion: bool,     // new, default false
}

pub struct InputConfig {
    pub move_modifier: String,    // existing (currently unused by apps/ui)
    pub move_button: String,      // existing
    pub resize_button: String,    // existing
    pub overview_modifier: String, // new, default "Super": "Super" | "Alt" | "CtrlAlt"
}
```

`hot_corners` already models per-corner action/delay/fullscreen-suppression
as a `BTreeMap<String, HotCornerConfig>`; no schema change needed there,
only new UI to edit existing fields. `Config::validate()` gains checks for
the three new string-enum fields (dock_alignment, overview_modifier)
alongside its existing hot-corner-action and schema-version checks.
Schema version stays 1 (additive, defaulted fields — see `serde(default)`
usage already established for other fields in this struct).

### 4. `apps/ui` becomes a config consumer (new)

`apps/ui/Cargo.toml` gains a `groveshell-config` dependency. At startup
(`imp/mod.rs`'s `main()`), it loads `config.toml` via
`groveshell_config::load_or_default` and applies:
- `appearance.top_bar_height` → replaces the hardcoded `BAR_HEIGHT`
  constant as the bar window's actual height.
- `appearance.dock_icon_size` → replaces `DOCK_ICON_SIZE` in `dock.rs`.
- `appearance.dock_alignment` → shifts the dock's horizontal anchor point
  in its layout calculation (`dock.rs`'s content-width-based centering
  becomes left/center/right anchored against the work area width).
- `appearance.dock_mode` → already has a runtime concept (overview-only
  today is implied default); this wires the always-visible and autohide
  variants.
- `appearance.animation_scale` / `reduced_motion` → multiplies into the
  existing `ease_out`/`progress` animation-timing helpers in `imp/util.rs`;
  `reduced_motion` forces effectively-instant transitions regardless of
  the scale value.
- `appearance.top_bar_blur` / `overview_blur` → `DwmEnableBlurBehindWindow`
  toggled on the bar window and the overview window respectively at
  startup and on reload.
- `input.overview_modifier` → `movesize.rs`'s keyboard hook currently
  hardcodes `VK_LWIN`/`VK_RWIN`; this becomes a config-driven vkcode (or
  vkcode pair for Ctrl+Alt, requiring both keys held) that the hook checks
  instead.

**Live reload**: `apps/ui` binds its own named pipe (`groveshell-ui`, via
the same `groveshell_ipc::pipe`/`envelope` machinery `host`/`watchdog`
already use) listening for a new `config.reload` message type (added as a
constant alongside the existing `host.ping`/`watchdog.heartbeat` ones in
`crates/ipc/src/envelope.rs`). On receipt, `ui` reloads `config.toml` and
re-applies every field above in place — re-registering the keyboard hook
with the new modifier, re-running the blur toggle, recalculating dock/bar
geometry and repainting — without restarting the process or losing window
tracking state. The settings app sends this message (best-effort, ignored
if `ui` isn't running) immediately after every successful `config.toml`
save.

### 5. Settings window UI

One window, `Role::Settings`-style but its own top-level app (not part of
`apps/ui`'s shared `wndproc`, since this is a separate binary). Visual
language matches the bar/calendar: `0x202020`/`0x303030` panel
backgrounds, `0xE0E0E0` text, Segoe UI via the same DPI-aware font-creation
approach `util::bar_font` uses, rounded-rect panels at a similar corner
radius to the bar's `BAR_CORNER_RADIUS`. Layout: a left-hand vertical nav
list (~180px wide) and a right-hand content pane, both owner-drawn.

Pages:
- **Home / Status**: health indicator (colored dot + text), a small table
  of watchdog/host/ui with CPU%/RAM/running-state, the Restore Explorer /
  Start GroveShell button, and the Start-with-Windows checkbox.
- **Dock**: alignment (three-way segmented control: left/center/right),
  icon size (a hand-drawn slider, matching `quick_settings.rs`'s existing
  volume-slider drawing/hit-testing pattern), mode (dropdown: overview-only
  / always visible / autohide).
- **Top Bar**: height (slider), blur (toggle switch).
- **Overview**: blur (toggle), reduced motion (toggle), animation speed
  (slider, disabled when reduced motion is on).
- **Input**: overview/move-resize trigger (three-way choice: Super / Alt /
  Ctrl+Alt), four hot-corner dropdowns (one per screen corner, options
  drawn from the existing set of valid `HotCornerConfig::action` values).

Every control writes straight to an in-memory `Config` copy and calls
`groveshell_config::save` (existing atomic write + backup-rotation
behavior, unchanged) on each committed change (toggle flip, slider release,
dropdown selection) — no separate "Apply"/"Save" button, matching the
immediacy of the real Windows Settings app. Each successful save also
triggers the `config.reload` push described above.

### 6. App icon

`media/logo.png` (1024×1024 PNG) is converted once to a multi-resolution
`.ico` (16/32/48/256) and committed as `apps/settings/resources/icon.ico`.
A `build.rs` using the `embed-resource` crate (new workspace dependency,
standard for this purpose, no runtime cost) embeds it as the executable's
own icon. The same `.ico` is loaded at runtime via `LoadImageW`
(`IMAGE_ICON`, small + large sizes) for `Shell_NotifyIconW`'s `hIcon` and
for the settings window's title-bar icon.

## Testing

Pure-logic unit tests, consistent with this codebase's existing
Win32-adjacent testing convention (e.g. `shift_rect`,
`owned_windows_from_pairs`):
- `Config` validation/defaults for every new field (invalid
  `dock_alignment`/`overview_modifier` strings rejected, valid ones round
  trip through TOML).
- Dock alignment → anchor-x calculation (given work-area width, content
  width, and an alignment, produce the correct left edge) as a pure
  function, mirroring how `dock.rs`'s existing centering math is
  structured.
- CPU% calculation from two synthetic `(kernel_time, user_time,
  wall_time)` samples.
- Hot-corner action validation (already covered by existing `Config`
  tests; extended for the new UI's write path).

Everything else — the tray icon, process spawn/graceful-stop sequencing,
live `config.reload` round trip, `DwmEnableBlurBehindWindow`, autostart
registry write — is manual-verification-only, consistent with the rest of
this codebase's Win32-integration testing convention. Manual verification
checklist:
- Fresh launch: watchdog → host → ui come up in order, tray icon appears,
  Home page reports Healthy.
- Restore Explorer: real taskbar reappears, Start menu works, GroveShell's
  bar/dock/overview are gone, tray menu now offers Start GroveShell.
- Start GroveShell (after Restore): all three processes relaunch, bar/dock
  return.
- Changing dock alignment/icon size, top-bar height, blur toggles, and the
  overview-trigger modifier all take effect on the running `ui` process
  without restarting it.
- Killing `ui` (or `host`, or `watchdog`) out from under the settings app
  is reflected as Unhealthy with the correct reason within a couple of
  health-timer ticks.
- Start-with-Windows checkbox creates/removes the expected `Run` registry
  value and survives a real login cycle.
- Exit GroveShell cleanly stops everything and removes the tray icon.
