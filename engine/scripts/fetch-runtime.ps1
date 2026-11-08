<#
.SYNOPSIS
Downloads the pinned prebuilt native runtimes the Rust engine needs on Windows into engine\.runtime (gitignored).

.DESCRIPTION
    powershell -File engine/scripts/fetch-runtime.ps1 [-Only all|sherpa|ffmpeg|ffmpeg-lgpl] [-Force]

  - engine\.runtime\sherpa-onnx   sherpa-onnx 2.0.1 C API (sherpa-onnx-c-api.dll) and its onnxruntime.dll
                                  (k2-fsa/sherpa-onnx release asset win-x64-shared-MT-Release-lib: static CRT, the
                                  same onnxruntime.dll as the sherpa-onnx 2.0.1 Python wheel). se-diarize's FFI
                                  structs follow this exact version.
  - engine\.runtime\ffmpeg        FFmpeg 8.1 shared SDK (bin\*.dll, include, lib) from a dated BtbN autobuild
                                  (month-end autobuilds stay published long term; "latest" assets move). The GPL
                                  build: the engine's `preview` command encodes H.264 with libx264.
  - engine\.runtime\ffmpeg-lgpl   -Only ffmpeg-lgpl: the LGPL shared build of the same autobuild (no libx264 or
                                  other GPL parts), an extra component that is never installed instead of the GPL
                                  SDK. Only the desktop app ships it: its player only decodes, so
                                  apps\desktop\scripts\stage.ps1 and scripts\package-desktop-windows.ps1 put these
                                  DLLs (with the build's LICENSE.txt) next to SubtitleAdder.exe while the engine
                                  folder keeps the GPL ones. Same API and DLL names, so the app still compiles
                                  against the GPL SDK in FFMPEG_DIR.

`-Only all` (the default) installs sherpa-onnx and the GPL FFmpeg SDK, which every build needs. FFMPEG_DIR (and
GITHUB_PATH) always name the GPL SDK; the LGPL folder is printed (and exported to GITHUB_ENV) as FFMPEG_LGPL_DIR.

Every archive is checked against a pinned SHA-256 before it is extracted. A component whose folder already holds
the stamp of the pinned hash is skipped (use -Force to download again).

The engine's debug builds find engine\.runtime\sherpa-onnx on their own; stage-runtime.ps1 falls back to both
folders when SUBTITLE_ENGINE_SHERPA_DIR / FFMPEG_DIR are unset. For cargo, set FFMPEG_DIR to the printed folder
and put its bin on PATH (tests load the FFmpeg DLLs). On GitHub Actions this script does both through
GITHUB_ENV / GITHUB_PATH.
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'sherpa', 'ffmpeg', 'ffmpeg-lgpl')]
    [string]$Only = 'all',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Invoke-WebRequest is very slow with the progress bar (PS 5.1).
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Engine = Split-Path -Parent $PSScriptRoot
$Runtime = Join-Path $Engine '.runtime'

$Components = @(
    @{
        Name   = 'sherpa'
        Dir    = 'sherpa-onnx'
        Url    = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-win-x64-shared-MT-Release-lib.tar.bz2'
        Sha256 = 'afa4de3ca4ab0afd53bb4aab40d28ee15608364b549ffc0fe0caf6489bfc39a5'
        Inner  = 'sherpa-onnx-v2.0.1-win-x64-shared-MT-Release-lib\lib'
        Check  = @('sherpa-onnx-c-api.dll', 'onnxruntime.dll')
    },
    @{
        Name   = 'ffmpeg'
        Dir    = 'ffmpeg'
        Url    = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59/ffmpeg-n8.1.3-27-g8dca1586f4-win64-gpl-shared-8.1.zip'
        Sha256 = '3adc00b67118a8c4bdd6624a000e139df283b1eeb8b126fcec4849bd5b778dd4'
        Inner  = 'ffmpeg-n8.1.3-27-g8dca1586f4-win64-gpl-shared-8.1'
        Check  = @('bin\avcodec-62.dll', 'bin\avformat-62.dll', 'include\libavcodec\avcodec.h', 'lib\avcodec.lib')
    },
    @{
        Name   = 'ffmpeg-lgpl'
        # Not part of `-Only all`: requested by the app's stage / package scripts.
        Extra  = $true
        Dir    = 'ffmpeg-lgpl'
        Url    = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59/ffmpeg-n8.1.3-27-g8dca1586f4-win64-lgpl-shared-8.1.zip'
        Sha256 = '5cef7bc48e513f3cee7c881e8b2999d835541edb1846c3e89443a67f8f8556ab'
        Inner  = 'ffmpeg-n8.1.3-27-g8dca1586f4-win64-lgpl-shared-8.1'
        Check  = @('bin\avcodec-62.dll', 'bin\avformat-62.dll', 'bin\avutil-60.dll', 'bin\swresample-6.dll', 'bin\swscale-9.dll', 'LICENSE.txt')
    }
)

# Windows' bsdtar handles .zip and .tar.bz2 and drive-letter paths (Git's GNU tar on PATH does not).
$Tar = Join-Path $env:SystemRoot 'System32\tar.exe'
if (-not (Test-Path $Tar)) { throw "tar.exe not found at $Tar (Windows 10 1803+ ships it)." }

function Get-Sha256([string]$Path) {
    # .NET instead of Get-FileHash: works even when 5.1 inherited pwsh 7's PSModulePath.
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try { return ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}

function Install-Component($c) {
    if ($c.Sha256 -notmatch '^[0-9a-f]{64}$') {
        throw "$($c.Dir): no SHA-256 pinned yet for $($c.Url); download it once, verify it and pin its hash here."
    }
    $dest = Join-Path $Runtime $c.Dir
    $stamp = Join-Path $dest '.sha256'
    $complete = (Test-Path $stamp) -and ((Get-Content -Raw $stamp).Trim() -eq $c.Sha256) -and
        -not ($c.Check | Where-Object { -not (Test-Path (Join-Path $dest $_)) })
    if ($complete -and -not $Force) {
        Write-Host "$($c.Name): up to date ($dest)"
        return $dest
    }

    $work = Join-Path $Runtime ".download-$($c.Dir)"
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
    New-Item -ItemType Directory -Force $work | Out-Null
    try {
        $archive = Join-Path $work ([IO.Path]::GetFileName(([Uri]$c.Url).AbsolutePath))
        Write-Host "$($c.Name): downloading $($c.Url)"
        Invoke-WebRequest -Uri $c.Url -OutFile $archive -UseBasicParsing
        $actual = Get-Sha256 $archive
        if ($actual -ne $c.Sha256) { throw "$($c.Name): SHA-256 mismatch for $archive`n  expected $($c.Sha256)`n  actual   $actual" }

        $extract = Join-Path $work 'x'
        New-Item -ItemType Directory -Force $extract | Out-Null
        & $Tar -xf $archive -C $extract
        if ($LASTEXITCODE -ne 0) { throw "$($c.Name): extracting $archive failed (tar exit $LASTEXITCODE)" }
        $inner = Join-Path $extract $c.Inner
        foreach ($f in $c.Check) {
            if (-not (Test-Path (Join-Path $inner $f))) { throw "$($c.Name): $f missing from $($c.Url)" }
        }

        if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
        Move-Item $inner $dest
        Set-Content -NoNewline -Encoding ascii -Path $stamp -Value $c.Sha256
        Write-Host "$($c.Name): installed into $dest"
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
    return $dest
}

New-Item -ItemType Directory -Force $Runtime | Out-Null
$installed = @{}
foreach ($c in $Components) {
    if (($Only -eq 'all' -and -not $c.Extra) -or $Only -eq $c.Name) { $installed[$c.Dir] = Install-Component $c }
}

if ($installed.ContainsKey('sherpa-onnx')) { Write-Host "SUBTITLE_ENGINE_SHERPA_DIR=$($installed['sherpa-onnx'])" }
if ($installed.ContainsKey('ffmpeg')) { Write-Host "FFMPEG_DIR=$($installed['ffmpeg'])" }
if ($installed.ContainsKey('ffmpeg-lgpl')) { Write-Host "FFMPEG_LGPL_DIR=$($installed['ffmpeg-lgpl'])" }

# GitHub Actions: export the folders to later steps (UTF-8 without BOM; PS 5.1's Out-File -Encoding utf8 adds one).
function Add-Line([string]$File, [string]$Line) { [IO.File]::AppendAllText($File, "$Line`n", (New-Object Text.UTF8Encoding $false)) }
if ($env:GITHUB_ENV) {
    if ($installed.ContainsKey('sherpa-onnx')) { Add-Line $env:GITHUB_ENV "SUBTITLE_ENGINE_SHERPA_DIR=$($installed['sherpa-onnx'])" }
    if ($installed.ContainsKey('ffmpeg')) {
        Add-Line $env:GITHUB_ENV "FFMPEG_DIR=$($installed['ffmpeg'])"
        Add-Line $env:GITHUB_PATH (Join-Path $installed['ffmpeg'] 'bin')
    }
    if ($installed.ContainsKey('ffmpeg-lgpl')) { Add-Line $env:GITHUB_ENV "FFMPEG_LGPL_DIR=$($installed['ffmpeg-lgpl'])" }
}
