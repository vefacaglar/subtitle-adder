#Requires -Version 7
<#
.SYNOPSIS
Packages Subtitle Adder for Windows: the WinUI 3 app (self-contained) + the Rust engine folder.

.DESCRIPTION
    pwsh scripts/package-windows.ps1 [-SkipEngineBuild] [-SmokeTest] [-Zip] [-Version 0.2.0]

Output: build\Subtitle Adder\SubtitleAdder.exe with build\Subtitle Adder\subtitle-engine\subtitle-engine.exe next to
it (EngineCommand.Locate's packaged path), plus the engine's runtime DLLs (FFmpeg, OpenMP, ONNX Runtime, sherpa-onnx)
staged by engine\scripts\stage-runtime.ps1. Builds for win-x64 only. Needs cargo (PATH or %USERPROFILE%\.cargo\bin)
unless the engine build is skipped.

Environment variables:
  SKIP_ENGINE_BUILD     1 = same as -SkipEngineBuild.
  VERSION               Version (default: <Version> of SubtitleAdder.csproj); -Version wins.
  SIGN_CERT_THUMBPRINT  SHA-1 thumbprint of a code-signing certificate in the certificate store. If set and signtool
                        is found (PATH or the Windows SDK), the app's and the engine's own binaries are Authenticode
                        signed; otherwise signing is skipped.
  SIGN_TIMESTAMP_URL    RFC 3161 timestamp server (default: http://timestamp.digicert.com).

.PARAMETER SkipEngineBuild
Skips fetch-runtime / cargo build / stage-runtime and reuses an already staged engine: build\engine-dist\subtitle-engine
if present, else engine\dist (like SKIP_ENGINE_BUILD=1 on macOS).

.PARAMETER SmokeTest
Launches SubtitleAdder.exe briefly to verify startup.

.PARAMETER Zip
Also writes build\SubtitleAdder-<version>-win-x64.zip.
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
$Build = Join-Path $Root 'build'
$AppDir = Join-Path $Build 'Subtitle Adder'
$Publish = Join-Path $Build 'app-publish'
$EngineDist = Join-Path $Build 'engine-dist\subtitle-engine'
$EngineExe = Join-Path $EngineDist 'subtitle-engine.exe'
$AppProject = Join-Path $Root 'apps\windows\src\SubtitleAdder\SubtitleAdder.csproj'
$LocateCheck = Join-Path $Root 'scripts\windows\LocateCheck\LocateCheck.csproj'

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
    # Newest SDK first (bin\10.0.26100.0\x64\signtool.exe).
    return Get-ChildItem $kits -Directory -Filter '10.*' |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}

if (-not $Version) { $Version = $env:VERSION }
if (-not $Version) { $Version = ([xml](Get-Content $AppProject)).Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1 }
if (-not $Version) { $Version = '0.1.0' }
# Assembly versions must be numeric (0.2.0-rc1 -> 0.2.0); the full string stays the informational version.
$NumericVersion = ($Version -split '[-+]')[0]

Log "1/5 Publishing app (dotnet publish, Release, win-x64, self-contained, $Version)"
if (Test-Path $Publish) { Remove-Item $Publish -Recurse -Force }
dotnet publish $AppProject -c Release -r win-x64 --self-contained true -o $Publish `
    -p:Version=$NumericVersion -p:InformationalVersion=$Version -nologo
if (-not (Test-Path (Join-Path $Publish 'SubtitleAdder.exe'))) { throw "dotnet publish produced no SubtitleAdder.exe in $Publish." }
# An unpackaged WinUI publish (.NET 10 + WinAppSDK 1.8) leaves out the app's own SubtitleAdder.pri and compiled XAML
# (*.xbf), which the build output has; without them the app crashes in Microsoft.UI.Xaml.dll at startup (0xc000027b).
$outDir = dotnet msbuild $AppProject -nologo -getProperty:OutDir -p:Configuration=Release -p:RuntimeIdentifier=win-x64
$outDir = Join-Path (Split-Path $AppProject) $outDir.Trim()
Get-ChildItem $outDir -Recurse -File -Include *.pri, *.xbf | ForEach-Object {
    $target = Join-Path $Publish ([IO.Path]::GetRelativePath($outDir, $_.FullName))
    if (-not (Test-Path $target)) {
        New-Item -ItemType Directory -Force (Split-Path $target) | Out-Null
        Copy-Item $_.FullName $target
    }
}
if (-not (Test-Path (Join-Path $Publish 'SubtitleAdder.pri'))) { throw "SubtitleAdder.pri is missing from $Publish." }

if ($env:SKIP_ENGINE_BUILD -eq '1') { $SkipEngineBuild = $true }
$EngineDir = Join-Path $Root 'engine'
if ($SkipEngineBuild) {
    if (-not (Test-Path $EngineExe)) {
        # No previous package build: reuse the dev engine staged by stage-runtime.ps1.
        $EngineDist = Join-Path $EngineDir 'dist'
        $EngineExe = Join-Path $EngineDist 'subtitle-engine.exe'
    }
    if (-not (Test-Path $EngineExe)) { throw "-SkipEngineBuild: no staged engine in build\engine-dist\subtitle-engine or engine\dist." }
    Log "2/5 Engine build skipped (reusing $EngineDist)"
}
else {
    Log '2/5 Building engine (fetch-runtime, cargo build --release, stage-runtime)'
    $cargo = Find-Cargo
    if (-not $cargo) { throw 'cargo not found; install Rust (https://rustup.rs/) or add cargo to PATH.' }
    # Downloads the sherpa-onnx / FFmpeg runtimes into engine\.runtime for the cargo build and stage-runtime.ps1.
    & (Join-Path $EngineDir 'scripts\fetch-runtime.ps1')
    # The FFmpeg bindings link against FFMPEG_DIR at build time; default to the fetched SDK.
    $fetchedFFmpeg = Join-Path $EngineDir '.runtime\ffmpeg'
    if (-not $env:FFMPEG_DIR -and (Test-Path (Join-Path $fetchedFFmpeg 'bin'))) { $env:FFMPEG_DIR = $fetchedFFmpeg }
    # Toolchain: when MSVC's cl.exe is not on PATH (not a VS Developer shell), dot-source a machine-local setup script:
    # $env:SUBTITLE_ENGINE_DEVENV, else %USERPROFILE%\.devtools\devenv.ps1 (for example a portable MSVC). Not in the repo.
    $devenv = if ($env:SUBTITLE_ENGINE_DEVENV) { $env:SUBTITLE_ENGINE_DEVENV } else { Join-Path $env:USERPROFILE '.devtools\devenv.ps1' }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -and (Test-Path $devenv)) { . $devenv }
    # Long target paths break cl.exe, so default to a short one outside the repo.
    if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.subtitle-adder-target' }
    Push-Location $EngineDir
    try {
        & $cargo build --release -p se-cli -j 2
    }
    finally {
        Pop-Location
    }
    # A fresh folder so DLLs from an older runtime never linger in the package.
    if (Test-Path $EngineDist) { Remove-Item $EngineDist -Recurse -Force }
    # stage-runtime.ps1 also smoke-tests `models --json` from the staged folder with a clean PATH.
    & (Join-Path $EngineDir 'scripts\stage-runtime.ps1') -Dist $EngineDist
    if (-not (Test-Path $EngineExe)) { throw "stage-runtime.ps1 produced no $EngineExe." }
}

Log '3/5 Assembling folder'
if (Test-Path $AppDir) { Remove-Item $AppDir -Recurse -Force }
Copy-Item $Publish $AppDir -Recurse
Copy-Item $EngineDist (Join-Path $AppDir 'subtitle-engine') -Recurse
# Debug symbols are not shipped.
Get-ChildItem $AppDir -Filter *.pdb -File | Remove-Item

$thumbprint = $env:SIGN_CERT_THUMBPRINT
$signtool = if ($thumbprint) { Find-SignTool } else { $null }
if ($thumbprint -and $signtool) {
    Log "4/5 Signing (certificate $thumbprint)"
    $timestamp = if ($env:SIGN_TIMESTAMP_URL) { $env:SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    # Only our own binaries: the two launchers and the app's managed assemblies. Third-party DLLs (WinAppSDK,
    # FFmpeg, OpenMP, onnxruntime, sherpa-onnx) keep their vendors' signatures.
    $files = @(
        (Join-Path $AppDir 'SubtitleAdder.exe'),
        (Join-Path $AppDir 'SubtitleAdder.dll'),
        (Join-Path $AppDir 'SubtitleAdder.Core.dll'),
        (Join-Path $AppDir 'SubtitleAdder.EngineKit.dll'),
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
$bundledEngine = Join-Path $AppDir 'subtitle-engine\subtitle-engine.exe'
# The packaged engine runs on its own (no cargo, no repo) and speaks protocol v1; `models` needs no network.
$line = @(& $bundledEngine models --json)[0]
$listing = $line | ConvertFrom-Json
if ($listing.event -ne 'models' -or $listing.models.Count -lt 1) { throw "Unexpected output from the bundled engine: $line" }
Write-Host "Bundled engine lists $($listing.models.Count) models."
# The app must pick the bundled engine, not the repo's engine\dist dev fallback (build\ is inside the repo).
$locateOut = Join-Path $Build 'locate-check'
dotnet build $LocateCheck -c Release -o $locateOut -nologo -v quiet
& (Join-Path $locateOut 'LocateCheck.exe') $AppDir
# Optional startup smoke test: launches the app for 8 seconds when -SmokeTest is given.
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
    # Top-level "Subtitle Adder\" folder inside the zip.
    [System.IO.Compression.ZipFile]::CreateFromDirectory($AppDir, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    Write-Host "Zip: $zipPath ($([math]::Round((Get-Item $zipPath).Length / 1MB)) MB)"
}

Log "Done: $AppDir"
foreach ($dir in $AppDir, (Join-Path $AppDir 'subtitle-engine')) {
    $mb = (Get-ChildItem $dir -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
    Write-Host ('{0,6:N0} MB  {1}' -f $mb, $dir)
}
