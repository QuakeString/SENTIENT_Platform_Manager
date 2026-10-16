# Copyright (c) 2016-2026 The SENTIENT Authors
#
# Build the offline SENTIENT Setup package for Windows:
#
#   SENTIENT-Setup-<version>\
#     SENTIENT-Setup.exe           the GPUI installer + Platform Manager (one exe)
#     payload\
#       postgresql-18.6-1-windows-x64-binaries.zip
#       timescaledb-postgresql-18-windows-amd64.zip
#       sentient-payload.tar         the SENTIENT server, UI and data
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
foreach ($f in $required) {
    Copy-Item (Join-Path $PayloadDir $f) (Join-Path $dir "payload")
}
Set-Content -Path (Join-Path $dir "payload\VERSION") -Value $SentientVersion -Encoding ASCII

$zip = Join-Path $Out "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $dir -DestinationPath $zip
Write-Host "Built $zip (Platform Manager $cargoVersion, SENTIENT $SentientVersion)"
