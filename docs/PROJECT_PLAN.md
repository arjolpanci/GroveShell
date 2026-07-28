# GroveShell
## Technical Design, Architecture, and Phased Implementation Plan

*A GNOME-inspired, workspace-first desktop shell for Windows 11*

**Purpose:** provide a durable specification that can guide iterative implementation with Codex or another coding agent.
**Status:** Working design - not an implementation guarantee
**Target platform:** Windows 11 x64
**Primary implementation language:** Rust
**License recommendation:** Apache-2.0 OR MIT dual license

### Naming note

The project originally carried a name that compounded the GNOME word mark, which the GNOME Foundation's trademark guidance doesn't allow. It was renamed to GroveShell — an independent name — early in development, which resolved that concern (see ADR-008). The standing rules are simple: no GNOME code, assets, foot logo, or branding imitation, and the README keeps a disclaimer that the project is independent and not affiliated with, sponsored by, or endorsed by the GNOME Foundation. GNOME is a registered trademark of the GNOME Foundation.

## 1. Executive summary

GroveShell is an experimental desktop shell and window-management layer for Windows 11. It will preserve the Windows kernel, Win32 application model, graphics drivers, Desktop Window Manager (DWM), security model, and ordinary .exe compatibility while replacing or visually superseding the normal Explorer-centric desktop experience.

The intended experience is overview-first and workspace-centric: a single top bar, configurable hot corners, an Activities-style overview, a bottom dock, dynamic workspaces, global keyboard navigation, and modifier-plus-pointer window movement or resizing from anywhere inside a window. It is not a port of GNOME Shell and it will not replace DWM. It is a native Windows shell inspired by selected interaction ideas.

- Phase 0-2 produce a safe companion utility that coexists with Explorer.
- Phase 3-5 add the top bar, dock, workspace model, and overview.
- Phase 6 hardens multi-monitor, DPI, accessibility, recovery, and packaging.
- Phase 7 optionally replaces Explorer for a dedicated test account.
- Every phase must remain independently runnable, testable, and reversible.

## 2. Product vision

### 2.1 Product statement

Create a fast, keyboard-friendly Windows desktop environment that makes windows and workspaces easier to understand and navigate, while retaining native Windows application compatibility and avoiding unsupported replacement of the Windows compositor.

### 2.2 Design principles

| Principle | Meaning |
|---|---|
| Overview first | The primary navigation surface shows workspaces, windows, search, and the dock together. |
| Native underneath | Use documented Win32, COM, Windows App SDK, DWM, capture, and accessibility APIs whenever possible. |
| Failure must be recoverable | A shell crash must not strand the user without a desktop, launcher, or way to restart Explorer. |
| Progressive replacement | Begin as an ordinary app. Hide or replace Explorer only after the core is reliable. |
| Policy over hacks | Represent app-specific behavior through explicit rules rather than fragile global modifications. |
| Keyboard and pointer parity | Every important action should be possible from both keyboard and pointer. |
| Performance is a feature | Idle CPU usage, memory use, input latency, and animation smoothness are first-class acceptance criteria. |
| Independent identity | Take workflow inspiration without copying GNOME branding, assets, source code, or exact visual identity. |

### 2.3 Target users

- Windows users who prefer GNOME-like workspaces and overview navigation.
- Developers and power users who want global move/resize gestures and deterministic window rules.
- Open-source contributors interested in Windows shell, graphics, accessibility, and systems programming.
- Users who want a radically different shell without abandoning native Windows applications.

### 2.4 Non-goals

- Replacing dwm.exe or the Windows graphics kernel.
- Reimplementing the Windows login screen, UAC secure desktop, or lock screen.
- Guaranteeing control over every elevated, protected, exclusive-fullscreen, or anti-cheat application.
- Pixel-perfect replication of GNOME Shell.
- Injecting code into arbitrary applications merely to restyle them.
- Globally replacing every third-party application title bar.
- Supporting Windows 10 in the first stable release.

## 3. Functional requirements

| ID | Capability | Requirement |
|---|---|---|
| FR-001 | Top bar | Display workspace state, active app, clock, and system indicators in a reserved strip at the top of each monitor. |
| FR-002 | Activities overview | Display scaled previews of manageable top-level windows grouped by workspace and monitor. |
| FR-003 | Hot corners | Allow each screen corner to trigger a configurable action with delay and fullscreen suppression rules. |
| FR-004 | Dock | Show pinned and running applications, launch apps, focus windows, and expose multiple-window state. |
| FR-005 | Workspaces | Provide workspace creation, switching, window assignment, reordering, and optional dynamic-workspace behavior. |
| FR-006 | Global input | Support configurable global hotkeys plus modifier + left drag to move and modifier + right drag to resize. |
| FR-007 | Search | Search installed apps, running windows, settings commands, and optional user-configured providers. |
| FR-008 | Window rules | Match windows by executable, AUMID, class, title, owner, style, or process properties and apply policies. |
| FR-009 | Multi-monitor | Track monitor topology, per-monitor scale, work areas, and workspace policy. |
| FR-010 | Recovery | Provide a panic shortcut and watchdog that restores Explorer, unhides windows, and exits safely. |
| FR-011 | Configuration | Persist settings in a human-readable file with schema validation and safe reload. |
| FR-012 | Diagnostics | Expose structured logs, a debug overlay, state dumps, and optional trace recording. |
| FR-013 | Autostart | Offer user-controlled startup with Windows; never enable silently. |
| FR-014 | Shell mode | Optionally run as the user shell only after explicit opt-in and compatibility checks. |

## 4. Quality attributes and acceptance targets

| Area | Initial target | Stable target |
|---|---|---|
| Cold startup | < 2.0 seconds | < 1.0 second on a modern SSD |
| Idle CPU | < 1% average | < 0.2% average |
| Idle working set | < 250 MB | < 150 MB excluding preview surfaces |
| Overview animation | No obvious stalls | 60 FPS on integrated graphics at 1440p |
| Window event reaction | < 150 ms | < 50 ms typical |
| Workspace switch | < 250 ms | < 120 ms perceived latency |
| Crash recovery | Manual panic shortcut | Automatic watchdog + manual fallback |
| Data loss | No configuration corruption | Atomic writes and backups |
| Accessibility | Keyboard operable | UI Automation names, focus order, high contrast, screen-reader basics |
| Compatibility | Common Win32 apps | Win32, packaged apps, Electron, multi-DPI, common games in bypass mode |

## 5. System architecture

### 5.1 Architectural stance

The shell is a collection of user-mode processes above DWM. Windows remains responsible for compositing real application windows. GroveShell observes and controls top-level HWNDs, creates its own shell windows, and renders overview representations using DWM thumbnails or Windows Graphics Capture. No kernel driver is required.

```
Normal applications (Win32 / packaged / Electron)
                  |
         HWND + Windows messages
                  |
+--------------------------------------------------+
| GroveShell user-mode processes                     |
|  shell-host  | wm-core | overview | bar | broker |
+--------------------------------------------------+
                  |
 Win32 / COM / DWM / DirectComposition / WGC / UIA
                  |
       Desktop Window Manager (dwm.exe)
                  |
      Windows graphics stack and display drivers
```

### 5.2 Process decomposition

| Process / crate | Responsibilities | Failure behavior |
|---|---|---|
| groveshell-host | Lifecycle, single-instance enforcement, configuration, IPC routing, session state, startup/shutdown. | Restarts child components or exits to Explorer. |
| groveshell-wm | Window inventory, focus history, move/resize, rules, workspaces, monitor topology. | Watchdog restarts it; persisted assignments are reconstructed. |
| groveshell-ui | Top bar, dock, overview, search, settings surface, animations. | UI restarts without terminating the WM core. |
| groveshell-input | Hotkeys, low-level keyboard/mouse hooks, hot corners, gesture state machine. | Hooks are automatically removed when process exits. |
| groveshell-broker | Optional elevated helper for narrowly scoped operations. | Disabled by default; never runs full shell elevated. |
| groveshell-watchdog | Health checks, panic recovery, Explorer restore, crash-loop prevention. | Minimal dependencies; designed to remain alive. |
| groveshell-cli | Diagnostics and automation: list windows, switch workspace, reload config, dump state. | Noncritical. |
| groveshell-common | Shared models, IDs, configuration schema, IPC messages, error types. | Library crate. |

### 5.3 Why Rust

- Memory safety is valuable for a long-running shell that processes untrusted window metadata and global input.
- The windows crate exposes Win32 and WinRT APIs while preserving access to low-level HWND-oriented operations.
- Cargo workspaces make process and library boundaries explicit.
- Serde supports versioned configuration and IPC models.
- Tokio can coordinate IPC and background work, while UI and hook threads retain ordinary Windows message loops.
- A C++ graphics helper remains acceptable if a required DirectComposition or capture API is substantially easier to implement there.

### 5.4 UI technology decision

The recommended first implementation uses a custom native Win32 window host plus Direct3D 11 and DirectComposition for the overview, dock, and top bar. A settings application may use WinUI 3 later. This avoids making the critical shell UI dependent on a web runtime and gives direct control over transparent surfaces, animation timing, DPI, input regions, and live previews.

| Option | Use | Decision |
|---|---|---|
| Direct3D 11 + DirectComposition | Overview, preview transforms, blur/dim layers, animations | Primary choice |
| DWM thumbnails | Fast first preview implementation | Use in early phases |
| Windows Graphics Capture | Higher-control live previews | Introduce after overview MVP |
| WinUI 3 | Settings, onboarding, optional bar controls | Secondary choice |
| Tauri/WebView | Settings or documentation only | Do not use for latency-critical shell surfaces |
| WPF | Possible prototype | Not preferred for the long-term shell core |

### 5.5 Threading model

- Each process has one primary Windows message-loop thread.
- WinEvent callbacks perform minimal work and enqueue normalized events.
- Window reconciliation runs on a dedicated state thread to serialize mutations.
- Rendering has a dedicated render thread or composition dispatcher.
- IPC and file operations use asynchronous workers.
- Never block a global hook callback on logging, rendering, IPC, or application inspection.

## 6. Core domain model

```
WindowId      = stable session-local identifier wrapping HWND + generation
AppId         = normalized identity (AUMID or executable identity)
WorkspaceId   = UUID persisted in session state
MonitorId     = stable device path / display identifier
WindowRecord  = metadata + lifecycle + placement + policy
Workspace     = ordered window set + monitor association
ShellState    = monitors + workspaces + focused window + mode
Rule          = match expression + ordered actions
```

### 6.1 Window lifecycle states

```
Discovered -> Eligible -> Managed -> Visible
                    |          |       |
                    |          |       +-> HiddenByWorkspace
                    |          +----------> Minimized
                    +---------------------> Ignored
Any state --------------------------------> Destroyed
```

HWND values can be reused after destruction. WindowId must therefore include a generation counter or creation timestamp and must never treat a raw HWND as a permanently stable identifier.

### 6.2 Eligible top-level window policy

- Start from EnumWindows during reconciliation.
- Require a valid, visible or potentially visible top-level HWND.
- Exclude the shell's own windows and known infrastructure windows.
- Inspect owner relationships, cloaking state, extended styles, class name, process, and AppUserModelID.
- Treat tool windows, popups, dialogs, and transients separately from primary app windows.
- Record why a window is ignored so diagnostics can explain behavior.

## 7. Windows integration APIs

| Concern | Primary API | Notes |
|---|---|---|
| Initial window inventory | EnumWindows, GetWindowThreadProcessId | Reconcile at startup and periodically. |
| Window events | SetWinEventHook | Listen out-of-context; callback queues normalized events. |
| Shell events | RegisterShellHookWindow | Optional supplemental signal source. |
| Move and resize | SetWindowPos, DeferWindowPos | Batch operations during layout changes. |
| Placement/state | GetWindowPlacement, ShowWindow | Preserve restore rectangles and minimization. |
| Styles | GetWindowLongPtr, SetWindowLongPtr | Use sparingly and only under explicit policies. |
| DWM metadata | DwmGetWindowAttribute, DwmSetWindowAttribute | Cloaking, frame bounds, corners, supported appearance attributes. |
| Global hotkeys | RegisterHotKey | Preferred for simple non-pointer shortcuts. |
| Complex input | SetWindowsHookEx WH_KEYBOARD_LL / WH_MOUSE_LL | Required for modifier-drag; callbacks must be tiny. |
| Monitor geometry | EnumDisplayMonitors, GetMonitorInfo, DisplayConfig APIs | Use per-monitor DPI awareness v2. |
| DPI | GetDpiForWindow, GetDpiForMonitor where appropriate | Convert logical and physical coordinates deliberately. |
| Preview MVP | DwmRegisterThumbnail | Simple live thumbnails for overview. |
| Advanced capture | Windows.Graphics.Capture | Create frame pools per selected window. |
| Rendering | Direct3D 11, DirectComposition | Native composition and animation. |
| App identity | AUMID/property store + process executable path | Layered fallback strategy. |
| App launch | ShellExecuteEx / activation manager | Handle desktop and packaged apps. |
| Accessibility | UI Automation provider APIs | Expose names, roles, focus, actions. |
| Tray integration | Shell_NotifyIcon ecosystem / notification area behavior | Full replacement is complex; defer. |
| Notifications | Windows App SDK app notifications | Shell-level notification center is later scope. |
| Virtual desktops | Public IVirtualDesktopManager plus optional adapter | Public API is limited; keep backend swappable. |
| Shell replacement | Shell Launcher where available; controlled fallback otherwise | Only after hardening and explicit user opt-in. |

## 8. Workspace subsystem

### 8.1 Backend abstraction

```rust
trait WorkspaceBackend {
    fn enumerate(&self) -> Result<Vec<Workspace>>;
    fn current(&self) -> Result<WorkspaceId>;
    fn switch_to(&self, id: WorkspaceId) -> Result<()>;
    fn move_window(&self, window: WindowId, target: WorkspaceId) -> Result<()>;
    fn reconcile(&self, windows: &[WindowRecord]) -> Result<WorkspaceSnapshot>;
}
```

Implement two backends behind this interface:

- **ManagedWorkspaceBackend:** GroveShell owns workspace membership and hides/shows windows itself. This is fully controllable but needs extensive compatibility handling.
- **NativeDesktopBackend:** delegates to Windows virtual desktops where possible. This integrates better with the OS but richer control may require unstable undocumented interfaces. Undocumented support must be an optional, version-gated adapter rather than a core dependency.

### 8.2 Recommended MVP policy

Begin with ManagedWorkspaceBackend because it can be implemented using stable window APIs and tested deterministically. Preserve each window's pre-hide state. Never convert a user-minimized window into a merely workspace-hidden window or vice versa. Maintain separate flags: `minimized_by_user`, `hidden_by_workspace`, `cloaked_by_system`, and `visible_effective`.

### 8.3 Dynamic workspace rules

- Always retain one empty workspace after the last occupied workspace.
- Remove redundant empty workspaces after a debounce period.
- Do not destroy a workspace while a drag operation targets it.
- Define whether workspaces are global or per-monitor; start global for simplicity.
- Persist only meaningful named/pinned workspaces. Reconstruct transient empty workspaces at startup.

## 9. Window-management subsystem

### 9.1 Global move and resize

```
Idle
  -> ModifierArmed
  -> PointerDown(target HWND, origin cursor, origin rect)
  -> Moving | Resizing(edge policy)
  -> Commit
  -> Idle

Cancel paths: Escape, modifier release policy, target destroyed,
secure desktop transition, display topology change, shell shutdown.
```

- Default mapping: Alt + left drag moves; Alt + right drag resizes. Super/Windows can be optional because the OS reserves many Windows-key combinations.
- Resolve the root top-level window beneath the pointer using WindowFromPoint and GetAncestor.
- Use physical screen coordinates in a per-monitor-DPI-aware process.
- Throttle SetWindowPos to display cadence or coalesce pointer events.
- Apply minimum/maximum track sizes and optional magnetic snapping.
- Never manipulate secure-desktop, protected, shell-owned, or explicitly ignored windows.

### 9.2 Title bars and decorations

GroveShell can make its own title bars compact. It cannot reliably shrink every third-party application title bar. Windows App SDK title-bar customization applies to the application that owns the window. For foreign windows, DWM attributes and style changes are limited and app-dependent. Therefore the default policy is native decorations, with experimental per-app rules for borderless or compact treatment.

| Policy | Behavior | Risk |
|---|---|---|
| native | Leave frame untouched. | Low |
| dwm-appearance | Apply supported corner/color/frame attributes where meaningful. | Low to medium |
| borderless-tested | Remove selected styles for a tested application and provide shell controls. | High |
| ignore | Do not manage or restyle. | Lowest |

### 9.3 Snapping and layouts

- Do not replace Windows Snap initially; coexist with it.
- Later add optional half, quarter, centered, and user-defined zones.
- Use DeferWindowPos for multi-window layout changes.
- Store normalized placement ratios per monitor rather than raw pixels when persisting layouts.
- Respect app minimum sizes and allow rule-based floating windows.

## 10. Shell user interface

### 10.1 Top bar

- One bar per monitor; primary monitor contains full system controls.
- Reserve work area using an application desktop toolbar (AppBar-style behavior) or equivalent work-area management.
- Left: Activities affordance and optional active application label.
- Center: clock/date or configurable modules.
- Right: network, audio, battery, notifications, and session menu.
- The MVP may initially provide only Activities, workspace indicator, clock, and recovery menu.

### 10.2 Activities overview

1. Take an immutable snapshot of shell state.
2. Create or update preview sources for visible managed windows.
3. Compute workspace and window layout in logical coordinates.
4. Animate from current window rectangles to overview rectangles.
5. Enable hit testing only after the initial animation crosses a configured threshold.
6. Support click-to-focus, drag-to-workspace, close action, keyboard navigation, and search.
7. On exit, animate toward the selected window and restore ordinary input focus.

### 10.3 Dock

- Pinned apps are stored by normalized AppId, not only executable path.
- Running indicators represent zero, one, or multiple windows.
- Click launches or focuses; repeated click cycles windows according to policy.
- Middle click may open a new instance where supported.
- Right click opens a shell-owned jump menu with windows, pinning, launch, and close actions.
- Dock visibility modes: overview-only, always visible, intelligent hide.

### 10.4 Search

```
SearchProvider
  id()
  warm_index()
  query(text, cancellation_token) -> stream<SearchResult>
  activate(result)

Built-in providers:
  applications | open windows | shell commands | settings
Later providers:
  files | calculator | web | user plugins
```

Search must be cancellable and incremental. The UI should not wait for slow providers. Results are ranked by prefix quality, fuzzy match, usage recency, and provider priority.

## 11. Inter-process communication and state

### 11.1 IPC choice

Use Windows named pipes with length-prefixed messages serialized through a stable schema. JSON is acceptable for the first prototype because it is inspectable. A later version may use MessagePack or another binary encoding after profiling. Every message includes `protocol_version`, `request_id`, `sender`, `message_type`, and `payload`.

```json
{
  "protocol_version": 1,
  "request_id": "uuid",
  "sender": "groveshell-ui",
  "message_type": "workspace.switch",
  "payload": { "workspace_id": "uuid" }
}
```

### 11.2 State ownership

| State | Owner | Persistence |
|---|---|---|
| Window inventory | wm-core | Rebuilt every session |
| Workspace membership | wm-core | Session snapshot; reconciled at startup |
| Pinned apps | host/config | Persistent |
| Settings and keybindings | host/config | Persistent, versioned |
| UI animation state | ui | Never persistent |
| Crash counters | watchdog | Persistent small state |
| Search usage ranking | ui/search service | Persistent optional database |

### 11.3 Configuration format

```toml
schema_version = 1

[general]
start_with_windows = false
workspace_backend = "managed"

[input]
move_modifier = "Alt"
move_button = "Left"
resize_button = "Right"

[hot_corners.top_left]
action = "activities"
delay_ms = 150
disable_in_fullscreen = true

[appearance]
top_bar_height = 32
dock_mode = "overview"
animation_scale = 1.0

[[window_rules]]
match_exe = "devenv.exe"
workspace = "Development"
decoration = "native"
```

- Validate configuration before applying it.
- Write through a temporary file, fsync where meaningful, then atomically replace.
- Keep one previous-known-good backup.
- Reject unknown destructive actions but preserve unknown fields when practical for forward compatibility.
- A reload failure must retain the old live configuration.

## 12. Security, privilege, and trust boundaries

- Run the normal shell unelevated.
- Expect limited control over elevated windows due to User Interface Privilege Isolation.
- Do not request UIAccess signing in the MVP.
- If an elevated broker is added, expose a tiny allowlisted protocol and authenticate the client process.
- Never load arbitrary DLL plugins into the shell processes.
- Treat window titles, executable paths, icons, and accessibility text as untrusted input.
- Do not store typed search text or window titles in telemetry by default.
- Use local-only IPC with restrictive pipe security descriptors.
- Do not attempt to operate on the secure desktop or UAC prompt surface.

## 13. Reliability and recovery design

### 13.1 Panic recovery contract

- A hard-coded emergency shortcut remains available even when user keybindings are invalid.
- Recovery closes overview/bar windows, releases hooks, restores all workspace-hidden windows, clears reserved work areas, and starts explorer.exe.
- A command-line recovery executable can be launched from Task Manager or Win+R.
- Crash loops disable shell mode automatically after a threshold.
- The installer creates a Start Menu shortcut named "GroveShell Safe Recovery".

### 13.2 Watchdog protocol

```
host -> watchdog heartbeat every 2 seconds
watchdog marks unhealthy after 6 seconds
watchdog requests graceful recovery
if no response after 2 seconds:
  terminate shell job object
  restore Explorer
  write crash-loop marker
  show minimal diagnostic dialog
```

### 13.3 Job objects

Place non-watchdog shell processes in a Windows job object so the host can terminate the complete process tree on recovery. Keep the watchdog outside that job. Avoid KILL_ON_JOB_CLOSE until shutdown semantics are fully tested.

## 14. Repository structure

```
groveshell/
  Cargo.toml
  LICENSE-APACHE
  LICENSE-MIT
  README.md
  SECURITY.md
  CONTRIBUTING.md
  docs/
    architecture.md
    compatibility.md
    recovery.md
    adr/
  crates/
    common/
    config/
    ipc/
    win32/
    window-model/
    workspace/
    rules/
    input/
    render/
  apps/
    host/
    wm/
    ui/
    watchdog/
    cli/
    settings/
  tests/
    integration/
    fixtures/
    test-apps/
  packaging/
    msix/
    installer/
  scripts/
    dev-start.ps1
    recover.ps1
    collect-diagnostics.ps1
```

## 15. Testing strategy

### 15.1 Test layers

| Layer | Examples |
|---|---|
| Pure unit tests | Rule matching, workspace transitions, layout calculations, config migrations, ranking. |
| Win32 wrapper tests | Window enumeration, style inspection, monitor/DPI conversion, event normalization. |
| Synthetic app tests | Custom fixture apps create dialogs, tool windows, min/max constraints, custom frames, DPI changes. |
| Integration tests | Launch fixture windows, move them across workspaces, restart components, verify state. |
| Visual tests | Capture overview/bar screenshots under fixed topology and compare with tolerance. |
| Soak tests | Run 24-72 hours while opening/closing windows and changing monitors. |
| Manual compatibility matrix | Explorer, Terminal, Firefox, Chrome, VS Code, Office, Electron, packaged apps, games. |
| Recovery tests | Kill every process at every major state transition and verify Explorer restoration. |

### 15.2 Required fixture applications

- Standard decorated Win32 window.
- Borderless window with custom title bar.
- Owned modal dialog.
- Tool window and popup.
- Window with strict minimum and maximum size.
- Per-monitor-DPI-aware window that reports coordinate changes.
- Window that recreates its HWND.
- Elevated window fixture.
- Packaged application fixture where practical.
- High-frequency title-change and create/destroy stress fixture.

## 16. Phased implementation roadmap

### Phase 0 - Foundation and safety

**Goal:** A buildable repository with logging, configuration, IPC skeleton, watchdog, recovery script, CI, and architecture decisions. No hooks and no window movement.

Implementation tasks:
- Create Cargo workspace and shared error/logging conventions.
- Implement single-instance host and named-pipe ping.
- Implement watchdog heartbeat and manual recover command.
- Add structured tracing to rotating local files.
- Add CI for fmt, clippy, test, and Windows build.
- Document supported Windows build and development setup.

Exit criteria:
- All binaries build from a clean clone.
- Killing host causes watchdog to launch Explorer in a test session.
- Configuration parsing has unit tests and safe defaults.

### Phase 1 - Window inventory

**Goal:** Reliable, inspectable model of top-level windows and monitors.

Implementation tasks:
- Wrap EnumWindows and process identity retrieval.
- Install SetWinEventHook listeners for create, destroy, show, hide, focus, location, and name events.
- Build event coalescing and periodic reconciliation.
- Add CLI commands list-windows and list-monitors.
- Record eligibility decisions and reasons.

Exit criteria:
- Opening and closing common apps updates inventory without restart.
- No duplicate WindowId survives HWND reuse tests.
- Idle CPU remains below target.

### Phase 2 - Global move/resize and hot corners

**Goal:** First user-visible companion utility while Explorer remains fully active.

Implementation tasks:
- Implement RegisterHotKey commands.
- Implement low-level mouse/keyboard hook state machine.
- Move and resize the root top-level window beneath the pointer.
- Add fullscreen and ignored-window suppression.
- Implement configurable hot-corner activation with debounce.
- Add on-screen debug overlay.

Exit criteria:
- Alt+drag works across mixed-DPI monitors.
- Escape cancels safely.
- Hooks disappear immediately when the process exits.

### Phase 3 - Managed workspaces

**Goal:** Functional workspace switching without the overview.

Implementation tasks:
- Implement workspace domain model and backend trait.
- Assign new windows to the current workspace.
- Hide/show windows while preserving minimized state.
- Add keyboard switching and move-window shortcuts.
- Add dynamic empty workspace policy.
- Persist session snapshot and reconcile after restart.

Exit criteria:
- No window remains permanently hidden after crash/recovery.
- Dialogs follow their owner by default.
- Workspace switching works with at least 20 mixed app windows.

### Phase 4 - Top bar and dock

**Goal:** The project begins to resemble an alternative desktop shell.

Implementation tasks:
- Create per-monitor top bar windows and reserve working area.
- Show Activities button, workspace indicator, and clock.
- Build app identity normalization and icon loading.
- Implement pinned/running dock with launch and focus.
- Add settings for bar height, dock mode, and keybindings.
- Keep Explorer taskbar enabled by default; offer temporary hide for testing.

Exit criteria:
- Maximized windows respect top bar work area.
- Dock groups common apps correctly.
- Bar and dock survive display topology changes.

### Phase 5 - Activities overview

**Goal:** Overview-first workflow with live previews and workspace interaction.

Implementation tasks:
- Implement overview layout engine.
- Start with DWM thumbnails; abstract preview provider.
- Add DirectComposition transforms and animations.
- Support keyboard focus, click-to-focus, close, drag between workspaces.
- Integrate dock and basic application/window search.
- Add advanced Windows Graphics Capture provider after MVP stability.

Exit criteria:
- Overview opens and closes smoothly on target hardware.
- Preview resources are released when windows close.
- The user can complete a session without using the Windows taskbar.

### Phase 6 - Hardening and accessibility

**Goal:** Prepare an alpha suitable for other contributors.

Implementation tasks:
- Complete per-monitor DPI and topology testing.
- Add UI Automation semantics and keyboard-only traversal.
- Add high-contrast/reduced-motion options.
- Build compatibility rules and ignore list.
- Add diagnostics bundle and privacy controls.
- Run soak, crash injection, and visual regression tests.

Exit criteria:
- No known recovery failure in test matrix.
- Basic screen-reader navigation works.
- Published compatibility matrix distinguishes supported, partial, and ignored apps.

### Phase 7 - Optional Explorer replacement

**Goal:** Dedicated shell mode for advanced testers.

Implementation tasks:
- Implement explicit opt-in onboarding and compatibility checks.
- Provide supported Shell Launcher instructions where available.
- Create fallback startup task or controlled user-shell configuration only with warnings.
- Implement tray/notification strategy or document limitations.
- Ship safe-mode and uninstall recovery paths.
- Block shell mode after repeated crash loops.

Exit criteria:
- A fresh test user can sign in, launch apps, switch workspaces, and recover Explorer.
- Uninstall restores ordinary Windows shell behavior.
- Shell mode remains marked experimental until extended field testing.

## 17. Working effectively with Codex

Do not ask Codex to "build GroveShell." Treat each phase as a sequence of reviewable vertical slices. A good task changes one subsystem, adds tests, updates documentation, and leaves the repository buildable. Keep this document in `docs/PROJECT_PLAN.md` and require Codex to cite the relevant section in each implementation plan.

### 17.1 Per-task workflow

1. Ask Codex to inspect the current repository and restate the relevant invariants.
2. Request a short implementation plan listing files, APIs, failure modes, and tests.
3. Let Codex implement one bounded slice.
4. Run formatting, linting, unit tests, integration tests, and a manual smoke command.
5. Ask Codex to review its own diff specifically for unsafe Win32 lifetime, thread, DPI, and error-handling mistakes.
6. Commit only after the phase acceptance criterion remains true.

### 17.2 Initial Codex bootstrap prompt

```
You are implementing Phase 0 of the GroveShell technical design.
Read docs/PROJECT_PLAN.md completely before modifying code.

Constraints:
- Target Windows 11 x64.
- Use a Rust Cargo workspace.
- Do not add window hooks, shell replacement, or elevated behavior yet.
- Keep every binary runnable and recoverable.
- Use structured errors and tracing.
- Add tests for configuration and IPC framing.
- Update docs/adr with each architectural decision.

First, inspect the repository and propose a file-by-file plan. Do not code until
you have identified the phase exit criteria and the commands that will verify them.
```

### 17.3 Example Phase 1 task prompts

```
Task: implement the first read-only top-level window inventory.

Implement only:
- safe wrappers for EnumWindows and GetWindowThreadProcessId;
- a WindowRecord containing HWND wrapper, PID, title, class, visibility,
  process path when accessible, and diagnostic eligibility reason;
- `groveshell-cli list-windows --json`;
- unit tests for pure eligibility predicates;
- one integration smoke test using a fixture window.

Do not install global hooks yet. Do not move, resize, hide, or restyle windows.
Document all Win32 ownership and lifetime assumptions.
```

```
Task: add out-of-context WinEvent observation to the existing inventory.

Requirements:
- use SetWinEventHook with minimal callbacks;
- callbacks enqueue events and never perform process inspection or logging I/O;
- coalesce location/name events;
- periodically reconcile with EnumWindows;
- safely handle HWND destruction and reuse;
- add metrics for queue depth, dropped events, and reconcile duration.

Show the test plan before implementation.
```

### 17.4 Definition of done for every Codex task

- Code builds on a clean Windows checkout.
- New unsafe blocks are narrowly scoped and include a safety comment.
- Public behavior is covered by a test or explicit manual verification command.
- No callback blocks on slow work.
- Errors include context and do not panic in long-running processes.
- Configuration and IPC changes are versioned.
- Recovery behavior is preserved.
- Documentation and compatibility notes are updated.
- The diff contains no unrelated refactoring.

## 18. Key technical risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Explorer provides hidden integrations | Missing tray, notifications, file-dialog or startup behavior | Coexist first; inventory dependencies; replace incrementally. |
| Undocumented virtual desktop APIs change | Workspace breakage after Windows updates | Backend abstraction; managed fallback; version gating. |
| Global hooks introduce latency | System-wide input stutter | Minimal callback, lock-free/small queue, tracing, automatic hook disable. |
| Window identity is inconsistent | Incorrect dock grouping or rules | Layered AUMID/path/class strategy plus user overrides. |
| Mixed DPI causes jumps | Incorrect move/resize and overview geometry | Per-monitor-DPI-aware v2 from process start; coordinate tests. |
| Apps resist hiding or repositioning | Broken workspace semantics | Compatibility policy, ignore/floating rules, native backend option. |
| Preview capture consumes GPU/memory | Poor battery life and stalls | Lazy previews, frame throttling, thumbnail MVP, pause when overview closed. |
| Shell crash strands user | Severe usability failure | Watchdog, panic shortcut, recovery executable, Explorer fallback. |
| Elevated windows cannot be controlled | Inconsistent behavior | Document limitation; optional narrow broker later. |
| Name infringes GNOME trademark | Repository or release dispute | Resolved: renamed from the original GNOME-derived name to GroveShell (see ADR-008); never use GNOME branding or assets. |

## 19. Packaging, distribution, and release channels

- Early development: zip artifact containing binaries, config, recovery scripts, and symbols.
- Alpha: signed installer if feasible, per-user installation, no shell replacement by default.
- Later: MSIX may be used for settings/UI components, but low-level shell processes may require unpackaged deployment depending on capabilities.
- Publish checksums and a software bill of materials.
- Use semantic versioning only after protocol and configuration compatibility rules are defined.
- Maintain Stable, Preview, and Nightly channels only after automated update and rollback are trustworthy.

## 20. Open-source governance recommendations

- Dual-license under Apache-2.0 OR MIT for broad Rust ecosystem compatibility.
- Require a Developer Certificate of Origin or a lightweight contribution sign-off.
- Publish SECURITY.md with private vulnerability reporting instructions.
- Use architecture decision records for undocumented API use, privilege changes, and shell-mode behavior.
- Require screenshots and compatibility details for UI changes.
- Label experimental features clearly and keep them disabled by default.
- Do not accept copied GNOME assets or code unless license compatibility and attribution are verified independently.

## 21. Recommended first public milestone

The first meaningful public release should not replace Explorer. It should be a safe "workspace companion" containing:

- Reliable window inventory.
- Alt + drag move and resize.
- Configurable hot corner.
- Managed workspaces with keyboard switching.
- Simple top bar with Activities, workspace indicator, and clock.
- Basic overview using DWM thumbnails.
- Panic recovery and a documented compatibility list.

This milestone demonstrates the core idea while avoiding the hardest shell-replacement integrations. It is also small enough for contributors to run without risking their primary Windows session.

## 22. Implementation readiness checklist

| Decision | Recommended default |
|---|---|
| Target OS | Windows 11 x64, currently supported builds |
| Language | Rust stable |
| Win32 bindings | windows crate |
| Rendering | Direct3D 11 + DirectComposition |
| Preview MVP | DWM thumbnails |
| Advanced preview | Windows Graphics Capture |
| Workspace MVP | Managed hide/show backend |
| IPC | Windows named pipes + versioned JSON framing |
| Configuration | TOML + schema version + atomic replacement |
| Logs | tracing + rotating files |
| Installer mode | Per-user, Explorer remains default |
| Recovery | Watchdog + panic shortcut + standalone recover command |
| Project name | GroveShell only as a working codename pending trademark clearance |

## 23. Primary technical references

- GNOME Foundation - Logo and Trademarks
- Microsoft Learn - SetWinEventHook
- Microsoft Learn - SetWindowPos
- Microsoft Learn - RegisterShellHookWindow
- Microsoft Learn - RegisterHotKey
- Microsoft Learn - DwmSetWindowAttribute
- Microsoft Learn - DWMWINDOWATTRIBUTE
- Microsoft Learn - Windows title bar customization
- Microsoft Learn - Manage app windows
- Microsoft Learn - Shell Launcher
- Microsoft Learn - IVirtualDesktopManager
- Microsoft Learn - DWM thumbnail overview
- Microsoft Learn - Windows Graphics Capture

## Appendix A. Initial architecture decision records

**ADR-001: Do not replace DWM**
Decision: The project remains a user-mode shell and controller above the Windows compositor.

**ADR-002: Explorer coexistence first**
Decision: Explorer stays active through the MVP and alpha phases.

**ADR-003: Rust native core**
Decision: Long-running services and Win32 integration are implemented in Rust.

**ADR-004: DirectComposition shell UI**
Decision: Latency-critical overview and shell surfaces use native GPU composition.

**ADR-005: Workspace backend abstraction**
Decision: Managed and native desktop implementations remain interchangeable.

**ADR-006: No universal title-bar replacement**
Decision: Foreign application frames remain native by default.

**ADR-007: Recovery before features**
Decision: Watchdog and Explorer restoration precede global hooks and workspace hiding.

**ADR-008: Working codename only**
Decision: GroveShell is not treated as a cleared release trademark.

## Appendix B. Window-event reconciliation checklist

- Create before metadata is available: retry inspection with bounded backoff.
- Destroy after queued location events: discard events whose generation no longer matches.
- Show/hide caused by own workspace operation: tag operation to prevent feedback loops.
- Owner dialog appears on hidden workspace: inherit owner workspace unless rule overrides.
- Window process exits abruptly: remove all process windows during reconcile.
- Monitor disconnect while dragging: cancel gesture and clamp to remaining work area.
- DPI changes mid-session: recalculate logical geometry and preview layout.
- Explorer restarts: rediscover shell/taskbar windows and reapply coexistence state.
- Secure desktop transition: suspend hooks and previews, then reconcile on return.
- Resume from sleep: rebuild capture sources and monitor topology.

## Appendix C. Release gates

| Gate | Required evidence |
|---|---|
| Prototype | Build instructions, recovery script, no shell replacement. |
| MVP | Phase 0-3 exit criteria, 8-hour soak, common-app smoke test. |
| Alpha | Overview and top bar, recovery fault injection, mixed-DPI tests, published limitations. |
| Beta | Installer/uninstaller recovery, accessibility basics, telemetry/privacy review, 72-hour soak. |
| Experimental shell mode | Dedicated-user test, crash-loop fallback, uninstall restoration, explicit warning. |
| 1.0 | Stable configuration/IPC contracts, broad compatibility matrix, signed releases, independent project name. |

The correct first coding task is Phase 0: repository, recovery, configuration, IPC, diagnostics, and CI. Do not begin by replacing Explorer or drawing the overview.
