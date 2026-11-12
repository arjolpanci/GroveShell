<p align="center">
  <img src="media/logo.png" alt="GroveShell logo" width="160">
</p>

# GroveShell

GroveShell is an experimental desktop shell for Windows 11. It hides the
taskbar and puts a top bar, per-monitor workspaces, and a GNOME-style
Activities overview in its place.

It is written in Rust against Win32 and runs alongside Explorer. Quitting
it restores the Windows taskbar and your windows. Because it is
experimental, keep the [recovery instructions](#recovery-and-diagnostics)
handy when you try a new build.

![The Activities overview](media/activities-overview.png)

## Activities and workspaces

Tap the Windows key to open Activities. The overview shows the current
workspace with a live preview of each window, a strip of workspace
thumbnails above it, a search field, and a dock with your pinned and
running apps. Click a preview to focus that window, type to search, or drag
a preview onto another workspace.

![Dragging a window to another workspace](media/workspace-drag.png)

Each monitor has its own set of workspaces, so switching on one display
leaves the others alone. The last workspace in the set is always empty.
When you connect or disconnect a monitor, GroveShell moves the affected
windows into a workspace you can reach.

## Motion

Opening and closing Activities is one continuous motion, not a fade on a
fixed timer. Each window travels from where it sits on screen into its slot
in the grid, the dock slides up from below, and the thumbnail strip drops in
from above. You can stop the motion halfway and reverse it.

A three- or four-finger swipe on a precision touchpad drives it directly.
Swipe up and the overview opens as far as your fingers have travelled; hold
still and it waits; swipe back down and it closes again. It settles only
when you lift, and a flick carries on under its own momentum. Swiping
sideways slides between workspaces the same way while the overview is open,
and switches workspace when it is closed.

A two-finger scroll or the mouse wheel over the desktop does the same with
less precision. Every animated surface is computed from one progress value
per monitor, which is why the pieces stay in step. The
[motion design notes](docs/design/2026-10-13-gnome-motion-port-design.md)
explain the approach.

Windows has its own three- and four-finger swipes (Task View and virtual
desktops) and would run them at the same time, so GroveShell sets them to
"Nothing" while it runs and puts your setting back when it quits. You can
turn the swipes off under Settings > Input, which leaves Windows' own alone.

## Top bar

Every monitor gets an Activities button and workspace indicators. The
primary display also has a clock and calendar, system status, settings, a
power menu, and access to the Windows notification area.

![The calendar flyout](media/calendar.png)

### Control center

The control center has tiles for Wi-Fi, Bluetooth, appearance, and radios,
plus mute and a draggable volume slider. The arrow on each split button
opens more options: a Wi-Fi network list with saved connections, Bluetooth
pairing, themes, Windows airplane mode, audio devices, and the volume mixer.

![The control center](media/control-center.png)

You pick a saved Wi-Fi network inside GroveShell. The list shows signal
strength and supports disconnect, refresh, and scrolling. Setting up a new
network and entering its password go through Windows' own connection UI.
GroveShell does not collect or store Wi-Fi passwords.

The panel uses Segoe UI, rounded surfaces, and the Windows accent color. It
shows hover and keyboard focus, and its fade transitions respect reduced
motion. Radio operations run in the background so they do not block
painting.

Keyboard control:

- Tab moves between controls.
- Enter or Space activates the focused control.
- The arrow keys adjust the volume slider when it has focus.
- Escape goes back from an options page, or closes the panel.

### Notification area

GroveShell hosts the notification area itself while it runs. Applications
hand their tray icons to it directly, so the primary bar shows each
application's real icon and updates when the application changes it. A click
goes to the application, a double click opens it where it expects one, and a
right click opens its own menu.

The bar shows the icons you have switched on under Windows Settings >
Personalisation > Taskbar > Other system tray icons. The chevron opens
Windows' hidden-icons flyout for the rest. Every icon message is also passed
on to Explorer, so its tray is intact when GroveShell quits.
[ADR-011](docs/adr/0011-host-the-notification-area.md) has the details.

## Settings

The gear in the top bar opens settings for bar height, dock placement and
behavior, overview appearance, input, and accessibility. The dock can appear
only in the overview, stay visible on the desktop, or auto-hide on the
desktop. Appearance changes apply while GroveShell is running.

The shell follows the Windows accent color and supports high contrast,
reduced motion, and per-monitor DPI scaling. Compatibility rules can exclude
apps from workspace management. The
[compatibility notes](docs/compatibility.md) list the tested configurations
and the hardware checks still to do.

## Keyboard and mouse

| Action | Shortcut |
| --- | --- |
| Open or close Activities | Tap the Windows key |
| Switch workspace on the focused monitor | Ctrl + Alt + Left / Right |
| Move the focused window to another workspace | Ctrl + Alt + Shift + Left / Right |
| Move a window | Windows key + left-drag |
| Resize a window | Windows key + right-drag |
| Search apps and windows | Type in Activities; Enter opens the top result |
| Open Activities with the mouse | Click Activities or use the top-left hot corner |
| Open or close Activities on a touchpad | Three- or four-finger swipe up / down |
| Switch workspace on a touchpad | Three- or four-finger swipe left / right |

## Build and run

You need Windows 11 x64 and Rust stable with the MSVC toolchain, plus the
Windows SDK and the Visual Studio C++ build tools.

```powershell
cargo build --workspace
cargo test --workspace
.\scripts\dev-start.ps1
```

`dev-start.ps1` builds the workspace and starts the watchdog, host, and UI
in the background. It then shows a dashboard with process status and a
graceful shutdown command. Use graceful shutdown to restore the taskbar,
work areas, and parked windows.

## Recovery and diagnostics

If the shell becomes unresponsive, run:

```powershell
.\scripts\recover.ps1
```

The script stops GroveShell processes and makes sure Explorer is running. It
does not depend on a working GroveShell binary. Starting GroveShell, or
quitting it gracefully, also repairs a taskbar left hidden by a hard kill.

The host and watchdog monitor process health. Structured, rotating logs are
written to `%LOCALAPPDATA%\GroveShell\logs\`. The CLI has `ping`,
`shutdown`, `list-windows`, `list-monitors`, and `diagnostics` commands, and
`diagnostics` can redact window titles. Telemetry is off by default.

## Current limitations

- Workspace assignments do not persist across restarts.
- The radio tile switches the available radios together. It is not Windows'
  airplane-mode flag; its options page opens the actual Windows setting.
- Brightness, Night Light, and Do Not Disturb are not integrated controls.
- Network, volume and battery are GroveShell's own indicators. Windows 11
  draws its versions inside Explorer, where they cannot be hosted.
- Touchpad swipes need a precision touchpad. Whether Windows stops running
  its own swipes straight away or only after the next sign-in varies by
  build, and has not been confirmed on hardware.
- If GroveShell is killed instead of quit, Windows' swipes stay off until
  it next starts and quits, or until you run `scripts\recover.ps1`.
- Wi-Fi enumeration can require Windows location permission. If it is
  denied, the panel still gives access to Windows' network UI.
- Testing with Narrator, on mixed-monitor hardware, and over long sessions
  is not finished. Painted keyboard controls do not yet expose a complete UI
  Automation accessibility tree.

## Development

[CONTRIBUTING.md](CONTRIBUTING.md) covers how to contribute.
[The design document](docs/PROJECT_PLAN.md) describes the architecture and
future work, and the [architecture decisions](docs/adr/) record the
reasoning behind the design.

GroveShell takes workflow inspiration from GNOME Shell, using no GNOME code,
assets, or branding. It is not affiliated with or endorsed by the GNOME
Foundation. Interface icons are from [Lucide](https://lucide.dev), with their
license included in `crates/ui-kit/resources/icons/NOTICE.md`.

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
