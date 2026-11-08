<#
.SYNOPSIS
Release mode for Windows: packages the Rust desktop app (scripts/package-desktop-windows.ps1 -Zip, which builds the
app and the engine in release) into build\desktop\Subtitle Adder and starts the packaged SubtitleAdder.exe.
Used by the VS Code entry "Rust: Release (Windows)"; works from cmd, Windows PowerShell and pwsh.

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/release.ps1 [-NoZip] [-NoLaunch]
#>
param([switch]$NoZip, [switch]$NoLaunch)
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 started from pwsh 7 inherits pwsh's PSModulePath, which breaks module autoloading
# (e.g. Get-FileHash not found); fall back to 5.1's own module path.
if ($PSVersionTable.PSEdition -eq 'Desktop' -and $env:PSModulePath -match 'PowerShell[\/]7') {
    $env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
}
$root = Split-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) -Parent
$package = Join-Path $root 'scripts\package-desktop-windows.ps1'
# The package script needs PowerShell 7.
$pwsh = Get-Command pwsh.exe -ErrorAction SilentlyContinue
if (-not $pwsh) { throw 'PowerShell 7 (pwsh.exe) is required for scripts\package-desktop-windows.ps1.' }
# The engine is built (only when its sources changed) by build-dev.ps1 in its own process and target dir, then the
# package reuses engine\dist; building it inside the package script would compile CTranslate2/oneDNN again in the
# app's target dir.
$windowsPowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
& $windowsPowerShell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'engine\scripts\build-dev.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$packageArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $package, '-SkipEngineBuild')
if (-not $NoZip) { $packageArgs += '-Zip' }
& $pwsh.Source @packageArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($NoLaunch) { return }
$exe = Join-Path $root 'build\desktop\Subtitle Adder\SubtitleAdder.exe'
if (-not (Test-Path $exe)) { throw "Packaged app not found: $exe" }
Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe)
