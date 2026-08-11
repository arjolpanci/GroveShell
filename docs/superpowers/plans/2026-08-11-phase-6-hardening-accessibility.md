# Phase 6 — Hardening and Accessibility Implementation Plan

**Goal:** Bring GroveShell to the Phase 6 exit bar of `docs/PROJECT_PLAN.md` §16 —
"prepare an alpha suitable for other contributors" — by landing the code-level
deliverables of hardening and accessibility, plus the operator tooling and
documentation that let a contributor exercise the recovery/soak/compatibility
matrix on real hardware.

## Phase 6 tasks (PROJECT_PLAN §16) and how each is addressed here

| §16 task | This plan |
|---|---|
| Complete per-monitor DPI and topology testing | Pure logical↔physical DPI-conversion helpers in `window-model` with unit tests; `list-monitors` gains a DPI/scale column so topology can be inspected from the CLI. |
| Add UI Automation semantics and keyboard-only traversal | Shell windows get stable accessible names/classes; overview + dock keyboard traversal is documented and verified; a UIA *provider* (COM) is scoped as follow-up and called out honestly. |
| Add high-contrast / reduced-motion options | `reduced_motion` already exists; add `appearance.high_contrast`, wire it into the UI palette, expose it in Settings. |
| Build compatibility rules and ignore list | New `[compatibility]` config section with an ignore list matched on exe/class/title; pure matcher in `window-model` with unit tests; `snapshot` honors it. |
| Add diagnostics bundle and privacy controls | `groveshell-cli diagnostics` collects config + logs + window/monitor state into a timestamped bundle; new `[privacy]` config (`redact_window_titles`, `telemetry` off by default) redacts titles in dumps. |
| Run soak, crash injection, and visual regression tests | `scripts/soak.ps1`, `scripts/crash-injection.ps1`, `scripts/recovery-matrix.ps1` drive the runtime; results feed the compatibility matrix. |

## Exit criteria mapping

- **No known recovery failure in test matrix** → `scripts/recovery-matrix.ps1` kills each process at each state and asserts Explorer restoration; `docs/compatibility.md` records outcomes.
- **Basic screen-reader navigation works** → accessible names on shell windows + keyboard traversal; Narrator walkthrough documented in `docs/compatibility.md`.
- **Published compatibility matrix distinguishes supported / partial / ignored** → `docs/compatibility.md`.

## What is code (this session) vs. what needs real hardware

Automatable and landed here: config schema + validation, ignore-list matching,
DPI conversion math, diagnostics bundling + redaction, high-contrast palette,
scripts, and docs — all with unit tests where the logic is pure.

Needs a real multi-monitor / assistive-tech session to *sign off* (scripts and
docs provided so a contributor can run them): 72-hour soak, live Narrator
navigation, primary-monitor-unplug topology change, and the full app
compatibility sweep. These are marked "pending hardware" in `docs/compatibility.md`.

## Commit sequence

1. This plan.
2. `config`: `high_contrast`, `[privacy]`, `[compatibility]` ignore list + validation + tests.
3. `window-model`: ignore-list matcher + class name + DPI conversion helpers + tests; `snapshot` honors ignore rules.
4. `ui`: high-contrast palette wiring + accessible window names.
5. `settings`: Accessibility/Privacy surface for the new toggles.
6. `cli`: `diagnostics` bundle + `dump-state`, DPI column on `list-monitors`.
7. `scripts` + `docs/compatibility.md` + README/PROJECT_PLAN progress.
