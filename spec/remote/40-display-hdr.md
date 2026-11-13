# 40 — Per-seat virtual display and HDR

Behaviour spec of how the reference product (1.6.0.0) gives every seat its own virtual
monitor, how that monitor's mode is chosen, how frames reach the seat's stream server, and
what HDR looks like to the user. Each mechanism ends with a **Pulsar Split approach**
subsection that proposes a clean design within [../docs/agents/rules.md](../docs/agents/rules.md).
Written by the analyst. No code, no decompiled material. See [../CLEANROOM.md](../CLEANROOM.md).

Evidence markers: **[S]** static (shipped files), **[L]** live reference system (read-only),
**[D]** public Microsoft documentation (Microsoft Learn, Windows SDK headers), **[I]** analyst
inference, **[?]** unknown.

Related specs: [00-architecture.md](00-architecture.md) §5.3, §7.4, §8;
[20-service.md](20-service.md) §2 (registry) and §6 (seat start);
[30-session-rds.md](30-session-rds.md) §5 (headless client) and §11 (protocol provider);
[80-stream-server.md](80-stream-server.md) §3 (capture backend) and §8.3 (control pipe);
[10-manager.md](10-manager.md) (display settings card).

Limits of this analysis: the reference service was **stopped** during every live probe, so no
running seat, frame section or mode change was observed at runtime [L]. Phantom (not present)
device nodes of four earlier seat sessions were still in the PnP store and were read [L].

---

## 1. Summary

| Topic | Reference product | Pulsar Split |
| --- | --- | --- |
| Virtual monitor | the Microsoft RDP indirect display adapter that Remote Desktop creates for each seat session, with its UMDF image temporarily replaced by a substitute module that wraps and patches it at run time [S] | its **own** remote-session IddCx driver, named to the OS by Pulsar's **own Remote Desktop protocol provider** (§2.3). Phase 2 interim: stock RdpIdd, untouched |
| Mode source | stream server writes a mode request into a shared section; the module applies it [S] | stream server → service (pipe message) → driver (IOCTL) → documented remote display-config update |
| Frame hand-off | one shared texture handle + "frame ready" event per session, objects with a NULL DACL [S] | ring of named NT-shared textures with a fence, objects ACL'd to SYSTEM + LOCAL SERVICE |
| Render GPU | pinned by matching the adapter *description* string [S] | pinned by adapter LUID resolved from a stable PnP id, through the documented render-adapter call |
| HDR | exposed on the RDP display path, tier-gated, plus an out-of-scope kernel modification [S][L] | only IddCx 1.10 HDR / WCG of our own remote driver; SDR otherwise, with the reason shown |

---

## 2. Components and how they relate

### 2.1 What Windows does for a remote session's display [D][L]

```
 headless RDP client --loopback TCP--> RDP-Tcp WinStation (termsrv, protocol stack)
                                            |
                                            | protocol stack names its display driver by hardware id
                                            v
 Remote Desktop stack creates a software device  SWD\REMOTEDISPLAYENUM\<hwid>&SESSIONID_<NNNN>
   (parent HTREE\ROOT\0, Display class, function driver WUDFRd, upper filter IndirectKmd)
   properties: DEVPKEY_Device_SessionId = N, DEVPKEY_Device_TerminalLuid, DEVPKEY_Device_AdapterLuid
                                            |
                                            v
 one WUDF host process (session 0, LOCAL SERVICE) per device  ->  IddCx adapter (remote-session mode)
                                            |
                                            v  swap-chain frames composed by DWM of session N
```

| Fact | Evidence |
| --- | --- |
| Device id pattern `SWD\REMOTEDISPLAYENUM\RDPIDD_INDIRECTDISPLAY&SESSIONID_0002` … `_0005`, one per seat session; friendly name "Microsoft Remote Display Adapter"; hardware id `RdpIdd_IndirectDisplay`, compatible id `SWD\Generic` | [L] |
| Parent `HTREE\ROOT\0`, enumerator `SWD` (software device API) | [L] |
| `DEVPKEY_Device_SessionId` ({83da6326-97a6-4088-9453-a1923f573b29}, 6, UINT32) holds the seat session id; `DEVPKEY_Device_TerminalLuid` and `DEVPKEY_Device_AdapterLuid` (both {c50a3f10-aa5c-4247-b830-d6a6f8eaa310}, 2 / 3) are set | [L][D] (`devpkey.h`, `ntddvdeo.h`) |
| `DEVPKEY_IndirectDisplay` (same GUID, 1) holds the indirect-display info record: adapter LUID, flags = 3 (created an IddCx adapter + supports FP16), 16 monitors, target base 256 | [L][D] |
| RdpIdd INF: host process sharing **disabled** (one WUDF host per device), device group `RdpIddGroup`, UMDF 2.15, IddCx extension `IddCx0102`, hardware-key device security SDDL granting everyone all access | [L] |
| All WUDF host processes run in session 0 as **NT AUTHORITY\LOCAL SERVICE** | [L] |
| A remote-session driver sets the adapter flag REMOTE_SESSION_DRIVER (0x4). The OS fails adapter initialisation if a driver sets it on a device that the OS Remote Desktop stack did **not** create, or omits it on one that the stack did create | [D] |
| A remote-session driver must also set USE_SMALLEST_MODE and use progressive target modes only; Microsoft recommends host process sharing disabled and no device group | [D] |
| Which driver the stack loads is named by the protocol stack through the public protocol-provider interface for WDDM indirect display drivers (hardware-id query, driver-load and driver-unload notifications, mode notification). On driver load, termsrv hands the protocol stack an open handle to the driver | [D] |
| The reference ships an unused protocol provider (`DuoRds.dll`, 30 §11) that implements that interface and names `RdpIdd_IndirectDisplay` | [S] |
| On disconnect the OS destroys the session's display device node; a reconnect creates a new one | [D] |

### 2.2 What the reference changes [S]

- The service redirects the UMDF image of the RdpIdd service
  (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\RdpIdd` value `ImagePath`)
  to its own module, `%SystemRoot%\System32\drivers\UMDF\DuoIdd.dll`, for the few seconds in which
  a seat session's display device starts, then writes the genuine DriverStore path back.
  Coexistence detection of this value: §10.
- The module loads the genuine Microsoft driver and hooks internal IddCx / RdpIdd functions at
  run time. This is fragile across Windows builds and is not reproduced. Pulsar Split never
  hooks or patches Microsoft code.
- User-visible effects of the module on the seat's display:

| Effect | Notes |
| --- | --- |
| forced mode | the seat's own resolution, refresh, desktop scale and super-sampling replace what the RDP client asked for (§4) |
| pinned GPU | the session's display renders on the GPU chosen in the manager (§5) |
| hardware cursor declined | the OS draws the pointer into the frames |
| RDP encoding disabled | the RDP stack no longer encodes the session's frames for the headless client |
| frame published | every frame is offered to the seat's stream server through shared kernel objects (§6) |
| HDR metadata | HDR paths carry the static metadata of §8.2 |

| Item | Facts |
| --- | --- |
| Substitute module | `DuoIdd.dll`, 1.6.0.0, x64 UMDF DLL, exports only the UMDF driver entry; vendor-self-signed; lives in `C:\Windows\System32\drivers\UMDF\` [S][L] |
| Genuine driver on the live box | `RdpIdd.dll` 10.0.26100.9711 in `rdpidd.inf_amd64_*` [L]. The package also ships a 10.0.22621 copy [S] (never redistribute; 00 §8 item 2) |
| Adapter flags the module logs | REMOTE_SESSION = bit 0x4, FP16 = bit 0x40 [S], matching the public flag values [D] |

### 2.3 Pulsar Split approach

**Decision input (resolves the former blocker).** A third-party IDD cannot attach a monitor to an
RDP session by creating its own software device with a session id: the OS rejects the
remote-session flag on any device it did not create, and a console-mode IDD's monitors belong to
the console session [D]. The documented way to get *our* IDD into a seat session is that the
session's **protocol stack names it**. That means:

| Phase | Display in the seat session | Capture | HDR |
| --- | --- | --- | --- |
| 2 (bring-up) | stock RdpIdd via loopback RDP, unmodified | stream server captures in session N (§6.5, must be proven by test) | none (SDR) |
| 3 | Pulsar's own **remote-session IDD** (`drivers/idd/`), loaded because Pulsar's **own protocol provider** returns its hardware id; RdpIdd is not loaded in that session | driver → shared ring (§6) | IddCx 1.10 (§8) |

- The protocol provider is a WinStation-level change and needs its own spec section and a
  maintainer decision (30 §11). Until it exists, Phase 3 has no supported path for an own monitor.
- Own driver: UMDF 2 remote-session IDD, own hardware id (for example `PulsarSplit_IndirectDisplay`),
  own INF, device class Display, host process sharing disabled, no device group. Never touch
  `WUDF\Services\RdpIdd`, never load, wrap or redistribute Microsoft driver binaries.
- The service finds a seat's device by hardware id plus `DEVPKEY_Device_SessionId` = the seat's
  session id, then binds the seat explicitly over the control channel (§3.3, §7).
- Language: C/C++ starting from Microsoft's MIT-licensed indirect display sample is the lowest-risk
  path; Rust needs own IddCx bindings [I]. Build x64 and ARM64.
- In the Phase 2 layout the only display is RdpIdd, and Pulsar **never** switches it to HDR or
  changes its mode beyond what the headless client requests.
- Signing (factual): a UMDF driver package needs a catalog signature that chains to a trusted root.
  For end users without test mode this means Microsoft **attestation signing** (EV certificate +
  Partner Center) or WHQL/HLK. Attestation-signed packages load on Windows 10/11 client editions but
  **not on Windows Server**, which needs HLK (WHQL) certification. No private root CA. The driver is
  user-mode, so it works with Memory Integrity on.
- **Rule conflict — test signing (needs a maintainer decision).** Development of the driver needs
  test-signing mode, which requires Secure Boot off and is a form of DSE relaxation. The rules forbid
  "disabling or working around DSE". Proposed exception for the maintainer to accept or reject:
  test signing only on a dedicated development VM or dev machine, never on the x64 test PC without
  approval, never in the installer, never on user machines. Until a decision is recorded, Phase 3
  driver work cannot be tested on real hardware.

---

## 3. Seat binding and readiness

### 3.1 Reference behaviour [S]

- Before creating a seat's session the service writes the global value
  `HKLM\SOFTWARE\Duo` `Booting` (REG_SZ) = seat name. The module reads it when it loads; without it
  the module does nothing for any seat.
- The module finds the seat session by matching the WTS client name with the seat name (the
  headless RDP client connects with client name = seat name, 00 §7.2) and uses that session id in
  all kernel-object names (§6).
- The service removes `Booting` again when the seat start finishes. Seat starts are therefore
  serialised [I].

### 3.2 Ready / failed values (needed for the coexistence health check) [S]

| Value | Type | Meaning |
| --- | --- | --- |
| `Instances\<seat>\Status` | **REG_DWORD** | 1 = display captured and ready; 2 = display could not be captured. Deleted by the service before each start |
| `HKLM\SOFTWARE\Duo\Booting` | REG_SZ, global | name of the seat currently starting; absent otherwise |

- The service polls `Status` every 100 ms for up to 90 s; anything but 1 fails the seat start with
  a logged error (seen once in the live event log [L]). Before that it retries the client-name
  session lookup up to 60 times, 5 s apart [S].
- **Correction for 20-service.md §2.2:** that table lists `Status` as a string with value
  `Booting`. Static evidence in both the service and the module shows the DWORD above, and
  `Booting` as a separate global string [S]. The owner of 20-service must change its row; this
  spec does not edit other spec files.

### 3.3 Pulsar Split approach

- No global "booting" value, no client-name guessing. The service locates the seat's display device
  (§2.3) and sends **set seat config** (§7) with the seat GUID. Several seats may start in parallel.
- Readiness comes from the driver over the control channel: adapter initialised, monitor arrived,
  path committed, first frame published. Timeout ≈ 90 s as in the reference; the manager shows the
  step that failed.
- Never terminate the host process to force a restart. Report the error; the service decides to
  restart the seat. (A remote IDD may use the documented critical-error report for unrecoverable
  initial-configuration failures; that ends the user session, so use it only there [D].)

---

## 4. Modes: resolution, refresh, scale, super-sampling

### 4.1 Inputs [S]

Per-seat values under `HKLM\SOFTWARE\Duo\Instances\<seat>` read by the module:

| Value | Type | Default when missing/invalid | Accepted range | Notes |
| --- | --- | --- | --- | --- |
| `RefreshRate` | DWORD Hz | 60 (paid tier), 30 (free tier) | 15 – 500 (paid), 15 – 30 (free) | also written by the module |
| `IsScaleFactorStatic` | DWORD 0/1 | 0 | — | 0 = dynamic scale |
| `ScaleFactor` | DWORD % | 100 | 100 – 500 | only when static |
| `SuperSamplingFactor` | DWORD % | 100 | 100 – 500 | forced to 100 in the free tier |
| `ForceSDR` | DWORD 0/1 | 0 | — | §8 |
| `SDRWhiteLevel` | DWORD nits | 400 | 80 – 480, else 400 | §8 |
| `RenderAdapter` | REG_SZ | — | adapter description | §5 |
| `Width`, `Height` | DWORD px | — | — | written by the module; read by the service as the next headless client's desktop size |

Global values under `HKLM\SOFTWARE\Duo`: `MinLuminance`, `MaxLuminance`, `MaxFullFrameLuminance`
(DWORD nits, §8), `Verbosity`, `Booting` (§3.1). Live seat values: 1920 × 1080 @ 120, scale
dynamic/100, super-sampling 100, `SDRWhiteLevel` 80 and 0 (0 is invalid, so 400 applies) [L].
The tier limits are the reference's paid gating; Pulsar Split has no tiers (00 §9).

### 4.2 Who asks for a mode [S]

- The **stream server** asks. On every wake of its capture loop it compares the size, refresh and
  (when a path is active) HDR flag in the seat's section header (§6.2) with the mode its current
  client stream wants. If they differ it logs a mode change, writes width, height, refresh and HDR
  into the header and sets the request-pending field. HDR is requested as 0 while no path is
  active [S].
- The module applies the request and clears the pending field only on success [S].
- When the RDP stack itself changes the session's layout, the module keeps the RDP-requested size
  but substitutes the seat's refresh rate [S].
- Note for 80-stream-server §3.1: the server's upstream display-configuration code is still
  present, but seat mode changes travel through the section, not through `SetDisplayConfig`. In a
  WDDM remote session `SetDisplayConfig` and `DisplayConfigSetDeviceInfo` fail by design [D].

### 4.3 Mode rules (unordered) [S]

| Rule | Behaviour |
| --- | --- |
| refresh clamp | requested refresh clamped to the allowed range of §4.1 |
| HDR gate | HDR only when the request asks for HDR, `ForceSDR` = 0 and the reference's tier allows it |
| dynamic scale | requested height ÷ 1080 × 100 %, clamped 100 – 500 %: 1080p → 100 %, 1440p → 133 %, 2160p → 200 % |
| static scale | `ScaleFactor` |
| super-sampling | render size = requested size × `SuperSamplingFactor` ÷ 100 on both axes (integer); the stream server scales frames down to the stream size [I] |
| persisted values | the *requested* (not super-sampled) width, height and clamped refresh are stored as `Width`, `Height`, `RefreshRate` (REG_DWORD) |
| offered modes | exactly one target mode at the render size: no blanking (active = total), progressive, refresh as an integer rate, 8- and 10-bit RGB wire formats |
| committed path | mode valid + scale valid; HDR adds colorimetry valid + SDR white level valid (public path-flag values 0x1/0x2/0x8/0x10 [D]); wire bits RGB = 8-bit for SDR, 8+10-bit for HDR [I]; the desktop scale percentage goes with the path |
| no blanking of the session | the mode change happens on the running path, without detaching the monitor |

| Example request | Result on the seat's virtual monitor |
| --- | --- |
| 1920 × 1080 @ 120, SDR, SS 100, scale dynamic | 1920 × 1080 @ 120, scale 100 % |
| 2560 × 1440 @ 144, SDR, SS 150 | rendered 3840 × 2160 @ 144, scale 133 %; stream 2560 × 1440 |
| 3840 × 2160 @ 60, HDR, ForceSDR 1 | 3840 × 2160 @ 60 SDR, scale 200 % |
| 1280 × 720 @ 10 | refresh raised to 15; scale 100 % (clamped) |

### 4.4 Pulsar Split approach

- Same user-visible rules, no tiers: refresh 15 – 500 Hz (default 60), scale dynamic (height/1080,
  clamped 100 – 500 %) or static 100 – 500 %, super-sampling 100 – 500 %. Recommended: round dynamic
  scale to the nearest Windows scale step (100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500).
  This is a deliberate deviation; the manager says so.
- **Mode path.** The stream server never changes the display itself.

```
 Moonlight client launch / resume
        |
 stream server (session N, SYSTEM)  --seat control pipe event--> service (session 0)
   display.mode.request {w, h, hz, hdr}                            | apply §4.3 rules + seat settings
                                                                   v
                                          driver IOCTL REQUEST_MODE (§7)
                                                                   |
                         driver: update target modes, then documented remote display-config
                         update (mode, scale factor, colorimetry, SDR white level)
                                                                   |
 stream server  <--- display.mode.result {ok | error code} -------+   frames in new size/format (§6)
```

- New message pair on the per-seat stream-server control pipe (80 §8.3 option B). The server
  raises the event when a client launches or resumes a stream, and when the client's requested
  size, frame rate or dynamic range changes:

| Message | Direction | Fields | Notes |
| --- | --- | --- | --- |
| `display.mode.request` | server → service (event) | `w`, `h` (px), `hz` (integer), `hdr` (bool), `requestId` | stream size, not render size |
| `display.mode.result` | service → server | `requestId`, `status`, applied `w`/`h`/`hz`/`hdr`, `renderW`/`renderH`, `format` (§6.3) | sent after the driver reports the committed path |

| `status` | Meaning |
| --- | --- |
| `ok` | applied as requested |
| `ok_sdr_fallback` | applied, but SDR (Force SDR set, OS or GPU without IddCx HDR, driver without FP16) |
| `clamped` | refresh or size clamped to limits; applied values say what |
| `no_display` | seat display not ready / device gone |
| `rejected` | OS refused the configuration; service keeps the previous mode |
| `timeout` | no driver confirmation within 5 s |
| `unsupported` | Phase 2 layout (stock RdpIdd): mode follows the headless client, no change possible |

- Who commits the mode: the **driver**, through the documented remote display-config update. In a
  remote session no in-session process can do it with `SetDisplayConfig` /
  `DisplayConfigSetDeviceInfo` [D], so there is no in-session helper.
- **Desktop scale (resolved):** the remote path carries a per-monitor scale factor field, 100 – 500 %,
  documented as a hint the OS may override [D]. No undocumented call and no EDID trick is needed.
  The service reads back the effective DPI (`DisplayConfigGetDeviceInfo` works in remote sessions
  [D]) and reports a mismatch to the manager.
- The service persists last-used `Width`/`Height`/`RefreshRate` under Pulsar's own seat key; the
  driver reads no registry.
- Pipeline capacity: declare an adapter pipeline rate that covers the largest render mode; cap
  super-sampled modes so width × height × refresh stays within what the encoder GPU can handle, and
  tell the user why.

---

## 5. Render-GPU pinning [S]

- The seat's `RenderAdapter` holds the adapter **description string** as DXGI reports it, chosen in
  the manager (10-manager: hardware adapters only, duplicates removed, default = most dedicated video
  memory).
- When IddCx sets the render adapter, the module passes the LUID of the first DXGI adapter whose
  description equals `RenderAdapter`; no match → the original choice stands.
- Frames are rendered on that GPU; the stream server must open the shared texture there.
- Two identical GPUs cannot be told apart.

### Pulsar Split approach

- Store the GPU as **PnP instance id + description + last LUID**; resolve the current LUID at seat
  start (LUIDs change between boots) and pass it in **set seat config**. The driver uses the
  documented render-adapter call (IddCx 1.4+) [D].
- The stream server opens the ring on the adapter with that LUID; it gets the LUID in
  `display.mode.result` and in the header. GPU gone → system default GPU and a manager warning.
- Hybrid laptops: warn when the chosen GPU has no hardware encoder [I].

---

## 6. Frame hand-off to the stream server

### 6.1 Reference named objects (per seat session N, decimal id) [S]

| Name | Type | Creator → user | Security |
| --- | --- | --- | --- |
| `Global\DuoIdd<N>` | page-file section, 60 bytes | module → stream server (opened with all access) | **NULL DACL** (everyone full access) |
| `Global\DuoIdd<N>FrameReady` | auto-reset event | module sets, stream server waits (100 ms timeout loop) | NULL DACL |
| `Global\DuoIdd<N>FrameEncoded` | auto-reset event | opened by the server's encoder path; the module does not wait on it | NULL DACL |

The stream server derives N from its own process's session id [S].

### 6.2 Reference section header (60 bytes, little-endian) [S]

Recorded so an implementer can recognise the reference's objects; Pulsar defines its own (§6.4).

| Offset | Size | Field | Written by |
| --- | --- | --- | --- |
| 0x00 | 4 | producer process id (the UMDF host) | module |
| 0x04 | 8 | handle value, inside the producer, of an NT shared handle to the current frame surface | module |
| 0x0C | 8 | frame-ready event handle value (informational) | module |
| 0x14 | 8 | frame-encoded event handle value (informational) | module |
| 0x1C | 4 | presentation frame number of the last published frame | module |
| 0x20 | 4 | [?] unused | — |
| 0x24 | 4 | path active (non-zero) | module |
| 0x28 | 4 | width px | module (current) / server (request) |
| 0x2C | 4 | height px | module / server |
| 0x30 | 4 | refresh Hz | module / server |
| 0x34 | 4 | HDR (non-zero) | module / server |
| 0x38 | 4 | request pending | server sets, module clears on success |

Consumer behaviour [S]: the server opens the producer process (by the pid at 0x00) and duplicates the
surface handle at 0x04 into itself; it treats a changed frame number as a new frame.

### 6.3 Reference frame contract [S][I]

- One frame-ready signal per published frame; the header's frame number identifies it.
- The surface handle changes only when the swap-chain surface object changes; the old handle is
  closed by the producer.
- The producer does not wait for the consumer: a slow consumer can read a surface that DWM is
  already reusing, so tearing or repeated frames are possible [I].
- SDR surfaces are 8-bit BGRA; HDR surfaces are FP16 scRGB [I]. The server converts to Rec. 2020 PQ
  for HDR10 output [S].
- The pointer is part of the image (hardware cursor declined).

| Error case | Behaviour |
| --- | --- |
| section or events cannot be created | the frame is not published [S] |
| shared-handle creation fails | the header keeps the old handle [S] |
| stream server not running | frames are published and dropped [I] |

### 6.4 Pulsar Split approach — frame channel contract

**Objects** (per seat; `<seat>` = seat GUID in registry format without braces; backslashes are not
allowed in the object name after `Global\`):

| Name | Type | Creator | Consumer access |
| --- | --- | --- | --- |
| `Global\PulsarSplit.Display.<seat>.Header` | page-file section, 4 KiB | driver | map read; write only the `consumerHeartbeat` and `consumerFrame` fields |
| `Global\PulsarSplit.Display.<seat>.FrameReady` | auto-reset event | driver | synchronise |
| `Global\PulsarSplit.Display.<seat>.Tex<gen>.<i>` | named NT-shared D3D11 texture, i = 0…2 | driver | open by name on the pinned adapter, read |
| `Global\PulsarSplit.Display.<seat>.Fence<gen>` | named NT-shared D3D11 fence | driver | open by name, wait / read value |

- **Security descriptor on every object:** protected DACL, SYSTEM (S-1-5-18) full, LOCAL SERVICE
  (S-1-5-19, the WUDF host identity [L]) full, nobody else; no NULL DACLs, no Everyone, no
  Authenticated Users. The consumer is the stream server running with a SYSTEM token in session N
  (80 §10), so it matches the SYSTEM entry. Opening is **by name**; there is no handle
  duplication and no process handle to the driver host.
- 80-stream-server §3.3 uses `Global\PulsarSplit\Seat<seat-id>\...` and "SYSTEM + read for the
  server's token". Both must be replaced by the names and ACL above (backslash form is invalid;
  the server's token *is* SYSTEM). The 80 owner must align; this spec does not edit 80.
- `<gen>` increases whenever size or format changes; old textures are released after the consumer
  has acknowledged the new generation or after 1 s.

**Header layout v1** (little-endian, packed, no padding):

| Offset | Size | Field | Notes |
| --- | --- | --- | --- |
| 0x00 | 4 | `magic` | ASCII `PSDH` |
| 0x04 | 2 | `version` | 1 |
| 0x06 | 2 | `headerSize` | bytes in use |
| 0x08 | 16 | `seatGuid` | binary GUID |
| 0x18 | 4 | `generation` | as `<gen>` |
| 0x1C | 4 | `ringSize` | 3 |
| 0x20 | 8 | `frameNumber` | monotonic, per seat |
| 0x28 | 4 | `latestIndex` | ring slot of `frameNumber` |
| 0x2C | 4 | `flags` | bit 0 path active, bit 1 HDR, bit 2 driver stopping |
| 0x30 | 8 | `fenceValue` | fence value signalled when `latestIndex` is complete |
| 0x38 | 8 | `presentQpc` | QPC time of the frame |
| 0x40 | 4 | `width` | render size |
| 0x44 | 4 | `height` | |
| 0x48 | 4 | `refreshMilliHz` | |
| 0x4C | 4 | `dxgiFormat` | 87 = B8G8R8A8_UNORM (SDR) or 10 = R16G16B16A16_FLOAT (HDR / WCG) |
| 0x50 | 4 | `colorSpace` | DXGI colour space: RGB full G22 P709 for SDR, RGB full G10 P709 (scRGB) for FP16 |
| 0x54 | 8 | `adapterLuid` | render GPU |
| 0x5C | 4 | `hdrMetaGeneration` | increments when §8.2 values change |
| 0x60 | 40 | `hdrMeta` | 8 × u16 chromaticity (red, green, blue, white x/y, 10-bit fractions), 3 × u32 luminance (min, max, max full-frame, 1/10 000 nit), u32 SDR white level (nits), 8 bytes reserved; values as §8.2 |
| 0x88 | 8 | `producerHeartbeat` | QPC, updated at least every 500 ms even without frames |
| 0x90 | 8 | `consumerHeartbeat` | QPC, written by the consumer |
| 0x98 | 8 | `consumerFrame` | last frame number the consumer took |

**Contract:**

- One FrameReady signal per published frame. The driver copies the swap-chain surface into the next
  free ring slot on the render GPU, signals the fence, updates the header, sets the event, then
  finishes the IddCx frame. It never writes a slot the consumer may still read (slot ≠ the one at
  `consumerFrame`); if no slot is free it drops the oldest unread frame, never blocks DWM.
- Formats: only the two in the table. No RGB10A2 and no PQ conversion in the driver; the stream
  server converts scRGB to Rec. 2020 PQ (its existing shaders). 80 §3.3 lists RGB10A2 as a ring
  format; that entry must be dropped.
- Mode requests do **not** go through the header (§4.4).
- Either side treats a heartbeat older than 3 s as "peer gone".
- Cursor: the OS composites the pointer (as the reference). Hardware-cursor support is a later
  option.

### 6.5 Phase 2 capture (stock RdpIdd) [?]

- Without our own driver, the stream server must capture RdpIdd's output inside session N with
  Desktop Duplication or Windows Graphics Capture. Microsoft documents no remote-session exclusion
  for Desktop Duplication (only disconnected sessions and secure desktops) [D]; 80 §3.2 states it is
  unavailable for RDP indirect displays [I]. Not tested yet.
- Test to run before Phase 2 work (needs a loopback RDP seat session; the x64 test PC is
  read-only without approval, so use a dev VM): from a SYSTEM process in the seat session, record
  the result of duplicating the RdpIdd output and of creating a capture item for that monitor.
- The loopback RDP stack keeps encoding frames for the headless client in Phase 2. The headless
  client requests 32 bpp and no virtual channels (30 §5.1); its graphics pipeline mode is [?].
  Do **not** ask the server to suppress display updates: that makes the OS deactivate the
  session's display paths, which also stops capture [D]. Cost (CPU/GPU of termsrv and the client
  process per seat) is [?]; measure it with one seat running.

---

## 7. Control channel (Pulsar Split only)

The reference has no control channel beyond the registry and the section request field.

| Item | Design |
| --- | --- |
| Transport | device interface of the IDD, own interface GUID, registered by the driver; opened by the **service only** (session 0, SYSTEM) |
| Device security | hardware-key `Security` value in the INF's HW AddReg section, protected DACL: SYSTEM full, Administrators read (diagnostics); same mechanism RdpIdd uses with a permissive SDDL [L] |
| Termsrv handle | the protocol provider also receives an open driver handle on driver load [D]; it closes it at once and leaves control to the service [I] |
| Persistence | service owns all config; driver keeps nothing across restarts |
| Versioning | every buffer starts with `size` (u32) and `version` (u16, = 1) and `reserved` (u16); the driver rejects unknown major versions |

IOCTLs (device type FILE_DEVICE_UNKNOWN, buffered, read+write access):

| Code | Function | Input | Output |
| --- | --- | --- | --- |
| 0x0022E000 | `GET_INFO` | — | driver version, IddCx version (`IddCxGetVersion`), FP16 capable, HDR capable, max pipeline rate |
| 0x0022E004 | `SET_SEAT_CONFIG` | seat GUID, render adapter LUID, HDR allowed, SDR white level nits, min / max / max-full-frame luminance nits, scale mode + static %, super-sampling % | — |
| 0x0022E008 | `REQUEST_MODE` | request id, stream w, h, hz, hdr | applied w, h, hz, hdr, render w, h, DXGI format, status |
| 0x0022E00C | `GET_STATE` | — | adapter ready, monitor present, path active, current mode, HDR active, frame number, last error |
| 0x0022E010 | `WAIT_EVENT` | — (pended until an event) | event kind (ready, mode applied, mode rejected, HDR unavailable, device stopping) + data |
| 0x0022E014 | `SHUTDOWN_SEAT` | — | — (monitor departure, objects closed) |

| Error (NTSTATUS-style, returned as Win32 error to the service) | Meaning |
| --- | --- |
| invalid parameter | bad size/version or value outside §4.4 ranges |
| device not ready | no seat config yet, or adapter not initialised |
| not supported | HDR requested but OS / GPU / driver cannot (status `ok_sdr_fallback` is returned instead for `REQUEST_MODE`) |
| graphics indirect display device stopped | session disconnecting or adapter stopping (expected [D]) |
| busy | another mode request is in flight |

---

## 8. HDR — user-visible behaviour

### 8.1 What the user sees in the reference [S][L]

- A seat's stream can be HDR (10-bit, Rec. 2020, PQ) when the Moonlight client asks for HDR and the
  host is Windows 11 23H2 or later (the manager's own claim [S]). Otherwise SDR.
- *Force SDR* keeps the virtual monitor SDR. *SDR white level* (80 – 480 nits, default 400) sets the
  brightness of SDR content inside an HDR stream. Both, and HDR itself, are paid-tier features in
  the reference.
- The module performs no OS build check; it uses what IddCx offers [S]. The only build constant
  found in the reference service is 22631; it gates a device-property clean-up that belongs to
  50-shim-isolation, not HDR [S].
- The reference additionally performs a kernel-level modification at service start, only when
  Memory Integrity is off. That mechanism is **out of scope** and not described here (00 §8 item 3).

### 8.2 HDR metadata the virtual monitor reports [S]

| Item | Value |
| --- | --- |
| primaries (CIE x, y) | red 0.708, 0.292 · green 0.170, 0.797 · blue 0.131, 0.046 (Rec. 2020), each as a 10-bit fraction (× 1024, truncated) — the documented encoding [D] |
| white point | D65, 0.3127, 0.329 (same encoding) |
| min / max / max full-frame luminance | `MinLuminance` default 0, `MaxLuminance` default 1000, `MaxFullFrameLuminance` default 1000 nits; sent in 1/10 000 nit units [D] |
| validity rule | registry values used only if min < max full-frame ≤ max; otherwise all three defaults |
| SDR white level | `SDRWhiteLevel` if 80 – 480, else 400 |
| wire format | 8 or 10 bits per component RGB |

The luminance values are global; the 1.6 manager has no control for them (absent live) [S][L].

### 8.3 Availability matrix [D][L]

| Host | IddCx | HDR for a Pulsar seat |
| --- | --- | --- |
| Windows 10 (any) | ≤ 1.5 | no — SDR |
| Windows 11 21H2 / 22H2 (22000 / 22621) | 1.8 / 1.9 | no — SDR |
| Windows 11 23H2 (22631) | 1.10 (`IddCxGetVersion` 0x1A00) | yes |
| Windows 11 24H2 and later (26100+) | 1.10 (0x1A80) or 1.11 | yes |
| live box: build 26300, `IddCx.dll` 10.0.26100.9818 | 1.10+ [L] | yes |
| Windows Server 2022 (20348) | 1.7 | no — SDR |
| Windows Server 2025 (26100) | [?] not in Microsoft's table; same code base as 24H2 [I] | [?] |

### 8.4 Pulsar Split approach

- Only the IddCx 1.10 HDR / WCG path of our own remote driver: FP16 adapter capability flag, the
  "2" target-mode and buffer-acquire calls, the remote display-config update with colorimetry
  (BT.2020 RGB + ST 2084 flags) and SDR white level. A remote driver that declares FP16 must supply
  colorimetry and SDR white level and apply the SDR white level to the cursor [D].
- Detection: the driver checks at run time that the 1.10 functions exist and reports the result in
  `GET_INFO`; the service combines it with the OS build and the GPU. Manager reason text cites the
  build and IddCx version (for example "Windows 11 23H2 or later needed").
- HDR is switched on by the driver's path commit, not by an advanced-colour call from inside the
  session (that call fails in remote sessions [D]).
- Values: reference defaults and rules of §8.2; luminance values move **per seat** and appear in the
  manager (advanced).
- Phase 2 (stock RdpIdd): always SDR; Pulsar never enables HDR on the RDP display.
- Windows Server: Phase 3 needs a WHQL-signed driver (§2.3). Without one, Server hosts stay on the
  Phase 2 layout (SDR) and the manager says why. HDR on Server 2025 only once §8.3 is confirmed.
- Memory Integrity stays on; there is no alternate path. Everything free; no gating.

---

## 9. Related system settings touched by the reference service [S][L]

| Key / value | Live value | Effect |
| --- | --- | --- |
| `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers` `TerminateIndirectOnStall` | 0 | the OS does not tear down an indirect display that stalls |
| same key `IddCxDebugCtrl` | 128 | [?] IddCx debug/behaviour control |
| `…\Terminal Server\WinStations` `SetGpuRealtimePriority` | 1 | GPU realtime priority for remote sessions |
| Terminal Services policies (`fEnableWddmDriver`, graphics/AVC policies) | see 30 §3 | force the WDDM remote display driver and hardware rendering in RDP sessions |

### Pulsar Split approach

Set only what our own driver needs, documented and reversible (record original values, restore on
uninstall). `TerminateIndirectOnStall` should not be needed for a non-hooking driver; keep the OS
watchdog. Do not change `IddCxDebugCtrl`. The WDDM remote display policy must stay on, since a
remote IDD only loads in a WDDM remote session [I].

---

## 10. Coexistence with an installed reference product

| Check | Rule |
| --- | --- |
| `WUDF\Services\RdpIdd` `ImagePath` | genuine when it points into the DriverStore `rdpidd.inf_*` folder (live: yes [L]). If it points to `System32\drivers\UMDF\DuoIdd.dll`, a seat start is in progress or the reference died mid-start; every later RDP session on the machine then loads the substitute [I]. The reference's own restore writes the DriverStore path relative to `%SystemRoot%`, or the `System32\drivers\UMDF\RdpIdd.dll` copy when the DriverStore folder is not found [S]. Pulsar Split never changes this value while the reference is installed; the health page reports it; a restore is offered only when the reference is not installed |
| `Instances\<seat>\Status` | read as REG_DWORD (§3.2), not as a string |
| `Global\DuoIdd*` objects | present only while a reference seat runs; Pulsar never opens them |
| names | Pulsar's objects, interface GUIDs, device class, hardware id, event source and registry root never reuse `DuoIdd`, `Duo`, `Booting` or `Global\DuoIdd*` |
| protocol provider | Pulsar's provider must use its own WinStation, never replace `RDP-Tcp`'s, so reference seats keep working [I] |

---

## 11. Open questions

| # | Question | Status / how to resolve |
| --- | --- | --- |
| 1 | Session attachment of an own IDD | **resolved [D]:** only through a protocol stack that names the driver (§2.3). Remaining: protocol-provider design and its maintainer decision (30 §11) |
| 2 | Does Desktop Duplication / WGC of RdpIdd work in a loopback RDP seat session? | [?] test in §6.5; decides Phase 2 capture |
| 3 | CPU/GPU cost of RDP encoding per seat in Phase 2; headless client graphics mode | [?] measure with one running seat; read 30 §5 client settings |
| 4 | IddCx version and HDR on Windows Server 2025 | [?] read `IddCxGetVersion` / `IddCx.dll` version on a Server 2025 host |
| 5 | Can a protocol provider be registered and used on client SKUs alongside the wrapped termsrv? | [?] 30-session-rds |
| 6 | Effective DPI when the scale-factor hint is overridden by the OS | [?] Phase 3 test; service reports mismatch (§4.4) |
| 7 | Meaning of `IddCxDebugCtrl` = 128 | [?] not needed by Pulsar |
| 8 | Runtime confirmation of §3 and §6 on a running reference seat | [?] needs approval to start the reference service |
| 9 | Test-signing exception for driver development | maintainer decision (§2.3) |
| — | Former questions: HDR path-flag bits and colour field values | resolved [D]: path flags = valid-field bits; "1 / 3" = RGB wire bits 8 vs 8+10 [I]; irrelevant to Pulsar beyond §4.3 |
| — | Former: does the reference server signal FrameEncoded / use handle values | resolved [S]: it duplicates the handle out of the producer by pid (§6.2); FrameEncoded opened in the encoder path |
| — | Former: host processes per seat device | resolved [L]: process sharing disabled, one WUDF host per device |
