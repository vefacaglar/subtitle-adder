<#
.SYNOPSIS
Builds the Rust desktop app (release) and stages a runnable Windows app into apps\desktop\dist:

    dist\SubtitleAdder.exe
    dist\*.dll                      FFmpeg 8.1 shared libraries (avcodec, avformat, ... from FFMPEG_DIR\bin)
    dist\subtitle-engine\           the engine (copied from engine\dist: subtitle-engine.exe + its runtime DLLs)

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/stage.ps1 [-Force] [-SkipEngine]

The build is skipped when dist\SubtitleAdder.exe is newer than every desktop source (crates\**\*.rs|*.slint|*.toml|
*.xcstrings, Cargo.toml, Cargo.lock and the embedded catalogs of apps\windows / apps\macos); the copies are refreshed
when their source is newer. The engine is not built here: run engine\scripts\build-dev.ps1 first (a missing
engine\dist only produces a warning; the app then reports that the engine was not found).

Toolchain: MSVC (cl/link), libclang and FFmpeg 8.1 dev files. When cl.exe is not on PATH, a machine-local setup script
is dot-sourced first ($env:SUBTITLE_ENGINE_DEVENV, else %USERPROFILE%\.devtools\devenv.ps1; not part of the repo).
FFMPEG_DIR defaults to engine\.runtime\ffmpeg (fetched by engine\scripts\fetch-runtime.ps1 -Only ffmpeg when missing).
CARGO_TARGET_DIR defaults to %USERPROFILE%\.subtitle-adder-desktop-target because long paths break cl.exe.
Windows PowerShell 5.1 compatible.
#>
param([switch]$Force, [switch]$SkipEngine)
$ErrorActionPreference = 'Stop'

$desktop = Split-Path $PSScriptRoot -Parent
$repo = Split-Path (Split-Path $desktop -Parent) -Parent
$engine = Join-Path $repo 'engine'
$dist = Join-Path $desktop 'dist'
$exe = Join-Path $dist 'SubtitleAdder.exe'

# Long target paths break cl.exe, so default to a short one outside the repo (before devenv.ps1 picks its own).
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.subtitle-adder-desktop-target' }
if (-not $env:FFMPEG_DIR) { $env:FFMPEG_DIR = Join-Path $engine '.runtime\ffmpeg' }
$built = Join-Path $env:CARGO_TARGET_DIR 'release\SubtitleAdder.exe'

function Get-NewestSource {
    $roots = @(
        (Join-Path $desktop 'crates'),
        (Join-Path $repo 'apps\windows\src\SubtitleAdder.Core\Strings'),
        (Join-Path $repo 'apps\macos\Sources\SubtitleAdderApp\Resources'),
        (Join-Path $repo 'apps\macos\Sources\EngineKit\Resources')
    )
    $files = @()
    foreach ($r in $roots) {
        if (Test-Path $r) {
            $files += Get-ChildItem $r -Recurse -File -Include *.rs, *.slint, *.toml, *.xcstrings, *.manifest, *.ico, *.png |
                Where-Object { $_.FullName -notmatch '\\(target|bin|obj)\\' }
        }
    }
    $files += Get-Item (Join-Path $desktop 'Cargo.toml'), (Join-Path $desktop 'Cargo.lock')
    $files | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
}

$needBuild = $true
if (-not $Force -and (Test-Path $exe)) {
    $newest = Get-NewestSource
    if ($newest.LastWriteTimeUtc -le (Get-Item $exe).LastWriteTimeUtc) {
        Write-Host 'dist\SubtitleAdder.exe is up to date.'
        $needBuild = $false
    } else {
        Write-Host "Rebuilding the desktop app ($($newest.FullName) changed)."
    }
}

if ($needBuild) {
    $devenv = if ($env:SUBTITLE_ENGINE_DEVENV) { $env:SUBTITLE_ENGINE_DEVENV } else { Join-Path $env:USERPROFILE '.devtools\devenv.ps1' }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -and (Test-Path $devenv)) {
        $keepTarget = $env:CARGO_TARGET_DIR
        $keepFfmpeg = $env:FFMPEG_DIR
        . $devenv
        $env:CARGO_TARGET_DIR = $keepTarget
        $env:FFMPEG_DIR = $keepFfmpeg
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $env:Path = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:Path
    }
    if (-not (Test-Path (Join-Path $env:FFMPEG_DIR 'include\libavformat\avformat.h'))) {
        & (Join-Path $engine 'scripts\fetch-runtime.ps1') -Only ffmpeg
    }
    # ffmpeg-sys reads FFMPEG_DIR; the build scripts and tests load the DLLs from its bin.
    $env:Path = (Join-Path $env:FFMPEG_DIR 'bin') + ';' + $env:Path

    Push-Location $desktop
    try {
        cargo build --release -j 3 -p sa-app --bin SubtitleAdder
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

New-Item -ItemType Directory -Force -Path $dist | Out-Null

function Copy-IfNewer([string]$from, [string]$to) {
    if (-not (Test-Path $to) -or (Get-Item $from).LastWriteTimeUtc -gt (Get-Item $to).LastWriteTimeUtc -or
        (Get-Item $from).Length -ne (Get-Item $to).Length) {
        Copy-Item -LiteralPath $from -Destination $to -Force
    }
}

if ($needBuild -or -not (Test-Path $exe)) {
    if (-not (Test-Path $built)) { throw "missing build output $built (run with -Force)" }
    Copy-Item -LiteralPath $built -Destination $exe -Force
}

# FFmpeg runtime next to the exe (the player links it dynamically; LGPL/GPL shared DLLs, ARCHITECTURE.md section 11).
$ffbin = Join-Path $env:FFMPEG_DIR 'bin'
if (Test-Path $ffbin) {
    Get-ChildItem $ffbin -Filter *.dll | ForEach-Object { Copy-IfNewer $_.FullName (Join-Path $dist $_.Name) }
} else {
    Write-Warning "FFmpeg DLLs not found in $ffbin; the app will not start without them."
}

# The engine: <exe dir>\subtitle-engine\subtitle-engine.exe (ARCHITECTURE.md section 8.4).
if (-not $SkipEngine) {
    $engineDist = Join-Path $engine 'dist'
    $engineExe = Join-Path $engineDist 'subtitle-engine.exe'
    if (Test-Path $engineExe) {
        $target = Join-Path $dist 'subtitle-engine'
        New-Item -ItemType Directory -Force -Path $target | Out-Null
        Get-ChildItem $engineDist -File | ForEach-Object { Copy-IfNewer $_.FullName (Join-Path $target $_.Name) }
    } else {
        Write-Warning "engine\dist\subtitle-engine.exe not found; run engine\scripts\build-dev.ps1 first."
    }
}

Write-Host "Staged $exe"
