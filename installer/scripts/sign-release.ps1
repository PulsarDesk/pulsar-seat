# SPDX-License-Identifier: GPL-3.0-only
#
# Signs a release's SHA256SUMS.txt with the offline release key (minisign format) and
# checks the result against installer/release-signers.txt, the list compiled into the
# service: SHA256SUMS.txt.minisig is written next to it. Run it after the MSIs are
# Authenticode-signed and write-sums.ps1 rewrote SHA256SUMS.txt (docs/release/checklist.md).
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\sign-release.ps1 -Dist C:\release\v0.2.0
#   ... -Key E:\offline\pulsar-seat-key\minisign.key
#
# Never prints the secret key. Publish SHA256SUMS.txt.minisig as a release asset beside
# SHA256SUMS.txt; without it no client offers the release.
param(
    [Parameter(Mandatory = $true)] [string] $Dist,
    [string] $Key = "",
    [string] $CargoTargetDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $Key) { $Key = Join-Path $env:USERPROFILE ".pulsar-seat\release-key\minisign.key" }
if (-not (Test-Path $Key)) { throw "release key $Key not found (installer/scripts/release-keygen.ps1 creates it)" }
$sums = Join-Path $Dist "SHA256SUMS.txt"
if (-not (Test-Path $sums)) { throw "$sums not found (installer/scripts/write-sums.ps1 writes it)" }
$signers = Join-Path $repo "installer\release-signers.txt"
if ($CargoTargetDir) { $env:CARGO_TARGET_DIR = $CargoTargetDir }

# Every MSI listed must be in the folder with that hash: the signature vouches for them.
# Every PulsarSeat_*.msi in the folder must be listed too (nothing unchecked is published).
$releaseCommit = $null
$listed = @(Get-Content $sums | Where-Object { $_.Trim() } | ForEach-Object { ($_ -split '\s+[ *]?', 2)[1].TrimStart('*') })
foreach ($m in Get-ChildItem $Dist -Filter "PulsarSeat_*.msi") {
    if ($listed -notcontains $m.Name) { throw "$($m.Name) is in $Dist but not in SHA256SUMS.txt (remove it or rerun write-sums.ps1)" }
}
foreach ($line in Get-Content $sums) {
    if (-not $line.Trim()) { continue }
    if ($line -notmatch '^([0-9a-f]{64}) [ *](.+)$') { throw "SHA256SUMS.txt: unexpected line '$line'" }
    $f = Join-Path $Dist $Matches[2]
    if (-not (Test-Path $f)) { throw "SHA256SUMS.txt lists $($Matches[2]), which is not in $Dist" }
    $h = (Get-FileHash $f -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($h -ne $Matches[1]) { throw "$($Matches[2]): SHA-256 $h, SHA256SUMS.txt says $($Matches[1])" }
    $name = $Matches[2]
    $sig = Get-AuthenticodeSignature $f
    if ($sig.Status -ne "Valid") { throw "${name}: Authenticode $($sig.Status); sign the MSI first (the update check refuses it)" }
    # Inside the MSI: the credential provider DLL that LogonUI loads as SYSTEM must be validly
    # signed too (administrative extraction only: nothing is installed or registered).
    $extract = Join-Path ([IO.Path]::GetTempPath()) ("pulsar-seat-sign-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force $extract | Out-Null
    try {
        $p = Start-Process msiexec.exe -Wait -PassThru -NoNewWindow -ArgumentList "/a", "`"$f`"", "/qn", "TARGETDIR=`"$extract`""
        if ($p.ExitCode -ne 0) { throw "${name}: administrative extraction failed (exit $($p.ExitCode))" }
        $dlls = @(Get-ChildItem $extract -Recurse -Filter "pulsar-seat-credprov.dll" -File)
        if ($dlls.Count -ne 1) { throw "${name}: expected one pulsar-seat-credprov.dll inside, found $($dlls.Count)" }
        $ds = Get-AuthenticodeSignature $dlls[0].FullName
        if ($ds.Status -ne "Valid") {
            throw "${name}: pulsar-seat-credprov.dll inside is Authenticode $($ds.Status); LogonUI loads it as SYSTEM - sign it and rebuild with build-installer.ps1 -SignedCredProv"
        }
        $arch = if ($name -match '_arm64\.msi$') { "arm64" } elseif ($name -match '_x64\.msi$') { "x64" } else { throw "${name}: no architecture in the name" }
        # Build record (build-installer.ps1): every MSI of one release comes from the same
        # committed source, and the service inside is the one that build produced (a stale
        # MSI from an older build left in dist/ is refused).
        $recFile = "$f.build.txt"
        if (-not (Test-Path $recFile)) { throw "${name}: no $name.build.txt next to it (build it with build-installer.ps1; copy the record with the MSI)" }
        $rec = @{}
        foreach ($l in Get-Content $recFile) { if ($l -match '^([a-z_0-9]+): (.*)$') { $rec[$Matches[1]] = $Matches[2].Trim() } }
        foreach ($k in "version", "arch", "commit", "dirty", "release", "service_sha256") {
            if (-not $rec.ContainsKey($k)) { throw "${name}: build record has no '$k'" }
        }
        if ($rec.arch -ne $arch -or $name -ne "PulsarSeat_$($rec.version)_$arch.msi") { throw "${name}: build record is for $($rec.version) $($rec.arch)" }
        if ($rec.commit -notmatch '^[0-9a-f]{40}$') { throw "${name}: built from no commit ($($rec.commit)); release builds come from the tagged commit" }
        if ($rec.dirty -ne "false") { throw "${name}: built from a working tree with uncommitted changes" }
        if ($rec.release -ne "true") { throw "${name}: not a -Release build (no release-key, source-offer and notices gates)" }
        if ($null -eq $releaseCommit) { $releaseCommit = $rec.commit }
        elseif ($rec.commit -ne $releaseCommit) { throw "${name}: built from $($rec.commit), another MSI from $releaseCommit; rebuild both from the release tag" }
        $tagCommit = (git -C $repo rev-parse --verify --quiet "v$($rec.version)^{commit}" 2>$null)
        if ($LASTEXITCODE -ne 0 -or $tagCommit -ne $rec.commit) {
            throw "${name}: built from $($rec.commit), but tag v$($rec.version) is '$tagCommit' here (fetch the tags; build from the tag)"
        }
        $svc = @(Get-ChildItem $extract -Recurse -Filter "pulsar-seat-service.exe" -File)
        if ($svc.Count -ne 1) { throw "${name}: expected one pulsar-seat-service.exe inside, found $($svc.Count)" }
        $svcHash = (Get-FileHash $svc[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($svcHash -ne $rec.service_sha256) { throw "${name}: the service inside ($svcHash) is not the one its build record names ($($rec.service_sha256)); stale MSI" }
        # Driver packages were checked when they were staged (build-installer.ps1 -SignedDrivers).
        $drv = @(); if (Get-ChildItem $extract -Recurse -Filter *.sys -File) { $drv = @("--allow-drivers") }
        # --notices: LICENSE / NOTICES / SOURCE-OFFER and the generated third-party notices
        # (MIT / Apache / BSD texts of the bundled crates and JS packages) must be inside.
        bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $arch --release --notices @drv $extract
        if ($LASTEXITCODE -ne 0) { throw "${name}: release payload check failed" }
    } finally {
        Remove-Item -Recurse -Force $extract -ErrorAction SilentlyContinue
    }
}

cargo build --locked --release -p seat-release --bin pulsar-seat-release --manifest-path (Join-Path $repo "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }
$tool = Join-Path $targetDir "release\pulsar-seat-release.exe"

& $tool sign --file $sums --key $Key --signers $signers
if ($LASTEXITCODE -ne 0) { throw "signing failed (exit $LASTEXITCODE)" }
& $tool verify --file $sums --signers $signers
if ($LASTEXITCODE -ne 0) { throw "verification failed (exit $LASTEXITCODE)" }
Write-Host "Upload $sums and $sums.minisig with the MSIs."
