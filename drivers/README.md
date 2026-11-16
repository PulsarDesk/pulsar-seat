# drivers/

Our own drivers. They sit behind the same backend traits as the user-mode defaults
(`seat-core::backend`), and the service reports them as `driver_not_signed` until a
Microsoft-signed build exists. **None of them is loaded or tested until signed** (plan.md:
no money for signing now; no test-signing on the dev laptop or the test PC).

**State (2026-11-16): built unsigned; loaded test-signed in the Hyper-V test VM only**
(plan.md "Driver round in the test VM", results in docs/agents/status.md: input filter and
minifilter load and work, IddCx creates monitors after three fixes, Verifier clean; all
four load under HVCI / Memory Integrity with no CodeIntegrity complaint, 2026-11-16).
Before that: **(2026-11-14) built, unsigned, never loaded.** All four drivers (the IddCx one with two roles, plus the Rust pad driver) build with zero
warnings for ARM64 and x64, Release and Debug, with Visual Studio 2026 Build Tools (18) and
WDK / SDK 10.0.28000; infverif and the WDK code analysis pass (see Build below). Nothing is
signed, test-signed, installed or loaded. Every driver is written against the public WDK
documentation (Microsoft Learn). The WDK
sample repository is MS-PL, which is not GPL-compatible: samples are read as documentation
only, never copied (spec 00 §3.9 item 5).

| Dir | Driver | Framework | Position | Trait / backend kind | Interface GUID (`seat-core::consts`) |
| --- | --- | --- | --- | --- | --- |
| `seat-input-filter/` | Keyboard / mouse isolation **and** HID collection visibility (one driver `seatinput.sys`, role per devnode class; sources written, see its README) | KMDF | device upper filter **below** kbdclass / mouclass (extension INF, `AddFilter`; never class-wide); upper filter on HID collection devnodes, decides on create by requestor session | `InputRouter` / `kmdf_filter`; `DriverId::KbdMouFilter` + `HidFilter` | service `INPUT_FILTER_SERVICE` (`PulsarSeatInput`); named control device `\\.\PulsarSeatInput` (a non-PnP control device cannot register an interface) |
| `seat-idd/` | Virtual monitor per seat (sources written, see its README) | IddCx (UMDF 2) | root device via INF with a real hardware id | `DisplayBinding` / `pulsar_idd`; `DriverId::Idd` | `IDD_INTERFACE_GUID` |
| `seat-storage-filter/` | Strict storage isolation | FltMgr minifilter | allocated altitude (to request; placeholder `UNALLOCATED` today) | `DeviceGate` / `storage_minifilter`; `DriverId::StorageMinifilter` | service `STORAGE_FILTER_SERVICE` (`PulsarSeatStorageGate`); port `MINIFILTER_PORT_NAME` |

Remote seat kind (from Pulsar Split, merge-plan.md). Built by `build-drivers.ps1` since
round 4 (2026-11-14), unsigned, never loaded:

| Dir | Driver | Framework | Notes |
| --- | --- | --- | --- |
| `seat-idd/` (remote role) | Per-session virtual display, HDR via IddCx (`seat_display::remote` backend `pulsar_idd`) | IddCx (UMDF 2, C++) | was `split-idd/`; [seat-idd/README.md](seat-idd/README.md) "Remote role"; behaviour `spec/remote/40-display-hdr.md` |
| `split-pad/` | Per-seat virtual gamepad (`seat-pads` backend PulsarPad) | UMDF 2 HID minidriver (Rust, windows-drivers-rs, own Cargo workspace) | [split-pad/README.md](split-pad/README.md); shares `crates/seat-pads/src/{layout,descriptors,features}.rs` via `#[path]`; built with cargo into the shared target dir, then stampinf + inf2cat |

## One own IddCx driver, two roles (round 4, 2026-11-14)

Decision (merge-plan.md section 5, item 5): one own IddCx driver for both kinds, so it is
signed once. Done: `drivers/split-idd` is merged into `drivers/seat-idd` (one INF
`seat-idd.inf`, one binary `seat-idd.dll`):

| | local role | remote role |
| --- | --- | --- |
| Hardware id | `Root\PulsarSeatIdd` (root devnode made by the installer) + `PulsarSeatIdd_Adapter` (one software devnode per further render GPU, made by the service; interface minor 3) | `PulsarSplit_IndirectDisplay` (made by a Remote Desktop protocol stack; Windows 11 model sections only) |
| Sources | `src/driver.cpp`, `src/swapchain.cpp`, `public/seat_idd_ioctl.h` | `src/remote/`, `public/PulsarIddShared.h` |
| Monitors | virtual monitor per local seat | per-session display of a remote seat, HDR (FP16) |
| Control | `IDD_INTERFACE_GUID`, `seat_display::idd_proto` | `{B5FCB55F-1E09-4C81-A58F-0362792CFE99}`, `seat_display::remote::ioctl` |
| Rust backend | `DisplayBinding` kind `pulsar_idd` | `remote::DisplayBackend` kind `pulsar_idd` |
| Frame reader | `ADD_MONITOR` reader SID (`SeatDisplayPrefs.frame_reader_sid`) | `SET_SEAT_CONFIG` v2 reader SID (`SeatDisplay::frame_reader_sid`, `None` = SYSTEM) |

`src/roles.cpp` holds DriverEntry; the device add reads the devnode's hardware ids and
calls the role's device add (an unknown id is refused). Each devnode runs in its own
WUDFHost (`ProcessSharingDisabled`). The two control interfaces stay separate on purpose:
both clients and their byte-tested layouts stay unchanged, and each role reports its own
version. A later step may still fold them into one interface with capabilities. Signing
(attestation / WHQL) is then done once; `signed = true` lets `remote::choose_backend` pick
it, and `driver_not_signed` goes away for local seats. HYPOTHESIS (nothing loaded): both
roles load side by side from one package.
`seat-storage-filter` is outside the rules.md driver list: an exception that needs the
maintainer's explicit OK before it is ever installed (spec 70 §5.3, spec 90 §11 item 2).

## Common rules (spec 00 §5.5.2, §5.6, §5.8; spec 90 §11)

- C, written fresh. Each driver = `<name>.vcxproj` + `<name>.inf` + sources; one
  multi-architecture package (ARM64 + x64).
- First IOCTL of every control interface: version query → `{interface_major,
  interface_minor, build}`. The service refuses a driver whose major differs from
  `DRIVER_INTERFACE_MAJOR` and leaves the filter in fail-open state. Changes within a major
  are additive; a new major gets a new interface GUID.
- Control interface ACL: SYSTEM only (only the service opens it).
- Filters **fail open before the service's first connect since boot**; after a connect they
  drop other seats' input (bounded gap buffer) rather than leak it to seat 1 (spec 50
  §4.1.5). Safe Mode or the recovery value makes them pass everything.
- Seat of a devnode at device start: private DEVPROPKEY `DEVPROP_SEAT_GUID` pid 2 (seat mask,
  UINT32), written by the service.
- HVCI-clean by construction: no RWX sections, NX pool only (`POOL_FLAG_NON_PAGED`),
  `/INTEGRITYCHECK`, Spectre mitigation (`/Qspectre`) on all three. Must load with Memory
  Integrity and Secure Boot on. **What runs today** (`installer/scripts/build-drivers.ps1`,
  dev box, not CI: the CI job has never run): zero-warning builds (`/W4 /WX`), WDK code
  analysis (`-Analyze`, DriverRecommendedRules), infverif (`/w`, `/u` for seat-idd) and
  ApiValidator against the SDK's Universal DDI list (all binaries Universal, 2026-11-13).
  **Not run yet:** Static Driver Verifier, Driver Verifier and the HVCI readiness test.
  SDV: `msbuild <proj> /t:sdv` on WDK 10.0.28000 only answers "Static Driver Verifier (SDV)
  is no longer included in the Windows Driver Kit and is no longer compatible with VS2022
  and later. To use SDV, install an older version of the EWDK" (tried 2026-11-13). With the
  Windows 11 22H2 EWDK (10.0.22621.2428, mounted read-only, nothing installed) both kernel
  drivers build, but `staticdv` aborts in its first stage ("Unrecoverable error in
  NormalBuild stage") on the ARM64 dev laptop (x64 tools under emulation; details and the
  reproduction in [docs/release/sdv.md](../docs/release/sdv.md)). SDV therefore runs on an
  x64 machine: `installer/scripts/run-sdv.ps1 -EwdkRoot <mounted ISO>` and the CI job
  `sdv` (windows-2025, never run yet). Driver Verifier and HVCI need a test VM with a
  loaded, attestation-signed build ([docs/release/checklist.md](../docs/release/checklist.md) §7.2).
- seat-input-filter interface **minor 1** (2026-11-13): `SEATINPUT_CONFIG` grew to 96 bytes
  with `EmergencyScan` / `EmergencyFlags` (`SEATINPUT_CONFIG_V0_SIZE` = 88 is still
  accepted; then the move hotkey stays the emergency key, as in minor 0). The driver checks
  Ctrl+Shift+`EmergencyScan` on seat-1 keyboards on its own, also with the move hotkey off;
  the service always sends it (the hotkey, or F12 when the hotkey is off,
  `seat_input::convert::Hotkey::emergency_key`). Untested on hardware.
- Never: class-wide filter entries, editing other software's `UpperFilters` / `LowerFilters`,
  legacy (non-PnP) services, LegacyDriver placeholder devnodes, kernel patching, registry
  views of Windows keys, process / image-load hooks.
- Installed and removed only through the driver store (`pnputil /add-driver … /install`,
  `pnputil /delete-driver oem<N>.inf /uninstall`). Offline recovery: `dism /image:…
  /remove-driver`, documented in the troubleshooting guide ([docs/troubleshooting.md](../docs/troubleshooting.md), Turkish user copy).

## How the service sees them

`get_backends` reports each driver through `driver_status` (seat-service
`win/platform.rs`, mapping in `platform.rs::driver_status_from_probe`):

| State | When |
| --- | --- |
| `not_installed` | its service key (input, storage) or device interface (IddCx) is missing |
| `installed_not_loaded` | installed, but no control device / port / interface answers (not loaded) |
| `loaded {interface_major, interface_minor, build}` | the version query answered with our `DRIVER_INTERFACE_MAJOR` |
| `version_mismatch {found}` | the version query answered with another major |

The version query is the first call every control interface answers and the only thing
`driver_status` sends:

| Driver | Query | Client (probe) |
| --- | --- | --- |
| input filter | `IOCTL_SEATINPUT_GET_VERSION` (0x222000) on `\\.\PulsarSeatInput` -> `SEATINPUT_VERSION` | `seat_input::win::probe_filter_version` |
| storage minifilter | `PSSG_CMD_QUERY_VERSION` on port `\PulsarSeatStorageGate` -> `PSSG_VERSION_REPLY` | `seat_media::probe_minifilter_version` |
| IddCx | `IOCTL_PSIDD_GET_VERSION` (0x22e400) on the `IDD_INTERFACE_GUID` interface -> `PSIDD_VERSION` | `seat_display::win::idd::probe_idd_version` |

The input control device is exclusive and the minifilter port takes one connection: while
the service's own backend holds it, the probe returns the version that backend read
instead of opening a second handle (every open in the process is serialized, so the probe
never makes the backend's open fail). The control device, port and interface exist only
while the driver is loaded, and Windows (Secure Boot, Memory Integrity on, no test-signing)
loads only a Microsoft-signed build. Untested on hardware: no build has been loaded.

## Build

Toolchain: Visual Studio 2026 Build Tools (18) with the C++ x64 + ARM64 tools and the
Spectre-mitigated libraries, Windows SDK 10.0.28000 and WDK 10.0.28000 (winget ids
`Microsoft.VisualStudio.BuildTools`, `Microsoft.WindowsSDK.10.0.28000`,
`Microsoft.WindowsWDK.10.0.28000`; install the WDK after the Build Tools). The projects pin
`WindowsTargetPlatformVersion` 10.0.28000.0: the WDK 26100 MSBuild tasks
(`Microsoft.DriverKit.Build.Tasks.17.0.dll`) do not load in VS 18; only 28000 ships the 18.0
tasks.

```
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1
# options: -Platforms ARM64,x64  -Configurations Release,Debug  -Projects seat-idd
#          -Analyze (WDK code analysis, DriverRecommendedRules)  -SkipInfVerif  -OutRoot <dir>
```

The script rebuilds every project into `target\drivers\<Platform>\<Configuration>\`
(package = `<project>\<project>\`: stamped INF + `.sys` / `.dll`; intermediate files in
`obj\`, one log per build in `logs\`), fails on any compiler, linker, stampinf or inf2cat
warning (the projects also build with `/W4 /WX`), runs `infverif /w` on each package (plus
`/u` for the UMDF IddCx package) and checks the PE machine of every binary (0xAA64 ARM64,
0x8664 x64). It never pipes msbuild output (cutting the pipe of a running msbuild leaves
`cl.exe` hanging). CI (`.github/workflows/ci.yml`, job `drivers`) installs the same
toolchain and runs the same script, then the code analysis and the payload checker.

Notes from the first build (2026-11-13):

- `iddcx.h` is C++ only (enums with a fixed underlying type), so `seat-idd` is C++17
  (`.cpp`, plain C style, COM through C++ method calls); `UMDF_VERSION_MINOR` 31 (Windows
  10 2004, the INF floor), because the IddCx 1.4 headers need the 2.25+ WDF headers.
- The WDK does not add the versioned `iddcx\1.4` stub library directory by itself; the
  project does.
- `FilesToPackage` = `$(TargetPath)` puts the binary next to the INF; without it inf2cat
  fails (22.9.1).
- `SpecifyDriverVerDirectiveDate` is off: the INF's own DriverVer date is kept, because
  stampinf's "today" is local time and inf2cat rejects it as postdated near midnight UTC.
- Packages are isolated / declarative (`infverif /w`): no `DefaultUninstall` in the
  minifilter INF (removal via `pnputil /delete-driver <oem>.inf /uninstall`), and the IddCx
  INF pulls the WUDFRd service from the in-box `WUDFRD.inf` (`Include` / `Needs`).
- Kernel APIs: `IoGetRequestorProcess` / `IoGetRequestorSessionId` come from `ntifs.h`;
  `RtlIdentifierAuthoritySid` is not exported to kernel mode (the minifilter reads the
  documented `SID` structure instead).

## Signing, then loading

Nothing is installed until a package carries a Microsoft signature. Routes, in order of
preference:

1. **Attestation signing** (Partner Center hardware dashboard): needs an EV code-signing
   certificate for the Partner Center account, a CAB with the INF + binaries + PDBs per
   architecture, signed with that certificate. Microsoft returns the package signed for
   Windows 10 / 11 client. No HLK run. Enough for the input filter and the IddCx driver on
   client Windows.
2. **WHQL (HLK)**: HLK test passes for the device class, needed for Windows Server or for
   distribution through Windows Update with a "certified" label. Not planned now.
3. The storage minifilter additionally needs an **altitude** allocated by Microsoft
   (FSFilter Security Enhancer range) and the maintainer's explicit OK before it is ever
   signed or staged (spec 70 section 5.3, spec 90 section 11 item 2). The request e-mail
   is drafted in [docs/release/minifilter-altitude-request.md](../docs/release/minifilter-altitude-request.md)
   (not sent; Microsoft allocates only for a publicly shipping product).

The full ordered release list (attestation steps, test-VM steps):
[docs/release/checklist.md](../docs/release/checklist.md).

Never: test-signing (`bcdedit /set testsigning on`), disabling Secure Boot or Memory
Integrity, a changed code-integrity policy, or cross-signing tricks; not on the dev laptop,
the test PC or a user's PC.

Only a Microsoft-signed package for the native architecture is staged by the installer
(`seat_compat::drivers::stage_all`; `check-payload` refuses `.sys` / `.inf` without
`--allow-drivers`). Before the first signed release: Driver Verifier and the HVCI
readiness checks on a test VM, the hardware test plan in each driver's README.