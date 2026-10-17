# Copyright (c) 2016-2026 The SENTIENT Authors
#
# Build the offline SENTIENT Setup package for Windows:
#
#   SENTIENT-Setup-<version>\
#     SENTIENT-Setup.exe           the GPUI installer + Platform Manager (one exe),
#                                  signed when a certificate is configured
#                                  (scripts\sign-windows.ps1)
#     EULA.txt                     the licence agreement setup shows
#     payload\
#       postgresql-18.6-1-windows-x64-binaries.zip
#       timescaledb-postgresql-18-windows-amd64.zip
#       sentient-payload.tar         the SENTIENT server, UI and data
#       vc_redist.x64.exe            the Visual C++ runtime PostgreSQL and the
#                                    SENTIENT server need (a fresh Windows
#                                    has none); downloaded here if missing
#       VERSION                      the SENTIENT version, shown by setup
#
# and zip it. Setup finds payload\ next to itself and installs without
# internet. Pure ASCII on purpose: Windows PowerShell reads a BOM-less script
# as ANSI, and one curly quote breaks parsing.
#
# Usage (from the repository root, on Windows with Rust/MSVC):
#   powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1 `
#     -PayloadDir C:\payload -SentientVersion 4.3.0.24

param(
    [Parameter(Mandatory = $true)] [string] $PayloadDir,
    [Parameter(Mandatory = $true)] [string] $SentientVersion,
    [string] $Out = "dist",
    [switch] $NoBuild
)
$ErrorActionPreference = "Stop"

$required = @(
    "postgresql-18.6-1-windows-x64-binaries.zip",
    "timescaledb-postgresql-18-windows-amd64.zip",
    "sentient-payload.tar"
)
foreach ($f in $required) {
    if (-not (Test-Path (Join-Path $PayloadDir $f))) {
        throw "Missing $f in $PayloadDir"
    }
}

# The Visual C++ runtime goes into every package, so an offline install on a
# fresh Windows has it.
$vcRedist = Join-Path $PayloadDir "vc_redist.x64.exe"
if (-not (Test-Path $vcRedist)) {
    Write-Host "Downloading the Visual C++ runtime installer"
    Invoke-WebRequest -UseBasicParsing -Uri "https://aka.ms/vs/17/release/vc_redist.x64.exe" -OutFile $vcRedist
}
$required += "vc_redist.x64.exe"

if (-not $NoBuild) {
    Push-Location app
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    Pop-Location
}
$exe = "app\target\release\sentient-platform-manager.exe"

# Never ship a stale binary: the embedded FileVersion must match Cargo.toml.
$cargoVersion = (Select-String -Path app\Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$fileVersion = (Get-Item $exe).VersionInfo.FileVersion
if ($fileVersion -ne $cargoVersion) {
    throw "Stale build: $exe is $fileVersion, Cargo.toml says $cargoVersion"
}

$name = "SENTIENT-Setup-$SentientVersion"
$dir = Join-Path $Out $name
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
New-Item -ItemType Directory -Force (Join-Path $dir "payload") | Out-Null

Copy-Item $exe (Join-Path $dir "SENTIENT-Setup.exe")
# Signed in the package, not in target\: the installed Platform Manager is a
# copy of this file and keeps the signature.
& (Join-Path $PSScriptRoot "sign-windows.ps1") -Path (Join-Path $dir "SENTIENT-Setup.exe")
if (-not $?) { throw "Signing failed." }
Copy-Item "app\assets\licence.txt" (Join-Path $dir "EULA.txt")
foreach ($f in $required) {
    Copy-Item (Join-Path $PayloadDir $f) (Join-Path $dir "payload")
}
Set-Content -Path (Join-Path $dir "payload\VERSION") -Value $SentientVersion -Encoding ASCII

$zip = Join-Path $Out "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $dir -DestinationPath $zip
Write-Host "Built $zip (Platform Manager $cargoVersion, SENTIENT $SentientVersion)"
