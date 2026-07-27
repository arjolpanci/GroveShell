# ADR-004: DirectComposition shell UI

## Status
Accepted

## Decision
Latency-critical shell surfaces (overview, dock, top bar, once built in
Phase 4-5) use native Direct3D 11 + DirectComposition rather than a web
runtime (Tauri/WebView) or WinUI 3.

## Rationale
This gives direct control over transparent surfaces, animation timing,
DPI, input regions, and live previews without making the critical shell
UI dependent on a web runtime. WinUI 3 remains an option for the
lower-frequency settings surface. See `docs/PROJECT_PLAN.md` §5.4.
