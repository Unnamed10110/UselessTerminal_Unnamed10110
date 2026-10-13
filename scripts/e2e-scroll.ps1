# ShareX "Windows message" scrolling capture (§7.11): WM_VSCROLL sent to the main window scrolls the focused pane.
# PostMessage only (no keyboard/mouse), but the shell is driven through the app window like the other e2e scripts.
. "$PSScriptRoot\e2e-lib.ps1"
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class UtMsg { [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
# The dump is rewritten every 500 ms: a read can hit the file mid-write, so retry.
function Get-Vtop { for ($i = 0; $i -lt 10; $i++) { if ("$(Get-Dump)" -match 'vtop=(-?\d+)') { return [long]$Matches[1] }; Start-Sleep -Milliseconds 120 }; -1 }
function Vscroll([int]$code) { [void][UtMsg]::PostMessage((Get-AppHwnd), 0x0115, [IntPtr]$code, [IntPtr]::Zero); Start-Sleep -Milliseconds 700 }

[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Send-Text '1..500 | ForEach-Object { "line $_" }'; Send-Enter
Check '500 lines of output arrived' (Wait-Dump 'line 500' 20)
Start-Sleep -Milliseconds 800
$bottom = Get-Vtop
Vscroll 0; $a = Get-Vtop
Check 'SB_LINEUP scrolls up one line' ($a -eq $bottom - 1) "bottom=$bottom now=$a"
Vscroll 2; $b = Get-Vtop
Check 'SB_PAGEUP scrolls up a page' ($b -lt $a - 5) "now=$b"
Vscroll 1; $c = Get-Vtop
Check 'SB_LINEDOWN scrolls down one line' ($c -eq $b + 1) "now=$c"
Vscroll 3; $d = Get-Vtop
Check 'SB_PAGEDOWN scrolls down a page' ($d -gt $c + 5) "now=$d"
Vscroll 6; $top = Get-Vtop
Check 'SB_TOP reaches the start of the scrollback' ($top -lt $bottom - 400) "top=$top bottom=$bottom"
Vscroll 7; $end = Get-Vtop
Check 'SB_BOTTOM returns to the live screen' ($end -eq $bottom) "now=$end"

# Typing with the prompt out of sight brings the view back to it (every terminal's "scroll on keystroke").
# The live bottom moves as soon as PSReadLine draws its prediction list, so compare with what SB_BOTTOM reports afterwards.
function Live-Bottom { Vscroll 7; Get-Vtop }
Vscroll 6; $far = Get-Vtop
Check 'scrolled to the top again' ($far -lt $bottom - 400) "now=$far"
Send-Text 'x'; Start-Sleep -Milliseconds 700
$snapped = Get-Vtop; $live = Live-Bottom
Check 'typing a character returns to the prompt' ($snapped -eq $live -and $snapped -gt $far + 400) "after typing=$snapped live bottom=$live"
Send-Vk 0x08; Start-Sleep -Milliseconds 400    # backspace: leave the line empty
$live = Live-Bottom; Vscroll 2; $up = Get-Vtop
Check 'scrolled up one page' ($up -lt $live - 5) "now=$up live=$live"
Send-Vk 0x26 -Ext; Start-Sleep -Milliseconds 700    # Up arrow (history) is input too
$snapped = Get-Vtop; $live = Live-Bottom
Check 'a cursor key returns to the prompt as well' ($snapped -eq $live) "after Up=$snapped live bottom=$live"
Send-Vk 0x1B; Start-Sleep -Milliseconds 300    # Esc clears the recalled line
Vscroll 6; $far = Get-Vtop
Send-Vk 0x21 -Shift -Ext; Start-Sleep -Milliseconds 500   # Shift+PageUp is a scroll command, NOT input: it must not snap down
Check 'scroll shortcuts do not snap the view back down' ((Get-Vtop) -lt $far + 100) "now=$(Get-Vtop) (was $far at the top)"

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
