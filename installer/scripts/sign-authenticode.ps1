# SPDX-License-Identifier: GPL-3.0-only
#
# Authenticode-signs Pulsar Seat files with signtool (SHA-256 file digest, RFC 3161
# timestamp): the signing-machine step of docs/release/checklist.md §3 / §4 and the
# EV-signing of driver submission CABs (docs/release/driver-submission.md).
#
# Certificate sources (exactly one):
#   -Thumbprint <sha1>          a certificate with private key in CurrentUser\My
#                               (-MachineStore: LocalMachine\My); hardware tokens and
#                               EV smart cards show up there through their CSP / KSP
#   -Dlib <dll> -DlibMetadata <json>
#                               a cloud signing service's signtool plug-in (for example
#                               Azure Artifact Signing: Azure.CodeSigning.Dlib.dll + its
#                               metadata.json); HYPOTHESIS until the maintainer runs it
#
# Files: -Path takes files and folders (folders: every .exe .dll .sys .msi .cat .cab under
# them, except below folders named in -ExcludeDir, default remote\ = the pinned third-party
# payload). A file that already carries any Authenticode signature is left alone (upstream
# signatures stay); -Force re-signs.
#
# TEST certificates (subject containing "TEST ONLY", tools/vm/testsign.ps1) sign only below
# target\sign-dryrun\ (signing dry run, docs/release/signing-dry-run.md); never into
# target\installer\dist or anywhere else. check-payload.ts refuses any file carrying one.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\sign-authenticode.ps1 `
#     -Path target\payload\arm64 -Thumbprint <sha1> -TimestampUrl http://timestamp.example
#   ... -Path C:\release\PulsarSeat_0.2.0_arm64.msi -Dlib C:\tools\Azure.CodeSigning.Dlib.dll -DlibMetadata C:\tools\metadata.json -TimestampUrl http://timestamp.acs.microsoft.com
#
# Never prints or exports a key. Exit 0 = every file signed (or kept) and checked.
param(
    [Parameter(Mandatory = $true)] [string[]] $Path,
    [string] $Thumbprint = "",
    [switch] $MachineStore,
    [string] $Dlib = "",
    [string] $DlibMetadata = "",
    # RFC 3161 time-stamping URL of the certificate's CA (required unless -NoTimestamp).
    [string] $TimestampUrl = "",
    # Dry runs only: no timestamp (offline). A release signature without a timestamp stops
    # validating when the certificate expires.
    [switch] $NoTimestamp,
    [string] $Description = "Pulsar Seat",
    [string] $DescriptionUrl = "https://github.com/PulsarDesk/pulsar-seat",
    [switch] $Force,
    # Folder names whose files are never signed when a folder is given: the pinned
    # third-party payload (remote\, SHA-256-pinned by fetch-deps.ts; re-signing would change
    # the pinned bytes). Pass @() to sign everything.
    [string[]] $ExcludeDir = @("remote"),
    [string] $Sdk = "10.0.28000.0"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$dryRoot = [IO.Path]::GetFullPath((Join-Path $repo "target\sign-dryrun")) + "\"
$exts = @(".exe", ".dll", ".sys", ".msi", ".cat", ".cab")

if ([bool]$Thumbprint -eq [bool]$Dlib) { throw "give exactly one of -Thumbprint or -Dlib" }
if ($Dlib -and -not $DlibMetadata) { throw "-Dlib needs -DlibMetadata" }
if (-not $NoTimestamp -and -not $TimestampUrl) { throw "-TimestampUrl is required (or -NoTimestamp for a dry run)" }
if ($TimestampUrl -and $TimestampUrl -notmatch '^https?://[^\s"]+$') { throw "-TimestampUrl must be an http(s) URL" }

$kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
$hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -or $env:PROCESSOR_ARCHITEW6432 -eq "ARM64") { "arm64" } else { "x64" }
# A -Dlib plug-in must match signtool's own architecture (cloud plug-ins ship x64 / x86).
$order = if ($Dlib) { @("x64", "x86", $hostArch) } else { @($hostArch, "x64", "x86") }
$signtool = $order | ForEach-Object { Join-Path $kits "bin\$Sdk\$_\signtool.exe" } | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe not found under $kits\bin\$Sdk (Windows SDK)" }

$isTest = $false
$cert = $null
if ($Thumbprint) {
    $Thumbprint = $Thumbprint.Replace(" ", "").ToUpperInvariant()
    if ($Thumbprint -notmatch '^[0-9A-F]{40}$') { throw "-Thumbprint must be 40 hex digits" }
    $storePath = if ($MachineStore) { "Cert:\LocalMachine\My" } else { "Cert:\CurrentUser\My" }
    $cert = Get-ChildItem $storePath | Where-Object { $_.Thumbprint -eq $Thumbprint } | Select-Object -First 1
    if (-not $cert) { throw "no certificate $Thumbprint in $storePath" }
    if (-not $cert.HasPrivateKey) { throw "certificate $Thumbprint has no private key here" }
    $isTest = $cert.Subject -match "TEST ONLY"
}

# Expand the file list.
$files = New-Object System.Collections.Generic.List[string]
foreach ($p in $Path) {
    if (-not (Test-Path $p)) { throw "not found: $p" }
    $item = Get-Item $p
    if ($item.PSIsContainer) {
        Get-ChildItem $item.FullName -Recurse -File | Where-Object { $exts -contains $_.Extension.ToLowerInvariant() } | Where-Object {
            $rel = $_.FullName.Substring($item.FullName.TrimEnd('\').Length + 1)
            -not @($rel.Split('\') | Select-Object -SkipLast 1 | Where-Object { $ExcludeDir -contains $_ })
        } | ForEach-Object { $files.Add($_.FullName) }
    } elseif ($exts -contains $item.Extension.ToLowerInvariant()) {
        $files.Add($item.FullName)
    } else {
        throw "$($item.FullName): not a signable type ($($exts -join ' '))"
    }
}
if ($files.Count -eq 0) { throw "no signable file under $($Path -join ', ')" }

if ($isTest) {
    foreach ($f in $files) {
        if (-not ([IO.Path]::GetFullPath($f)).StartsWith($dryRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "TEST certificate: $f is outside $dryRoot; test signatures never leave the dry-run folder"
        }
    }
}

$signed = 0; $kept = 0
foreach ($f in $files) {
    $before = Get-AuthenticodeSignature $f
    if ($before.SignerCertificate -and -not $Force) {
        Write-Host "keep  $f (signed by $($before.SignerCertificate.Subject))"
        $kept++
        continue
    }
    $a = @("sign", "/fd", "SHA256", "/d", $Description, "/du", $DescriptionUrl)
    if ($Thumbprint) {
        $a += @("/sha1", $Thumbprint, "/s", "My")
        if ($MachineStore) { $a += "/sm" }
    } else {
        $a += @("/dlib", $Dlib, "/dmdf", $DlibMetadata)
    }
    if (-not $NoTimestamp) { $a += @("/tr", $TimestampUrl, "/td", "SHA256") }
    $a += $f
    $o = & $signtool @a 2>&1
    if ($LASTEXITCODE -ne 0) { $o | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }; throw "signtool failed on $f (exit $LASTEXITCODE)" }
    $after = Get-AuthenticodeSignature $f
    if (-not $after.SignerCertificate) { throw "${f}: no signature after signtool" }
    if ($Thumbprint -and $after.SignerCertificate.Thumbprint -ne $Thumbprint) { throw "${f}: signed by $($after.SignerCertificate.Thumbprint), expected $Thumbprint" }
    # A real certificate must chain to a trusted root; a TEST one never does (by design).
    if (-not $isTest -and $after.Status -ne "Valid") { throw "${f}: Authenticode $($after.Status) after signing" }
    Write-Host "sign  $f ($($after.Status))"
    $signed++
}
Write-Host "signed $signed, kept $kept (already signed)$(if ($isTest) { ' - TEST certificate: not trusted, never shippable' })"
