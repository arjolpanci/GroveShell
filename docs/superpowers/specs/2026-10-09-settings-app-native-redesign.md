# Settings App — Native Redesign, First Run, and Release Model

**Date:** 2026-10-09
**Status:** Proposed.
**Relevant ADRs:** ADR-003 (Rust native core), ADR-004 (DirectComposition
shell UI), ADR-010 (one painter, two canvas backends), ADR-009 (control
center and tray integration).
**Builds on:** `docs/superpowers/specs/2026-07-30-tray-settings-app-design.md`
(the app this redesigns) and `2026-10-08-win11-native-shell.md` (the design
system this adopts).

## 1. Goal

Make `groveshell-settings` read as a Windows 11 settings app rather than a
developer dashboard, and give the product a coherent install → run →
startup story.

Two outcomes:

1. The window uses the same native design system `apps/ui` already uses —
   system theme, system accent, Mica, Segoe UI Variable, Segoe Fluent
   Icons — rendered through the same `Canvas` painter.
2. A user installs GroveShell, runs it once, answers one question, and is
   never asked again.

## 2. What is wrong today

Three problems, all visible in the shipped build:

**The Home page lies.** It reports *"Unhealthy: watchdog is not running"*
while the shell is running on screen. `apps/settings/src/imp/process.rs`
documents the cause itself: when GroveShell was already running before
this instance started, "it doesn't know their pids". Health answers *did I
spawn these* rather than *are these running*.

**Home is a dashboard, not a settings page.** Process rows with CPU/RAM
and a 220px "Start GroveShell" button are a developer's view of the
system. A released app's landing page should not be an operations console.

**The visual language is invented, not native.** Every page centers its
text; a toggle's caption floats ~100px from its switch; colors are
hardcoded dark-only literals (`BG_WINDOW = 0x202020`, a brown-orange
accent) that ignore the system theme and accent; the window is a fixed
780×620 with no DPI scaling, no resize, no scroll, no hover, press, or
focus states, and no icons.

This last point is the odd one, because the shell solved it already:
`apps/ui/src/imp/design/` reads the system accent from
`HKCU\…\DWM\AccentColor`, follows light/dark and high contrast, and owns
the type ramp and metrics. The settings app cannot use any of it — it is
`pub(crate)` inside a different binary — so it reimplemented a worse
version. §5 fixes that by extraction, not by copying.

### 2.1 Reference material

The design below is taken from three captures made on a real Windows 11
machine, not from memory: **Settings → Personalization → Taskbar** (the
closest analogue to this app), **Settings → Accessibility → Text size**
(card and row metrics), and **PowerToys Settings** (the best third-party
precedent — a utility with a background component whose Home is
status-only).

They are not committed: they contain the capturing user's account name and
address. Reproduce them with a DPI-aware `CopyFromScreen` capture of the
target window (the scratchpad script used for this spec is the reference
implementation; `SetProcessDpiAwarenessContext(-4)` before capture is
required or the result is a cropped, upscaled fragment).

What they establish:

- Rows live inside cards. Icon gutter on the left, title plus muted
  description stacked, control hard-right. Nothing is centered.
- A switch is preceded by its own `On`/`Off` text label.
- Related rows collapse into one expander card with a chevron in its
  header; the header carries a title and a description of the group.
- Group captions sit above cards in small text ("Related settings").
- Status is phrased as calm fact ("You're up to date"), with an icon
  carrying the severity — not red body text.

## 3. Release and process model

**Startup.** The `HKCU\…\Run` entry stays `groveshell-settings.exe` and
gains a `--background` argument. At login the process starts silently:
tray icon, no window, and it spawns watchdog → host → ui exactly as it
does today. The user never presses a Start button because nothing waits
for them to.

Launched without `--background` (Start menu, top-bar gear, tray
double-click) it shows the Settings window, and still ensures the shell is
running, as it does now.

**Decision taken:** one binary keeps this role. Splitting launch and
supervision into a separate `groveshell.exe` is cleaner in principle but
ships another binary and another thing to explain; it is not required by
anything in this spec and is deferred.

**First run.** First run is "no config file exists yet", which is already
how `config_store` distinguishes a fresh profile. The window opens on a
**Welcome** page instead of the normal Settings content:

- A line stating GroveShell is running now, and that while it runs it
  takes over the Windows taskbar's screen space (`imp::taskbar`), which is
  restored on exit. This is a statement, not a choice: there is no
  config key for keeping the taskbar, and inventing one is out of scope.
- One real choice: **Start GroveShell when I sign in**, default on,
  writing both the Run key (`autostart::set_enabled`) and
  `general.start_with_windows`, which are two representations of one fact
  and must be written together.
- A **Get started** button that writes the config file and navigates to
  Settings.

Welcome is never shown again once the config file exists. It is reachable
afterwards only from About.

## 4. Information architecture

| Page | Contents |
|---|---|
| Home | Status card, startup toggle, problem banner when broken |
| Top Bar | Height, blur, what appears on the bar |
| Dock | Mode, icon size, alignment |
| Overview | Blur, previews |
| Workspaces | Backend, per-monitor behavior |
| Input | Move/resize modifiers and buttons, hot corners |
| Accessibility | Reduced motion, high contrast |
| About | Version, config file location, logs folder, re-run Welcome |

Workspaces and About are new; the rest are the existing pages re-laid out.
The nav rail gains a Segoe Fluent icon per item and keeps its current
selection language (pill plus left accent bar), which already matches
Windows 11.

## 5. The shared design crate

Extract `apps/ui/src/imp/design/` (color, typography, metrics, material,
motion), `imp::canvas` (the `Canvas` trait and both backends) and
`imp::icons` into a new workspace crate, `crates/ui-kit`
(`groveshell-ui-kit`), consumed by both `apps/ui` and `apps/settings`.

This is the cornerstone: the settings app becomes native by *using the
shell's own design system*, not by growing a second copy. ADR-010 already
blesses the painter shape; this only moves it somewhere both binaries can
reach.

The extraction is mechanical — these modules are leaf dependencies within
`apps/ui` — but it is the one step that touches a working shell, so it
lands as its own change, with `apps/ui` building and running against the
crate before any settings work starts (§11).

`apps/settings/src/imp/theme.rs` is deleted. Its widgets (`draw_toggle`,
`draw_slider`, `draw_segmented`, `fill_round_rect`, `draw_card`) are
superseded by §6 and must not be ported.

## 6. Control library and layout model

Pages stop computing rects by hand. A page declares rows; a layout pass
assigns rectangles; a paint pass draws them through `Canvas`; hit-testing
reads the same rectangles. One list, three consumers — the rule the shell
already follows for `region_at` and `qs_layout`.

Row kinds, each title + optional description + optional icon:

| Kind | Control | Used by |
|---|---|---|
| Toggle | switch with `On`/`Off` label | blur, reduced motion, startup |
| Slider | track, thumb, live value label | bar height, dock icon size |
| Choice | dropdown or segmented control | dock mode, alignment, backend |
| Navigation | chevron, opens a subpage | future |
| Action | button | Restart GroveShell |
| Link | accent-colored text | logs folder, config file |
| Status | severity icon, two lines | Home |

Cards group rows; an expander card adds a chevron header and a collapsed
state. Both are containers over the row list, not new drawing code.

Keyboard and focus are part of this layer, not an afterthought: Tab moves
between rows, Space/Enter activates, arrows adjust sliders and choices,
and the focused row draws the system focus rectangle. Getting this right
in the shared row code is what makes it correct on every page at once.

**Metrics** (radius, row heights, gutters, content column width) are to be
measured from the §2.1 captures during implementation rather than guessed
here. The conventions they should land near — 4–8px card radius, ~48px
single-line and ~68px two-line rows, 20px icons, a content column that
stops growing past roughly 1000px — are starting points to verify, not
values to hardcode on this document's authority.

## 7. The window

- **Mica** via `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW`, the same
  attribute the bar uses, plus `DWMWA_USE_IMMERSIVE_DARK_MODE` tracking
  the system theme so the title bar matches.
- **Resizable**, with a sensible minimum, and a vertically scrolling
  content column. The current fixed 780×620 exists only because the
  layout cannot reflow; once §6 lands it can.
- **Per-monitor DPI**: `WM_DPICHANGED` relayouts. The app declares
  per-monitor-v2 awareness.
- **Theme and accent changes apply live** via `WM_SETTINGCHANGE`, the same
  way the shell already reacts (`refresh_theme`, `refresh_accent`).
- A custom title-bar region carrying the app name, matching the shell's
  own chrome. Search is explicitly **not** in this spec (§9).

## 8. Home, and honest health

Home shows one status card and the startup toggle.

Health is rewritten to ask the system rather than consult a child list:
the shell is running if its known window class is present, and healthy if
the host answers a pipe ping. Both already exist
(`health::host_ping_ok`, the `GroveShellBar` class the dev scripts
already locate by name). `pid_for`-based reporting is deleted.

Three states:

| State | Home shows |
|---|---|
| Running, host responding | "GroveShell is running · v0.1.0" |
| Running, host not responding | Warning row, Restart action |
| Not running | "GroveShell isn't running", Start action |

The per-process CPU/RAM rows move to About, below the version, where a
developer can still find them and a user will not trip over them.
`imp::health`'s sampling code is kept for that purpose.

## 9. Out of scope

- The installer (MSI/winget/MSIX). This spec assumes the app runs from its
  install directory and writes its own Run key.
- Settings search. It is the natural next step once rows are data, but it
  is not needed to make the app native and adds a nav surface.
- A separate launcher binary (§3).
- Narrator/UI Automation support. Keyboard and focus are in scope (§6);
  exposing an automation tree is not, and that gap should be stated
  plainly rather than implied by "accessible".

## 10. Testing

The layout and state logic is pure and testable; the drawing is not. That
boundary is the test plan.

- Row layout: a declared row list produces non-overlapping rects in order,
  inside the content column, at several DPI scales and window widths —
  the same property `quick_settings`' `controls_do_not_overlap…` test
  already asserts for the shell.
- Hit-testing agrees with layout for every row kind.
- Keyboard traversal: Tab order matches visual order and wraps; a
  disabled row is skipped.
- Theme tokens: light, dark and high contrast each resolve to distinct,
  non-transparent values, and accent parsing from an ABGR DWORD is
  verified against a known value.
- First run: a missing config file selects Welcome; a present one does
  not; Get started writes both the Run key and `start_with_windows`.
- Health: each of the three §8 states is produced by its own condition,
  with the window-class and ping probes injected rather than called.
- Rendering changes are verified by capturing the window and looking at
  it, as this spec's own reference material was gathered.

## 11. Order of work

1. Extract `crates/ui-kit`; `apps/ui` builds and runs unchanged against it.
2. Row/card layout engine and the control library, with tests, rendering
   through `Canvas`.
3. Window shell: Mica, DPI, resize, scroll, live theme.
4. Port the existing pages to declared rows; delete
   `apps/settings/src/imp/theme.rs`.
5. Honest health, then Home.
6. Welcome, `--background`, and the startup wiring.
7. About, Workspaces.

Steps 1–3 are invisible to the user and land first; step 4 is where the
app visibly changes.

## 12. Known gaps carried forward

The rendering work (§5-§7, the pages, the nav rail) is implemented. A
whole-branch review raised three Critical and eight Important findings;
all were fixed. These are the ones deliberately left, to be picked up by
the plan that implements §3 and §8:

- `WM_GETMINMAXINFO` constrains *window* size where *client* size was
  meant, so the minimum is ~16px tighter than the layout asks for.
  `layout_page`'s own clamp absorbs it today; compute it through
  `AdjustWindowRectExForDpi`.
- `Control::Status` reserves no width, so a long status title ellipsises
  into its severity glyph rather than before it.
- A toggle's `On`/`Off` label gets 20px and no ellipsis flag.
- Nothing draws a scrollbar or any other scroll affordance, so a
  scrollable page gives no sign there is more below it.
- Choice rows display raw config identifiers — `autohide`, `CtrlAlt`,
  `top_left`. Not a regression, but it reads as a developer dashboard,
  which is what §2 set out to remove. The display label wants to be
  separate from the stored value.
- `pages::Page::on_activate` takes `&mut self`, so the window holds a
  `RefCell` borrow across `toggle_groveshell()`, which blocks for up to
  3s per child. Today that is a frozen window, not a panic, because
  none of those paths pump messages — but `cards()` borrows the same
  cell, so the day one does, the process aborts. All six page structs
  are stateless; `&self` would remove the hazard outright.
- `material::apply`'s return value is discarded. §3.3 describes `false`
  as the feature detection that should select the legacy blur-behind
  path; on pre-22621 Windows the window currently gets no backdrop and,
  with `WS_EX_NOREDIRECTIONBITMAP`, a transparent nav rail.
- §7's "custom title-bar region carrying the app name" is not
  implemented; the window uses the standard caption with a dark-mode
  tint plus a page-title band. Arguably more native — worth deciding
  explicitly rather than by omission.
- Unexplained: `groveshell-settings` exited once during verification
  with no panic on stderr and no shutdown log line. Twenty synchronous
  `WM_MOUSEMOVE` messages and four consecutive resizes do not reproduce
  it.
