# split-pad — UNBUILT · UNTESTED · UNSIGNED

The PulsarPad UMDF 2 HID minidriver: per-seat virtual gamepads for Pulsar Split
(spec 60 §2, §3.7, §4.5, §6.4; contracts.md §5.2). Licence GPL-3.0-only.

> This driver has never been compiled, loaded or signed. The plan (2026-11-12) is to
> write our own drivers now and load them only after signing. The service never selects
> this backend by default. `seat_pads::PulsarPadBackend` is used only when the operator
> sets `pad_backend = pulsar_pad` **and** the package is staged.

## Design

- **Stack** (all kernel parts are inbox Microsoft binaries, so Memory Integrity does not
  apply to our code):

  ```
  [xinputhid]  (Xbox kind only, upper filter)
  mshidumdf    (function driver, HID <-> UMDF)
  WUDFRd       (lower filter) -> WUDFHost.exe -> split_pad.dll (this driver)
  ```

- **Device**: a software device from `SwDeviceCreate`, created by the service (spec 60
  §3.6 method A).
  - Instance id: `SWD\PulsarSplitPad\VID_<vid>&PID_<pid>[&IG_00]&PULSARPAD&<seat32>&<slot>`.
  - Hardware ids: `PULSARPAD\VID_…&PID_…[&IG_00]`, then `PULSARPAD\<kind>`.
  - Setup class `{7E548021-9E66-49BA-890C-5431C3D03F4C}` (fresh GUID).
  - `PnpLockdown=1`; the INF HW AddReg sets `Security` (SYSTEM and Administrators only).
    The service then sets the per-seat SDDL.
- **Properties** (§3.7) are read at device add with `WdfFdoInitQueryPropertyEx`:
  channel, seat, slot, kind and protocol. If one is missing or invalid, the start fails.
  The driver never falls back to the registry. The channel name must start with
  `Global\PulsarSplit_Pad_`. An optional `PKEY_PulsarPad_Mac` (pid 7) supplies the
  serial / pairing MAC. Without it the driver uses an FNV hash of seat and slot.
- **Channel** (§4.5): the driver opens the service-created sections and events and never
  creates them. It reads input with the seqlock rule and appends output-, set-output- and
  set-feature reports to the ring, then sets the output event.
- **Reads**: `IOCTL_HID_READ_REPORT` requests wait in a manual queue. A thread-pool wait
  on the input event, with a 4 ms timeout, completes one request in each of these cases:
  - on new input;
  - every 4 ms while the motion flag is set, with counters and timestamps advanced;
  - otherwise every 100 ms as a keepalive.
- **HID requests**:
  - device and report descriptors;
  - attributes (VID, PID, version 1);
  - strings: Sony/Microsoft manufacturer, the product name of the kind, and the MAC as
    the serial;
  - feature reports from `features.rs`;
  - get input report;
  - activate / deactivate.
- **Shared source**: `layout.rs`, `descriptors.rs` and `features.rs` are included from
  `crates/seat-pads/src` via `#[path]`. The service and the driver can therefore never
  disagree on offsets or report layouts.
- No WDK sample code is used. The Windows-driver-samples repository is MS-PL, which is
  GPL-incompatible (OQ 26). The code follows the public API documentation only.

## Files

| File | Purpose |
| --- | --- |
| `Cargo.toml` | own `[workspace]`, `cdylib`, `wdk`/`wdk-sys`/`wdk-panic`/`wdk-build`, UMDF 2.15 metadata |
| `build.rs` | `wdk_build::configure_wdk_binary_build()` |
| `Makefile.toml` | cargo-make flow from windows-drivers-rs (stampinf, inf2cat) |
| `split_pad.inx` | INF template (stampinf fills `$ARCH$`, `$UMDFVERSION$`, `DriverVer`) |
| `src/lib.rs` | `DriverEntry`, device add, queues, context, cleanup |
| `src/props.rs` | §3.7 property reads |
| `src/channel.rs` | §4.5 objects: open, seqlock read, ring write |
| `src/hid.rs` | HID IOCTL dispatch, read completion, native-rate counters |

## Build (when a WDK is available; not done yet)

1. Install Visual Studio 2022 with the WDK (matching SDK), LLVM (`clang` for bindgen),
   and `cargo install cargo-make --no-default-features --features tls-native`.
2. From a "Developer PowerShell for VS", run in this directory:
   `cargo make` (debug) or `cargo make --profile production`. The flow builds the DLL,
   runs `stampinf` on `split_pad.inx` and `inf2cat` for the catalog.
3. Targets: `x86_64-pc-windows-msvc`. On ARM64 hosts, build with
   `--target aarch64-pc-windows-msvc`; the ARM64 driver signing ladder is the same.
4. Pin the `wdk*` crate versions to the release that builds, and commit `Cargo.lock`
   then (it is ignored until the first real build).

Done so far on the dev box (no WDK): the `core`-only parts were type-checked in a
scratch `no_std` crate together with `windows-sys` 0.61. Those parts are `layout.rs`,
`descriptors.rs`, `features.rs` and `src/channel.rs`. The WDF parts (`lib.rs`,
`props.rs`, `hid.rs`) were **not** type-checked, because wdk-sys needs the WDK headers.

## Signing ladder (spec 60 §11)

1. **Release:** Microsoft attestation signing through the Hardware Dev Center. It needs
   an EV certificate (paid; not available now).
2. **Development only:** test-signing on maintainer machines. Not on the test PC for now
   (plan.md).
3. **Not allowed:** a self-installed root certificate (rules.md). We never do this.

The INF must be installed once at product install and removed only by the uninstaller.

## VERIFY on the first real build

- UMDF HID transfer buffers with `UmdfMethodNeitherAction=Copy`: is the report id in the
  input buffer and the report in the output buffer, for get and set requests alike
  (`hid.rs::xfer`)?
- `WdfFdoInitQueryPropertyEx` with UMDF 2.15. Does `mshidumdf` / `WUDFRd` bind to a
  software device? Does the HID child get `DEVPKEY_Device_SessionId` (spec 60 §13 #3, #7)?
- The Xbox kind: the section names of the inbox `xinputhid.inf` for `Include` / `Needs`.
  They are commented out in the INF. The `UpperFilters` AddReg alone may not be enough.
- Our Xbox HID report descriptor may not be what `xinputhid` expects (no public layout).
  If XInput does not see the pad, use DS4/DualSense kinds for XInput-less games.
- Thread-pool callbacks can overlap. The report buffer is not locked against concurrent
  `GET_INPUT_REPORT`, which is benign but worth a spin lock.
- The WUDFHost account is LOCAL SERVICE (spec 60 §13 #11). The channel DACL grants
  exactly that account.
- wdk-sys field and enum names (`_WDF_IO_QUEUE_DISPATCH_TYPE`, `_WDF_TRI_STATE`,
  `WDF_DEVICE_PROPERTY_DATA`, `GUID` field case) can differ between wdk-sys releases.

## Review fix (2026-11-12)

- The current input report sits behind an SRW lock in the device context
  (`DeviceContext::with_report`); callbacks never create `&mut DeviceContext`.
  `GET_INPUT_REPORT` and the read completion copy a snapshot taken under the lock.
- `on_input` runs single-file: a second thread-pool invocation of the same wait returns
  at once (`in_input` flag).
- The input wait is registered in `EvtDeviceD0Entry` and unregistered (blocking) in
  `EvtDeviceD0Exit`, which then purges the read queue; cleanup unregisters as a fallback.
  `VERIFY`: the `WDF_PNPPOWER_EVENT_CALLBACKS` field names and `WdfIoQueueStart` after a
  purge on the first build.
- `Channel::read_input` accepts only `len == input_len(kind)` with report id 0x01; any
  other content counts as invalid and the neutral report is served (the section is
  writable by any LOCAL SERVICE process). Read requests whose buffer is smaller than a
  full input report are failed with `STATUS_BUFFER_TOO_SMALL`.
- The channel name the driver opens now carries a per-instance nonce
  (`..._<slot>_<8 hex>`); the prefix check is unchanged.
