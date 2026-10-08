# Windows 11 Native Shell — Material, Theme, Typography, Live Previews

**Date:** 2026-10-08
**Status:** Implemented. This document was rewritten after the work and
describes what the shell actually does, not the design proposed before it.
Several of the original design's assumptions turned out to be wrong; §7
records them, because they are the part worth keeping.
**Supersedes:** the deferred material and motion items of
`docs/superpowers/specs/2026-08-11-phase-4-shell-ui-design-language.md`,
and the earlier `2026-10-08-win11-native-material-design.md`, which was
replaced by this file rather than patched.
**Relevant ADRs:** ADR-003 (Rust native core), ADR-004 (DirectComposition
shell UI, rewritten alongside this), ADR-010 (one painter, two canvas
backends).

## 1. Goal

Make the shell read as a native Windows 11 surface: the system backdrop
materials, the system light/dark theme, the system icon font and type
ramp, and live window previews in Activities.

### 1.1 Why not .NET / WinUI 3

The work was prompted by a request to rewrite these surfaces with .NET GUI
libraries. That was not done, and a later experiment showed it would not
have helped: WinUI 3 gets Mica by rendering through DirectComposition with
no redirection bitmap, which is exactly the mechanism used here. The
capability belongs to the compositor, not the language, and `apps/ui`
already talks to that compositor directly.

Adopting WinUI 3 would also mean giving up always-topmost no-activate
windows, custom input regions, AppBar reservation and arbitrary flyout
anchoring, or reimplementing them. ADR-004 covers this.

## 2. What shipped

| Surface | Renderer | Material | Notes |
|---|---|---|---|
| Top bar | Direct2D / DComp (`bar_gpu`) | **Mica** | GDI painter kept as fallback |
| Quick Settings | Direct2D via `Canvas` | **Acrylic**, card at 78% | One painter, two backends (ADR-010) |
| Calendar | Direct2D / DComp | **Acrylic** | Clears to alpha 0; the material *is* the background |
| Desktop dock | Direct2D / DComp | translucent fill, 72% | No DWM backdrop — see §4.3 |
| Session menu | native `TrackPopupMenu` | system | Already native; untouched |

Plus system light/dark theme following, Segoe Fluent Icons and the Segoe
UI Variable type ramp, and the dropdown reveal on both flyouts.

## 3. Material

Three DWM window attributes, wrapped in `design::material`:

| Attribute | Bar | Flyouts |
|---|---|---|
| `DWMWA_SYSTEMBACKDROP_TYPE` | `DWMSBT_MAINWINDOW` (Mica) | `DWMSBT_TRANSIENTWINDOW` (Acrylic) |
| `DWMWA_WINDOW_CORNER_PREFERENCE` | `DWMWCP_DONOTROUND` | `DWMWCP_ROUND` |
| `DWMWA_USE_IMMERSIVE_DARK_MODE` | `!light_theme()` | `!light_theme()` |

The bar is full-width and flush to the top edge, so letting DWM round it
would notch the screen corners; its rounded *bottom* corners come from the
window region it already had.

Three things must all be true for a backdrop to be visible, and missing
any one of them silently yields an opaque surface:

1. The window is created with `WS_EX_NOREDIRECTIONBITMAP`, or its opaque
   GDI redirection bitmap is composited over the material.
2. The surface is a DirectComposition surface drawn with Direct2D.
3. The paint **clears** to alpha 0 rather than filling a background. A
   painted card hides the material exactly as well as a redirection bitmap
   does.

### 3.1 Fallback, and why it is load-bearing

A window with no redirection bitmap cannot be painted by GDI at all, so a
surface that failed to get a composition surface would be *invisible*, not
merely opaque. Every creation site gates the style on `gpu::is_enabled()`
and then re-checks the per-window surface, destroying and rebuilding the
window opaque on failure.

This is not hypothetical. The overview has been failing per-window with
`DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED` since **2026-07-30** and
silently running its GDI path — a pre-existing bug, unrelated to this
work, still open (§8).

`DWMWA_SYSTEMBACKDROP_TYPE` needs Windows 11 22621+ and the corner
preference 22000+. On older builds `DwmSetWindowAttribute` fails, and that
failure *is* the feature detection: no version gate, no registry probe.

## 4. Theme, typography, icons

### 4.1 Light / dark

`design::color` tokens branch three ways, in this precedence:

```
high_contrast  →  light  →  dark
```

High contrast keeps winning, so `appearance.high_contrast` behaves exactly
as before. Each token splits into a pure `*_for(hc, light)` core and a
live getter, so the precedence is unit-testable without touching the
thread-local mirrors — the same split `motion::effective_ms_with` already
used.

A `LIGHT_THEME` thread-local `Cell` mirrors the system setting. This is
mandatory rather than stylistic: tokens are read from inside paint, which
already sits inside a `STATE` borrow, and a nested `STATE.with(borrow)`
panics — a recurring crash class in this project.

It refreshes on `WM_SETTINGCHANGE` with `ImmersiveColorSet`, which is also
what `theme::set_apps_use_light_theme` broadcasts, so the Quick Settings
theme chip restyles the shell live with no extra wiring.

Light values follow the WinUI ramp: `#F3F3F3` base, `#FBFBFB` raised,
`#FFFFFF` overlay, `#1A1A1A` text, `#5F5F5F` muted, `#E5E5E5` stroke.

### 4.2 Type and icons

`design::typography` holds the WinUI ramp in **logical pixels** (caption
12, body 14, subtitle 20) — not points: `CreateFontW` takes a negative
character height in logical units, which is what the bar always passed.
The UI face is `Segoe UI Variable Text`, falling back to `Segoe UI`.

Face availability is probed once and mirrored. `CreateFontW` never fails
for a missing face — it silently substitutes the closest match — so the
only reliable check is to select the font and read the face back with
`GetTextFaceW`.

Icons come from Segoe Fluent Icons, with the bundled PNGs kept as the
pre-Windows-11 fallback. Every codepoint was verified against the
installed font: checked for existence with `GetGlyphIndicesW`, then
rendered and inspected. That second step mattered — `E75E` exists and
sounds like "bluetooth off" but draws a device pill, and the battery run
is `E850` (empty) through `E859` (full), with charging variants living
separately at `E83E`/`E83F`. Segoe Fluent has **no** bluetooth-disabled
glyph, so that one variant keeps its PNG and a test pins the exception so
it cannot later be "fixed" into a wrong glyph.

### 4.3 Where the material cannot be used

A DWM backdrop applies to a window's whole rect. The desktop dock's window
is deliberately much larger than its visible panel — headroom for the
magnification wave — so the material would paint a large rectangle around
the dock. It paints a translucent rounded fill instead: see-through, but
not blurred, since only the system backdrop can blur what is behind a
window.

## 5. Motion

Both flyouts open by unrolling downward from the bar's edge: the window's
top stays pinned and its height animates `0 → full` over `motion::BASE_MS`,
eased by `ease_out_cubic`, on the existing 16ms timer.

This is cheap because content is laid out against fixed height constants
(`QS_HEIGHT`, `CAL_HEIGHT`) rather than the client rect, so it stays put
while the window grows over it and the overflow is clipped. Quick
Settings' existing double buffer clips it for free; the calendar's
Direct2D surface stays at full size and the window bounds clip the
composition, so `gpu.rs` needs no resize entry point.

Because the window grows with the content, the backdrop and the DWM round
corners grow with it — the material never appears as an empty box waiting
to be filled, which a translate-based slide would produce.
`reduced_motion` collapses the reveal to an instant full-height show.

## 6. Activities: tried and reverted

Live previews (`DwmRegisterThumbnail`) and a fly-out/fly-in animation
between each window's real rect and its grid slot were built and then
**reverted** (commit reverting `eee9d08` and `4646ef1`). Recorded because
the reasons are properties of the APIs, not bugs:

- **DWM thumbnails cannot be clipped and always composite above the
  destination window's own content.** They therefore covered the card
  chrome the overview paints — rounded preview corners, shadow, border.
  Live previews are only worth having if the chrome is drawn *into* the
  same composition tree, above them.
- **Interpolating a preview between its real window rect and its grid
  slot stretches between two different aspect ratios**, which reads as a
  window being resized oddly rather than moving. A fly animation needs to
  letterbox or crop rather than stretch.

Neither was asked for; the actual ask was frame rate (§8).

## 7. What the implementation disproved

Kept because each of these cost real time to establish.

**GDI can never show a DWM backdrop.** Tested by registering the bar's
class with a null background brush and painting nothing: the background
stayed opaque and the text picked up black boxes from the uninitialized
redirection surface. There is no brush / `WM_ERASEBKGND` combination that
yields transparency. This is what forced the Direct2D ports rather than a
tweak.

**But "the material cannot be shown" did not follow.** An earlier draft
concluded exactly that. The calendar was already on DirectComposition; it
was opaque only because the window kept its redirection bitmap and the
paint filled a card. Fixing both produced real Acrylic. Measured, panel
interior against three backdrops:

| behind the flyout | before | after |
|---|---|---|
| blue wallpaper `13,29,44` | `41,41,41` | `50,67,83` |
| grey band `46,46,46` | `41,41,41` | `48,48,49` |
| black `11,11,11` | `41,41,41` | `61,61,61` |

A constant interior means opaque; one that tracks its backdrop means
translucent. The same test on the finished bar: `32,32,32` with Mica,
`14,31,47` with `DWMSBT_NONE` — the raw wallpaper, straight through the
window.

**The bar could never have followed a theme.** Its background came from
its window class brush, a solid color fixed at `RegisterClass` time, so
the light palette would have landed on every surface except the most
visible one. Found by accident while testing the above.

**`icons.rs` does not hand-draw glyphs**, as an earlier draft claimed — it
embeds Lucide-style PNGs. That strengthened rather than weakened the case
for Fluent icons, since that style reads as distinctly non-Windows.

**Painting a surface's background twice hides the first one.** Quick
Settings filled the client *and* then the card on top; making only the
first translucent left the whole panel opaque and looked like the
translucency had failed entirely.

## 8. Known gaps

- **Activities renders on GDI, at roughly 15 FPS.** Two separate defects
  sit under this. The first is fixed: every `GpuSurface` created its own
  `IDCompositionTarget`, and a window can hold only one, so the
  overview's second surface always failed with
  `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED` and the whole renderer
  fell back — the warning in the logs since 2026-07-30. Targets and
  surfaces are now separate (`gpu::create_child_surface`).
  The second is open: with that unblocked, the composition tree builds,
  `BeginDraw` succeeds, `paint_backdrop` fills the root surface opaque
  and the fade drives opacity to 1.0, yet the window composites fully
  transparent — with and without `WS_EX_NOREDIRECTIONBITMAP`, on both
  monitors. `OVERVIEW_GPU_ENABLED` in `overview_gpu.rs` holds the
  renderer off until that is found; flipping it is the whole of the fix.
- Live previews have square corners (§6).
- `appearance.top_bar_blur` is superseded by the system material on
  supported builds; the settings app still presents it without saying so.
- Light-theme values are verified on the bar but not inspected across
  every Quick Settings page and control state.
