# Copyright (c) 2016-2026 The SENTIENT Authors
#
# Optional, for managed sites: trust INVENIA SYSTEMS' self-signed
# code-signing certificate on this computer, so Windows shows "INVENIA
# SYSTEMS" as the verified publisher of SENTIENT programs (UAC prompt, file
# Properties) instead of "Unknown publisher".
#
# It adds INVENIA-SYSTEMS-codesign.cer (next to this script) to the computer's
# Trusted Root Certification Authorities and Trusted Publishers. Only do this
# if you trust INVENIA SYSTEMS: a trusted root certificate is trusted for what
# its key signs. This certificate allows code signing only.
# Nothing else needs it: SENTIENT and the Platform Manager install and update
# without it, and each download is checked against its published signature
# either way.
#
# Run as administrator:
#   powershell -ExecutionPolicy Bypass -File Trust-INVENIA.ps1            add
#   powershell -ExecutionPolicy Bypass -File Trust-INVENIA.ps1 -Remove    remove again
#   powershell -ExecutionPolicy Bypass -File Trust-INVENIA.ps1 -Status    show only
# A site can also deploy the .cer with Group Policy (Computer Configuration >
# Windows Settings > Security Settings > Public Key Policies: Trusted Root
# Certification Authorities and Trusted Publishers).
# Pure ASCII on purpose (Windows PowerShell 5.1 reads scripts as ANSI).

param([switch]$Remove, [switch]$Status)
$ErrorActionPreference = "Stop"

# SHA-1 thumbprint of the certificate this script is for: a different .cer
# next to it is refused.
$Thumbprint = "C7DC8013F1ECD97ED20193F04E56359B17B24CBD"
$Stores = @("Root", "TrustedPublisher")

function Show-Status {
    foreach ($s in $Stores) {
        $here = Test-Path "Cert:\LocalMachine\$s\$Thumbprint"
        "{0,-17} {1}" -f $s, $(if ($here) { "trusted" } else { "not added" })
    }
}

if ($Status) { Show-Status; exit 0 }

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw "Run this as administrator (right-click PowerShell > Run as administrator)." }

if ($Remove) {
    foreach ($s in $Stores) {
        $p = "Cert:\LocalMachine\$s\$Thumbprint"
        if (Test-Path $p) { Remove-Item $p; Write-Host "Removed from $s." } else { Write-Host "Not in $s." }
    }
    Show-Status
    exit 0
}

$cer = Join-Path $PSScriptRoot "INVENIA-SYSTEMS-codesign.cer"
if (-not (Test-Path $cer)) { throw "INVENIA-SYSTEMS-codesign.cer must be next to this script." }
$cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 $cer
if ($cert.Thumbprint -ne $Thumbprint) {
    throw "INVENIA-SYSTEMS-codesign.cer is not the expected certificate (thumbprint $($cert.Thumbprint)); nothing was changed."
}
Write-Host "Certificate: $($cert.Subject)"
Write-Host "Valid:       $($cert.NotBefore.ToString('yyyy-MM-dd')) to $($cert.NotAfter.ToString('yyyy-MM-dd'))"
foreach ($s in $Stores) {
    Import-Certificate -FilePath $cer -CertStoreLocation "Cert:\LocalMachine\$s" | Out-Null
    Write-Host "Added to $s."
}
Show-Status
