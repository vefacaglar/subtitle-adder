<#
.SYNOPSIS
Creates the end-to-end test data for the Rust desktop app in a scratch folder (never the user's real data):

    <Root>\media\*.mp4            short clips: a Turkish TTS clip (Windows SAPI voice) and two test-pattern clips
    <Root>\media\thumbs\*.png     frames used as hub thumbnails
    <Root>\data\                  a Windows data folder written by the C# app's own classes, frozen in
                                  crates\sa-app\tests\fixtures\csharp\e2e-data\data (see its README):
                                  profiles.json + profiles.db, settings.json, media-index.json,
                                  translation-providers.json

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/e2e-data.ps1 -Root <folder> [-Force]

The C# app recorded the clips at C:\SubtitleAdderE2E\template\media (profile rows are keyed by the clips' content
fingerprints, the media index by their paths), so -Root C:\SubtitleAdderE2E\template reproduces that data folder
exactly; any other root works too, with the C# history pointing at the recorded paths. The clips are regenerated here
and checked against the hashes the C# data was made from (media.sha256 next to the frozen data); their modification
times are set back to the ones in the frozen media index.

Run the app on a COPY of <Root>\data: set SUBTITLE_ADDER_DATA_DIR to that copy. Needs ffmpeg (FFMPEG_DIR\bin, default
engine\.runtime\ffmpeg). Windows PowerShell 5.1 compatible.
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

# 4. The data folder the C# app's own classes wrote (frozen; regenerated clips must match what they were made from).
$fixture = Join-Path $desktop 'crates\sa-app\tests\fixtures\csharp\e2e-data'
Copy-Item -Path (Join-Path $fixture 'data\*') -Destination $data -Recurse
$recorded = 'C:\SubtitleAdderE2E\template\media'
if ($media -ne $recorded) { Write-Warning "the C# data recorded the clips in $recorded; this run uses $media" }
foreach ($line in Get-Content (Join-Path $fixture 'media.sha256')) {
    $hash, $rel = $line -split '  ', 2
    $file = Join-Path $media $rel.Replace('/', '\')
    $actual = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
    if ($actual -ne $hash) {
        Write-Warning "$rel differs from the clip the C# data was made from (other ffmpeg build or TTS voice?): its content fingerprint will not match the C# profile rows"
    }
}
$index = Get-Content -Raw -Encoding UTF8 (Join-Path $data 'media-index.json') | ConvertFrom-Json
foreach ($entry in $index.directories[0].mediaFiles) {
    $file = Join-Path $media $entry.fileName
    $stamp = $entry.lastWriteTimeUtc
    # Windows PowerShell 5.1 keeps the string; PowerShell 7 already parses it into a DateTime.
    $utc = if ($stamp -is [datetime]) { $stamp.ToUniversalTime() } else {
        [DateTimeOffset]::Parse($stamp, [Globalization.CultureInfo]::InvariantCulture).UtcDateTime
    }
    (Get-Item $file).LastWriteTimeUtc = $utc
}
Write-Host "E2E data written to $Root"
