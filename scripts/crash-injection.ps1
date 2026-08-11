# GroveShell crash-injection test (PROJECT_PLAN Phase 6: "run ... crash
# injection ... tests"; §13 reliability: watchdog + crash-loop handling).
#
# In a loop, force-kills a randomly chosen shell process, then checks that
# the desktop is still recoverable. This shakes out crashes at arbitrary
# points in the reconcile/render cycle and verifies the recovery contract
# holds no matter which process dies. After the run it confirms Explorer is
# healthy via recover.ps1 so the machine is never left without a desktop.
#
# Usage:  scripts\crash-injection.ps1 [-Iterations 20]

param(
    [int]$Iterations = 20
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$watchdogExe = "$repoRoot\target\debug\groveshell-watchdog.exe"
$hostExe = "$repoRoot\target\debug\groveshell-host.exe"
$uiExe = "$repoRoot\target\debug\groveshell-ui.exe"
$recover = "$PSScriptRoot\recover.ps1"

function Start-Stack {
    Start-Process -FilePath $watchdogExe -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds 1
    Start-Process -FilePath $hostExe -WindowStyle Hidden | Out-Null
    Start-Process -FilePath $uiExe -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds 3
}

function Test-ExplorerHealthy {
    $null -ne (Get-Process -Name 'explorer' -ErrorAction SilentlyContinue)
}

$targets = @('groveshell-watchdog', 'groveshell-host', 'groveshell-ui')
$failures = 0

for ($i = 1; $i -le $Iterations; $i++) {
    & $recover *> $null
    Start-Stack

    $victim = $targets | Get-Random
    Write-Host "[$i/$Iterations] injecting crash into $victim" -ForegroundColor Cyan
    $p = Get-Process -Name $victim -ErrorAction SilentlyContinue
    if ($p) { $p | Stop-Process -Force -Confirm:$false }

    Start-Sleep -Seconds 8   # watchdog unhealthy threshold + recovery

    if (-not (Test-ExplorerHealthy)) {
        Write-Warning "iteration $i ($victim): Explorer NOT healthy after recovery window"
        $failures++
    }
}

# Always leave the machine in a clean, recovered state.
& $recover *> $null

Write-Host ""
if ($failures -gt 0) {
    Write-Error "CRASH-INJECTION FAILURE: $failures of $Iterations iterations left Explorer unhealthy."
    exit 1
}
Write-Host "All $Iterations crash-injection iterations recovered cleanly." -ForegroundColor Green
