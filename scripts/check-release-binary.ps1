param(
    [ValidateSet("windows", "macos", "linux")]
    [string]$Platform,
    [int]$MaxBinaryMiB = 60
)

$ErrorActionPreference = "Stop"
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$binaryName = if ($Platform -eq "windows") { "termior.exe" } else { "termior" }
$binary = Join-Path $repoRoot "target/release/$binaryName"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "Release binary not found: $binary"
}
$info = Get-Item -LiteralPath $binary
$sizeMiB = $info.Length / 1MB
Write-Output ("Release binary: {0:N2} MiB ({1})" -f $sizeMiB, $info.FullName)
if ($sizeMiB -gt $MaxBinaryMiB) {
    throw ("Release binary exceeds {0} MiB" -f $MaxBinaryMiB)
}

$treePath = Join-Path $repoRoot "target/release-dependency-tree.txt"
cargo tree --workspace | Set-Content -LiteralPath $treePath -Encoding utf8NoBOM
Write-Output "Dependency tree: $treePath"
