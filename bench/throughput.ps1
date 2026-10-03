# Throughput / latency benchmark of the REAL app (isolated data dirs, release build by default).
#   bench\throughput.ps1 [-ExePath target\release\UselessTerminal.exe] [-SizeMB 200] [-Keys 100] [-WindowSec 10]
#
# Measures, in a clean `pwsh -NoProfile` tab (a heavy profile would dominate every number):
#   * flood throughput over a WindowSec window, for two producers: `type` (cmd writes line by line) and `copy /b file CON`
#     (large blocks). The window is read from the app's byte counter, then Ctrl+C ends the flood;
#   * how long output keeps arriving after Ctrl+C;
#   * keystroke -> echo latency (p50/p99) at an idle prompt;
#   * `seq 1 5000000` in WSL, wall time, when WSL is present.
# Budgets (§19): >= 10 MB/s, Ctrl+C <= 300 ms, echo p99 <= 100 ms.
#
# READ THE NUMBERS WITH THE MACHINE IN MIND: ConPTY's conhost handles ~12k console writes per second on the reference
# laptop, so a producer that writes line by line (`type`, WSL) is capped near 1 MB/s whatever the terminal does, while
# block writers reach ~10 MB/s (`ut-pty` test `perf_type_of_a_big_file`, `UT_PERF_CMD`). The terminal's own parse+grid
# stage measures 12-22 MB/s (`ut-term` test `perf_sink_throughput`).
# Figures come from the debug dump (UT_DUMP_MS=20): <= 20 ms of sampling error; they cover app write -> shell echo -> app
# read, not painting. Needs the foreground: do not touch the keyboard while it runs.
param([string]$ExePath, [int]$SizeMB = 200, [int]$Keys = 100, [int]$WindowSec = 10)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\..\scripts\e2e-lib.ps1"
# NB: the parameter is not called $Exe: e2e-lib.ps1 owns $script:Exe, the same variable, and would overwrite it.
if (-not $ExePath) { $ExePath = Join-Path $PSScriptRoot '..\target\release\UselessTerminal.exe' }
if (-not (Test-Path $ExePath)) { $ExePath = Join-Path $PSScriptRoot '..\target\debug\UselessTerminal.exe'; Write-Warning 'release build not found: benchmarking the DEBUG build' }
$script:Exe = (Resolve-Path $ExePath).Path
Write-Host "benchmarking $script:Exe"

$big = Join-Path $env:TEMP "ut-bench-$SizeMB.txt"
if (-not (Test-Path $big) -or (Get-Item $big).Length -lt $SizeMB * 1MB) {
  $line = [Text.Encoding]::ASCII.GetBytes(('x' * 77) + "`r`n")
  $fs = [IO.File]::Create($big)
  try { for ($i = 0; $i -lt ($SizeMB * 1MB / $line.Length); $i++) { $fs.Write($line, 0, $line.Length) } } finally { $fs.Dispose() }
}

function Get-Rx { $m = [regex]::Match("$(Get-Dump)", 'rx=(\d+)'); if ($m.Success) { [long]$m.Groups[1].Value } else { -1 } }
function Get-PanePid { $m = [regex]::Match("$(Get-Dump)", '# pid=(\d+)'); if ($m.Success) { [uint32]$m.Groups[1].Value } else { 0 } }
function Wait-Marker([string]$Pat, [int]$TimeoutSec) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) { if ((Get-Dump) -match $Pat) { return $sw.Elapsed.TotalSeconds }; Start-Sleep -Milliseconds 20 }
  return $null
}
function Pct([double[]]$v, [double]$p) { $s = $v | Sort-Object; $s[[math]::Min($s.Count - 1, [int][math]::Ceiling($p * $s.Count) - 1)] }

# Flood for WindowSec, report MB/s from the byte counter, Ctrl+C, and how long output kept coming afterwards.
function Measure-Flood([string]$Name, [string]$Command) {
  Send-Text $Command; Send-Enter
  Start-Sleep -Seconds 2                                  # let the producer ramp up
  $r0 = Get-Rx; $sw = [Diagnostics.Stopwatch]::StartNew()
  Start-Sleep -Seconds $WindowSec
  $r1 = Get-Rx; $secs = $sw.Elapsed.TotalSeconds
  $mbs = ($r1 - $r0) / 1MB / $secs
  Send-Vk 0x43 -Ctrl
  $sw.Restart(); $last = Get-Rx; $quiet = [Diagnostics.Stopwatch]::StartNew(); $stopAt = $null
  while ($sw.Elapsed.TotalSeconds -lt 15) {
    $rx = Get-Rx
    if ($rx -ne $last) { $last = $rx; $quiet.Restart() } elseif ($quiet.ElapsedMilliseconds -ge 150) { $stopAt = $sw.ElapsedMilliseconds - 150; break }
    Start-Sleep -Milliseconds 10
  }
  [void](Wait-Dump 'phase=Input' 30); Start-Sleep -Milliseconds 800
  $script:rows += [pscustomobject]@{ Test = "$Name flood"; Result = ('{0:N1} MB/s over {1:N0} s' -f $mbs, $secs); Pass = ($mbs -ge 10) }
  $script:rows += [pscustomobject]@{ Test = "${Name}: output stops after Ctrl+C"; Result = $(if ($null -ne $stopAt) { "$stopAt ms" } else { 'never stopped' }); Pass = ($null -ne $stopAt -and $stopAt -le 300) }
}

trap { Stop-App; throw $_ }   # never leave the app running when a check throws (e.g. lost foreground)
$env:UT_DUMP_MS = '20'
[void](Start-App -Fresh)
if (-not (Wait-Dump 'phase=Input' 40)) { throw 'the shell never reached its prompt' }
Focus-App
Write-Host "data dir: $script:Root"

# a clean shell in a new tab (second launch forwards `-- <command>` to this instance)
$before = Get-PanePid
Start-Process -FilePath (Join-Path $script:Root 'UselessTerminal.exe') -ArgumentList @('--', 'pwsh.exe', '-NoProfile', '-NoLogo') -WindowStyle Hidden | Out-Null
$end = (Get-Date).AddSeconds(30); while ((Get-Date) -lt $end -and (Get-PanePid) -eq $before) { Start-Sleep -Milliseconds 200 }
if ((Get-PanePid) -eq $before) { throw 'the clean pwsh tab did not open' }
if (-not (Wait-Dump 'phase=Input' 30)) { throw 'the clean pwsh tab never reached its prompt' }
Focus-App; Isolate-History pwsh; Start-Sleep 1

$rows = @()
Measure-Flood 'type (line writes)' "cmd /c type `"$big`""
Measure-Flood 'copy /b CON (block writes)' "cmd /c copy /b `"$big`" CON"

# keystroke -> echo latency at an idle prompt
$lat = @()
for ($i = 0; $i -lt $Keys; $i++) {
  $rx0 = Get-Rx
  $sw = [Diagnostics.Stopwatch]::StartNew()
  [UtIn]::Unicode([char](97 + $i % 26))
  while ((Get-Rx) -le $rx0 -and $sw.ElapsedMilliseconds -lt 1000) { Start-Sleep -Milliseconds 1 }
  $lat += $sw.Elapsed.TotalMilliseconds
  Start-Sleep -Milliseconds 40
}
$p99 = Pct $lat 0.99
$rows += [pscustomobject]@{ Test = "keystroke echo ($Keys keys)"; Result = ('p50 {0:N0} ms / p99 {1:N0} ms' -f (Pct $lat 0.5), $p99); Pass = ($p99 -le 100) }
Send-Vk 0x43 -Ctrl   # drop the typed text

if (Get-Command wsl -ErrorAction SilentlyContinue) {
  Send-Text "wsl seq 1 5000000; echo ('DONE-'+(2+2))"; Send-Enter
  $t = Wait-Marker 'DONE-4\r?\n' 300
  $rows += [pscustomobject]@{ Test = 'wsl seq 1 5000000 (38.9 MB)'; Result = $(if ($t) { '{0:N1} s = {1:N1} MB/s' -f $t, (38888895 / 1MB / $t) } else { 'TIMEOUT' }); Pass = [bool]($t -and (38888895 / 1MB / $t) -ge 10) }
}

Stop-App
$rows | Format-Table -AutoSize
if ($rows | Where-Object { -not $_.Pass }) { exit 1 }
