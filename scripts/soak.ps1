# GroveShell soak test (PROJECT_PLAN Phase 6: "run soak ... tests"; quality
# targets in §4: idle CPU, working set, no crashes over long runs).
#
# Starts the shell (assumes a build already exists), then for a configured
# duration repeatedly opens and closes throwaway windows while sampling each
# GroveShell process's CPU and working set into a CSV. Any process that
# disappears mid-run is flagged as a crash. This is the harness for the
# 8-72h soak; default is a short 10-minute smoke so it can be run casually.
#
# Usage:  scripts\soak.ps1 [-Minutes 60] [-Csv path]

param(
    [int]$Minutes = 10,
    [string]$Csv = "$env:LOCALAPPDATA\GroveShell\logs\soak-$(Get-Date -Format yyyyMMdd-HHmmss).csv"
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$watchdogExe = "$repoRoot\target\debug\groveshell-watchdog.exe"
$hostExe = "$repoRoot\target\debug\groveshell-host.exe"
$uiExe = "$repoRoot\target\debug\groveshell-ui.exe"

Write-Host "Starting shell for a $Minutes-minute soak..." -ForegroundColor Cyan
Start-Process -FilePath $watchdogExe -WindowStyle Hidden | Out-Null
Start-Sleep -Seconds 1
Start-Process -FilePath $hostExe -WindowStyle Hidden | Out-Null
Start-Process -FilePath $uiExe -WindowStyle Hidden | Out-Null
Start-Sleep -Seconds 3

"timestamp,process,pid,cpu_seconds,working_set_mb" | Out-File -FilePath $Csv -Encoding utf8

$deadline = (Get-Date).AddMinutes($Minutes)
$crashed = $false
$churn = $null

while ((Get-Date) -lt $deadline) {
    # Window churn: open a Notepad, let the shell react, close it. This is
    # what exercises the WinEvent reconcile path over and over.
    $churn = Start-Process -FilePath 'notepad.exe' -PassThru
    Start-Sleep -Seconds 2

    foreach ($name in @('groveshell-watchdog', 'groveshell-host', 'groveshell-ui')) {
        $p = Get-Process -Name $name -ErrorAction SilentlyContinue
        if (-not $p) {
            Write-Warning "$name is GONE at $(Get-Date -Format HH:mm:ss) - crash during soak"
            $crashed = $true
            continue
        }
        $wsMb = [math]::Round($p.WorkingSet64 / 1MB, 1)
        "$(Get-Date -Format s),$name,$($p.Id),$([math]::Round($p.CPU,2)),$wsMb" |
            Add-Content -Path $Csv -Encoding utf8
    }

    if ($churn -and -not $churn.HasExited) { $churn | Stop-Process -Force -Confirm:$false }
    Start-Sleep -Seconds 3
}

Write-Host "Soak finished. Samples written to $Csv" -ForegroundColor Green
if ($crashed) {
    Write-Error "SOAK FAILURE: at least one GroveShell process crashed during the run."
    exit 1
}
Write-Host "No crashes observed." -ForegroundColor Green
