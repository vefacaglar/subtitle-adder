# Generates Assets/AppIcon.ico (multi-size, PNG entries) and Assets/AppIcon.png (title bar) for the Windows app.
# Same design as the macOS icon (scripts/macos/make-icon.swift): a white squircle body, a sound wave inside a
# purple speech bubble, with two subtitle lines below. Drawn once on the 1024 grid, then scaled down per size.
# The outputs are committed; re-run only when the design changes:  pwsh apps/windows/src/SubtitleAdder/Assets/make-icon.ps1
Add-Type -AssemblyName System.Drawing

$here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Color([int]$hex, [double]$alpha = 1) {
    [System.Drawing.Color]::FromArgb([int]($alpha * 255), ($hex -shr 16) -band 0xFF, ($hex -shr 8) -band 0xFF, $hex -band 0xFF)
}

function RoundedRect([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc([single]$x, [single]$y, [single]$d, [single]$d, 180, 90)
    $p.AddArc([single]($x + $w - $d), [single]$y, [single]$d, [single]$d, 270, 90)
    $p.AddArc([single]($x + $w - $d), [single]($y + $h - $d), [single]$d, [single]$d, 0, 90)
    $p.AddArc([single]$x, [single]($y + $h - $d), [single]$d, [single]$d, 90, 90)
    $p.CloseFigure()
    $p
}

# The "squircle" (superellipse, n=5) body from the macOS icon grid.
function Squircle([double]$x, [double]$y, [double]$w, [double]$h, [double]$n = 5) {
    $a = $w / 2; $b = $h / 2; $mx = $x + $a; $my = $y + $b
    $steps = 720
    $pts = for ($i = 0; $i -lt $steps; $i++) {
        $t = $i / $steps * 2 * [Math]::PI
        $c = [Math]::Cos($t); $s = [Math]::Sin($t)
        $px = $mx + $a * [Math]::Sign($c) * [Math]::Pow([Math]::Abs($c), 2 / $n)
        $py = $my + $b * [Math]::Sign($s) * [Math]::Pow([Math]::Abs($s), 2 / $n)
        New-Object System.Drawing.PointF ([single]$px), ([single]$py)
    }
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddPolygon([System.Drawing.PointF[]]$pts)
    $p
}

function New-MasterIcon {
    # All measurements are on the 1024 grid, measured from the top (as in make-icon.swift).
    $bmp = New-Object System.Drawing.Bitmap 1024, 1024, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    # Body: white, with a light shadow (offset 10, blur ~24, 25% black), approximated by stacked soft strokes.
    $body = Squircle 100 100 824 824
    $shadow = $body.Clone()
    $m = New-Object System.Drawing.Drawing2D.Matrix
    $m.Translate(0, 10)
    $shadow.Transform($m)
    $layers = 12
    for ($k = $layers; $k -ge 1; $k--) {
        $pen = New-Object System.Drawing.Pen (Color 0x000000 (0.25 / $layers)), ([single](4 * $k))
        $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        $g.DrawPath($pen, $shadow)
        $pen.Dispose()
    }
    $g.FillPath((New-Object System.Drawing.SolidBrush (Color 0x000000 0.25)), $shadow)
    $g.FillPath((New-Object System.Drawing.SolidBrush (Color 0xFFFFFF)), $body)

    # Speech bubble: rounded rectangle + a tail at the bottom left.
    $accent = New-Object System.Drawing.SolidBrush (Color 0x5B4BDB)
    $bx = 232; $by = 232; $bw = 560; $bh = 360
    $g.FillPath($accent, (RoundedRect $bx $by $bw $bh 112))
    $tail = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p0 = New-Object System.Drawing.PointF 292, 668
    $q = New-Object System.Drawing.PointF 360, 644
    $p2 = New-Object System.Drawing.PointF 448, 576
    # Quadratic curve (p0, q, p2) as a cubic Bézier.
    $c1 = New-Object System.Drawing.PointF ($p0.X + 2 / 3 * ($q.X - $p0.X)), ($p0.Y + 2 / 3 * ($q.Y - $p0.Y))
    $c2 = New-Object System.Drawing.PointF ($p2.X + 2 / 3 * ($q.X - $p2.X)), ($p2.Y + 2 / 3 * ($q.Y - $p2.Y))
    $tail.AddLine((New-Object System.Drawing.PointF 318, 530), $p0)
    $tail.AddBezier($p0, $c1, $c2, $p2)
    $tail.CloseFigure()
    $g.FillPath($accent, $tail)

    # Sound wave: white, centered vertical bars.
    $white = New-Object System.Drawing.SolidBrush (Color 0xFFFFFF)
    $heights = 64, 136, 208, 120, 232, 152, 80
    $barW = 36.0; $gap = 28.0
    $totalW = $heights.Count * $barW + ($heights.Count - 1) * $gap
    $midX = $bx + $bw / 2; $midY = $by + $bh / 2
    for ($i = 0; $i -lt $heights.Count; $i++) {
        $h = $heights[$i]
        $x = $midX - $totalW / 2 + $i * ($barW + $gap)
        $g.FillPath($white, (RoundedRect $x ($midY - $h / 2) $barW $h ($barW / 2)))
    }

    # Subtitle lines: dark, two lines, centered.
    $ink = New-Object System.Drawing.SolidBrush (Color 0x1F2333)
    foreach ($line in @(@(704, 496), @(776, 320))) {
        $y = $line[0]; $w = $line[1]
        $g.FillPath($ink, (RoundedRect (512 - $w / 2) $y $w 40 20))
    }
    $g.Dispose()
    $bmp
}

function Resize([System.Drawing.Bitmap]$src, [int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($src, 0, 0, $size, $size)
    $g.Dispose()
    $bmp
}

$master = New-MasterIcon
$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = foreach ($s in $sizes) {
    $bmp = Resize $master $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    if ($s -eq 64) { $bmp.Save((Join-Path $here 'AppIcon.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
    , $ms.ToArray()
}
$master.Dispose()

# ICO container: ICONDIR + ICONDIRENTRY[] + PNG payloads.
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $data = $pngs[$i]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$data.Length); $w.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($data in $pngs) { $w.Write($data) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $here 'AppIcon.ico'), $out.ToArray())
Write-Host "Wrote AppIcon.ico and AppIcon.png"
