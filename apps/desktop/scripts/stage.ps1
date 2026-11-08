<#
.SYNOPSIS
Builds the Rust desktop app (release) and stages a runnable Windows app into apps\desktop\dist:

    dist\SubtitleAdder.exe
    dist\*.dll                      FFmpeg 8.1 LGPL shared libraries (avcodec, avformat, ... from FFMPEG_LGPL_DIR\bin)
    dist\subtitle-engine\           the engine (copied from engine\dist: subtitle-engine.exe + its runtime DLLs,
                                    including its own GPL FFmpeg build)

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/stage.ps1 [-Force] [-SkipEngine]

The build is skipped when dist\SubtitleAdder.exe is newer than every desktop source (crates\**\*.rs|*.slint|*.toml|
*.xcstrings, Cargo.toml, Cargo.lock and the embedded catalogs of apps\windows / apps\macos); the copies are refreshed
when their source is newer. The engine is not built here: run engine\scripts\build-dev.ps1 first (a missing
engine\dist only produces a warning; the app then reports that the engine was not found).

Toolchain: MSVC (cl/link), libclang and FFmpeg 8.1 dev files. When cl.exe is not on PATH, a machine-local setup script
is dot-sourced first ($env:SUBTITLE_ENGINE_DEVENV, else %USERPROFILE%\.devtools\devenv.ps1; not part of the repo).
FFMPEG_DIR defaults to engine\.runtime\ffmpeg (fetched by engine\scripts\fetch-runtime.ps1 -Only ffmpeg when missing).

FFmpeg licensing: the app compiles against the GPL SDK in FFMPEG_DIR (the engine's; same API and DLL names), but
the DLLs staged next to SubtitleAdder.exe are the LGPL shared build of the same BtbN autobuild (FFMPEG_LGPL_DIR,
default engine\.runtime\ffmpeg-lgpl, fetched by fetch-runtime.ps1 -Only ffmpeg-lgpl when missing): the
player only decodes. The engine folder keeps its GPL DLLs (libx264 for `preview`). Windows resolves each process's
import DLLs from its own executable's folder first, so neither process loads the other's copies; the staged app is
checked with `SubtitleAdder.exe --probe` on a PATH without any FFmpeg (it must load and report an LGPL avcodec).
CARGO_TARGET_DIR defaults to %USERPROFILE%\.subtitle-adder-desktop-target because long paths break cl.exe.
Windows PowerShell 5.1 compatible.
#>
param([switch]$Force, [switch]$SkipEngine)
$ErrorActionPreference = 'Stop'
function Get-Sha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try { return ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}
# Windows PowerShell 5.1 started from pwsh 7 inherits pwsh's PSModulePath, which breaks module autoloading
# (e.g. Get-FileHash not found); fall back to 5.1's own module path.
if ($PSVersionTable.PSEdition -eq 'Desktop' -and $env:PSModulePath -match 'PowerShell[\/]7') {
    $env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
}

$desktop = Split-Path $PSScriptRoot -Parent
$repo = Split-Path (Split-Path $desktop -Parent) -Parent
$engine = Join-Path $repo 'engine'
$dist = Join-Path $desktop 'dist'
$exe = Join-Path $dist 'SubtitleAdder.exe'

# Long target paths break cl.exe, so default to a short one outside the repo (before devenv.ps1 picks its own).
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.subtitle-adder-desktop-target' }
if (-not $env:FFMPEG_DIR) { $env:FFMPEG_DIR = Join-Path $engine '.runtime\ffmpeg' }
$ffmpegLgpl = if ($env:FFMPEG_LGPL_DIR) { $env:FFMPEG_LGPL_DIR } else { Join-Path $engine '.runtime\ffmpeg-lgpl' }
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

# The player's FFmpeg runtime next to the exe: the LGPL shared build (ARCHITECTURE.md section 11), never the GPL DLLs
# of FFMPEG_DIR (those stay the engine's). Copied by content: both builds use the same file names.
$ffbin = Join-Path $ffmpegLgpl 'bin'
if (-not (Test-Path (Join-Path $ffbin 'avcodec-62.dll')) -and -not $env:FFMPEG_LGPL_DIR) {
    try {
        & (Join-Path $engine 'scripts\fetch-runtime.ps1') -Only ffmpeg-lgpl
    } catch {
        Write-Warning "Fetching the LGPL FFmpeg failed: $($_.Exception.Message)"
    }
}
$requireLgpl = $true
if (-not (Test-Path (Join-Path $ffbin 'avcodec-62.dll'))) {
    if ($env:FFMPEG_LGPL_DIR) { throw "LGPL FFmpeg DLLs not found in $ffbin (FFMPEG_LGPL_DIR)." }
    # A local run must still start; never what a package ships (package-desktop-windows.ps1 has no fallback).
    Write-Warning "LGPL FFmpeg DLLs not found in $ffbin; staging the GPL DLLs of FFMPEG_DIR for this local run only."
    $ffbin = Join-Path $env:FFMPEG_DIR 'bin'
    $requireLgpl = $false
}
$lgplDlls = @(Get-ChildItem $ffbin -Filter *.dll -File)
foreach ($dll in $lgplDlls) {
    $to = Join-Path $dist $dll.Name
    if (-not (Test-Path $to) -or (Get-Sha256Hex $to) -ne (Get-Sha256Hex $dll.FullName)) {
        Copy-Item -LiteralPath $dll.FullName -Destination $to -Force
    }
}
# The license text and notice of whichever build was staged (LGPL; GPL only in the local fallback above).
. (Join-Path $engine 'scripts\ffmpeg-notice.ps1')
Write-FFmpegNotice -FFmpegDir (Split-Path $ffbin -Parent) -To $dist -Kind $(if ($requireLgpl) { 'LGPL' } else { 'GPL' })
# FFmpeg DLLs an earlier (GPL) staging left behind that the LGPL build does not have.
$ffmpegNames = '^(avcodec|avdevice|avfilter|avformat|avutil|postproc|swresample|swscale)-\d+\.dll$'
Get-ChildItem $dist -Filter *.dll -File | Where-Object {
    $_.Name -match $ffmpegNames -and -not ($lgplDlls.Name -contains $_.Name)
} | Remove-Item -Force

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

# The staged app must start from its own folder alone and load the LGPL FFmpeg (no FFmpeg on PATH, so a missing DLL
# or symbol cannot be filled in from the GPL SDK).
$savedPath = $env:Path
$probeOut = Join-Path ([IO.Path]::GetTempPath()) "SubtitleAdder-probe-$PID.txt"
try {
    $env:Path = "$env:SystemRoot\System32;$env:SystemRoot"
    $probe = Start-Process -FilePath $exe -ArgumentList '--probe' -NoNewWindow -Wait -PassThru -RedirectStandardOutput $probeOut
    $probeText = if (Test-Path $probeOut) { (Get-Content -Raw $probeOut) } else { '' }
} finally {
    $env:Path = $savedPath
    Remove-Item $probeOut -ErrorAction SilentlyContinue
}
if ($probe.ExitCode -ne 0) { throw "dist\SubtitleAdder.exe --probe failed (exit code $($probe.ExitCode)); a runtime DLL is missing." }
if ($requireLgpl -and $probeText -notmatch '(?m)^ffmpeg license: LGPL') { throw "dist\SubtitleAdder.exe did not load an LGPL FFmpeg:`n$probeText" }
Write-Host ($probeText.Trim() -split "`r?`n" | Where-Object { $_ -match '^ffmpeg' }) -Separator "`n"

Write-Host "Staged $exe"
