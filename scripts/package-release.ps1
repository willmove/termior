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

function Find-Iscc {
    $cmd = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($candidate in @(
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe",
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
    )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    return $null
}

function Assert-SizeAndChecksum {
    param([string]$Path, [int]$MaxMiB)
    $info = Get-Item -LiteralPath $Path
    $mib = $info.Length / 1MB
    if ($mib -gt $MaxMiB) {
        throw ("{0} is {1:N2} MiB, above the {2} MiB release limit" -f $info.Name, $mib, $MaxMiB)
    }
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$Path.sha256" -Value "$hash  $($info.Name)" -Encoding ascii
    Write-Output ("Packaged {0} ({1:N2} MiB)" -f $info.FullName, $mib)
    Write-Output $hash
}

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
        Assert-SizeAndChecksum -Path $archive -MaxMiB $MaxArchiveMiB
        $iscc = Find-Iscc
        if (-not $iscc) {
            throw "Inno Setup 6 (ISCC.exe) not found; install it or add it to PATH (winget install JRSoftware.InnoSetup)"
        }
        & $iscc "/DAppVersion=$Version" (Join-Path $repoRoot "packaging/windows/termior.iss")
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }
        Assert-SizeAndChecksum -Path (Join-Path $distRoot "$artifactName-setup.exe") -MaxMiB $MaxArchiveMiB
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
        Assert-SizeAndChecksum -Path $archive -MaxMiB $MaxArchiveMiB
        $dmgStage = Join-Path $distRoot "$artifactName-dmg"
        if (Test-Path -LiteralPath $dmgStage) { Remove-Item -LiteralPath $dmgStage -Recurse -Force }
        New-Item -ItemType Directory -Path $dmgStage | Out-Null
        Copy-Item -Recurse -LiteralPath $app -Destination (Join-Path $dmgStage "Termior.app")
        & ln -s /Applications (Join-Path $dmgStage "Applications")
        $dmg = Join-Path $distRoot "$artifactName.dmg"
        if (Test-Path -LiteralPath $dmg) { Remove-Item -LiteralPath $dmg -Force }
        & hdiutil create -volname "Termior $Version" -srcfolder $dmgStage -ov -format UDZO $dmg
        if ($LASTEXITCODE -ne 0) { throw "hdiutil failed with exit code $LASTEXITCODE" }
        Remove-Item -LiteralPath $dmgStage -Recurse -Force
        Assert-SizeAndChecksum -Path $dmg -MaxMiB $MaxArchiveMiB
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
        Assert-SizeAndChecksum -Path $archive -MaxMiB $MaxArchiveMiB
    }
}

