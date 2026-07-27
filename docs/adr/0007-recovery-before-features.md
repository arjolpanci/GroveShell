# ADR-007: Recovery before features

## Status
Accepted

## Decision
Watchdog heartbeat/recovery and Explorer restoration are implemented in
Phase 0, before any global input hook or workspace-hiding behavior exists
anywhere in the codebase.

## Rationale
A shell crash must never strand the user without a desktop or way back to
Explorer (`docs/PROJECT_PLAN.md` §2.2, "Failure must be recoverable").
Building recovery first means every subsequent phase that adds riskier
capability (hooks, hiding windows) already has a tested safety net under
it. See `docs/PROJECT_PLAN.md` §13 and §16 Phase 0.
