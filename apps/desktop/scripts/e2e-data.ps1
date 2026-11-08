<#
.SYNOPSIS
Creates the end-to-end test data for the Rust desktop app in a scratch folder (never the user's real data):

    <Root>\media\*.mp4            short clips: a Turkish TTS clip (Windows SAPI voice) and two test-pattern clips
    <Root>\media\thumbs\*.png     frames used as hub thumbnails
    <Root>\data\                  a Windows data folder written by the C# app's own code (tools\fixtures\E2EData):
                                  profiles.json + profiles.db, settings.json, media-index.json,
                                  translation-providers.json

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/e2e-data.ps1 -Root <folder> [-Force]

Run the app on a COPY of <Root>\data: set SUBTITLE_ADDER_DATA_DIR to that copy. Needs ffmpeg (FFMPEG_DIR\bin, default
engine\.runtime\ffmpeg) and the .NET 10 SDK. Broker files and logs of the C# tool go to <Root>\tmp.
Windows PowerShell 5.1 compatible.
#>
param([Parameter(Mandatory = $true)][string]$Root, [switch]$Force)
$ErrorActionPreference = 'Stop'

$desktop = Split-Path $PSScriptRoot -Parent
$repo = Split-Path (Split-Path $desktop -Parent) -Parent
if (-not $env:FFMPEG_DIR) { $env:FFMPEG_DIR = Join-Path $repo 'engine\.runtime\ffmpeg' }
$ffmpeg = Join-Path $env:FFMPEG_DIR 'bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) { throw "ffmpeg not found at $ffmpeg (run engine\scripts\fetch-runtime.ps1 -Only ffmpeg)" }

$Root = [IO.Path]::GetFullPath($Root)
$media = Join-Path $Root 'media'
$thumbs = Join-Path $media 'thumbs'
$data = Join-Path $Root 'data'
$tmp = Join-Path $Root 'tmp'
if ($Force -and (Test-Path $Root)) { Remove-Item -Recurse -Force $Root }
if (Test-Path (Join-Path $data 'profiles.json')) {
    Write-Host "E2E data already exists in $Root (use -Force to recreate)."
    exit 0
}
New-Item -ItemType Directory -Force -Path $media, $thumbs, $data, $tmp | Out-Null

function Invoke-Ffmpeg([string[]]$arguments) {
    & $ffmpeg -hide_banner -loglevel error -y @arguments
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed: $arguments" }
}

# 1. A short Turkish speech clip from the Windows SAPI voice (Tolga when installed, else the default voice).
Add-Type -AssemblyName System.Speech
$speech = New-Object System.Speech.Synthesis.SpeechSynthesizer
$voice = $speech.GetInstalledVoices() | Where-Object { $_.Enabled -and $_.VoiceInfo.Culture.Name -eq 'tr-TR' } | Select-Object -First 1
if ($voice) { $speech.SelectVoice($voice.VoiceInfo.Name) }
$wav = Join-Path $tmp 'speech.wav'
$speech.SetOutputToWaveFile($wav)
$speech.Speak('Merhaba, bu bir altyazı testidir. Bugün hava çok güzel. Sen ne yapacaksın?')
$speech.Dispose()
$clip = Join-Path $media 'altyazi-testi.mp4'
Invoke-Ffmpeg @('-f', 'lavfi', '-i', 'color=c=0x1f3a5f:s=640x360:r=25', '-i', $wav, '-shortest',
    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', $clip)

# 2. Two silent test-pattern clips (hub history and lists).
Invoke-Ffmpeg @('-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=25:d=8', '-f', 'lavfi', '-i', 'sine=f=440:d=8',
    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-shortest', (Join-Path $media 'belgesel-uzay.mp4'))
Invoke-Ffmpeg @('-f', 'lavfi', '-i', 'smptebars=s=640x360:r=25:d=6', '-f', 'lavfi', '-i', 'sine=f=660:d=6',
    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-shortest', (Join-Path $media 'yaz-tatili.mp4'))

# 3. Thumbnails (PNG, what the C# app stores from the shell thumbnail provider).
foreach ($name in 'belgesel-uzay', 'yaz-tatili') {
    Invoke-Ffmpeg @('-ss', '2', '-i', (Join-Path $media "$name.mp4"), '-frames:v', '1', '-vf', 'scale=240:-2',
        (Join-Path $thumbs "$name.png"))
}

# 4. The data folder, written by the C# app's own classes.
$tool = Join-Path $desktop 'tools\fixtures\E2EData'
$keepTmp = $env:TMP; $keepTemp = $env:TEMP
$env:TMP = $tmp; $env:TEMP = $tmp
try {
    & dotnet run --project $tool -c Release -- make $data $media
    if ($LASTEXITCODE -ne 0) { throw "E2EData make failed (exit $LASTEXITCODE)" }
} finally {
    $env:TMP = $keepTmp; $env:TEMP = $keepTemp
}
Write-Host "E2E data written to $Root"
