# Windows 11 Native Material — Mica, System Theme, Fluent Typography

**Date:** 2026-10-08
**Status:** Approved design — not an implementation guarantee.
**Supersedes:** the deferred items of
`docs/superpowers/specs/2026-08-11-phase-4-shell-ui-design-language.md` §3
("subtle Mica-like translucency on bar + flyouts") and its plan's Task 3
Step 5 (consuming the `flyout` lifecycle).
**Relevant ADRs:** ADR-003 (Rust native core), ADR-004 (DirectComposition
shell UI) — both upheld, see §1.1.

## 1. Goal

Make the top bar and its flyouts read as native Windows 11 surfaces:
the system backdrop materials, the system light/dark theme, the system
icon font, and the system type ramp. Close out the material and motion
items the Phase 4 design language specified but left unbuilt.

### 1.1 Why not .NET / WinUI 3

The originating request was to rewrite these surfaces with .NET GUI
libraries for a more native look. Rejected, and ADR-004 already decided
it: WinUI 3's window model fights the three things these surfaces
require — always-topmost, no-activate, and arbitrarily anchored
flyouts — and adopting it would mean either a second process per
surface with IPC, an in-process CLR host, or rewriting ~15k lines of
`apps/ui` in C#.

The investigation behind this spec found the non-native feel is not
caused by the language. It is caused by three concrete absences, all
fixable in place:

- No Windows 11 backdrop material. `mod.rs:574` `set_blur_behind` uses
  legacy `DwmEnableBlurBehindWindow` (Aero blur), gated on
  `appearance.top_bar_blur`, which **defaults to `false`**
  (`crates/config/src/model.rs:112`). There is no
  `DWMWA_SYSTEMBACKDROP_TYPE` call anywhere in the tree.
- No system theme following. `design::color` hardcodes dark values and
  branches only on `high_contrast`. `theme.rs` reads
  `AppsUseLightTheme` but only to *write* the system theme from the
  Quick Settings chip; it never feeds our own palette.
- No system icon font or type ramp. `icons.rs` hand-draws geometry and
  `bar.rs:57` uses a bare `U+2699` for the settings gear;
  `util::bar_font` requests plain `Segoe UI`.

ADR-003 and ADR-004 are therefore unchanged. No new runtime dependency.

### 1.2 Out of scope

- Porting `bar.rs`, `quick_settings.rs`, or `session_menu.rs` from GDI
  to Direct2D. Considered and deliberately excluded: it rewrites the
  paint path of the most interaction-heavy surfaces and risks
  regressing hit-testing and the AppBar reservation landed in
  `4acc003`. Recorded as the follow-up in §8.
- The overview, the desktop dock, and the settings app.
- Any change to the control-center *functionality* shipped in ADR-009.

## 2. Surface inventory

The decisive constraint: **Mica does not composite on a
`WS_EX_LAYERED` window.**

| Surface | Created at | Extended style today | Backdrop |
|---|---|---|---|
| Top bar | `mod.rs:226` | `TOPMOST\|TOOLWINDOW\|NOACTIVATE`, not layered | Mica, directly |
| Calendar | `mod.rs:364` | `TOPMOST\|TOOLWINDOW`, not layered | Acrylic, directly |
| Quick Settings | `mod.rs:399` | adds `WS_EX_LAYERED` + `LWA_COLORKEY\|LWA_ALPHA` | Acrylic, after un-layering (§3.2) |
| Session menu | native `TrackPopupMenu` | — | Already native; untouched |

## 3. Material

### 3.1 Backdrop assignment

A new `design::material` module wraps three `DwmSetWindowAttribute`
calls. Values are named, not magic numbers.

| Surface | `DWMWA_SYSTEMBACKDROP_TYPE` (38) | Rationale |
|---|---|---|
| Top bar | `DWMSBT_MAINWINDOW` (2) — Mica | Mica is for long-lived chrome tied to the desktop. The bar never moves. |
| Calendar, Quick Settings | `DWMSBT_TRANSIENTWINDOW` (3) — Acrylic | What Windows 11 uses for its own flyouts. Transient, floats above content. |

Flyouts additionally get `DWMWA_WINDOW_CORNER_PREFERENCE` (33) =
`DWMWCP_ROUND` (2), so DWM draws the corners and the drop shadow. This
replaces both the hand-drawn `RoundRect` corner and the
`metrics::shadow()` software shadow on these two windows —
`metrics::shadow()` stays for the D2D surfaces that still composite
their own.

All three attributes are set once after `CreateWindowExW`, and
re-asserted on `WM_DWMCOMPOSITIONCHANGED`.

### 3.2 Un-layering Quick Settings

`QS_COLOR_KEY` currently does double duty: `LWA_COLORKEY` punches out
magenta to shape the rounded corners, and `LWA_ALPHA` drives the
open/close fade (`quick_settings.rs:1203`, `:1427`, `mod.rs:414`).

Both uses are removed:

- Corner shaping → `DWMWCP_ROUND` (§3.1). Strictly better: DWM
  antialiases, GDI's color key does not.
- The fade → **dropped.** Quick Settings appears and dismisses
  instantly, exactly as it already does when `reduced_motion` is on.

`WS_EX_LAYERED` comes off the creation flags, all three
`SetLayeredWindowAttributes` call sites go, and `QS_COLOR_KEY` and the
magenta-key painting it drove are deleted.

This is an accepted, deliberate regression of one animation in exchange
for the system material, real corners, and the system shadow. The
restoration path is the D2D port in §8, where opacity comes from D2D
and Mica composites underneath.

### 3.3 Fallback

`DWMWA_SYSTEMBACKDROP_TYPE` requires Windows 11 build 22621+. On older
builds `DwmSetWindowAttribute` returns a failure `HRESULT`, which is the
detection mechanism — no version gate, no registry probe. On failure the
existing `set_blur_behind` path runs instead and `appearance.top_bar_blur`
keeps its current meaning. When the backdrop *is* applied, `top_bar_blur`
is ignored — the system material supersedes it.

The config key keeps its name and default; no migration. One line of
explanatory copy on the settings app's Appearance page is the sole
exception to §1.2's "settings app out of scope", since leaving a now-inert
toggle unexplained is worse than the small diff.

`DWMWCP_ROUND` requires 22000+ and fails the same way; the fallback is
the current `RoundRect` corner, so the code keeps the rounded-corner
paint behind that one branch.

## 4. Light / dark theme

### 4.1 State mirror

A `LIGHT_THEME: Cell<bool>` thread-local mirror in `state.rs`, with
`light_theme()` / `set_light_theme()`, following the existing
`HIGH_CONTRAST` and accent mirror pattern.

This is mandatory, not stylistic: tokens are read from inside paint,
which already sits inside a `STATE` borrow, and a nested
`STATE.with(borrow)` panics. That crash is a known recurring failure
mode in this project.

Populated at startup from `theme::apps_use_light_theme()` and refreshed
in the existing `WM_SETTINGCHANGE` handler — the one Task 2 added for
accent — when `lParam` is `ImmersiveColorSet`. `theme.rs` already
broadcasts exactly that string from its own toggle, so the Quick
Settings theme chip restyles the shell live with no extra wiring.

`apps_use_light_theme()` returns `None` on a fresh account that has
never touched personalization; that maps to dark, preserving today's
appearance.

### 4.2 Token branching

Every `design::color` getter becomes three-way, in this precedence:

```
high_contrast  →  light  →  dark
```

High contrast keeps winning, exactly as today, so
`appearance.high_contrast` behavior is unchanged.

| Token | Dark (today) | Light (new) |
|---|---|---|
| `surface_base` | `#1E1E1E` | `#F3F3F3` |
| `surface_raised` | `#262626` | `#FBFBFB` |
| `surface_overlay` | `#2E2E2E` | `#FFFFFF` |
| `text` | `#E8E8E8` | `#1A1A1A` |
| `text_muted` | `#9A9A9A` | `#5F5F5F` |
| `stroke` | `#3A3A3A` | `#E5E5E5` |
| `accent_text` | `#FFFFFF` | `#FFFFFF` |

Light values follow the WinUI common-resource ramp. `accent()` is
theme-independent — it is the live registry accent either way.

Under a backdrop the surface tokens are no longer the whole story: they
paint *over* Mica/Acrylic. Where text sits directly on the material it
keeps a solid backing plate, as the Phase 4 spec §8 required.

### 4.3 Per-window tint

Each surface gets `DWMWA_USE_IMMERSIVE_DARK_MODE` (20) set to
`!light_theme()` so DWM tints the backdrop and any system-drawn
non-client pixels to match. Re-applied on the same
`WM_SETTINGCHANGE`/`ImmersiveColorSet` path as the tokens, followed by
invalidating the bar and every open flyout.

## 5. Typography and icons

### 5.1 Type ramp

New `design::type_ramp` with the WinUI sizes, so surfaces stop passing
raw sizes. Values are **logical pixels at the 96-DPI reference**, matching
how `util::bar_font` already works — it passes `-scaled(12, dpi)` to
`CreateFontW`, a negative character height in logical device units, not
points:

```
CAPTION_PX  = 12     // workspace dots, tray labels
BODY_PX     = 14     // bar text, flyout rows
SUBTITLE_PX = 20     // flyout headers
```

Note this **changes the bar's text size**: `bar_font` is 12px today, and
`BODY_PX` is 14px, which is what Windows itself uses for body text. That
is the intended correction, but `bar.rs`'s own header comment warns every
96-DPI constant in that file was tuned against the original bar height, so
larger text can overflow tuned layouts. Smoke step 6 covers it; if a
layout breaks, the bar keeps `CAPTION_PX` and only the flyouts move to
`BODY_PX`, rather than retuning the whole bar in this pass.

`util::bar_font` requests `Segoe UI Variable Text` and falls back to
`Segoe UI`. Face availability is probed **once** at startup via
`EnumFontFamiliesExW` and stored in a thread-local mirror; never per
paint. `Segoe UI Variable` ships only on Windows 11, so the fallback is
a real path, not a theoretical one.

Existing per-surface font scaling (`bar_content_scale`, the DPI helpers)
is untouched — the ramp supplies the base size those multiply.

### 5.2 Fluent icon glyphs

`icons.rs` gains a `Segoe Fluent Icons` glyph table and keeps every
existing hand-drawn path as the fallback when that font is absent
(pre-Win11), selected by the same one-time probe as §5.1. The `Icon`
enum and all call sites keep their current shape; only the draw
implementation branches.

Glyphs to map: settings gear (replacing the bare `U+2699` at
`bar.rs:57`), wifi with its strength states, volume with its level and
muted states, battery with its charge and charging states, chevron,
bluetooth, airplane, bell.

**Every codepoint is verified against the installed font before use**,
by rendering the candidate table to a fixture and inspecting it —
`scripts/inspect-tray.ps1` is the pattern to follow. Codepoints are not
asserted from memory; the Segoe MDL2 and Segoe Fluent tables differ and
several glyph names collide across them. Battery in particular is a
contiguous run of charge-level glyphs, and the correct base must be
read off the font rather than assumed.

## 6. Consuming `flyout.rs`

`flyout.rs` ships complete and unit-tested with a module-level
`#![allow(dead_code)]` and a comment saying it is not yet consumed and
that the redesign consuming it should remove the allow. This spec is
that redesign.

Calendar and Quick Settings replace their ad-hoc show/hide with
`Flyout::open()` / `close()` / `tick()`. The phase machine drives
visibility and dismissal; `is_animating()` drives the existing 16ms
timer.

After §3.2 neither window has layered alpha, so `scale_opacity`'s
**opacity term is unused on these two surfaces**. The scale term stays
available for the D2D surfaces in §8. `scale_opacity` itself is not
changed or narrowed — it remains correct for its eventual consumers —
but the `#![allow(dead_code)]` comes off the module, and if any
individual item is still genuinely unconsumed it gets a targeted
`#[allow]` with a reason rather than a module-wide blanket.

The shared lifecycle also replaces Quick Settings' own
`INTERACTION.motion` bookkeeping, so there is one flyout state machine
rather than two.

## 7. Testing

**Unit (pure, no Windows state):**

- Token branching precedence: high contrast wins over light; light over
  dark; each of the seven tokens returns its table value in all three
  modes. Factored as `token_with(hc, light)` pure helpers so the
  thread-local mirrors are not needed in tests, matching how
  `motion::effective_ms_with` is already split.
- `apps_use_light_theme() == None` maps to dark.
- Font/glyph fallback: with the Fluent face absent, the glyph table
  resolves to the hand-drawn path for every `Icon` variant — so no icon
  can silently render as a missing-glyph box.
- Backdrop failure maps to the `set_blur_behind` fallback.
- Existing `flyout.rs` phase tests continue to pass unchanged; they are
  the regression net for §6.

**Fixture:** extend the existing
`cargo test -p groveshell-ui render_control_center_previews -- --ignored`
GDI export to emit a light-theme pass alongside dark, for side-by-side
diffing.

**Live smoke** (per project memory: GDI capture misses these overlays,
so this is a Win+Shift+S clipboard snip, saved and inspected — and the
watchdog must be stopped before any `cargo build`, or it respawns and
locks `groveshell-ui.exe`):

1. Bar shows Mica over a busy wallpaper; text stays legible.
2. Flyouts show Acrylic with DWM-drawn round corners and shadow.
3. Quick Settings opens/dismisses instantly with no magenta fringe —
   the color-key regression to watch for.
4. Flipping Windows' Dark mode toggle restyles bar and flyouts live,
   without restart, including via the Quick Settings theme chip.
5. `high_contrast: true` still overrides both themes.
6. 100% / 150% / 200% DPI: glyphs and type scale with the bar-height
   slider, per `bar.rs`'s `bar_content_scale` note. Specifically confirm
   the 14px body (§5.1) still fits the default 32px bar without clipping
   the status pill or the clock.

## 8. Risks and follow-ups

| Risk | Mitigation |
|---|---|
| Mica hurts contrast over busy wallpaper | Solid backing plate behind text, per Phase 4 §8. Verified in smoke step 1. |
| Un-layering QS leaves magenta color-key artifacts | Delete the key and its paint together, not separately. Smoke step 3 targets exactly this. |
| Losing the QS fade feels abrupt | Accepted trade (§3.2). DWM's own flyout shadow softens the appearance. Restored by the §8 D2D port. |
| Backdrop unsupported on older Win11/Win10 | `HRESULT` failure drives the `set_blur_behind` fallback; no version gate. |
| Fluent Icons / Segoe UI Variable absent | One-time probe, hand-drawn and `Segoe UI` fallbacks retained and unit-tested. |
| Wrong glyph codepoints | Verified against the installed font before use, never from memory (§5.2). |
| `STATE` re-entrancy panic | The new theme read goes through a thread-local `Cell` mirror, never a nested `STATE` borrow. |

**Follow-ups, explicitly not in this spec:**

- Port `bar.rs` / `quick_settings.rs` / `session_menu.rs` to Direct2D on
  the existing `GpuSurface`. Restores the QS fade and gives antialiased
  geometry at fractional DPI. The natural successor to this pass.
- A light-theme pass over the overview and desktop dock, which this spec
  leaves dark.
- `DWMSBT_TABBEDWINDOW` (Mica Alt) as a bar option, once there is a real
  preference to express.
