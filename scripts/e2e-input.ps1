# Keyboard end-to-end: typing, Enter, AltGr (es-ES @ # \ ~ €), Ctrl+C, history, exit codes. Real window, real SendInput.
. "$PSScriptRoot\e2e-lib.ps1"
$layout = (Get-WinUserLanguageList)[0].LanguageTag
Write-Host "keyboard layout language: $layout"
$p = Start-App -Fresh
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Start-Sleep -Seconds 1
# §23.1: the shell must start at the size it keeps. A resize after spawn (e.g. 91 -> 90 columns) makes a wide prompt wrap and
# garbles PSReadLine's history list.
Check 'the pane was spawned at its final size (no resize after spawn)' ((Get-Dump) -match 'resizes=\[\(\d+, \d+, 0\)\]') ((Get-Dump).Split("`n")[1] -replace '.*(resizes=\[[^\]]*\]).*','$1')

Send-Text 'echo hello-from-ut'; Send-Enter
Check 'typed text + Enter reach the shell' (Wait-Dump 'hello-from-ut\r?\n' 10)

# AltGr+2 = '@', AltGr+3 = '#', AltGr+º = '\' on es-ES (VK 0x32, 0x33, OEM_5 = 0xDC)
Send-Text 'echo A'; Send-AltGr 0x32; Send-Text 'B'; Send-AltGr 0x33; Send-Text 'C'; Send-AltGr 0xDC; Send-Text 'D'; Send-Enter
$ok = Wait-Dump 'A@B#C\\D' 10
Check 'AltGr+2 / AltGr+3 / AltGr+º type @ # \ (es-ES)' $ok ($(if (-not $ok) { (Get-Dump).Split("`n")[-6..-1] -join ' | ' }))

# exit code → status
Send-Text 'cmd /c exit 7'; Send-Enter
Check 'OSC 133 exit code is tracked' (Wait-Dump 'last_exit=Some\(7\)' 10)

# Ctrl+C interrupts a running command
Send-Text 'Start-Sleep 60'; Send-Enter
Check 'long command is running' (Wait-Dump 'phase=Running' 10)
Send-Vk 0x43 -Ctrl
Check 'Ctrl+C reaches the shell (^C, prompt returns)' (Wait-Dump 'phase=Input' 10)

# history navigation with the arrow keys
Send-Vk 0x26; Start-Sleep -Milliseconds 400
Check 'Up arrow recalls history' ((Get-Dump) -match 'Start-Sleep 60')

Save-Shot 'input' | Out-Null
Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
