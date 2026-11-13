# SPDX-License-Identifier: GPL-3.0-only
#
# Writes SHA256SUMS.txt for every PulsarSeat_*.msi in a folder, after signing (signing
# changes the file, so checksums from the unsigned build are wrong). The update check
# (crates/seat-service/src/update.rs) reads this file from the GitHub release only with a
# valid SHA256SUMS.txt.minisig from a release key (sign-release.ps1), and also refuses any
# installer without a valid Authenticode signature.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\write-sums.ps1 target\installer\dist
param([Parameter(Mandatory = $true)] [string] $Dir)

$ErrorActionPreference = "Stop"
$msis = Get-ChildItem $Dir -Filter "PulsarSeat_*_*.msi" | Sort-Object Name
if (-not $msis) { throw "no PulsarSeat_<version>_<arch>.msi in $Dir" }
$lines = foreach ($m in $msis) {
    $sig = Get-AuthenticodeSignature $m.FullName
    if ($sig.Status -ne "Valid") { Write-Warning "$($m.Name) is not validly signed ($($sig.Status)): the update check will refuse it" }
    if (-not (Test-Path "$($m.FullName).build.txt")) { Write-Warning "$($m.Name) has no build record ($($m.Name).build.txt): sign-release.ps1 will refuse it" }
    "$((Get-FileHash $m.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $($m.Name)"
}
# LF, no BOM (`sha256sum -c` format).
$sumsPath = Join-Path (Resolve-Path $Dir) "SHA256SUMS.txt"
[System.IO.File]::WriteAllText($sumsPath, (($lines -join "`n") + "`n"))
# A signature of the previous checksum file no longer matches: remove it, sign again.
if (Test-Path "$sumsPath.minisig") { Remove-Item "$sumsPath.minisig"; Write-Host "removed the old SHA256SUMS.txt.minisig" }
$lines | ForEach-Object { Write-Host $_ }
Write-Host "Next: installer\scripts\sign-release.ps1 -Dist $Dir (release key signature; the update check needs it)"
