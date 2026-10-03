# Helper for e2e-drop.ps1 (run with `powershell -STA`): a tiny always-on-top form that starts a REAL OLE file drag
# (CF_HDROP) and, on a background thread, moves the mouse over the target point, holds it there, and releases.
# The mouse is only touched when the form is the foreground window and the target pixel belongs to the expected window.
param(
  [Parameter(Mandatory)][string]$FilesBar,   # paths separated by '|'
  [Parameter(Mandatory)][int]$SourceX, [Parameter(Mandatory)][int]$SourceY,
  [Parameter(Mandatory)][int]$TargetX, [Parameter(Mandatory)][int]$TargetY,
  [Parameter(Mandatory)][long]$TargetHwnd,
  [int]$HoldMs = 1200
)
$Files = [string[]]($FilesBar -split '\|' | Where-Object { $_ })
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class DragMouse {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  // a lone Alt tap lets a background process take the foreground
  public static void AltTap() { keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero); }
  [DllImport("user32.dll")] static extern void mouse_event(uint f, int dx, int dy, int d, UIntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(POINT p);
  [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr h, uint flags);
  public static bool Over(int x, int y, long hwnd) {
    var h = GetAncestor(WindowFromPoint(new POINT { X = x, Y = y }), 2 /*GA_ROOT*/);
    return h.ToInt64() == hwnd;
  }
  public static void Press(int x, int y) { SetCursorPos(x, y); Thread.Sleep(80); mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); }
  // Runs while DoDragDrop is blocking the UI thread.
  public static void MoveAndRelease(int fx, int fy, int tx, int ty, long hwnd, int holdMs) {
    var t = new Thread(() => {
      Thread.Sleep(600);
      for (int i = 1; i <= 25; i++) { SetCursorPos(fx + (tx - fx) * i / 25, fy + (ty - fy) * i / 25); Thread.Sleep(20); }
      Thread.Sleep(holdMs);
      mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    });
    t.IsBackground = true; t.Start();
  }
}
'@

$form = New-Object Windows.Forms.Form
$form.FormBorderStyle = 'None'; $form.StartPosition = 'Manual'; $form.TopMost = $true; $form.ShowInTaskbar = $false
$form.Location = New-Object Drawing.Point ($SourceX - 100), ($SourceY - 40); $form.Size = New-Object Drawing.Size 200, 80
$label = New-Object Windows.Forms.Label
$label.Dock = 'Fill'; $label.Text = 'drag source (test)'; $label.TextAlign = 'MiddleCenter'; $label.BackColor = [Drawing.Color]::Gold
$form.Controls.Add($label)
$global:started = $false
$label.Add_MouseDown({
  $global:started = $true
  [DragMouse]::MoveAndRelease($SourceX, $SourceY, $TargetX, $TargetY, $TargetHwnd, $HoldMs)
  $d = New-Object Windows.Forms.DataObject
  $d.SetData([Windows.Forms.DataFormats]::FileDrop, [string[]]$Files)
  [void]$label.DoDragDrop($d, [Windows.Forms.DragDropEffects]::Copy)
  $form.Close()
})
$form.Add_Shown({
  [DragMouse]::AltTap(); $form.Activate(); [void][DragMouse]::SetForegroundWindow($form.Handle)
  $t = New-Object Windows.Forms.Timer; $t.Interval = 500
  $t.Add_Tick({
    param($sender, $e)
    $sender.Stop()
    if ([DragMouse]::GetForegroundWindow() -ne $form.Handle) { Write-Error 'drag source is not in the foreground; not touching the mouse'; $form.Close(); return }
    if (-not [DragMouse]::Over($TargetX, $TargetY, $TargetHwnd)) { Write-Error 'the target pixel is not over the target window; not touching the mouse'; $form.Close(); return }
    [DragMouse]::Press($SourceX, $SourceY)
  })
  $t.Start()
})
# safety net: never linger
$kill = New-Object Windows.Forms.Timer; $kill.Interval = 20000; $kill.Add_Tick({ $form.Close() }); $kill.Start()
[void]$form.ShowDialog()
if (-not $global:started) { exit 2 }
