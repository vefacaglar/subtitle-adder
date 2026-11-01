#Requires -Version 7
<#
.SYNOPSIS
Packages Subtitle Adder for Windows: the WinUI 3 app (self-contained) + the PyInstaller (onedir) engine.

.DESCRIPTION
    pwsh scripts/package-windows.ps1 [-SkipEngineBuild] [-NoSmokeTest] [-Zip] [-Version 0.2.0]

Output: build\Subtitle Adder\SubtitleAdder.exe with build\Subtitle Adder\subtitle-engine\subtitle-engine.exe next to
it (EngineCommand.Locate's packaged path). Builds for win-x64 only.

Environment variables:
  VERSION               Version (default: <Version> of SubtitleAdder.csproj); -Version wins.
  SIGN_CERT_THUMBPRINT  SHA-1 thumbprint of a code-signing certificate in the certificate store. If set and signtool
                        is found (PATH or the Windows SDK), the app's and the engine's own binaries are Authenticode
                        signed; otherwise signing is skipped.
  SIGN_TIMESTAMP_URL    RFC 3161 timestamp server (default: http://timestamp.digicert.com).

.PARAMETER SkipEngineBuild
Reuses build\engine-dist\subtitle-engine if it is already built (like SKIP_ENGINE_BUILD=1 on macOS).

.PARAMETER NoSmokeTest
Skips the 8-second GUI startup smoke test (useful when VS Code launches SubtitleAdder.exe immediately afterwards).

.PARAMETER Zip
Also writes build\SubtitleAdder-<version>-win-x64.zip.
#>
[CmdletBinding()]
param(
    [switch]$SkipEngineBuild,
    [switch]$NoSmokeTest,
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

# Same places EngineCommand.UvCandidates looks, so a machine that runs the dev app can also package it.
function Find-Uv {
    $onPath = Get-Command uv -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    $candidates = @()
    if ($env:USERPROFILE) {
        $candidates += Join-Path $env:USERPROFILE '.local\bin\uv.exe'
        $candidates += Join-Path $env:USERPROFILE '.cargo\bin\uv.exe'
    }
    if ($env:APPDATA -and (Test-Path (Join-Path $env:APPDATA 'Python'))) {
        $candidates += Get-ChildItem (Join-Path $env:APPDATA 'Python') -Directory -Filter 'Python3*' |
            Sort-Object { [int]($_.Name.Substring('Python'.Length) -replace '\D', '') } -Descending |
            ForEach-Object { Join-Path $_.FullName 'Scripts\uv.exe' }
    }
    return $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
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

if (-not $SkipEngineBuild -or -not (Test-Path $EngineExe)) {
    Log '2/5 Building engine (PyInstaller onedir)'
    $uv = Find-Uv
    if (-not $uv) { throw 'uv not found; install it (https://docs.astral.sh/uv/) or add it to PATH.' }
    # A separate environment with only the runtime deps + PyInstaller: the dev engine\.venv (pytest, cuda group)
    # is left untouched and nothing from it leaks into the package.
    $env:UV_PROJECT_ENVIRONMENT = Join-Path $Build 'engine-venv'
    Push-Location (Join-Path $Root 'engine')
    try {
        & $uv sync --locked --no-default-groups --group build --quiet
        & $uv run --no-sync pyinstaller subtitle-engine.spec --noconfirm --log-level WARN `
            --distpath (Join-Path $Build 'engine-dist') --workpath (Join-Path $Build 'engine-work')
    }
    finally {
        Pop-Location
        Remove-Item Env:UV_PROJECT_ENVIRONMENT -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path $EngineExe)) { throw "PyInstaller produced no $EngineExe." }
}
else {
    Log '2/5 Engine build skipped'
}

# Seed the local CUDA runtime cache from engine\.venv if available so the packaged app uses the GPU immediately
# without having to re-download the wheels on this machine.
$devNvidia = Join-Path $Root 'engine\.venv\Lib\site-packages\nvidia'
if ($env:LOCALAPPDATA -and (Test-Path $devNvidia)) {
    $cudaCacheNvidia = Join-Path $env:LOCALAPPDATA 'SubtitleAdder\cuda\nvidia'
    Get-ChildItem $devNvidia -Directory | ForEach-Object {
        $srcBin = Join-Path $_.FullName 'bin'
        if (Test-Path $srcBin) {
            $dstBin = Join-Path $cudaCacheNvidia "$($_.Name)\bin"
            New-Item -ItemType Directory -Force $dstBin | Out-Null
            Get-ChildItem $srcBin -Filter *.dll -File | ForEach-Object {
                $dstFile = Join-Path $dstBin $_.Name
                if (-not (Test-Path $dstFile)) { Copy-Item $_.FullName $dstFile }
            }
        }
    }
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
    # onnxruntime, Python) keep their vendors' signatures.
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
# The packaged engine runs on its own (no uv, no repo) and speaks protocol v1; `models` needs no network.
$line = @(& $bundledEngine models --json)[0]
$listing = $line | ConvertFrom-Json
if ($listing.event -ne 'models' -or $listing.models.Count -lt 1) { throw "Unexpected output from the bundled engine: $line" }
Write-Host "Bundled engine lists $($listing.models.Count) models."
# The app must pick the bundled engine, not the repo's `uv run` fallback (build\ is inside the repo).
$locateOut = Join-Path $Build 'locate-check'
dotnet build $LocateCheck -c Release -o $locateOut -nologo -v quiet
& (Join-Path $locateOut 'LocateCheck.exe') $AppDir
# Startup smoke test: a publish that misses XAML resources crashes within a second or two. Skipped on CI runners,
# which may have no interactive desktop, or when -NoSmokeTest is passed (e.g. VS Code preLaunchTask).
if (-not $env:GITHUB_ACTIONS -and -not $NoSmokeTest) {
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
