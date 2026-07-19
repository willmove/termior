param(
    [string]$Binary = "",
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Binary)) {
    $Binary = Join-Path $repoRoot "target\debug\termior.exe"
}
$Binary = (Resolve-Path $Binary).Path

$targetRoot = (Resolve-Path (Join-Path $repoRoot "target")).Path
$runRoot = Join-Path $targetRoot ("preview-smoke-" + [guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $runRoot)
$resolvedRunRoot = (Resolve-Path $runRoot).Path
if (-not $resolvedRunRoot.StartsWith($targetRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to use smoke directory outside target: $resolvedRunRoot"
}

$server = $null
$app = $null
try {
    $listener = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
    if ($listener) {
        throw "Port 3000 is already in use; preview smoke requires an isolated fixture server"
    }

    $serverOut = Join-Path $resolvedRunRoot "server.stdout.log"
    $serverErr = Join-Path $resolvedRunRoot "server.stderr.log"
    $python = (Get-Command python).Source
    $server = Start-Process -FilePath $python `
        -ArgumentList @("-u", "-m", "http.server", "3000", "--bind", "127.0.0.1") `
        -WorkingDirectory $repoRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $serverOut `
        -RedirectStandardError $serverErr `
        -PassThru

    $serverDeadline = (Get-Date).AddSeconds(5)
    while (-not (Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue)) {
        if ((Get-Date) -ge $serverDeadline) {
            throw "Fixture server did not start"
        }
        Start-Sleep -Milliseconds 100
    }

    $env:TERMIOR_DATA_DIR = Join-Path $resolvedRunRoot "data"
    $env:TERMIOR_WORKSPACE = $repoRoot
    $env:TERMIOR_PREVIEW_SMOKE_TEST = "1"
    $env:RUST_BACKTRACE = "1"
    $appOut = Join-Path $resolvedRunRoot "app.stdout.log"
    $appErr = Join-Path $resolvedRunRoot "app.stderr.log"
    $app = Start-Process -FilePath $Binary `
        -WorkingDirectory $repoRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $appOut `
        -RedirectStandardError $appErr `
        -PassThru

    if (-not $app.WaitForExit($TimeoutSeconds * 1000)) {
        throw "Preview smoke timed out after $TimeoutSeconds seconds"
    }

    [string]$stdout = Get-Content -Raw $appOut -ErrorAction SilentlyContinue
    [string]$stderr = Get-Content -Raw $appErr -ErrorAction SilentlyContinue
    [string]$requests = Get-Content -Raw $serverErr -ErrorAction SilentlyContinue
    if ([string]::IsNullOrWhiteSpace($stdout) -or
        $stdout -notmatch "TERMIOR_PREVIEW_SMOKE_OK") {
        throw "Preview never became ready.`nSTDOUT:`n$stdout`nSTDERR:`n$stderr"
    }
    if ([string]::IsNullOrWhiteSpace($requests) -or
        $requests -notmatch 'GET / HTTP/') {
        throw "Embedded preview did not request the fixture page.`nSERVER:`n$requests"
    }
    if ($stderr -match "RefCell already borrowed|panicked at") {
        throw "Preview emitted a panic despite reporting ready.`n$stderr"
    }

    Write-Output "TERMIOR_PREVIEW_SMOKE_OK: WebView mounted and requested http://localhost:3000/"
}
finally {
    Remove-Item Env:\TERMIOR_DATA_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_WORKSPACE -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_PREVIEW_SMOKE_TEST -ErrorAction SilentlyContinue
    if ($app -and -not $app.HasExited) {
        Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
        [void]$app.WaitForExit(5000)
    }
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        [void]$server.WaitForExit(5000)
    }
    if ($app) { $app.Dispose() }
    if ($server) { $server.Dispose() }
    if (Test-Path -LiteralPath $resolvedRunRoot) {
        for ($attempt = 0; $attempt -lt 10; $attempt++) {
            try {
                Remove-Item -LiteralPath $resolvedRunRoot -Recurse -Force
                break
            }
            catch {
                if ($attempt -eq 9) { throw }
                Start-Sleep -Milliseconds 100
            }
        }
    }
}
