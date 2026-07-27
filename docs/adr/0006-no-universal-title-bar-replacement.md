# ADR-006: No universal title-bar replacement

## Status
Accepted

## Decision
Foreign application window frames remain native by default. GroveShell does
not attempt to globally restyle or shrink third-party title bars.

## Rationale
Title-bar customization APIs apply to the application that owns the
window; for foreign windows, DWM attribute and style changes are limited
and app-dependent. The default policy is `native`, with experimental,
explicitly opted-in per-app rules (`dwm-appearance`, `borderless-tested`)
for cases that have been tested. See `docs/PROJECT_PLAN.md` §9.2.
