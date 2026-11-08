<#
.SYNOPSIS
Builds the Rust engine and stages it into engine\dist (the engine the dev Windows app uses), skipping the build when
engine\dist is already newer than every engine source.

    pwsh -NoProfile -File engine/scripts/build-dev.ps1 [-Force]

Toolchain: MSVC (cl/link), CMake, Ninja, libclang and nuget must be reachable, as from a VS Developer shell. When
cl.exe is not on PATH, a machine-local setup script is dot-sourced first ($env:SUBTITLE_ENGINE_DEVENV, else
%USERPROFILE%\.devtools\devenv.ps1; not part of the repo). CARGO_TARGET_DIR defaults to %USERPROFILE%\.subtitle-adder-target
because long paths break cl.exe.
#>
param([switch]$Force)
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 started from pwsh 7 inherits pwsh's PSModulePath, which breaks module autoloading
# (e.g. Get-FileHash not found); fall back to 5.1's own module path.
if ($PSVersionTable.PSEdition -eq 'Desktop' -and $env:PSModulePath -match 'PowerShell[\/]7') {
    $env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
}
$engine = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $engine 'dist\subtitle-engine.exe'

# dist\.stamp marks the last finished build-dev run (the staged exe keeps cargo's timestamp, so it cannot tell when the
# staging scripts last ran). Staging-script changes (runtime DLLs, license notices) restage the folder too.
$stamp = Join-Path $engine 'dist\.stamp'
if (-not $Force -and (Test-Path $exe)) {
    $built = if (Test-Path $stamp) { (Get-Item $stamp).LastWriteTimeUtc } else { (Get-Item $exe).LastWriteTimeUtc }
    $sources = @(Get-ChildItem (Join-Path $engine 'crates') -Recurse -File -Include *.rs, *.toml) +
        @(Get-Item (Join-Path $engine 'Cargo.toml'), (Join-Path $engine 'Cargo.lock')) +
        @(Get-ChildItem (Join-Path $engine 'scripts') -File -Include *.ps1 -Recurse)
    $newer = $sources | Where-Object { $_.LastWriteTimeUtc -gt $built } | Select-Object -First 1
    if (-not $newer) {
        Write-Host 'engine\dist is up to date.'
        exit 0
    }
    Write-Host "Rebuilding the engine ($($newer.FullName) changed)."
}

# Toolchain: when MSVC's cl.exe is not on PATH (not a VS Developer shell), dot-source a machine-local setup script:
# $env:SUBTITLE_ENGINE_DEVENV, else %USERPROFILE%\.devtools\devenv.ps1 (for example a portable MSVC). Not in the repo.
$devenv = if ($env:SUBTITLE_ENGINE_DEVENV) { $env:SUBTITLE_ENGINE_DEVENV } else { Join-Path $env:USERPROFILE '.devtools\devenv.ps1' }
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -and (Test-Path $devenv)) { . $devenv }
# Long target paths break cl.exe, so default to a short one outside the repo.
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.subtitle-adder-target' }
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
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Set-Content -Path $stamp -Value (Get-Date).ToUniversalTime().ToString('o') -Encoding ASCII
