param(
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

& cargo build -p termior --quiet
if ($LASTEXITCODE -ne 0) {
    throw "Could not build the Termior desktop binary"
}
$binary = (Resolve-Path (Join-Path $repoRoot "target\debug\termior.exe")).Path

$targetRoot = (Resolve-Path (Join-Path $repoRoot "target")).Path
$runRoot = Join-Path $targetRoot ("markdown-preview-smoke-" + [guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $runRoot)
$resolvedRunRoot = (Resolve-Path $runRoot).Path
if (-not $resolvedRunRoot.StartsWith($targetRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to use smoke directory outside target: $resolvedRunRoot"
}

$app = $null
try {
    $workspace = Join-Path $resolvedRunRoot "workspace"
    [void](New-Item -ItemType Directory -Path $workspace)
    [System.IO.File]::WriteAllText(
        (Join-Path $workspace "preview-smoke.md"),
        "# Markdown preview smoke`n`n- renders the active document`n"
    )

    $env:TERMIOR_DATA_DIR = Join-Path $resolvedRunRoot "data"
    $env:TERMIOR_WORKSPACE = $workspace
    $env:TERMIOR_MARKDOWN_PREVIEW_SMOKE_TEST = "1"
    $env:RUST_BACKTRACE = "1"
    $appOut = Join-Path $resolvedRunRoot "app.stdout.log"
    $appErr = Join-Path $resolvedRunRoot "app.stderr.log"
    $app = Start-Process -FilePath $binary `
        -WorkingDirectory $repoRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $appOut `
        -RedirectStandardError $appErr `
        -PassThru

    if (-not $app.WaitForExit($TimeoutSeconds * 1000)) {
        throw "Markdown preview smoke timed out after $TimeoutSeconds seconds"
    }

    [string]$stdout = Get-Content -Raw $appOut -ErrorAction SilentlyContinue
    [string]$stderr = Get-Content -Raw $appErr -ErrorAction SilentlyContinue
    if ([string]::IsNullOrWhiteSpace($stdout) -or
        $stdout -notmatch "TERMIOR_MARKDOWN_PREVIEW_SMOKE_OK") {
        throw "The Preview button did not open a Markdown preview.`nSTDOUT:`n$stdout`nSTDERR:`n$stderr"
    }
    if ($stderr -match "RefCell already borrowed|panicked at") {
        throw "Markdown preview emitted a panic despite reporting ready.`n$stderr"
    }

    Write-Output "TERMIOR_MARKDOWN_PREVIEW_SMOKE_OK: active Markdown document opened as a preview"
}
finally {
    Remove-Item Env:\TERMIOR_DATA_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_WORKSPACE -ErrorAction SilentlyContinue
    Remove-Item Env:\TERMIOR_MARKDOWN_PREVIEW_SMOKE_TEST -ErrorAction SilentlyContinue
    if ($app -and -not $app.HasExited) {
        Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
        [void]$app.WaitForExit(5000)
    }
    if ($app) { $app.Dispose() }
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
