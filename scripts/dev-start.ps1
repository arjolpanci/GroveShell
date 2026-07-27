# Convenience script for local development: builds the workspace, then
# starts the watchdog before the host (the watchdog must be up first so
# the host's first heartbeat has somewhere to go).

$repoRoot = Split-Path -Parent $PSScriptRoot

Write-Host "Building workspace..."
cargo build --workspace --manifest-path "$repoRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed."
    exit 1
}

Write-Host "Starting groveshell-watchdog..."
Start-Process -FilePath "$repoRoot\target\debug\groveshell-watchdog.exe"

Start-Sleep -Seconds 1

Write-Host "Starting groveshell-host..."
Start-Process -FilePath "$repoRoot\target\debug\groveshell-host.exe"

Write-Host ""
Write-Host "Both processes started. Try:"
Write-Host "  $repoRoot\target\debug\groveshell-cli.exe ping"
