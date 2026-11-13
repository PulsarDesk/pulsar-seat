# drivers/seat-idd: Pulsar Seat IddCx virtual display

UMDF 2 + IddCx 1.4 driver that gives a seat a virtual monitor of its own (spec 40 §6.3,
backend `pulsar_idd`, `DriverId::Idd`). Used only by session backends without a session
display of their own (spec 00 §5.2: backend D / future local sessions). RDP-loopback seats
(the default) render into their RDP display and do **not** use it (spec 00 T1).

**Status: built (WDK 10.0.28000, ARM64 + x64, zero warnings, infverif /w /u and WDK code
analysis clean), not signed, never installed** (plan.md: no money for signing, no
test-signing). The service reports it as
`driver_not_signed` (`seat_display::win::idd::WinIdd::availability`). Untested on hardware.

Written from the public IddCx / UMDF / D3D11 documentation (Microsoft Learn). The MS-PL
IddCx sample was **not** copied (MS-PL is not GPL-compatible, spec 00 §3.9 item 5).

## Files

| File | What |
| --- | --- |
| `public/seat_idd_ioctl.h` | control IOCTLs + frame-section layout; mirror of `crates/seat-display/src/idd_proto.rs` (layout pinned by Rust tests and `_Static_assert`s; checked with clang against the SDK headers for ARM64 and x64) |
| `src/driver.h`, `src/driver.cpp` | DriverEntry, device add, adapter init (`IddCxAdapterInitAsync`), monitor create / arrival / departure, mode callbacks, `EvtIddCxDeviceIoControl` |
| `src/swapchain.cpp` | frame section + event (named, ACL), swap-chain thread (D3D11 on the render LUID, `IddCxSwapChainReleaseAndAcquireBuffer`, staging copy, sequence lock), hardware cursor (`IddCxMonitorSetupHardwareCursor` / `QueryHardwareCursor`) |
| `seat-idd.inf` | root-enumerated `Root\PulsarSeatIdd`, Display class, `IndirectKmd` upper filter, `UmdfExtensions = IddCx0102`, device Security `D:P(A;;GA;;;SY)`, NTamd64 + NTarm64 |
| `seat-idd.vcxproj` | ARM64 + x64, `WindowsUserModeDriver10.0`, UMDF 2.31, C++17 (iddcx.h is C++ only; the code is C style, COM via C++ method calls), `/INTEGRITYCHECK`, CFG, CET (x64) |

## Behaviour

- Control interface `IDD_INTERFACE_GUID` (`seat-core::consts`), SYSTEM only (devnode
  `Security`). First call: `IOCTL_PSIDD_GET_VERSION` → `{1, 1, build}`; the client fails
  closed on another major.
- `IOCTL_PSIDD_ADD_MONITOR {seat, connector, EDID, modes, flags, render LUID, reader SID}`:
  slot = (seat − 1) × 4 + connector (connector index of the IddCx monitor). Modes are
  validated (1..16384 px per side, ≤ 1 000 000 mHz), the EDID must be whole 128-byte
  blocks. The driver patches the EDID serial number (bytes 12..15 = slot + `SP`, checksum
  recomputed) so identical monitor models get distinct descriptions; ParseMonitorDescription
  matches modes by these unique EDID bytes and gives an empty / ambiguous description the
  default mode only, never another seat's modes. `flags` bit 0
  (`PSIDD_ADD_FLAG_SOFTWARE_CURSOR`, minor 1) skips the hardware cursor (per-seat cursor
  option). The render adapter is set by `IddCxAdapterSetRenderAdapter` with the
  physical monitor's GPU. This is **adapter-wide** and the driver has one adapter, so
  with seats on several GPUs the last seat added wins for all of them (see Known gaps).
  Creates `Global\PulsarSeat.Idd.S<NN>.C<n>` (section) and
  `….Ready.<32 hex digits>` (auto-reset event, minor 2) with
  `D:P(A;;GA;;;SY)(A;;GA;;;BA)` plus read / synchronize for the reader SID (characters
  checked so a request cannot inject SDDL). The event name ends in a 128-bit nonce from
  `BCryptGenRandom`, published only in the section header (`ReadyNonce`): a Global\ event
  needs no privilege, so a fixed name could be pre-created by any user and block a
  seat's monitor for good; the section itself needs SeCreateGlobalPrivilege. If the
  section name already exists (`ERROR_ALREADY_EXISTS`: a seat host still holds the
  previous instance), the add fails with `STATUS_OBJECT_NAME_COLLISION` and never uses
  the foreign object; the service closes the readers and retries. An event name that
  exists is never used either: a new nonce is drawn (4 tries). `FrameCapacity` comes
  from `VirtualQuery` of the mapped view, not from the requested size.
- `IOCTL_PSIDD_REMOVE_MONITOR {seat, connector}`: the slot is claimed under the lock
  (`Removing`, object cleared) before `IddCxMonitorDeparture`, so concurrent removers get
  `STATUS_NOT_FOUND` and an add of the slot gets `STATUS_OBJECT_NAME_COLLISION` until the
  thread is stopped and the section closed.
- Swap chains: each slot has its own `SwapLock` (device context, outside the zeroed
  monitor slot). Assign, Unassign and RemoveMonitor start / stop the thread and tear the
  section down only under it (lock order: device `Lock`, then `SwapLock`; the IddCx
  callbacks take only the `SwapLock`). The thread uses `SwapMonitor` (the monitor its
  swap chain was assigned to), never `Object`, which RemoveMonitor clears early. A late
  assign after teardown fails with `STATUS_DEVICE_REMOVED`.
- ParseMonitorDescription: on the adding thread (arrival) it uses the pending slot; on
  any other thread it scans the slots under the shared lock and copies the mode list out.
  The lock is only tried; while an add holds it, a description equal to the pending
  slot's EDID is answered from that slot, otherwise it retries for up to 2 s and then
  answers with the default mode (never blocks for ever).
- `IOCTL_PSIDD_SET_RENDER_ADAPTER` / the LUID of an add need the adapter ready
  (`STATUS_DEVICE_NOT_READY` before `EvtIddCxAdapterInitFinished`). That callback may run
  before `IddCxAdapterInitAsync` returns, so it uses the single device context
  (`g_Device`, cleared in the device cleanup callback) when the adapter context is not set
  yet.
- Frames: CPU copy (staging texture → section) under the frame sequence lock (`Seq`); the seat host reads
  them with `seat_display::win::idd::IddFrameReader` and `pump()`s them into a presenter.
  Shared GPU textures are a later step (spec 40 open question 2).
- Cursor: hardware cursor shape + position into the section under its own sequence lock
  (`CursorSeq`, bumped on every cursor update; `CursorShapeId` bumped on shape changes),
  so pointer moves reach the presenter without a new frame. `ColorXorCursorSupport =
  IDDCX_XOR_CURSOR_SUPPORT_NONE`: the presenter blends with alpha only, so the OS draws
  XOR / inverting cursors (text beam) into the frames itself. If hardware cursor setup is
  refused, or the seat selects the software cursor, DWM draws the pointer into the frames.
- Rotation: the header's `Rotation` is always 0; IddCx hands the swap chain in scan-out
  orientation (HYPOTHESIS, to verify on hardware).

## Known gaps (to resolve with a signed build)

- Whether a local non-console session can own an IddCx monitor at all (spec 40 q1); the
  monitor is plugged for the whole PC today, so every session's DWM may see it.
- **One adapter, one render GPU.** Spec 40 §6.3.4 / §6.4 wants one IddCx adapter per
  render GPU. The driver has one root devnode and one adapter, and
  `IddCxAdapterSetRenderAdapter` is adapter-wide: with seats on two GPUs every seat
  composes on the GPU of the last seat added and the other seat's frames cross GPUs.
  Multi-GPU seats are unsupported for now; seat-display logs a warning when a binding
  spans several GPUs. Fix: one `Root\PulsarSeatIdd` devnode per render LUID (the service
  creates them) and drop the single `g_Device` assumption.
- Who owns the swap chain after a *failed* assign is not stated in the docs: the driver
  does not delete it on those paths (a leak beats a double delete). Untested on hardware.

## Build

```
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1 -Projects seat-idd
# package: target\drivers\<Platform>\<Configuration>\seat-idd\seat-idd\ (seat-idd.dll + seat-idd.inf)
```

Toolchain: [../README.md](../README.md) (Build). The script checks the PE machine of
`seat-idd.dll` (0xAA64 / 0x8664) and runs `infverif /w` and `/u`. The INF is UTF-16 LE and
takes the WUDFRd reflector service from the in-box `WUDFRD.inf` (`Include` / `Needs`), as
an isolated package must. `RemoveMonitor` gives the slot back unchanged when
`IddCxMonitorDeparture` fails; a failed `IddCxSwapChainFinishedProcessingFrame` ends the
swap-chain thread (the OS assigns a new swap chain). Then attestation signing (Partner
Center + EV certificate). Never test-sign on the dev box or the test PC.
## Install (installer, after signing)

1. `pnputil /add-driver seat-idd.inf /install`
2. Create the root devnode `Root\PulsarSeatIdd` (SetupAPI `SetupDiCreateDeviceInfoW` +
   `DIF_REGISTERDEVICE` + `UpdateDriverForPlugAndPlayDevicesW`, or `SwDeviceCreate` from
   the service with a lifetime tied to Pulsar Seat being enabled — to decide).
3. Uninstall: remove the devnode, `pnputil /delete-driver oem<N>.inf /uninstall`.

## Alternative: an existing signed open-source IddCx driver

Candidate: **Virtual Display Driver** (github.com/VirtualDrivers/Virtual-Display-Driver),
IddCx based, x64 + ARM64, signed through the SignPath Foundation. To verify before any
use (2026-11-12 web search, not yet checked against the repo itself):

- Licence: the SignPath project page lists **MIT** (GPL-compatible); confirm the licence
  file in the repo and keep its notice.
- Signing: confirm the released package carries a Microsoft-accepted signature (attestation)
  and not only an Authenticode one. The project README warns that **ARM64 on Windows 11
  24H2+ may need test-signing** — that would break our rules (Memory Integrity / Secure
  Boot on, no test-signing), so on ARM64 it is likely unusable until fixed upstream.
- Control: it is driven by its own settings file / pipe, not by our IOCTLs, so it would
  need its own small `IddControl` implementation and gives no per-seat frame section; the
  presenter would have to acquire its monitors through Display Core instead.
- Other names seen (Parsec VDD, usbmmidd): closed source, not options.
