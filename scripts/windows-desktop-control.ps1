param(
 [Parameter(Mandatory)][int]$AppProcessId,
 [Parameter(Mandatory)][string]$OutputPath,
 [ValidateSet('capture','show','click','text','key')][string]$Action = 'capture',
 [int]$X = 0, [int]$Y = 0, [string]$Text = '', [int]$KeyCode = 13
)
$ErrorActionPreference = 'Stop'
$taskProcess = Get-Process -Id $AppProcessId
if ($taskProcess.ProcessName -notin @('termior','agent_acceptance')) { throw 'The desktop control helper only targets a Termior acceptance process' }
Add-Type -AssemblyName System.Drawing
if (-not ('AcceptanceDesktop' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class AcceptanceDesktop {
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
 public delegate bool EnumProc(IntPtr h, IntPtr p);
 [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out Rect r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 public struct Rect {public int L,T,R,B;}
 public static IntPtr Find(uint pid) {
  IntPtr result=IntPtr.Zero;
  EnumWindows((h,l)=> { uint p; GetWindowThreadProcessId(h,out p); Rect r; GetWindowRect(h,out r);
   if(p==pid && r.R-r.L>500 && r.B-r.T>300) {result=h;return false;} return true;},IntPtr.Zero);
  return result;
 }
}
'@
}
[void][AcceptanceDesktop]::SetProcessDPIAware()
[void][AcceptanceDesktop]::SetThreadDpiAwarenessContext([IntPtr](-4))
$taskPid = $AppProcessId
$taskWindow = $taskProcess.MainWindowHandle
if ($taskWindow -eq [IntPtr]::Zero) { $taskWindow = [AcceptanceDesktop]::Find($taskPid) }
if ($taskWindow -eq [IntPtr]::Zero) { throw "No test window for process $taskPid" }
[void][AcceptanceDesktop]::ShowWindow($taskWindow,4)
Start-Sleep -Milliseconds 100
if ($Action -eq 'show') { [void][AcceptanceDesktop]::ShowWindow($taskWindow,9); [void][AcceptanceDesktop]::SetForegroundWindow($taskWindow); Start-Sleep -Milliseconds 400 }
if ($Action -eq 'click') {
 # Coordinates match this window's PrintWindow capture. Dispatch only to the
 # selected HWND, so another foreground application never receives the click.
 $point=[IntPtr](($Y -shl 16) -bor ($X -band 0xffff))
 [void][AcceptanceDesktop]::PostMessage($taskWindow,0x200,[IntPtr]::Zero,$point)
 [void][AcceptanceDesktop]::PostMessage($taskWindow,0x201,[IntPtr]1,$point)
 Start-Sleep -Milliseconds 80
 [void][AcceptanceDesktop]::PostMessage($taskWindow,0x202,[IntPtr]::Zero,$point)
 Start-Sleep -Milliseconds 300
}
if ($Action -eq 'text') { foreach($ch in $Text.ToCharArray()) { [void][AcceptanceDesktop]::PostMessage($taskWindow,0x102,[IntPtr][int]$ch,[IntPtr]::Zero) }; Start-Sleep -Milliseconds 300 }
if ($Action -eq 'key') { [void][AcceptanceDesktop]::PostMessage($taskWindow,0x100,[IntPtr]$KeyCode,[IntPtr]1); [void][AcceptanceDesktop]::PostMessage($taskWindow,0x101,[IntPtr]$KeyCode,[IntPtr]1); Start-Sleep -Milliseconds 300 }
$rect = [AcceptanceDesktop+Rect]::new()
[void][AcceptanceDesktop]::GetWindowRect($taskWindow,[ref]$rect)
$bitmap = [System.Drawing.Bitmap]::new($rect.R-$rect.L,$rect.B-$rect.T)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$dc = $graphics.GetHdc()
try { if(-not [AcceptanceDesktop]::PrintWindow($taskWindow,$dc,2)){throw 'PrintWindow failed'} }
finally {$graphics.ReleaseHdc($dc);$graphics.Dispose()}
$bitmap.Save([System.IO.Path]::GetFullPath($OutputPath),[System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()
Write-Output "pid=$taskPid hwnd=$taskWindow screenshot=$OutputPath"
