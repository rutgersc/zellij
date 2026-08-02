# Kill all running zellij processes, then copy the pre-built release binaries.
# Must be run from OUTSIDE zellij (plain PowerShell/CMD window).
#
# Copies both the main `zellij.exe` and the small client-only `zellijctl.exe`
# (the dispatcher mux shells out to). zellijctl is optional here: if it hasn't
# been built yet we warn loudly and still install zellij, rather than silently
# leaving mux pointed at a missing binary.
$cargoBin = "$env:USERPROFILE\.cargo\bin"
$src      = "$PSScriptRoot\target\release\zellij.exe"
$srcCtl   = "$PSScriptRoot\target\release\zellijctl.exe"

if (-not (Test-Path $src)) {
    Write-Error "No release build found at $src. Run 'cargo build --release' first."
    exit 1
}

Write-Host "Stopping zellij processes..."
Get-Process -Name zellij, zellijctl -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

# Verify no processes remain
$remaining = Get-Process -Name zellij, zellijctl -ErrorAction SilentlyContinue
if ($remaining) {
    Write-Error "Could not kill all zellij processes. Run this from outside zellij."
    exit 1
}

Write-Host "Copying $src -> $cargoBin\zellij.exe"
Copy-Item -Path $src -Destination "$cargoBin\zellij.exe" -Force

if (Test-Path $srcCtl) {
    Write-Host "Copying $srcCtl -> $cargoBin\zellijctl.exe"
    Copy-Item -Path $srcCtl -Destination "$cargoBin\zellijctl.exe" -Force
} else {
    Write-Warning "zellijctl.exe not found at $srcCtl - mux's fast switch/focus path will fail until you build it: cargo build --release -p zellijctl"
}

Write-Host "Done. Installed $(& "$cargoBin\zellij.exe" --version)"
