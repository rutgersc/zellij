# Kill all running zellij processes (every session, server and client).
# Must be run from OUTSIDE zellij (plain PowerShell/CMD window).
$running = @(Get-Process -Name zellij, zellijctl -ErrorAction SilentlyContinue)
if ($running.Count -eq 0) {
    Write-Host "No zellij processes running."
    exit 0
}

Write-Host "Stopping $($running.Count) zellij process(es)..."
$running | Stop-Process -Force
Start-Sleep -Seconds 2

$remaining = Get-Process -Name zellij, zellijctl -ErrorAction SilentlyContinue
if ($remaining) {
    Write-Error "Could not kill all zellij processes. Run this from outside zellij."
    exit 1
}

Write-Host "Done."
