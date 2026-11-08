<#
.SYNOPSIS
Run mode for Windows: builds the engine into engine\dist when its sources changed (engine\scripts\build-dev.ps1),
builds and stages the Rust desktop app when its sources changed (stage.ps1), then starts dist\SubtitleAdder.exe.
Used by the VS Code entry "Rust: Run (Windows)"; works from cmd, Windows PowerShell and pwsh.

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/run.ps1 [app arguments...]
#>
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 started from pwsh 7 inherits pwsh's PSModulePath, which breaks module autoloading
# (e.g. Get-FileHash not found); fall back to 5.1's own module path.
if ($PSVersionTable.PSEdition -eq 'Desktop' -and $env:PSModulePath -match 'PowerShell[\/]7') {
    $env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
}
$root = Split-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) -Parent
$windowsPowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
# A child process: build-dev.ps1 picks the engine's own CARGO_TARGET_DIR, which must not leak into the app build.
& $windowsPowerShell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'engine\scripts\build-dev.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& (Join-Path $PSScriptRoot 'stage.ps1')
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path (Split-Path $PSScriptRoot -Parent) 'dist\SubtitleAdder.exe'
if (-not (Test-Path $exe)) { throw "Staged app not found: $exe" }
if ($args.Count -gt 0) { Start-Process -FilePath $exe -ArgumentList $args } else { Start-Process -FilePath $exe }
