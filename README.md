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

- A top bar on every monitor: Activities button and workspace dots (each
  reflecting that monitor's own independent workspace set), plus — on the
  primary monitor only, since these reflect machine-wide state rather than
  anything per-monitor — a clock with a calendar flyout and a Wi-Fi/volume/
  battery status pill. Every bar is per-monitor DPI aware and reserves its
  strip through the same AppBar mechanism the real taskbar uses.
- Real icon assets ([Lucide](https://lucide.dev), ISC-licensed) instead of
  hand-drawn shapes, checked into `apps/ui/resources/icons/` and embedded
  into the binary at compile time. States pick the matching variant rather
  than one icon being reused for everything: Wi-Fi on/off, three volume
  levels plus muted, five battery levels plus charging, Bluetooth on/off.
- A GNOME-style Quick Settings panel behind that status pill (hover it for a
  highlight, click anywhere on it to open): a real rounded card with a drop
  shadow, and four toggle chips that all flip actual system state, not just
  their own appearance. Wi-Fi is a plain Win32 call (`wlanapi.dll`); Dark
  Mode writes the same registry values Settings' own toggle does; Bluetooth
  and Airplane Mode go through the WinRT `Windows.Devices.Radios` API, since
  there's no classic Win32 call for either (Airplane Mode is approximated
  the way most third-party toggles do it: on means every known radio is off,
  and it doesn't remember each radio's prior state the way the real OS flag
  does). Below the chips, a real draggable volume slider with the
  percentage shown, and battery status with charging/low-battery states.
  Do Not Disturb, Night Light, and a brightness slider still aren't in:
  the first two need fragile undocumented registry blobs with no public API
  at all, and brightness control is unreliable on laptop panels without WMI.
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
- Live window tracking. `SetWinEventHook` picks up new, closed, and renamed
  windows in the background, and switching to a parked window through
  Alt+Tab or the taskbar's own window list brings its workspace along with
  it. A small identity registry means a recycled window handle never
  inherits a dead window's workspace or preview.
- The Activities overview: one per monitor, each scoped to only that
  monitor's own workspaces — fixed-size workspace cards in a draggable
  carousel. The focused card is a little larger than its neighbors, opening
  and closing animate as a zoom, and cards get rounded corners and drop
  shadows. Dragging a window's preview onto another card pops it out with a
  short animation and moves it there, and hovering any preview (or a dock
  icon) glows it so you can see what you're about to click. A search bar
  sits at the top the whole time the overview is open, ready for you to type
  and search open windows or installed apps. All of it is our own
  double-buffered GDI compositing.
- A dock along the bottom of the focused card, GNOME-dash style: appears
  only inside the overview, not as an always-visible taskbar. It mirrors
  your real Windows taskbar's pinned shortcuts and adds a running-indicator
  dot for anything currently open, pinned or not. A click focuses a running
  app or launches a pinned one.
- The Windows key remapped to Activities, GNOME-style: a plain tap opens or
  closes the overview instead of the Start Menu. Holding it down and
  left-dragging moves whatever window is under the cursor; holding it and
  right-dragging resizes that window from whichever corner you grabbed.
  Works on any window, not just this shell's own, through a pair of
  system-wide low-level hooks. Pushing the cursor into a monitor's
  top-left corner also opens the overview, the same hot-corner trigger
  GNOME uses.
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

### Phase 2: Global move/resize and hot corners (done)
- [x] Low-level mouse and keyboard hooks
- [x] Win+drag move and resize for any window
- [x] Hot-corner activation for the overview
- [x] Win key remapped to Activities

### Phase 3: Managed workspaces (partial)
- [x] Workspace domain model (pure, unit-tested) with the dynamic empty-tail policy
- [x] Independent per-monitor workspace sets: one pinned workspace and one
  dynamic tail per monitor, with live hotplug (new monitors get their own
  set; disconnected ones hand their windows to the primary monitor)
- [x] Keyboard switching and move-window shortcuts, resolved to whichever
  monitor currently has keyboard focus
- [x] Park/unpark instead of hide/show, with crash recovery on the next start
- [ ] Session persistence across restarts (abandoned for now — still
  deciding the best approach to matching reopened windows back to their
  saved workspace, given Win32 gives no stable window identity across a
  restart)
- [x] Owned dialogs following their owner window: carried along by
  `park_window`/`unpark_window` during a workspace switch, and
  repositioned to preserve their relative offset when their owner is
  dragged to a different monitor (manually verified on real hardware)

### Phase 4: Top bar and dock (partial)
- [x] Per-monitor top bars with AppBar work-area reservation
- [x] Activities button, workspace dots, clock, calendar
- [x] Per-monitor DPI scaling, rounded bottom corners
- [x] Windows taskbar hidden while running, restored on exit
- [x] Dock (pinned and running apps), overview-only, mirrored from the real taskbar's pins
- [x] GNOME-style Quick Settings: rounded card with a drop shadow, status pill with Wi-Fi/volume/battery glyphs, working Wi-Fi/Dark Mode/Bluetooth/Airplane Mode toggles, draggable volume slider
- [x] Unified design language: a `design::` token module (color/metrics/motion) every surface draws from, with the shell's active/focus accents following the **live Windows accent color** (updates when you change it) and high-contrast support
- [x] System tray: a bar chevron that **hosts the real Windows hidden-icons overflow window** (best-effort across Win11 builds; hidden when no overflow window is found) alongside the curated Wi-Fi/volume/battery indicators
- [x] Session/power menu on the bar (lock, sleep, sign out, restart, shut down) with confirms on the destructive actions
- [x] Central settings UI — the `groveshell-settings` tray app (bar height, dock, overview, input, accessibility pages)
- [ ] Do Not Disturb, Night Light toggles, and a brightness slider (no public API for the first two; brightness needs WMI and is unreliable on laptop panels; deferred)
- [~] Motion polish: shared easing/duration system and unit-tested flyout + desktop-dock-reveal state machines are in (`design::motion`, `flyout`, `desktop_dock`); wiring the grow-from-anchor animation into the existing flyout windows and building the opt-in floating desktop-dock window (`dock_mode = always`/`autohide`) are the remaining pieces, deferred for live visual verification

### Phase 5: Activities overview (done)
- [x] One Activities overview per monitor, each with its own carousel
  layout engine and focused-card scaling, scoped to only that monitor's
  own workspaces
- [x] Window previews from our own captures, which survive apps that stop rendering off-screen
- [x] Zoom and fade open/close animations, smooth drag with snap
- [x] Click to focus, click empty space to switch or cancel
- [x] Dragging a window between workspaces in the overview, with pop in/out animations and a hover glow on previews and dock icons
- [x] Application and window search, always visible, not just while typing (type to search, Enter activates the top result)

### Phase 6: Hardening and accessibility
- [x] Mixed-DPI handling with tested logical/physical conversion helpers; `groveshell-cli list-monitors` shows per-monitor DPI and scale
- [x] Accessible window names; reduced-motion and high-contrast options (Settings → Accessibility)
- [x] Compatibility rules and ignore list (`[compatibility]` in config), with a [published compatibility matrix](docs/compatibility.md)
- [x] Diagnostics bundle (`groveshell-cli diagnostics`) and privacy controls (window-title redaction, telemetry off by default)
- [x] Recovery-matrix, soak, and crash-injection scripts under `scripts/`
- [ ] Full multi-monitor / Narrator / long-soak sign-off on real hardware (see [docs/compatibility.md](docs/compatibility.md), "pending hardware")

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
