# Browser panel end-to-end (§17): lazy child WebView2, page renders inside the panel, bounds follow the window and the
# splitter, airspace (hidden under the palette), toggle, address bar, window.open in the same view, last URL restored.
# Runs against a LOCAL http server (127.0.0.1) so it needs no network, never signs in anywhere, downloads nothing.
#   $env:UT_E2E_EXE = '...\target-browser\debug\UselessTerminal.exe'; .\scripts\e2e-browser.ps1
# Window pixels are grabbed with CopyFromScreen (egui's own UT_DUMP screenshot cannot see the child window).
# -Scale 1.5 runs the same flow with the UI zoom (settings ui.scale) at 150 % to check the points -> pixels maths.
param([double]$Scale = 1.0)

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class UtWin {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref P p);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out Rc r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct P { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct Rc { public int L, T, R, B; }
}
'@
[void][UtWin]::SetProcessDPIAware()   # physical pixels everywhere (before any window call)
Add-Type -AssemblyName System.Drawing
. "$PSScriptRoot\e2e-lib.ps1"

$port = 18700 + ($PID % 200)
$www = Join-Path $script:Root 'www'
New-Item -ItemType Directory -Force $www, "$script:Root\roaming" | Out-Null
$base = "http://127.0.0.1:$port"
# page one: magenta, a big yellow link that opens in a NEW window (must stay in this view), a textarea that reports
# what is pasted into it to the server log (the "send selection" check).
Set-Content "$www\index.html" @"
<!doctype html><title>UT one</title><body style="margin:0;overflow:hidden;background:#ff00ff">
<a href="/two.html" target="_blank" style="position:absolute;left:20px;top:60px;width:300px;height:100px;background:#ffff00;display:block">open</a>
<textarea id=t autofocus style="position:absolute;left:20px;top:200px;width:300px;height:100px" oninput="fetch('/got?text='+encodeURIComponent(this.value))"></textarea>
"@
Set-Content "$www\two.html" @"
<!doctype html><title>UT two</title><body style="margin:0;overflow:hidden;background:#00ffff">
<textarea id=t autofocus style="position:absolute;left:20px;top:200px;width:300px;height:100px" oninput="fetch('/got?text='+encodeURIComponent(this.value))"></textarea>
"@
$serverLog = Join-Path $script:Root 'server.log'
$server = Start-Process -FilePath python -ArgumentList @('-u', '-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $www) -RedirectStandardError $serverLog -RedirectStandardOutput "$script:Root\server.out" -WindowStyle Hidden -PassThru
Set-Content "$script:Root\roaming\browser.json" "{`"lastUrl`":`"$base/`"}"
if ($Scale -ne 1.0) { Set-Content "$script:Root\roaming\settings.json" ('{"ui":{"scale":' + $Scale.ToString([cultureinfo]::InvariantCulture) + '}}') }
$gut = [int][math]::Round(5 * $Scale)   # the splitter's grab zone / the page's side gutter, in physical pixels

function Stop-Server { if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force } }
trap { Stop-Server; Stop-App; throw $_ }

function Get-Browser {
  $f = "$($script:Dump).browser"
  # the app rewrites this file in place every 500 ms: a read can catch it half-written, so read again until it is whole
  $t = ''
  for ($i = 0; $i -lt 5 -and $t -notmatch 'egui_focus=(None|Some\(.*\))\s*$'; $i++) { if ($i) { Start-Sleep -Milliseconds 60 }; $t = if (Test-Path $f) { [string](Get-Content $f -Raw -ErrorAction SilentlyContinue) } else { '' } }
  $o = [ordered]@{ Raw = $t; Open = ($t -match 'open=true'); Exists = ($t -match 'exists=true'); Shown = ($t -match 'shown=true'); Visible = ($t -match 'visible=true'); Url = ''; X = 0; Y = 0; W = 0; H = 0; AX = 0; AY = 0; AW = 0; AH = 0 }
  if ($t -match 'url="([^"]*)"') { $o.Url = $Matches[1] }
  if ($t -match 'addr=Some\(\[(-?\d+), (-?\d+), (\d+), (\d+)\]\)') { $o.AX = [int]$Matches[1]; $o.AY = [int]$Matches[2]; $o.AW = [int]$Matches[3]; $o.AH = [int]$Matches[4] }
  if ($t -match 'child=(-?\d+),(-?\d+),(\d+)x(\d+)') { $o.X = [int]$Matches[1]; $o.Y = [int]$Matches[2]; $o.W = [int]$Matches[3]; $o.H = [int]$Matches[4] }
  [pscustomobject]$o
}
function Wait-Browser([scriptblock]$Cond, [int]$TimeoutSec = 20) {
  $end = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $end) { $b = Get-Browser; if (& $Cond $b) { return $true }; Start-Sleep -Milliseconds 250 }
  return $false
}

# --- pixels ---------------------------------------------------------------------------------------------------
function Grab([string]$Name) {
  $h = Get-AppHwnd
  $r = New-Object UtWin+Rc; [void][UtWin]::GetClientRect($h, [ref]$r)
  if ($r.R -le 0 -or $r.B -le 0) { throw "e2e: the app window has an empty client area (hwnd=$h client=$($r.R)x$($r.B))" }
  $p = New-Object UtWin+P; [void][UtWin]::ClientToScreen($h, [ref]$p)
  $bmp = New-Object System.Drawing.Bitmap $r.R, $r.B
  $g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($p.X, $p.Y, 0, 0, $bmp.Size); $g.Dispose()
  $bmp.Save((Join-Path $script:Root "$Name.png"))
  [pscustomobject]@{ Bmp = $bmp; Client = $r; Origin = $p }
}
function Px($shot, [int]$x, [int]$y) {
  if ($x -lt 0 -or $y -lt 0 -or $x -ge $shot.Bmp.Width -or $y -ge $shot.Bmp.Height) { return 'out' }
  $c = $shot.Bmp.GetPixel($x, $y); '{0:x2}{1:x2}{2:x2}' -f $c.R, $c.G, $c.B
}
# WebView2 output is not bit-exact on screen (ff00ff arrives as ff00ed): compare with a tolerance
function Near([string]$got, [string]$want) {
  if ($got -eq 'out' -or $got.Length -ne 6) { return $false }
  foreach ($i in 0, 2, 4) { if ([math]::Abs([Convert]::ToInt32($got.Substring($i, 2), 16) - [Convert]::ToInt32($want.Substring($i, 2), 16)) -gt 100) { return $false } }
  return $true
}
# true when the page colour fills the child rect (centre + 4 inner corners) and is absent 4 px outside every edge
function Page-Fills($shot, $b, [string]$color) {
  $in = 6
  $pts = @(@(($b.X + $b.W / 2), ($b.Y + 340)), @(($b.X + $in), ($b.Y + $in)), @(($b.X + $b.W - $in), ($b.Y + $in)), @(($b.X + $in), ($b.Y + $b.H - $in)), @(($b.X + $b.W - $in), ($b.Y + $b.H - $in)))
  foreach ($q in $pts) { if (-not (Near (Px $shot ([int]$q[0]) ([int]$q[1])) $color)) { return $false } }
  foreach ($q in @(@(($b.X - 4), ($b.Y + $b.H / 2)), @(($b.X + $b.W + 4), ($b.Y + $b.H / 2)))) { if (Near (Px $shot ([int]$q[0]) ([int]$q[1])) $color) { return $false } }
  return $true
}
function Wait-Fills([string]$color, [int]$TimeoutSec = 25) {
  $end = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $end) {
    $b = Get-Browser
    if ($b.Shown -and $b.W -gt 0) {
      $s = Grab 'probe'; $ok = Page-Fills $s $b $color; $s.Bmp.Dispose()
      if ($ok) { return $true }
    }
    Start-Sleep -Milliseconds 400
  }
  return $false
}

# --- mouse (only while the app window is in front) ---------------------------------------------------------------
# SetCursorPos alone posts no WM_MOUSEMOVE to the window under the new position, and egui drops a press whose pointer
# position it does not know yet (the pointer was last over the child window, so the app's window saw it leave). Real
# hardware sends a stream of moves: send a few small ones, each hit-tested at the new position.
function Mouse-Move([int]$x, [int]$y) {
  Ensure-Fg; [void][UtIn]::SetCursorPos($x + 3, $y); Start-Sleep -Milliseconds 40
  foreach ($i in 1..3) { [UtIn]::mouse_event(0x0001, -1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 }
  Start-Sleep -Milliseconds 100
}
function Mouse-Click([int]$x, [int]$y) { Mouse-Move $x $y; [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 50; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 250 }
function Mouse-Drag([int]$x0, [int]$y0, [int]$x1, [int]$y1) {
  Mouse-Move $x0 $y0; Start-Sleep -Milliseconds 300   # egui registers the hover on the splitter before the press
  [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 120
  $n = 12; for ($i = 1; $i -le $n; $i++) { Mouse-Move ([int]($x0 + ($x1 - $x0) * $i / $n)) ([int]($y0 + ($y1 - $y0) * $i / $n)); Start-Sleep -Milliseconds 40 }
  Start-Sleep -Milliseconds 150; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 400
}
# Other windows (another agent's run, a notification) can grab the foreground between steps: win it back before any input.
function Ensure-Fg { for ($i = 0; $i -lt 5; $i++) { try { Assert-Fg; return } catch { Focus-App } }; Assert-Fg }
# A chord held like a person holds it (e2e-lib's Send-Vk releases after ~20 ms; the page's key events reach the app a
# few frames late, a debug build needs the modifiers to still be down then).
function K([int]$vk, [switch]$Ctrl, [switch]$Shift, [switch]$Alt) {
  Ensure-Fg
  if ($Ctrl) { [UtIn]::Key(0x11, $true, $false); Start-Sleep -Milliseconds 20 }
  if ($Shift) { [UtIn]::Key(0x10, $true, $false); Start-Sleep -Milliseconds 20 }
  if ($Alt) { [UtIn]::Key(0x12, $true, $false); Start-Sleep -Milliseconds 20 }
  [UtIn]::Key($vk, $true, $false); Start-Sleep -Milliseconds $(if ($Scale -ne 1.0) { 300 } else { 150 }); [UtIn]::Key($vk, $false, $false); Start-Sleep -Milliseconds 40
  if ($Alt) { [UtIn]::Key(0x12, $false, $false) }; if ($Shift) { [UtIn]::Key(0x10, $false, $false) }; if ($Ctrl) { [UtIn]::Key(0x11, $false, $false) }
  Start-Sleep -Milliseconds 100
}
function T([string]$t) { Ensure-Fg; Send-Text $t }
function Screen-Of($shot, [int]$x, [int]$y) { @(($shot.Origin.X + $x), ($shot.Origin.Y + $y)) }


# CopyFromScreen sees whatever is in front: keep our (temp copy of the) app window above every other normal window.
function Make-Topmost { [void][UtWin]::SetWindowPos((Get-AppHwnd), [IntPtr](-1), 0, 0, 0, 0, 0x0013) }   # HWND_TOPMOST, NOMOVE | NOSIZE | NOACTIVATE
function Front { Focus-App; Make-Topmost; Start-Sleep -Milliseconds 300 }
function Right-Click([int]$x, [int]$y) { Mouse-Move $x $y; [UtIn]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 50; [UtIn]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 300 }

# ================================================================================================================
# Another run (another agent, a dev session) owns the foreground while it works: wait for it instead of fighting.
for ($i = 0; $i -lt 60; $i++) {
  $other = @(Get-CimInstance Win32_Process -Filter "Name='UselessTerminal.exe'" | Where-Object { $_.ExecutablePath -like "$env:TEMP\ut-e2e-*" })
  if (-not $other) { break }
  Write-Host "waiting for another e2e run (pid $($other[0].ProcessId))..."; Start-Sleep -Seconds 5
}
$P = Start-App
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40) "exited=$($P.HasExited)"
Front
Start-Sleep -Seconds 2

# --- lazy: nothing web-ish exists until the panel is opened --------------------------------------------------------
$b = Get-Browser
Check 'lazy: no webview at startup' ((-not $b.Exists) -and $b.Raw -match 'open=false')
$edge = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -like ('*' + $script:Root + '*WebView2Browser-rs*') })
Check 'lazy: no WebView2 process for the browser profile' ($edge.Count -eq 0)

# --- open: renders inside the panel rect -------------------------------------------------------------------------------
K 0x42 -Ctrl -Shift
Check 'Ctrl+Shift+B opens the panel and creates the webview' (Wait-Browser { param($b) $b.Open -and $b.Exists -and $b.Shown } 25)
Check 'initial page = last URL from browser.json' (Wait-Browser { param($b) $b.Url -eq "$base/" } 10) ((Get-Browser).Url)
$ok = Wait-Fills 'ff00ff' 30
$s = Grab 'open'
$b = Get-Browser
Check 'page renders (magenta) exactly inside the child rect' $ok "child=$($b.X),$($b.Y),$($b.W)x$($b.H) client=$($s.Client.R)x$($s.Client.B)"
Check 'child rect is right-docked with a gutter and below the nav bar' (($b.Y -gt 60) -and ($s.Client.R - ($b.X + $b.W) -ge 5) -and ($s.Client.R - ($b.X + $b.W) -le 40) -and ($b.W -gt 300))
Check 'webview profile folder is the isolated one' (Test-Path "$script:Root\local\WebView2Browser-rs")
$edge = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -like ('*' + $script:Root + '*WebView2Browser-rs*') })
Check 'WebView2 runs on the isolated profile' ($edge.Count -gt 0)

# --- window resize: bounds follow -------------------------------------------------------------------------------------
$h = Get-AppHwnd
$wr = New-Object UtIn+RECT; [void][UtIn]::GetWindowRect($h, [ref]$wr)
$w0 = $wr.Right - $wr.Left; $h0 = $wr.Bottom - $wr.Top
$b0 = $b
[void][UtWin]::SetWindowPos($h, [IntPtr](-1), $wr.Left, $wr.Top, $w0 - 100, $h0 - 120, 0x0010)   # stay topmost, no activate
Start-Sleep -Milliseconds 1200
$s = Grab 'resized'; $b2 = Get-Browser
# the panel keeps its width and stays glued to the right edge: x moves left by the shrink, the height drops by it
# (the scaled run starts squeezed against the tab area's minimum width, so there the panel itself gets narrower instead)
$follows = if ($Scale -eq 1.0) { ([math]::Abs(($b0.X - $b2.X) - 100) -le 2) -and ($b2.W -eq $b0.W) } else { $b2.W -lt $b0.W }
Check 'window shrink: child rect follows (x moves left, height drops) and the page still fills it' ((Page-Fills $s $b2 'ff00ff') -and $follows -and ($b0.H - $b2.H -ge 100)) "was $($b0.X),$($b0.Y),$($b0.W)x$($b0.H) now $($b2.X),$($b2.Y),$($b2.W)x$($b2.H)"
# a window too narrow squeezes the panel but must not forget the chosen width
[void][UtWin]::SetWindowPos($h, [IntPtr](-1), $wr.Left, $wr.Top, 760, $h0, 0x0010)
Start-Sleep -Milliseconds 1200
$s = Grab 'narrow'; $bn = Get-Browser
Check 'narrow window: panel is squeezed (tab area keeps its minimum) and the page still fills it' (($bn.W -lt $b0.W) -and (Page-Fills $s $bn 'ff00ff')) "narrow child=$($bn.X),$($bn.Y),$($bn.W)x$($bn.H)"
[void][UtWin]::SetWindowPos($h, [IntPtr](-1), $wr.Left, $wr.Top, $w0, $h0, 0x0010)
Start-Sleep -Milliseconds 1200
$s = Grab 'restored'; $b3 = Get-Browser
Check 'window restored: the panel is back at its chosen width and the page fills it' ((Page-Fills $s $b3 'ff00ff') -and ($b3.W -eq $b0.W) -and ($b3.X -eq $b0.X)) "child=$($b3.X),$($b3.Y),$($b3.W)x$($b3.H) (was $($b0.W) wide)"

# --- splitter drag (hidden while dragging, back at the new width) ------------------------------------------------------
Front
$s = Grab 'pre-drag'; $b = Get-Browser
$gx = $b.X - $gut                   # the splitter sits in the gutter left of the page
$gy = $b.Y + [int]($b.H / 2)
# Scale 1: drag left = wider. Scaled run: the panel is already squeezed to its maximum, so drag right = narrower.
$dx = if ($Scale -eq 1.0) { -150 } else { 100 }
$from = Screen-Of $s $gx $gy; $to = Screen-Of $s ($gx + $dx) $gy
Mouse-Drag $from[0] $from[1] $to[0] $to[1]
Start-Sleep -Milliseconds 900
$s = Grab 'post-drag'; $b4 = Get-Browser
$moved = if ($dx -lt 0) { $b4.W -gt $b.W + 100 } else { $b4.W -lt $b.W - 60 }
Check 'splitter drag: the page width follows the drag and it still fills its rect' ($moved -and (Page-Fills $s $b4 'ff00ff')) "before $($b.W) after $($b4.W)"

# --- airspace: palette hides the page, closing it brings it back --------------------------------------------------------
Front
K 0x50 -Ctrl -Shift
Check 'airspace: opening the palette hides the child window' (Wait-Browser { param($b) -not $b.Visible } 8) ((Get-Browser).Raw)
$s = Grab 'palette'
Check 'airspace: no page pixels where the palette is open' (-not (Near (Px $s ([int]($b4.X + $b4.W / 2)) ([int]($b4.Y + $b4.H - 20))) 'ff00ff'))
K 0x1B
Check 'airspace: closing the palette restores the child window' (Wait-Browser { param($b) $b.Visible } 8)
Check 'page is back after the palette' (Wait-Fills 'ff00ff' 10)

# --- collapse / reopen ------------------------------------------------------------------------------------------------------
Front
K 0x42 -Ctrl -Shift
Check 'toggle closes: child hidden, not destroyed' (Wait-Browser { param($b) (-not $b.Visible) -and $b.Exists -and (-not $b.Open) } 8)
K 0x42 -Ctrl -Shift
Check 'toggle reopens: same page back' (Wait-Browser { param($b) $b.Open -and $b.Visible -and $b.Url -eq "$base/" } 10)
Check 'page fills again after reopen' (Wait-Fills 'ff00ff' 10)

# --- Ctrl+Shift+B while the PAGE has the keyboard (app shortcuts are forwarded from the page) -------------------------------------
$s = Grab 'pre-focus'; $b = Get-Browser
$ta = Screen-Of $s ($b.X + 100) ($b.Y + 240)
Mouse-Click $ta[0] $ta[1]                      # a click inside the page (the textarea) gives the page the keyboard
Start-Sleep -Milliseconds 500
K 0x42 -Ctrl -Shift
Check 'shortcut from inside the page closes the panel' (Wait-Browser { param($b) (-not $b.Open) -and (-not $b.Visible) } 8) ((Get-Browser).Raw)
K 0x42 -Ctrl -Shift
Check 'and opens it again (keyboard is back on the main window)' (Wait-Browser { param($b) $b.Open -and $b.Visible } 8)

# --- target=_blank stays in this view ---------------------------------------------------------------------------------------
Check 'page back before the link test' (Wait-Fills 'ff00ff' 10)
$s = Grab 'pre-link'; $b = Get-Browser
$pt = Screen-Of $s ($b.X + 60) ($b.Y + 100)
Mouse-Click $pt[0] $pt[1]
Check 'target=_blank navigates this view (url + cyan page)' ((Wait-Browser { param($b) $b.Url -eq "$base/two.html" } 12) -and (Wait-Fills '00ffff' 12)) ((Get-Browser).Url)
$nw = [UtIn]::BigWindow([uint32]$P.Id)
Check 'no extra top-level window was opened' ($nw -eq (Get-AppHwnd))

# --- address bar (egui text field above the page) -----------------------------------------------------------------------------
$s = Grab 'pre-addr'; $b = Get-Browser
if ($b.AW -le 0) { throw 'e2e: the app did not report the address bar position' }   # never guess: a stray click can hit a quick link (a real site)
$pt = Screen-Of $s ($b.AX + [int]($b.AW / 2)) ($b.AY + [int]($b.AH / 2))
Mouse-Click $pt[0] $pt[1]
Check 'address bar: a click focuses it (egui text field)' (Wait-Browser { param($b) $b.Raw -match 'focus_addr=true' } 5) ((Get-Browser).Raw)
$null = Grab 'addr-focused'
T "$base/"
K 0x0D
Check 'address bar: typed URL navigates (back to the magenta page)' ((Wait-Browser { param($b) $b.Url -eq "$base/" } 12) -and (Wait-Fills 'ff00ff' 12)) ((Get-Browser).Url)
Save-Shot 'egui' | Out-Null
# navigate once more so the saved last URL differs from the start page
$s = Grab 'pre-link2'; $b = Get-Browser
$pt = Screen-Of $s ($b.X + 60) ($b.Y + 100)
Mouse-Click $pt[0] $pt[1]
Check 'second navigation (link) before quitting' (Wait-Browser { param($b) $b.Url -eq "$base/two.html" } 12)

# --- restart: last URL restored, panel state restored ------------------------------------------------------------------------
Set-Content "$script:Root\last-before.txt" ((Get-Browser).Raw)
Start-Sleep -Seconds 1
Stop-App
Start-Sleep -Seconds 2
$j = Get-Content "$script:Root\roaming\browser.json" -Raw
Check 'browser.json holds the last URL' ($j -match [regex]::Escape("$base/two.html")) $j
Check 'window state remembers the panel is open' ((Get-Content "$script:Root\roaming\windowstate.json" -Raw) -match '"browserOpen":\s*true')
$okw = (Get-Content "$script:Root\roaming\windowstate.json" -Raw) -match '"browserWidth":\s*(\d+)'
$savedW = if ($okw) { [int]$Matches[1] } else { 0 }
Check 'window state remembers the dragged panel width (250..1600, wider than the default only after a drag left)' ($okw -and $savedW -ge 250 -and $savedW -le 1600 -and (($Scale -ne 1.0) -or $savedW -gt 600) -and $savedW -ne 500) "browserWidth=$savedW"
$P = Start-App
Check 'restart: shell prompt again' (Wait-Dump 'phase=Input' 40)
Front
Check 'restart: panel reopens by itself with the saved page' (Wait-Browser { param($b) $b.Open -and $b.Exists -and $b.Shown -and $b.Url -eq "$base/two.html" } 30) ((Get-Browser).Raw)
Check 'restart: page renders (cyan)' (Wait-Fills '00ffff' 25)
Check 'restart: same width as before' ((Get-Browser).W -eq $b4.W) "now $((Get-Browser).W), before $($b4.W)"

# --- send selection: terminal text -> clipboard -> focus the page -> Ctrl+V into its focused field ----------------------------
if ($Scale -eq 1.0) {
Front
T 'Clear-Host; ''SENDME-123'''; K 0x0D            # the output lands on row 0 of the pane
Start-Sleep -Seconds 1
$s = Grab 'sel'; $pt = Screen-Of $s 330 74
Mouse-Move $pt[0] $pt[1]
foreach ($i in 1..3) { [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 70 }   # triple click = the line
Start-Sleep -Milliseconds 400
$null = Grab 'selected'
K 0x50 -Ctrl -Shift; Start-Sleep -Milliseconds 600
T 'Send Selection to Browser'; Start-Sleep -Milliseconds 500; K 0x0D
Start-Sleep -Seconds 4
$log = Get-Content $serverLog -Raw -ErrorAction SilentlyContinue
Check 'send selection: the page received the terminal text through the clipboard (server saw /got?text=...SENDME...)' ($log -match 'got\?text=[^ ]*SENDME') (($log -split "`n" | Select-String 'got' | Select-Object -Last 2) -join ' | ')
$null = Grab 'after-send'
}

# --- the page's window destroyed behind the app's back (a page closing itself, a crashed view): the panel recovers ----------------
Front
$s = Grab 'pre-destroy'; $b = Get-Browser
$hwndBefore = if ($b.Raw -match 'hwnd=([0-9a-f]+)') { $Matches[1] } else { '' }
Check 'the child window handle is reported' ($hwndBefore -ne '') $b.Raw
[void][UtWin]::PostMessage([IntPtr][Convert]::ToInt64($hwndBefore, 16), 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)   # WM_CLOSE: the container window goes away
Check 'destroyed child window: the panel creates a new one (new handle, visible, same page)' (Wait-Browser { param($b) $b.Exists -and $b.Visible -and ($b.Raw -match 'hwnd=([0-9a-f]+)') -and $Matches[1] -ne $hwndBefore -and $b.Url -eq "$base/two.html" } 25) ((Get-Browser).Raw)
Check 'and the page renders again (cyan)' (Wait-Fills '00ffff' 25)
$null = Grab 'after-destroy'

Save-Shot 'end' | Out-Null
Stop-App
Stop-Server
$script:Results | Format-Table -AutoSize
Write-Host "artifacts: $script:Root"
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
