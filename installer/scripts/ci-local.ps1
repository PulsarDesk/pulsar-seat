# SPDX-License-Identifier: GPL-3.0-only
#
# Runs the gates of .github/workflows/ci.yml on this machine, in the same order and with
# the same commands, without installing anything (needs rustup targets, bun, and for
# -Drivers the VS 18 Build Tools + WDK 10.0.28000 of drivers/README.md). Stops at the
# first failing gate and prints a summary.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\ci-local.ps1
#   ... -Targets aarch64-pc-windows-msvc            (one target only)
#   ... -Payload        also the release binaries, helper DLLs, the credprov DLL and the PE / payload gate (implies -CredProv)
#   ... -CredProv       also the credprov job (msbuild ARM64 + x64, self-test; VS 18 Build Tools + SDK)
#   ... -Drivers        also the drivers job (msbuild ARM64 + x64, infverif, analysis, submission CABs)
#   ... -Installers     also the MSI for the native arch (build-installer.ps1 -SkipCargo; implies -Payload)
#   ... -Release        also the tag-only gates (release key list); never sets up a tag
#
# Tests run only for the native target (CI runs each target on its own runner); the other
# target is built and clippy-checked (cross-compiled), like cargo check.
param(
    [string[]] $Targets = @("aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc"),
    [switch] $Payload,
    [switch] $CredProv,
    [switch] $Drivers,
    [switch] $Installers,
    [switch] $Release,
    [string] $CargoTargetDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if ($CargoTargetDir) { $env:CARGO_TARGET_DIR = $CargoTargetDir }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }
$native = if ((Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture -eq 12) { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$archOf = @{ "aarch64-pc-windows-msvc" = "arm64"; "x86_64-pc-windows-msvc" = "x64" }
$done = New-Object System.Collections.Generic.List[string]
$sw = [System.Diagnostics.Stopwatch]::StartNew()

function Gate([string] $Name, [scriptblock] $Block) {
    Write-Host ""
    Write-Host "== $Name" -ForegroundColor Cyan
    # Not $t: the block runs in this scope (dynamic scoping) and the callers' loops use $t.
    $gateClock = [System.Diagnostics.Stopwatch]::StartNew()
    $global:LASTEXITCODE = 0
    $failure = $null
    try { & $Block } catch { $failure = $_.Exception.Message }
    if (-not $failure -and $LASTEXITCODE -ne 0) { $failure = "exit $LASTEXITCODE" }
    if ($failure) {
        Write-Host "FAILED: $Name ($failure)" -ForegroundColor Red
        Write-Host "passed before it: $($done -join ', ')"
        exit 1
    }
    $done.Add("$Name ($([int]$gateClock.Elapsed.TotalSeconds) s)")
}

# Each native command checked on its own (a block only reports its last exit code).
# Callers quote '--': PowerShell eats a bare -- as its own end-of-parameters marker when it
# binds a function call, so cargo would never see it.
function Run([string] $Exe) {
    & $Exe @args
    if ($LASTEXITCODE -ne 0) { throw "$Exe $($args -join ' ') failed (exit $LASTEXITCODE)" }
}

Push-Location $repo
try {
    # Structure of ci.yml, plus: every job / installer script there has its counterpart in
    # this file (sections "# ---- <job> job", or a "# ci-local skips:" line).
    Gate "workflow lint" { Run bun installer/scripts/lint-ci.ts .github/workflows/ci.yml --local installer/scripts/ci-local.ps1 }
    Gate "bun install" { Run bun install --frozen-lockfile }

    # ---- rust job
    foreach ($t in $Targets) {
        Gate "cargo build ($t)" { Run cargo build --locked --target $t }
        if ($t -eq $native) {
            Gate "cargo test ($t)" { Run cargo test --locked --target $t }
        }
        Gate "clippy ($t)" { Run cargo clippy --locked --target $t --all-targets --features seat-core/mock '--' -D warnings }
    }
    Gate "manager frontend build" { Push-Location apps/manager; try { Run bun run build } finally { Pop-Location } }
    foreach ($t in $Targets) {
        if ($t -eq $native) { Gate "manager (Rust) test ($t)" { Run cargo test --locked --target $t -p pulsar-seat-manager } }
        Gate "manager (Rust) clippy ($t)" { Run cargo clippy --locked --target $t -p pulsar-seat-manager --all-targets '--' -D warnings }
    }
    # Separate Cargo workspaces, same target dir (debug builds only; the release builds of
    # build-installer.ps1 keep their own sub-folders, see installer/README.md).
    foreach ($t in $Targets) {
        if ($t -eq $native) {
            Gate "API helper test ($t)" { Run cargo test --locked --manifest-path crates/seat-compat/apihelper/Cargo.toml --target $t }
        }
        Gate "API helper clippy ($t)" { Run cargo clippy --locked --manifest-path crates/seat-compat/apihelper/Cargo.toml --target $t --all-targets '--' -D warnings }
    }

    # ---- bun job
    Gate "manager check / test" { Push-Location apps/manager; try { Run bun run check; Run bun run test } finally { Pop-Location } }
    Gate "installer scripts (bun test)" { Run bun test installer/scripts }
    Gate "public-repository scan" { Run bun installer/scripts/repo-scan.ts }

    # ---- shim job (remote kind's per-process shim, own x64 / x86 workspace). CI runs it on
    # the x64 runner; on an ARM64 host the x64 test binaries run under x64 emulation.
    Gate "shim test + clippy (x64), check (x86)" {
        Run cargo test --locked --manifest-path shim/Cargo.toml --target x86_64-pc-windows-msvc
        Run cargo clippy --locked --manifest-path shim/Cargo.toml --target x86_64-pc-windows-msvc --all-targets '--' -D warnings
        Run cargo check --locked --manifest-path shim/Cargo.toml --target i686-pc-windows-msvc --all-targets
    }
    # ---- shim-arm64 job: the ARM64 module (Detours backend, parity-arm64); only an ARM64
    # host runs its tests (its release builds are build-installer.ps1's, -Installers).
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
        Gate "shim test + clippy (arm64)" {
            Run cargo test --locked --manifest-path shim/Cargo.toml --target aarch64-pc-windows-msvc
            Run cargo clippy --locked --manifest-path shim/Cargo.toml --target aarch64-pc-windows-msvc --all-targets '--' -D warnings
        }
    }

    if ($Release) {
        Gate "release key list (tag gate)" {
            Run cargo run --locked --quiet -p seat-release --bin pulsar-seat-release '--' check-signers installer/release-signers.txt --require
        }
    }

    # ---- credprov job (C++ credential provider: Release + Debug, /WX, self-test)
    # -Command, not -File: -File passes the array Release,Debug as the text "System.Object[]".
    if ($CredProv -or $Payload -or $Installers) {
        foreach ($p in "ARM64", "x64") {
            Gate "credprov $p (build, self-test, PE)" {
                Run powershell -NoProfile -ExecutionPolicy Bypass -Command "& ./installer/scripts/build-credprov.ps1 -Platforms $p -Configurations Release,Debug; exit `$LASTEXITCODE"
            }
        }
    }

    # ---- payload (rust job release part)
    if ($Payload -or $Installers) {
        # Release helper DLLs in their own folder: Tauri's MSI bundler would pick up a DLL
        # in <target>\<triple>\release (same rule as build-installer.ps1).
        $helperDir = Join-Path $targetDir "apihelper"
        foreach ($t in $Targets) {
            $arch = $archOf[$t]
            $out = Join-Path $repo "target\payload\$arch"
            $old = $env:RUSTFLAGS
            $env:RUSTFLAGS = "-C target-feature=+crt-static"
            if ($Release) { $env:PULSAR_SEAT_RELEASE = "1" }
            try {
                Gate "release binaries ($t)" {
                    Run cargo build --locked --release --target $t -p seat-service
                    Run cargo build --locked --release --target $t -p seat-host
                }
                $sets = [ordered]@{ x86 = "i686-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc" }
                if ($arch -eq "arm64") { $sets.arm64 = "aarch64-pc-windows-msvc"; $sets.arm64ec = "arm64ec-pc-windows-msvc" }
                foreach ($k in $sets.Keys) {
                    $ht = $sets[$k]
                    Gate "API helper DLL $k ($ht)" {
                        Run cargo build --locked --release --manifest-path crates/seat-compat/apihelper/Cargo.toml --target $ht --target-dir $helperDir
                        $dst = Join-Path $out "apihelper\$k"
                        New-Item -ItemType Directory -Force $dst | Out-Null
                        Copy-Item (Join-Path $helperDir "$ht\release\pulsar_seat_api.dll") (Join-Path $dst "pulsar-seat-api.dll") -Force
                    }
                }
            } finally {
                $env:RUSTFLAGS = $old
                Remove-Item Env:PULSAR_SEAT_RELEASE -ErrorAction SilentlyContinue
            }
            Gate "payload + PE machine ($arch)" {
                New-Item -ItemType Directory -Force $out | Out-Null
                foreach ($n in "pulsar-seat-service.exe", "pulsar-seat-host.exe", "pulsar-seat-agent.exe") {
                    Copy-Item (Join-Path $targetDir "$t\release\$n") $out -Force
                }
                $pl = if ($arch -eq "arm64") { "ARM64" } else { "x64" }
                Copy-Item (Join-Path $repo "credprov\build\$pl\Release\pulsar-seat-credprov.dll") $out -Force
                Run bun installer/scripts/check-payload.ts --arch $arch $out
            }
        }
    }

    # ---- drivers job
    if ($Drivers) {
        foreach ($p in "x64", "ARM64") {
            Gate "drivers $p (build, infverif, PE)" {
                Run powershell -NoProfile -ExecutionPolicy Bypass -Command "& ./installer/scripts/build-drivers.ps1 -Platforms $p -Configurations Release,Debug; exit `$LASTEXITCODE"
            }
            Gate "drivers $p (WDK code analysis)" {
                Run powershell -NoProfile -ExecutionPolicy Bypass -File installer/scripts/build-drivers.ps1 -Platforms $p -Configurations Release -Analyze -SkipInfVerif -OutRoot target/drivers-analyze
            }
            $arch = if ($p -eq "ARM64") { "arm64" } else { "x64" }
            Gate "drivers $p (payload checker, machine only)" {
                Run bun installer/scripts/check-payload.ts --arch $arch --machine-only "target/drivers/$p/Release"
            }
        }
        # CI makes one CAB per platform job; here both platforms exist, so one CAB per driver
        # with both (the layout the maintainer submits; docs/release/driver-submission.md).
        Gate "drivers submission CABs (unsigned)" {
            Run bun installer/scripts/driver-cab.ts --out target/driver-submission --allow-unallocated-altitude
        }
    }

    # ---- sdv job: not run here (the 17 GB 22H2 EWDK, x64 tools; docs/release/sdv.md has
    # the local run-sdv.ps1 command)
    # ci-local skips: run-sdv.ps1 (sdv job: needs the mounted EWDK ISO)
    # ci-local skips: js-notices.ts (installers job, tag builds: generated together with cargo-about, which CI installs; build-installer.ps1 -Release refuses without the notices)

    # ---- installers job (native arch only; Tauri needs its frontend + WiX download)
    if ($Installers) {
        $arch = $archOf[$native]
        $extra = @(); if ($Release) { $extra += "-Release" }
        Gate "MSI ($arch)" {
            Run powershell -NoProfile -ExecutionPolicy Bypass -File installer/scripts/build-installer.ps1 -Arch $arch -SkipCargo @extra
        }
    }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "All gates passed in $([int]$sw.Elapsed.TotalMinutes) min:" -ForegroundColor Green
$done | ForEach-Object { Write-Host "  $_" }
