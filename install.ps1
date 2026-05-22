#!/usr/bin/env pwsh
# One-shot install for this zellij fork on Windows.
#
# zellij dynamic-loads CreatePseudoConsole/Resize/Close from a sideloaded
# conpty.dll if it sits next to zellij.exe, falling back to kernel32 if
# absent. To dodge the Win11 23H2 system-conhost crash
# (wezterm/wezterm#7520, #7774), we always sideload a v1.24+ pair. This
# script fetches the pair from the Microsoft Terminal release on first
# run (cached under %LOCALAPPDATA%\zellij-conpty), kills any running
# zellij so the .exe is unlocked, runs `cargo install`, and drops the
# ConPTY pair next to the freshly installed zellij.exe.
#
# Idempotent. Re-run after any pull or to refresh the install.

$ErrorActionPreference = 'Stop'
$repo     = $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$cache    = Join-Path $env:LOCALAPPDATA 'zellij-conpty'

# OpenSSL build deps live under Strawberry Perl on this machine.
$env:PATH = "C:\Strawberry\perl\bin;$env:PATH"

# Pinned ConPTY release. Bump these when bumping the bundled version.
$conptyVer = '1.24.260402001'
$nupkgUrl  = "https://github.com/microsoft/terminal/releases/download/v1.24.10921.0/Microsoft.Windows.Console.ConPTY.$conptyVer.nupkg"

# --- 1. Ensure ConPTY pair is cached on disk ---
$conpty      = Join-Path $cache 'conpty.dll'
$openConsole = Join-Path $cache 'OpenConsole.exe'
if (-not (Test-Path $conpty) -or -not (Test-Path $openConsole)) {
    Write-Host "fetching ConPTY pair v$conptyVer ..."
    New-Item -ItemType Directory -Force -Path $cache | Out-Null
    $nupkg = Join-Path $cache 'conpty.nupkg'
    Invoke-WebRequest -Uri $nupkgUrl -OutFile $nupkg -UseBasicParsing
    $ext = Join-Path $cache 'ext'
    if (Test-Path $ext) { Remove-Item -Recurse -Force $ext }
    Expand-Archive -Path $nupkg -DestinationPath $ext -Force
    Copy-Item (Join-Path $ext 'build\native\runtimes\x64\OpenConsole.exe') $openConsole -Force
    Copy-Item (Join-Path $ext 'runtimes\win-x64\native\conpty.dll')        $conpty      -Force
}

# --- 2. Stop running zellij so the .exe is unlocked ---
$running = @(Get-Process zellij -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    Write-Host "stopping $($running.Count) running zellij process(es) ..."
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

# --- 3. cargo install ---
Push-Location $repo
try {
    cargo install --path . --no-default-features --features web_server_capability --force
    if ($LASTEXITCODE -ne 0) { throw "cargo install failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

# --- 4. Sideload ConPTY pair next to zellij.exe ---
#
# If a running wezterm-gui has already LoadLibrary'd these from $cargoBin,
# the file is held open and Copy-Item -Force will fail. Skip the copy when
# the destination is already at the wanted version — that's the common
# steady-state case after a fresh wezterm install.
function Sync-ConPtyFile {
    param([string]$Src, [string]$DstDir)
    $name    = Split-Path $Src -Leaf
    $dst     = Join-Path $DstDir $name
    $srcVer  = (Get-Item $Src).VersionInfo.FileVersion
    $dstVer  = if (Test-Path $dst) { (Get-Item $dst).VersionInfo.FileVersion } else { $null }
    if ($dstVer -eq $srcVer) {
        Write-Host "  $name $dstVer already in $DstDir (skip)"
        return
    }
    try {
        Copy-Item $Src $dst -Force -ErrorAction Stop
        Write-Host "  installed $name $srcVer -> $DstDir (was $dstVer)"
    } catch {
        Write-Warning "could not overwrite $dst (likely held by a running process). Close any wezterm/zellij and re-run, or accept current $dstVer."
    }
}
Sync-ConPtyFile -Src $openConsole -DstDir $cargoBin
Sync-ConPtyFile -Src $conpty      -DstDir $cargoBin
Write-Host "done." -ForegroundColor Green
