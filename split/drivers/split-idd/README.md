# split-idd — UNBUILT · UNTESTED · UNSIGNED — do not install

Pulsar Split's own remote-session indirect display driver (UMDF 2 + IddCx 1.10, C++).
GPL-3.0-only. Written from Microsoft Learn's IddCx / WDF documentation; no code from
Windows-driver-samples was copied (its licence is unclear for GPL use, open question 26).

Status, plainly:

- **Never compiled.** The dev machine has no WDK. Some IddCx 1.10 identifiers are written
  from the documentation and marked `VERIFY` in the source (for example the colour-mode
  enum names in `Modes.cpp` and the colorimetry field of `IDDCX_PATH2` in `Driver.cpp`).
  Expect a first-build fix-up pass against the real `iddcx.h`.
- **Never loaded or tested** on any machine. No test-signing (rules.md), no CI build.
- **Unsigned.** It cannot load on a normal system until it is signed (below).
- **Needs a protocol provider.** A remote-session IDD only loads in a seat session when
  that session's Remote Desktop protocol stack names its hardware id
  (`PulsarSplit_IndirectDisplay`). That provider does not exist yet (spec 40 §2.3,
  spec 30 §11, maintainer decision). Installing this INF alone makes no device appear.
- The Rust side (`crates/split-display`, backend `PulsarIdd`) is complete and unit-tested
  against an in-memory simulation of this driver's IOCTL interface. It is never chosen
  automatically: `probe` reports `signed = false` (`OWN_DRIVER_SIGNED`).

## What it does (spec 40)

| Area | Implementation |
| --- | --- |
| Adapter (§2.3) | `REMOTE_SESSION_DRIVER` + `USE_SMALLEST_MODE`; with IddCx 1.10 also `CAN_PROCESS_FP16` + `ALL_TARGET_MODES_MONITOR_COMPATIBLE`. One monitor, no EDID, target modes only. Host process sharing disabled, no device group. |
| Run-time 1.10 check (§8.4) | `IDD_IS_FIELD_AVAILABLE` for the 1.10 callbacks, `IDD_IS_FUNCTION_AVAILABLE` for `IddCxSwapChainReleaseAndAcquireBuffer2`, `IddCxMonitorUpdateModes2`, `IddCxAdapterDisplayConfigUpdate2`. Without them: SDR, legacy calls. Reported in `GET_INFO`. |
| Control channel (§7) | Device interface `{B5FCB55F-1E09-4C81-A58F-0362792CFE99}`, buffered IOCTLs `0x0022E000`–`0x0022E014`, versioned buffers (`include/PulsarIddShared.h`, mirrored and byte-tested in `crates/split-display/src/ioctl.rs`). Device security `D:P(A;;GA;;;SY)(A;;GR;;;BA)` in the INF. |
| Seat binding (§3.3) | `SET_SEAT_CONFIG` (seat GUID, render LUID, HDR allowed, SDR white, luminance, scale, super-sampling). No registry, nothing persisted. |
| Render GPU (§5) | `IddCxAdapterSetRenderAdapter` with the LUID from the seat config. |
| Modes (§4.4) | `REQUEST_MODE` is pended; the driver sets one target mode at the render size (`IddCxMonitorUpdateModes2`) and asks the OS with `IddCxAdapterDisplayConfigUpdate2` (mode, scale factor 100–500, colorimetry BT.2020 RGB + ST 2084 and SDR white level when HDR). The request completes from `EvtIddCxAdapterCommitModes2`; `busy` while one is in flight; the service cancels after 5 s. |
| Frames (§6.4) | Section `Global\PulsarSplit.Display.<seat>.Header` (header v1), `…FrameReady` event, ring of 3 named NT-shared textures `…Tex<gen>.<i>`, named shared fence `…Fence<gen>`, all with `D:P(A;;GA;;;SY)(A;;GA;;;LS)`. Never writes the slot at `consumerFrame`, never blocks DWM, heartbeat every 500 ms. Section + FrameReady are created once per seat (first swap chain after `SET_SEAT_CONFIG`) and kept until `SHUTDOWN_SEAT`; a new swap chain / device only makes a new texture + fence generation. Refuses a pre-existing (squatted) section or FrameReady; a texture / fence name that already exists is skipped to the next generation. The LOCAL SERVICE grant is open question display-7. |
| Readiness (§3.3) | `GET_STATE` flags + `WAIT_EVENT` events: adapter init, monitor arrived, path committed, first frame. |
| Shutdown | `SHUTDOWN_SEAT`: stop the worker, mark the header "stopping", close objects, monitor departure, complete pended requests. |

Files: `PulsarSplitIdd.inf`, `PulsarSplitIdd.vcxproj`, `include/PulsarIddShared.h`,
`src/Driver.{h,cpp}` (entry + callback table), `src/Adapter.cpp`, `src/Modes.cpp`,
`src/Control.cpp`, `src/FrameChannel.cpp`, `src/SwapChain.cpp`.

## Build (once a WDK is available)

Needs Visual Studio 2022 with the "Desktop development with C++" workload, the Windows
SDK and the matching **WDK 10.0.26100** (or later) with its Visual Studio extension, and
the ARM64 build tools for ARM64.

```
msbuild drivers\split-idd\PulsarSplitIdd.vcxproj /p:Configuration=Release /p:Platform=x64
msbuild drivers\split-idd\PulsarSplitIdd.vcxproj /p:Configuration=Release /p:Platform=ARM64
```

Output per platform: `PulsarSplitIdd.dll`, `PulsarSplitIdd.inf` (stamped), and the
catalog from `Inf2Cat`:

```
Inf2Cat /driver:<output folder> /os:10_X64,10_VB_X64,10_NI_X64,10_GE_X64   (x64)
Inf2Cat /driver:<output folder> /os:10_ARM64,10_NI_ARM64,10_GE_ARM64       (ARM64)
```

## Signing (not done; no money for it yet, plan.md)

- Client Windows 10/11 without test mode: **Microsoft attestation signing** — EV code
  signing certificate + Partner Center hardware dashboard, submit the CAB with the
  `.dll`, `.inf`, `.cat`.
- **Windows Server** does not load attestation-signed drivers: it needs **WHQL / HLK**
  certification. Until then Server hosts stay on the RDP display (SDR) and the manager
  says why (`server_needs_whql`).
- No private root CA, never test-signing on user machines; a test-signing exception for a
  development VM needs a maintainer decision (spec 40 §2.3, open question 9 there).
- The driver is user-mode, so it works with Memory Integrity on.

## Licence

GPL-3.0-only. Uses only Microsoft's public WDK headers / libraries at build time; ships no
Microsoft binaries.

## Threading (review fixes 2026-11-12)

Lock order `m_control` -> `m_lifecycle` -> `m_lock`.

- `m_control`: `SET_SEAT_CONFIG`, `REQUEST_MODE` (`ApplyMode`), `SHUTDOWN_SEAT`,
  `CreateMonitor`; guards `m_monitor`. Held across IddCx calls; no IddCx callback takes it.
- `m_lifecycle`: `AssignSwapChain`, `UnassignSwapChain`, `SHUTDOWN_SEAT`, destructor. The
  previous worker is joined before the next starts and before the channel closes; after
  `SHUTDOWN_SEAT` swap chains are handed back until the next `SET_SEAT_CONFIG` (which also
  re-creates the departed monitor). Never held across an IddCx call (monitor departure
  runs after it is released).
- `m_lock`: state, events, pending mode. No WDF call that can run our cancel routine or
  complete a request is made under it. `REQUEST_MODE` stores the request, marks it with
  `WdfRequestMarkCancelableEx` outside the lock (an already-cancelled request is completed
  directly), and finishes it outside the lock. A completion that races the marking is
  deferred and done by the handler right after marking.
- `REQUEST_MODE` completes when size, refresh and colour mode are committed (SDR at the
  target size after an HDR request counts as `ok_sdr_fallback` only on the
  `COMMITMODES2` path), or at once when the path already equals the target.
- `WAIT_EVENT` checks the event queue and parks the request under one lock; every event
  (also `MODE_APPLIED` / `MODE_REJECTED`) is drained into parked requests after the lock
  is released. A waiter is never completed without an event.

