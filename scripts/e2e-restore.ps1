# Crash restore (§21.3): the app process is KILLED (no graceful shutdown); on relaunch the tab comes back in its last cwd.
. "$PSScriptRoot\e2e-lib.ps1"
function Get-Pid { if ("$(Get-Dump)" -match '# pid=(\d+)') { [uint32]$Matches[1] } else { 0 } }

[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Send-Text 'Set-Location C:\Windows'; Send-Enter
Check 'cwd changed' (Wait-Dump 'cwd=Some\("C:\\\\Windows"\)' 10)
Write-Host 'waiting for the periodic state save (15 s)...'
$f = Join-Path $script:Root 'roaming\windowstate.json'
$end = (Get-Date).AddSeconds(30); while (-not ((Test-Path $f) -and ((Get-Content $f -Raw) -match 'Windows')) -and (Get-Date) -lt $end) { Start-Sleep -Milliseconds 500 }
Check 'windowstate.json holds the cwd before the crash' ((Test-Path $f) -and ((Get-Content $f -Raw) -match 'Windows'))

Stop-Process -Id $script:Proc.Id -Force      # a crash: no shutdown path runs
Start-Sleep -Seconds 1
Remove-Item "$($script:Dump)*" -Force -ErrorAction SilentlyContinue
[void](Start-App)                              # same data dir
Check 'relaunch reaches a prompt' (Wait-Dump 'phase=Input' 40)
Check 'the tab came back in its last cwd' (Wait-Dump 'cwd=Some\("C:\\\\Windows"\)' 15) ((Get-Dump).Split("`n")[1])
Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
