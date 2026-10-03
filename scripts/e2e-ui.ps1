# Screenshots of the main chrome (sidebar, settings window) and shortcut routing while a text field has focus.
. "$PSScriptRoot\e2e-lib.ps1"
[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Start-Sleep -Seconds 1
Write-Host ('main:     ' + (Save-Shot 'ui-main'))
Send-Vk 0xBC -Ctrl      # Ctrl+Comma = settings
Start-Sleep -Milliseconds 900
Write-Host ('settings: ' + (Save-Shot 'ui-settings'))
Send-Vk 0x1B            # Esc closes it
Start-Sleep -Milliseconds 500

# App shortcuts keep working while a text field has focus (shortcuts-only router mode): Ctrl+B opens the sidebar WITH the
# search box focused, a second Ctrl+B must close it again.
function Sidebar-Open { "$(Get-Dump)" -match 'sidebar=True' }
Send-Vk 0x42 -Ctrl; Start-Sleep -Milliseconds 700
Check 'Ctrl+B closes the sidebar (terminal focus)' (-not (Sidebar-Open))
Send-Vk 0x42 -Ctrl; Start-Sleep -Milliseconds 700
Check 'Ctrl+B opens it again' (Sidebar-Open)
Send-Text 'zzz'; Start-Sleep -Milliseconds 400
Check 'typing goes to the focused search box, not the shell' (-not ("$(Get-Dump)" -match 'zzz'))
Send-Vk 0x42 -Ctrl; Start-Sleep -Milliseconds 700
Check 'Ctrl+B closes the sidebar from the search box' (-not (Sidebar-Open))

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
Write-Host "shots in $script:Root"
