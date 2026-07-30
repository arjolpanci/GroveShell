# Activities overview: DirectComposition per-card visual port

## Problem

The calendar flyout (Phase 1 of this animation-fluidity work) proved the
DirectComposition/Direct2D pipeline: GPU-composited, near-zero idle CPU,
same visuals as the old GDI path. The Activities overview is where the
user's actual complaint lives — low-FPS, non-fluid open/close and
carousel motion — and it's a much bigger surface: wallpaper, per-card
window thumbnails (captured bitmaps, not live thumbnails), drag
interactions, search, dock icons, and continuous animation currently
driven by fully redrawing the entire scene via GDI on a 16ms app timer.

This document designs porting the overview directly to a per-card
DirectComposition visual architecture (skipping an intermediate
flat-surface-only step, per direct request — avoids doing the
layout/thumbnail porting work twice).

## Scope

**In scope:**
- Per-monitor overview visual tree: one root visual (wallpaper/dock/search
  chrome) + one child visual per workspace card (frame/shadow/thumbnails/
  label).
- Carousel slide + focused-card zoom, and the whole-overview open/close
  alpha fade, become compositor transform/opacity updates instead of
  full-scene GDI redraws.
- Window thumbnails (today's `PrintWindow`-captured `HBITMAP`, unchanged
  capture mechanism) bridged to Direct2D via WIC so they can be drawn
  into a card's surface.
- GDI fallback per monitor, same one-time-decision pattern as the
  calendar.

**Explicitly out of scope for this phase** (stay on today's model,
unchanged): window drag-pop animation, hover glow, search box rendering
and interaction. These remain occasional, event-triggered redraws of the
relevant card's own (now much smaller) surface — not the source of the
original complaint, and not worth the added risk here.

**Also out of scope:** real `IDCompositionAnimation` compositor timelines.
Chosen instead: keep the app's existing 16ms `ANIM_TIMER` and all its
state machines (`CarouselAnim`, `OverviewMode::Opening`/`Closing`,
`WindowPopAnim`) completely structurally unchanged — only what each tick
*does* changes. This is a much smaller, lower-risk change that still fixes
the actual bottleneck (CPU-bound GDI redraw of the whole scene, every
tick) — real compositor timelines remain a well-scoped future refinement
if this isn't smooth enough in practice.

**Correction to prior assumption:** the overview's own open/close is a
plain whole-window alpha fade (`SetLayeredWindowAttributes`) today, not a
scale/zoom — `OverviewMode::Opening`/`Closing`'s doc comments confirm the
cards/thumbnails are already at final layout throughout; only alpha
animates. The "zoom" language in `README.md` refers to the focused-card-
larger effect during carousel scroll and the window drag-pop animation,
neither of which changes in this phase.

## Design

### 1. Visual tree, per monitor's `OverviewInstance`

A new field, e.g. `gpu: Option<OverviewGpuState>`, alongside the other
per-monitor overview state. `OverviewGpuState` holds:

- The root `IDCompositionTarget`/`IDCompositionVisual2` bound to the
  overview `HWND`, with one shared `IDCompositionSurface` for wallpaper +
  dock icons + search chrome — drawn (redrawn) only when that content
  actually changes (dock app list changes, search query changes, wallpaper
  changes), same "redraw on content change" model as the calendar.
- A `Vec` of per-card entries, one per current `CardAnim` — each with its
  own child `IDCompositionVisual2` (parented under the root) and its own
  `IDCompositionSurface`, drawn with that card's frame/shadow/thumbnails/
  label at the card's *natural* size (unscrolled, unzoomed — carousel
  position/zoom is a transform applied on top, never baked into the
  card's own drawn content).

Rebuilt whenever `build_carousel_pages`/`rebuild_open_overview_pages` runs
(workspace count changes, monitor hotplug, etc.) — same trigger points as
today, extended to also recreate the per-card visual list to match the
new `CardAnim` count.

`IDCompositionDevice2`/`IDCompositionVisual2` (rather than Phase 1's
plain `IDCompositionDevice`/`IDCompositionVisual`) are needed here, since
opacity control (`SetOpacity2`) requires the `2` interface tier — a
one-line upgrade to `gpu.rs`'s existing device-creation call, with no
behavior change for the calendar's existing usage (`IDCompositionVisual2`
still satisfies everywhere a plain `IDCompositionVisual` is expected).

### 2. What each animation tick does now

`on_animation_tick`'s existing logic (advance `CarouselAnim`/`WindowPopAnim`/
`OverviewMode` state via elapsed time and `ease_out`, exactly as today) is
unchanged. What changes is what happens with the result:

- **Carousel position/zoom** (`carousel_offset`, whether from an active
  drag or a `CarouselAnim` settle): instead of calling `repaint_overview`
  (a full GDI redraw of the whole scene), compute each card's current
  `displayed_rect`/`zoom_rect` (same pure functions as today) and call
  `SetTransform` on that card's visual — translate + scale, no redraw of
  the card's own bitmap content at all.
- **Overview open/close alpha** (`OverviewMode::Opening`/`Closing`): instead
  of `SetLayeredWindowAttributes` on the whole `HWND`, call
  `SetOpacity2` on the root visual with the same eased alpha value
  already being computed today.
- **Card/thumbnail content** (frame, shadow, thumbnails, label) is *not*
  touched by the per-tick path at all — it's drawn once when the card's
  visual is created, and redrawn only by the specific existing call sites
  that already change a card's actual content: page rebuilds, a thumbnail
  refresh, hover-glow state changes, drag-state changes. Each such call
  site keeps doing what it does today, just targeting that one card's
  small surface instead of the whole window.

Hit-testing (`on_overview_click`, `on_overview_hover`, etc.) is completely
unchanged — it still reads `cards[].rect`/`thumbs[].rect` combined with
the current `carousel_offset` exactly as today, since the app still owns
and computes that state every tick (this is exactly why Option A doesn't
need to duplicate any compositor-side interpolation math for hit-testing).

### 3. Window thumbnails via WIC bridge

Capture is unchanged (`capture_window_snapshot`'s `PrintWindow` call,
`slot_scaled_snapshot`'s GDI `StretchBlt`-based pre-scaling — both stay
exactly as they are, since they're already cheap one-time-per-change
operations, not a per-frame cost).

When a card's surface is (re)drawn, its thumbnail `HBITMAP`s are bridged
to Direct2D via `IWICImagingFactory::CreateBitmapFromHBITMAP` →
`ID2D1DeviceContext::CreateBitmapFromWicBitmap`, then drawn with
`DrawBitmap`. This bridging happens only when a card's content is
actually redrawn (not per animation tick), so its cost is in the same
category as the existing `StretchBlt` pre-scaling — infrequent, not a
hot path.

### 4. Fallback

Same pattern as the calendar, per monitor: if this specific overview
window's DirectComposition target/visual/surface setup fails (even though
the process-wide device succeeded), that monitor's overview keeps calling
today's GDI `paint_overview` entirely unchanged, and `on_animation_tick`
keeps calling `repaint_overview` (the existing full-redraw path) for that
monitor specifically — the GPU/GDI decision is per-overview-window, not
just process-wide, since a monitor's own overview surface could
individually fail to create even when the process-wide device didn't.

### Testing

Same convention as the calendar phase: pure geometry (`displayed_rect`,
`zoom_rect`, `ease_out`) already has no dependency on GDI or
DirectComposition and needs no new tests — porting the drawing backend
doesn't change what these functions compute. Live compositor/Direct2D
behavior — visual tree creation, transform updates, WIC bitmap bridging —
is manual-verification-only, consistent with every other Win32-integration
piece of this codebase. Manual verification for this phase specifically
should check: carousel drag/snap and overview open/close feel
meaningfully smoother and show near-zero idle CPU when not actively
animating; every card's thumbnails/frame/shadow/label still look correct;
hover glow, drag-pop, and search still work exactly as before (unchanged
code paths); and the per-monitor GDI fallback still works if forced.
