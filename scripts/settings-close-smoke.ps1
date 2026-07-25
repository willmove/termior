# Opens Termior, opens Settings, closes it via remove_window, asserts no
# `window not found` / invalid-handle noise on stderr.
#
# Usage:
#   powershell -File scripts/settings-close-smoke.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$env:TERMIOR_SETTINGS_CLOSE_SMOKE_TEST = "1"
$env:RUST_LOG = "error"
$env:TERMIOR_WORKSPACE = $root

$stderrFile = Join-Path $env:TEMP "termior-settings-close-smoke.err.log"
$stdoutFile = Join-Path $env:TEMP "termior-settings-close-smoke.out.log"
Remove-Item $stderrFile, $stdoutFile -ErrorAction SilentlyContinue

Write-Host "Building termior (settings-close smoke)..."
& cargo build -p termior --quiet
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed"
}

$exe = Join-Path $root "target\debug\termior.exe"
Write-Host "Running $exe ..."
$proc = Start-Process -FilePath $exe `
    -WorkingDirectory $root `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdoutFile `
    -RedirectStandardError $stderrFile
$finished = $proc.WaitForExit(60000)
if (-not $finished) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Write-Error "settings-close smoke timed out after 60s"
}

$stdout = Get-Content $stdoutFile -Raw -ErrorAction SilentlyContinue
$stderr = Get-Content $stderrFile -Raw -ErrorAction SilentlyContinue

Write-Host "----- stdout -----"
Write-Host $stdout
Write-Host "----- stderr -----"
Write-Host $stderr

if ($stdout -notmatch "TERMIOR_SETTINGS_CLOSE_SMOKE_OK") {
    Write-Error "missing TERMIOR_SETTINGS_CLOSE_SMOKE_OK in stdout"
}

$bad = @()
if ($stderr -match "window not found") { $bad += "window not found" }
if ($stderr -match "Invalid window handle") { $bad += "Invalid window handle" }
# Win32 invalid-handle HRESULTs seen on zh-CN / en-US GPUI closes.
if ($stderr -match "0x80040102|0x80070578") { $bad += "invalid-hwnd-hresult" }
# Debug-layer probe must not ERROR when Graphics Tools optional feature is absent.
if ($stderr -match "0x887A002D") { $bad += "dxgi-sdk-component-missing" }

if ($bad.Count -gt 0) {
    Write-Error ("settings-close smoke failed; stderr contains: " + ($bad -join ", "))
}

Write-Host "settings-close smoke passed"
exit 0
