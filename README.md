# GroveShell

**Status: Phase 0 (foundation), experimental and pre-alpha. Nothing in this phase touches or replaces Explorer.**

GroveShell is an experimental desktop shell for Windows 11: workspaces and an overview screen instead of the usual taskbar-and-Start-menu flow. It runs alongside `explorer.exe` as ordinary user-mode processes and doesn't touch `dwm.exe`. Replacing Explorer is a much later, opt-in phase, and one you'll be able to back out of.

This project takes workflow inspiration from GNOME Shell, nothing more. GNOME is a registered trademark of the GNOME Foundation, and GroveShell is independent: not affiliated with, sponsored by, or endorsed by the Foundation. "GroveShell" itself is just a working codename for now, not a cleared release name. See `docs/PROJECT_PLAN.md` for the trademark note and `docs/adr/0008-working-codename-only.md` for the reasoning.

## What's actually here in Phase 0

- A Cargo workspace: library crates `groveshell-common`, `groveshell-config`, `groveshell-ipc`, plus the `groveshell-host`, `groveshell-watchdog`, and `groveshell-cli` binaries.
- A host process that loads config, answers IPC pings, and sends heartbeats.
- A watchdog process that restores `explorer.exe` if the host stops sending heartbeats.
- A standalone PowerShell recovery script that doesn't depend on the Rust binaries at all.
- No window enumeration, no global hooks, no UI. Those come in Phase 1 and later; see `docs/PROJECT_PLAN.md` §16 for the full roadmap.

## Building (Windows 11 x64, Rust stable)

```powershell
cargo build --workspace
cargo test --workspace
```

## Trying it out

```powershell
# terminal 1
.\target\debug\groveshell-watchdog.exe

# terminal 2
.\target\debug\groveshell-host.exe

# terminal 3
.\target\debug\groveshell-cli.exe ping
```

You should get a `pong` back with a round-trip time. Kill `groveshell-host.exe` from Task Manager and watch the watchdog's log (see below): it should notice the missed heartbeats and confirm or restore Explorer.

Or just run `.\scripts\dev-start.ps1` and skip the three terminals.

## If something goes wrong

Run `.\scripts\recover.ps1`. It stops every `groveshell-*` process and makes sure `explorer.exe` is running, without relying on any GroveShell binary working correctly first.

## Logs

Structured logs land in `%LOCALAPPDATA%\GroveShell\logs\`, one rotating file per process: `host.log`, `watchdog.log`, `cli.log`.

## License

Dual-licensed under Apache-2.0 OR MIT. See `LICENSE-APACHE` and `LICENSE-MIT`.

## Design document

The full technical design, architecture, and phased roadmap lives in `docs/PROJECT_PLAN.md`. Architecture decisions are recorded under `docs/adr/`.
