# Phase 4 Shell UI — Design Language & Remaining Features Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish Phase 4 by unifying every shell surface under one design-token +
motion system (native Win11 look with a live Windows accent and subtle Mica),
and landing the remaining features — tray hosting, session menu, opt-in desktop
dock modes, notifications indicator.

**Architecture:** A new `design::` token module (color/metrics/motion) becomes the
single source of truth; existing surfaces migrate their hard-coded literals to it.
A shared `flyout` lifecycle module unifies the pop-ups. New `tray`, `session_menu`,
and `desktop_dock` modules add features, reusing an extracted `dock_render`.

**Tech Stack:** Rust, `windows` crate (Win32 GDI/Direct2D, DWM, Registry, Shell,
Power), existing `groveshell-config`/`groveshell-common` crates. No new runtime deps.

## Execution status (2026-08-11, branch `phase-4-shell-ui`)

Implemented inline with build + unit-test gates on each commit; visual
surfaces still need a live pass (`scripts/dev-start.ps1`), per the agreed
"continue inline, verify later" approach.

- **Done & verified (build + tests):** Task 1 (design tokens + live accent),
  Task 2 (token migration + accent-change handler), Task 4 (tray-overflow
  chevron hosting the real Windows overflow window), Task 5 (session/power
  menu). Task 3's `flyout` lifecycle and Task 6's `desktop_dock` reveal state
  machine are implemented and unit-tested.
- **Deferred for live verification (tested cores in place):** wiring the
  flyout grow-from-anchor motion into the existing Quick Settings / calendar
  windows (`flyout` module ready), and building the opt-in floating
  desktop-dock window that consumes the `desktop_dock` reveal SM. The Dock
  settings page already exposes `always`/`autohide`, which stay inert until
  that window lands.
- **Notifications:** the calendar flyout's notifications section exists and is
  token-styled; a dedicated bar bell indicator is deferred (glyph choice best
  confirmed live).
- **Explicitly out of scope (unchanged):** DND / Night Light / brightness.

## Global Constraints

- Windows-only; every new source file is under `apps/ui/src/imp/` and gated like its neighbors.
- No new runtime dependency beyond the workspace's existing set.
- All new config reads from paint/reconcile code go through re-entrancy-safe thread-local mirrors in `state.rs` (project memory: nested `STATE.with(borrow)` panics recur).
- Colors are stored as Win32 `COLORREF` (`0x00BBGGRR`); one `rgb()` helper converts from `#RRGGBB`.
- Motion honors `appearance.reduced_motion` (0 duration) and `appearance.animation_scale` (multiplier).
- `dock_mode` default stays `"overview"`; `always`/`autohide` are opt-in.
- Never co-author commits; keep the diff free of unrelated refactoring.
- The config `save`-path tests fail environmentally on this machine (project memory) — verify via logic tests + `cargo build`, and run the suite with `--no-fail-fast`.

---

### Task 1: `design::` foundation — color tokens, live accent, metrics, motion

**Files:**
- Create: `apps/ui/src/imp/design/mod.rs`, `design/color.rs`, `design/metrics.rs`, `design/motion.rs`
- Modify: `apps/ui/src/imp/mod.rs` (add `mod design;`), `apps/ui/src/imp/state.rs` (accent mirror), delete `apps/ui/src/imp/palette.rs` after folding it in
- Test: inline `#[cfg(test)]` in `color.rs` and `motion.rs`

**Interfaces:**
- Produces:
  - `design::color::{surface_base, surface_raised, surface_overlay, text, text_muted, stroke, accent, accent_text}() -> u32`
  - `design::color::rgb(hex: u32) -> u32` (swaps `0x00RRGGBB` → `0x00BBGGRR`)
  - `design::color::accent_from_dword(argb: u32) -> u32`
  - `design::color::refresh_accent()` and `state::set_accent(u32)` / `state::accent() -> u32`
  - `design::metrics::{RADIUS_CHIP, RADIUS_CARD, SPACING, STROKE_WIDTH}` and `shadow()`
  - `design::motion::{FAST_MS, BASE_MS, ease_out_cubic(f32)->f32, ease_in_out_cubic(f32)->f32, effective_ms(named: u32) -> u32}`

- [ ] **Step 1: Write failing tests for `rgb`, `accent_from_dword`, easing, `effective_ms`**

```rust
// design/color.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rgb_swaps_to_colorref() { assert_eq!(rgb(0x00_1E_2A_3C), 0x00_3C_2A_1E); }
    #[test] fn accent_from_dword_strips_alpha_and_swaps() {
        // DWM AccentColor is AABBGGRR; drop AA, keep as COLORREF (BBGGRR).
        assert_eq!(accent_from_dword(0xFF_3C_2A_1E), 0x00_3C_2A_1E);
    }
}
// design/motion.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ease_out_cubic_endpoints() { assert_eq!(ease_out_cubic(0.0),0.0); assert_eq!(ease_out_cubic(1.0),1.0); }
    #[test] fn ease_out_cubic_is_ahead_of_linear_midway() { assert!(ease_out_cubic(0.5) > 0.5); }
    #[test] fn reduced_motion_zeroes_duration() { assert_eq!(effective_ms_with(BASE_MS, true, 1.0), 0); }
    #[test] fn scale_multiplies_duration() { assert_eq!(effective_ms_with(BASE_MS, false, 2.0), BASE_MS*2); }
}
```

- [ ] **Step 2: Run tests, verify they fail to compile (modules absent).**
  Run: `cargo test -p groveshell-ui design:: 2>&1 | tail`

- [ ] **Step 3: Implement `design/color.rs`.** `rgb` byte-swaps; `accent_from_dword` masks `& 0x00FFFFFF`; token fns branch on `state::high_contrast()` (fold in the Phase 6 palette values); `accent()` reads `state::accent()`; `refresh_accent()` reads `HKCU\Software\Microsoft\Windows\DWM` value `AccentColor` (fallback `ColorizationColor`, then constant `rgb(0x004CC2FF)`) via `RegGetValueW`, calls `state::set_accent`.

- [ ] **Step 4: Implement `design/metrics.rs`** (consts + `shadow()` returning a small struct `{blur:i32, dx:i32, dy:i32, color:u32}`).

- [ ] **Step 5: Implement `design/motion.rs`.** `effective_ms(named)` reads `state::animation_config()`; factor the pure part into `effective_ms_with(named, reduced, scale)` so it's testable without state.

- [ ] **Step 6: Add `accent` + setter mirror to `state.rs`** (thread-local `Cell<u32>`, same pattern as `HIGH_CONTRAST`), default the fallback constant.

- [ ] **Step 7: Fold `palette.rs` into `design::color`.** Repoint the Phase 6 call sites (`bar.rs`, `calendar.rs`) from `palette::*` to `design::color::*` (names map 1:1: `text`,`text_muted`,`background→surface_base`,`panel→surface_raised`,`accent`). Delete `palette.rs`, remove `mod palette;`.

- [ ] **Step 8: Call `design::color::refresh_accent()` at startup** (in `main`, after `set_compat_a11y_config`).

- [ ] **Step 9: Run tests + build.**
  Run: `cargo test -p groveshell-ui --no-fail-fast 2>&1 | tail -5` (expect pass) and `cargo build -p groveshell-ui 2>&1 | tail -3`.

- [ ] **Step 10: Commit.** `git add apps/ui && git commit -m "feat(ui): design token module (color/accent/metrics/motion), folding in palette"`

---

### Task 2: Migrate surfaces to tokens + Mica material + accent refresh handler

**Files:**
- Modify: `apps/ui/src/imp/bar.rs`, `calendar.rs`, `quick_settings.rs`, `overview.rs`/`overview_gpu.rs` (color literals → `design::color::*`), `mod.rs` (accent-change message handling + Mica)

**Interfaces:**
- Consumes: all of Task 1.

- [ ] **Step 1: Grep the remaining hard-coded colors.**
  Run: `grep -rn "0x00[0-9A-Fa-f]\{6\}" apps/ui/src/imp/{bar,calendar,quick_settings,overview,overview_gpu}.rs`

- [ ] **Step 2: Replace each literal with the nearest token** (`0x00E0E0E0`→`text()`, `0x00202020`→`surface_base()`, `0x00262626`→`surface_raised()`, `0x00303030`→`surface_overlay()`, muted greys→`text_muted()`, active/selected → `accent()`). Keep the color-key magenta (`0x00FF00FF`) and shadow blacks as-is.

- [ ] **Step 3: Handle `WM_DWMCOLORIZATIONCOLORCHANGED` + `WM_SETTINGCHANGE` in `wndproc`:** call `design::color::refresh_accent()` then invalidate the bar and any open flyout HWNDs. (Add `WM_DWMCOLORIZATIONCOLORCHANGED = 0x0320`.)

- [ ] **Step 4: Confirm Mica material** — the bar/overview already call `set_blur_behind`; ensure the bar's default enables the subtle blur (respect `top_bar_blur` config; if off, leave solid). No new API.

- [ ] **Step 5: Build + visual smoke.**
  Run: `cargo build -p groveshell-ui`, then via the run skill launch the shell and screenshot the bar + quick settings; confirm accent shows on the active workspace dot and toggles.

- [ ] **Step 6: Commit.** `git commit -m "feat(ui): route shell surfaces through design tokens; live Windows accent"`

---

### Task 3: Shared `flyout` lifecycle module + grow-from-anchor motion

**Files:**
- Create: `apps/ui/src/imp/flyout.rs`
- Modify: `mod.rs` (`mod flyout;`), `quick_settings.rs` + `calendar.rs` (drive open/close through it)
- Test: inline `#[cfg(test)]` in `flyout.rs`

**Interfaces:**
- Produces:
  - `enum FlyoutPhase { Hidden, Opening, Open, Closing }`
  - `struct Flyout { phase, started: Instant }` with `open()`, `close()`, `is_visible()->bool`, `tick(now)->f32` (eased 0..1 progress; advances phase when a transition completes), `scale_opacity()->(f32,f32)`.

- [ ] **Step 1: Write failing tests** for the transition table:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn open_moves_hidden_to_opening() { let mut f=Flyout::new(); f.open(); assert_eq!(f.phase, FlyoutPhase::Opening); }
    #[test] fn opening_completes_to_open() { let mut f=Flyout::new(); f.open(); f.force_complete(); assert_eq!(f.phase, FlyoutPhase::Open); }
    #[test] fn close_mid_open_goes_to_closing() { let mut f=Flyout::new(); f.open(); f.close(); assert_eq!(f.phase, FlyoutPhase::Closing); }
    #[test] fn reduced_motion_opens_instantly() { let mut f=Flyout::new(); f.open_instant(); assert_eq!(f.phase, FlyoutPhase::Open); }
}
```

- [ ] **Step 2: Run, verify fail.** `cargo test -p groveshell-ui flyout 2>&1 | tail`
- [ ] **Step 3: Implement `flyout.rs`** — phase enum + struct; `tick` uses `design::motion::ease_out_cubic` over `effective_ms(BASE_MS)`; `scale_opacity` maps progress → (0.96..1.0, 0..1) for Opening and the reverse for Closing.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Drive Quick Settings + Calendar through `Flyout`** — replace their ad-hoc show/hide with `open()`/`close()`, paint with the current `scale_opacity` (apply via layered-window alpha / D2D transform already present).
- [ ] **Step 6: Build + visual smoke** (flyouts grow from anchor; reduced-motion pops instantly). 
- [ ] **Step 7: Commit.** `git commit -m "feat(ui): shared flyout lifecycle with grow-from-anchor motion"`

---

### Task 4: Tray — curated indicator layout + overflow probe/render/forward

**Files:**
- Create: `apps/ui/src/imp/tray.rs`
- Modify: `bar.rs` (right-side layout gains a chevron region + notifications bell slot), `mod.rs` (`mod tray;`)
- Test: inline `#[cfg(test)]` in `tray.rs` for button-rect math

**Interfaces:**
- Produces:
  - `struct TrayButton { hwnd: isize, icon: HICON, rect_in_toolbar: RECT, tooltip: String }`
  - `fn probe() -> Option<Vec<TrayButton>>` (None on newer builds where the toolbar is absent)
  - `fn forward_click(button: &TrayButton)` (SendMessage to the real toolbar/owner)
  - `fn overflow_available() -> bool`

- [ ] **Step 1: Write failing test for the pure rect mapping** (mapping a toolbar-relative button rect + toolbar screen origin → screen rect):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn button_screen_rect_offsets_by_toolbar_origin() {
        let r = button_screen_rect(RECT{left:2,top:0,right:18,bottom:16}, (100, 50));
        assert_eq!((r.left,r.top,r.right,r.bottom),(102,50,118,66));
    }
}
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement `probe()`** — `FindWindow("Shell_TrayWnd")` → child `TrayNotifyWnd` → `SysPager` → `ToolbarWindow32`; if any missing return `None`. Read `TB_BUTTONCOUNT`, then for each `TB_GETBUTTON`/`TB_GETITEMRECT` cross-process (`ReadProcessMemory` into a local `TBBUTTON`; allocate remote buffer with `VirtualAllocEx` in the tray's process, `SendMessage`, `ReadProcessMemory` back, `VirtualFreeEx`). Extract icon via the button's image list. Guard every failure to a clean `None`. (This is the classic community technique; comment the safety of each cross-process step.)
- [ ] **Step 4: Implement `button_screen_rect`** (pure) and `forward_click` (compute screen rect, `SendMessage(toolbar, WM_LBUTTONDOWN/UP, ...)` at the button's toolbar-relative point).
- [ ] **Step 5: Bar layout + overflow flyout** — add a chevron region on the bar right when `overflow_available()`; clicking opens a `Flyout` listing the tray icons; clicking one calls `forward_click`. Add a notifications bell slot left of the indicator pill.
- [ ] **Step 6: Run tests + build + smoke** (icons appear on a classic-tray build; chevron hidden when `probe()` is `None`).
- [ ] **Step 7: Commit.** `git commit -m "feat(ui): curated tray indicators + best-effort overflow hosting"`

---

### Task 5: Session / power menu

**Files:**
- Create: `apps/ui/src/imp/session_menu.rs`
- Modify: `bar.rs` (session glyph region far right), `mod.rs` (`mod session_menu;`)
- Test: inline test for the action enum → confirm-required mapping

**Interfaces:**
- Produces:
  - `enum SessionAction { Settings, Lock, SignOut, Sleep, Restart, ShutDown }`
  - `fn needs_confirm(a: SessionAction) -> bool` (true for SignOut/Restart/ShutDown)
  - `fn execute(a: SessionAction)`

- [ ] **Step 1: Failing test:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn destructive_actions_need_confirm() {
        assert!(needs_confirm(SessionAction::ShutDown));
        assert!(needs_confirm(SessionAction::Restart));
        assert!(needs_confirm(SessionAction::SignOut));
        assert!(!needs_confirm(SessionAction::Lock));
        assert!(!needs_confirm(SessionAction::Settings));
    }
}
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement `execute`** — Settings: `ShellExecuteW` the settings exe; Lock: `LockWorkStation`; Sleep: `SetSuspendState(false,false,false)`; SignOut/Restart/ShutDown: acquire `SE_SHUTDOWN_NAME` via `OpenProcessToken`/`AdjustTokenPrivileges` then `ExitWindowsEx(EWX_LOGOFF|EWX_REBOOT|EWX_SHUTDOWN, ...)`. Log failures.
- [ ] **Step 4: Session flyout UI** — a `Flyout` menu with the six rows (icon + label), inline confirm row for destructive ones.
- [ ] **Step 5: Run test + build + smoke** (menu opens; Lock works; do NOT actually trigger shutdown in smoke — verify the confirm row appears).
- [ ] **Step 6: Commit.** `git commit -m "feat(ui): session/power menu (settings, lock, sign out, sleep, restart, shut down)"`

---

### Task 6: Extract `dock_render`; add `desktop_dock` with always/autohide

**Files:**
- Create: `apps/ui/src/imp/dock_render.rs` (extracted layout/paint/hit-test), `apps/ui/src/imp/desktop_dock.rs`
- Modify: `dock.rs` (call the extracted renderer), `mod.rs`, `state.rs` (dock-mode mirror + reveal state), config already has `dock_mode`
- Test: inline tests for the reveal state machine + dock layout math

**Interfaces:**
- Produces:
  - `dock_render::layout(icons: &[DockIcon], bounds: RECT, icon_size: i32, align: &str) -> Vec<RECT>`
  - `enum RevealPhase { Hidden, Revealing, Shown, Hiding }`
  - `struct AutoHide { phase }` with `on_hot_edge()`, `on_leave()`, `tick(now)->f32`
  - `desktop_dock::apply_mode(mode: &str)` (create/destroy window, register/unregister bottom AppBar)

- [ ] **Step 1: Failing tests** for `layout` (centered row math already exists in `dock.rs` — move its test here) and the reveal SM:

```rust
#[test] fn hot_edge_from_hidden_starts_revealing() { let mut a=AutoHide::new(); a.on_hot_edge(); assert_eq!(a.phase, RevealPhase::Revealing); }
#[test] fn leave_from_shown_starts_hiding() { let mut a=AutoHide::new(); a.force(RevealPhase::Shown); a.on_leave(); assert_eq!(a.phase, RevealPhase::Hiding); }
#[test] fn re_enter_cancels_hide() { let mut a=AutoHide::new(); a.force(RevealPhase::Hiding); a.on_hot_edge(); assert_eq!(a.phase, RevealPhase::Shown); }
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Extract `dock_render`** from `dock.rs` — the icon layout, running-dot, and paint into free functions taking explicit params; `dock.rs` (overview dock) calls them. No behavior change; overview dock still works.
- [ ] **Step 4: Implement `desktop_dock`** — a `WS_POPUP|WS_EX_LAYERED|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE` window at bottom-center; `always` registers a bottom AppBar strip (mirror `register_appbar`); `autohide` registers no AppBar and uses the `AutoHide` SM driven by a bottom hot-edge check in the existing hot-corner/mouse path + a dwell timer. Paint via `dock_render`. Reuse dock click/launch/focus from `dock.rs` (shared helpers).
- [ ] **Step 5: Wire `dock_mode`** — mirror it in `state.rs`; `apply_mode` called at startup and on `WM_APP_CONFIG_RELOADED`. Leaving `always` unregisters the AppBar.
- [ ] **Step 6: Run tests + build + smoke** — set `dock_mode="always"`, confirm a persistent bottom dock that reserves work area; `"autohide"` reveals on bottom-edge; `"overview"` unchanged.
- [ ] **Step 7: Commit.** `git commit -m "feat(ui): opt-in always-visible / autohide desktop dock via shared dock_render"`

---

### Task 7: Notifications indicator + flyout section

**Files:**
- Modify: `bar.rs` (bell click opens calendar flyout at notifications section), `calendar.rs` (add a notifications section header + placeholder copy)

**Interfaces:**
- Consumes: `flyout` (Task 3), the bell slot added in Task 4.

- [ ] **Step 1: Add a notifications section to the calendar flyout** — a header "Notifications" + muted "No live notifications yet" line below the month grid, styled with tokens.
- [ ] **Step 2: Wire the bell** — clicking the Task-4 bell opens the calendar flyout (scrolled/anchored to the notifications section).
- [ ] **Step 3: Build + smoke** (bell opens the flyout; section renders).
- [ ] **Step 4: Commit.** `git commit -m "feat(ui): notifications indicator and flyout section (indicator-only, listener deferred)"`

---

### Task 8: Docs — Phase 4 completion

**Files:**
- Modify: `README.md` (Phase 4 checklist), `docs/compatibility.md` (tray-hosting note), optionally a short `docs/adr/` entry for reversing the overview-only dock decision.

- [ ] **Step 1: Update the README Phase 4 checklist** — check off tray hosting (best-effort), session menu, dock modes, notifications indicator; note DND/Night Light/brightness still deferred.
- [ ] **Step 2: Add an ADR** noting the dock is now overview-by-default with opt-in always/autohide (supersedes the overview-only memory).
- [ ] **Step 3: Commit.** `git commit -m "docs: mark Phase 4 features complete; ADR for opt-in desktop dock"`

---

## Self-Review

**Spec coverage:** §3 tokens → Task 1; §3.1 accent → Task 1/2; migration + Mica → Task 2; §5.1 flyout SM → Task 3; §7 tray + §6.1 forwarding → Task 4; session menu → Task 5; §5.3/§7 dock → Task 6; notifications → Task 7; docs → Task 8. All spec sections covered.

**Placeholder scan:** Notifications "placeholder copy" is the intentionally-deferred feature per spec §11, not a plan gap. No TBD/TODO steps.

**Type consistency:** `design::color::*`, `Flyout`/`FlyoutPhase`, `TrayButton`/`probe`/`forward_click`, `SessionAction`/`needs_confirm`/`execute`, `dock_render::layout`, `AutoHide`/`RevealPhase` are used consistently across tasks.

## Notes for the implementer

- TDD applies to the **pure logic** (color conversion, easing, phase machines, layout/rect math). GDI/Direct2D painting is verified by build + a real-app screenshot (the `run` skill), not unit tests — don't fabricate render tests.
- After each task: `cargo build -p groveshell-ui` and `cargo test -p groveshell-ui --no-fail-fast` must be green before committing.
