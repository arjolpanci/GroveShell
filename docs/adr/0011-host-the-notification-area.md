# ADR-011: Host the notification area instead of copying Explorer's

Date: 2026-11-12

Status: Accepted. Supersedes the tray-icon half of ADR-009; its control
center decisions stand.

## Context

ADR-009 showed tray icons by screenshotting Explorer's taskbar before
hiding it, cropping each icon out of the picture, and clicking through UI
Automation elements kept from that moment. It recorded the cost as a
limitation: Windows 11 stops rendering a hidden XAML taskbar, so the
pictures could not update. A new icon or a changed badge needed the
taskbar visible again, or a shell restart.

That approach started from the wrong question, "how do we read Explorer's
tray?". Explorer does not own the icons. Applications do, and they hand
them to whichever window answers to the class `Shell_TrayWnd`:
`Shell_NotifyIcon` finds the first such top-level window and sends it a
`WM_COPYDATA` with the icon handle, the tooltip, and the window and
message the application wants its clicks sent to.

## Decision

GroveShell creates its own `Shell_TrayWnd` window and keeps it above
Explorer's in the z-order, so the lookup finds it first.

- Each icon message is parsed and applied to GroveShell's own icon list,
  then forwarded unchanged to Explorer's real tray. Explorer's record
  stays complete, so nothing has to be handed back when the shell exits.
- At startup the shell broadcasts `TaskbarCreated`, the message Explorer
  sends when it restarts. Applications answer by adding their icons
  again, and those adds arrive at the new host. An application that never
  answers is picked up at its next change, which is treated as an add.
- The icon handle belongs to the application. It is copied and drawn
  into a pixel buffer as soon as it arrives and never kept.
- Clicks are delivered as the application's own callback message, in the
  shape its registered version expects, after `AllowSetForegroundWindow`
  so its menu can take focus and close properly. A second left click
  within the double-click time is sent as a double click.
- Everything else addressed to the taskbar (AppBar messages, private
  taskbar messages) is forwarded to Explorer, with a timeout.
- `Shell_NotifyIconGetRect` is answered with the icon's rectangle in the
  bar, so an application that anchors a flyout to its icon puts it there.
- Code that means "Explorer's taskbar" finds it through
  `tray_icons::explorer_tray`, which skips the shell's own window.

Still nothing is injected into Explorer, reparented, or run in another
security context. The window class name is the only thing borrowed.

## Consequences

Icons are the applications' real ones, at their real size, and change
when the application changes them. Icons added after startup appear.
Context menus are the applications' own.

The capture path, its UI Automation worker, and the label-matching
fallback for opening the overflow (English and German only) are removed.

What remains outside this:

- Network, volume and battery are drawn by Explorer itself on Windows 11
  and are not notify icons. The bar has its own controls for them.
- Which icons go in the bar is the user's existing Windows choice
  (Settings > Personalisation > Taskbar > Other system tray icons), read
  from `HKCU\Control Panel\NotifyIconSettings` and re-read every few
  seconds. The rest are in Explorer's overflow flyout, which the chevron
  still opens. That registry layout is not documented; if the key is
  missing, every icon is shown. At most eight are drawn.
- Balloon notifications are left to Explorer, which receives them through
  the forward.
- An application that looked up `Shell_TrayWnd` once at startup and
  cached the handle keeps talking to Explorer until it is told the
  taskbar was recreated. The broadcast covers the ones that listen.

## Validation

Unit tests cover the payload layout, the add/modify/delete rules, GUID
identity, hidden state, pruning, the taskbar preference match,
known-folder path expansion, callback parameter shapes, the double-click
window, and alpha recovery.

Checked by hand on Windows 11 build 26300:

- With the preference filter off, eight third-party icons appeared at
  startup. With it on, only the one icon switched on in Settings did.
- An icon added by another process after startup appeared within
  seconds, changed when that process changed its picture, and went when
  it was removed.
- A right click opened the application's own menu, which closed on
  Escape and on a click elsewhere. A double click opened the
  application's window.

Not yet checked: Explorer restarting while the shell runs, an elevated
application's icon (its window may refuse messages from a non-elevated
shell), and more than one monitor.

## References

- [Shell_NotifyIcon](https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shell_notifyiconw)
- [The notification area](https://learn.microsoft.com/windows/win32/shell/notification-area)
- [ManagedShell](https://github.com/cairoshell/ManagedShell), which hosts
  the tray the same way for Cairo and RetroBar. Consulted for the
  approach; no code was taken from it.
