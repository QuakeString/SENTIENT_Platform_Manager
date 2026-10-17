# Copyright (c) 2016-2026 The SENTIENT Authors
#
# Sign Windows executables with Authenticode (SHA-256, RFC 3161 timestamp)
# and verify the result. The certificate comes from, in this order:
#
#   SPM_SIGN_THUMBPRINT     a certificate already in the store (CurrentUser\My,
#                           else LocalMachine\My) -- EV certificates on a
#                           hardware token or in a cloud HSM are used this way
#   SPM_SIGN_PFX            a .pfx file, or SPM_SIGN_PFX_BASE64 its contents
#                           (CI secrets); SPM_SIGN_PFX_PASSWORD its password
#
#   SPM_SIGN_TIMESTAMP_URL  RFC 3161 server (default: DigiCert's)
#
# With no certificate configured the files are left unsigned and a warning is
# printed, unless -Require is given. Pure ASCII on purpose (see
# package-windows.ps1).
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\sign-windows.ps1 `
#          -Path dist\SENTIENT-Setup.exe [-Require]

param(
    [Parameter(Mandatory = $true)][string[]]$Path,
    [switch]$Require
)
$ErrorActionPreference = "Stop"

function Find-SignTool {
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "\\x64\\" } | Sort-Object FullName | Select-Object -Last 1
    if (-not $found) { throw "signtool.exe not found: install the Windows SDK (Signing Tools)." }
    return $found.FullName
}

$timestamp = if ($env:SPM_SIGN_TIMESTAMP_URL) { $env:SPM_SIGN_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
$certArgs = $null
$tempPfx = $null

if ($env:SPM_SIGN_THUMBPRINT) {
    $thumb = $env:SPM_SIGN_THUMBPRINT -replace "\s", ""
    $inUser = Test-Path "Cert:\CurrentUser\My\$thumb"
    $certArgs = @("/sha1", $thumb)
    if (-not $inUser) { $certArgs += "/sm" }
} elseif ($env:SPM_SIGN_PFX -or $env:SPM_SIGN_PFX_BASE64) {
    $pfx = $env:SPM_SIGN_PFX
    if (-not $pfx) {
        $tempPfx = Join-Path $env:TEMP ("spm-sign-" + [guid]::NewGuid() + ".pfx")
        [IO.File]::WriteAllBytes($tempPfx, [Convert]::FromBase64String($env:SPM_SIGN_PFX_BASE64))
        $pfx = $tempPfx
    }
    $certArgs = @("/f", $pfx)
    if ($env:SPM_SIGN_PFX_PASSWORD) { $certArgs += @("/p", $env:SPM_SIGN_PFX_PASSWORD) }
}

if (-not $certArgs) {
    if ($Require) { throw "No signing certificate configured (SPM_SIGN_THUMBPRINT, SPM_SIGN_PFX or SPM_SIGN_PFX_BASE64)." }
    Write-Warning "No signing certificate configured: leaving $($Path -join ', ') unsigned."
    exit 0
}

$signtool = Find-SignTool
try {
    foreach ($p in $Path) {
        # A timestamp server can be briefly unavailable: try three times.
        $signed = $false
        for ($i = 1; $i -le 3 -and -not $signed; $i++) {
            & $signtool sign /fd sha256 /td sha256 /tr $timestamp /d "SENTIENT Setup" @certArgs $p
            if ($LASTEXITCODE -eq 0) { $signed = $true } else { Start-Sleep -Seconds (5 * $i) }
        }
        if (-not $signed) { throw "Signing $p failed." }
        & $signtool verify /pa /tw $p
        if ($LASTEXITCODE -ne 0) { throw "$p is signed but its signature does not verify." }
        Write-Host "Signed $p"
    }
} finally {
    if ($tempPfx) { Remove-Item -Force $tempPfx -ErrorAction SilentlyContinue }
}
