# ADR-009: Control center and Explorer notification integration

Date: 2026-09-08

Status: Accepted for the experimental Phase 4 polish implementation.

## Context

The control center had working radio switches but no options, synchronous
WinRT queries during painting, and an overflow-only notification area.
Windows 11's XAML notification buttons expose UI Automation invocation, but
Explorer does not provide a supported public notification-icon enumeration API.

Desktop diagnostics on Windows 11 found `NotifyItemIcon` controls under
`Shell_TrayWnd`. `PrintWindow` with flag 2 rendered their images while the
taskbar was visible. Hiding it removed its discoverable accessibility tree and
produced black captures. Previously acquired automation elements remained
queryable. Cross-process DWM cloaking returned access denied; it is not used.

## Decision

- Run radio operations, network enumeration, and notification-provider work
  on workers. Paint from copied snapshots and retain the existing recovery path.
- Use WLAN APIs to list networks and connect/disconnect saved profiles. Treat
  connection acceptance as pending until a later query reports the connection,
  with a 20-second timeout. Show an explicit error when enumeration is denied.
- Use Windows' network UI for new-network credentials and enterprise sign-in.
  Bluetooth pairing, device routing, and advanced appearance settings likewise
  open Windows controls through settings URIs. No credential storage is added.
- Capture promoted tray images and retain their native automation elements
  before hiding Explorer, with a one-second startup timeout. Do not reparent,
  inject into, or change the security context of Explorer.
- Retain captured icons when the hidden XAML host cannot render. This is an
  explicit limitation: snapshots do not deliver live badge updates or discover
  newly added icons while the taskbar stays hidden.
- Keep native overflow hosting. When the overflow has not yet been created,
  best-effort invocation recognizes the English and German overflow labels.
  Do not guess by invoking the first arbitrary normal button: it may be the
  input-language selector. Unknown languages need an existing overflow window.
- Remove the generic `XamlExplorerHostIslandWindow` candidate: it can identify
  unrelated Explorer UI and is too broad to move or show safely.

## Validation and remaining work

The layout tests cover split targets, non-overlapping controls, network-list
shrinkage, and 100–250% DPI. An opt-in render test exports real GDI images of
the home and options pages at 100%, 150%, and 200% DPI. The images are diagnostic
fixtures under `target/`, not screenshots of a connected user's network list.

The renderer exposed an empty-string `DrawTextW` crash; the shared helper now
returns early for empty text. Icon streams are retained for GDI+'s lazy decoding.
The opt-in `capture_promoted_icons_on_desktop` test also passed against the
real visible taskbar, validating native automation lookup and icon pixel capture.

Run `scripts/inspect-tray.ps1` for a read-only Explorer/UIA inventory. `-Capture`
also writes a local taskbar PNG. `-TestHiddenCapture` temporarily hides the
taskbar and restores its prior visibility in `finally` to reproduce the XAML
limitation. Do not use this diagnostic during an important desktop operation.

Full native tray invocation/context-menu, Explorer restart, mixed-monitor,
Narrator, and password/pairing flows require interactive hardware validation.
This change does not claim a complete tray replacement or a complete UI
Automation tree for the painted control-center controls.

## References

- [Launch Windows Settings](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-settings)
- [WlanGetAvailableNetworkList](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlangetavailablenetworklist)
- [PrintWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-printwindow)
