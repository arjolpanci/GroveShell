# ADR-010: One painter, two canvas backends

## Status

Accepted (2026-10-08).

## Context

ADR-004 puts every shell surface on DirectComposition so it can be
translucent, while keeping GDI as the fallback where the GPU path is
unavailable. That means each surface has to be drawable two ways.

The top bar took the obvious route: a second painter (`bar_gpu`) beside
the GDI one, sharing the layout helpers and hit-testing so the two cannot
disagree about *where* anything is. At roughly sixty lines of drawing that
is fine.

Quick Settings is about 570 lines across five pages — chips, split-button
arrows, a draggable volume slider, a scrolling network list, focus rings.
Two copies of that would have drifted apart the first time anyone touched
a page, and the drift would be silent: both versions compile, and only one
of them is on screen for any given user.

## Decision

Surfaces above trivial drawing complexity are written **once** against a
`Canvas` trait (`apps/ui/src/imp/canvas.rs`) with two implementations:

- `GdiCanvas` — draws to an `HDC`. Opaque by construction; its
  `fill_round_rect_alpha` ignores alpha, because GDI cannot express it.
- `D2DCanvas` — draws to an `ID2D1DeviceContext` backed by a
  DirectComposition surface, and can be translucent.

The caller picks the backend; the drawing code does not know which it got.

The trait is deliberately **stateful** — a current text color and font
size, set and then consumed by later `text` calls — mirroring the GDI code
it replaced. This was a conversion decision, not an aesthetic one: it kept
the port mechanical and compiler-checked rather than a hand rewrite of
several hundred call sites, and the compiler enumerated every remaining
raw GDI call once the signatures changed.

Coordinates are window-client pixels, already DPI-scaled, which are the
same values the layout functions and hit-testing use.

## Consequences

- One source of truth for what a panel looks like. A visual change lands
  on both backends or neither.
- The GDI backend's appearance is unchanged from before the port, since
  alpha is the only operation it drops.
- The trait is intentionally small: the operations the shell's panels
  actually use, not a general 2D API. Anything added has to be
  implementable on both backends — which is the point, because that
  constraint is what stops the two paths diverging.
- `D2DCanvas::icon` draws Fluent glyphs rather than decoding the bundled
  PNGs, so it is only valid where that face exists. That is checked once
  via `bar_gpu::available`, and is the same condition that gates the
  composition path.
- The bar keeps its separate painter. Folding it into `Canvas` would be
  tidy but buys little: its drawing is small, and it needs per-call font
  switching between the UI face and the icon face that the stateful
  interface would make clumsier than the duplication it saves.
