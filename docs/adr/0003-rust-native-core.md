# ADR-003: Rust native core

## Status
Accepted

## Decision
Long-running services and Win32 integration are implemented in Rust using
the `windows` crate, organized as a Cargo workspace.

## Rationale
Memory safety matters for a long-running shell that processes untrusted
window metadata and global input. The `windows` crate exposes both Win32
and WinRT APIs while preserving low-level HWND-oriented control, and Cargo
workspaces make process/library boundaries explicit. See
`docs/PROJECT_PLAN.md` §5.3.
