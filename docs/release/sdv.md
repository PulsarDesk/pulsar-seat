# Static Driver Verifier (SDV): status

Checklist item: [checklist.md](checklist.md) §7.1. Script: `installer/scripts/run-sdv.ps1`.
CI: `.github/workflows/ci.yml` job `sdv` (manual `workflow_dispatch` with `sdv: true`, and
tags).

## Why an old EWDK

SDV is no longer part of the WDK: Microsoft Learn ("Static Driver Verifier", read
2026-11-13) says it is not in WDKs newer than build 26017 and points to the **Windows 11
22H2 EWDK with VS Build Tools 17.1.5 (released 2023-10-24)** as the only remaining way to
run it. `msbuild /t:sdv` with WDK 10.0.28000 answers that SDV "is no longer included" (tried
2026-11-13). CodeQL is Microsoft's replacement for new work (not set up here yet).

The ISO used: `EWDK_ni_release_svc_prod1_22621_230929-1800.iso` (build
`ni_release_svc_prod1.22621.2428`), 17 048 598 528 bytes, SHA-256
`07311C772DDF22B2D6074DBAB9C9B38EAAAE3BD59C4833CE6255931AFC5F0629`, from
`https://go.microsoft.com/fwlink/?linkid=2249942` ("Unsupported WDK versions" page, row
Windows 11 23H2 / 22621.2428; the EWDK is not affected by CVE-2024-29187 according to that
page). It ships SDV at `Program Files\Windows Kits\10\Tools\sdv\bin\staticdv.exe` (x64
only) with VS 2022 Build Tools 14.31 (x64 host tools only). No `hvciscan` tool is in it.

## What `run-sdv.ps1` does

1. Copies `drivers/<project>/` into `target\sdv\<project>\` (SDV writes `sdv\`,
   `sdv-map.h`, logs next to the project; nothing lands in `drivers/`).
2. Retargets **the copy** to the EWDK's kit: `WindowsTargetPlatformVersion` 10.0.22621.0
   (the repo pins 10.0.28000, which the EWDK lacks), Spectre libraries off,
   `ApiValidator_Enable` false, code analysis off. The driver projects set Spectre as
   `Driver_SpectreMitigation` (the generic `SpectreMitigation` is matched too) and the
   script throws if the copy still has it on. Command-line `/p:` properties are not
   enough: SDV's own build stages re-run msbuild with only Configuration / Platform.
3. In one `cmd` session: `BuildEnv\SetupBuildEnv.cmd amd64`, `msbuild /t:clean`,
   `msbuild /t:sdv /p:Inputs="/check:default.sdv"`, `msbuild /t:sdv /p:Inputs="/dvl"`.
4. Parses the defect / pass / not-applicable / timeout (+ spaceout) counts. Exit 0 only
   when every project ran **and is clean**: 0 defects, at least one rule passed (all rules
   "not applicable" means nothing was analysed), 0 timeouts / spaceouts (a rule SDV never
   finished is not verified), and every count found in the log. `-AllowTimeouts` reports
   timeouts as "not clean" without failing, for a local look only; the CI job and the
   release gate never pass it. `target\sdv\sdv-summary.txt` has one line per project with
   the verdict and the reasons.
   HYPOTHESIS: the summary lines are `Pass(es): N`, `Defect(s): N`, `Not Applicable: N`,
   `Timeout(s): N`, `Spaceout(s): N` (SDV never completed a run here); if the first real
   run prints them differently the script fails with "no ... count in the log", and the
   patterns must be adjusted from that log.

Rule sets: `default.sdv` = the default rules of the driver model SDV detects (KMDF for
`seat-input-filter`, WDM for `seat-storage-filter`; Filter Manager minifilters are
analysed with the WDM rules). `-Check all` runs every rule.

## Result on the ARM64 dev laptop (2026-11-13): SDV does not run here

- The EWDK mounts read-only (`Mount-DiskImage -Access ReadOnly`, no elevation) and its x64
  tools run under emulation. Both drivers **build** with the EWDK (10.0.22621, cl 14.31,
  x64 Release, 0 errors) once ApiValidator is off: its `aitstatic` helper exits with code
  216 under emulation (`ApiValidation : error : ... aitstatic is returned exit code 216`).
- `staticdv /check:default.sdv` stops for **both** projects, a few seconds in, with
  `[FATAL ERROR] Unrecoverable error in NormalBuild stage.` (`/debug`: "Executing:
  NormalBuild", "cumulative exit code is 1"). The commands of that stage
  (`smv\analysisplugins\sdv\configurations\MSBuild-NormalBuild.xml`: write `runsdv.bat`,
  `msbuild /t:rebuild <project> /p:configuration=... /p:platform=...`) succeed when run by
  hand in the same environment (rebuild: 0 errors), and SDV leaves no stage log behind.
  The x64 VC++ runtime is present (`HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64`
  14.51), so the documented SDV prerequisite is not the cause.
- HYPOTHESIS: SDV's SMV scheduler (x64 .NET + native interceptor, `Setting platform to
  x86_amd64`) does not work under x64 emulation on Windows 11 ARM64; the EWDK 22621
  predates ARM64-host support of the kits (WDK 26100 is the first that runs on ARM64
  hosts, and that one has no SDV). Not investigated further: there is no supported SDV
  on any ARM64 host.

So the check has to run on an **x64 machine**: the CI job `sdv` (windows-2025 runner,
downloads the same ISO, checks the pinned SHA-256, mounts it read-only, runs
`run-sdv.ps1`, unmounts, uploads `target/sdv` logs and `SDV.DVL.xml`), or locally on an
x64 PC with the ISO mounted:

```powershell
$img = Mount-DiskImage -ImagePath <path>\EWDK_ni_release_svc_prod1_22621_230929-1800.iso -Access ReadOnly -PassThru
$root = "$(($img | Get-Volume).DriveLetter):\"
powershell -ExecutionPolicy Bypass -File installer\scripts\run-sdv.ps1 -EwdkRoot $root
Dismount-DiskImage -ImagePath <path>\EWDK_ni_release_svc_prod1_22621_230929-1800.iso
```

The CI job never ran (no remote). HYPOTHESIS: the windows-2025 image has ≥ 20 GB free for
the ISO (the job checks and fails early otherwise) and the run fits in 4 hours.

The ISO stays on the dev laptop outside the repository, unmounted (path in `docs/local/`),
for a later run; delete it to free 17 GB.

## Expected follow-ups once SDV runs

- Fix every defect, or document a false positive with the rule name in the driver README.
- The 22621 DDI model is older than the 28000 headers the release build uses: a DDI that
  only exists in 28000 would break the SDV build of the copy (none is used today: both
  drivers built cleanly against 22621).
- `seat-idd` is UMDF (IddCx): SDV does not apply; Driver Verifier / WDF Verifier on the
  test VM cover it (checklist §7.2).
