# Run the debug build against ISOLATED data folders (never touches %APPDATA%\UselessTerminal, which the previous
# WPF version may still be using) and with the DevTools port open for scripted checks.
#   scripts\dev-run.ps1 [-Port 9222] [-Fresh]
param([int]$Port = 9222, [switch]$Fresh, [string]$Exe = "$PSScriptRoot\..\target\debug\UselessTerminal.exe")
$root = Join-Path $env:TEMP 'ut-dev'
if ($Fresh -and (Test-Path $root)) { Remove-Item -Recurse -Force $root }
New-Item -ItemType Directory -Force "$root\roaming", "$root\local" | Out-Null
$env:UT_APPDATA = "$root\roaming"
$env:UT_LOCALAPPDATA = "$root\local"
$env:UT_DEBUG_PORT = "$Port"
$env:UT_LOG = 'info'
$p = Start-Process -FilePath (Resolve-Path $Exe) -PassThru -RedirectStandardError "$root\stderr.log" -RedirectStandardOutput "$root\stdout.log"
"started pid $($p.Id); data in $root; devtools on http://127.0.0.1:$Port"
