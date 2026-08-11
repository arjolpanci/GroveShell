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

# Interop for gracefully closing a running groveshell-ui by posting WM_CLOSE
# to its bar window (used by Stop-ExistingGroveShell and Stop-UiGracefully).
# Defined before the build because the build has to stop a previous run
# first: a running groveshell-*.exe locks its own file, so cargo's relink
# fails with "Access is denied" (the error this script used to die on when
# groveshell-settings.exe was still resident in the tray).
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class GroveNative {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hwnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp);
}
"@

# Stop any groveshell-* processes left over from a previous run before
# building. groveshell-ui is closed via WM_CLOSE first so it restores the
# Windows taskbar/work areas (a force kill would leave the taskbar hidden);
# every other groveshell-* process (settings, host, watchdog) is stopped
# outright. Without this, cargo can't overwrite a running .exe.
function Stop-ExistingGroveShell {
    $ui = Get-Process -Name 'groveshell-ui' -ErrorAction SilentlyContinue
    foreach ($p in $ui) {
        $uiPid = $p.Id
        $cb = {
            param($hwnd, $lp)
            $windowPid = [uint32]0
            [GroveNative]::GetWindowThreadProcessId($hwnd, [ref]$windowPid) | Out-Null
            if ($windowPid -eq $uiPid) {
                $sb = New-Object System.Text.StringBuilder 256
                [GroveNative]::GetClassName($hwnd, $sb, 256) | Out-Null
                if ($sb.ToString() -eq 'GroveShellBar') {
                    [GroveNative]::PostMessage($hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
                }
            }
            return $true
        }
        [GroveNative]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    }
    if ($ui) {
        $deadline = (Get-Date).AddSeconds(3)
        while ((Get-Date) -lt $deadline -and (Get-Process -Name 'groveshell-ui' -ErrorAction SilentlyContinue)) {
            Start-Sleep -Milliseconds 100
        }
    }
    $remaining = Get-Process -Name 'groveshell-*' -ErrorAction SilentlyContinue
    if ($remaining) {
        Write-Host "Stopping $($remaining.Count) leftover GroveShell process(es) before build..."
        $remaining | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }
}

Stop-ExistingGroveShell

Write-Host "Building workspace..."
cargo build --workspace --manifest-path "$repoRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed."
    exit 1
}

$script:watchdogProc = $null
$script:hostProc = $null
$script:uiProc = $null

# groveshell-ui restores the Windows taskbar/work areas in its WM_DESTROY
# handler, so it must be closed via WM_CLOSE, not Stop-Process — a force
# kill leaves the real taskbar hidden and the desktop without a Start menu.
# (The GroveNative interop it uses is defined above, before the build.)

function Stop-UiGracefully {
    if (-not (Test-Alive $script:uiProc)) { return $false }
    $uiPid = $script:uiProc.Id
    $script:uiBarFound = $false
    $cb = {
        param($hwnd, $lp)
        $windowPid = [uint32]0
        [GroveNative]::GetWindowThreadProcessId($hwnd, [ref]$windowPid) | Out-Null
        if ($windowPid -eq $uiPid) {
            $sb = New-Object System.Text.StringBuilder 256
            [GroveNative]::GetClassName($hwnd, $sb, 256) | Out-Null
            if ($sb.ToString() -eq 'GroveShellBar') {
                # WM_CLOSE -> DestroyWindow -> WM_DESTROY (taskbar restore)
                [GroveNative]::PostMessage($hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
                $script:uiBarFound = $true
                return $false
            }
        }
        return $true
    }
    [GroveNative]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    if ($script:uiBarFound) {
        # give it a moment to unpark windows and restore the taskbar
        $deadline = (Get-Date).AddSeconds(3)
        while ((Get-Date) -lt $deadline -and (Test-Alive $script:uiProc)) {
            Start-Sleep -Milliseconds 100
        }
    }
    if (Test-Alive $script:uiProc) {
        Stop-Process -Id $script:uiProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
        return $false
    }
    return $true
}

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
    # restart/force-kill. groveshell-ui always gets the WM_CLOSE attempt
    # first (with a force-kill fallback inside) so it can restore the real
    # taskbar and unpark windows.
    if (Test-Alive $script:hostProc) {
        Stop-Process -Id $script:hostProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Test-Alive $script:watchdogProc) {
        Stop-Process -Id $script:watchdogProc.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    }
    Stop-UiGracefully | Out-Null
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
                if (Stop-UiGracefully) {
                    Write-Host "groveshell-ui: closed gracefully"
                } else {
                    Write-Host "groveshell-ui: force-killed (WM_CLOSE didn't finish in time)"
                }
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
