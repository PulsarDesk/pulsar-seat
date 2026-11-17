# SPDX-License-Identifier: GPL-3.0-only
#
# End-to-end Authenticode dry run with the TEST certificate only, into a throwaway folder
# (docs/release/signing-dry-run.md). It walks the release order of
# docs/release/checklist.md §3 / §4 on COPIES of the built MSIs and proves that every hook
# works and that every gate refuses a test signature:
#
#   1. copy dist\PulsarSeat_*.msi (+ build records) into target\sign-dryrun\<stamp>\dist
#   2. extract each MSI copy (msiexec /a, nothing registered) and sign the payload's own
#      exe / dll with sign-authenticode.ps1 (third-party files keep their signatures)
#   3. check-payload.ts --release on the signed payload: must REFUSE (test_signed)
#   4. sign the MSI copies; the signature must name the test certificate and must NOT be
#      Valid (untrusted root)
#   5. build-setup.ps1 from each signed MSI copy, sign the setup copies
#   6. write-sums.ps1 on the dry-run dist (warns: not validly signed)
#   7. sign-release.ps1 -CheckOnly: must REFUSE (Authenticode not Valid); no key is opened
#   8. the real dist folder is byte-for-byte unchanged
#
# The test certificate is the one tools\vm\testsign.ps1 -Action Cert created in
# CurrentUser\My ("CN=Pulsar Seat TEST ONLY driver signing"); it is used through the
# certificate store by thumbprint only. Its exported key file is never opened. This script
# creates no certificate, installs nothing and changes no system setting. No timestamp
# (offline).
#
#   powershell -ExecutionPolicy Bypass -File installer\scripts\sign-dryrun.ps1
#   ... -Dist C:\path\to\dist -SkipSetup -Keep
param(
    # Folder with the unsigned PulsarSeat_<v>_<arch>.msi files (read only).
    [string] $Dist = "",
    # Skip step 5 (saves the two setup builds).
    [switch] $SkipSetup,
    # Keep the throwaway folder (default: deleted at the end, the report is printed).
    [switch] $Keep,
    [string] $CargoTargetDir = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 3

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $Dist) { $Dist = Join-Path $repo "target\installer\dist" }
$Dist = (Resolve-Path $Dist).Path
$subject = "CN=Pulsar Seat TEST ONLY driver signing"
$cert = @(Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) }) | Select-Object -First 1
if (-not $cert) { throw "no test certificate '$subject' in CurrentUser\My (tools\vm\testsign.ps1 -Action Cert creates it; this script never does)" }
$thumb = $cert.Thumbprint

$root = Join-Path $repo ("target\sign-dryrun\" + (Get-Date -Format "yyyyMMdd-HHmmss"))
if (Test-Path $root) { throw "$root exists" }
$work = Join-Path $root "dist"
New-Item -ItemType Directory -Force $work | Out-Null
$report = New-Object System.Collections.Generic.List[string]
$failed = $false
function Step([string] $name, [bool] $ok, [string] $detail) {
    $line = "{0}  {1}{2}" -f $(if ($ok) { "PASS" } else { "FAIL" }), $name, $(if ($detail) { " - $detail" } else { "" })
    $script:report.Add($line)
    Write-Host $line -ForegroundColor $(if ($ok) { "Green" } else { "Red" })
    if (-not $ok) { $script:failed = $true }
}
function Invoke-Ps([string] $script, [string[]] $argv) {
    # Child stderr (cargo progress, warnings) is output here, not an error of this script.
    $ErrorActionPreference = "Continue"
    $o = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot $script) @argv 2>&1 | ForEach-Object { "$_" }
    return [pscustomobject]@{ Code = $LASTEXITCODE; Out = ($o -join "`n") }
}
$sign = "sign-authenticode.ps1"

# 0: fingerprint of the real dist
$distBefore = @{}
Get-ChildItem $Dist -File | ForEach-Object { $distBefore[$_.Name] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash }
$msis = @(Get-ChildItem $Dist -Filter "PulsarSeat_*_*.msi" | Where-Object { $_.Name -match '^PulsarSeat_[0-9.]+_(arm64|x64)\.msi$' })
if (-not $msis) { throw "no PulsarSeat_<v>_<arch>.msi in $Dist (build-installer.ps1 builds them)" }
$report.Add("dry run $(Get-Date -Format s), test certificate $thumb ($subject), dist $Dist, folder $root")

# 1: copies
foreach ($m in $msis) {
    Copy-Item $m.FullName $work
    if (Test-Path "$($m.FullName).build.txt") { Copy-Item "$($m.FullName).build.txt" $work }
}
Step "copy $($msis.Count) MSI(s) into the dry-run folder" $true ""

foreach ($m in $msis) {
    $arch = if ($m.Name -match '_arm64\.msi$') { "arm64" } else { "x64" }
    $copy = Join-Path $work $m.Name
    # 2: extract + sign the payload
    $payload = Join-Path $root "payload-$arch"
    $p = Start-Process msiexec.exe -Wait -PassThru -NoNewWindow -ArgumentList "/a", "`"$copy`"", "/qn", "TARGETDIR=`"$payload`""
    Step "$arch extract (msiexec /a)" ($p.ExitCode -eq 0) "exit $($p.ExitCode)"
    if ($p.ExitCode -ne 0) { continue }
    $r = Invoke-Ps $sign @("-Path", $payload, "-Thumbprint", $thumb, "-NoTimestamp")
    Step "$arch sign payload exe / dll (sign-authenticode.ps1)" ($r.Code -eq 0) (($r.Out -split "`n") | Select-Object -Last 1)
    $cp = @(Get-ChildItem $payload -Recurse -Filter "pulsar-seat-credprov.dll" -File)
    $cpOk = $cp.Count -eq 1 -and (Get-AuthenticodeSignature $cp[0].FullName).SignerCertificate.Thumbprint -eq $thumb
    Step "$arch credential provider DLL carries the test signature" $cpOk ""
    # 3: the release payload gate refuses test signatures
    $ErrorActionPreference = "Continue"
    $o = & bun (Join-Path $repo "installer\scripts\check-payload.ts") --arch $arch --release $payload 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = "Stop"
    Step "$arch check-payload --release refuses the test-signed payload" ($code -ne 0 -and ($o -join "`n") -match "test_signed") "exit $code"
    # 4: sign the MSI copy
    $r = Invoke-Ps $sign @("-Path", $copy, "-Thumbprint", $thumb, "-NoTimestamp")
    $s = Get-AuthenticodeSignature $copy
    Step "$arch sign MSI copy" ($r.Code -eq 0 -and $s.SignerCertificate.Thumbprint -eq $thumb) "status $($s.Status)"
    Step "$arch test-signed MSI is not trusted" ($s.Status -ne "Valid") "status $($s.Status)"
    # 5: setup from the signed MSI copy
    if (-not $SkipSetup) {
        $argv = @("-Arch", $arch, "-Msi", $copy, "-OutDir", $work)
        if ($CargoTargetDir) { $argv += @("-CargoTargetDir", $CargoTargetDir) }
        $r = Invoke-Ps "build-setup.ps1" $argv
        Step "$arch build setup.exe from the signed MSI copy" ($r.Code -eq 0) (($r.Out -split "`n") | Where-Object { $_ -match '^setup:' } | Select-Object -First 1)
        $setup = Join-Path $work ([IO.Path]::GetFileNameWithoutExtension($m.Name) + "_setup.exe")
        if (Test-Path $setup) {
            $r = Invoke-Ps $sign @("-Path", $setup, "-Thumbprint", $thumb, "-NoTimestamp")
            Step "$arch sign setup.exe copy" ($r.Code -eq 0 -and (Get-AuthenticodeSignature $setup).SignerCertificate.Thumbprint -eq $thumb) ""
        }
    }
}

# 6: checksums of the signed copies
$r = Invoke-Ps "write-sums.ps1" @($work)
Step "write-sums.ps1 on the dry-run dist" ($r.Code -eq 0 -and $r.Out -match "not validly signed") "warns about the untrusted signatures"
# 7: the release gate refuses (no key is opened with -CheckOnly)
$r = Invoke-Ps "sign-release.ps1" @("-Dist", $work, "-CheckOnly")
Step "sign-release.ps1 -CheckOnly refuses test signatures" ($r.Code -ne 0 -and $r.Out -match "Authenticode") (($r.Out -split "`n") | Where-Object { $_ -match "Authenticode" } | Select-Object -First 1)

# 8: the real dist is untouched
$same = $true
$after = @{}
Get-ChildItem $Dist -File | ForEach-Object { $after[$_.Name] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash }
if ($after.Count -ne $distBefore.Count) { $same = $false }
foreach ($k in $distBefore.Keys) { if ($after[$k] -ne $distBefore[$k]) { $same = $false } }
Step "real dist unchanged ($($distBefore.Count) files)" $same ""
# The test signature never reached the real dist.
$leak = @(Get-ChildItem $Dist -File | Where-Object { $exts = ".exe", ".msi"; $exts -contains $_.Extension } | Where-Object { $sc = (Get-AuthenticodeSignature $_.FullName).SignerCertificate; $sc -and $sc.Thumbprint -eq $thumb })
Step "no test signature in the real dist" ($leak.Count -eq 0) ""

$reportFile = Join-Path $root "DRYRUN-REPORT.txt"
[IO.File]::WriteAllText($reportFile, (($report -join "`n") + "`n"))
Write-Host ""
$report | ForEach-Object { Write-Host $_ }
if ($Keep) { Write-Host "kept: $root" } else { Remove-Item -Recurse -Force $root; Write-Host "removed $root" }
if ($failed) { exit 1 }
Write-Host "Dry run passed: every hook ran, every gate refused the test signature." -ForegroundColor Green
exit 0
