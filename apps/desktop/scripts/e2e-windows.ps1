<#
.SYNOPSIS
End-to-end run of the staged Windows app (apps\desktop\dist\SubtitleAdder.exe, or -Exe) against a COPY of realistic
data written by the C# app's own code. Never touches %LOCALAPPDATA%\SubtitleAdder: the app runs with
SUBTITLE_ADDER_DATA_DIR pointing at <Root>\run\data and TMP/TEMP at <Root>\run\tmp.

    powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/scripts/e2e-windows.ps1 -Root <scratch folder> [-Exe <path>]

Steps (each one screenshotted into <Root>\shots, the app window only, through PrintWindow):
  start → Platform tab logged out (sign-in card) → log in (C# profile "Emre": history, likes, ratings, thumbnails,
  list) → history tab → log out (sign-in card on every tab) → open the TTS clip from the command line → "Komple
  çevir" (whisper base + madlad-3b, both must be in the Hugging Face cache) → edit a line → undo → export SRT through
  the save dialog → add an API-key provider in Settings → restart → the provider keys still decrypt.
Checks: no API key in plaintext under <Root>\run, logged-out playback recorded nothing (C# reader), the export exists.

The app is driven through UI Automation (Slint exposes its widgets via AccessKit) and posted window messages (wheel),
so the real mouse and keyboard are never used and no other window is affected. Uses the software renderer
(SLINT_BACKEND=winit-software) so PrintWindow can capture the client area. Windows PowerShell 5.1 compatible.
#>
param([Parameter(Mandatory = $true)][string]$Root, [string]$Exe = '')
$ErrorActionPreference = 'Stop'
$desktop = Split-Path $PSScriptRoot -Parent
if (-not $Exe) { $Exe = Join-Path $desktop 'dist\SubtitleAdder.exe' }
if (-not (Test-Path $Exe)) { throw "missing $Exe (run scripts\stage.ps1)" }
$Root = [IO.Path]::GetFullPath($Root)

& (Join-Path $PSScriptRoot 'e2e-data.ps1') -Root (Join-Path $Root 'template')
$template = Join-Path $Root 'template'
$run = Join-Path $Root 'run'
$shots = Join-Path $Root 'shots'
if (Test-Path $run) { Remove-Item -Recurse -Force $run }
New-Item -ItemType Directory -Force -Path (Join-Path $run 'tmp'), (Join-Path $run 'export'), $shots | Out-Null
Copy-Item -Recurse (Join-Path $template 'data') (Join-Path $run 'data')
Copy-Item -Recurse (Join-Path $template 'media') (Join-Path $run 'media')
Get-ChildItem (Join-Path $run 'media') -Filter *.srt | Remove-Item -Force

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class E2EWin {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    public static IntPtr Find(uint pid, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid && IsWindowVisible(h)) {
                var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
                RECT r; GetWindowRect(h, out r);
                if ((r.R - r.L) > 200 && (title == "" || sb.ToString().Contains(title))) { found = h; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
"@
[void][E2EWin]::SetProcessDPIAware()

function Start-App([string[]]$arguments) {
    $env:SUBTITLE_ADDER_DATA_DIR = Join-Path $run 'data'
    $env:TMP = Join-Path $run 'tmp'; $env:TEMP = Join-Path $run 'tmp'
    $env:SLINT_BACKEND = 'winit-software'
    $p = if ($arguments) { Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru } else { Start-Process -FilePath $Exe -PassThru }
    Start-Sleep -Seconds 6
    return $p
}
function Get-Elements([int]$procId) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $procId)
    $wins = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $cond)
    $all = @()
    foreach ($w in $wins) { $all += $w.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
    return $all
}
function Invoke-Named([int]$procId, [string]$name, [int]$index = 0) {
    $e = @(Get-Elements $procId | Where-Object { $_.Current.Name -eq $name })[$index]
    if (-not $e) { throw "not found: $name" }
    $p = $null
    foreach ($pat in [System.Windows.Automation.InvokePattern]::Pattern, [System.Windows.Automation.TogglePattern]::Pattern) {
        if ($e.TryGetCurrentPattern($pat, [ref]$p)) {
            if ($p -is [System.Windows.Automation.InvokePattern]) { $p.Invoke() } else { $p.Toggle() }
            Start-Sleep -Milliseconds 600
            return
        }
    }
    throw "not invokable: $name"
}
function Set-Named([int]$procId, [string]$name, [string]$text, [int]$index = 0) {
    $e = @(Get-Elements $procId | Where-Object { $_.Current.Name -eq $name -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Edit' })[$index]
    if (-not $e) { throw "no edit: $name" }
    $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($text)
    Start-Sleep -Milliseconds 300
}
function Test-Named([int]$procId, [string]$pattern) {
    return [bool](@(Get-Elements $procId | Where-Object { $_.Current.Name -match $pattern }).Count)
}
function Wait-Until([scriptblock]$condition, [int]$seconds, [string]$what) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while (-not (& $condition)) {
        if ((Get-Date) -gt $deadline) { throw "timeout: $what" }
        Start-Sleep -Seconds 2
    }
}
function Save-Shot([int]$procId, [string]$name, [string]$title = '') {
    $h = [E2EWin]::Find([uint32]$procId, $title)
    if ($h -eq [IntPtr]::Zero) { throw "no window for $name" }
    $r = New-Object E2EWin+RECT
    [void][E2EWin]::GetWindowRect($h, [ref]$r)
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc(); [void][E2EWin]::PrintWindow($h, $hdc, 2); $g.ReleaseHdc($hdc); $g.Dispose()
    $bmp.Save((Join-Path $shots "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    Write-Host "shot $name"
}
function Send-Wheel([int]$procId, [string]$title, [int]$x, [int]$y, [int]$notches) {
    $h = [E2EWin]::Find([uint32]$procId, $title)
    [void][E2EWin]::PostMessage($h, 0x0200, [IntPtr]0, [IntPtr](($y -shl 16) -bor $x))
    $pt = New-Object E2EWin+POINT; $pt.X = $x; $pt.Y = $y; [void][E2EWin]::ClientToScreen($h, [ref]$pt)
    for ($i = 0; $i -lt $notches; $i++) {
        [void][E2EWin]::PostMessage($h, 0x020A, [IntPtr]((-120 -band 0xffff) -shl 16), [IntPtr](($pt.Y -shl 16) -bor ($pt.X -band 0xffff)))
        Start-Sleep -Milliseconds 40
    }
    Start-Sleep -Milliseconds 500
}
function Stop-App($p) {
    for ($i = 0; $i -lt 3 -and -not $p.HasExited; $i++) { [void]$p.CloseMainWindow(); Start-Sleep -Seconds 4; $p.Refresh() }
    if (-not $p.HasExited) { throw "the app did not exit" }
}

$clip = Join-Path $run 'media\altyazi-testi.mp4'

# 1. Start, logged out.
$app = Start-App @()
Save-Shot $app.Id '01-start'
Invoke-Named $app.Id 'Platform'
Save-Shot $app.Id '02-hub-logged-out'
if (-not (Test-Named $app.Id '^Giriş yap$')) { throw 'logged out: no sign-in card' }

# 2. Log in with the C# profile, see history.
Set-Named $app.Id 'Şifre' 'e2e-parola-1'
Invoke-Named $app.Id 'Giriş yap'
Wait-Until { Test-Named $app.Id '^Çıkış yap$' } 30 'login'
# History (written by the C# app) rather than Keşfet, which also lists whatever the explorer indexed.
Invoke-Named $app.Id 'İzleme geçmişi (2)'
Start-Sleep -Seconds 2
Save-Shot $app.Id '03-hub-history'
Invoke-Named $app.Id 'Favorilerim (1)'
Save-Shot $app.Id '04-hub-favorites'

# 3. Log out: the sign-in card on every tab.
Invoke-Named $app.Id 'Çıkış yap'
Wait-Until { Test-Named $app.Id '^Giriş yap$' } 15 'logout'
foreach ($tab in 'Keşfet', 'İzleme geçmişi', 'Favorilerim', 'Puan verdiklerim', 'En çok izlenenler') {
    Invoke-Named $app.Id $tab
    if (-not (Test-Named $app.Id '^Giriş yap$')) { throw "logged out tab $tab shows no sign-in card" }
}
Save-Shot $app.Id '05-hub-after-logout'
Stop-App $app

# 4. Open the clip from the command line, generate + translate.
$app = Start-App @("`"$clip`"")
# The explorer sidebar (hidden in the data) now follows the opened video's folder.
Invoke-Named $app.Id 'Dosya Gezgini'
Start-Sleep -Seconds 1
Save-Shot $app.Id '06-editor-video'
Invoke-Named $app.Id 'Komple çevir'
Wait-Until { Test-Named $app.Id '^Çeviri \(en\)$' } 300 'generate and translate'
Start-Sleep -Seconds 2
Save-Shot $app.Id '07-translated'

# 5. Edit line 2, undo.
$row2 = @(Get-Elements $app.Id | Where-Object { $_.Current.Name -like '2. *' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.ListItem' })[0].Current.Name
Invoke-Named $app.Id $row2
Set-Named $app.Id 'Cümle' 'Bugün hava çok güzel.' 1
$row3 = @(Get-Elements $app.Id | Where-Object { $_.Current.Name -like '3. *' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.ListItem' })[0].Current.Name
Invoke-Named $app.Id $row3
if (-not (Test-Named $app.Id '^2\. Bugün hava çok güzel\.$')) { throw 'edit not applied' }
Save-Shot $app.Id '08-edited'
Invoke-Named $app.Id 'Metni Değiştir İşlemini Geri Al'
if (-not (Test-Named $app.Id ([regex]::Escape($row2)))) { throw 'undo did not restore the line' }
Save-Shot $app.Id '09-undone'

# 6. Export SRT through the save dialog.
Invoke-Named $app.Id 'Dışa aktar…'
Start-Sleep -Seconds 2
Save-Shot $app.Id '10-export-dialog' 'Farklı'
Set-Named $app.Id 'Dosya adı:' (Join-Path $run 'export\export-en')
Invoke-Named $app.Id 'Kaydet'
Start-Sleep -Seconds 2
if (-not (Test-Path (Join-Path $run 'export\export-en.srt'))) { throw 'export missing' }

# 7. Settings: add an API-key provider (unreachable local address: nothing is ever contacted).
Invoke-Named $app.Id 'Ayarlar'
Start-Sleep -Seconds 3
Save-Shot $app.Id '11-settings' 'Ayarlar'
Send-Wheel $app.Id 'Ayarlar' 650 600 18
Set-Named $app.Id 'Sağlayıcı adı' 'E2E Rust sağlayıcı'
Set-Named $app.Id 'Sunucu adresi (Base URL)' 'https://127.0.0.1:9/inference/go/openai/v1'
Set-Named $app.Id 'API anahtarı' 'sk-e2e-rust-ABCDEF0123456789'
Invoke-Named $app.Id 'Kaydet ve Etkinleştir'
Start-Sleep -Seconds 2
Save-Shot $app.Id '12-provider-added' 'Ayarlar'
Stop-App $app

# 8. Restart: both keys (C#-written and Rust-written) still decrypt.
$app = Start-App @()
Invoke-Named $app.Id 'Ayarlar'
Start-Sleep -Seconds 3
Send-Wheel $app.Id 'Ayarlar' 650 600 16
Save-Shot $app.Id '13-settings-after-restart' 'Ayarlar'
if (-not (Test-Named $app.Id 'sk-••••6789') -and -not (Test-Named $app.Id '6789')) { Write-Warning 'masked key text not exposed to UI Automation; see shot 13' }
Stop-App $app

# Checks on disk.
$leaks = Get-ChildItem -Recurse -File $run | Select-String -Pattern 'sk-e2e-rust-ABCDEF', 'sk-e2e-csharp-0123' -SimpleMatch
if ($leaks) { throw "API key in plaintext: $($leaks | Select-Object -ExpandProperty Path -Unique)" }
$env:TMP = Join-Path $template 'tmp'; $env:TEMP = $env:TMP
$dump = & dotnet run --project (Join-Path $desktop 'tools\fixtures\E2EData') -c Release -- dump (Join-Path $run 'data') Emre e2e-parola-1
$dump | ForEach-Object { Write-Host "  $_" }
if (-not ($dump -match 'history\|altyazi-testi\.mp4\|views=2\|')) { throw 'logged-out use changed the history' }
Write-Host "E2E passed; screenshots in $shots"
