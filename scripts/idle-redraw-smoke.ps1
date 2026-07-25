# Verifies NFR-03 idle zero-redraw: after settle, WorkspaceView::render must not
# run during the probe window (no cx.notify / repeating animations).
#
# Usage:
#   powershell -File scripts/idle-redraw-smoke.ps1
#   powershell -File scripts/idle-redraw-smoke.ps1 -Seconds 10

param(
    [int]$Seconds = 10
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$env:TERMIOR_IDLE_REDRAW_PROBE = "$Seconds"
$env:RUST_LOG = "error"
$env:TERMIOR_WORKSPACE = $root

$stderrFile = Join-Path $env:TEMP "termior-idle-redraw-smoke.err.log"
$stdoutFile = Join-Path $env:TEMP "termior-idle-redraw-smoke.out.log"
Remove-Item $stderrFile, $stdoutFile -ErrorAction SilentlyContinue

Write-Host "Building termior (idle-redraw smoke, ${Seconds}s probe)..."
& cargo build -p termior --quiet
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed"
}

$exe = Join-Path $root "target\debug\termior.exe"
$timeoutMs = [Math]::Max(($Seconds + 20) * 1000, 60000)
Write-Host "Running $exe (timeout ${timeoutMs}ms)..."
$proc = Start-Process -FilePath $exe `
    -WorkingDirectory $root `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdoutFile `
    -RedirectStandardError $stderrFile
$finished = $proc.WaitForExit($timeoutMs)
if (-not $finished) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Write-Error "idle-redraw smoke timed out"
}

$stdout = Get-Content $stdoutFile -Raw -ErrorAction SilentlyContinue
$stderr = Get-Content $stderrFile -Raw -ErrorAction SilentlyContinue

Write-Host "----- stdout -----"
Write-Host $stdout
Write-Host "----- stderr -----"
Write-Host $stderr

if ($stdout -notmatch "TERMIOR_IDLE_REDRAW_FRAMES=(\d+)") {
    Write-Error "missing TERMIOR_IDLE_REDRAW_FRAMES in stdout"
}
$frames = [int]$Matches[1]
if ($frames -ne 0) {
    Write-Error "idle redraw frames expected 0, got $frames (NFR-03)"
}

Write-Host "idle-redraw smoke passed (0 frames in ${Seconds}s)"
exit 0
