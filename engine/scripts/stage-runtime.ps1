<#
.SYNOPSIS
Stages the Rust engine as a standalone folder: subtitle-engine.exe plus every runtime DLL it loads.

.DESCRIPTION
    pwsh engine/scripts/stage-runtime.ps1 [-TargetDir <cargo target dir>] [-Profile release] [-Dist <folder>]

Copies into the dist folder (default engine\dist, gitignored):
  - subtitle-engine.exe              from <TargetDir>\<Profile>
  - libiomp5md.dll                   Intel OpenMP, copied next to the binary by se-asr's build script
  - FFmpeg shared DLLs               from $FFmpegDir\bin (avcodec, avformat, avutil, swresample, swscale, ...)
  - onnxruntime.dll                  shared by ort (se-asr) and sherpa-onnx, so one ONNX Runtime per process
  - sherpa-onnx-c-api.dll            sherpa-onnx C API (se-diarize, loaded with libloading)

The engine finds onnxruntime.dll and sherpa-onnx-c-api.dll next to the executable, and Windows resolves the FFmpeg
and OpenMP import DLLs from the executable's folder, so the dist folder runs without PATH or env setup.

.PARAMETER TargetDir
Cargo target dir (default: $env:CARGO_TARGET_DIR, else engine\target).

.PARAMETER FFmpegDir
FFmpeg 8.x shared SDK (default: $env:FFMPEG_DIR, else engine\.runtime\ffmpeg from fetch-runtime.ps1).

.PARAMETER SherpaDir
Folder holding sherpa-onnx-c-api.dll and onnxruntime.dll (default: $env:SUBTITLE_ENGINE_SHERPA_DIR, else
engine\.runtime\sherpa-onnx from fetch-runtime.ps1).
#>
[CmdletBinding()]
param(
    [string]$TargetDir,
    [string]$Profile = 'release',
    [string]$Dist,
    [string]$FFmpegDir = $env:FFMPEG_DIR,
    [string]$SherpaDir = $env:SUBTITLE_ENGINE_SHERPA_DIR
)

$ErrorActionPreference = 'Stop'
$EngineRs = Split-Path -Parent $PSScriptRoot

if (-not $TargetDir) { $TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $EngineRs 'target' } }
if (-not $Dist) { $Dist = Join-Path $EngineRs 'dist' }
$RuntimeDir = Join-Path $EngineRs '.runtime'
if (-not $SherpaDir) { $SherpaDir = Join-Path $RuntimeDir 'sherpa-onnx' }
if (-not $FFmpegDir -and (Test-Path (Join-Path $RuntimeDir 'ffmpeg\bin'))) { $FFmpegDir = Join-Path $RuntimeDir 'ffmpeg' }
if (-not $FFmpegDir) { throw 'FFmpeg SDK not found: pass -FFmpegDir, set FFMPEG_DIR or run engine\scripts\fetch-runtime.ps1.' }
if (-not (Test-Path (Join-Path $SherpaDir 'sherpa-onnx-c-api.dll'))) {
    throw "sherpa-onnx runtime not found in ${SherpaDir}: pass -SherpaDir, set SUBTITLE_ENGINE_SHERPA_DIR or run engine\scripts\fetch-runtime.ps1."
}

$Bin = Join-Path $TargetDir $Profile
$Exe = Join-Path $Bin 'subtitle-engine.exe'
if (-not (Test-Path $Exe)) { throw "subtitle-engine.exe not found in $Bin (run cargo build --release -p se-cli first)." }

function Copy-Required([string]$Path) {
    if (-not (Test-Path $Path -PathType Leaf)) { throw "Missing runtime file: $Path" }
    Copy-Item $Path -Destination $Dist -Force
    Write-Host "  $(Split-Path -Leaf $Path)"
}

New-Item -ItemType Directory -Force $Dist | Out-Null
Write-Host "Staging into $Dist"
Copy-Required $Exe
Copy-Required (Join-Path $Bin 'libiomp5md.dll')
$ffmpegDlls = Get-ChildItem (Join-Path $FFmpegDir 'bin') -Filter '*.dll'
if (-not $ffmpegDlls) { throw "No FFmpeg DLLs in $FFmpegDir\bin" }
foreach ($dll in $ffmpegDlls) { Copy-Required $dll.FullName }
# GPL redistribution: the build's license text and a notice of what the DLLs are and where their source is.
. (Join-Path $PSScriptRoot 'ffmpeg-notice.ps1')
Write-FFmpegNotice -FFmpegDir $FFmpegDir -To $Dist -Kind GPL
Copy-Required (Join-Path $SherpaDir 'onnxruntime.dll')
Copy-Required (Join-Path $SherpaDir 'sherpa-onnx-c-api.dll')

# Smoke test with a clean PATH: the binary must start from the dist folder alone.
$savedPath = $env:PATH
try {
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $out = & (Join-Path $Dist 'subtitle-engine.exe') models --json
    if ($LASTEXITCODE -ne 0) { throw "Smoke test failed: subtitle-engine models --json exited with $LASTEXITCODE" }
    if ($out -notmatch '"event": "models"') { throw "Smoke test failed: unexpected output: $out" }
} finally {
    $env:PATH = $savedPath
}
Write-Host 'Smoke test passed (models --json with a clean PATH).'
