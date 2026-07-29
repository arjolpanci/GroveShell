# DirectComposition rendering foundation (Phase 1)

## Problem

The Activities overview's open/close animation is low-FPS and not fluid.
The root cause isn't the animation math — it's that every window in
GroveShell (bar, dock, overview, calendar, Quick Settings) is painted with
plain GDI (`CreateCompatibleDC`/`BitBlt`/`StretchBlt`), fully software
rendered, redrawn from scratch on a 16ms `WM_TIMER`, with no GPU compositor
involved at all. That ceiling has to be replaced before any "shared
element, spatial continuity" animation (the user's actual ask, an
iOS-app-launch-style zoom from icon to full view) can look and perform the
way it's supposed to.

This is a large enough change that it ships in stages. This document
covers only the first stage: proving out a DirectComposition/Direct2D
rendering pipeline on one simple window (the calendar flyout), with a
GDI fallback if GPU setup fails. Later phases port the bar, the dock, and
the Activities overview; a further phase after that builds the actual
shared-element open/close transition on top of the overview once it's
running on this pipeline. None of that is designed here.

## Scope

- Convert `apps/ui/src/imp/calendar.rs`'s rendering to DirectComposition +
  Direct2D.
- Build the reusable core infrastructure (D3D11 device, DXGI device,
  `IDCompositionDevice`, per-window target/visual/surface setup, a thin
  Direct2D drawing wrapper) in a new module, sized for calendar's actual
  needs today — not a general engine speculatively built ahead of the
  windows that will eventually use it.
- A GDI fallback: if device/target creation fails, the calendar keeps
  using its current `paint_calendar` GDI implementation, completely
  unchanged.

Out of scope (deliberately, for a later phase): the bar, the dock, the
Activities overview, Quick Settings, and any actual animation work.

## Design

### 1. Core infrastructure (new module, `apps/ui/src/imp/gpu.rs`)

**Startup, once, before any window is created:**

1. `D3D11CreateDevice` with `D3D11_CREATE_DEVICE_BGRA_SUPPORT` (required
   for Direct2D interop).
2. Wrap the resulting `ID3D11Device` as an `IDXGIDevice`.
3. Create one process-wide `IDCompositionDevice` from that DXGI device.

If any step fails, GPU rendering is unavailable for the *entire* process,
decided exactly once, right here — never retried, never re-evaluated per
window or per calendar-open. A global (e.g. `OnceLock<bool>`, or a plain
field set once during `main()`) records the outcome; every other GPU-path
call site checks it and falls back to the existing GDI code path with
zero GPU-specific behavior if it's `false`.

**Per-window setup (called once, when the calendar window is created):**

- `IDCompositionTarget` bound to the calendar's `HWND`.
- A root `IDCompositionVisual` covering the full client area.
- One `IDCompositionSurface`, sized to the calendar's current
  `CAL_WIDTH`×`CAL_HEIGHT` (scaled for DPI same as today). The calendar is
  static content redrawn only when it's about to be shown — not
  continuously animated — so a single composition surface Direct2D draws
  into via `BeginDraw`/`EndDraw` is enough; no DXGI swap chain needed.
  (A swap chain becomes necessary once a later phase reaches genuinely
  animated content like the overview.)

If per-window setup fails even though the process-wide device succeeded,
that's treated identically to the process-wide failure — GPU path off, GDI
fallback used. (This shouldn't happen if step 1-3 already succeeded, but
costs nothing to handle uniformly.)

### 2. Minimal Direct2D drawing wrapper

Only what `paint_calendar` actually uses today: fill a rect with a solid
color, draw a string centered/vcentered in a rect (matching
`draw_text_in`'s current `DT_CENTER|DT_VCENTER|DT_SINGLELINE` behavior).
No general text layout engine, no gradients, no effects — calendar doesn't
use any of that today and won't gain new visual features in this phase.

### 3. Porting `paint_calendar`

Once a `IDCompositionTarget`+root visual is bound to `calendar_hwnd`, that
visual tree becomes the window's actual displayed content — the window's
own GDI-painted surface (including whatever `WM_ERASEBKGND`'s default
class-background-brush erase would have produced) no longer contributes
to what's on screen. Today, `paint_calendar` never draws its own
background — Windows fills it automatically via the `GroveShellCalendar`
window class's brush (`COLORREF(0x00303030)`, a solid dark gray) before
`WM_PAINT` runs. The GPU-path redraw function must draw that same fill
itself, explicitly, as the first thing it does — otherwise the flyout's
background silently goes missing (transparent/whatever garbage was
previously in the surface) once GDI's automatic erase stops mattering.

Everything else in `paint_calendar` — the month/year header, day-of-week
labels, the day grid with today highlighted, the notifications section —
ports mechanically: same layout math (`CAL_PADDING`, `cell_w`,
`CAL_CELL_HEIGHT`, etc., all untouched), each GDI call swapped for its
Direct2D-wrapper equivalent at the same coordinates.

### 4. When redraws happen

DirectComposition doesn't need `WM_PAINT` to keep displaying already-drawn
content — once drawn and `IDCompositionDevice::Commit()` is called, the
compositor keeps it on screen with zero app-side redraw cost. So:

- `WM_PAINT`'s `Role::Calendar` arm, when the GPU path is active, becomes
  a no-op ack (`BeginPaint`/`EndPaint`, nothing drawn) — the compositor is
  already showing the last commit.
- The actual redraw trigger becomes "content is about to matter again":
  `toggle_calendar`'s show path (today: one `InvalidateRect` call right
  before `ShowWindow(SW_SHOW)`) becomes one explicit redraw-and-commit
  call instead.
- If the GPU path is unavailable (startup fallback engaged), `WM_PAINT`
  keeps calling `paint_calendar` exactly as it does today, and
  `toggle_calendar`'s `InvalidateRect` call stays as-is — the fallback
  path is bit-for-bit the current code, untouched.
- Resize/DPI change (if the calendar's size ever changes at runtime,
  which it doesn't dynamically today, but the class of situation
  matters for later phases): the composition surface would need
  recreating at the new size before the next redraw. Not exercised by
  this phase's actual behavior, but the redraw function is written to
  recreate the surface if the requested size doesn't match the surface's
  current size, so it's not a landmine for later.

### Testing

- **Unit-tested**: the calendar's existing pure date-grid math
  (`is_leap_year`, `days_in_month`, `month_name`) has no test coverage
  today — this phase adds it, since the file is already being touched
  throughout and the functions are trivially pure.
- **Manual verification only** (consistent with this codebase's
  established convention for Win32-integration work with no automated
  coverage): the calendar flyout must look pixel-equivalent to today,
  open/close via the same `toggle_calendar`/`hide_calendar` triggers with
  no visual regression, and — the actual point of this phase — show near-
  zero CPU usage while open and idle (proving the compositor, not an
  app-side redraw loop, is doing the work).
- **Fallback path verification**: a real D3D11 device-creation failure
  can't be reliably forced on ordinary dev hardware. The implementer
  deliberately exercises the fallback branch (e.g. temporarily forcing
  the startup GPU-decision flag to `false`) to confirm the GDI code path
  still works standalone, rather than trusting it by inspection alone.
