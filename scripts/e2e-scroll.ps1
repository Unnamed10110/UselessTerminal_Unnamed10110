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

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
