# Quake drop-down (§7.8): toggled through the debug hook (`<dump>.toggle`), never through a real global hotkey (the user's
# old app owns Win+`). Checks the geometry: full work-area width, heightPercent tall, at the top, always on top.
. "$PSScriptRoot\e2e-lib.ps1"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class UtWin { [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i); [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); }
'@
function Toggle { New-Item -ItemType File -Force "$($script:Dump).toggle" | Out-Null; for ($i = 0; $i -lt 30 -and (Test-Path "$($script:Dump).toggle"); $i++) { Start-Sleep -Milliseconds 100 }; Start-Sleep -Milliseconds 700 }
function Rect { $r = New-Object UtIn+RECT; [void][UtIn]::GetWindowRect((Get-AppHwnd), [ref]$r); $r }

# the work area of the monitor under the cursor (the app uses the same one)
$wa = [System.Windows.Forms.Screen]::FromPoint([System.Windows.Forms.Cursor]::Position).WorkingArea
New-Item -ItemType Directory -Force "$script:Root\roaming", "$script:Root\local" | Out-Null
'{"quake":{"dropdown":true,"heightPercent":40,"hideOnBlur":false,"hotkey":"Ctrl+Alt+Shift+F11"}}' | Set-Content "$script:Root\roaming\settings.json" -Encoding UTF8

[void](Start-App)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Toggle   # visible + focused -> hides
Check 'toggle hides the window' (-not [UtWin]::IsWindowVisible((Get-AppHwnd)) -or (Get-AppHwnd) -eq [IntPtr]::Zero)
Toggle   # shows as a drop-down
$h = Get-AppHwnd
Check 'window is visible again' ($h -ne [IntPtr]::Zero)
$r = Rect
$expectH = [int]($wa.Height * 0.40)
Check 'width = work-area width' ([math]::Abs(($r.Right - $r.Left) - $wa.Width) -le 2) "got $($r.Right - $r.Left), want $($wa.Width)"
Check 'height = 40% of the work area' ([math]::Abs(($r.Bottom - $r.Top) - $expectH) -le 2) "got $($r.Bottom - $r.Top), want $expectH"
Check 'docked at the top-left of the work area' (([math]::Abs($r.Top - $wa.Top) -le 2) -and ([math]::Abs($r.Left - $wa.Left) -le 2)) "got $($r.Left),$($r.Top) want $($wa.Left),$($wa.Top)"
Check 'always on top' (([UtWin]::GetWindowLong($h, -20) -band 0x8) -ne 0)
[void](Save-Shot 'quake')
Toggle   # hides again
Check 'second toggle hides it' (-not [UtWin]::IsWindowVisible((Get-AppHwnd)) -or (Get-AppHwnd) -eq [IntPtr]::Zero)

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
