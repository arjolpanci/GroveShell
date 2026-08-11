# GroveShell Compatibility & Accessibility Matrix

Phase 6 (see `docs/PROJECT_PLAN.md` §16) requires a published matrix that
"distinguishes supported, partial, and ignored apps", plus a record of the
accessibility and recovery state. This is that document.

Legend:

- **Supported** — GroveShell manages the app normally: it appears in the
  overview, follows workspace switches, and honors move/resize.
- **Partial** — mostly works, with a known caveat noted in the row.
- **Ignored** — deliberately left alone (never managed, hidden, moved, or
  shown in the overview), either by the built-in eligibility policy or via a
  `[compatibility]` ignore rule.

## How windows are classified

A window is **eligible** (Supported/Partial) when it passes the policy in
`crates/window-model/src/lib.rs`: visible, unowned, not a tool window, has a
title, is not DWM-cloaked, and is not one of GroveShell's own windows. On top
of that, the user can force a window to **Ignored** with a rule:

```toml
[[compatibility.ignore]]
exe = "Widgets.exe"      # exact, case-insensitive; file name only

[[compatibility.ignore]]
class = "Shell_TrayWnd"  # exact Win32 class
title = "Overlay"        # case-insensitive substring
```

All present fields in a rule must match (logical AND). An empty rule is
rejected at config-validation time so it can't accidentally hide everything.
`groveshell-cli list-windows` prints the exe/class/title of every eligible
window, which is how you find the values to match on.

## Application matrix

| App | Class(es) | Status | Notes |
|---|---|---|---|
| File Explorer windows | `CabinetWClass` | Supported | Managed like any app window. |
| Windows Terminal | `CASCADIA_HOSTING_WINDOW_CLASS` | Supported | |
| Notepad / Notepad++ | `Notepad`, `Notepad++` | Supported | |
| Chrome / Edge | `Chrome_WidgetWin_1` | Supported | Each top-level window managed independently. |
| Firefox | `MozillaWindowClass` | Supported | |
| VS Code / Electron apps | `Chrome_WidgetWin_1` | Supported | Grouped by exe in the dock via AUMID/path. |
| Office (Word/Excel/…) | `OpusApp`, `XLMAIN`, … | Supported | |
| Owned dialogs / tool palettes | (owned) | Partial | Not top-level; they follow their owner during workspace switches rather than being managed on their own (see `owned_windows_of`). |
| Elevated windows | (any) | Partial | UIPI blocks move/resize and exe inspection from the unelevated shell (PROJECT_PLAN §12); still shown by class/title. |
| Exclusive-fullscreen games | (varies) | Partial → Ignored | Work when windowed; add an ignore rule for overlays or windows that resist repositioning. |
| Real taskbar / Start / Search / Widgets | `Shell_TrayWnd`, `Windows.UI.Core.CoreWindow`, … | Ignored | Excluded by the cloaked/owner/tool-window policy; add an explicit rule if a build surfaces one. |
| On-screen keyboard, launchers, overlays | (varies) | Ignored | Intended targets for `[compatibility]` ignore rules — they should stay on every workspace. |
| GroveShell's own bar/overview/flyouts | `GroveShellBar`, `GroveShellOverview`, … | Ignored | Excluded by the "not our own windows" check. |

This table is a starting set; extend it as apps are exercised on real
hardware. The mechanism (eligibility + ignore rules) is the stable part.

## Accessibility status

| Area | Status | Detail |
|---|---|---|
| Reduced motion | Done | `appearance.reduced_motion` skips overview animations. Settings → Overview. |
| High contrast | Done | `appearance.high_contrast` switches the bar/calendar/overview to a black/white/yellow palette. Settings → Accessibility. Restyles GroveShell's own surfaces only, not foreign apps. |
| Accessible window names | Done | Shell windows carry descriptive titles ("GroveShell Top Bar", "GroveShell Activities", "GroveShell Calendar", "GroveShell Quick Settings"), which become the UIA Name the default provider exposes to Narrator. |
| Keyboard traversal | Partial | The overview supports arrow-key navigation, type-to-search, and Enter-to-activate. The top bar and dock are pointer-first today. |
| UIA custom provider | Not yet | A first-class `IRawElementProviderSimple` provider for the bar/dock (named, role-tagged, invokable elements) is the main remaining accessibility gap. Narrator can reach the windows by name but not yet walk their internal controls. |

## Recovery, soak, and topology (test harnesses + sign-off)

| Check | Harness | Automatable here | Hardware sign-off |
|---|---|---|---|
| Kill each process → Explorer/taskbar restored | `scripts/recovery-matrix.ps1` | Logic verified; script parses and runs the full stack | Pending: run on a real desktop session |
| Long-run stability (idle CPU/RAM, no crashes) | `scripts/soak.ps1` (default 10 min; `-Minutes` for 8–72 h) | Sampling + churn implemented | Pending: 24–72 h run |
| Crash at arbitrary points → recoverable | `scripts/crash-injection.ps1` | Loop + assertion implemented | Pending: extended run |
| Mixed-DPI coordinate conversion | unit tests in `crates/window-model` (`dpi_tests`) | Done — `scale_for_dpi` / `logical_to_physical` / `physical_to_logical` | — |
| Per-monitor DPI / topology inspection | `groveshell-cli list-monitors` (DPI + scale column) | Done | Pending: primary-monitor-unplug live test |
| Diagnostics bundle for bug reports | `groveshell-cli diagnostics` | Done — config + logs + redacted state dump | — |

"Pending hardware" items have working tooling checked in; what remains is
running them in a real multi-monitor / assistive-technology session and
recording the outcomes here.

## Privacy

- `privacy.redact_window_titles` (default **on**) replaces window titles with
  `<redacted>` in the diagnostics bundle and `dump-state` output. Titles can
  carry document names, URLs, and message contents, so they are redacted
  unless you opt in. Toggle in Settings → Accessibility.
- `privacy.telemetry` (default **off**) — there is no telemetry transport in
  GroveShell; the flag records the default now so a future one honors it.
- Rotating log files avoid recording window titles (PROJECT_PLAN §12) and are
  copied into the diagnostics bundle verbatim.
