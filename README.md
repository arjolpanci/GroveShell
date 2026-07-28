<p align="center">
  <img src="media/logo.png" alt="GroveShell logo" width="160">
</p>

# GroveShell

An experimental desktop shell for Windows 11, built around workspaces and an
Activities-style overview instead of the taskbar-and-Start-menu flow. Written
in Rust against the raw Win32 API.

While GroveShell runs, it draws its own bar along the top of the screen, hides
the Windows taskbar, and hands the taskbar's screen space back to your apps.
Workspaces sit one keystroke away. Quit it and everything comes back: the
taskbar, the work areas, and any windows it had parked. Explorer keeps running
underneath the whole time. Nothing here replaces system components, and there
is a watchdog plus a standalone recovery script for the day a bad build
misbehaves.

![The Activities overview](media/activities-overview.png)

This is the overview: workspace cards with your actual wallpaper, window
previews with app icons, and a carousel you can drag between workspaces.
Opening zooms out of the current workspace. Closing zooms back into the one
you picked.

![Dragging between workspaces](media/workspace-drag.png)

## What works today

- A top bar on every monitor: Activities button, workspace indicator dots, a
  clock with a calendar flyout, battery, and quick settings. It is
  per-monitor DPI aware and reserves its strip through the same AppBar
  mechanism the real taskbar uses.
- GNOME-style workspaces. Each monitor is a pinned workspace, and a dynamic
  tail keeps one empty workspace at the end. `Ctrl+Alt+←/→` switches,
  `Ctrl+Alt+Shift+←/→` sends the focused window away. Windows on inactive
  workspaces are parked off-screen rather than hidden, with a snapshot taken
  at park time so previews don't go blank when apps stop rendering.
- Live window tracking. `SetWinEventHook` picks up new, closed, and renamed
  windows in the background, and switching to a parked window through
  Alt+Tab or the taskbar's own window list brings its workspace along with
  it. A small identity registry means a recycled window handle never
  inherits a dead window's workspace or preview.
- The Activities overview: fixed-size workspace cards in a draggable
  carousel. The focused card is a little larger than its neighbors, opening
  and closing animate as a zoom, and cards get rounded corners and drop
  shadows. You can drag a window's preview onto another card to move it
  there, or start typing to search open windows and installed apps. All of
  it is our own double-buffered GDI compositing.
- Taskbar replacement. The Windows taskbar is hidden and its reserved strip
  handed back to applications while GroveShell runs, then restored on exit.
  If a run dies without cleaning up, the next launch repairs it.
- A safety net: host and watchdog processes with heartbeats, a job object so
  child processes die together, and `scripts\recover.ps1`, which depends on
  nothing else working.
- A `groveshell-cli` for diagnostics: `ping`, `shutdown`, `list-windows`, and
  `list-monitors`.

## Roadmap

The full plan lives in [`docs/PROJECT_PLAN.md`](docs/PROJECT_PLAN.md) §16.
Condensed, with current status:

### Phase 0: Foundation and safety (done)
- [x] Cargo workspace, shared error and logging conventions
- [x] Single-instance host with named-pipe ping
- [x] Watchdog heartbeat and Explorer recovery
- [x] Structured rotating logs, standalone recovery script

### Phase 1: Window inventory (done)
- [x] Top-level window enumeration with an eligibility policy (visible, uncloaked, unowned, titled)
- [x] `SetWinEventHook` live tracking (create, destroy, show, hide, name change, foreground), debounced into the workspace model
- [x] Generation-counter `WindowId` identity across HWND reuse
- [x] `list-windows` / `list-monitors` CLI commands

### Phase 2: Global move/resize and hot corners
- [ ] Low-level mouse and keyboard hooks
- [ ] Alt+drag move and resize for any window
- [ ] Hot-corner activation for the overview
- [ ] Win key remapped to Activities

### Phase 3: Managed workspaces (partial)
- [x] Workspace domain model (pure, unit-tested) with the dynamic empty-tail policy
- [x] Pinned per-monitor workspaces plus a shared dynamic tail
- [x] Keyboard switching and move-window shortcuts
- [x] Park/unpark instead of hide/show, with crash recovery on the next start
- [ ] Session persistence across restarts
- [ ] Owned dialogs following their owner window

### Phase 4: Top bar and dock (partial)
- [x] Per-monitor top bars with AppBar work-area reservation
- [x] Activities button, workspace dots, clock, calendar, quick settings
- [x] Per-monitor DPI scaling, rounded bottom corners
- [x] Windows taskbar hidden while running, restored on exit
- [ ] Dock (pinned and running apps)
- [ ] Settings for bar height and keybindings

### Phase 5: Activities overview (done)
- [x] Carousel layout engine with focused-card scaling
- [x] Window previews from our own captures, which survive apps that stop rendering off-screen
- [x] Zoom and fade open/close animations, smooth drag with snap
- [x] Click to focus, click empty space to switch or cancel
- [x] Dragging a window between workspaces in the overview
- [x] Application and window search (type to search, Enter activates the top result)

### Phase 6: Hardening and accessibility
- [ ] Mixed-DPI and display-topology testing
- [ ] UI Automation semantics, keyboard-only traversal
- [ ] High-contrast and reduced-motion options
- [ ] Compatibility rules and ignore list

### Phase 7: Optional Explorer replacement
- [ ] Opt-in shell mode for a dedicated test account, with safe mode and full uninstall

## Building (Windows 11 x64, Rust stable)

```powershell
cargo build --workspace
cargo test --workspace
```

## Running

```powershell
.\scripts\dev-start.ps1
```

That builds everything and starts the watchdog, host, and UI as background
processes, with a small dashboard for pinging and shutting them down. Use the
dashboard's graceful shutdown rather than killing the processes: the UI
restores the Windows taskbar and un-parks windows in its close path.

## If something goes wrong

Run `.\scripts\recover.ps1`. It stops every `groveshell-*` process and makes
sure `explorer.exe` is running, without relying on any GroveShell binary
working correctly first. If a hard kill left the taskbar hidden, starting and
gracefully quitting GroveShell once also repairs it.

## Logs

Structured logs land in `%LOCALAPPDATA%\GroveShell\logs\`, one rotating file
per process: `host.log`, `watchdog.log`, `ui.log`, `cli.log`.

## Naming

This project started under a different name that leaned on the GNOME
trademark, and was renamed to GroveShell early on. GroveShell takes workflow
inspiration from GNOME Shell and nothing else: no GNOME code, assets, or
branding. It is independent and not affiliated with, sponsored by, or endorsed
by the GNOME Foundation.

## License

Dual-licensed under Apache-2.0 OR MIT. See `LICENSE-APACHE` and `LICENSE-MIT`.

## Design document

The full technical design, architecture, and phased roadmap lives in
[`docs/PROJECT_PLAN.md`](docs/PROJECT_PLAN.md). Architecture decisions are
recorded under [`docs/adr/`](docs/adr/).
