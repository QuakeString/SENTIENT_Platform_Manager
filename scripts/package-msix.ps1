# Copyright (c) 2016-2026 The SENTIENT Authors
#
# The SENTIENT Platform Manager as a Microsoft Store package (MSIX), from a
# built program. Windows only: it needs makeappx.exe and makepri.exe from
# the Windows 10/11 SDK (preinstalled on GitHub's windows-latest runners).
# See docs/MICROSOFT_STORE.md.
#
#   1. Lays out the package: the program (as sentient-platform-manager.exe),
#      packaging\msix\Assets and AppxManifest.xml.
#   2. Fills the manifest with the program's own "msix-manifest" job
#      (app/src/store.rs): it refuses a pre-release version (the Store takes
#      final releases only; 0.9.4 becomes 0.9.4.0) and malformed identity
#      values.
#   3. makepri: resources.pri, so Windows picks the logo for each scale.
#   4. makeappx pack: SENTIENT-Platform-Manager-<version>.msix.
#   5. SENTIENT-Platform-Manager-<version>.msixupload for Partner Center: a
#      zip that holds the .msix (Partner Center also takes the .msix itself).
#   6. Only with -SignWith: signs the .msix for a sideload test. A package
#      for the Store is uploaded unsigned; Microsoft signs it.
#
# The package identity is the one Partner Center gave the app (Product
# management > Product identity; Store ID 9PP6JLZV7S2R, package family
# INVENIASYSTEMS.SENTIENTPlatformManager_sjyzajq1qbzj2):
#   Name                  INVENIASYSTEMS.SENTIENTPlatformManager
#   Publisher             CN=8CCEC686-FD1B-4801-ABEE-6B4CE6A2E92C
#   PublisherDisplayName  INVENIA SYSTEMS
# Parameters or the environment variables MSIX_IDENTITY_NAME, MSIX_PUBLISHER
# and MSIX_PUBLISHER_DISPLAY_NAME override them. -TestIdentity packages a
# test identity instead (INVENIASYSTEMS.SENTIENTPlatformManager.Test,
# CN=INVENIA SYSTEMS, O=INVENIA SYSTEMS, C=IN: the subject of INVENIA's own
# code-signing certificate, so a sideload test package signs with it); it is
# never submitted.
#
# Signing for a sideload test: -SignWith <file.pfx>; its password from the
# environment variable MSIX_SIGN_PFX_PASSWORD (never on the command line).
# The certificate's subject must equal the package's Publisher.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 `
#     -Exe app\target\release\sentient-platform-manager.exe [-Version 0.9.4] `
#     [-OutDir dist] [-TestIdentity] [-SignWith C:\keys\invenia.pfx]
#
# Pure ASCII on purpose (Windows PowerShell 5.1 reads scripts as ANSI).

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Version = "",
    [string]$IdentityName = $env:MSIX_IDENTITY_NAME,
    [string]$Publisher = $env:MSIX_PUBLISHER,
    [string]$PublisherDisplayName = $env:MSIX_PUBLISHER_DISPLAY_NAME,
    [string]$OutDir = "dist",
    [string]$SignWith = "",
    [switch]$TestIdentity
)
$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$StoreName = "INVENIASYSTEMS.SENTIENTPlatformManager"
$StorePublisher = "CN=8CCEC686-FD1B-4801-ABEE-6B4CE6A2E92C"
$TestName = "INVENIASYSTEMS.SENTIENTPlatformManager.Test"
$TestPublisher = "CN=INVENIA SYSTEMS, O=INVENIA SYSTEMS, C=IN"

if (-not (Test-Path $Exe)) { throw "The program $Exe is not there: build it first (cargo build --release in app)." }
$Exe = (Resolve-Path $Exe).Path
if (-not $Version) {
    $Version = (Select-String -Path (Join-Path $Root "app\Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
}
$test = [bool]$TestIdentity
if ($test) {
    $IdentityName = $TestName
    $Publisher = $TestPublisher
} else {
    if (-not $IdentityName) { $IdentityName = $StoreName }
    if (-not $Publisher) { $Publisher = $StorePublisher }
}
if (-not $PublisherDisplayName) { $PublisherDisplayName = "INVENIA SYSTEMS" }
if ($test) {
    Write-Host "TEST package ($IdentityName, $Publisher): for sideloading only, never submitted to the Store."
} else {
    Write-Host "Store package: $IdentityName, $Publisher"
}

function Find-SdkTool([string]$name) {
    $onPath = Get-Command $name -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Recurse -Filter $name -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "\\x64\\" } | Sort-Object FullName | Select-Object -Last 1
    if (-not $found) { throw "$name not found: install the Windows 10/11 SDK." }
    return $found.FullName
}

function Invoke-Tool([string]$tool, [string[]]$arguments) {
    & $tool @arguments
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $tool) failed (exit code $LASTEXITCODE)." }
}

$makeappx = Find-SdkTool "makeappx.exe"
$makepri = Find-SdkTool "makepri.exe"

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$work = Join-Path $OutDir "msix-work"
$layout = Join-Path $work "layout"
if (Test-Path $work) { Remove-Item -Recurse -Force $work }
New-Item -ItemType Directory -Force $layout | Out-Null

# 1. The program and the logos.
Copy-Item $Exe (Join-Path $layout "sentient-platform-manager.exe")
Copy-Item -Recurse (Join-Path $Root "packaging\msix\Assets") (Join-Path $layout "Assets")

# 2. The manifest, by the program itself (one rule for the version, tested
#    in app/src/store.rs). A windowed program: its result is its exit code
#    and the file it writes; its messages go to a file.
$manifest = Join-Path $layout "AppxManifest.xml"
$errFile = Join-Path $work "msix-manifest.err"
$outFile = Join-Path $work "msix-manifest.out"
$jobArgs = @(
    "--job", "msix-manifest",
    "--template", ('"' + (Join-Path $Root "packaging\msix\AppxManifest.xml") + '"'),
    "--out", ('"' + $manifest + '"'),
    "--version", $Version,
    "--identity-name", $IdentityName,
    "--publisher", ('"' + $Publisher + '"'),
    "--publisher-display-name", ('"' + $PublisherDisplayName + '"')
)
$p = Start-Process -FilePath $Exe -ArgumentList $jobArgs -Wait -PassThru -NoNewWindow `
    -RedirectStandardError $errFile -RedirectStandardOutput $outFile
if ($p.ExitCode -ne 0 -or -not (Test-Path $manifest)) {
    $why = if (Test-Path $errFile) { (Get-Content $errFile -Raw) } else { "" }
    throw "The manifest was not made (exit code $($p.ExitCode)): $why"
}
$packageVersion = ([xml](Get-Content $manifest -Raw)).Package.Identity.Version
Write-Host "Package $IdentityName $packageVersion ($Publisher)"

# 3. resources.pri: the logos by scale and size.
$priConfig = Join-Path $work "priconfig.xml"
Invoke-Tool $makepri @("createconfig", "/cf", $priConfig, "/dq", "en-US", "/pv", "10.0.0", "/o")
Push-Location $layout
try {
    Invoke-Tool $makepri @("new", "/pr", $layout, "/cf", $priConfig, "/mn", $manifest, "/of", (Join-Path $layout "resources.pri"), "/o")
} finally {
    Pop-Location
}

# 4. The package.
$base = "SENTIENT-Platform-Manager-$Version"
$msix = Join-Path $OutDir "$base.msix"
if (Test-Path $msix) { Remove-Item -Force $msix }
Invoke-Tool $makeappx @("pack", "/d", $layout, "/p", $msix, "/o")

# 5. The upload file for Partner Center: a zip holding the .msix.
$upload = Join-Path $OutDir "$base.msixupload"
if (Test-Path $upload) { Remove-Item -Force $upload }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($upload, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $msix, "$base.msix", [System.IO.Compression.CompressionLevel]::Optimal)
} finally {
    $zip.Dispose()
}

# 6. Signed for a sideload test only.
if ($SignWith) {
    if (-not (Test-Path $SignWith)) { throw "The certificate file $SignWith is not there." }
    $signtool = Find-SdkTool "signtool.exe"
    $secure = if ($env:MSIX_SIGN_PFX_PASSWORD) { ConvertTo-SecureString $env:MSIX_SIGN_PFX_PASSWORD -AsPlainText -Force } else { New-Object System.Security.SecureString }
    # Imported for the signature and removed after: signtool then needs no
    # password on its command line.
    $flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]"UserKeySet,PersistKeySet"
    $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2((Resolve-Path $SignWith).Path, $secure, $flags)
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store("My", "CurrentUser")
    $store.Open("ReadWrite")
    $store.Add($cert)
    $store.Close()
    try {
        if ($cert.Subject -ne $Publisher) {
            throw "The certificate's subject ($($cert.Subject)) is not the package's Publisher ($Publisher): Windows refuses such a package. Build with -Publisher `"$($cert.Subject)`"."
        }
        Invoke-Tool $signtool @("sign", "/fd", "SHA256", "/sha1", $cert.Thumbprint, "/s", "My", $msix)
        Write-Host "Signed for a sideload test with $($cert.Subject) ($($cert.Thumbprint))."
    } finally {
        Remove-Item -Force "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
    }
}

Remove-Item -Recurse -Force $work
Write-Host "Made $msix"
Write-Host "Made $upload"
if ($test) { Write-Host "TEST identity: for sideloading only." }
