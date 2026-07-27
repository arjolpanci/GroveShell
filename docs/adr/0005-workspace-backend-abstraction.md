# ADR-005: Workspace backend abstraction

## Status
Accepted

## Decision
Workspace logic (Phase 3+) is implemented behind a `WorkspaceBackend` trait
with two interchangeable implementations: a `ManagedWorkspaceBackend`
(GroveShell owns hide/show) and an optional `NativeDesktopBackend` (delegates
to Windows virtual desktops).

## Rationale
The public `IVirtualDesktopManager` API is limited, and richer control
requires unstable, undocumented interfaces that can change between Windows
updates. Starting with the managed backend, built on stable window APIs,
keeps the MVP deterministic and testable while leaving native-desktop
integration as a version-gated adapter rather than a core dependency. See
`docs/PROJECT_PLAN.md` §8.
