# SPDX-License-Identifier: GPL-3.0-only
#
# VM-only test-signing of our driver packages (plan.md "Driver round in the test VM",
# 2026-11-15). TEST ONLY: the output is never shipped, never staged by the MSI and never
# used on the dev laptop, the test PC or a user's PC; check-payload refuses any file that
# carries the test certificate's subject marker ("Pulsar Seat TEST ONLY").
#
#   -Action Cert   creates the self-signed code-signing certificate once, in the current
#                  user's store, and exports it under %USERPROFILE%\.pulsar-seat\testsign\
#                  (testsign.cer = public part for the VM; testsign.pfx + testsign.pfx.dpapi =
#                  private key, password random, kept only DPAPI-protected). Never prints
#                  the password or the key. Nothing in the repo.
#   -Action Sign   copies the built packages (installer\scripts\build-drivers.ps1 output) to
#                  target\vm\testsigned\<Platform>\<project>\, writes the TEST ONLY minifilter
#                  altitude into the staged INF copy, signs every .sys / .dll, rebuilds the
#                  catalog (inf2cat) and signs it.
#
#   powershell -ExecutionPolicy Bypass -File tools\vm\testsign.ps1 -Action Cert
#   powershell -ExecutionPolicy Bypass -File tools\vm\testsign.ps1 -Action Sign [-Platform ARM64]
param(
    [ValidateSet("Cert", "Sign")] [string] $Action = "Sign",
    [ValidateSet("ARM64", "x64")] [string] $Platform = "ARM64",
    [string] $Configuration = "Release",
    [string[]] $Projects = @("seat-input-filter", "seat-idd", "seat-storage-filter", "split-pad"),
    [string] $Sdk = "10.0.28000.0"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$certDir = Join-Path $env:USERPROFILE ".pulsar-seat\testsign"
$subject = "CN=Pulsar Seat TEST ONLY driver signing"
# TEST ONLY minifilter altitude: inside the FSFilter Security Enhancer range the INF names
# (400000-409999), at its top end. Microsoft has not allocated one to us; never ship it.
$testAltitude = "409990"

function Get-TestCert {
    @(Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) }) | Select-Object -First 1
}

if ($Action -eq "Cert") {
    New-Item -ItemType Directory -Force $certDir | Out-Null
    $cert = Get-TestCert
    if (-not $cert) {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject -CertStoreLocation Cert:\CurrentUser\My `
            -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(1)
        Write-Host "created test certificate $($cert.Thumbprint)"
    } else {
        Write-Host "test certificate exists: $($cert.Thumbprint)"
    }
    Export-Certificate -Cert $cert -FilePath (Join-Path $certDir "testsign.cer") -Type CERT | Out-Null
    $pfx = Join-Path $certDir "testsign.pfx"
    if (-not (Test-Path $pfx)) {
        # Random password, only in memory and DPAPI-protected (current user) on disk.
        $bytes = New-Object byte[] 32
        [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
        $pw = ConvertTo-SecureString ([Convert]::ToBase64String($bytes)) -AsPlainText -Force
        Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $pw | Out-Null
        ConvertFrom-SecureString $pw | Set-Content -Encoding ascii (Join-Path $certDir "testsign.pfx.dpapi")
        Write-Host "private key exported to $pfx (password DPAPI-protected next to it)"
    }
    Write-Host "public certificate: $(Join-Path $certDir 'testsign.cer')"
    exit 0
}

$cert = Get-TestCert
if (-not $cert) { throw "no test certificate: run -Action Cert first" }
$kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
$hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -or $env:PROCESSOR_ARCHITEW6432 -eq "ARM64") { "arm64" } else { "x64" }
$signtool = @("bin\$Sdk\$hostArch\signtool.exe", "bin\$Sdk\x64\signtool.exe", "bin\$Sdk\x86\signtool.exe") | ForEach-Object { Join-Path $kits $_ } | Where-Object { Test-Path $_ } | Select-Object -First 1
$inf2cat = Join-Path $kits "bin\$Sdk\x86\inf2cat.exe"
if (-not $signtool) { throw "signtool not found under $kits" }
$os = if ($Platform -eq "ARM64") { "10_VB_ARM64" } else { "10_VB_X64" }
$src = Join-Path $repo "target\drivers\$Platform\$Configuration"
$out = Join-Path $repo "target\vm\testsigned\$Platform"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null
Set-Content -Encoding ascii (Join-Path $out "TEST-SIGNED-ONLY.txt") "Pulsar Seat TEST ONLY: test-signed for the Hyper-V test VM. Never ship, never install outside the VM."

function Invoke-Tool([string] $exe, [string[]] $toolArgs, [string] $what) {
    $o = & $exe @toolArgs 2>&1
    if ($LASTEXITCODE -ne 0) { $o | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }; throw "$what failed ($LASTEXITCODE)" }
}

foreach ($name in $Projects) {
    $pkg = Join-Path $src "$name\$name"
    if (-not (Test-Path $pkg)) { throw "package missing: $pkg (run build-drivers.ps1 -Platforms $Platform -Configurations $Configuration)" }
    $dst = Join-Path $out $name
    New-Item -ItemType Directory -Force $dst | Out-Null
    Get-ChildItem $pkg -File | Where-Object { $_.Extension -ne ".cat" } | Copy-Item -Destination $dst
    if ($name -eq "seat-storage-filter") {
        $inf = Join-Path $dst "seat-storage-filter.inf"
        $text = [System.IO.File]::ReadAllText($inf)
        $old = 'Instance1.Altitude = "UNALLOCATED"'
        if (-not $text.Contains($old)) { throw "altitude placeholder not found in $inf" }
        $text = $text.Replace($old, "Instance1.Altitude = `"$testAltitude`" ; Pulsar Seat TEST ONLY altitude (VM test, never ship)")
        [System.IO.File]::WriteAllText($inf, $text, [System.Text.Encoding]::Unicode)
        Write-Host "$name : TEST ONLY altitude $testAltitude in the staged INF"
    }
    foreach ($bin in Get-ChildItem $dst -File | Where-Object { $_.Extension -in ".sys", ".dll" }) {
        Invoke-Tool $signtool @("sign", "/fd", "SHA256", "/sha1", $cert.Thumbprint, "/s", "My", $bin.FullName) "signtool $($bin.Name)"
    }
    Invoke-Tool $inf2cat @("/os:$os", "/driver:$dst") "inf2cat $name"
    foreach ($cat in Get-ChildItem $dst -Filter *.cat) {
        Invoke-Tool $signtool @("sign", "/fd", "SHA256", "/sha1", $cert.Thumbprint, "/s", "My", $cat.FullName) "signtool $($cat.Name)"
    }
    Write-Host "$name : signed ($((Get-ChildItem $dst -File | ForEach-Object Name) -join ', '))"
}
Write-Host "test-signed packages: $out"
