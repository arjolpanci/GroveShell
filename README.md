<p align="center">
  <img src="media/logo.png" alt="GroveShell logo" width="160">
</p>

# GroveShell

An experimental desktop shell for Windows 11, built around workspaces and an
Activities-style overview instead of the taskbar-and-Start-menu flow. Written
in Rust against the raw Win32 API.

While GroveShell runs, it takes over the top of the screen with its own bar,
hides the Windows taskbar (and gives its screen space back to your apps), and
puts workspaces one keystroke away. Quit it and everything is restored —
taskbar, work areas, any windows it parked. Explorer keeps running underneath
the whole time; nothing here replaces system components, and a watchdog plus a
standalone recovery script exist precisely so a bad build can't leave you
stranded.

![The Activities overview](media/activities-overview.png)

The overview: workspace cards with the real wallpaper, window previews with
their app icons, a carousel you can drag between workspaces. Opening zooms out
of the current workspace; closing zooms back into the one you picked.

![Dragging between workspaces](media/workspace-drag.png)

## What works today

- **Top bar** on every monitor: Activities button, workspace indicator dots, a
  clock with a calendar flyout, battery and quick settings. Per-monitor DPI
  aware, reserves its strip via the same AppBar mechanism the real taskbar
  uses.
- **Workspaces**, GNOME-style: each monitor is a pinned workspace, plus a
  dynamic tail that always keeps one empty workspace at the end. `Ctrl+Alt+←/→`
  switches, `Ctrl+Alt+Shift+←/→` sends the focused window away. Windows on
  inactive workspaces are parked off-screen (not hidden), with a snapshot
  taken at park time so previews never go blank.
- **Activities overview**: fixed-size workspace cards in a draggable carousel,
  the focused card slightly larger than its neighbors, zoom-out/zoom-in
  open/close animations, rounded corners and drop shadows throughout. All
  rendering is our own double-buffered GDI compositing.
- **Taskbar replacement**: the Windows taskbar is hidden and its reserved
  strip handed back to applications while GroveShell runs, then restored on
  exit. If a run dies without cleaning up, the next launch self-heals.
- **Safety net**: host/watchdog processes with heartbeats, a job object so
  child processes die together, and `scripts\recover.ps1`, which depends on
  nothing else working.

## Roadmap

The full plan lives in [`docs/PROJECT_PLAN.md`](docs/PROJECT_PLAN.md) §16.
Condensed, with current status:

### Phase 0 — Foundation and safety ✅
- [x] Cargo workspace, shared error/logging conventions
- [x] Single-instance host with named-pipe ping
- [x] Watchdog heartbeat and Explorer recovery
- [x] Structured rotating logs, standalone recovery script

### Phase 1 — Window inventory 🟡
- [x] Top-level window enumeration with eligibility policy (visible, uncloaked, unowned, titled)
- [ ] `SetWinEventHook` live tracking (create/destroy/focus events) — currently snapshot + re-sync on demand
- [ ] Generation-counter `WindowId` identity across HWND reuse
- [ ] `list-windows` / `list-monitors` CLI commands

### Phase 2 — Global move/resize and hot corners
- [ ] Low-level mouse/keyboard hooks
- [ ] Alt+drag move and resize for any window
- [ ] Hot-corner activation for the overview
- [ ] Win key remapped to Activities

### Phase 3 — Managed workspaces 🟡
- [x] Workspace domain model (pure, unit-tested) with dynamic empty-tail policy
- [x] Pinned per-monitor workspaces plus shared dynamic tail
- [x] Keyboard switching and move-window shortcuts
- [x] Park/unpark instead of hide/show, with crash recovery on next start
- [ ] Session persistence across restarts
- [ ] Owned dialogs following their owner window

### Phase 4 — Top bar and dock 🟡
- [x] Per-monitor top bars with AppBar work-area reservation
- [x] Activities button, workspace dots, clock, calendar, quick settings
- [x] Per-monitor DPI scaling, rounded bottom corners
- [x] Windows taskbar hidden while running, restored on exit
- [ ] Dock (pinned/running apps)
- [ ] Settings for bar height, keybindings

### Phase 5 — Activities overview 🟡
- [x] Carousel layout engine with focused-card scaling
- [x] Window previews from our own captures (survives apps that stop rendering off-screen)
- [x] Zoom + fade open/close animations, smooth drag with snap
- [x] Click to focus, click empty space to switch or cancel
- [ ] Dragging a window between workspaces in the overview
- [ ] Application/window search

### Phase 6 — Hardening and accessibility
- [ ] Mixed-DPI and display-topology testing
- [ ] UI Automation semantics, keyboard-only traversal
- [ ] High-contrast and reduced-motion options
- [ ] Compatibility rules and ignore list

### Phase 7 — Optional Explorer replacement
- [ ] Opt-in shell mode for a dedicated test account, with safe-mode and full uninstall

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
processes, with a small dashboard for pinging and shutting them down. Prefer
the dashboard's graceful shutdown over killing the processes: the UI restores
the Windows taskbar and un-parks windows in its close path.

## If something goes wrong

Run `.\scripts\recover.ps1`. It stops every `groveshell-*` process and makes
sure `explorer.exe` is running, without relying on any GroveShell binary
working correctly first. If the taskbar was left hidden by a hard kill,
starting and gracefully quitting GroveShell once also repairs it.

## Logs

Structured logs land in `%LOCALAPPDATA%\GroveShell\logs\`, one rotating file
per process: `host.log`, `watchdog.log`, `ui.log`, `cli.log`.

## Naming

This project started under a different name that leaned on the GNOME
trademark; it was renamed to GroveShell early on. GroveShell takes workflow
inspiration from GNOME Shell and nothing else — no GNOME code, assets, or
branding. It is independent and not affiliated with, sponsored by, or endorsed
by the GNOME Foundation.

## License

Dual-licensed under Apache-2.0 OR MIT. See `LICENSE-APACHE` and `LICENSE-MIT`.

## Design document

The full technical design, architecture, and phased roadmap lives in
[`docs/PROJECT_PLAN.md`](docs/PROJECT_PLAN.md). Architecture decisions are
recorded under [`docs/adr/`](docs/adr/).
