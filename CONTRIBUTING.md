# Contributing to GroveShell

This project is in Phase 0 (foundation). Read `docs/PROJECT_PLAN.md` before
proposing changes — it defines the phase roadmap, non-goals, and the
per-task workflow expected when working with a coding agent (§17).

## Ground rules

- Every change must keep `cargo build --workspace` and `cargo test --workspace`
  passing on Windows.
- New `unsafe` blocks must be narrowly scoped and carry a safety comment
  explaining the invariant being relied upon.
- Architectural decisions — especially anything touching undocumented APIs,
  privilege escalation, or shell-mode behavior — get an ADR under `docs/adr/`.
- No feature that changes what's visible/interactive on the desktop ships
  without a note on which phase (per `docs/PROJECT_PLAN.md` §16) it belongs to.
- Sign off your commits (Developer Certificate of Origin): add
  `Signed-off-by: Your Name <email>` to commit messages (`git commit -s`).

## Development setup

Windows 11 x64, Rust stable (`rustup default stable`). No other SDKs are
required for Phase 0.
