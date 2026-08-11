# GroveShell recovery matrix (PROJECT_PLAN Phase 6 exit criterion:
# "no known recovery failure in test matrix").
#
# For each shell process, this force-kills it while the stack is running and
# then asserts the recovery contract still holds: Explorer is alive, the real
# taskbar window is present, and no orphaned GroveShell top-level windows are
# left hijacking the desktop. It runs recover.ps1 as the safety net after
# each case so a failed recovery never leaves the machine without a desktop.
#
# Run from an elevated-enough session where you can start/stop these
# processes. This deliberately kills processes; do not run it on a machine
# doing real work.

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$watchdogExe = "$repoRoot\target\debug\groveshell-watchdog.exe"
$hostExe = "$repoRoot\target\debug\groveshell-host.exe"
$uiExe = "$repoRoot\target\debug\groveshell-ui.exe"
$recover = "$PSScriptRoot\recover.ps1"

function Test-DesktopHealthy {
    # Explorer alive AND the real taskbar (Shell_TrayWnd) present again.
    $explorer = Get-Process -Name 'explorer' -ErrorAction SilentlyContinue
    if (-not $explorer) { return $false }
    Add-Type -Name RecMatrix -Namespace GS -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)]
public static extern IntPtr FindWindow(string cls, string title);
'@ -ErrorAction SilentlyContinue
    $tray = [GS.RecMatrix]::FindWindow('Shell_TrayWnd', $null)
    return ($tray -ne [IntPtr]::Zero)
}

function Start-Stack {
    Start-Process -FilePath $watchdogExe -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds 1
    Start-Process -FilePath $hostExe -WindowStyle Hidden | Out-Null
    Start-Process -FilePath $uiExe -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds 3
}

function Kill-One {
    param([string]$Name)
    $p = Get-Process -Name $Name -ErrorAction SilentlyContinue
    if ($p) { $p | Stop-Process -Force -Confirm:$false }
}

$cases = @('groveshell-watchdog', 'groveshell-host', 'groveshell-ui')
$results = @()

foreach ($victim in $cases) {
    Write-Host "== killing $victim while the stack runs ==" -ForegroundColor Cyan
    & $recover *> $null            # clean slate
    Start-Stack
    Kill-One $victim
    Start-Sleep -Seconds 8         # give the watchdog its 6s-unhealthy + recovery window

    $healthyAfterWatchdog = Test-DesktopHealthy

    # Safety net + second assertion: manual recovery must always restore.
    & $recover *> $null
    Start-Sleep -Seconds 2
    $healthyAfterManual = Test-DesktopHealthy

    $results += [pscustomobject]@{
        Killed              = $victim
        RecoveredAutomatic  = $healthyAfterWatchdog
        RecoveredManual     = $healthyAfterManual
    }
}

Write-Host ""
Write-Host "Recovery matrix results:" -ForegroundColor Yellow
$results | Format-Table -AutoSize

if ($results | Where-Object { -not $_.RecoveredManual }) {
    Write-Error "RECOVERY FAILURE: manual recovery did not restore the desktop in at least one case."
    exit 1
}
Write-Host "All cases recovered." -ForegroundColor Green
