# shim/ — per-process seat isolation

Phase 4 isolation layer (spec `spec/remote/50-shim-isolation.md`, contract
`docs/split/agents/contracts.md` §4). Processes in seat session N should behave as if seat N
were the only interactive session; the console must not see seat-owned input.

This directory is a **separate Cargo workspace** (not a root member), Windows **x64 + x86
only**. ARM64 hosts are out of scope (spec 50 §14). All decisions live in the root crate
`crates/seat-shim-core` (pure, unit-tested); this workspace is the thin Windows glue.

| Crate | Output | What |
| --- | --- | --- |
| `module/` | `PulsarSplitShim.dll` | the injected module: detours, health frame, fault guard |
| `launcher/` | `pulsar-split-launch.exe` (x64), same source built for x86 and shipped as `pulsar-split-launch32.exe` | start a process suspended, load the module, run its init, resume |
| `shimctl/` | `pulsar-split-shimctl.exe` | operator tool: status / install / uninstall / kill switch / DXVK |

## Mechanism (decision record, contracts §4.1)

Opt-in, in this order of preference:

1. **No injection.** Pad device security (pads package) scopes our virtual pads to their
   seat user for every API (HID, Raw Input, GameInput, WGI). Steam runs per seat as the
   seat user with `-master_ipc_name_override` (`seat_shim_core::steam_launch_args`), so
   HKCU is naturally per seat. This is the primary path.
2. **Launcher injection** for processes the service or the stream server starts (Steam,
   autostart apps, app-list entries): `pulsar-split-launch [--wait] -- <exe> <args…>`.
   The target runs in the caller's session and token; nothing system-wide changes.
3. **AppHelp custom database** (opt-in) for processes the seat shell starts: our own GUID
   `{d9a4fcd5-83a4-45c2-8caa-b3a4f973ad03}`, own names, module `PulsarSplitShim.dll` in
   `System32` / `SysWOW64` (AppHelp loads custom shim DLLs only from there). Targets = the
   operator's list only; `svchost.exe`, `dwm.exe`, `lsass.exe`, `csrss.exe`, `winlogon.exe`
   and other system-critical processes are refused. No machine-wide `__COMPAT_LAYER`: the
   launcher puts our layer only into the environment of the processes it starts, so their
   children (games started by Steam) are covered too.

**Trade-off (svchost / dwm).** The reference targets `svchost.exe` and `dwm.exe` by name, so
its module lands in every service host and the compositor; a fault there restarts the
desktop or stops the OS, protected service hosts refuse non-Microsoft modules anyway, and
masking the console id inside them changes OS decisions. We never target them. Cost:
controller focus is filtered only in the seat shell (`explorer.exe`, `sihost.exe`), and the
game-input service is not shimmed; isolation there relies on pad device security.
Shell-started games without the AppHelp database get only device-security isolation.

The module installs **no hooks** in session 0, in our own binaries (under
`%ProgramFiles%\Pulsar Split\`), in safe mode, or when `HKLM\SOFTWARE\PulsarSplit\Shim`
`ShimDisabled` = 1 (kill switch; settable offline from WinRE with `reg load`). Console
processes get only the HID rule, and only while a running seat asks for HID isolation.

## What the module does (spec 50 §4–§8)

Each hook calls a pure function of `seat_shim_core::decide`:

| Group | Hooks | Decision |
| --- | --- | --- |
| HID open | `ntdll!NtCreateFile` / `NtOpenFile` on `\??\HID#…` and XUSB interface paths (fallback if those fail: kernelbase `CreateFileW/A`, `CreateFile2`) | `classify_nodes` (parent chain via CfgMgr32) + `hid_access`; deny = `STATUS_ACCESS_DENIED` / `ERROR_ACCESS_DENIED` |
| HID enumeration | `CM_Get_Device_Interface_ListW`, `SetupDiEnumDeviceInterfaces`, `GetRawInputDeviceList` | same rule; denied devices are removed from the lists |
| XInput focus | `RtlPublishWnfStateData` (seat shell only) | `filter_focus`; only on builds listed in `Shim\WnfBuilds` (none verified yet, so off) |
| Remote masking | `WTSGetActiveConsoleSessionId`, `RtlGetActiveConsoleId`, `GetSystemMetrics(SM_REMOTESESSION)` | `mask_remote`: only exes listed in `Shim\MaskApps`, seat sessions only |
| Steam | kernelbase `GetCommandLineW/A` in `steam.exe` + `steam_master_ipc_name_override` env | `steam_cmdline_patch` |
| HKCU redirect | kernelbase `RegOpenKeyExW/A`, `RegCreateKeyExW/A` | `hkcu_redirect`, only for seats flagged `shared_account` (forbidden by validation) |

Win32 hooks go on the kernelbase export first (kernel32 / advapi32 only as fallback):
kernel32's exports are stubs into kernelbase and API-set callers bind to kernelbase
directly. A target module that is not loaded yet is loaded from System32 on the launcher
path, or hooked from an `LdrRegisterDllNotification` callback when it loads (AppHelp and
DllMain-fallback paths). Every patch runs with the other threads suspended (process heap
lock held while suspending; a thread inside the patched bytes makes the patch retry). A
wanted group that is not in place is reported as `hook_failed` in the health frame.

Role and plan are re-read from the seat map at most every 3 s, so a seat whose map entry
is published after its shell started is still isolated, and the kill switch works without
a restart (hooks then pass through). The refresh runs outside every lock, and hooks
reached from inside it pass through. Device classification recognises `TERMINPUT_BUS…SESSION<N>`,
`&PULSARPAD&<seat32>&`, the DS5 serial marker `PSPAD<seat32><slot>`, ViGEm pads we own
(map `PadInstances`) and unowned ViGEm pads; never the reference product's tag.

Health: one frame per process to `\\.\pipe\PulsarSplit.ShimHealth` (u32 LE length + JSON,
≤ 4 KiB: pid, session, exe file name, version, role, hooks on, error code). Crash guard: a
vectored exception handler reports `fault` once when an exception is raised inside the
module (no lock, no allocation: prebuilt frames, `NtCreateFile` / `NtWriteFile`); the pipe
is opened with `FILE_WRITE_DATA | SYNCHRONIZE` and `SECURITY_IDENTIFICATION` QoS
(open question shim-8); the service counts faults (`FaultCounter`) and lists the target in
`Shim\DisabledTargets`. Logging: TraceLogging provider `PulsarSplit.Shim`
(`81916b1b-e779-43f0-9ba0-6ecf949e9db9`); errors also to the Application event log, source
`PulsarSplitShim` (at most 5 per process). `Shim\Verbosity` gates both (0 nothing,
1 errors = default, 3 info, 4 verbose decisions). No SIDs, user names or user paths are logged.

## Kiosk seats (round 4, local kind)

Local kiosk seats (architecture.md §4.1.3) all run in the console session, so the session
map cannot tell their processes apart. Instead (`seat_shim_core::kiosk`):

| Piece | What |
| --- | --- |
| Kiosk map | `HKLM\SOFTWARE\PulsarSplit\Runtime\KioskSeats\<NN>` (volatile, same SDDL as the seat map): `Seat`, `SessionId`, `Generation`, `Devices` (MULTI_SZ, upper-case instance-id prefixes), `Flags` (1 = HID isolation, 2 = raw-input data filter). Written by seat-service only. |
| Seat of a process | `PULSAR_SEAT_KIOSK=<seat>`, set by the kiosk shell and inherited. Role `kiosk` only when it names a published seat of the process's own session; otherwise the usual rules. Not a security boundary. |
| Loading | kiosk shell: `pulsar-split-launch --attach <pid>` on the suspended app (exit 0 hooks on, 3 no hooks, 4 blocked / not supported, 5 timed out = shell restarts the app without the module). Or AppHelp, as for remote seats. |
| HID groups | kiosk K: its own kiosk devices and unowned ViGEm pads only; console devices, other seats' devices, remote session devices and seat pads denied. Console: kiosk devices denied while a kiosk seat of its session wants HID isolation. With kiosk seats running, enumeration also filters keyboard / mouse class interfaces (`GUID_DEVINTERFACE_KEYBOARD` / `_MOUSE`). |
| Raw-input data (`rt/raw.rs`) | `GetRawInputData`: a denied device's header / input fails with `ERROR_INVALID_HANDLE`. `GetRawInputBuffer` (64-bit processes only): denied records removed in place (`filter::compact_raw_records`, layout checked first; a buffer that does not parse is passed on unchanged). Records without a device (injected input) pass. |

**No ARM64 module:** native ARM64 targets start without it (`arm64_unsupported`); this
round decided against an ARM64 inline-hooking backend. x64 apps under emulation on an
ARM64 PC would take the x64 module (HYPOTHESIS; the ARM64 MSI ships no shim today).
32-bit apps keep `GetRawInputBuffer` unfiltered (WOW64 layout). HYPOTHESIS: nothing here
ran on hardware.

## Local hosted seats (2026-11-17, pads-pen)

Local seats with a session of their own (hosted / dedicated seats) are published in the
same seat map as remote seats (`seat-service/src/pad_runtime.rs`): seat id
`5053454c-4f43-4100-8000-<seat number>`, the seat user, and as `PadInstances` the
instance ids of the HID units routed to that seat alone plus the parents of its pads (the
XUSB interface of an Xbox pad sits on the parent). So the existing rules apply unchanged:
a shimmed process of seat N opens (HID and XInput) only seat N's pads, the console no
longer opens them, and seats never open the console's (unrouted) pads. The module itself
did not change. Kiosk seats keep their own map. HYPOTHESIS: never ran on hardware.

## DXVK (opt-in per game, spec 50 §10.2)

No system DLL redirect and no DXVK binaries in this repo or the installer. `shimctl dxvk
download` (or the service) fetches the release over HTTPS (WinHTTP), checks its SHA-256
against the manifest (default: DXVK 3.1.1,
`40565b4a724aadc4433fa4e010b4b23916d9b1f1baeee64e17186db94f54e608`), extracts only
`x32`/`x64` `d3d8.dll` + `d3d9.dll` to `%ProgramData%\Pulsar Seat\shim\dxvk\<version>\`, writes
`release.json` (per-file hashes) and `LICENSE.txt`, and pins the release in HKLM
(`Shim\Dxvk`: version, archive hash, `release.json` hash). `dxvk deploy` uses only the
pinned release and checks it against the pin first, then copies the matching
bitness next to the game (D3D8 brings D3D9 along), backs up existing files as
`*.pulsarsplit-orig`, writes `pulsarsplit-dxvk.json` and journals every change; `dxvk
undo` restores the originals and leaves files the user replaced since. DXVK needs a
Vulkan-capable GPU driver.

Data folders (`%ProgramData%\Pulsar Seat\shim` and its `journal`, `sdb`, `dxvk`; moved
from `%ProgramData%\PulsarSplit` by the merge review, 2026-11-14) are created with a
protected SYSTEM + Administrators DACL. The product root `%ProgramData%\Pulsar Seat` keeps
the service's DACL: it is only checked (no junction, trusted owner) or created with the
Seat DACL when missing. An existing one that is a junction or not
owned by SYSTEM / Administrators is refused, and journal / database files are checked
(owner, no hard link) before they are trusted. Restore replays only our module copies and
DXVK DLLs next to games. All HKLM access uses the 64-bit view (`KEY_WOW64_64KEY`), so the
x86 module and launcher see the same keys.

## Build and test

```
rustup target add i686-pc-windows-msvc
cd shim
cargo check --target x86_64-pc-windows-msvc --all-targets
cargo check --target i686-pc-windows-msvc --all-targets
cargo clippy --target x86_64-pc-windows-msvc --all-targets
cargo test                                   # host (ARM64: pure filters + shimctl parsing)
cargo test --target x86_64-pc-windows-msvc   # runs under x64 emulation on ARM64
cargo build --release --target x86_64-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
```

Core logic: `cargo test -p seat-shim-core` and `cargo clippy -p seat-shim-core
--all-targets` at the repo root. Static CRT (`.cargo/config.toml`): no Microsoft runtime
DLLs are shipped. Release profile uses `panic = "abort"`.

Operator use (elevated prompt for changes):

```
pulsar-split-shimctl status
pulsar-split-shimctl install --x64 <x64\PulsarSplitShim.dll> --x86 <x86\PulsarSplitShim.dll> --target explorer.exe --target steam.exe=Steam
pulsar-split-shimctl kill-switch on
pulsar-split-shimctl uninstall          # mapped modules: renamed + deleted at next reboot
pulsar-split-shimctl restore            # replay %ProgramData%\Pulsar Seat\shim\journal\shim.jsonl
```

## Verified on the dev box (Windows 11 ARM64, no elevation, no system change)

- Both module builds link; exports `GetHookAPIs`, `NotifyShims`, `PulsarSplitInit` are
  undecorated in the x86 build too; imports are OS DLLs only.
- 2026-11-12 (after the review fixes): `pulsar-split-launch --module … --wait --
  pulsar-split-shimctl.exe status`, x64 and x86 release builds: loaded, init ran, child
  resumed and exited 0; hooks off (console session).
- `pulsar-split-launch --wait -- pulsar-split-shimctl.exe notice dxvk` (x64 and x86 builds,
  under emulation): the module is loaded into the suspended child, `PulsarSplitInit` runs,
  the child resumes and exits normally. Hooks stay off there (console session, empty map).
- `shimctl status` reads the real registry (no foreign shim, database absent).

## Not tested (needs the x64 test host, a VM or signing)

- Any hook actually firing: HID deny/filter in a seat, focus filter, masking, Steam command
  line, HKCU redirect. Needs a running seat on the x64 host. In particular the
  `NtCreateFile` / `NtOpenFile` detours, deferred hooks and thread freezing (shim-11), and
  the launcher's System32-copy path and timeout restart (shim-10).
- AppHelp: database writing through apphelp's `Sdb*` writer, `sdbinst`, matching, whether
  the engine keeps a module that returns no hooks, `NotifyShims` timing, safe mode,
  uninstall of a mapped module (open questions shim-3). The tag ids are from public
  sources, not checked against a live apphelp.
- Loading with Memory Integrity on, Smart App Control or WDAC user-mode policies, and AV
  behaviour. The module is unsigned until there is a publicly trusted code-signing
  certificate; a blocked load degrades to "no isolation" with `shim_blocked` from the
  launcher (spec 50 §12).
- Cross-bitness hand-off between the installed `x64\` and `x86\` launchers (needs the
  installer layout), WinHTTP download of the real DXVK release from the service, the
  fault handler's health frame.
- Known limitation: a console process decides at start whether it needs the HID hooks; one
  started before any seat with HID isolation ran keeps no hooks (device security still
  applies). Seat-session processes always get the seat hook set and follow the live map.
- Kiosk seats (round 4): `--attach` on a suspended process, the kiosk map read in kiosk
  apps, raw-input data / buffer filtering, keyboard / mouse class enumeration, the x64
  module in x64 apps under emulation on ARM64.
- WNF focus payload layout (`HYPOTHESIS`: target PID first); the filter is off everywhere
  until a build is verified.

## Third-party notices

- `retour` 0.4.0-alpha.4 (BSD-2-Clause, <https://github.com/Hpmason/retour-rs>), with
  `iced-x86` (MIT). Copyright (C) 2017 Elliott Linder; minhook-rs parts (C) 2015 Jascha
  Neutelings. Redistribution in binary form must reproduce the copyright notice, the list
  of conditions and the disclaimer (see the crate's LICENSE); the installer copies it into
  the install folder's licence notices.
- `tracelogging` (MIT, Microsoft), `windows-sys` (MIT/Apache-2.0), `serde`/`serde_json`,
  `thiserror`, `log`, `sha2`, `flate2`, `tar` (MIT/Apache-2.0).
- DXVK (zlib/libpng licence) is never bundled; its notice (`seat_shim_core::DXVK_NOTICE`)
  is written next to the downloaded release and shown in the manager's About screen.
