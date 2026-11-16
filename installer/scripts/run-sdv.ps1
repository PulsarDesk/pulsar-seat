# SPDX-License-Identifier: GPL-3.0-only
#
# Static Driver Verifier (SDV) on the kernel drivers, with the last Enterprise WDK that
# still ships SDV: EWDK for Windows 11 22H2 (10.0.22621, VS Build Tools 17.x), ISO
# EWDK_ni_release_svc_prod1_22621_230929-1800.iso (https://go.microsoft.com/fwlink/?linkid=2249942,
# listed on Microsoft Learn "Unsupported WDK versions"). SDV is not in WDK 10.0.28000.
# See docs/release/sdv.md for what ran where.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\run-sdv.ps1 -EwdkRoot E:\
#   ... -Projects seat-input-filter -Platform x64 -WorkDir target\sdv
#
# The EWDK is used read-only (mounted ISO). The projects are copied into -WorkDir so SDV's
# own files (sdv\, sdv-map.h, smvbuild.log ...) never land in drivers/. The copy is built
# against the EWDK's SDK / WDK 10.0.22621 (WindowsTargetPlatformVersion override): SDV
# analyses the code against that kit's DDI model, so a DDI newer than 22621 would show up
# here as a build error, not as an SDV result.
#
# Exit code: 0 = SDV ran and every project is clean: no defect, at least one rule passed,
# no rule timed out; 1 = a defect, a failed run, a project SDV could not analyse (no rule
# passed, or the result counts are missing), or a timeout (a rule SDV never finished is not
# verified; -AllowTimeouts reports them without failing, for a local look only - the release
# gate never passes it). Results: <WorkDir>\<project>\sdv\SDV.DVL.xml and the summary
# <WorkDir>\sdv-summary.txt (one line per project with the counts and why it failed).
param(
    [Parameter(Mandatory = $true)] [string] $EwdkRoot,
    [string[]] $Projects = @("seat-input-filter", "seat-storage-filter", "seat-wfp"),
    [ValidateSet("x64")] [string] $Platform = "x64",
    [string] $Configuration = "Release",
    [string] $WorkDir = "",
    # SDV rule set: "default.sdv" (the driver model's default rules) or "all" (every rule).
    [string] $Check = "default.sdv",
    # Report rule timeouts without failing the run (local analysis only; not clean).
    [switch] $AllowTimeouts
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $WorkDir) { $WorkDir = Join-Path $repo "target\sdv" }
if (-not [System.IO.Path]::IsPathRooted($WorkDir)) { $WorkDir = Join-Path $repo $WorkDir }
$WorkDir = [System.IO.Path]::GetFullPath($WorkDir)
$setup = Join-Path $EwdkRoot "BuildEnv\SetupBuildEnv.cmd"
if (-not (Test-Path $setup)) { throw "$setup not found: -EwdkRoot must be the mounted EWDK (22621) root" }
$sdvBin = Get-ChildItem (Join-Path $EwdkRoot "Program Files\Windows Kits\10\Tools") -Recurse -Filter staticdv.exe -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $sdvBin) { throw "staticdv.exe not found under $EwdkRoot (this EWDK has no SDV)" }
Write-Host "SDV: $($sdvBin.FullName)"

if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Force $WorkDir | Out-Null
$summary = New-Object System.Collections.Generic.List[string]
$failed = $false
$notClean = $false

foreach ($p in $Projects) {
    $src = Join-Path $repo "drivers\$p"
    if (-not (Test-Path (Join-Path $src "$p.vcxproj"))) { throw "no drivers\$p\$p.vcxproj" }
    $dst = Join-Path $WorkDir $p
    Copy-Item $src $dst -Recurse
    $proj = Join-Path $dst "$p.vcxproj"
    $log = Join-Path $WorkDir "$p.log"
    # SDV's own inner build (its NormalBuild / intercepted build stages) runs msbuild on the
    # project with only Configuration / Platform, so command-line properties do not reach
    # it: the copy itself is retargeted to the EWDK's kit. 10.0.22621 instead of 10.0.28000;
    # Spectre libraries off (not needed for analysis; the projects set it as
    # Driver_SpectreMitigation, the generic name is matched too); ApiValidator off (its aitstatic
    # helper exits with 216 under x64 emulation on an ARM64 host); no code analysis.
    $x = Get-Content $proj -Raw
    $x = $x -replace '<WindowsTargetPlatformVersion>[^<]*</WindowsTargetPlatformVersion>', '<WindowsTargetPlatformVersion>10.0.22621.0</WindowsTargetPlatformVersion>'
    $x = $x -replace '<Driver_SpectreMitigation>[^<]*</Driver_SpectreMitigation>', '<Driver_SpectreMitigation>false</Driver_SpectreMitigation>'
    $x = $x -replace '<SpectreMitigation>[^<]*</SpectreMitigation>', '<SpectreMitigation>false</SpectreMitigation>'
    if ($x -match '<(Driver_)?SpectreMitigation>(?!false<)') { throw "${p}: Spectre mitigation still on in the SDV copy" }
    $x = $x -replace '(<PropertyGroup Label="Globals">)', "`$1`r`n    <ApiValidator_Enable>false</ApiValidator_Enable>`r`n    <RunCodeAnalysis>false</RunCodeAnalysis>"
    [System.IO.File]::WriteAllText($proj, $x)
    $props = "/p:Configuration=$Configuration /p:Platform=$Platform"
    # One cmd session: the EWDK environment, a clean, the SDV check, then the defect log
    # view (writes SDV.DVL.xml). msbuild is the EWDK's x64 build; on an ARM64 host it runs
    # under emulation. cmd redirects itself, so the log is plain ANSI text.
    $bat = Join-Path $WorkDir "$p-sdv.cmd"
    $lines = @(
        "@echo off",
        "call `"$setup`" amd64 >nul",
        "msbuild `"$proj`" /t:clean $props /nologo /v:m || exit /b 1",
        "msbuild `"$proj`" /t:sdv `"/p:Inputs=/check:$Check`" $props /nologo /v:n || exit /b 2",
        "msbuild `"$proj`" /t:sdv `"/p:Inputs=/dvl`" $props /nologo /v:m || exit /b 3",
        "exit /b 0"
    )
    [System.IO.File]::WriteAllText($bat, ($lines -join "`r`n") + "`r`n", [System.Text.Encoding]::ASCII)
    Write-Host "== SDV $p ($Platform $Configuration, $Check)"
    $proc = Start-Process cmd.exe -ArgumentList "/d", "/c", "`"`"$bat`" > `"$log`" 2>&1`"" -Wait -PassThru -NoNewWindow -WorkingDirectory $dst
    $code = $proc.ExitCode
    $text = Get-Content $log -Raw
    $dvl = Join-Path $dst "SDV.DVL.xml"
    if (-not (Test-Path $dvl)) { $dvl = Get-ChildItem $dst -Recurse -Filter SDV.DVL.xml -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName }
    $defects = if ($text -match '(?m)Defect\(s\):\s*(\d+)') { [int]$Matches[1] } else { -1 }
    $passed = if ($text -match '(?m)Pass\(es\):\s*(\d+)') { [int]$Matches[1] } else { -1 }
    $na = if ($text -match '(?m)Not Applicable:\s*(\d+)') { [int]$Matches[1] } else { -1 }
    $timeouts = if ($text -match '(?m)Timeout\(s\):\s*(\d+)') { [int]$Matches[1] } else { -1 }
    # Spaceouts (SDV ran out of memory on a rule) are unverified rules like timeouts.
    if ($text -match '(?m)Spaceout\(s\):\s*(\d+)' -and $timeouts -ge 0) { $timeouts += [int]$Matches[1] }
    $why = @()
    if ($code -ne 0) { $why += "run failed (exit $code)" }
    if ($defects -ne 0) { $why += $(if ($defects -lt 0) { "no defect count in the log" } else { "$defects defect(s)" }) }
    if ($passed -le 0) { $why += $(if ($passed -lt 0) { "no pass count in the log" } else { "no rule passed (nothing analysed)" }) }
    if ($timeouts -lt 0) { $why += "no timeout count in the log" }
    elseif ($timeouts -gt 0 -and -not $AllowTimeouts) { $why += "$timeouts rule(s) timed out (not verified)" }
    $verdict = if ($why) { "FAILED: " + ($why -join "; ") } elseif ($timeouts -gt 0) { "not clean: $timeouts timeout(s) allowed by -AllowTimeouts" } else { "clean" }
    $line = "$p : $verdict; exit $code, defects $defects, passes $passed, not applicable $na, timeouts+spaceouts $timeouts, log $log" + $(if ($dvl) { ", dvl $dvl" } else { "" })
    Write-Host $line
    $summary.Add($line)
    if ($why) { $failed = $true }
    if ($timeouts -gt 0) { $notClean = $true }
}

$summary | Set-Content -Encoding utf8 (Join-Path $WorkDir "sdv-summary.txt")
if ($failed) { Write-Host "SDV: FAILED (see the logs)" -ForegroundColor Red; exit 1 }
if ($notClean) { Write-Host "SDV: no defects, but rules timed out (-AllowTimeouts): NOT clean for a release" -ForegroundColor Yellow; exit 0 }
Write-Host "SDV: clean (no defects, no timeouts)" -ForegroundColor Green
