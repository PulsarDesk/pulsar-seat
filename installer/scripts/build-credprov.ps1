# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the Pulsar Seat credential provider DLL (credprov/credprov.vcxproj) and its
# self-test for each platform, runs the self-test where this PC can run it, checks the PE
# machine of every output (0xAA64 ARM64, 0x8664 x64) and, with -OutDir, copies
# pulsar-seat-credprov.dll there. Never registers anything (only the service does that,
# crates/seat-session/src/credprov.rs). Unsigned; see credprov/README.md.
#
# ARM64 builds an ARM64X DLL (BuildAsX in credprov.vcxproj: ARM64 half for the native
# LogonUI, ARM64EC half for x64 processes under emulation, e.g. CredUI hosts) and checks it
# is hybrid (CHPE metadata in the load config). The self-test also runs as ARM64EC there, so
# the EC half's code is exercised natively. -NoArm64X builds a plain ARM64 DLL (toolchain
# without the ARM64EC MSVC libraries: test builds only).
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\build-credprov.ps1
#   ... -Platforms ARM64 -OutDir target\payload\arm64
param(
    [ValidateSet("ARM64", "x64")] [string[]] $Platforms = @("ARM64", "x64"),
    [ValidateSet("Release", "Debug")] [string[]] $Configurations = @("Release"),
    [string] $OutDir = "",
    [switch] $NoSelfTest,
    [switch] $NoArm64X
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$proj = Join-Path $repo "credprov"
$hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -or $env:PROCESSOR_ARCHITEW6432 -eq "ARM64") { "arm64" } else { "x64" }

function Find-MSBuild {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    $roots = @()
    if (Test-Path $vswhere) {
        $roots = @(& $vswhere -products * -latest -requires Microsoft.Component.MSBuild -property installationPath)
    }
    $roots += Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\18\BuildTools"
    foreach ($r in $roots) {
        if (-not $r) { continue }
        foreach ($sub in @("MSBuild\Current\Bin\$hostArch\MSBuild.exe", "MSBuild\Current\Bin\amd64\MSBuild.exe", "MSBuild\Current\Bin\MSBuild.exe")) {
            $p = Join-Path $r $sub
            if (Test-Path $p) { return $p }
        }
    }
    throw "MSBuild not found (install Visual Studio Build Tools 18 with the Windows SDK 10.0.28000)"
}

function Get-Machine([string] $Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    $o = [BitConverter]::ToInt32($b, 0x3c)
    return [BitConverter]::ToUInt16($b, $o + 4)
}

# True when a PE32+ image carries CHPE (hybrid) metadata: IMAGE_LOAD_CONFIG_DIRECTORY64
# .CHPEMetadataPointer (offset 0xC8) is set. With machine 0xAA64 that is an ARM64X image.
# Same rule as isHybridPe in check-payload.ts.
function Test-Hybrid([string] $Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($b, 0x3c)
    $opt = $pe + 24
    if ([BitConverter]::ToUInt16($b, $opt) -ne 0x20B) { return $false }
    $nSec = [BitConverter]::ToUInt16($b, $pe + 6)
    $optSize = [BitConverter]::ToUInt16($b, $pe + 20)
    $lcRva = [BitConverter]::ToUInt32($b, $opt + 112 + 10 * 8)
    if ($lcRva -eq 0) { return $false }
    $sec = $opt + $optSize
    for ($i = 0; $i -lt $nSec; $i++) {
        $s = $sec + 40 * $i
        $va = [BitConverter]::ToUInt32($b, $s + 12)
        $vs = [BitConverter]::ToUInt32($b, $s + 8)
        $raw = [BitConverter]::ToUInt32($b, $s + 20)
        if ($lcRva -ge $va -and $lcRva -lt $va + $vs) {
            $lc = $raw + ($lcRva - $va)
            if ([BitConverter]::ToUInt32($b, $lc) -lt 0xD0) { return $false }
            return [BitConverter]::ToUInt64($b, $lc + 0xC8) -ne 0
        }
    }
    return $false
}

$msbuild = Find-MSBuild
$want = @{ ARM64 = 0xAA64; x64 = 0x8664; ARM64EC = 0x8664 }
function Invoke-Build([string] $p, [string] $pl, [string] $c, [string[]] $extra = @()) {
    Write-Host "== msbuild $p $pl $c $extra"
    # Output goes to a log file, never through a pipe (a cut pipe leaves msbuild hanging).
    $log = Join-Path $proj "build\msbuild-$($p -replace '\.vcxproj$','')-$pl-$c.log"
    New-Item -ItemType Directory -Force (Split-Path $log) | Out-Null
    & $msbuild (Join-Path $proj $p) -nologo -v:minimal "-p:Configuration=$c" "-p:Platform=$pl" "-p:WindowsTargetPlatformVersion=10.0.28000.0" @extra "-fl" "-flp:logfile=$log;verbosity=minimal" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "msbuild $p $pl $c failed (see $log)" }
}
function Invoke-SelfTest([string] $pl, [string] $c) {
    $exe = Join-Path $proj "build\$pl\$c\credprov-selftest.exe"
    $m = Get-Machine $exe
    if ($m -ne $want[$pl]) { throw ("credprov-selftest.exe ({0}): machine 0x{1:X4}, want 0x{2:X4}" -f $pl, $m, $want[$pl]) }
    Write-Host ("   credprov-selftest.exe ({0}) 0x{1:X4}" -f $pl, $m)
    # ARM64 hosts run x64 / ARM64EC; an x64 host cannot run ARM64 or ARM64EC.
    $canRun = $hostArch -eq "arm64" -or $pl -eq "x64"
    if (-not $NoSelfTest -and $canRun) {
        & $exe | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "credprov self-test $pl $c failed" }
    }
}
foreach ($pl in $Platforms) {
    foreach ($c in $Configurations) {
        $x = $pl -eq "ARM64" -and -not $NoArm64X
        $extra = @(); if ($pl -eq "ARM64" -and $NoArm64X) { $extra = @("-p:BuildAsX=false") }
        Invoke-Build "credprov.vcxproj" $pl $c $extra
        Invoke-Build "credprov-selftest.vcxproj" $pl $c
        $dll = Join-Path $proj "build\$pl\$c\pulsar-seat-credprov.dll"
        $m = Get-Machine $dll
        if ($m -ne $want[$pl]) { throw ("pulsar-seat-credprov.dll: machine 0x{0:X4}, want 0x{1:X4}" -f $m, $want[$pl]) }
        $hybrid = Test-Hybrid $dll
        if ($x -and -not $hybrid) { throw "pulsar-seat-credprov.dll ($pl $c) is not ARM64X (no CHPE metadata)" }
        if (-not $x -and $hybrid) { throw "pulsar-seat-credprov.dll ($pl $c) is hybrid, want a plain $pl image" }
        Write-Host ("   pulsar-seat-credprov.dll 0x{0:X4}{1}" -f $m, $(if ($hybrid) { " (ARM64X)" } else { "" }))
        Invoke-SelfTest $pl $c
        if ($x) {
            # The EC half of the ARM64X DLL, as a runnable self-test.
            Invoke-Build "credprov-selftest.vcxproj" "ARM64EC" $c
            Invoke-SelfTest "ARM64EC" $c
        }
    }
}
if ($OutDir) {
    if ($Platforms.Count -ne 1) { throw "-OutDir needs exactly one platform" }
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    Copy-Item (Join-Path $proj "build\$($Platforms[0])\$($Configurations[0])\pulsar-seat-credprov.dll") $OutDir -Force
    Write-Host "copied pulsar-seat-credprov.dll to $OutDir"
}
$global:LASTEXITCODE = 0
