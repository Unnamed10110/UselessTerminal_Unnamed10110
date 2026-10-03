param([switch]$Restart, [switch]$FocusPage, [switch]$SendSelection, [switch]$Palette, [switch]$CtrlV, [switch]$Link, [switch]$Topmost, [switch]$Grab, [switch]$AltTap)
# Switches reproduce what the full e2e (e2e-browser.ps1) does before the child window dies; none is needed for the basic case:
#   -Restart      restart the app first (the panel reopens by itself)
#   -FocusPage    click inside the page so it has the keyboard      -CtrlV  also type Ctrl+V into it
#   -Link         click the target=_blank link first (navigates in the same view)
#   -SendSelection  run the palette's 'Send Selection to Browser' first   -Palette  open and close the palette first
#   -Topmost / -Grab / -AltTap  make the window topmost, CopyFromScreen it, Focus-App (Alt tap) right before the destroy
# Browser panel recovery: the page's child window is destroyed behind the app's back (a page calling window.close(),
# a crashed view). The panel must notice and bring up a new view on the same page, and the UI must not freeze meanwhile.
# Local http server only (127.0.0.1); no network, no sign-in. Reports how long the replacement took and the longest
# gap between UI frames (the dump file is rewritten every 500 ms while the UI thread runs).
. "$PSScriptRoot\e2e-lib.ps1"
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class UtRec {
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref P p);
  [StructLayout(LayoutKind.Sequential)] public struct P { public int X, Y; }
}
'@
$port = 18900 + ($PID % 100)
$www = Join-Path $script:Root 'www'
New-Item -ItemType Directory -Force $www, "$script:Root\roaming" | Out-Null
# a text field at the same place as in e2e-browser.ps1 (x 20..320, y 200..300): focusing it brings the text services in
Set-Content "$www\index.html" '<!doctype html><title>UT recover</title><body style="margin:0;background:#ff00ff"><a href="/two.html" target="_blank" style="position:absolute;left:20px;top:60px;width:300px;height:100px;background:#ffff00;display:block">open</a><textarea id=t style="position:absolute;left:20px;top:200px;width:300px;height:100px"></textarea>'
Set-Content "$www/two.html" '<!doctype html><title>UT two</title><body style="margin:0;background:#00ffff"><textarea id=t style="position:absolute;left:20px;top:200px;width:300px;height:100px"></textarea>'
$server = Start-Process -FilePath python -ArgumentList @('-u', '-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $www) -RedirectStandardError "$script:Root\server.log" -RedirectStandardOutput "$script:Root\server.out" -WindowStyle Hidden -PassThru
Set-Content "$script:Root\roaming\browser.json" "{`"lastUrl`":`"http://127.0.0.1:$port/`"}"
trap { if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }; Stop-App; throw $_ }

function Get-Browser { $f = "$($script:Dump).browser"; if (Test-Path $f) { [string](Get-Content $f -Raw -ErrorAction SilentlyContinue) } else { '' } }

[void](Start-App)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Send-Vk 0x42 -Ctrl -Shift       # Ctrl+Shift+B
$end = (Get-Date).AddSeconds(30); while ((Get-Date) -lt $end -and (Get-Browser) -notmatch 'exists=true.*shown=true') { Start-Sleep -Milliseconds 300 }
Check 'panel open with a webview' ((Get-Browser) -match 'exists=true.*shown=true')
if ($Restart) {
  Start-Sleep -Seconds 2; Stop-App; Start-Sleep -Seconds 2
  [void](Start-App)
  Check 'restart: shell prompt again' (Wait-Dump 'phase=Input' 40)
  Focus-App
  $end = (Get-Date).AddSeconds(40); while ((Get-Date) -lt $end -and (Get-Browser) -notmatch 'exists=true.*shown=true') { Start-Sleep -Milliseconds 300 }
  Check 'restart: the panel reopened by itself' ((Get-Browser) -match 'exists=true.*shown=true')
}
Start-Sleep -Seconds 3
if ($Grab) {
  Add-Type -AssemblyName System.Drawing
  [void][UtRec]::SetProcessDPIAware()
  $o = New-Object UtRec+P; [void][UtRec]::ClientToScreen((Get-AppHwnd), [ref]$o)
  $bmp = New-Object System.Drawing.Bitmap 1200, 800
  $g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($o.X, $o.Y, 0, 0, $bmp.Size); $g.Dispose(); $bmp.Dispose()
}
if ($AltTap) { Focus-App }
if ($Topmost) { [void][UtRec]::SetWindowPos((Get-AppHwnd), [IntPtr](-1), 0, 0, 0, 0, 0x0013); Start-Sleep -Milliseconds 500 }   # HWND_TOPMOST, NOMOVE | NOSIZE | NOACTIVATE
if ($Link) {
  [void][UtRec]::SetProcessDPIAware()
  $null = (Get-Browser) -match 'child=(-?\d+),(-?\d+),(\d+)x(\d+)'
  $p = New-Object UtRec+P; [void][UtRec]::ClientToScreen((Get-AppHwnd), [ref]$p)
  Focus-App
  [void][UtIn]::SetCursorPos($p.X + [int]$Matches[1] + 60, $p.Y + [int]$Matches[2] + 100); Start-Sleep -Milliseconds 300
  foreach ($i in 1..3) { [UtIn]::mouse_event(0x0001, -1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }
  [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 50; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Seconds 2
}
if ($FocusPage) {
  [void][UtRec]::SetProcessDPIAware()
  $null = (Get-Browser) -match 'child=(-?\d+),(-?\d+),(\d+)x(\d+)'
  $p = New-Object UtRec+P; [void][UtRec]::ClientToScreen((Get-AppHwnd), [ref]$p)
  Focus-App
  [void][UtIn]::SetCursorPos($p.X + [int]$Matches[1] + 80, $p.Y + [int]$Matches[2] + 200); Start-Sleep -Milliseconds 300
  foreach ($i in 1..3) { [UtIn]::mouse_event(0x0001, -1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }
  [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 50; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 500
  if ($CtrlV) { Send-Vk 0x56 -Ctrl; Start-Sleep -Milliseconds 800 }
}
if ($Palette) {
  Focus-App; Send-Vk 0x50 -Ctrl -Shift; Start-Sleep -Milliseconds 900; Send-Vk 0x1B; Start-Sleep -Milliseconds 900
}
if ($SendSelection) {
  [void][UtRec]::SetProcessDPIAware()
  $o = New-Object UtRec+P; [void][UtRec]::ClientToScreen((Get-AppHwnd), [ref]$o)
  Focus-App
  Send-Text 'Clear-Host; ''SENDME-123'''; Send-Enter; Start-Sleep -Seconds 1
  [void][UtIn]::SetCursorPos($o.X + 330, $o.Y + 74); Start-Sleep -Milliseconds 300
  foreach ($i in 1..3) { [UtIn]::mouse_event(0x0001, -1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }
  foreach ($i in 1..3) { [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 70 }
  Start-Sleep -Milliseconds 400
  Send-Vk 0x50 -Ctrl -Shift; Start-Sleep -Milliseconds 600
  Send-Text 'Send Selection to Browser'; Start-Sleep -Milliseconds 500; Send-Enter
  Start-Sleep -Seconds 4
  Focus-App
}
$old = if ((Get-Browser) -match 'hwnd=([0-9a-f]+)') { $Matches[1] } else { '' }
Check 'child window handle reported' ($old -ne '')

[void][UtRec]::PostMessage([IntPtr][Convert]::ToInt64($old, 16), 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)   # WM_CLOSE
$t0 = Get-Date; $maxGap = 0.0; $lastWrite = (Get-Item "$($script:Dump).browser").LastWriteTime; $new = ''
while (((Get-Date) - $t0).TotalSeconds -lt 120) {
  Start-Sleep -Milliseconds 100
  $w = (Get-Item "$($script:Dump).browser" -ErrorAction SilentlyContinue).LastWriteTime
  if ($w -ne $lastWrite) { $gap = ($w - $lastWrite).TotalSeconds; if ($gap -gt $maxGap) { $maxGap = $gap }; $lastWrite = $w }
  $b = Get-Browser
  if ($b -match 'exists=true' -and $b -match 'visible=true' -and $b -match 'hwnd=([0-9a-f]+)' -and $Matches[1] -ne $old) { $new = $Matches[1]; break }
}
$took = ((Get-Date) - $t0).TotalSeconds
Check 'a replacement view appeared (new handle, visible)' ($new -ne '') ('{0:N1} s' -f $took)
Check 'replacement within 10 s' ($took -le 10) ('{0:N1} s' -f $took)
Check 'the UI never froze for more than 3 s meanwhile' ($maxGap -le 3) ('longest gap between frames {0:N1} s' -f $maxGap)
Check 'it is back on the same page' ((Get-Browser) -match "url=`"http://127.0.0.1:$port/`"")
Stop-App
if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
