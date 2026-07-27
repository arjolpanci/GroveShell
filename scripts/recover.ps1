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

$explorer = Get-Process -Name "explorer" -ErrorAction SilentlyContinue
if (-not $explorer) {
    Write-Host "Explorer is not running. Restarting it..."
    Start-Process "explorer.exe"
    Write-Host "Explorer restarted."
} else {
    Write-Host "Explorer is already running."
}

Write-Host "Recovery complete."
