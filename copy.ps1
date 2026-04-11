# Kill all running zellij processes, then copy the pre-built release binary.
# Must be run from OUTSIDE zellij (plain PowerShell/CMD window).
$dest = "$env:USERPROFILE\.cargo\bin\zellij.exe"
$src  = "$PSScriptRoot\target\release\zellij.exe"

if (-not (Test-Path $src)) {
    Write-Error "No release build found at $src. Run 'cargo build --release' first."
    exit 1
}

Write-Host "Stopping zellij processes..."
Get-Process -Name zellij -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

# Verify no processes remain
$remaining = Get-Process -Name zellij -ErrorAction SilentlyContinue
if ($remaining) {
    Write-Error "Could not kill all zellij processes. Run this from outside zellij."
    exit 1
}

Write-Host "Copying $src -> $dest"
Copy-Item -Path $src -Destination $dest -Force

Write-Host "Done. Installed $(& $dest --version)"
