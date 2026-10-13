# Mouse selection like a text editor: double-click selects a word and, with the button still held, dragging extends it WORD BY
# WORD (both directions); triple-click selects the line; shift+click extends; a plain click clears.
# Drives the real window with the real mouse (only while the app window is in front) and reads `sel=` from the debug dump.
. "$PSScriptRoot\e2e-lib.ps1"
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class UtSel {
  [StructLayout(LayoutKind.Sequential)] public struct P { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref P p);
}
'@
[void][UtSel]::SetProcessDPIAware()   # physical pixels everywhere

function Header { for ($i = 0; $i -lt 8; $i++) { $d = "$(Get-Dump)"; if ($d -match 'sel=.*ppp=') { return $d.Split("`n")[0] + ' ' + $d.Split("`n")[1] }; Start-Sleep -Milliseconds 120 }; '' }
function Sel { $h = Header; if ($h -match 'sel=None') { return $null }; if ($h -match 'sel=Some\("(.*?)"\) cell=') { return ($Matches[1] -replace '\\n', "`n" -replace '\\"', '"') }; return '?' }
function Wait-Sel([scriptblock]$Cond, [int]$Sec = 4) { $end = (Get-Date).AddSeconds($Sec); while ((Get-Date) -lt $end) { Start-Sleep -Milliseconds 250; if (& $Cond (Sel)) { return $true } }; $false }

# screen position of the middle of cell (col,row)
function Cell-Screen([int]$col, [int]$row) {
  $h = Header
  if ($h -notmatch 'cell=([\d.]+)x([\d.]+) origin=([\d.]+),([\d.]+) ppp=([\d.]+)') { throw 'e2e: no grid geometry in the dump' }
  $cw = [double]$Matches[1]; $ch = [double]$Matches[2]; $ox = [double]$Matches[3]; $oy = [double]$Matches[4]; $ppp = [double]$Matches[5]
  $p = New-Object UtSel+P; [void][UtSel]::ClientToScreen((Get-AppHwnd), [ref]$p)
  @([int]($p.X + ($ox + ($col + 0.5) * $cw) * $ppp), [int]($p.Y + ($oy + ($row + 0.5) * $ch) * $ppp))
}
# the viewport row that shows the LAST buffer line equal to $text
function Row-Of([string]$text) {
  $d = "$(Get-Dump)"; $lines = $d.Split("`n") | Select-Object -Skip 1
  $hist = if ($d -match 'grid=\(\d+, \d+, (\d+)\)') { [int]$Matches[1] } else { 0 }
  $idx = -1; for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i].TrimEnd("`r") -eq $text) { $idx = $i } }
  if ($idx -lt 0) { throw "e2e: '$text' is not in the buffer" }
  $idx - $hist
}

# --- mouse (egui needs real pointer movement before a press) ---
function Mouse-Move([int]$x, [int]$y) {
  Assert-Fg; [void][UtIn]::SetCursorPos($x + 2, $y); Start-Sleep -Milliseconds 40
  foreach ($i in 1..2) { [UtIn]::mouse_event(0x0001, -1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40 }
  Start-Sleep -Milliseconds 80
}
function Down { Assert-Fg; [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero) }
function Up { [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero) }
function Click([int]$n = 1) { for ($i = 0; $i -lt $n; $i++) { Down; Start-Sleep -Milliseconds 35; Up; if ($i -lt $n - 1) { Start-Sleep -Milliseconds 60 } }; Start-Sleep -Milliseconds 120 }
function Shift-Down { [UtIn]::Key(0x10, $true, $false) }
function Shift-Up { [UtIn]::Key(0x10, $false, $false) }
function Drag-To([int]$x, [int]$y) {   # moves in small steps with the button held
  $p = [System.Windows.Forms.Cursor]::Position
  $n = 10; for ($i = 1; $i -le $n; $i++) { [void][UtIn]::SetCursorPos([int]($p.X + ($x - $p.X) * $i / $n), [int]($p.Y + ($y - $p.Y) * $i / $n)); [UtIn]::mouse_event(0x0001, 1, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
  Start-Sleep -Milliseconds 120
}
Add-Type -AssemblyName System.Windows.Forms

[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Send-Text "'alpha beta gamma delta'"; Send-Enter
Check 'the test line is on screen' (Wait-Dump "(?m)^alpha beta gamma delta\s*$" 10)
Start-Sleep -Milliseconds 900
$row = Row-Of 'alpha beta gamma delta'
# columns of the words: alpha 0-4, beta 6-9, gamma 11-15, delta 17-21
$alpha = Cell-Screen 2 $row; $beta = Cell-Screen 7 $row; $gamma = Cell-Screen 13 $row; $delta = Cell-Screen 19 $row

Mouse-Move $beta[0] $beta[1]; Click 2
Check 'double-click selects the word' (Wait-Sel { param($s) $s -eq 'beta' }) "sel=$(Sel)"

# double press, keep the button down, drag: the selection grows word by word
Mouse-Move $beta[0] $beta[1]; Click 1; Start-Sleep -Milliseconds 60; Down
Start-Sleep -Milliseconds 150
Check 'holding the second press keeps the word selected' (Wait-Sel { param($s) $s -eq 'beta' }) "sel=$(Sel)"
Drag-To $gamma[0] $gamma[1]
Check 'dragging right extends to the next word' (Wait-Sel { param($s) $s -eq 'beta gamma' }) "sel=$(Sel)"
Drag-To $delta[0] $delta[1]
Check 'dragging further extends word by word' (Wait-Sel { param($s) $s -eq 'beta gamma delta' }) "sel=$(Sel)"
Drag-To $alpha[0] $alpha[1]
Check 'dragging back over the anchor word extends to the left' (Wait-Sel { param($s) $s -eq 'alpha beta' }) "sel=$(Sel)"
Up; Start-Sleep -Milliseconds 300
Check 'the selection stays after the button is released' (Wait-Sel { param($s) $s -eq 'alpha beta' }) "sel=$(Sel)"

# triple-click: the whole line
Mouse-Move $gamma[0] $gamma[1]; Click 3
Check 'triple-click selects the line' (Wait-Sel { param($s) $s -match '^alpha beta gamma delta' }) "sel=$(Sel)"

# plain click clears
Mouse-Move $delta[0] $delta[1]; Click 1
Check 'a plain click clears the selection' (Wait-Sel { param($s) $null -eq $s })

# shift+click extends the selection (double-click beta, then shift+click delta)
Mouse-Move $beta[0] $beta[1]; Click 2
[void](Wait-Sel { param($s) $s -eq 'beta' })
Mouse-Move $delta[0] $delta[1]; Shift-Down; Click 1; Shift-Up
Check 'shift+click extends the selection to the clicked word' (Wait-Sel { param($s) $s -eq 'beta gamma delta' }) "sel=$(Sel)"

# an ordinary character drag still works
Mouse-Move $alpha[0] $alpha[1]; Click 1
Start-Sleep -Milliseconds 900   # longer than the double-click time, or the next press would be a double press
Mouse-Move $alpha[0] $alpha[1]; Down; Start-Sleep -Milliseconds 60; Drag-To $beta[0] $beta[1]; Up; Start-Sleep -Milliseconds 300
# pressed in the middle of "alpha", released in the middle of "beta": a partial first word, so NOT a word selection
Check 'a plain drag selects characters (not words)' (Wait-Sel { param($s) $s -and $s.Length -ge 3 -and $s -notmatch '^alpha' -and $s -match 'b' }) "sel=$(Sel)"
[void](Save-Shot 'select')

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
