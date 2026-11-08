<#
.SYNOPSIS
Builds and stages the Rust desktop app when its sources changed (stage.ps1), then starts dist\SubtitleAdder.exe.
Used by the VS Code launch entry "Subtitle Adder (Rust)"; works from cmd, Windows PowerShell and pwsh.

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/run.ps1 [app arguments...]
#>
$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'stage.ps1')
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path (Split-Path $PSScriptRoot -Parent) 'dist\SubtitleAdder.exe'
if (-not (Test-Path $exe)) { throw "Staged app not found: $exe" }
if ($args.Count -gt 0) { Start-Process -FilePath $exe -ArgumentList $args } else { Start-Process -FilePath $exe }
