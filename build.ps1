#!/usr/bin/env pwsh
# Build the release binaries WITHOUT touching the running zellij.
#
# Split out from install.ps1 so you can rebuild while a zellij session is
# still up, then swap the binary on your own schedule: run `.\copy.ps1`
# (which stops zellij and copies the freshly built .exe into ~/.cargo/bin)
# whenever you're ready. Nothing in here kills a process or copies anything.
#
# Builds the same artifacts install.ps1 does:
#   - zellij.exe via `cargo xtask ci build-release --no-web`, which first
#     rebakes the default plugins under [profile.release] (strip=true) into
#     zellij-utils/assets/plugins/ and then embeds them — so a plugin source
#     change actually lands in the binary (the bake step).
#   - zellijctl.exe, the client-only dispatcher mux shells out to (not part of
#     the xtask release target, so it's built explicitly).
#
# When it finishes the binaries sit in target/release/ ready for copy.ps1.
# Because build-release rebakes plugins, expect zellij-utils/assets/plugins/*.wasm
# to change too — stage those alongside any plugin source change when you commit.

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = $PSScriptRoot

# OpenSSL build deps live under Strawberry Perl on this machine (matches install.ps1).
$env:PATH = "C:\Strawberry\perl\bin;$env:PATH"

Push-Location $repo
try {
    cargo xtask ci build-release --no-web
    if ($LASTEXITCODE -ne 0) { throw "build failed (exit $LASTEXITCODE)" }
    cargo build --release -p zellijctl
    if ($LASTEXITCODE -ne 0) { throw "zellijctl build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "Built (running zellij left untouched):" -ForegroundColor Green
Write-Host "  $repo\target\release\zellij.exe"
Write-Host "  $repo\target\release\zellijctl.exe"
Write-Host ""
Write-Host "When ready, swap the installed binary by running from OUTSIDE zellij:" -ForegroundColor Yellow
Write-Host "  .\copy.ps1"
