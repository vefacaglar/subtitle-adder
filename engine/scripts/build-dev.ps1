<#
.SYNOPSIS
Builds the Rust engine and stages it into engine\dist (the engine the dev Windows app uses), skipping the build when
engine\dist is already newer than every engine source.

    pwsh -NoProfile -File engine/scripts/build-dev.ps1 [-Force]

Toolchain: MSVC (cl/link), CMake, Ninja, libclang and nuget must be reachable, as from a VS Developer shell. A
machine-local setup script %LOCALAPPDATA%\subtitle-adder\devenv.ps1 is dot-sourced first when it exists (for example
to point at a portable MSVC); it is not part of the repo. CARGO_TARGET_DIR defaults to a short path under
%LOCALAPPDATA% because long paths break cl.exe.
#>
param([switch]$Force)
$ErrorActionPreference = 'Stop'
$engine = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $engine 'dist\subtitle-engine.exe'

if (-not $Force -and (Test-Path $exe)) {
    $built = (Get-Item $exe).LastWriteTimeUtc
    $sources = @(Get-ChildItem (Join-Path $engine 'crates') -Recurse -File -Include *.rs, *.toml) +
        @(Get-Item (Join-Path $engine 'Cargo.toml'), (Join-Path $engine 'Cargo.lock'))
    $newer = $sources | Where-Object { $_.LastWriteTimeUtc -gt $built } | Select-Object -First 1
    if (-not $newer) {
        Write-Host 'engine\dist is up to date.'
        exit 0
    }
    Write-Host "Rebuilding the engine ($($newer.FullName) changed)."
}

$hook = Join-Path $env:LOCALAPPDATA 'subtitle-adder\devenv.ps1'
if (Test-Path $hook) { . $hook }
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'subtitle-adder\target' }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $env:Path = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:Path
}

& (Join-Path $PSScriptRoot 'fetch-runtime.ps1')
if (-not $env:FFMPEG_DIR) { $env:FFMPEG_DIR = Join-Path $engine '.runtime\ffmpeg' }

Push-Location $engine
try {
    cargo build --release -p se-cli -j 2
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
& (Join-Path $PSScriptRoot 'stage-runtime.ps1')
