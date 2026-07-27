# ADR-001: Do not replace DWM

## Status
Accepted

## Decision
GroveShell remains a user-mode shell and controller above the Windows
compositor. It does not replace, hook, or attempt to substitute for
`dwm.exe`.

## Rationale
DWM is deeply integrated with the Windows graphics stack, security model,
and driver ecosystem. Replacing it is unsupported, high-risk, and
unnecessary for the product goal of an overview-first workspace shell.
GroveShell observes and controls top-level HWNDs and renders its own
surfaces via Direct3D/DirectComposition on top of DWM's compositing,
per `docs/PROJECT_PLAN.md` §5.1.
