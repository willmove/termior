<#
.SYNOPSIS
    Captures Termior's chrome to target/ui-shots/ so UI changes can be reviewed as images.

.DESCRIPTION
    Launches the app against a throwaway data directory seeded with the requested theme,
    so a run is reproducible and never touches the developer's real settings. Screens are
    grabbed off the composited desktop rather than with PrintWindow: the window renders
    through DirectComposition, which PrintWindow captures as black.

.EXAMPLE
    ./scripts/ui-screenshot.ps1 -Label before
    ./scripts/ui-screenshot.ps1 -Theme "tokyo-night" -Appearance dark -Maximized -Settings
#>
param(
    # Prefix for the output file names, e.g. "before" -> before-main.png.
    [string]$Label = "main",
    [string]$Theme = "default",
    [ValidateSet("light", "dark", "follow_system")]
    [string]$Appearance = "dark",
    # Maximize before capturing, to check the client-side titlebar's maximized padding.
    [switch]$Maximized,
    # Also open the settings window (Ctrl+,) and capture it.
    [switch]$Settings,
    [int]$TimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class Shot {
    private delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(
        IntPtr hWnd, int attr, out RECT value, int size);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    /// Top-level visible windows owned by the given process, newest last.
    public static IntPtr[] WindowsOf(uint target) {
        var found = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == target && IsWindowVisible(h)) { found.Add(h); }
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }
}
"@

# Physical pixels, otherwise every coordinate is scaled and the capture is cropped.
[void][Shot]::SetProcessDPIAware()

# DWMWA_EXTENDED_FRAME_BOUNDS: the visible frame. GetWindowRect would include the
# invisible resize border and pad the capture with desktop pixels.
$DWMWA_EXTENDED_FRAME_BOUNDS = 9
$SW_MAXIMIZE = 3

function Get-ProcessWindow([int]$processId) {
    , @([Shot]::WindowsOf([uint32]$processId))
}

function Wait-ForWindow([System.Diagnostics.Process]$process, [int]$expected, [int]$timeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $process.Refresh()
        if ($process.HasExited) { throw "Termior exited before a window appeared" }
        $found = Get-ProcessWindow $process.Id
        if ($found.Count -ge $expected) { return $found }
        Start-Sleep -Milliseconds 300
    }
    throw "Timed out waiting for $expected Termior window(s)"
}

function Save-WindowShot([IntPtr]$handle, [string]$path) {
    [void][Shot]::SetForegroundWindow($handle)
    Start-Sleep -Milliseconds 700

    $rect = New-Object Shot+RECT
    $size = [Runtime.InteropServices.Marshal]::SizeOf($rect)
    if ([Shot]::DwmGetWindowAttribute($handle, $DWMWA_EXTENDED_FRAME_BOUNDS, [ref]$rect, $size) -ne 0) {
        throw "Could not read the window frame bounds"
    }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) { throw "Window has no visible area" }

    $bitmap = New-Object System.Drawing.Bitmap $width, $height
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size) }
        finally { $graphics.Dispose() }
        $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally { $bitmap.Dispose() }
    Write-Output "$path ($width x $height)"
}

& cargo build -p termior --quiet
if ($LASTEXITCODE -ne 0) { throw "Could not build the Termior desktop binary" }
$binary = (Resolve-Path (Join-Path $repoRoot "target\debug\termior.exe")).Path

$shotDir = Join-Path $repoRoot "target\ui-shots"
[void](New-Item -ItemType Directory -Force -Path $shotDir)

$runRoot = Join-Path $repoRoot ("target\ui-shot-" + [guid]::NewGuid().ToString("N"))
$dataDir = Join-Path $runRoot "data"
[void](New-Item -ItemType Directory -Path $dataDir -Force)

# Seed the theme instead of clicking through the UI, so a run is reproducible.
# Schema v2: fixed appearance uses theme_id; FollowSystem uses the light/dark pair.
$lightTheme = if ($Theme -match '-light$' -or $Appearance -eq 'light') { $Theme } else { 'default-light' }
$darkTheme = if ($Theme -match '-light$') { ($Theme -replace '-light$', '') } else { $Theme }
$settingsJson = @{
    version          = 2
    appearance       = $Appearance
    theme_id         = $Theme
    light_theme_id   = $lightTheme
    dark_theme_id    = $darkTheme
    editor_theme_id  = 'default'
} | ConvertTo-Json
[System.IO.File]::WriteAllText((Join-Path $dataDir "Termior-settings.json"), $settingsJson)

$app = $null
try {
    $env:TERMIOR_DATA_DIR = $dataDir
    $env:TERMIOR_WORKSPACE = $repoRoot
    if ($Settings) { $env:TERMIOR_OPEN_SETTINGS = "1" }
    $app = Start-Process -FilePath $binary -WorkingDirectory $repoRoot -PassThru

    $main = (Wait-ForWindow $app 1 $TimeoutSeconds)[0]
    if ($Maximized) {
        [void][Shot]::ShowWindow($main, $SW_MAXIMIZE)
        Start-Sleep -Milliseconds 900
    }
    # Terminals and the file tree populate asynchronously; a blank first frame would
    # make the screenshot useless for reviewing chrome.
    Start-Sleep -Seconds 3
    Save-WindowShot $main (Join-Path $shotDir "$Label-main.png")

    if ($Settings) {
        $windows = Wait-ForWindow $app 2 15
        $settingsWindow = $windows | Where-Object { $_ -ne $main } | Select-Object -First 1
        if (-not $settingsWindow) { throw "Settings window did not appear" }
        Start-Sleep -Milliseconds 800
        Save-WindowShot $settingsWindow (Join-Path $shotDir "$Label-settings.png")
    }
}
finally {
    Remove-Item Env:\TERMIOR_DATA_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_WORKSPACE -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_OPEN_SETTINGS -ErrorAction SilentlyContinue
    if ($app -and -not $app.HasExited) {
        Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
        [void]$app.WaitForExit(5000)
    }
    if ($app) { $app.Dispose() }
    Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
}
