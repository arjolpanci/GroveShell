# GroveShell Safe Recovery
# Deliberately independent of every GroveShell binary: this script must keep
# working even if the Rust processes are broken, hanging, or missing.

Write-Host "GroveShell Safe Recovery"

$groveshellProcesses = Get-Process -Name "groveshell-*" -ErrorAction SilentlyContinue
if ($groveshellProcesses) {
    Write-Host "Stopping $($groveshellProcesses.Count) groveshell-* process(es)..."
    $groveshellProcesses | Stop-Process -Force
} else {
    Write-Host "No groveshell-* processes found."
}

Start-Sleep -Milliseconds 500

# GroveShell switches Windows' three- and four-finger touchpad swipes off
# while it runs, and records what they were first. A killed shell never
# puts them back, so this does, from that record: one "Name=value" line
# per setting, "Name=absent" for a value that did not exist.
$gestureRecord = Join-Path $env:LOCALAPPDATA "GroveShell\touchpad-gestures"
if (Test-Path $gestureRecord) {
    $touchpadKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad"
    foreach ($line in Get-Content $gestureRecord) {
        $name, $value = $line.Trim() -split '=', 2
        if (-not $name -or $name -notmatch '^(Three|Four)FingerSlideEnabled$') { continue }
        if ($value -eq 'absent') {
            Remove-ItemProperty -Path $touchpadKey -Name $name -ErrorAction SilentlyContinue
        } elseif ($value -match '^\d+$') {
            Set-ItemProperty -Path $touchpadKey -Name $name -Value ([int]$value) -Type DWord
        }
    }
    Remove-Item $gestureRecord -ErrorAction SilentlyContinue
    Write-Host "Touchpad gestures restored. Sign out and back in if Windows' swipes do not return."
}

$explorer = Get-Process -Name "explorer" -ErrorAction SilentlyContinue
if (-not $explorer) {
    Write-Host "Explorer is not running. Restarting it..."
    Start-Process "explorer.exe"
    Write-Host "Explorer restarted."
} else {
    Write-Host "Explorer is already running."
}

Write-Host "Recovery complete."
