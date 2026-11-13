# SPDX-License-Identifier: GPL-3.0-only
#
# Creates the Pulsar Seat release signing key (Ed25519, minisign format) ONCE, for the
# maintainer. The secret key goes OUTSIDE the repository (default
# %USERPROFILE%\.pulsar-seat\release-key\minisign.key), is never printed, and is
# restricted to the current user; the public key line is printed for
# installer/release-signers.txt. Refuses to overwrite an existing key and refuses a folder
# inside any source tree.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\release-keygen.ps1
#   ... -OutDir E:\offline\pulsar-seat-key      (e.g. an encrypted removable drive, NTFS;
#                                                 FAT / exFAT is refused: no ACLs)
#
# Afterwards (docs/release/checklist.md "Release signing key"):
#   1. paste the printed `RWQ...` line into installer/release-signers.txt and commit it;
#   2. back the key folder up offline (two copies); losing it means a key rotation release;
#   3. never copy minisign.key into the repository, CI secrets or a cloud folder.
#
# Alternative: the minisign tool itself (`minisign -G -p minisign.pub -s minisign.key`,
# password-protected). Its keys and signatures are accepted as well; sign with
# `minisign -S -s minisign.key -m SHA256SUMS.txt` in that case.
param(
    [string] $OutDir = "",
    [string] $CargoTargetDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $OutDir) { $OutDir = Join-Path $env:USERPROFILE ".pulsar-seat\release-key" }
$full = [System.IO.Path]::GetFullPath($OutDir)
if ($full.StartsWith($repo.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or $full -ieq $repo) {
    throw "refusing to put the release key inside the repository ($full)"
}
if ($CargoTargetDir) { $env:CARGO_TARGET_DIR = $CargoTargetDir }

Write-Host "== building pulsar-seat-release"
cargo build --locked --release -p seat-release --bin pulsar-seat-release --manifest-path (Join-Path $repo "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }
$tool = Join-Path $targetDir "release\pulsar-seat-release.exe"

& $tool keygen --out-dir $full
if ($LASTEXITCODE -ne 0) { throw "keygen failed (exit $LASTEXITCODE)" }

# The tool creates minisign.key with a protected DACL (current user + SYSTEM) and refuses a
# volume without ACLs. Re-apply and check it here as well: a failure is an error, not a
# warning (the key must never stay readable by other local users).
$key = Join-Path $full "minisign.key"
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& icacls.exe $key /inheritance:r /grant:r "*${me}:(F)" "*S-1-5-18:(F)" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "could not restrict the ACL of $key (icacls exit $LASTEXITCODE); delete the key and run again on an NTFS folder" }
$acl = Get-Acl $key
$rules = $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])
$others = @($rules | Where-Object { $_.IdentityReference.Value -notin @($me, "S-1-5-18") })
if (-not $acl.AreAccessRulesProtected -or $others.Count -gt 0) {
    throw "$key is readable by other accounts ($(($others | ForEach-Object { $_.IdentityReference }) -join ', ')); delete it and run again on an NTFS folder"
}
Write-Host ""
Write-Host "Next: add the RWQ... line above to installer/release-signers.txt, then back up $full offline."
