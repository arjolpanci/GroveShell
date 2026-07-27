# Security Policy

GroveShell is pre-alpha software that runs unelevated, user-mode processes
alongside Explorer. It does not currently install any global input hooks,
elevated broker, or driver.

## Reporting a vulnerability

Please do not open a public GitHub issue for security reports. Instead,
open a private security advisory on this repository (GitHub Security
Advisories) or contact the maintainer directly if that isn't available yet.
Include reproduction steps, affected version/commit, and impact.

We aim to acknowledge reports within 5 business days. Given the project's
pre-alpha status, fixes are not guaranteed on any particular timeline, but
all reports will be triaged and tracked.

## Scope notes

- Named-pipe IPC is local-only and not intended to be reachable remotely.
- Window titles, executable paths, and other window metadata are treated as
  untrusted input throughout the codebase (see `docs/PROJECT_PLAN.md` §12).
