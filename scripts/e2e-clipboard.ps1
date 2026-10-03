# Clipboard (§21.3): Ctrl+V pastes text exactly once; a multi-line paste raises the confirmation (modal) and Cancel pastes nothing.
# Uses the user's clipboard for the duration of the test and restores its text afterwards.
Add-Type -AssemblyName System.Windows.Forms
. "$PSScriptRoot\e2e-lib.ps1"
# the clipboard can be briefly held by other apps (clipboard managers): retry
function Set-Clip([string]$t) { for ($i = 0; $i -lt 20; $i++) { try { [Windows.Forms.Clipboard]::SetText($t); return } catch { Start-Sleep -Milliseconds 150 } } throw 'clipboard stayed busy' }
$saved = try { [Windows.Forms.Clipboard]::GetText() } catch { '' }
try {
  [void](Start-App -Fresh)
  Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
  Focus-App

  Set-Clip ('pasted-once-xyz')
  Send-Vk 0x56 -Ctrl; Start-Sleep -Milliseconds 800
  $n = ([regex]::Matches("$(Get-Dump)", 'pasted-once-xyz')).Count
  Check 'Ctrl+V pastes the text exactly once' ($n -eq 1) "occurrences: $n"
  Send-Vk 0x1B; Send-Vk 0x43 -Ctrl; Start-Sleep -Milliseconds 400   # clear the line

  # PowerShell (PSReadLine) has bracketed paste, so `auto` does not warn there; cmd has none -> the warning appears.
  function Get-AppPid { if ("$(Get-Dump)" -match '# pid=(\d+)') { [uint32]$Matches[1] } else { 0 } }
  $before = Get-AppPid
  Start-Process -FilePath (Join-Path $script:Root 'UselessTerminal.exe') -ArgumentList @('--', 'cmd.exe') -WindowStyle Hidden | Out-Null
  $end = (Get-Date).AddSeconds(30); while ((Get-Date) -lt $end -and (Get-AppPid) -eq $before) { Start-Sleep -Milliseconds 300 }
  Check 'cmd tab opened' ((Get-AppPid) -ne $before)
  Check 'cmd reaches its prompt' (Wait-Dump 'phase=Input' 25)
  Focus-App
  Set-Clip ("first-line-aaa`r`nsecond-line-bbb")
  Send-Vk 0x56 -Ctrl; Start-Sleep -Milliseconds 900
  Check 'multi-line paste asks for confirmation (modal open)' ("$(Get-Dump)" -match 'modal=True')
  [void](Save-Shot 'clipboard-multiline')
  Send-Vk 0x1B; Start-Sleep -Milliseconds 600      # Esc = Cancel
  Check 'Cancel pastes nothing' (-not ("$(Get-Dump)" -match 'first-line-aaa'))
  Check 'the dialog is closed' ("$(Get-Dump)" -match 'modal=False')
} finally {
  if ($saved) { Set-Clip ($saved) }
  Stop-App
}
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
