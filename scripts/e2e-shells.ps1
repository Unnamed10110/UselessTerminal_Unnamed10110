# Shell integration per shell (§21.3): cmd, Git Bash, WSL, Windows PowerShell. Each shell is opened as a NEW TAB by forwarding
# `-- <command>` to the running (isolated) instance, then: it reaches its prompt (OSC 133), `cd` updates the cwd, and the
# exit code of the last command is tracked.
. "$PSScriptRoot\e2e-lib.ps1"
function Get-Pid { if ("$(Get-Dump)" -match '# pid=(\d+)') { [uint32]$Matches[1] } else { 0 } }
function Open-Tab([string[]]$Command) {
  $before = Get-Pid
  $copy = Join-Path $script:Root 'UselessTerminal.exe'
  Start-Process -FilePath $copy -ArgumentList (@('--') + $Command) -WindowStyle Hidden | Out-Null
  $end = (Get-Date).AddSeconds(40)
  while ((Get-Date) -lt $end) { if ((Get-Pid) -ne $before -and (Get-Pid) -ne 0) { return $true }; Start-Sleep -Milliseconds 300 }
  $false
}
function Run-Line([string]$Line) { Focus-App; Send-Text $Line; Send-Enter }

[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)

# cmd
$ok = Open-Tab @('cmd.exe'); Check 'cmd: tab opened' $ok
Check 'cmd: reaches its prompt (OSC 133)' (Wait-Dump 'phase=Input' 25)
Run-Line 'cd /d C:\Windows'
Check 'cmd: cd updates the cwd' (Wait-Dump 'cwd=Some\("C:\\\\Windows"\)' 10) ((Get-Dump).Split("`n")[1])
# cmd cannot report an exit code from PROMPT (spec A.4 emits 133;D without one): only the phase cycle is checked.
Run-Line 'echo cmd-ok'
Check 'cmd: the prompt markers cycle (output seen, back at the prompt)' ((Wait-Dump 'cmd-ok\r?\n' 10) -and (Wait-Dump 'phase=Input' 10))

# Windows PowerShell 5
$ok = Open-Tab @('powershell.exe'); Check 'powershell 5: tab opened' $ok
Check 'powershell 5: reaches its prompt' (Wait-Dump 'phase=Input' 25)
Focus-App; Isolate-History pwsh
Run-Line 'Set-Location C:\Windows'
Check 'powershell 5: cd updates the cwd' (Wait-Dump 'cwd=Some\("C:\\\\Windows"\)' 10) ((Get-Dump).Split("`n")[1])
Run-Line 'cmd /c exit 4'
Check 'powershell 5: exit code is tracked' (Wait-Dump 'last_exit=Some\(4\)' 10)

# Git Bash
$git = 'C:\Program Files\Git\bin\bash.exe'
if (Test-Path $git) {
  $ok = Open-Tab @('"' + $git + '"', '--login', '-i'); Check 'git bash: tab opened' $ok
  Check 'git bash: reaches its prompt' (Wait-Dump 'phase=Input' 40)
  Focus-App; Isolate-History bash
  Run-Line 'cd /c/Windows'
  # Git Bash reports its cwd as /c/Windows (OSC 7); the status bar / drops convert it (§6.3, §11.2), the raw value stays POSIX
  Check 'git bash: cd updates the cwd' (Wait-Dump 'cwd=Some\("(/c/Windows|C:\\\\Windows)"\)' 10) ((Get-Dump).Split("`n")[1])
  Run-Line 'false'
  Check 'git bash: exit code is tracked' (Wait-Dump 'last_exit=Some\(1\)' 10)
} else { Write-Host 'SKIP  git bash not installed' }

# WSL (default distro)
if (Get-Command wsl.exe -ErrorAction SilentlyContinue) {
  $ok = Open-Tab @('wsl.exe'); Check 'wsl: tab opened' $ok
  Check 'wsl: reaches its prompt' (Wait-Dump 'phase=Input' 60)
  Focus-App; Isolate-History bash
  Run-Line 'cd /tmp'
  Check 'wsl: cd updates the cwd' (Wait-Dump 'cwd=Some\(".*tmp' 10) ((Get-Dump).Split("`n")[1])
  Run-Line 'false'
  Check 'wsl: exit code is tracked' (Wait-Dump 'last_exit=Some\(1\)' 10)
  [void](Save-Shot 'shells-wsl')
}

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
