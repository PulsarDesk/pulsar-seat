# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the Pulsar Seat credential provider DLL (credprov/credprov.vcxproj) and its
# self-test for each platform, runs the self-test where this PC can run it, checks the PE
# machine of every output (0xAA64 ARM64, 0x8664 x64) and, with -OutDir, copies
# pulsar-seat-credprov.dll there. Never registers anything (only the service does that,
# crates/seat-session/src/credprov.rs). Unsigned; see credprov/README.md.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\build-credprov.ps1
#   ... -Platforms ARM64 -OutDir target\payload\arm64
param(
    [ValidateSet("ARM64", "x64")] [string[]] $Platforms = @("ARM64", "x64"),
    [ValidateSet("Release", "Debug")] [string[]] $Configurations = @("Release"),
    [string] $OutDir = "",
    [switch] $NoSelfTest
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

$msbuild = Find-MSBuild
$want = @{ ARM64 = 0xAA64; x64 = 0x8664 }
foreach ($pl in $Platforms) {
    foreach ($c in $Configurations) {
        foreach ($p in "credprov.vcxproj", "credprov-selftest.vcxproj") {
            Write-Host "== msbuild $p $pl $c"
            # Output goes to a log file, never through a pipe (a cut pipe leaves msbuild hanging).
            $log = Join-Path $proj "build\msbuild-$($p -replace '\.vcxproj$','')-$pl-$c.log"
            New-Item -ItemType Directory -Force (Split-Path $log) | Out-Null
            & $msbuild (Join-Path $proj $p) -nologo -v:minimal "-p:Configuration=$c" "-p:Platform=$pl" "-p:WindowsTargetPlatformVersion=10.0.28000.0" "-fl" "-flp:logfile=$log;verbosity=minimal" | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "msbuild $p $pl $c failed (see $log)" }
        }
        $dir = Join-Path $proj "build\$pl\$c"
        foreach ($f in "pulsar-seat-credprov.dll", "credprov-selftest.exe") {
            $m = Get-Machine (Join-Path $dir $f)
            if ($m -ne $want[$pl]) { throw ("{0}: machine 0x{1:X4}, want 0x{2:X4}" -f $f, $m, $want[$pl]) }
            Write-Host ("   {0} 0x{1:X4}" -f $f, $m)
        }
        # ARM64 hosts run x64 under emulation; an x64 host cannot run ARM64.
        $canRun = $hostArch -eq "arm64" -or $pl -eq "x64"
        if (-not $NoSelfTest -and $canRun) {
            & (Join-Path $dir "credprov-selftest.exe") | Out-Host
            if ($LASTEXITCODE -ne 0) { throw "credprov self-test $pl $c failed" }
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
