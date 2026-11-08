#Requires -Version 7
<#
.SYNOPSIS
Packages the Rust desktop app (apps/desktop, Slint) for Windows: SubtitleAdder.exe + FFmpeg DLLs + the Rust engine folder.

.DESCRIPTION
    pwsh scripts/package-desktop-windows.ps1 [-SkipEngineBuild] [-SmokeTest] [-Zip] [-Version 0.2.0]

Output: build\desktop\Subtitle Adder\SubtitleAdder.exe with the FFmpeg runtime DLLs next to it (the player links
FFmpeg 8 dynamically; the LGPL shared build, see FFMPEG_LGPL_DIR), Assets\AppIcon.ico / AppIcon.png (window icon and file-association icon, win-ui-main §3/§4),
and build\desktop\Subtitle Adder\subtitle-engine\subtitle-engine.exe (the packaged engine path of
EngineCommand::locate, ARCHITECTURE.md §8.4) plus the engine's runtime DLLs (its own GPL FFmpeg build: `preview`
encodes with libx264). Both folders hold DLLs of the same names; Windows resolves each process's imports from its own
executable's folder first, so the app never loads the engine's copies (checked below with --probe on a PATH without
FFmpeg, which must report an LGPL avcodec) and the engine never loads the app's. The binary must stay SubtitleAdder.exe:
the file-association registrar and the single-instance contract key on that name. The exe is built with the static
CRT (no VC++ redistributable needed) and carries its icon and manifest (PerMonitorV2, UTF-8, Common Controls v6)
from crates\sa-platform\build.rs. Builds for win-x64 only.

Environment variables:
  SKIP_ENGINE_BUILD     1 = same as -SkipEngineBuild.
  VERSION               Version (default: the workspace version in apps\desktop\Cargo.toml); -Version wins.
  FFMPEG_DIR            FFmpeg 8 SDK (headers, import libs, bin\*.dll); default engine\.runtime\ffmpeg. The GPL
                        build: the app compiles against it (same API and DLL names) and the engine ships its DLLs.
  FFMPEG_LGPL_DIR       FFmpeg 8 LGPL shared build whose bin\*.dll ship next to SubtitleAdder.exe; default
                        engine\.runtime\ffmpeg-lgpl (fetch-runtime.ps1 -Only ffmpeg-lgpl, run when missing).
  CARGO_TARGET_DIR      Default %USERPROFILE%\.subtitle-adder-desktop-target (long target paths break cl.exe).
  SUBTITLE_ENGINE_DEVENV  Toolchain setup script dot-sourced when cl.exe/link.exe are not on PATH
                        (default %USERPROFILE%\.devtools\devenv.ps1).
  SIGN_CERT_THUMBPRINT  SHA-1 thumbprint of a code-signing certificate; if set and signtool is found (PATH or the
                        Windows SDK), SubtitleAdder.exe and the engine's own binary are Authenticode signed.
  SIGN_TIMESTAMP_URL    RFC 3161 timestamp server (default: http://timestamp.digicert.com).

.PARAMETER SkipEngineBuild
Reuses an already staged engine: build\engine-dist\subtitle-engine if present, else engine\dist.

.PARAMETER SmokeTest
Launches SubtitleAdder.exe briefly to verify startup.

.PARAMETER Zip
Also writes build\desktop\SubtitleAdder-<version>-win-x64.zip.
#>
[CmdletBinding()]
param(
    [switch]$SkipEngineBuild,
    [switch]$SmokeTest,
    [switch]$Zip,
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Root = Split-Path -Parent $PSScriptRoot
$Desktop = Join-Path $Root 'apps\desktop'
$Build = Join-Path $Root 'build\desktop'
$AppDir = Join-Path $Build 'Subtitle Adder'
$EngineDist = Join-Path $Root 'build\engine-dist\subtitle-engine'
$EngineExe = Join-Path $EngineDist 'subtitle-engine.exe'
$WinAssets = Join-Path $Desktop 'crates\sa-platform\windows'

function Log([string]$Message) { Write-Host "`n==> $Message" }

function Find-Cargo {
    $onPath = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    if ($env:USERPROFILE) {
        $rustup = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (Test-Path $rustup) { return $rustup }
    }
    return $null
}

function Find-SignTool {
    $onPath = Get-Command signtool -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (-not (Test-Path $kits)) { return $null }
    return Get-ChildItem $kits -Directory -Filter '10.*' |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}

function Use-Toolchain {
    if (Get-Command cl.exe -ErrorAction SilentlyContinue) { return }
    $devenv = if ($env:SUBTITLE_ENGINE_DEVENV) { $env:SUBTITLE_ENGINE_DEVENV } else { Join-Path $env:USERPROFILE '.devtools\devenv.ps1' }
    if (Test-Path $devenv) { . $devenv }
}

if (-not $Version) { $Version = $env:VERSION }
if (-not $Version) {
    $workspace = Get-Content (Join-Path $Desktop 'Cargo.toml') -Raw
    if ($workspace -match '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { $Version = $Matches[1] }
}
if (-not $Version) { $Version = '0.1.0' }

$cargo = Find-Cargo
if (-not $cargo) { throw 'cargo not found; install Rust (https://rustup.rs/) or add cargo to PATH.' }
Use-Toolchain
$fetchedFFmpeg = Join-Path $Root 'engine\.runtime\ffmpeg'
if (-not $env:FFMPEG_DIR -and (Test-Path (Join-Path $fetchedFFmpeg 'bin'))) { $env:FFMPEG_DIR = $fetchedFFmpeg }
if (-not $env:FFMPEG_DIR -or -not (Test-Path (Join-Path $env:FFMPEG_DIR 'bin'))) {
    throw 'FFmpeg 8 SDK not found: set FFMPEG_DIR or run engine\scripts\fetch-runtime.ps1 first.'
}
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.subtitle-adder-desktop-target' }
$FFmpegLgpl = if ($env:FFMPEG_LGPL_DIR) { $env:FFMPEG_LGPL_DIR } else { Join-Path $Root 'engine\.runtime\ffmpeg-lgpl' }
if (-not (Test-Path (Join-Path $FFmpegLgpl 'bin\avcodec-62.dll')) -and -not $env:FFMPEG_LGPL_DIR) {
    & (Join-Path $Root 'engine\scripts\fetch-runtime.ps1') -Only ffmpeg-lgpl
}
if (-not (Test-Path (Join-Path $FFmpegLgpl 'bin\avcodec-62.dll'))) {
    throw "LGPL FFmpeg not found in $FFmpegLgpl (set FFMPEG_LGPL_DIR or run engine\scripts\fetch-runtime.ps1 -Only ffmpeg-lgpl)."
}

Log "1/5 Building app (cargo build --release -p sa-app, $Version)"
# Static CRT (no VCRUNTIME140.dll dependency) comes from apps/desktop/.cargo/config.toml, so this build shares the
# cache of the run mode (apps/desktop/scripts/stage.ps1). The exe's icon and manifest are embedded by
# crates/sa-platform/build.rs.
Push-Location $Desktop
try {
    & $cargo build --release -p sa-app --bin SubtitleAdder -j 3
    & $cargo build --release -p sa-platform --bin sa-make-icon -j 3
}
finally {
    Pop-Location
}
$AppExe = Join-Path $env:CARGO_TARGET_DIR 'release\SubtitleAdder.exe'
$MakeIcon = Join-Path $env:CARGO_TARGET_DIR 'release\sa-make-icon.exe'
if (-not (Test-Path $AppExe)) { throw "cargo produced no $AppExe." }

if ($env:SKIP_ENGINE_BUILD -eq '1') { $SkipEngineBuild = $true }
$EngineDir = Join-Path $Root 'engine'
if ($SkipEngineBuild) {
    # Prefer engine\dist: engine\scripts\build-dev.ps1 keeps it current (apps\desktop\scripts\release.ps1 runs it
    # first), while build\engine-dist is only refreshed by a full package run and may be stale.
    $devDist = Join-Path $EngineDir 'dist'
    if (Test-Path (Join-Path $devDist 'subtitle-engine.exe')) {
        $EngineDist = $devDist
        $EngineExe = Join-Path $EngineDist 'subtitle-engine.exe'
    }
    if (-not (Test-Path $EngineExe)) { throw "-SkipEngineBuild: no staged engine in engine\dist or build\engine-dist\subtitle-engine." }
    Log "2/5 Engine build skipped (reusing $EngineDist)"
}
else {
    Log '2/5 Building engine (fetch-runtime, cargo build --release, stage-runtime)'
    & (Join-Path $EngineDir 'scripts\fetch-runtime.ps1')
    Push-Location $EngineDir
    try {
        & $cargo build --release -p se-cli -j 2
    }
    finally {
        Pop-Location
    }
    if (Test-Path $EngineDist) { Remove-Item $EngineDist -Recurse -Force }
    & (Join-Path $EngineDir 'scripts\stage-runtime.ps1') -Dist $EngineDist
    if (-not (Test-Path $EngineExe)) { throw "stage-runtime.ps1 produced no $EngineExe." }
}

Log '3/5 Assembling folder'
if (Test-Path $AppDir) { Remove-Item $AppDir -Recurse -Force }
New-Item -ItemType Directory -Force $AppDir | Out-Null
Copy-Item $AppExe $AppDir
# The player's FFmpeg runtime: the LGPL shared build of the engine SDK's autobuild (same DLL names as the import
# libraries the app links against). The player only decodes; the GPL DLLs stay in the engine folder.
$ffmpegDlls = Get-ChildItem (Join-Path $FFmpegLgpl 'bin') -Filter *.dll -File
if ($ffmpegDlls.Count -eq 0) { throw "No FFmpeg DLLs in $(Join-Path $FFmpegLgpl 'bin')." }
$ffmpegDlls | Copy-Item -Destination $AppDir
# LGPL redistribution: the build's license text and a notice next to the DLLs (the engine folder carries its GPL
# ones, written by stage-runtime.ps1).
. (Join-Path $Root 'engine\scripts\ffmpeg-notice.ps1')
Write-FFmpegNotice -FFmpegDir $FFmpegLgpl -To $AppDir -Kind LGPL
# Window / registered icon: the committed crates\sa-platform\windows\AppIcon.ico / .png, else rendered by sa-make-icon.
$assets = Join-Path $AppDir 'Assets'
New-Item -ItemType Directory -Force $assets | Out-Null
foreach ($name in 'AppIcon.ico', 'AppIcon.png') {
    $source = Join-Path $WinAssets $name
    if (Test-Path $source) { Copy-Item $source $assets }
}
if (-not (Test-Path (Join-Path $assets 'AppIcon.ico'))) { & $MakeIcon --ico (Join-Path $assets 'AppIcon.ico') }
if (-not (Test-Path (Join-Path $assets 'AppIcon.png'))) { & $MakeIcon (Join-Path $assets 'AppIcon.png') 64 }
Copy-Item $EngineDist (Join-Path $AppDir 'subtitle-engine') -Recurse
# The engine's GPL FFmpeg DLLs carry their own license text and notice (stage-runtime.ps1 writes them).
foreach ($name in 'FFmpeg-LICENSE.txt', 'FFmpeg-NOTICE.txt') {
    if (-not (Test-Path (Join-Path $AppDir "subtitle-engine\$name"))) { throw "subtitle-engine\$name missing: restage the engine (stage-runtime.ps1)." }
}
Get-ChildItem $AppDir -Recurse -Filter *.pdb -File | Remove-Item

$thumbprint = $env:SIGN_CERT_THUMBPRINT
$signtool = if ($thumbprint) { Find-SignTool } else { $null }
if ($thumbprint -and $signtool) {
    Log "4/5 Signing (certificate $thumbprint)"
    $timestamp = if ($env:SIGN_TIMESTAMP_URL) { $env:SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    # Only our own binaries; FFmpeg / OpenMP / onnxruntime / sherpa-onnx keep their vendors' signatures.
    $files = @(
        (Join-Path $AppDir 'SubtitleAdder.exe'),
        (Join-Path $AppDir 'subtitle-engine\subtitle-engine.exe')
    ) | Where-Object { Test-Path $_ }
    & $signtool sign /sha1 $thumbprint /fd SHA256 /tr $timestamp /td SHA256 /q @files
    & $signtool verify /pa /q @files
}
elseif ($thumbprint) {
    Log '4/5 Signing skipped (SIGN_CERT_THUMBPRINT is set but signtool was not found)'
}
else {
    Log '4/5 Signing skipped (SIGN_CERT_THUMBPRINT not set)'
}

Log 'Verifying'
# Import names are plain ASCII in the PE import table.
$exeText = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes((Join-Path $AppDir 'SubtitleAdder.exe')))
if ($exeText -match '(?i)vcruntime140(_1)?\.dll') {
    throw 'SubtitleAdder.exe imports the VC++ runtime (VCRUNTIME140.dll); it must be built with the static CRT.'
}
$bundledEngine = Join-Path $AppDir 'subtitle-engine\subtitle-engine.exe'
$line = @(& $bundledEngine models --json)[0]
$listing = $line | ConvertFrom-Json
if ($listing.event -ne 'models' -or $listing.models.Count -lt 1) { throw "Unexpected output from the bundled engine: $line" }
Write-Host "Bundled engine lists $($listing.models.Count) models."
# The engine keeps its own (GPL, libx264) FFmpeg: its avcodec must not be the app's LGPL one.
$engineAvcodec = Join-Path $AppDir 'subtitle-engine\avcodec-62.dll'
if ((Test-Path $engineAvcodec) -and
    (Get-FileHash -LiteralPath $engineAvcodec).Hash -eq (Get-FileHash -LiteralPath (Join-Path $AppDir 'avcodec-62.dll')).Hash) {
    throw 'subtitle-engine\avcodec-62.dll is the LGPL build; the engine needs its GPL FFmpeg (libx264 for preview).'
}
# The app's native stack must load from the package alone (no FFmpeg on PATH).
$savedPath = $env:PATH
try {
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $probeOut = Join-Path $Build 'probe.txt'
    $probe = Start-Process (Join-Path $AppDir 'SubtitleAdder.exe') -ArgumentList '--probe' -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $probeOut
    if ($probe.ExitCode -ne 0) { throw "SubtitleAdder.exe --probe failed (exit code $($probe.ExitCode)); a runtime DLL is missing." }
    $probeText = (Get-Content $probeOut -Raw).Trim()
    Write-Host $probeText
    if ($probeText -notmatch '(?m)^ffmpeg license: LGPL') { throw 'SubtitleAdder.exe did not load an LGPL FFmpeg build.' }
    Remove-Item $probeOut -ErrorAction SilentlyContinue
}
finally {
    $env:PATH = $savedPath
}
if ($SmokeTest -and -not $env:GITHUB_ACTIONS) {
    $app = Start-Process (Join-Path $AppDir 'SubtitleAdder.exe') -PassThru
    if ($app.WaitForExit(8000)) { throw "SubtitleAdder.exe exited at startup (code $($app.ExitCode))." }
    $app.Kill($true)
    Write-Host 'App starts.'
}

if ($Zip) {
    Log '5/5 Zip'
    $zipPath = Join-Path $Build "SubtitleAdder-$Version-win-x64.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory($AppDir, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    Write-Host "Zip: $zipPath ($([math]::Round((Get-Item $zipPath).Length / 1MB)) MB)"
}

Log "Done: $AppDir"
foreach ($dir in $AppDir, (Join-Path $AppDir 'subtitle-engine')) {
    $mb = (Get-ChildItem $dir -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
    Write-Host ('{0,6:N0} MB  {1}' -f $mb, $dir)
}
