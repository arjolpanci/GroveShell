# Windows 11 Native Material — Mica, System Theme, Fluent Typography

**Date:** 2026-10-08
**Status:** Implemented. Revised against what the implementation proved —
see §9 for the findings that contradicted this spec's own assumptions.
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
- No system icon font or type ramp. `icons.rs` embeds bundled PNG assets
  in a non-Windows (Lucide) drawing style — *not* hand-drawn geometry, as
  an earlier draft of this spec claimed — and `bar.rs:57` uses a bare
  `U+2699` for the settings gear; `util::bar_font` requests plain
  `Segoe UI`.

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
- The fade → **replaced by a dropdown reveal** (§3.4). No opacity
  animation is needed, so nothing is lost.

`WS_EX_LAYERED` comes off the creation flags, all three
`SetLayeredWindowAttributes` call sites go, and `QS_COLOR_KEY` and the
magenta-key painting it drove are deleted.

### 3.4 Dropdown reveal

Both flyouts open by unrolling downward from the bar's bottom edge: the
window's **top stays pinned** and its **height animates** `0 → full`
over `motion::BASE_MS`, eased by the existing `ease_out_cubic`. Closing
runs it in reverse. One `SetWindowPos` per frame on the existing 16ms
timer; no per-pixel alpha, so `WS_EX_LAYERED` is not needed and the
backdrop composites normally.

The key property that makes this nearly free: **content is laid out
against fixed height constants, not the client rect.** Quick Settings
derives positions from `QS_HEIGHT` (`quick_settings.rs:115`,
`card_bottom = margin + scaled(QS_HEIGHT, dpi)`); the calendar from
`CAL_HEIGHT` (`calendar.rs:22`, `:73`). So content stays put while the
window grows over it, and the overflow is clipped:

- **Quick Settings (GDI):** already double-buffers through a compatible
  DC sized from `GetClientRect` (`quick_settings.rs:255`–`266`). During
  the reveal that back buffer is simply shorter, so painting content at
  full-height coordinates clips for free. The paint code is unchanged.
- **Calendar (Direct2D/DirectComposition):** its `GpuSurface` is created
  once at `CAL_WIDTH, CAL_HEIGHT` (`mod.rs:379`) and **is not resized** —
  `gpu.rs` has no resize entry point and does not need one. The window
  bounds clip the composition. The surface staying larger than the
  window is fine and intentional.

Because the window grows with the content, the backdrop and the
DWM-drawn round corners grow with it too — the material never appears as
an empty box waiting to be filled, which is what a translate-based slide
or a full-size-then-clip approach would produce.

`reduced_motion` collapses the reveal to an instant full-height show, via
the same `effective_ms` → 0 path every other transition already uses.

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
larger text can overflow tuned layouts. Smoke step 7 covers it; if a
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

The reveal in §3.4 is driven by `tick()`'s eased progress directly, so
`flyout.rs` gains one small method:

```rust
/// The window height to show at eased progress `p` for a flyout that
/// unrolls to `full`. Hidden is 0; Open is `full`.
pub(crate) fn reveal_extent(&self, p: f32, full: i32) -> i32
```

`scale_opacity` is **not** changed or narrowed — it stays correct for the
scale-and-fade surfaces that come later — but neither of its terms is
used by these two flyouts now. The `#![allow(dead_code)]` comes off the
module; anything still genuinely unconsumed gets a targeted `#[allow]`
with a stated reason rather than a module-wide blanket.

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
- `reveal_extent`: 0 at `p=0`, `full` at `p=1`, monotonic between, and
  never exceeding `full` or going negative for out-of-range `p` (matching
  how the easing functions already clamp).
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
3. Both flyouts unroll downward from the bar edge and roll back up on
   dismiss, with the backdrop and round corners growing with the window
   rather than popping in at full size. No tearing or flicker from the
   per-frame resize, and no stuck half-open frame if dismissed mid-open.
   `reduced_motion` shows them instantly at full height.
4. Quick Settings opens/dismisses with no magenta fringe —
   the color-key regression to watch for.
5. Flipping Windows' Dark mode toggle restyles bar and flyouts live,
   without restart, including via the Quick Settings theme chip.
6. `high_contrast: true` still overrides both themes.
7. 100% / 150% / 200% DPI: glyphs and type scale with the bar-height
   slider, per `bar.rs`'s `bar_content_scale` note. Specifically confirm
   the 14px body (§5.1) still fits the default 32px bar without clipping
   the status pill or the clock.

## 8. Risks and follow-ups

| Risk | Mitigation |
|---|---|
| Mica hurts contrast over busy wallpaper | Solid backing plate behind text, per Phase 4 §8. Verified in smoke step 1. |
| Un-layering QS leaves magenta color-key artifacts | Delete the key and its paint together, not separately. Smoke step 4 targets exactly this. |
| Per-frame `SetWindowPos` during the reveal tears or flickers | 16ms on the existing timer, the same cadence the fade already ran at; content is double-buffered (QS) or DComp-composed (calendar), so neither repaints unbuffered. Smoke step 3 is the gate. If the calendar tears, it falls back to `IDCompositionVisual::SetClip` via the existing `gpu::visual()` accessor, with the window kept at full size. |
| Backdrop may not composite under a DComp-rendered window (calendar) | **Unverified by code reading** — Mica shows through where window content is transparent, and `gpu::clear_transparent` exists, but the DWM/DComp interaction needs a live check. First thing to confirm in smoke step 2. If it does not composite, the calendar keeps a solid `surface_raised` card and only the bar and QS take the material; §4.2's token values make that degradation coherent rather than broken. |
| Backdrop unsupported on older Win11/Win10 | `HRESULT` failure drives the `set_blur_behind` fallback; no version gate. |
| Fluent Icons / Segoe UI Variable absent | One-time probe, hand-drawn and `Segoe UI` fallbacks retained and unit-tested. |
| Wrong glyph codepoints | Verified against the installed font before use, never from memory (§5.2). |
| `STATE` re-entrancy panic | The new theme read goes through a thread-local `Cell` mirror, never a nested `STATE` borrow. |

**Follow-ups, explicitly not in this spec:**

- Port `bar.rs` / `quick_settings.rs` / `session_menu.rs` to Direct2D on
  the existing `GpuSurface`, for antialiased geometry at fractional DPI.
  No longer needed to recover any animation — §3.4's reveal supersedes
  the fade — so this is now a pure rendering-quality follow-up, and
  correspondingly lower priority than when it was a regression fix.
- A light-theme pass over the overview and desktop dock, which this spec
  leaves dark.
- `DWMSBT_TABBEDWINDOW` (Mica Alt) as a bar option, once there is a real
  preference to express.


## 9. Findings from the implementation

Recorded because three of them contradict assumptions made above, and a
later reader should not re-derive them.

### 9.1 The backdrop material is not visible on GDI surfaces

**Mica and Acrylic cannot show through a GDI-painted window.** GDI has no
alpha channel, so every pixel it paints is opaque and DWM's material
never reaches the screen.

Tested directly rather than assumed: the bar's window class was
re-registered with a null background brush so nothing would paint over
the backdrop. The background stayed opaque and the text picked up black
boxes from the uninitialized redirection surface. There is no arrangement
of class brush and `WM_ERASEBKGND` that yields transparency here, because
the double-buffered `BitBlt` path (Quick Settings) and the direct
`BeginPaint` path (bar) both write opaque pixels.

The `DWMWA_SYSTEMBACKDROP_TYPE` calls are kept: they cost nothing, they
are correct, and they will start showing the moment a surface moves to
Direct2D. But the visible wins from this pass are the DWM corners and
shadow, the system theme, and the Fluent type and icons — not
translucency. §1.2's follow-up port is what unlocks the material, which
raises its value relative to how §8 first framed it.

### 9.2 Round corners and the reveal both work as designed

`DWMWCP_ROUND` applies cleanly to the un-layered flyouts, drawing an
antialiased curve plus DWM's own hairline border and drop shadow —
visibly better than the color-key corners it replaced. Verified by
zooming into the rendered corner, not inferred from the call succeeding.

The dropdown reveal works for both flyouts with no surface resizing, as
§3.4 predicted: the window's height animates while content stays laid out
against the fixed constants, and the window bounds clip the overflow.

### 9.3 The bar could never have followed a theme

Independent of this spec's goals, the bar's background came from its
window class brush — a solid color fixed at `RegisterClass` time. No
theme change could ever have restyled it. The light palette would have
landed on every surface except the most visible one. The bar now paints
its own background from `surface_base()`, which also replaces the
class-brush erase as the thing that clears the previous hover highlight.

### 9.4 Icon mapping is verified, not recalled

Every codepoint was checked with `GetGlyphIndicesW` and then rendered and
inspected. That second step caught real errors: `E75E` exists and sounds
like "bluetooth off" but draws a device pill, and the battery run is
`E850` (empty) through `E859` (full) with charging variants living
separately at `E83E`/`E83F`. Segoe Fluent Icons has **no**
bluetooth-disabled glyph, so that one `Icon` variant keeps its PNG and a
test pins the exception so it cannot be "fixed" into a wrong glyph later.

### 9.5 Not done

- The `top_bar_blur` settings-app copy change described in §3.3.
- Light-theme values are verified on the bar but not inspected across
  every Quick Settings page and control state.
- The light-theme preview fixture pass described in §7.
