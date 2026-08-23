<#
.SYNOPSIS
    Verifies that the Composer and workspace status bar stay inside a 760px-tall client area.

.DESCRIPTION
    Launches an isolated Termior instance with an expanded Composer, moves its window off-screen,
    resizes it to the default logical height, and checks pixels rendered by the real Windows GPUI
    backend. The test fails when the workspace status bar is pushed below the client area.

.EXAMPLE
    cargo build -p termior
    ./scripts/windows-bottom-chrome-smoke.ps1
    ./scripts/windows-bottom-chrome-smoke.ps1 -Binary target/codex-layout/debug/termior.exe
#>
param(
    [string]$Binary = "",
    [int]$ClientHeight = 760,
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Binary) { $Binary = Join-Path $repoRoot "target\debug\termior.exe" }
$Binary = (Resolve-Path -LiteralPath $Binary).Path

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TermiorBottomChromeSmoke {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int width, int height, uint flags);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
}
'@

$runRoot = Join-Path $repoRoot ("target\layout-smoke-" + [guid]::NewGuid().ToString("N"))
$dataDir = Join-Path $runRoot "data"
$shotPath = Join-Path $repoRoot "target\windows-bottom-chrome-smoke.png"
[void](New-Item -ItemType Directory -Force -Path $dataDir)

$settings = @{
    version = 2
    appearance = "light"
    theme_id = "default-light"
    light_theme_id = "default-light"
    dark_theme_id = "default"
    editor_theme_id = "default"
} | ConvertTo-Json
[IO.File]::WriteAllText((Join-Path $dataDir "Termior-settings.json"), $settings)

$sessions = @{
    sessions = @(
        @{
            id = "layout-smoke"
            title = "Layout smoke"
            agent_id = $null
            messages = @(
                @{ role = "user"; content = "Verify the bottom Agent bar layout." },
                @{ role = "assistant"; content = "The Composer is expanded for this layout check." }
            )
        }
    )
    active_id = "layout-smoke"
} | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText((Join-Path $dataDir "Termior-ai-sessions.json"), $sessions)

$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $Binary
$startInfo.WorkingDirectory = $repoRoot
$startInfo.UseShellExecute = $false
$startInfo.Environment["TERMIOR_DATA_DIR"] = $dataDir
$startInfo.Environment["TERMIOR_WORKSPACE"] = $repoRoot
$app = [Diagnostics.Process]::Start($startInfo)
$previousDpi = [IntPtr]::Zero

try {
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $handle = [IntPtr]::Zero
    while ((Get-Date) -lt $deadline) {
        $app.Refresh()
        if ($app.HasExited) { throw "Termior exited before its main window appeared" }
        $handle = $app.MainWindowHandle
        if ($handle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($handle -eq [IntPtr]::Zero) { throw "Timed out waiting for the Termior window" }

    $previousDpi = [TermiorBottomChromeSmoke]::SetThreadDpiAwarenessContext([IntPtr](-4))
    $windowRect = New-Object TermiorBottomChromeSmoke+RECT
    $clientRect = New-Object TermiorBottomChromeSmoke+RECT
    [void][TermiorBottomChromeSmoke]::GetWindowRect($handle, [ref]$windowRect)
    [void][TermiorBottomChromeSmoke]::GetClientRect($handle, [ref]$clientRect)
    $windowWidth = $windowRect.Right - $windowRect.Left
    $windowHeight = $windowRect.Bottom - $windowRect.Top
    $clientHeightPhysical = $clientRect.Bottom - $clientRect.Top
    $nonClientHeight = $windowHeight - $clientHeightPhysical
    $scale = [TermiorBottomChromeSmoke]::GetDpiForWindow($handle) / 96.0
    $targetHeight = [int][Math]::Round($ClientHeight * $scale) + $nonClientHeight

    # Keep the real Windows renderer active without covering the developer's desktop.
    [void][TermiorBottomChromeSmoke]::SetWindowPos(
        $handle,
        [IntPtr]::Zero,
        -20000,
        -20000,
        $windowWidth,
        $targetHeight,
        0x0014
    )
    Start-Sleep -Seconds 2

    $resized = New-Object TermiorBottomChromeSmoke+RECT
    [void][TermiorBottomChromeSmoke]::GetWindowRect($handle, [ref]$resized)
    $width = $resized.Right - $resized.Left
    $height = $resized.Bottom - $resized.Top
    $bitmap = New-Object Drawing.Bitmap $width, $height
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $hdc = $graphics.GetHdc()
            try {
                if (-not [TermiorBottomChromeSmoke]::PrintWindow($handle, $hdc, 2)) {
                    throw "PrintWindow failed"
                }
            }
            finally { $graphics.ReleaseHdc($hdc) }
        }
        finally { $graphics.Dispose() }
        $bitmap.Save($shotPath, [Drawing.Imaging.ImageFormat]::Png)

        $statusOffset = [int][Math]::Round(7.5 * $scale)
        $titlebarOffset = [int][Math]::Round(15.0 * $scale)
        $statusPixel = $bitmap.GetPixel([int]($width / 2), $height - $statusOffset)
        $chromePixel = $bitmap.GetPixel([int]($width / 2), $titlebarOffset)
        $distance = [Math]::Abs([int]$statusPixel.R - [int]$chromePixel.R) +
            [Math]::Abs([int]$statusPixel.G - [int]$chromePixel.G) +
            [Math]::Abs([int]$statusPixel.B - [int]$chromePixel.B)
        if ($distance -ge 8) {
            throw "Bottom chrome is clipped: the status sample does not match the titlebar chrome (distance=$distance, screenshot=$shotPath)"
        }
        Write-Output "TERMIOR_BOTTOM_CHROME_OK distance=$distance screenshot=$shotPath"
    }
    finally { $bitmap.Dispose() }
}
finally {
    if ($previousDpi -ne [IntPtr]::Zero) {
        [void][TermiorBottomChromeSmoke]::SetThreadDpiAwarenessContext($previousDpi)
    }
    if ($app -and -not $app.HasExited) {
        $app.Kill($true)
        [void]$app.WaitForExit(5000)
    }
    if ($app) { $app.Dispose() }
    $resolvedRunRoot = [IO.Path]::GetFullPath($runRoot)
    $resolvedTarget = [IO.Path]::GetFullPath((Join-Path $repoRoot "target"))
    if ($resolvedRunRoot.StartsWith($resolvedTarget + [IO.Path]::DirectorySeparatorChar)) {
        Remove-Item -LiteralPath $resolvedRunRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
