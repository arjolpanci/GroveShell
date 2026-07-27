# ADR-002: Explorer coexistence first

## Status
Accepted

## Decision
`explorer.exe` stays active and is not hidden, killed, or replaced through
the MVP and alpha phases (Phases 0-6). Explorer replacement is isolated to
the opt-in Phase 7.

## Rationale
Explorer provides tray, notification, and file-dialog integrations that
are expensive to reproduce. Coexisting first lets the shell be developed
and tested without risking the user's primary desktop session, per
`docs/PROJECT_PLAN.md` §2.2 ("Progressive replacement") and §16.
