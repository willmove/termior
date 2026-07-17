param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [Parameter(Mandatory = $true)]
    [ValidateSet("windows", "macos", "linux")]
    [string]$Platform,
    [int]$MaxBinaryMiB = 60,
    [int]$MaxArchiveMiB = 100
)

$ErrorActionPreference = "Stop"
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$distRoot = Join-Path $repoRoot "dist"
$artifactName = "termior-$Version-$Platform-x86_64"
$stage = Join-Path $distRoot $artifactName
$binaryName = if ($Platform -eq "windows") { "termior.exe" } else { "termior" }
$binary = Join-Path $repoRoot "target/release/$binaryName"

if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "Release binary not found: $binary"
}
$binaryInfo = Get-Item -LiteralPath $binary
$binaryMiB = $binaryInfo.Length / 1MB
if ($binaryMiB -gt $MaxBinaryMiB) {
    throw ("Release binary is {0:N2} MiB, above the {1} MiB limit" -f $binaryMiB, $MaxBinaryMiB)
}
Write-Output ("Release binary: {0:N2} MiB" -f $binaryMiB)
New-Item -ItemType Directory -Force -Path $distRoot | Out-Null
if (Test-Path -LiteralPath $stage) {
    $resolvedStage = [System.IO.Path]::GetFullPath($stage)
    if (-not $resolvedStage.StartsWith($distRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to replace staging directory outside dist: $resolvedStage"
    }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage | Out-Null

Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination $stage
Copy-Item -LiteralPath (Join-Path $repoRoot "NOTICE") -Destination $stage
Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination $stage

switch ($Platform) {
    "windows" {
        Copy-Item -LiteralPath $binary -Destination (Join-Path $stage "termior.exe")
        Copy-Item -LiteralPath (Join-Path $repoRoot "assets/icons/termior.ico") -Destination $stage
        $archive = Join-Path $distRoot "$artifactName.zip"
        if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
        Compress-Archive -LiteralPath $stage -DestinationPath $archive -CompressionLevel Optimal
    }
    "macos" {
        $app = Join-Path $stage "Termior.app"
        $contents = Join-Path $app "Contents"
        $macos = Join-Path $contents "MacOS"
        $resources = Join-Path $contents "Resources"
        New-Item -ItemType Directory -Path $macos, $resources | Out-Null
        Copy-Item -LiteralPath $binary -Destination (Join-Path $macos "termior")
        & chmod +x (Join-Path $macos "termior")
        Copy-Item -LiteralPath (Join-Path $repoRoot "assets/icons/termior.icns") -Destination $resources
        $plist = @"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDisplayName</key><string>Termior</string>
<key>CFBundleExecutable</key><string>termior</string>
<key>CFBundleIdentifier</key><string>app.termior.Termior</string>
<key>CFBundleIconFile</key><string>termior.icns</string>
<key>CFBundleName</key><string>Termior</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$Version</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"@
        Set-Content -LiteralPath (Join-Path $contents "Info.plist") -Value $plist -Encoding utf8NoBOM
        $archive = Join-Path $distRoot "$artifactName.tar.gz"
        if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
        & tar -C $distRoot -czf $archive $artifactName
        if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
    }
    "linux" {
        $binDir = Join-Path $stage "bin"
        $applications = Join-Path $stage "share/applications"
        $icons = Join-Path $stage "share/icons/hicolor"
        New-Item -ItemType Directory -Path $binDir, $applications, $icons | Out-Null
        Copy-Item -LiteralPath $binary -Destination (Join-Path $binDir "termior")
        & chmod +x (Join-Path $binDir "termior")
        Copy-Item -LiteralPath (Join-Path $repoRoot "packaging/linux/app.termior.Termior.desktop") -Destination $applications
        Copy-Item -Path (Join-Path $repoRoot "assets/icons/hicolor/*") -Destination $icons -Recurse
        $archive = Join-Path $distRoot "$artifactName.tar.gz"
        if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
        & tar -C $distRoot -czf $archive $artifactName
        if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
    }
}

$archiveInfo = Get-Item -LiteralPath $archive
$archiveMiB = $archiveInfo.Length / 1MB
if ($archiveMiB -gt $MaxArchiveMiB) {
    throw ("Archive is {0:N2} MiB, above the {1} MiB release limit" -f $archiveMiB, $MaxArchiveMiB)
}
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
$checksum = "$hash  $($archiveInfo.Name)"
Set-Content -LiteralPath "$archive.sha256" -Value $checksum -Encoding ascii
Write-Output ("Packaged {0} ({1:N2} MiB)" -f $archiveInfo.FullName, $archiveMiB)
Write-Output $checksum
