# ADR-004: DirectComposition shell UI

## Status

Accepted. Rewritten 2026-10-08 — the original decision stands, but its
scope widened from "latency-critical surfaces" to "every shell surface",
and the reason changed. See *History*.

## Decision

Every shell surface — top bar, Quick Settings, calendar, dock, overview —
renders through Direct3D 11 + Direct2D on a DirectComposition surface,
not through a web runtime (Tauri/WebView) and not through WinUI 3.

GDI remains as a per-surface fallback for machines where the GPU path is
unavailable, reached automatically and never as a preference.

## Rationale

### Why not a web runtime or WinUI 3

Unchanged from the original decision. These surfaces need always-topmost,
no-activate windows with custom input regions, arbitrarily anchored
flyouts, AppBar work-area reservation, and live window previews. WinUI 3's
window model fights all of those, and adopting it would mean either a
second process per surface with IPC, an in-process CLR host, or rewriting
`apps/ui` in C#.

A later investigation (2026-10-08) confirmed this is not a capability
trade: WinUI 3 gets Mica by rendering through DirectComposition with no
redirection bitmap, which is exactly the mechanism used here. The
capability belongs to the compositor, not the language.

### Why DirectComposition everywhere, not just where latency matters

The original decision scoped this to "latency-critical" surfaces, leaving
the bar and panels on GDI. That turned out to be the wrong axis. The
deciding property is not frame rate, it is **alpha**:

- GDI has no alpha channel. Every pixel it writes is opaque, so a DWM
  backdrop (Mica, Acrylic) behind a GDI-painted window can never reach the
  screen. Verified by experiment: registering the bar's class with a null
  background brush and painting nothing left the background opaque and put
  black boxes behind the text, from the uninitialized redirection surface.
- A DirectComposition surface is premultiplied-alpha. Clearing it to
  alpha 0 lets the system material *be* the surface's background.

So translucency — the thing that makes these surfaces read as native
Windows 11 — is only reachable on the composition path. That applies to a
32px bar as much as to the overview.

Two further mechanisms are required and easy to miss:

- The window must be created with `WS_EX_NOREDIRECTIONBITMAP`, or its
  opaque GDI redirection bitmap is composited over the material regardless
  of the backdrop attribute.
- The surface must be *cleared*, not filled. A painted card covers the
  material just as effectively as a redirection bitmap does.

## Consequences

- **A window without a redirection bitmap cannot be painted by GDI at
  all.** A surface that fails to materialize would be invisible, not
  merely opaque. Every creation site therefore gates the style on
  `gpu::is_enabled()` and then re-checks the per-window surface, rebuilding
  the window opaque on failure. This is not defensive padding: the
  overview has been hitting `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`
  per-window since 2026-07-30.
- **Two painters per surface is not acceptable above a certain size.** The
  bar carries a second painter (`bar_gpu`) because its drawing is small
  and layout is shared. Quick Settings instead draws once against a
  `Canvas` trait with GDI and Direct2D backends — see ADR-010.
- **DWM backdrops apply to a whole window, not a region.** A surface whose
  visible panel is smaller than its window (the dock keeps headroom for
  its magnification wave) cannot use the system material without painting
  a rectangle around itself; it paints a translucent fill instead, which
  is see-through but not blurred.
- A GDI fallback path still exists everywhere and must keep working.

## History

The original version of this ADR (Phase 4) decided DirectComposition for
"latency-critical shell surfaces (overview, dock, top bar, once built in
Phase 4-5)" and recorded WinUI 3 as "an option for the lower-frequency
settings surface". In practice the bar and panels shipped on GDI, and the
Windows 11 material work in 2026-10 found that this, not the language or
the toolkit, was what kept them from looking native. The decision was
widened rather than reversed.

See `docs/superpowers/specs/2026-10-08-win11-native-shell.md`
for the measurements behind this.
