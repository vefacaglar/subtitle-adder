<#
.SYNOPSIS
Writes the FFmpeg license files next to shipped FFmpeg DLLs (dot-source it, then call Write-FFmpegNotice).

.DESCRIPTION
FFmpeg's shared libraries may be redistributed under the (L)GPL only together with the license text and a notice of
what they are and where their source code is. Write-FFmpegNotice copies the build's own LICENSE.txt (BtbN builds
ship it at the archive root) as FFmpeg-LICENSE.txt and writes FFmpeg-NOTICE.txt. Throws when the build has no
license text, so nothing ships without it.
#>

function Write-FFmpegNotice {
    param(
        [Parameter(Mandatory = $true)][string]$FFmpegDir,
        [Parameter(Mandatory = $true)][string]$To,
        # 'LGPL' (the desktop app's player) or 'GPL' (the engine: libx264 for `preview`).
        [Parameter(Mandatory = $true)][ValidateSet('LGPL', 'GPL')][string]$Kind
    )
    $license = Join-Path $FFmpegDir 'LICENSE.txt'
    if (-not (Test-Path $license)) { throw "FFmpeg build in $FFmpegDir has no LICENSE.txt; refusing to ship its DLLs without it." }
    Copy-Item -LiteralPath $license -Destination (Join-Path $To 'FFmpeg-LICENSE.txt') -Force
    $name = if ($Kind -eq 'LGPL') { 'GNU Lesser General Public License (LGPL), version 3 or later' } else { 'GNU General Public License (GPL), version 3 or later' }
    $build = ''
    $stamp = Join-Path $FFmpegDir '.sha256'
    if (Test-Path $stamp) { $build = "Archive SHA-256: $((Get-Content -Raw $stamp).Trim())`r`n" }
    $text = @"
FFmpeg

The FFmpeg DLLs in this folder (avcodec, avformat, avutil, swresample, swscale and the others shipped with them)
are unmodified shared libraries of the FFmpeg project (https://ffmpeg.org), licensed under the
$name. The full license text is in FFmpeg-LICENSE.txt.

They come from the FFmpeg-Builds autobuilds (https://github.com/BtbN/FFmpeg-Builds/releases), FFmpeg 8.1.
$build
Source code: the FFmpeg source of that release is at https://git.ffmpeg.org/ffmpeg.git (mirror:
https://github.com/FFmpeg/FFmpeg), and the scripts that configured and built these binaries, including every
third-party library they contain, are at https://github.com/BtbN/FFmpeg-Builds. The application links to them
dynamically: they can be replaced with a compatible build of the same FFmpeg version.
"@
    Set-Content -LiteralPath (Join-Path $To 'FFmpeg-NOTICE.txt') -Value $text -Encoding ascii
}
