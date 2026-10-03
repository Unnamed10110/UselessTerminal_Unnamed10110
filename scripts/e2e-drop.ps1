# File-drop end to end: a REAL OLE drag of two files onto the terminal copies them into the pane's cwd; dropping the
# same files again raises the "File already exists" dialog (Enter = Rename -> "name (1).ext"). Needs the mouse for ~5 s.
. "$PSScriptRoot\e2e-lib.ps1"
$work = Join-Path $env:TEMP 'ut-e2e-drop'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
$src = Join-Path $work 'src'; $dest = Join-Path $work 'dest'
New-Item -ItemType Directory -Force $src, $dest | Out-Null
'one' | Set-Content "$src\a.txt"; 'two' | Set-Content "$src\b.txt"

[void](Start-App -Fresh)
Check 'app starts and the shell reaches its prompt' (Wait-Dump 'phase=Input' 40)
Focus-App
Start-Sleep 1
Send-Text "Set-Location '$dest'"; Send-Enter
$doubled = $dest.Replace('\', '\\')   # the dump prints the cwd with Rust Debug escaping
$escaped = [regex]::Escape($doubled)
Check 'the pane reports its new cwd (OSC 7)' (Wait-Dump ('cwd=Some\("' + $escaped) 10)
# Never drop while the cwd is wrong: files would land in the user's real folders.
if (-not $script:Results[-1].Pass) { Stop-App; exit 1 }

function Drop-Files([string[]]$Files) {
  $r = New-Object UtIn+RECT
  [void][UtIn]::GetWindowRect((Get-AppHwnd), [ref]$r)
  $tx = [int](($r.Left + $r.Right) / 2); $ty = [int](($r.Top + $r.Bottom) / 2 + 60)
  # the drag source sits well outside the app window, on the primary screen's top-left area
  $sx = 200; $sy = 200
  $q = { param($v) '"' + $v + '"' }   # Start-Process joins the array with spaces, so quote each value
  $argsList = @('-STA', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (& $q "$PSScriptRoot\drag-source.ps1"), '-FilesBar', (& $q ($Files -join '|')),
    '-SourceX', $sx, '-SourceY', $sy, '-TargetX', $tx, '-TargetY', $ty, '-TargetHwnd', (Get-AppHwnd).ToInt64())
  $p = Start-Process powershell -ArgumentList $argsList -PassThru -WindowStyle Hidden -RedirectStandardError "$work\helper.err"
  $null = $p.Handle   # keeps ExitCode readable after a redirected run
  Start-Sleep -Milliseconds 2300   # drag in flight: the pane under the cursor shows the hover card
  [void](Save-Shot 'drop-hover')
  [void]$p.WaitForExit(30000)
  if ((Test-Path "$work\helper.err") -and (Get-Item "$work\helper.err").Length) { Write-Host ('helper: ' + ((Get-Content "$work\helper.err" -Raw) -replace '\s+', ' ').Substring(0, 300)) }
  return $p.ExitCode
}

$code = Drop-Files @("$src\a.txt", "$src\b.txt")
Check 'the drag source ran' ($code -eq 0) "exit $code"
$ok = $false; for ($i = 0; $i -lt 40 -and -not $ok; $i++) { $ok = (Test-Path "$dest\a.txt") -and (Test-Path "$dest\b.txt"); if (-not $ok) { Start-Sleep -Milliseconds 250 } }
Check 'both files were copied into the pane cwd' $ok
if ($ok) { Check 'content is intact' ((Get-Content "$dest\a.txt" -Raw).Trim() -eq 'one') }
Start-Sleep -Milliseconds 600
[void](Save-Shot 'drop-done')

$code = Drop-Files @("$src\a.txt", "$src\b.txt")
Start-Sleep -Milliseconds 1500
[void](Save-Shot 'drop-conflict')
Focus-App
Send-Enter   # Rename is the default button
$ok = $false; for ($i = 0; $i -lt 40 -and -not $ok; $i++) { $ok = (Test-Path "$dest\a (1).txt") -and (Test-Path "$dest\b (1).txt"); if (-not $ok) { Start-Sleep -Milliseconds 250 } }
Check 'conflict dialog: Enter renames to "name (1).ext"' $ok ((Get-ChildItem $dest).Name -join ', ')
Check 'the originals were not overwritten' ((Get-Content "$dest\a.txt" -Raw).Trim() -eq 'one')

Stop-App
$script:Results | Format-Table -AutoSize
if ($script:Results | Where-Object { -not $_.Pass }) { exit 1 }
