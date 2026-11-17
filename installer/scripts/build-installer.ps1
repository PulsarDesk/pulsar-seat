# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the Pulsar Seat MSI for one architecture (ARM64 or x64) the way CI does:
#   1. release binaries (static CRT) of the service, seat host and agent
#   2. the app API helper DLL for every app architecture the PC can run
#   3. optional: signed driver packages (-SignedDrivers <dir>), staged only when every
#      catalog verifies and is signed by the Microsoft hardware publisher
#   3b. remote seat kind (merge step 10): the shim (both packages; arm64 adds shim\arm64) and the pinned
#      third-party payload (fetch-deps.ts: drivers + Sunshine per arch) under remote\
#   4. PE-machine / payload gate (check-payload.ts)
#   5. bundle config + generated driver and remote-payload fragments (bundle-config.ts)
#   6. `tauri build --bundles msi` (Tauri downloads WiX 3.14 into %LOCALAPPDATA%\tauri
#      itself; nothing is installed system-wide)
#   7. administrative extraction of the MSI (msiexec /a: plain copy, nothing registered)
#      and the payload gate again on every file inside it
#   8. dist\PulsarSeat_<version>_<arch>.msi + dist\SHA256SUMS.txt (the names the update
#      check expects, crates/seat-service/src/update.rs)
#
# Never signs anything, never installs the MSI. Sign the MSI and every exe / dll with the
# maintainer's certificate outside this script (installer/README.md "Signing").
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\build-installer.ps1 -Arch arm64
#   ... -Arch x64 -SignedDrivers target\drivers-signed\x64
#   ... -Arch arm64 -NoArm64ec      (toolchain without the arm64ec target: test builds only)
param(
    [ValidateSet("arm64", "x64", "")] [string] $Arch = "",
    # Folder with one sub-folder per signed driver package (inf + cat + sys/dll).
    [string] $SignedDrivers = "",
    # Use binaries already in the payload folder (CI: the rust job's artifact).
    [switch] $SkipCargo,
    # Leave out the ARM64EC helper (rustup target missing). Not for releases.
    [switch] $NoArm64ec,
    # Expected product version (x.y.z); must equal the Cargo workspace, tauri.conf.json and
    # package.json versions (the build refuses a mismatch). Default: those.
    [string] $Version = "",
    # Release build: sets PULSAR_SEAT_RELEASE for cargo, so the service refuses to compile
    # while installer/release-signers.txt lists no release key
    # (crates/seat-service/src/update.rs); also checks the source-offer tag.
    [switch] $Release,
    # Authenticode-signed pulsar-seat-credprov.dll (signing machine, docs/release/checklist.md
    # §3) to bundle instead of the freshly built, unsigned one. Its signature must be Valid.
    [string] $SignedCredProv = "",
    # Leave out the remote kind's pinned third-party payload (fetch-deps.ts needs network).
    # Test builds only; refused with -Release.
    [switch] $NoRemoteDeps,
    [string] $CargoTargetDir = "",
    # MSI culture (installer/wix/locales/<culture>.wxl). en-US is the release default; tr-TR
    # builds the Turkish MSI (dist name gets a _tr-TR suffix).
    [ValidateSet("en-US", "tr-TR")] [string] $Language = "en-US"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $Arch) {
    # Native machine, not the WOW64 variable of this (maybe emulated) PowerShell.
    $Arch = if ((Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture -eq 12) { "arm64" } else { "x64" }
}
$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
if (-not $CargoTargetDir) { $CargoTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" } }
# Absolute, so tauri (run from apps\manager) builds into the same folder as cargo here; a
# relative dir made it write its own MSI under apps\manager\src-tauri and this script
# picked up a stale one (VM test 2026-11-15).
$CargoTargetDir = [IO.Path]::GetFullPath($(if ([IO.Path]::IsPathRooted($CargoTargetDir)) { $CargoTargetDir } else { Join-Path (Get-Location).Path $CargoTargetDir }))
$env:CARGO_TARGET_DIR = $CargoTargetDir
$buildStarted = Get-Date
$work = Join-Path $repo "target\installer\$Arch"
$payload = Join-Path $repo "target\payload\$Arch"
$dist = Join-Path $repo "target\installer\dist"
New-Item -ItemType Directory -Force $work, $payload, $dist | Out-Null

# One version everywhere: the service reports the Cargo workspace version
# (SERVICE_VERSION) to the update check, the MSI name / ProductVersion come from
# tauri.conf.json. A mismatch builds an MSI the update check cannot find, or one that is
# offered again after it was installed (update loop).
$cargoToml = Get-Content (Join-Path $repo "Cargo.toml") -Raw
if ($cargoToml -notmatch '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') { throw "Cargo.toml: no [workspace.package] version" }
$versions = [ordered]@{
    "Cargo.toml [workspace.package]" = $Matches[1]
    "apps/manager/src-tauri/tauri.conf.json" = (Get-Content (Join-Path $repo "apps\manager\src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json).version
    "apps/manager/package.json" = (Get-Content (Join-Path $repo "apps\manager\package.json") -Raw | ConvertFrom-Json).version
}
if ($Version) { $versions["-Version"] = $Version }
$distinct = @($versions.Values | Sort-Object -Unique)
if ($distinct.Count -ne 1) {
    throw "version mismatch: " + (($versions.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ", ")
}
$Version = $distinct[0]
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version must be x.y.z, got $Version" }
Write-Host "Pulsar Seat $Version $Arch ($triple), cargo target dir $CargoTargetDir"

function Invoke-Checked([string] $What, [scriptblock] $Block) {
    Write-Host "== $What"
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit $LASTEXITCODE)" }
}

# ---------------------------------------------------------------- 1 + 2: binaries
if ($Release) {
    $env:PULSAR_SEAT_RELEASE = "1"
    # GPL-3 section 6: installer/licenses/SOURCE-OFFER.txt points at the public repository
    # and the tag of this version. A release whose tag is not published there would ship a
    # source offer that points nowhere.
    $offerRepo = "https://github.com/PulsarDesk/pulsar-seat"
    $tagRef = git ls-remote --tags $offerRepo "refs/tags/v$Version" 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $tagRef) {
        throw "release gate: tag v$Version is not reachable at $offerRepo (SOURCE-OFFER.txt would point nowhere); publish the repository and the tag first"
    }
    # The update check of this build accepts only releases signed by a key in
    # installer/release-signers.txt (compiled into the service). The service build also
    # refuses an empty list under PULSAR_SEAT_RELEASE; this says why, before cargo runs.
    Invoke-Checked "release key list" {
        cargo run --locked --quiet -p seat-release --bin pulsar-seat-release --manifest-path (Join-Path $repo "Cargo.toml") -- check-signers (Join-Path $repo "installer\release-signers.txt") --require
    }
}
if ($Arch -eq "arm64" -and -not $NoArm64ec -and -not $SkipCargo) {
    $installed = @(rustup target list --installed)
    if ($installed -notcontains "arm64ec-pc-windows-msvc") {
        throw "rustup target arm64ec-pc-windows-msvc is missing (rustup target add arm64ec-pc-windows-msvc); -NoArm64ec builds a test-only package"
    }
}
if ($Release -and $NoArm64ec) { throw "-Release needs the ARM64EC helper; drop -NoArm64ec" }
# The bundle copies installer/licenses/ into the MSI: a release needs the generated
# third-party notices there before tauri runs (generated as in ci.yml "Third-party notices":
# cargo about generate ... -o installer/licenses/THIRD-PARTY-NOTICES-RUST.md and
# bun installer/scripts/js-notices.ts apps/manager installer/licenses/THIRD-PARTY-NOTICES-JS.md).
if ($Release) {
    foreach ($f in "THIRD-PARTY-NOTICES-RUST.md", "THIRD-PARTY-NOTICES-JS.md") {
        $np = Join-Path $repo "installer\licenses\$f"
        if (-not (Test-Path $np) -or (Get-Item $np).Length -lt 1024) {
            throw "release gate: $np missing or empty; generate the third-party notices first (installer/README.md, ci.yml 'Third-party notices')"
        }
    }
}
if (-not $SkipCargo) {
    $oldFlags = $env:RUSTFLAGS
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    try {
        # Two invocations: cargo unifies features across the packages of one build, so a
        # joint build would compile the service against seat-rdp/client (IronRDP, sspi,
        # native-tls) that only the seat host enables.
        Invoke-Checked "cargo build (service)" {
            cargo build --locked --release --target $triple -p seat-service -p seat-wrds
        }
        Invoke-Checked "cargo build (host, agent)" {
            cargo build --locked --release --target $triple -p seat-host
        }
        foreach ($n in "pulsar-seat-service.exe", "pulsar-seat-host.exe", "pulsar-seat-agent.exe") {
            Copy-Item (Join-Path $CargoTargetDir "$triple\release\$n") $payload -Force
        }
        # Remote Desktop protocol provider (crates/seat-wrds): loaded by TermService.
        Copy-Item (Join-Path $CargoTargetDir "$triple\release\pulsar_seat_wrds.dll") (Join-Path $payload "pulsar-seat-wrds.dll") -Force
        $sets = [ordered]@{ x86 = "i686-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc" }
        if ($Arch -eq "arm64") {
            $sets.arm64 = "aarch64-pc-windows-msvc"
            if (-not $NoArm64ec) { $sets.arm64ec = "arm64ec-pc-windows-msvc" }
        }
        $helperTarget = Join-Path $CargoTargetDir "apihelper"
        foreach ($k in $sets.Keys) {
            $t = $sets[$k]
            Invoke-Checked "app API helper $k ($t)" {
                cargo build --locked --release --manifest-path (Join-Path $repo "crates\seat-compat\apihelper\Cargo.toml") --target $t --target-dir $helperTarget
            }
            $dst = Join-Path $payload "apihelper\$k"
            New-Item -ItemType Directory -Force $dst | Out-Null
            Copy-Item (Join-Path $helperTarget "$t\release\pulsar_seat_api.dll") (Join-Path $dst "pulsar-seat-api.dll") -Force
        }
    } finally {
        $env:RUSTFLAGS = $oldFlags
    }
}
# Credential provider DLL (C++, msbuild; native for $Arch). With -SkipCargo it must
# already be in the payload folder.
if (-not $SkipCargo) {
    Invoke-Checked "credential provider ($Arch)" {
        # ARM64: an ARM64X DLL (also loadable by x64 processes under emulation); -NoArm64ec
        # test builds (no ARM64EC toolchain) get a plain ARM64 one (check-payload warns).
        & (Join-Path $PSScriptRoot "build-credprov.ps1") -Platforms @($(if ($Arch -eq "arm64") { "ARM64" } else { "x64" })) -OutDir $payload -NoArm64X:$NoArm64ec
    }
}
if ($SignedCredProv) {
    $sig = Get-AuthenticodeSignature $SignedCredProv
    if ($sig.Status -ne "Valid") { throw "-SignedCredProv ${SignedCredProv}: Authenticode $($sig.Status)" }
    Copy-Item $SignedCredProv (Join-Path $payload "pulsar-seat-credprov.dll") -Force
    Write-Host "credential provider: signed DLL from $SignedCredProv"
}
foreach ($n in "pulsar-seat-service.exe", "pulsar-seat-host.exe", "pulsar-seat-agent.exe", "pulsar-seat-credprov.dll", "pulsar-seat-wrds.dll") {
    if (-not (Test-Path (Join-Path $payload $n))) { throw "payload is missing $n ($payload)" }
}

# Per-process shim (remote seats, kiosk seats; shim/ is an own workspace). x64 package:
# shim\x64 (+ shimctl) and shim\x86. ARM64 package (parity-arm64): shim\arm64 (native module
# with the Detours backend, the launcher the service calls, shimctl), shim\x64 (x64 apps
# under emulation, no shimctl) and shim\x86. check-payload.ts checks every folder's machine.
$shimDir = Join-Path $payload "shim"
$shimTarget = Join-Path $CargoTargetDir "shim"
$shimTriples = [ordered]@{ x64 = "x86_64-pc-windows-msvc"; x86 = "i686-pc-windows-msvc" }
if ($Arch -eq "arm64") { $shimTriples = [ordered]@{ arm64 = "aarch64-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc"; x86 = "i686-pc-windows-msvc" } }
# The folder that carries shimctl: the package's own architecture.
$shimNative = if ($Arch -eq "arm64") { "arm64" } else { "x64" }
if (-not $SkipCargo) {
    $oldFlags = $env:RUSTFLAGS
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    try {
        foreach ($t in $shimTriples.Values) {
            # Own sub-folder (like the API helper): Tauri's MSI bundler picks up every DLL
            # next to the manager exe in <target>\<triple>\release.
            Invoke-Checked "shim ($t)" { cargo build --locked --release --manifest-path (Join-Path $repo "shim\Cargo.toml") --target $t --target-dir $shimTarget }
        }
    } finally {
        $env:RUSTFLAGS = $oldFlags
    }
    if (Test-Path $shimDir) { Remove-Item -Recurse -Force $shimDir }
    foreach ($f in $shimTriples.Keys) {
        $d = Join-Path $shimDir $f
        $r = Join-Path $shimTarget "$($shimTriples[$f])\release"
        New-Item -ItemType Directory -Force $d | Out-Null
        Copy-Item (Join-Path $r "PulsarSplitShim.dll") $d -Force
        $launcher = if ($f -eq "x86") { "pulsar-split-launch32.exe" } else { "pulsar-split-launch.exe" }
        Copy-Item (Join-Path $r "pulsar-split-launch.exe") (Join-Path $d $launcher) -Force
        if ($f -eq $shimNative) { Copy-Item (Join-Path $r "pulsar-split-shimctl.exe") $d -Force }
    }
}
$shimFiles = @("$shimNative\pulsar-split-shimctl.exe", "x86\PulsarSplitShim.dll", "x86\pulsar-split-launch32.exe")
foreach ($f in $shimTriples.Keys) { if ($f -ne "x86") { $shimFiles += "$f\PulsarSplitShim.dll", "$f\pulsar-split-launch.exe" } }
foreach ($n in $shimFiles) {
    if (-not (Test-Path (Join-Path $shimDir $n))) { throw "payload is missing shim\$n ($payload)" }
}
# Pinned third-party payload (drivers.json, drivers\, stream-server.json, stream-server\):
# downloaded and hash-checked by fetch-deps.ts (cached under target\installer-deps\cache),
# copied to <payload>\remote. -NoRemoteDeps builds a package without it (no network; test
# builds only, refused with -Release).
$remoteDir = Join-Path $payload "remote"
if (Test-Path $remoteDir) { Remove-Item -Recurse -Force $remoteDir }
if ($NoRemoteDeps) {
    if ($Release) { throw "-Release needs the remote payload; drop -NoRemoteDeps" }
    Write-Warning "no remote payload (-NoRemoteDeps): remote seats cannot stream with this package"
} else {
    $deps = Join-Path $repo "target\installer-deps"
    Invoke-Checked "remote payload (fetch-deps.ts)" { bun (Join-Path $repo "installer\scripts\fetch-deps.ts") --arch $Arch --out $deps }
    Copy-Item (Join-Path $deps "$Arch\remote") $remoteDir -Recurse -Force
}
# LogonUI loads the credential provider as SYSTEM: a published release carries it signed.
# CI tag builds have no signing (no secret in CI), so this warns here; sign-release.ps1
# refuses an MSI whose DLL is not validly signed, and without its .minisig no client offers
# the release.
if ($Release -and (Get-AuthenticodeSignature (Join-Path $payload "pulsar-seat-credprov.dll")).Status -ne "Valid") {
    Write-Warning "pulsar-seat-credprov.dll is not Authenticode-signed: this MSI is for testing only (sign-release.ps1 refuses it; rebuild with -SignedCredProv)"
}

# ---------------------------------------------------------------- 3: signed drivers
$staged = Join-Path $work "drivers"
if (Test-Path $staged) { Remove-Item -Recurse -Force $staged }
New-Item -ItemType Directory -Force $staged | Out-Null
$checkArgs = @()
if ($SignedDrivers) {
    $publisher = "Microsoft Windows Hardware Compatibility Publisher"
    # signtool of the Windows SDK, native for this host (catalog membership check).
    $hostArch = if ((Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture -eq 12) { "arm64" } else { "x64" }
    $signtool = Get-ChildItem (Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin\*\$hostArch\signtool.exe") -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
    if (-not $signtool) { Write-Warning "signtool not found: only embedded Microsoft signatures are accepted on driver binaries" }
    $pkgs = Get-ChildItem $SignedDrivers -Directory
    if (-not $pkgs) { throw "-SignedDrivers ${SignedDrivers}: no package folders" }
    foreach ($p in $pkgs) {
        $cats = @(Get-ChildItem $p.FullName -Filter *.cat -File)
        $infs = @(Get-ChildItem $p.FullName -Filter *.inf -File)
        if ($cats.Count -ne 1 -or $infs.Count -ne 1) { throw "$($p.Name): need exactly one .inf and one .cat" }
        $sig = Get-AuthenticodeSignature $cats[0].FullName
        if ($sig.Status -ne "Valid") { throw "$($p.Name): catalog signature $($sig.Status) - not staged" }
        $subject = $sig.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
        if ($subject -ne $publisher) { throw "$($p.Name): catalog signed by '$subject', not '$publisher' - not staged" }
        # Every binary must be a member of that catalog (signtool verify /c: hash listed in
        # the catalog and the catalog's signature valid) or embedded-signed by the same
        # Microsoft publisher. An embedded signature from anyone else refuses the package.
        foreach ($b in Get-ChildItem $p.FullName -File | Where-Object { $_.Extension -in ".sys", ".dll" }) {
            $bs = Get-AuthenticodeSignature $b.FullName
            $embedded = $bs.Status -eq "Valid" -and $bs.SignerCertificate -and
                $bs.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -eq $publisher
            if ($bs.SignerCertificate -and -not $embedded -and $bs.Status -ne "NotSigned") {
                $who = $bs.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
                throw "$($p.Name)\$($b.Name): signed by '$who' ($($bs.Status)), not '$publisher' - not staged"
            }
            $inCatalog = $false
            if ($signtool) {
                # Windows PowerShell turns native stderr into errors; "Stop" would throw.
                $eap = $ErrorActionPreference
                $ErrorActionPreference = "Continue"
                try { & $signtool verify /q /pa /c $cats[0].FullName $b.FullName 2>$null | Out-Null; $inCatalog = $LASTEXITCODE -eq 0 }
                finally { $ErrorActionPreference = $eap }
            }
            if (-not ($embedded -or $inCatalog)) {
                throw "$($p.Name)\$($b.Name): neither in $($cats[0].Name) nor embedded-signed by '$publisher' - not staged"
            }
        }
        Copy-Item $p.FullName (Join-Path $staged $p.Name) -Recurse
        Write-Host "staged signed driver package $($p.Name) ($subject)"
    }
    $checkArgs += "--allow-drivers"
}

# ---------------------------------------------------------------- 4: payload gate
Invoke-Checked "payload check" { bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $Arch $payload }
if ($SignedDrivers) {
    Invoke-Checked "driver package check" { bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $Arch @checkArgs $staged }
}

# ---------------------------------------------------------------- 5: bundle config
$config = Join-Path $work "tauri-bundle.json"
Invoke-Checked "bundle config" {
    $extra = @("--bin", $payload); if ($NoArm64ec -and $Arch -eq "arm64") { $extra += "--no-arm64ec" }
    if (Test-Path $remoteDir) { $extra += @("--remote", $remoteDir) }
    bun (Join-Path $repo "installer\scripts\bundle-config.ts") $config --drivers $staged --version $Version --language $Language @extra
}

# ---------------------------------------------------------------- 6: tauri build (MSI)
$env:PULSAR_SEAT_BIN = $payload
$oldFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = "-C target-feature=+crt-static"
# Tauri's MSI bundler adds every DLL next to the manager exe in <target>\<triple>\release.
# A shared target dir collects DLLs of other builds there (split-pad from build-drivers.ps1,
# a shim built without its own --target-dir), which then landed in the MSI root (VM round
# 2026-11-17: check-payload `shim_outside_folder`). Hold them aside while tauri runs.
$releaseDir = Join-Path $CargoTargetDir "$triple\release"
$held = Join-Path $releaseDir "held-dlls"
$heldDlls = @(Get-ChildItem $releaseDir -File -Filter *.dll -ErrorAction SilentlyContinue)
if ($heldDlls) {
    New-Item -ItemType Directory -Force $held | Out-Null
    $heldDlls | ForEach-Object { Write-Host "holding aside $($_.Name) (not part of the MSI)"; Move-Item $_.FullName $held -Force }
}
Push-Location (Join-Path $repo "apps\manager")
try {
    Invoke-Checked "tauri build --bundles msi" { bun run tauri build --target $triple --bundles msi --config $config }
} finally {
    Pop-Location
    $env:RUSTFLAGS = $oldFlags
    if (Test-Path $held) {
        Get-ChildItem $held -File | ForEach-Object { Move-Item $_.FullName $releaseDir -Force }
        Remove-Item $held -Force -ErrorAction SilentlyContinue
    }
}
$manager = Join-Path $CargoTargetDir "$triple\release\pulsar-seat-manager.exe"
Copy-Item $manager $payload -Force
Invoke-Checked "payload check (with manager)" { bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $Arch $payload }

$msiDir = Join-Path $CargoTargetDir "$triple\release\bundle\msi"
$msi = Get-ChildItem $msiDir -Filter "*_${Version}_*.msi" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $msi) { $msi = Get-ChildItem $msiDir -Filter *.msi | Sort-Object LastWriteTime -Descending | Select-Object -First 1 }
if (-not $msi) { throw "no MSI in $msiDir" }
if ($msi.LastWriteTime -lt $buildStarted) { throw "stale MSI $($msi.FullName) (written $($msi.LastWriteTime), build started $buildStarted)" }

# ---------------------------------------------------------------- 7: check inside the MSI
$extract = Join-Path $work "msi-extract"
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
New-Item -ItemType Directory -Force $extract | Out-Null
$p = Start-Process msiexec.exe -Wait -PassThru -NoNewWindow -ArgumentList "/a", "`"$($msi.FullName)`"", "/qn", "TARGETDIR=`"$extract`"", "/l*", "`"$work\msi-extract.log`""
if ($p.ExitCode -ne 0) { throw "administrative extraction failed (exit $($p.ExitCode), log $work\msi-extract.log)" }
# A release MSI must carry every licence text, the generated third-party notices included.
$msiCheckArgs = @($checkArgs); if ($Release) { $msiCheckArgs += "--notices" }
Invoke-Checked "payload check inside the MSI" { bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $Arch @msiCheckArgs $extract }

# ---------------------------------------------------------------- 8: dist + checksums
$name = if ($Language -eq "en-US") { "PulsarSeat_${Version}_$Arch.msi" } else { "PulsarSeat_${Version}_${Arch}_$Language.msi" }
$out = Join-Path $dist $name
Copy-Item $msi.FullName $out -Force
$hash = (Get-FileHash $out -Algorithm SHA256).Hash.ToLowerInvariant()
# Build record next to the MSI (published with it as a CI artifact, never inside the MSI):
# sign-release.ps1 refuses to sign a dist whose MSIs come from different or uncommitted
# sources, or whose record does not match the service binary inside the MSI (a stale MSI
# of an older build left in dist/).
$commit = (git -C $repo rev-parse --verify --quiet "HEAD^{commit}" 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $commit) { $commit = "none" }
$dirty = @(git -C $repo status --porcelain --untracked-files=no 2>$null).Count -gt 0
$svcHash = (Get-FileHash (Join-Path $payload "pulsar-seat-service.exe") -Algorithm SHA256).Hash.ToLowerInvariant()
$record = @(
    "version: $Version",
    "arch: $Arch",
    "commit: $commit",
    "dirty: $($dirty.ToString().ToLowerInvariant())",
    "release: $($Release.IsPresent.ToString().ToLowerInvariant())",
    "built_utc: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "msi_sha256: $hash",
    "service_sha256: $svcHash"
)
[System.IO.File]::WriteAllText("$out.build.txt", (($record -join "`n") + "`n"))
$sums = Join-Path $dist "SHA256SUMS.txt"
$lines = @()
if (Test-Path $sums) { $lines = @(Get-Content $sums | Where-Object { $_ -and -not $_.EndsWith("  $name") }) }
$lines += "$hash  $name"
# LF line endings, no BOM: the `sha256sum -c` format the update check parses.
[System.IO.File]::WriteAllText($sums, (($lines | Sort-Object { $_.Substring(66) }) -join "`n") + "`n")
Write-Host "MSI:  $out"
Write-Host "SHA-256: $hash"
Write-Host "(unsigned: Authenticode-sign the MSI, rerun write-sums.ps1, then sign-release.ps1 for SHA256SUMS.txt.minisig;"
Write-Host " the update check refuses unsigned installers and unsigned checksum files - docs/release/checklist.md)"
