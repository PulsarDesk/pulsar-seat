# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the three Pulsar Seat drivers (drivers/*/*.vcxproj) for ARM64 and x64, checks
# that the build is warning-free, runs infverif on each INF, runs ApiValidator (the SDK's
# Universal DDI list) on every .sys / .dll and checks the PE machine of every .sys / .dll. Output: target/drivers/<Platform>/<Configuration>/<project>/ (package:
# inf + sys/dll [+ cat]), intermediate files in .../obj/, one log per build in .../logs/.
#
# Never signs, never test-signs, never installs or loads anything: the packages are
# unsigned and stay that way until Microsoft signs them (drivers/README.md).
#
# Needs Visual Studio Build Tools 2026 (18) + Windows SDK + WDK 10.0.28000 (the WDK
# 26100 MSBuild tasks do not load in VS 18). Works on an ARM64 or an x64 host.
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1
#   ... -Platforms ARM64 -Configurations Release -Projects seat-idd -Analyze
param(
    [string[]] $Platforms = @("ARM64", "x64"),
    [string[]] $Configurations = @("Release", "Debug"),
    [string[]] $Projects = @("seat-input-filter", "seat-idd", "seat-storage-filter"),
    [string] $Sdk = "10.0.28000.0",
    [string] $OutRoot = "",
    # Also run the WDK code analysis (/analyze with the driver rule sets).
    [switch] $Analyze,
    # WDK rule set for -Analyze (Windows Kits\10\CodeAnalysis\<name>).
    [string] $RuleSet = "DriverRecommendedRules.ruleset",
    [switch] $SkipInfVerif,
    [switch] $SkipApiValidator
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

# "-Projects a,b" through powershell -File arrives as one string: split it.
$Platforms = @($Platforms | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$Configurations = @($Configurations | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$Projects = @($Projects | ForEach-Object { $_ -split "," } | Where-Object { $_ })

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $OutRoot) { $OutRoot = Join-Path $repo "target\drivers" }
$kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
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
    $cmd = Get-Command msbuild.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw "MSBuild not found (install Visual Studio Build Tools with the WDK)"
}

# PE machine of a file: 0xAA64 ARM64, 0x8664 x64.
function Get-PeMachine([string] $Path) {
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $br = New-Object System.IO.BinaryReader($fs)
        $fs.Position = 0x3C
        $pe = $br.ReadInt32()
        $fs.Position = $pe
        if ($br.ReadUInt32() -ne 0x00004550) { throw "$Path is not a PE file" }
        return [int]$br.ReadUInt16()
    } finally { $fs.Dispose() }
}

$msbuild = Find-MSBuild
$infverif = Join-Path $kits "Tools\$Sdk\$hostArch\infverif.exe"
if (-not (Test-Path $infverif)) { $infverif = Join-Path $kits "Tools\$Sdk\x64\infverif.exe" }
$apivalidator = Join-Path $kits "bin\$Sdk\$hostArch\apivalidator.exe"
if (-not (Test-Path $apivalidator)) { $apivalidator = Join-Path $kits "bin\$Sdk\x64\apivalidator.exe" }
Write-Host "MSBuild:  $msbuild"
Write-Host "SDK/WDK:  $Sdk"

$failures = New-Object System.Collections.Generic.List[string]
$results = New-Object System.Collections.Generic.List[object]

foreach ($platform in $Platforms) {
    $wantMachine = if ($platform -eq "ARM64") { 0xAA64 } else { 0x8664 }
    foreach ($config in $Configurations) {
        foreach ($name in $Projects) {
            $proj = Join-Path $repo "drivers\$name\$name.vcxproj"
            if (-not (Test-Path $proj)) { $failures.Add("$name : project missing"); continue }
            $base = Join-Path $OutRoot "$platform\$config"
            $out = Join-Path $base "$name\"
            $obj = Join-Path $base "obj\$name\"
            $logs = Join-Path $base "logs"
            New-Item -ItemType Directory -Force $out, $obj, $logs | Out-Null
            $log = Join-Path $logs "$name.log"
            $mbArgs = @(
                $proj, "/t:Rebuild", "/m", "/nologo", "/v:m",
                "/p:Configuration=$config", "/p:Platform=$platform",
                "/p:WindowsTargetPlatformVersion=$Sdk",
                "/p:OutDir=$out", "/p:IntDir=$obj",
                "/p:SignMode=Off",
                "/flp:LogFile=$log;Verbosity=normal"
            )
            if ($Analyze) { $mbArgs += "/p:RunCodeAnalysis=true"; $mbArgs += "/p:CodeAnalysisRuleSet=$(Join-Path $kits "CodeAnalysis\$RuleSet")" }
            Write-Host "== $name $platform $config"
            # Output goes to the log file; never cut the pipe of a running msbuild.
            & $msbuild @mbArgs | Out-Null
            $code = $LASTEXITCODE
            $text = Get-Content $log -Raw
            $warn = ([regex]::Matches($text, "(?m)^.*: warning [A-Z]*\d+:.*$") | ForEach-Object { $_.Value } | Sort-Object -Unique)
            $err = ([regex]::Matches($text, "(?m)^.*: (fatal )?error [A-Z]*\d+:.*$") | ForEach-Object { $_.Value } | Sort-Object -Unique)
            foreach ($l in $err) { Write-Host "  $l" -ForegroundColor Red }
            foreach ($l in $warn) { Write-Host "  $l" -ForegroundColor Yellow }
            if ($code -ne 0) { $failures.Add("$name $platform $config : msbuild exit $code (log $log)"); continue }
            # MSBuild's own summary also counts warnings the regex might miss.
            $summary = [regex]::Match($text, "(?m)^\s*(\d+) Warning\(s\)")
            if ($summary.Success -and [int]$summary.Groups[1].Value -gt 0 -and @($warn).Count -eq 0) { $warn = @("$($summary.Groups[1].Value) warning(s), see the log") }
            if (@($warn).Count -gt 0) { $failures.Add("$name $platform $config : $(@($warn).Count) warning(s) (log $log)") }

            # Package dir: the WDK copies the stamped inf + the binary (FilesToPackage) to
            # <OutDir><ProjectName>\; inf2cat already ran on it inside msbuild.
            $pkg = Join-Path $out $name
            if (-not (Test-Path $pkg)) { $failures.Add("$name $platform $config : no package dir $pkg"); continue }
            $bins = Get-ChildItem $pkg -Recurse -File | Where-Object { $_.Extension -in ".sys", ".dll" }
            if (-not $bins) { $failures.Add("$name $platform $config : no .sys/.dll produced"); continue }
            foreach ($b in $bins) {
                $m = Get-PeMachine $b.FullName
                $ok = $m -eq $wantMachine
                $results.Add([pscustomobject]@{ Driver = $name; Platform = $platform; Config = $config; File = $b.FullName.Substring($OutRoot.Length + 1); Machine = ("0x{0:X4}" -f $m); Ok = $ok })
                if (-not $ok) { $failures.Add("$($b.FullName) : machine 0x$("{0:X4}" -f $m), want 0x$("{0:X4}" -f $wantMachine)") }

                if (-not $SkipApiValidator) {
                    # Universal driver: every imported API must be in the SDK's Universal
                    # DDI list for the target architecture.
                    $ddiDir = Join-Path $kits "build\$Sdk\universalDDIs\$($platform.ToLower())"
                    $avLog = Join-Path $logs ("$name." + $b.BaseName + ".apivalidator.log")
                    & $apivalidator "-BinaryPath:$($b.FullName)" "-SupportedApiXmlFiles:$(Join-Path $ddiDir 'UniversalDDIs.xml')" "-ModuleWhiteListXmlFiles:$(Join-Path $ddiDir 'ModuleWhitelist.xml')" *> $avLog
                    $avCode = $LASTEXITCODE
                    $avText = Get-Content $avLog -Raw
                    if ($avCode -ne 0 -or $avText -match "(?i)ApiValidation:\s*Error") {
                        Get-Content $avLog | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
                        $failures.Add("$($b.Name) $platform $config : ApiValidator exit $avCode (log $avLog)")
                    }
                }
            }

            if (-not $SkipInfVerif) {
                $infs = Get-ChildItem $pkg -Recurse -Filter *.inf
                foreach ($inf in $infs) {
                    # /w: Windows Driver rules (declarative + isolated) for every package;
                    # /u: universal rules in addition for the UMDF (IddCx) package.
                    $modes = if ($name -eq "seat-idd") { @("/w", "/u") } else { @("/w") }
                    foreach ($mode in $modes) {
                        $ivLog = Join-Path $logs ("$name.infverif" + $mode.TrimStart("/") + ".log")
                        & $infverif $mode /v $inf.FullName *> $ivLog
                        $ivCode = $LASTEXITCODE
                        if ($ivCode -ne 0) {
                            Get-Content $ivLog | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
                            $failures.Add("$($inf.Name) $platform $config : infverif $mode exit $ivCode (log $ivLog)")
                        }
                    }
                }
            }
        }
    }
}

$results | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
if ($failures.Count -gt 0) {
    Write-Host "FAILED:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
Write-Host "All driver builds clean (unsigned, not loaded)." -ForegroundColor Green
exit 0
