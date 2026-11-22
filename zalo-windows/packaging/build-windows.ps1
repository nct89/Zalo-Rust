# Build & package Zalo-Rust (GPUI) for Windows (x86_64 / ARM64).
# Run on Windows with Visual Studio Build Tools (MSVC + Windows SDK), CMake, NASM, and Rust.
#
# Usage:
#   pwsh zalo-windows/packaging/build-windows.ps1
#   $env:VERSION = "0.2.2"; pwsh zalo-windows/packaging/build-windows.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Push-Location $Root
try {
    $CargoToml = Get-Content (Join-Path $Root "Cargo.toml") -Raw
    $WorkspaceVersion = "0.2.2"
    if ($CargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $WorkspaceVersion = $Matches[1]
    }
    $Version = if ($env:VERSION) { $env:VERSION.TrimStart('v', 'V') } else { $WorkspaceVersion }
    $env:VERSION = $Version
    if (-not $env:ZALO_BUILD_VERSION) {
        $env:ZALO_BUILD_VERSION = $Version
    }
    if (-not $env:CMAKE_POLICY_VERSION_MINIMUM) {
        # CMake 4+ rejects older cmake_minimum_required(VERSION 3.1) inside bundled audiopus_sys.
        $env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
    }

    $Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }

    Write-Host "Building zalo-gpui v$Version for Windows ($Arch)..." -ForegroundColor Cyan
    cargo build --release -p zalo-gpui --features tray-xembed
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }

    $ExeSrc = Join-Path $Root "target\release\zalo-gpui.exe"
    if (-not (Test-Path $ExeSrc)) {
        throw "Expected binary not found at $ExeSrc"
    }

    $DistDir = Join-Path $Root "dist"
    $StageName = "Zalo-$Version-windows-$Arch"
    $Stage = Join-Path $DistDir $StageName
    if (Test-Path $Stage) {
        Remove-Item -Recurse -Force $Stage
    }
    New-Item -ItemType Directory -Force -Path `
        $Stage, `
        (Join-Path $Stage "fonts"), `
        (Join-Path $Stage "zcall-sound"), `
        (Join-Path $Stage "wasm") | Out-Null

    Copy-Item $ExeSrc (Join-Path $Stage "zalo-gpui.exe")
    Copy-Item (Join-Path $Root "protocol.json") (Join-Path $Stage "protocol.json")

    $Fonts = @(
        "Zalo-Segoe-UI.dc1a7d868a0b3a0e2daa8b36567e9ea9.ttf",
        "Zalo-Segoe-UI-Semibold.8de26695631b05cf79e17378f6d1f134.ttf",
        "Zalo-Segoe-UI-Bold.18b859523696ff3330adac79b99ef554.ttf",
        "Zalo-Segoe-UI-Text.41364bc2c9bcbffced63c4bad472bfa9.ttf",
        "zalo-font.6835d695d84d61b03bec2efaf8b51c42.ttf"
    )
    foreach ($f in $Fonts) {
        $src = Join-Path $Root "assets\fonts\$f"
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $Stage "fonts\$f")
        }
    }

    $Sounds = @("zalo_ringtone.mp3", "zalo_ringback.mp3", "endcall.mp3")
    foreach ($s in $Sounds) {
        $src = Join-Path $Root "assets\sounds\$s"
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $Stage "zcall-sound\$s")
        }
    }

    $Wasms = @(
        "libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm",
        "libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm"
    )
    foreach ($w in $Wasms) {
        $src = Join-Path $Root "assets\wasm\$w"
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $Stage "wasm\$w")
        }
    }

    $ZipPath = Join-Path $DistDir "$StageName.zip"
    if (Test-Path $ZipPath) {
        Remove-Item -Force $ZipPath
    }
    Compress-Archive -Path $Stage -DestinationPath $ZipPath -CompressionLevel Optimal
    Write-Host "Wrote $ZipPath" -ForegroundColor Green

    # Optional Inno Setup installer when `iscc` is on PATH or in the standard install location.
    $IsccCmd = Get-Command iscc -ErrorAction SilentlyContinue
    $IsccExe = if ($IsccCmd) {
        $IsccCmd.Source
    } elseif (Test-Path "C:\Program Files (x86)\Inno Setup 6\ISCC.exe") {
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
    } else {
        $null
    }
    $IssFile = Join-Path $PSScriptRoot "installer.iss"
    if ($IsccExe -and (Test-Path $IssFile)) {
        Write-Host "Building Inno Setup installer..." -ForegroundColor Cyan
        & $IsccExe "/DMyAppVersion=$Version" "/DMyStageDir=$Stage" "/DMyOutputDir=$DistDir" "/DMyArch=$Arch" $IssFile
        if ($LASTEXITCODE -ne 0) {
            throw "Inno Setup failed with exit code $LASTEXITCODE"
        }
    }

    $ChecksumLines = @()
    foreach ($artifact in Get-ChildItem $DistDir -File | Where-Object { $_.Name -like "Zalo-$Version-windows-*.zip" -or $_.Name -like "Zalo-$Version-windows-*-setup.exe" }) {
        $Hash = (Get-FileHash -Algorithm SHA256 $artifact.FullName).Hash.ToLowerInvariant()
        $ChecksumLines += "$Hash  $($artifact.Name)"
    }
    $ChecksumLines | Out-File -Encoding ascii (Join-Path $DistDir "SHA256SUMS-windows.txt")
    Get-ChildItem $DistDir | Format-Table Name, Length
}
finally {
    Pop-Location
}
