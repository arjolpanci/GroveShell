# GroveShell dev dashboard: builds the workspace, starts groveshell-watchdog,
# groveshell-host, and groveshell-ui as hidden background processes (so you
# don't get three extra console windows), then shows live status from this
# terminal with commands to ping, gracefully shut down, restart, or
# force-kill them.

$repoRoot = Split-Path -Parent $PSScriptRoot
$hostExe = "$repoRoot\target\debug\groveshell-host.exe"
$watchdogExe = "$repoRoot\target\debug\groveshell-watchdog.exe"
$uiExe = "$repoRoot\target\debug\groveshell-ui.exe"
$cliExe = "$repoRoot\target\debug\groveshell-cli.exe"
$logDir = "$env:LOCALAPPDATA\GroveShell\logs"

# Rust panics go to stderr, bypassing the tracing log files under $logDir,
# so they're captured here instead of being silently lost behind
# -WindowStyle Hidden.
$hostErrLog = "$repoRoot\target\groveshell-host.stderr.log"
$watchdogErrLog = "$repoRoot\target\groveshell-watchdog.stderr.log"
$uiErrLog = "$repoRoot\target\groveshell-ui.stderr.log"

Write-Host "Building workspace..."
cargo build --workspace --manifest-path "$repoRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed."
    exit 1
}

$script:watchdogProc = $null
$script:hostProc = $null
$script:uiProc = $null

function Start-Watchdog {
    $script:watchdogProc = Start-Process -FilePath $watchdogExe -WindowStyle Hidden `
        -RedirectStandardError $watchdogErrLog -PassThru
}

function Start-HostProcess {
    $script:hostProc = Start-Process -FilePath $hostExe -WindowStyle Hidden `
        -RedirectStandardError $hostErrLog -PassThru
}

function Start-Ui {
    $script:uiProc = Start-Process -FilePath $uiExe -WindowStyle Hidden `
        -RedirectStandardError $uiErrLog -PassThru
}

function Test-Alive {
    param($proc)
    if ($null -eq $proc) { return $false }
    $p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    return ($null -ne $p)
}

function Stop-Tracked {
    # The host and watchdog get a chance to exit gracefully over IPC first
    # (see the 's' handler below); this is the force-kill fallback used by
    # restart/force-kill, plus the only stop path groveshell-ui has, since
    # it doesn't speak the host/watchdog IPC protocol yet.
    if (Test-Alive $script:hostProc) {
        Stop-Process -Id $script:hostProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Test-Alive $script:watchdogProc) {
        Stop-Process -Id $script:watchdogProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Test-Alive $script:uiProc) {
        Stop-Process -Id $script:uiProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    }
}

Write-Host "Starting groveshell-watchdog..."
Start-Watchdog
Start-Sleep -Seconds 1
Write-Host "Starting groveshell-host..."
Start-HostProcess
Write-Host "Starting groveshell-ui..."
Start-Ui

function Show-Dashboard {
    Clear-Host
    Write-Host "GroveShell Dev Dashboard" -ForegroundColor Cyan
    Write-Host "========================"
    Write-Host ""

    if (Test-Alive $script:watchdogProc) {
        Write-Host ("  watchdog   RUNNING  (pid {0})" -f $script:watchdogProc.Id) -ForegroundColor Green
    } else {
        Write-Host "  watchdog   STOPPED" -ForegroundColor Red
    }

    if (Test-Alive $script:hostProc) {
        Write-Host ("  host       RUNNING  (pid {0})" -f $script:hostProc.Id) -ForegroundColor Green
    } else {
        Write-Host "  host       STOPPED" -ForegroundColor Red
    }

    if (Test-Alive $script:uiProc) {
        Write-Host ("  ui         RUNNING  (pid {0})" -f $script:uiProc.Id) -ForegroundColor Green
    } else {
        Write-Host "  ui         STOPPED" -ForegroundColor Red
    }

    Write-Host ""
    Write-Host "logs: $logDir"
    Write-Host ""
    Write-Host "[P] Ping   [S] Graceful shutdown   [R] Restart   [K] Force-kill   [Q] Quit dashboard (leaves processes running)"
}

$exitDashboard = $false
while (-not $exitDashboard) {
    Show-Dashboard

    $deadline = (Get-Date).AddSeconds(1)
    $key = $null
    while ((Get-Date) -lt $deadline) {
        if ([Console]::KeyAvailable) {
            $key = [Console]::ReadKey($true)
            break
        }
        Start-Sleep -Milliseconds 100
    }

    if ($null -eq $key) { continue }

    switch ($key.KeyChar.ToString().ToLower()) {
        'p' {
            Write-Host ""
            & $cliExe ping
            Write-Host ""
            Write-Host "Press any key to continue..."
            [Console]::ReadKey($true) | Out-Null
        }
        's' {
            Write-Host ""
            & $cliExe shutdown
            if (Test-Alive $script:uiProc) {
                Stop-Process -Id $script:uiProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
                Write-Host "groveshell-ui: stopped"
            } else {
                Write-Host "groveshell-ui: not running"
            }
            Start-Sleep -Milliseconds 300
            Write-Host ""
            Write-Host "Press any key to continue..."
            [Console]::ReadKey($true) | Out-Null
        }
        'r' {
            Write-Host ""
            Write-Host "Restarting..."
            Stop-Tracked
            Start-Sleep -Milliseconds 300
            Start-Watchdog
            Start-Sleep -Seconds 1
            Start-HostProcess
            Start-Ui
        }
        'k' {
            Write-Host ""
            Write-Host "Force-killing..."
            Stop-Tracked
            Start-Sleep -Milliseconds 300
        }
        'q' {
            Write-Host ""
            Write-Host "Leaving groveshell-host / groveshell-watchdog / groveshell-ui running in the background."
            $exitDashboard = $true
        }
    }
}
