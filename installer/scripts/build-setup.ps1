# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the native setup program PulsarSeat_<version>_<arch>_setup.exe (crates/seat-bootstrap)
# with the MSI of the same architecture embedded: an ARM64 build for the ARM64 MSI, an x64
# build for the x64 MSI (no x86 stub; docs/agents/rules.md). Both cross-compile on either
# host. Writes <exe>.build.txt (msi name + SHA-256, setup SHA-256, commit, release) next to
# it; sign-release.ps1 refuses a setup whose embedded MSI is not the listed one.
#
# Order for a release: Authenticode-sign the MSI first, then build the setup from the
# signed MSI with -Release (the release setup verifies the MSI's signature before it runs
# msiexec, so this script refuses an MSI that is not validly signed), then sign the setup,
# then write-sums.ps1 + sign-release.ps1 (docs/release/checklist.md §4).
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\build-setup.ps1 -Arch arm64
#   ... -Arch x64 -Msi C:\release\PulsarSeat_0.2.0_x64.msi -OutDir C:\release -Release
#
# Never signs anything and never runs the MSI (only `--payload-info` of the setup, when the
# host can run its architecture).
param(
    [Parameter(Mandatory = $true)] [ValidateSet("arm64", "x64")] [string] $Arch,
    # Default: target\installer\dist\PulsarSeat_<version>_<arch>.msi
    [string] $Msi = "",
    # Default: the MSI's folder.
    [string] $OutDir = "",
    [switch] $Release,
    [string] $CargoTargetDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$toml = Get-Content (Join-Path $repo "Cargo.toml") -Raw
if ($toml -notmatch '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') { throw "no [workspace.package] version in Cargo.toml" }
$version = $Matches[1]
$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$wantMachine = if ($Arch -eq "arm64") { 0xAA64 } else { 0x8664 }

if (-not $Msi) { $Msi = Join-Path $repo "target\installer\dist\PulsarSeat_${version}_$Arch.msi" }
if (-not (Test-Path $Msi)) { throw "MSI not found: $Msi (build-installer.ps1 -Arch $Arch builds it)" }
$Msi = (Resolve-Path $Msi).Path
$msiName = Split-Path $Msi -Leaf
if ($msiName -notmatch "^PulsarSeat_$([regex]::Escape($version))_$Arch(_[A-Za-z]{2}-[A-Za-z]{2})?\.msi$") {
    throw "$msiName is not PulsarSeat_${version}_$Arch[_<culture>].msi (version / arch of this build)"
}
if (-not $OutDir) { $OutDir = Split-Path $Msi -Parent }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$culture = if ($msiName -match '_([A-Za-z]{2}-[A-Za-z]{2})\.msi$') { "_" + $Matches[1] } else { "" }
$exeName = "PulsarSeat_${version}_$Arch${culture}_setup.exe"

if ($Release) {
    $sig = Get-AuthenticodeSignature $Msi
    if ($sig.Status -ne "Valid") { throw "${msiName}: Authenticode $($sig.Status); a -Release setup refuses an unsigned MSI at run time, so sign the MSI first" }
}

if (-not $CargoTargetDir) { $CargoTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" } }
# Own sub-folder: the release exe embeds a whole MSI and must never sit where the Tauri
# bundler collects files (<target>\<triple>\release).
$setupTarget = Join-Path $CargoTargetDir "setup"
$msiHash = (Get-FileHash $Msi -Algorithm SHA256).Hash.ToLowerInvariant()

$saved = @{ RUSTFLAGS = $env:RUSTFLAGS; PULSAR_SEAT_SETUP_MSI = $env:PULSAR_SEAT_SETUP_MSI; PULSAR_SEAT_RELEASE = $env:PULSAR_SEAT_RELEASE }
try {
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    $env:PULSAR_SEAT_SETUP_MSI = $Msi
    if ($Release) { $env:PULSAR_SEAT_RELEASE = "1" } else { Remove-Item Env:PULSAR_SEAT_RELEASE -ErrorAction SilentlyContinue }
    & cargo build --locked --release --manifest-path (Join-Path $repo "Cargo.toml") -p seat-bootstrap --target $triple --target-dir $setupTarget
    if ($LASTEXITCODE -ne 0) { throw "cargo build of seat-bootstrap ($triple) failed (exit $LASTEXITCODE)" }
} finally {
    foreach ($k in $saved.Keys) {
        if ($null -eq $saved[$k]) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue } else { Set-Item "Env:$k" $saved[$k] }
    }
}

$built = Join-Path $setupTarget "$triple\release\pulsar-seat-setup.exe"
$out = Join-Path $OutDir $exeName
Copy-Item $built $out -Force

# PE machine (the same reader as check-payload.ts, inline: one file).
$fs = [System.IO.File]::OpenRead($out)
try {
    $br = New-Object System.IO.BinaryReader($fs)
    $fs.Position = 0x3C; $pe = $br.ReadInt32(); $fs.Position = $pe
    if ($br.ReadUInt32() -ne 0x00004550) { throw "$out is not a PE file" }
    $machine = [int]$br.ReadUInt16()
} finally { $fs.Dispose() }
if ($machine -ne $wantMachine) { throw ("{0}: machine 0x{1:X4}, want 0x{2:X4}" -f $exeName, $machine, $wantMachine) }

# Embedded manifest (asInvoker): without it UAC installer detection elevates *setup*.exe.
$bytes = [System.IO.File]::ReadAllBytes($out)
$ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
if ($ascii.IndexOf("requestedExecutionLevel level=""asInvoker""", [StringComparison]::Ordinal) -lt 0) { throw "${exeName}: no asInvoker manifest embedded" }

# --payload-info when this host runs the architecture (ARM64 runs both; x64 only x64).
$hostArm = (Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture -eq 12
if ($Arch -eq "x64" -or $hostArm) {
    $info = Join-Path ([IO.Path]::GetTempPath()) ("pulsar-seat-setup-info-" + [Guid]::NewGuid().ToString("N") + ".txt")
    try {
        $p = Start-Process $out -ArgumentList "/payload-info" -Wait -PassThru -NoNewWindow -RedirectStandardOutput $info
        if ($p.ExitCode -ne 0) { throw "${exeName} /payload-info: exit $($p.ExitCode)" }
        $text = Get-Content $info -Raw
        if ($text -notmatch "(?m)^sha256: $msiHash\s*$") { throw "${exeName} /payload-info does not report the MSI's SHA-256:`n$text" }
        if ($text -notmatch "(?m)^name: $([regex]::Escape($msiName))\s*$") { throw "${exeName} /payload-info names another MSI:`n$text" }
    } finally { Remove-Item $info -ErrorAction SilentlyContinue }
} else {
    Write-Host "($exeName not run: this host cannot run $Arch code)"
}

$commit = (git -C $repo rev-parse --verify --quiet "HEAD^{commit}" 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $commit) { $commit = "none" }
$dirty = @(git -C $repo status --porcelain --untracked-files=no 2>$null).Count -gt 0
$setupHash = (Get-FileHash $out -Algorithm SHA256).Hash.ToLowerInvariant()
$record = @(
    "version: $version",
    "arch: $Arch",
    "commit: $commit",
    "dirty: $($dirty.ToString().ToLowerInvariant())",
    "release: $($Release.IsPresent.ToString().ToLowerInvariant())",
    "built_utc: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "msi: $msiName",
    "msi_sha256: $msiHash",
    "setup_sha256: $setupHash"
)
[System.IO.File]::WriteAllText("$out.build.txt", (($record -join "`n") + "`n"))
Write-Host "setup:   $out ($([int]((Get-Item $out).Length / 1MB)) MiB, machine 0x$("{0:X4}" -f $machine))"
Write-Host "embeds:  $msiName ($msiHash)"
Write-Host "SHA-256: $setupHash"
if (-not $Release) { Write-Host "(test build: not a release setup; it does not require a signed MSI)" }
