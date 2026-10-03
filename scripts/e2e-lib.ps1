# Helpers for scripted end-to-end checks of the real app. Dot-source this file.
#   . scripts\e2e-lib.ps1
# The app runs against ISOLATED data folders and writes its active pane (stats + text) to $script:Dump about every
# 500 ms (UT_DUMP); `<dump>.shot` triggers a screenshot, `<dump>.quit` closes it.

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class UtIn {
  [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit)] struct U { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public U u; }
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] i, int size);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, int data, UIntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
  delegate bool EnumProc(IntPtr h, IntPtr l);
  // The app's real window = its largest visible top-level window (Process.MainWindowHandle can return a 16x16 tray helper).
  public static IntPtr BigWindow(uint pid) {
    IntPtr best = IntPtr.Zero; long area = 0;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      RECT r;
      var cls = new System.Text.StringBuilder(64); GetClassName(h, cls, 64);
      // debug builds are console-subsystem apps: their console window is not the app window
      if (p == pid && IsWindowVisible(h) && cls.ToString() != "ConsoleWindowClass" && GetWindowRect(h, out r)) {
        long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
        if (a > area && a >= 200 * 200) { area = a; best = h; }
      }
      return true;
    }, IntPtr.Zero);
    return best;
  }
  public static void Key(ushort vk, bool down, bool ext) {
    var i = new INPUT[1];
    i[0].type = 1;
    i[0].u.ki.wVk = vk;
    i[0].u.ki.dwFlags = (down ? 0u : 2u) | (ext ? 1u : 0u);
    SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
  public static void Unicode(char c) {
    var i = new INPUT[2];
    i[0].type = 1; i[0].u.ki.wScan = c; i[0].u.ki.dwFlags = 4;
    i[1].type = 1; i[1].u.ki.wScan = c; i[1].u.ki.dwFlags = 4 | 2;
    SendInput(2, i, Marshal.SizeOf(typeof(INPUT)));
  }
}
'@

$script:Exe = if ($env:UT_E2E_EXE) { $env:UT_E2E_EXE } else { Join-Path $PSScriptRoot '..\target\debug\UselessTerminal.exe' }
# One data dir per run: single-instance is keyed on it, so concurrent runs (several agents, a dev instance) never
# forward to each other or delete each other's data. Override with UT_E2E_ROOT to inspect a known folder.
$script:Root = if ($env:UT_E2E_ROOT) { $env:UT_E2E_ROOT } else { Join-Path $env:TEMP "ut-e2e-$PID" }
$script:Dump = Join-Path $script:Root 'dump.txt'
$script:Proc = $null

function Start-App([string[]]$AppArgs = @(), [switch]$Fresh) {
  if ($Fresh -and (Test-Path $script:Root)) { Remove-Item -Recurse -Force $script:Root }
  New-Item -ItemType Directory -Force "$script:Root\roaming", "$script:Root\local" | Out-Null
  Remove-Item "$($script:Dump)*" -Force -ErrorAction SilentlyContinue
  $env:UT_APPDATA = "$script:Root\roaming"; $env:UT_LOCALAPPDATA = "$script:Root\local"; $env:UT_DUMP = $script:Dump
  # Run a COPY: a running test never locks target\debug\UselessTerminal.exe, so `cargo build` keeps working.
  $copy = Join-Path $script:Root 'UselessTerminal.exe'
  Copy-Item (Resolve-Path $script:Exe).Path $copy -Force
  $sp = @{ FilePath = $copy; PassThru = $true }
  if ($AppArgs.Count) { $sp.ArgumentList = $AppArgs }
  $script:Proc = Start-Process @sp
  return $script:Proc
}

# Input is only ever sent while OUR window is in front; anything else would type into the user's other windows.
function Get-AppHwnd { if ($script:Proc -and -not $script:Proc.HasExited) { [UtIn]::BigWindow([uint32]$script:Proc.Id) } else { [IntPtr]::Zero } }

function Assert-Fg {
  $script:Proc.Refresh()
  if (-not $script:Proc -or $script:Proc.HasExited -or (Get-AppHwnd) -eq [IntPtr]::Zero -or [UtIn]::GetForegroundWindow() -ne (Get-AppHwnd)) {
    throw 'e2e: the app window is not in the foreground; refusing to send input'
  }
}

function Focus-App([switch]$Click) {
  for ($i = 0; $i -lt 40 -and (Get-AppHwnd) -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 200 }
  if ((Get-AppHwnd) -eq [IntPtr]::Zero) { throw 'e2e: the app has no window' }
  # The ALT-key trick lets a background process take the foreground.
  [UtIn]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [UtIn]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
  [void][UtIn]::ShowWindow((Get-AppHwnd), 9)
  [void][UtIn]::SetForegroundWindow((Get-AppHwnd))
  Start-Sleep -Milliseconds 300
  Assert-Fg
  # Optional real click in the terminal area (the app must NOT need it to accept typing).
  $r = New-Object UtIn+RECT
  if ($Click -and [UtIn]::GetWindowRect((Get-AppHwnd), [ref]$r)) {
    [void][UtIn]::SetCursorPos([int](($r.Left + $r.Right) / 2 + 100), [int](($r.Top + $r.Bottom) / 2))
    [UtIn]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40; [UtIn]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
  }
  Start-Sleep -Milliseconds 400
  # The first shell is the default pwsh: keep everything these tests type out of the user's real PSReadLine history.
  if (-not $script:HistoryIsolated) { $script:HistoryIsolated = $true; Isolate-History pwsh }
}

# Test commands must never end up in the user's real shell histories. Run once per shell, at its prompt:
# pwsh / Windows PowerShell -> PSReadLine history file in the run's own folder; bash (Git Bash, WSL) -> no history file.
function Isolate-History([ValidateSet('pwsh', 'bash')][string]$Kind = 'pwsh') {
  # PSReadLine appends a line to the history file when it is accepted, before this takes effect. Its default
  # AddToHistoryHandler keeps lines that look sensitive (the word "secret" is enough) out of the file: tag this one.
  if ($Kind -eq 'pwsh') { Send-Text "Set-PSReadLineOption -HistorySavePath '$script:Root\psrl-history.txt' # secret" } else { Send-Text 'unset HISTFILE' }
  Send-Enter
  Start-Sleep -Milliseconds 700
}

function Get-Dump { if (Test-Path $script:Dump) { Get-Content $script:Dump -Raw -ErrorAction SilentlyContinue } else { '' } }

function Wait-Dump([string]$Pattern, [int]$TimeoutSec = 30) {
  $end = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $end) {
    $d = Get-Dump
    if ($d -match $Pattern) { return $true }
    Start-Sleep -Milliseconds 300
  }
  return $false
}

function Send-Text([string]$Text) { Assert-Fg; foreach ($c in $Text.ToCharArray()) { [UtIn]::Unicode($c); Start-Sleep -Milliseconds 15 } }
function Send-Vk([int]$Vk, [switch]$Ctrl, [switch]$Shift, [switch]$Alt, [switch]$Ext) {
  Assert-Fg
  if ($Ctrl) { [UtIn]::Key(0x11, $true, $false) }; if ($Shift) { [UtIn]::Key(0x10, $true, $false) }; if ($Alt) { [UtIn]::Key(0x12, $true, $false) }
  [UtIn]::Key($Vk, $true, [bool]$Ext); Start-Sleep -Milliseconds 20; [UtIn]::Key($Vk, $false, [bool]$Ext)
  if ($Alt) { [UtIn]::Key(0x12, $false, $false) }; if ($Shift) { [UtIn]::Key(0x10, $false, $false) }; if ($Ctrl) { [UtIn]::Key(0x11, $false, $false) }
  Start-Sleep -Milliseconds 60
}
function Send-Enter { Send-Vk 0x0D }
# A real AltGr chord: right Alt (extended) held while the key is pressed. Windows adds the implicit Ctrl itself.
function Send-AltGr([int]$Vk) {
  Assert-Fg
  [UtIn]::Key(0xA5, $true, $true); Start-Sleep -Milliseconds 30
  [UtIn]::Key($Vk, $true, $false); Start-Sleep -Milliseconds 30; [UtIn]::Key($Vk, $false, $false); Start-Sleep -Milliseconds 30
  [UtIn]::Key(0xA5, $false, $true); Start-Sleep -Milliseconds 80
}

function Save-Shot([string]$Name) {
  New-Item -ItemType File -Force "$($script:Dump).shot" | Out-Null
  $end = (Get-Date).AddSeconds(8)
  while ((Test-Path "$($script:Dump).shot") -and (Get-Date) -lt $end) { Start-Sleep -Milliseconds 200 }
  Start-Sleep -Milliseconds 400
  $dest = Join-Path $script:Root "$Name.png"
  if (Test-Path "$($script:Dump).png") { Move-Item "$($script:Dump).png" $dest -Force; return $dest }
}

function Stop-App {
  if ($script:Proc -and -not $script:Proc.HasExited) {
    New-Item -ItemType File -Force "$($script:Dump).quit" | Out-Null
    if (-not $script:Proc.WaitForExit(8000)) { Stop-Process -Id $script:Proc.Id -Force }
  }
}

$script:Results = @()
function Check([string]$Name, [bool]$Ok, [string]$Detail = '') {
  $script:Results += [pscustomobject]@{ Check = $Name; Pass = $Ok; Detail = $Detail }
  Write-Host ("{0,-5} {1} {2}" -f ($(if ($Ok) { 'PASS' } else { 'FAIL' }), $Name, $Detail))
}

# A terminating error in a test script (lost foreground, timeout...) must not leave the app running.
trap { Write-Host $_.ScriptStackTrace; Stop-App; throw $_ }
