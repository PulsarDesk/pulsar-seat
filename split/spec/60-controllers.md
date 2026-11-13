# 60 — Virtual controllers and Bluetooth handling

How the reference product gives each seat its own virtual gamepads, how feedback flows
back to the streaming client, how pads are tied to a session, and how it stops remote
seats from taking over the console's Bluetooth controllers. Evidence markers as in
`00-architecture.md`: [S] static, [L] live, [I] inferred, [?] unknown. No code; prose and
tables only. Each part ends with a **Pulsar Split approach** subsection. Decisions that
need the maintainer are collected in §12, open questions in §13.

Revision 2 (2026-11-12). This revision removes the step-by-step descriptions of
the reference's internal routines. It adds live evidence from the reference host's
PnP install log (instance-id form, matched hardware ids, filter placement, session tag
on the HID child), the code-integrity state of the host, and the Bluetooth user-service
configuration. It also fixes the Pulsar Split pad contract (control messages, shared
memory layout, device properties, ACL) shared with `80-stream-server.md`.

Correction to `00-architecture.md` §5: the shared-section name uses the *device
instance id* of the pad (with `\` replaced by `_`), not the 32-hex-digit value alone.

---

## 1. Components

| # | Component | Kind | Runs in | Role |
| --- | --- | --- | --- | --- |
| 1 | `DuoController\DuoController.dll` | one x64 DLL with **two roles** [S] | (a) the seat's stream-server process; (b) the UMDF host process (`WUDFHost.exe`) | (a) **client library**: exported C functions the stream server calls to create, feed and remove pads. It installs the driver package and creates and removes device nodes. (b) **UMDF 2.15 HID minidriver** that answers the HID class for each virtual pad |
| 2 | `DuoController.inf` + catalog | driver package, own setup class [S][L] | — | binds four root hardware ids to the DLL under the inbox UMDF reflector and the inbox HID-UMDF pass-through. The Xbox variant also adds the inbox `xinputhid` filter |
| 3 | `sunshine.exe` (vendor fork) | stream server [S] | seat session | loads library (1) by a path relative to its install folder. It turns Moonlight gamepad packets into per-kind input records and sends rumble back to Moonlight. It still contains the ViGEm client code [S] |
| 4 | `Duo.exe` (service) | [S] | session 0 | removes leftover pad device nodes and the pad INF at service start and stop. It installs and removes the Bluetooth wrapper |
| 5 | `DuoBluetooth.dll` | service-DLL wrapper [S][L] | per-user `BluetoothUserService_*` svchost | lets the real Bluetooth user service run only in the active console session |
| 6 | ViGEmBus 1.21.442.0 (MSI 1.22.0) + `nefconw.exe` | third-party kernel bus driver, WHQL-signed [L][S] | kernel | standard virtual Xbox 360 / DualShock 4 pads. Present and running on the live box [L]. Secondary role in 1.6 (§7) |

```
 Moonlight client --(gamepad packets)--> sunshine.exe (seat session, elevated)
                                             |  DuoController.dll (client role)
                                             |   +- SetupAPI: create ROOT devnode, install driver
                                             |   +- global registry value (session hand-over, §8.1)
                                             |   +- opens Global\Duo_<padId>_* sections/events
                                             v
                        +---------- shared memory + named events ----------+
                        |  ..._input  (+ ..._input_event)   client -> driver |
                        |  ..._output (+ ..._output_event)  driver -> client |
                        +-------------------------------------------------- +
                                             ^
 ROOT devnode  ROOT\VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>\0000   [L]
   stack (top to bottom):
     [xinputhid]      upper filter, Xbox kind only                  [L]
     mshidumdf        function driver (HID <-> UMDF pass-through)   [L]
     WUDFRd           lower filter (reflector) -> WUDFHost.exe (session 0)
                        -> DuoController.dll (driver role, creates the sections)
        |
        v  HIDClass enumerates one child per pad
 HID child     HID\VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>\1&<hash>&0&0000   [L]
   function: inbox input.inf raw HID + hidgamepad upper filter       [L]
        v
 games / Steam / SDL in the seat session
```

The filter placement corrects revision 1. `xinputhid` sits on the root (minidriver)
stack above `mshidumdf`, not on the HID child [L].

---

## 2. Driver package

### 2.1 Identity

| Item | Value | Ev. |
| --- | --- | --- |
| Setup class name | `DuoController` | [S][L] |
| Setup class GUID | `{070D9FF9-4F04-4419-90D2-C4CA9DFDB2EE}` (the service uses it to find its INF for removal) | [S][L] |
| Provider / signer | the vendor. The catalog is signed with the vendor's own self-signed certificate. PnP reports signer score "Authenticode" and driver rank `0x00FF0000` | [L] |
| DriverVer | 2026-08-14, 1.6.0.0 | [S][L] |
| Target | Windows 10 1803 or newer, x64 only | [S] |
| Published as | `oemNNN.inf`. The number changes between installs (oem188, then oem195 after a reinstall, on the live box) because the service uninstalls the INF on every stop | [L] |
| Licence header | the INF text carries an Apache-2.0 header with the vendor's copyright | [S] |

### 2.2 Hardware ids, matching and install sections [L]

Observed in the reference host's PnP install log for every pad created on 2026-11-11/12
(18 root installs: DualSense, DualShock 4 and Xbox kinds):

| Item | DualSense | DualSense Edge | DualShock 4 | Xbox One Elite |
| --- | --- | --- | --- | --- |
| Root instance id | `ROOT\VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>\0000` | `ROOT\VID_054C&PID_0DF2&DUOCONTROLLER&<32hex>\0000` [I] | `ROOT\VID_054C&PID_05C4&DUOCONTROLLER&<32hex>\0000` | `ROOT\VID_045E&PID_02FF&DUOCONTROLLER&<32hex>\0000` |
| Root hardware id searched (= INF match id) | `ROOT\VID_054C&PID_0CE6` | `ROOT\VID_054C&PID_0DF2` [S] | `ROOT\VID_054C&PID_05C4` | `ROOT\VID_045E&PID_02FF&IG_00` |
| Install section kind | PlayStation | PlayStation | PlayStation | Xbox |
| Root stack | function `mshidumdf`, lower filter `WUDFRd` | same | same | same + upper filter `xinputhid` |
| HID child instance id | `HID\VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>\1&<hash>&0&0000` | [?] | `HID\VID_054C&PID_05C4&DUOCONTROLLER&<32hex>\1&…` | `HID\VID_045E&PID_02FF&DUOCONTROLLER&<32hex>\1&…` |
| HID child hardware ids | `HID\VID_054C&PID_0CE6`, `HID\VID_054C&UP:0001_U:0005`, `HID_DEVICE_SYSTEM_GAME`, `HID_DEVICE_UP:0001_U:0005`, `HID_DEVICE`; no compatible ids | [?] | [?] | [?] (the inbox `xinputhid.inf` lists `HID\VID_045E&PID_02FF&IG_00`) |
| HID child driver | inbox `input.inf` raw HID (match `HID_DEVICE_SYSTEM_GAME`) + `hidgamepad` upper filter | [?] | [?] | [?] |

Facts that follow from the table:
- The root devnode's **hardware id** is the short `ROOT\VID_…&PID_…` form, and PnP ranks
  the INF on it. The `DUOCONTROLLER&<32hex>` part is only the device-id part of the
  **instance id**. Revision 1 had these the wrong way round.
- The HID child's device-id part copies the root's device-id part (`VID_…&PID_…&DUOCONTROLLER&<32hex>`).
  This is the HID class's own naming, so the marker reaches the child without any work
  by the driver [L][I].
- The Xbox root hardware id carries `IG_00`, but the instance id does not. The HID child
  instance id, and therefore its device-interface path, has no `IG_` token [L]. Public
  SDL code uses an `IG_` token in the device path to detect XInput-backed HID pads. So
  this pad is probably seen both through XInput and through raw HID [I].
- Removing an Xbox pad while a process holds it open makes the query-removal fail with
  an "outstanding open" veto. The reference then deletes the child explicitly [L].

### 2.3 UMDF settings [S]

The INF uses the standard UMDF 2 HID-minidriver settings that the WDK documents for a
driver under `mshidumdf`: UMDF library 2.15, binary in `System32\drivers\UMDF`, kernel
clients allowed, null and unknown file objects allowed, METHOD_NEITHER requests copied,
FsContext2 usable, high host priority. It sets no host-sharing or account override, so
the driver runs in the default shared UMDF host, whose account is **LOCAL SERVICE**
according to the WDK documentation [S][I]. For the Xbox kind the INF pulls in the inbox
`xinputhid.inf` sections for the GIP-HID service and hardware settings (which add the
`xinputhid` upper filter [L]). It also writes a `DevicePropertyFlags` DWORD = 4 to the
device hardware key. What that value controls is not documented publicly [?].

**Pulsar Split approach (package).** Own setup class and GUID, own enumerator and id
marker (§3.5), own provider string. Never reuse the reference GUID, so both products can
coexist. Keep UMDF 2 + `mshidumdf` (+ `xinputhid` for the Xbox kind). Every kernel
component involved is an inbox Microsoft binary, so nothing new runs in the kernel.
Install the package **once** at product install; uninstall only in the uninstaller.
Signing: see §11.

---

## 3. Reference client library and the Pulsar Split pad contract

### 3.1 Reference exports (interface) [S]

All exports are plain C functions with the x64 calling convention. They return an
HRESULT. Each one sets the Windows Runtime to multi-threaded mode for the calling
thread. The stream server binds them by name at runtime. If a name is missing, it logs
an error and runs without pads.

| Export | Parameters | Behaviour | Errors |
| --- | --- | --- | --- |
| `DuoController_Initialize` | none | marks the library as ready | none |
| `DuoController_Uninitialize` | none | removes every pad still registered | `E_UNEXPECTED` if not initialised |
| `DuoController_CreateController` | (1) pad type (u32, §3.3); (2) feedback callback (may be null); (3) opaque user pointer for the callback; (4) out: pad handle | creates and installs a device node, connects to the driver's sections, starts feedback delivery. For the Xbox type a neutral report follows after about 1 s | `E_UNEXPECTED` not initialised; `E_INVALIDARG` null out-pointer or unknown type; `E_OUTOFMEMORY`; `E_ACCESSDENIED` caller not elevated; `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)` INF missing next to the DLL, or the driver did not create its sections within about 1 s; SetupAPI errors as `HRESULT_FROM_WIN32` |
| `DuoController_SendReport` | (1) pad handle; (2) pointer to the per-kind input record | updates the pad's input report and notifies the driver | `E_UNEXPECTED`; `E_INVALIDARG`; `HRESULT_FROM_WIN32(ERROR_PIPE_NOT_CONNECTED)` section not mapped |
| `DuoController_RemoveController` | (1) pad handle | stops feedback, disconnects, removes the device node and waits (up to about 10 s) until it is gone | `E_UNEXPECTED`; `E_INVALIDARG` unknown handle |

There is no fixed pad limit in the library [S]; the stream server's limit applies [I].

### 3.2 Observable effects of pad creation [S][L]

| Effect | Detail |
| --- | --- |
| Driver package | staged into the driver store on first use (no overwrite) |
| New ROOT devnode | instance id `ROOT\VID_<vid>&PID_<pid>&DUOCONTROLLER&<suffix>\0000`, hardware id per §2.2 [L] |
| Suffix | 32 upper-case hex digits in four 8-digit fields: tick-count low, tick-count high, creating process id, per-process counter from 1. Live example: tick high 0, PID 0x205C, counter 7 [S][L]. So every creation makes a new devnode and a new HID child id |
| Owner session | passed to the driver through one global registry value under `HKLM\SOFTWARE\Duo` (`DuoControllerSessionId`, REG_DWORD) that exists only during the install (§8.1) |
| Pad id | instance id with `\` → `_`; used in the section names (§4.1) |
| Failure | the new devnode is removed again |

### 3.3 Pad types [S]

| Type value | Pad | VID / PID | Report descriptor | Driver kind |
| --- | --- | --- | --- | --- |
| 0 | Xbox One Elite (XInput via `xinputhid`) | 045E / 02FF (`IG_00`) | 202 bytes; input id 1 (62 bytes with id), output id 3 (9 bytes with id) | Xbox |
| 1 | DualShock 4 (first revision) | 054C / 05C4 | 467 bytes, DS4 USB layout: input 0x01 64 bytes, output 0x05 32 bytes, many feature ids | DS4 |
| 2 | DualSense | 054C / 0CE6 | 273 bytes, DualSense USB layout: input 0x01 64 bytes, output 0x02 48 bytes, feature ids 0x05, 0x08, 0x09, 0x0A, 0x20–0x22, 0x80–0x85, 0xA0, 0xE0, 0xF0–0xF2, 0xF4, 0xF5 | DualSense |
| 3 | DualSense Edge | 054C / 0DF2 | the **same** DualSense descriptor (no Edge-specific reports) | DualSense |

The driver derives the kind from the PID in its instance id [S].

### 3.4 Reference feedback (observable) [S]

The callback gets (pad handle, 8-byte record, user pointer), on a library thread, once
per output the driver passes on.

| Byte | Xbox | DualSense / Edge | DualShock 4 |
| --- | --- | --- | --- |
| 0 | actuator enable mask | 3 | 3 |
| 1, 2 | left / right trigger motor | 0 | 0 |
| 3 | strong (left) motor | strong motor | **weak** motor |
| 4 | weak (right) motor | weak motor | **strong** motor |
| 5–7 | duration, start delay, loop count | 0 | 0 |

Observable behaviour: only rumble reaches the stream server. For DualSense, a game's
output report that does not ask for a rumble change gives a zero record. For DS4 the two
motors arrive swapped, which looks like a defect [I]. Lightbar, player LEDs, mic LED,
adaptive triggers and audio haptics stay in the driver and are dropped [S].

### 3.5 Pulsar Split approach: pad lifecycle and control messages

Roles. The **service** (SYSTEM, session 0) is the only component that creates and
removes pads. The stream server (SYSTEM in the seat session, `80-stream-server.md` §2)
asks for pads and moves input and feedback through shared memory. It needs no SetupAPI
rights and loads no driver code.

Channel. The control messages use the existing seat control pipe of `80` §8.4: the
stream server is the pipe server, the service is the only client, and the framing and
envelope are as defined there. Because only the service sends requests, the stream
server asks for a pad with an **event**, and the service answers with a **request**.
`80-stream-server.md` must add these rows (this file is the source; 80 refers here).
`pulsar_pad` must appear in the `hello` capabilities.

| Direction | Message | Body | Meaning |
| --- | --- | --- | --- |
| server → service, event | `pad.wanted` | `slot` (u8, 0–15), `kind` (`ds4` / `ds5` / `ds5e` / `xone`), `client_type` (Moonlight controller type: `xbox` / `ps` / `nintendo` / `unknown`), `caps` (list: `rumble`, `trigger_rumble`, `touchpad`, `accel`, `gyro`, `battery`, `rgb_led`), `generation` (u32, server counter) | a client controller arrived in that slot |
| service → server, request | `pad.attach` | `slot`, `generation`, `pad_id` (string, §3.6), `kind`, `channel` (base object name, §4.5), `protocol` (1) | the pad exists and the driver has started. The server opens the objects and replies ok. If it cannot open them, it replies error `not_found` and the service removes the pad |
| service → server, request | `pad.denied` | `slot`, `generation`, `code` (`pad_unavailable` driver package missing or devnode failed to start; `limit` slot limit reached; `disabled` seat has pads off), `detail` | no pad for that slot. The server keeps forwarding nothing for the slot |
| server → service, event | `pad.released` | `slot`, `generation`, `reason` (`controller_removed` / `stream_ended` / `server_stopping`) | the client controller left, or the stream ended |
| service → server, request | `pad.detach` | `slot`, `generation`, `reason` (`released` / `seat_stopping` / `device_lost`) | the server closes its handles, then replies ok. The service removes the devnode only after the reply or after 2 s |

Rules:
- At most one live pad per (seat, slot). A `pad.wanted` for a slot that already has a
  pad of the same kind reuses it (re-attach, no new devnode). A different kind removes
  the old pad first.
- `generation` makes late messages harmless: the service ignores a `pad.released` whose
  generation is not the current one for that slot.
- On `stream_ended` the service keeps the pads for a linger time (default 30 s) so a
  reconnecting client gets the same devices. Games keep their handles that way. On
  `server_stopping`, seat stop, or pipe loss, the service removes all of the seat's pads.
- Feedback does **not** use the pipe. It uses the output ring of §4.5, which the server
  reads with low latency.
- Slot limit: 16 per seat (the Moonlight protocol's maximum). The service may impose a
  lower per-seat limit from settings.

### 3.6 Pulsar Split approach: devnode creation

Two methods were compared. The second is the fallback, and the choice is open until the
tests in §13 run.

| | **A. Software device** (`SwDeviceCreate`) — preferred | **B. Root devnode** (SetupAPI, fixed instance id) — fallback |
| --- | --- | --- |
| Instance id | `SWD\PulsarSplitPad\<device-id-part>` | `ROOT\PULSARPAD\<seat32>_<slot>` |
| Lifetime | the default handle lifetime: the devnode disappears when the service closes the handle **or the service process dies**. No orphans on a crash | persistent; the service must remove it, and phantoms remain after a crash |
| Properties at creation | yes: session id and the private properties (§3.7) are passed in the create call, so the driver sees them at its first start. No race | set between registering and installing the devnode |
| Async | yes, completion callback; the service waits up to 5 s | synchronous |
| Install | PnP picks the staged INF by hardware id; no per-pad install call | explicit device install per creation |
| Untested points | whether `DEVPKEY_Device_SessionId` may be supplied by the creator; whether `mshidumdf` and `WUDFRd` bind to a software device; the HID child id form | known to work (the reference uses it) |

Ids (both methods):
- `<seat32>` = seat GUID as 32 upper-case hex digits without braces or dashes. `<slot>` =
  2 hex digits.
- Device-id part = `VID_<vid>&PID_<pid>[&IG_00]&PULSARPAD&<seat32>&<slot>`. `IG_00` is
  present only for the Xbox kind, so the HID child id and path carry the `IG_` token that
  XInput-aware software looks for [I]. Length: at most 70 characters, well under the
  200-character device-id limit.
- Hardware ids: `PULSARPAD\VID_<vid>&PID_<pid>[&IG_00]` (matched by our INF), then
  `PULSARPAD\<kind>`. No compatible ids. The `PULSARPAD\` prefix keeps the reference's
  `ROOT\VID_…` INF from matching our devnodes, and the other way round.
- Because the id is fixed per (seat, slot), re-creating a pad gives the same devnode and,
  if the HID class keeps its naming (§2.2), the same HID child. Steam and DirectInput then
  see one stable device per slot instead of a new one each time.
- pad id (in `pad.attach` and logs) = `<seat32>-<slot>`.

### 3.7 Pulsar Split approach: device properties

Properties set by the service on the pad's devnode before the driver starts (method A:
in the create call; method B: before the device install):

| Key | Type | Value |
| --- | --- | --- |
| `DEVPKEY_Device_SessionId` `{83DA6326-97A6-4088-9453-A1923F573B29}`, 6 | `DEVPROP_TYPE_UINT32` | owner seat session id (standard key; the reference uses the same one [S][L]) |
| `PKEY_PulsarPad_Channel` `{6F1D3A52-8B47-4C9E-A0D3-5E2B7C914F08}`, 2 | `DEVPROP_TYPE_STRING` | channel base name (§4.5), at most 96 characters |
| `PKEY_PulsarPad_Seat` same GUID, 3 | `DEVPROP_TYPE_GUID` | seat id |
| `PKEY_PulsarPad_Slot` same GUID, 4 | `DEVPROP_TYPE_UINT32` | slot 0–15 |
| `PKEY_PulsarPad_Kind` same GUID, 5 | `DEVPROP_TYPE_UINT32` | 0 = xone, 1 = ds4, 2 = ds5, 3 = ds5e |
| `PKEY_PulsarPad_Protocol` same GUID, 6 | `DEVPROP_TYPE_UINT32` | shared-memory protocol version (1) |

The property GUID was generated for Pulsar Split; it is not taken from the reference.
The driver reads these properties at device add. If any is missing or the version is
unknown, it fails the device start, and PnP shows a start failure. It never falls back
to the registry.

---

## 4. Shared sections and events

### 4.1 Reference names and objects (per pad) [S]

| Name | Object | Created by | Size | Client access | Direction |
| --- | --- | --- | --- | --- | --- |
| `Global\Duo_<padId>_input` | page-file section | driver (WUDFHost) | 66 bytes | write | client → driver |
| `Global\Duo_<padId>_input_event` | event, auto-reset | driver | — | set | client → driver |
| `Global\Duo_<padId>_output` | page-file section | driver | 47 bytes | read | driver → client |
| `Global\Duo_<padId>_output_event` | event, auto-reset | driver | — | wait | driver → client |

`<padId>` = device instance id with `\` → `_`, e.g.
`ROOT_VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>_0000` [L][S].

Security descriptor of all four: generic read + write for **Authenticated Users**,
generic all for **Administrators** [S]. So any signed-in user in any session can feed
input into any seat's pad or read its feedback [I].

### 4.2 Reference input section (66 bytes) [S]

| Offset | Size | Content |
| --- | --- | --- |
| 0 | 1 | 0x02 (format marker) |
| 1 | 1 | 0x40 (length of what follows) |
| 2 | 64 | full HID input report including report id 0x01; the Xbox kind uses the first 62 bytes |

The driver ignores a record with another marker or length. There is no lock or sequence
number, so a report can tear [I].

### 4.3 Reference output section (47 bytes) [S]

| Offset | Size | Content |
| --- | --- | --- |
| 0 | 1 | report id of the last output written by a game (write-report or set-output-report) |
| 1 | ≤ 46 | report payload after the id, cut at 46 bytes |

One slot only: a newer output overwrites an older one before the client reads it [I]. A
DualSense output report has 47 payload bytes, so its last byte is lost [I]. Bytes past a
shorter report keep stale content [I].

### 4.4 Account of the driver host [S][I]

The driver runs in the default UMDF host as LOCAL SERVICE (§2.3). On the reference this
does not matter because the objects are open to Authenticated Users. Not checked live,
because no pad existed during the analysis and creating one needs the reference service
running [?].

### 4.5 Pulsar Split approach: shared-memory contract (protocol 1)

Who creates. The **service** creates all objects before the devnode exists and keeps
them until after the devnode is gone. The driver and the stream server only open them.
This removes the "wait until the driver created the sections" polling of the reference.

Names (base = `Global\PulsarSplit_Pad_<seat32>_<slot>`):

| Object | Name | Kind |
| --- | --- | --- |
| input section | `<base>_In` | page-file section, 4096 bytes |
| input event | `<base>_InEvt` | auto-reset event; set by the stream server after each input write |
| output section | `<base>_Out` | page-file section, 4096 bytes |
| output event | `<base>_OutEvt` | auto-reset event; set by the driver after each output entry |

DACL (protected, no inheritance), the same for all four:

| Trustee | Rights | Why |
| --- | --- | --- |
| SYSTEM | full | service and stream server (both SYSTEM) |
| LOCAL SERVICE (S-1-5-19) | section: read + write (map); event: synchronise + modify state | the UMDF driver host account (§2.3) |

No seat-user ACE and no Authenticated Users. Seat processes never touch these objects;
they only see the HID device, which `50-shim-isolation.md` and the device security of
§8.2 protect. Trade-off: any LOCAL SERVICE process could open the objects. To narrow this, the
service may verify after creation that only the expected `WUDFHost.exe` has a handle
open, but that is optional. A tighter ACE (a per-host-process SID) has no supported
mechanism for UMDF hosts [I].

All multi-byte fields are little-endian. Offsets are in bytes.

**Input section**

| Offset | Size | Field | Writer | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | magic | service | `PPIN` (0x4E495050) |
| 4 | 2 | version | service | 1 |
| 6 | 2 | header_size | service | 64 |
| 8 | 4 | kind | service | as `PKEY_PulsarPad_Kind` |
| 12 | 4 | seq | stream server | seqlock counter (below) |
| 16 | 2 | report_len | stream server | length of the input report including its id, 1–128 |
| 18 | 2 | flags | stream server | bit 0 = motion active (driver should keep reporting at the native rate, §6.3); other bits 0 |
| 20 | 4 | reserved | – | 0 |
| 24 | 8 | qpc | stream server | `QueryPerformanceCounter` value of the write |
| 32 | 32 | reserved | – | 0 |
| 64 | 128 | report | stream server | full HID input report, byte 0 = report id |

**Output section**

| Offset | Size | Field | Writer | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | magic | service | `PPOU` (0x554F5050) |
| 4 | 2 | version | service | 1 |
| 6 | 2 | header_size | service | 64 |
| 8 | 2 | entry_count | service | 16 |
| 10 | 2 | entry_size | service | 96 |
| 12 | 4 | write_index | driver | count of entries written so far, wraps at 2^32; entry slot = index mod 16 |
| 16 | 4 | dropped | driver | entries overwritten before the reader could have seen them (driver's view, informational) |
| 20 | 44 | reserved | – | 0 |
| 64 | 16 × 96 | entries | driver | ring |

Output entry (96 bytes):

| Offset | Size | Field | Meaning |
| --- | --- | --- | --- |
| 0 | 4 | seq | seqlock counter of this entry |
| 4 | 2 | len | payload length including report id, 1–80 |
| 6 | 1 | source | 0 = write report, 1 = set output report, 2 = set feature |
| 7 | 1 | reserved | 0 |
| 8 | 8 | qpc | time of the request |
| 16 | 80 | payload | report bytes, byte 0 = report id |

Maximum output sizes this must hold (USB layouts, with id): DualSense / Edge 0x02 = 48,
DualShock 4 0x05 = 32, Xbox 0x03 = 9. Bluetooth-format output reports are not emulated
(all pads appear as USB devices), so 80 bytes is enough. A longer request is truncated
and counted in `dropped`.

Seqlock rule (both sections). Before writing, the writer makes `seq` odd. After writing,
it makes `seq` even again (seq + 1 each time), with full memory barriers around both
steps. A reader copies the data and accepts it only if `seq` was even and unchanged
before and after the copy; otherwise it copies again. Input has a single writer (the
stream server) and output has a single writer (the driver), so no lock is needed. The
reader of the output ring keeps its own last index. If `write_index` moved more than 16
ahead, the reader skips to the newest 16 and treats the rest as lost.

Validation (driver and stream server): wrong magic, version, header size, entry size or
lengths out of range → the object is treated as absent. The driver then serves a neutral
report and logs once. Neither side ever trusts lengths without these checks.

---

## 5. Input reports

### 5.1 Reference behaviour (observable) [S]

The stream server passes a compact per-kind record (Xbox ≥ 13 bytes, DS4 30 bytes,
DualSense / Edge 63 bytes). The library turns it into the kind's public USB input
report. These records are private to the reference and are not part of any interface
Pulsar Split needs. Observable properties of the resulting HID reports:

| Kind | Property |
| --- | --- |
| Xbox | sticks signed 16-bit, triggers 10-bit, hat, 15 buttons, four back paddles; vendor bytes zero |
| DS4 | report counter, timestamp and touch-packet counter stay 0; battery/cable byte fixed (wired, charged); touch coordinates clamped to the DS4 touchpad range |
| DualSense / Edge | the sequence byte increments per report; both sensor timestamps are milliseconds, not the device's native unit [I]; battery defaults to a non-zero level and the plug state to "cable" when the client sent none |

The public USB report layouts of all four pads are documented by open-source drivers
(Linux `hid-playstation`, `hid-microsoft`/`xpad`; SDL's HIDAPI drivers) and are the
reference for Pulsar Split, not the reference binary.

### 5.2 Pulsar Split approach (input)

Define one internal, versioned pad-state structure in Rust (named button bits, signed
sticks, 16-bit triggers, motion with timestamp, up to two touch points, battery). Convert
it to each HID layout in one place, in the stream-server backend. Produce native-unit
timestamps (DS4: about 5.33 µs per tick; DualSense: about 0.33 µs per tick) and real
report and touch counters so SDL/Steam motion filtering works. Write our own report
descriptors from the public sources above.

### 5.3 Pulsar Split approach: Moonlight mapping

Moonlight fields and messages below are public (Moonlight / upstream Sunshine, GPL-3).

Kind selection for `gamepad = auto` (`80` §4.2 ids):

| Client controller | Chosen kind | Note |
| --- | --- | --- |
| type `ps` | `ds5` | DualSense matches modern PlayStation pads best |
| type `xbox`, `nintendo`, `unknown`, without touchpad and motion | `xone` | XInput path for most games |
| type not `ps`, but with `touchpad` or `gyro`/`accel` capability | `ds5` | keeps motion and touch; **differs from `80` §4.2**, which maps all non-PlayStation pads to `xone`. `80` should adopt this row |
| config forces a kind | that kind | features the kind cannot carry are logged once, as upstream does |

Client → pad:

| Moonlight input | DualSense / Edge | DS4 | Xbox |
| --- | --- | --- | --- |
| buttons, sticks, triggers | report fields | report fields | report fields |
| paddles / misc buttons | Edge: back buttons and Fn; DualSense: mic button for "misc" | touchpad click for "misc" (per config) | back paddles |
| touchpad events | touch points | touch points | ignored |
| motion events (accel, gyro) | sensor fields | sensor fields | ignored |
| battery state | battery and plug-state fields | battery/cable byte | ignored |

Pad → client (from the output ring):

| Game output | Moonlight message | Kinds |
| --- | --- | --- |
| motor strengths (strong / weak) | rumble | all (DS4 in correct order) |
| trigger motors | trigger rumble | Xbox |
| lightbar colour | RGB LED | DS4, DualSense, Edge |
| adaptive-trigger effects | adaptive triggers (newer Moonlight clients) | DualSense, Edge |
| player LEDs, mic LED, audio haptics | not forwarded (no Moonlight message) | — |
| sensor enable | the server asks the client for motion events at the pad's native rate when the pad is created with motion; it stops on `pad.detach` | DS4, DualSense, Edge |

---

## 6. Driver role (HID minidriver)

### 6.1 Reference device setup (observable) [S][L]

| Item | Value |
| --- | --- |
| Kind | from the PID in the instance id |
| HID serial string | the 32-hex suffix of the instance id, so unique per creation |
| Session tag | `DEVPKEY_Device_SessionId` (UINT32) on the root devnode, from the global registry value (§8.1) |
| Device interface | class `{3D877443-4DDA-4BAB-A3E2-DF607B30DE4D}` registered per pad; the class key exists empty on the live box [S][L] |
| HID attributes | VID 054C or 045E, the kind's PID, version 0x0001 |
| HID descriptor | HID 1.00, country 0, one report descriptor (§3.3) |
| Default input | a neutral report until the first input arrives |

### 6.2 HID requests handled by the reference [S]

| Request (HID minidriver IOCTL) | Code | Answer |
| --- | --- | --- |
| get device descriptor | 0xB0003 | the HID descriptor |
| get report descriptor | 0xB0007 | the kind's report descriptor |
| read report | 0xB000B | queued; completed with the current input report when new input arrives [I] |
| write report | 0xB000F | to the output section (§4.3) |
| get string | 0xB0013 | manufacturer (Sony or Microsoft), product name per kind (`Wireless Controller` for DS4), serial (§6.1) |
| get device attributes | 0xB0027 | VID / PID / version |
| set feature | 0xB0053 | accepted, not forwarded [I] |
| get feature | 0xB0057 | §6.3 |
| set output report | 0xB005B | to the output section |
| get input report | 0xB005F | current input state [I] |
| get indexed string | 0xB01E2 | as get string [I] |
| idle notification 0xB002B, physical descriptor 0xB019A, others | — | not supported |

### 6.3 Reference feature reports [S]

| Report id | Minimum buffer | Content |
| --- | --- | --- |
| 0x05 | 40 bytes | fixed motion calibration (DualSense layout) |
| 0x09 | 20 bytes | pairing info with a **fixed controller MAC address that is the same for every pad** |
| 0x20 | 64 bytes | firmware info with a fixed build date and version |
| other ids | ≥ 1 byte | zeros, report id in byte 0 |
| too-small buffer | — | buffer-too-small status |

The same answers serve every kind, so DS4 calibration (0x02), MAC (0x12/0x81) and
firmware (0xA3) read as zeros [S]. Software that divides by DS4 calibration ranges gets
no usable motion [I]. Because the MAC is the same, Steam and SDL may treat several pads
as one device [I].

### 6.4 Pulsar Split approach: driver

Language. Plan.md asks for Rust where possible. `windows-drivers-rs` (Microsoft,
MIT/Apache-2.0, GPL-compatible) has UMDF support, but it is young. Two options for the
maintainer: (a) Rust on `windows-drivers-rs` (preferred if a prototype loads under
`mshidumdf`); (b) C under GPL-3 written from the WDK documentation. Do **not** copy the
WDK `vhidmini2` sample: the Windows-driver-samples repository is under MS-PL (verify the
current LICENSE file), and the FSF lists MS-PL as incompatible with the GPL. Reading
public API documentation is fine.

Behaviour:
- Read the §3.7 properties at device add; fail start if they are invalid. Open the §4.5
  objects by name; fail start if they are missing.
- HID strings: manufacturer and product as real pads report them (interop identifiers);
  serial = per-pad MAC (below) as 12 hex digits.
- Read reports: complete each pending read when a new input arrives (seqlock-verified).
  When input flag bit 0 (motion active) is set, also complete reads at the native
  report rate with the last state, so counters and timestamps keep advancing. The driver
  fills in the counters and timestamps for that case. Without motion, send only changes,
  plus one repeat every 100 ms as a keepalive [I] (see §13 for the timing check).
- Write report, set output report and set feature: append to the output ring, then set
  the output event.
- Feature reports per kind, from the public layouts (sizes include the report id):

| Kind | Id | Size | Content |
| --- | --- | --- | --- |
| DS4 | 0x02 | 37 | motion calibration with plausible non-zero ranges |
| DS4 | 0x12 | 16 | pairing info: pad MAC, then a host MAC |
| DS4 | 0x81 | 7 | pad MAC |
| DS4 | 0xA3 | 49 | firmware/hardware version block |
| DualSense, Edge | 0x05 | 41 | motion calibration with plausible non-zero ranges |
| DualSense, Edge | 0x09 | 20 | pairing info: pad MAC (bytes 1–6) |
| DualSense, Edge | 0x20 | 64 | firmware info (build date, hardware and firmware versions) |
| DualSense Edge | profile/extra ids | [?] | not described in the public sources consulted; answer zero-filled with the id, as other unknown ids |
| any | unknown id | requested size | zeros with the report id |

- MAC per pad: 6 bytes from a hash (SHA-256) of the seat GUID, slot and a per-host random
  salt stored by the service. Then set the locally-administered bit (0x02) and clear the
  multicast bit (0x01) of the first octet. Stable per (host, seat, slot), different
  across pads. MAC byte order inside each report follows the public drivers (reversed in
  the PlayStation reports) [I].
- Edge: own report descriptor from public sources, PID 0DF2.

---

## 7. ViGEmBus

| Item | Observation |
| --- | --- |
| Package | `assets\drivers\ViGEmBus\ViGEmBus.{inf,sys,cat}` 1.21.442.0 (2022-08-30), Microsoft-signed (WHQL), class System, plus `nefconw.exe` [S][L] |
| On the live box | service `ViGEmBus`, kernel, system start, running; devnode `ROOT\SYSTEM\0006`; separate MSI product 1.22.0 [L] |
| Use in 1.6 | ViGEm client code still linked in the stream server [S]. The pad-type choice offers Xbox One, DS4, DualSense and Edge. Which of them still goes through ViGEm is [?] (`80` §4.1) |
| Session behaviour | ViGEm pads carry no session property and every session sees them [I] |

**Pulsar Split approach (ViGEm).** This file recommends **not** using ViGEmBus in any
phase: it is a third-party kernel driver, end-of-life upstream, and its pads are global.
`80-stream-server.md` §4.2 currently keeps upstream's ViGEm path for Phases 2–3. That
conflict is listed in §12 for a maintainer decision. Pulsar Split never installs
ViGEmBus in either case.

---

## 8. Pad ↔ session ownership

### 8.1 Reference model

| Aspect | Behaviour |
| --- | --- |
| Owner | the session of the process that created the pad (the seat's stream server) [S] |
| Hand-over | one global registry value, written just before the driver install and deleted just after [S] |
| Marking | `DEVPKEY_Device_SessionId` on the root devnode [S]. **The HID child carries the same value**: all nine phantom children on the live box have `DEVPKEY_Device_SessionId` = 2 [L]. The driver writes the root only, so PnP probably copies the value from parent to child [I] |
| Visibility | the per-process shim filters HID devices by session (`50-shim-isolation.md`) [S]. Whether Windows itself hides session-tagged HID devices from other sessions could not be tested: it needs a live pad, and the reference service was not started [?] |
| Lifetime | tied to the stream server's remove call; if that process dies the devnode stays until service cleanup (§9) [S][I] |
| Weaknesses | (1) two seats creating pads at the same moment share the one registry value, so a pad can get the wrong session [I]; (2) Authenticated-Users ACL (§4.1) [S]; (3) the caller must be elevated [S] |

### 8.2 Pulsar Split approach (ownership)

- The service is the only creator. It keeps `{pad id, seat, slot, session id, seat user
  SID, kind, generation}` in memory and persists nothing global.
- Session id and private properties go onto that exact devnode at creation (§3.7), so
  there is no shared value and no race. If the child does not inherit the session id
  under method A, the service sets it on the HID child as soon as the child appears.
  It watches for the child through device-interface arrival notifications.
- Device security: the service sets a security descriptor on the HID child (documented
  device-security route of `50-shim-isolation.md` §4.4) that grants
  open access to SYSTEM, Administrators and the **owning seat user's SID** only. This
  is a change of trust boundary: seat users get access to their own pad device,
  nothing else. It needs to be recorded in plan.md (§12).
- Pads are removed when the seat's session ends, when its stream server exits (pipe
  loss), on `pad.detach`, and (method A) automatically if the service dies.

---

## 9. Orphan devnodes and cleanup

| Observation | Ev. |
| --- | --- |
| Pad removal: a global-scope device removal, then the library waits until the devnode is gone (about 10 s maximum) | [S] |
| Removing the root leaves the HID child as a non-present device | [L] |
| Live box: nine phantom `HID\VID_054C&PID_0CE6&DUOCONTROLLER&<32hex>\1&…` children (non-present, parent now shown as the root of the tree), last removal 2026-11-11/12; no root pad nodes present | [L] |
| Service cleanup at start and stop: every device, present or not, whose instance id contains `DUOCONTROLLER` is removed; then every `oem*.inf` of the pad class is force-uninstalled | [S][L: the log shows the per-child "Delete Device" entries] |
| A new suffix per pad means new HID child ids, new joystick entries for DirectInput/Steam and new `Enum` keys on every creation, until cleanup | [L][I] |

**Pulsar Split approach (cleanup).**
- Marker: enumerator `PulsarSplitPad` (method A) or `ROOT\PULSARPAD` (method B) for the
  root. For HID children, the device-id part contains `&PULSARPAD&` (copied from the
  parent, §2.2). Cleanup matches only these exact tokens, never `DUOCONTROLLER` or
  anything else, so the reference product and other products are not touched.
- Explicit pad removal: list the HID children of the pad devnode (configuration-manager
  child enumeration, while the parent is still present), remove the parent, then remove
  each child that is now non-present.
- Service start and stop: enumerate all devices including non-present ones, and remove
  those carrying the marker that the service does not own right now. With method A
  this is rarely needed (handle lifetime). With method B it is required.
- Never uninstall the INF on service stop; only the uninstaller does that.
- Log every removal with its instance id.

---

## 10. Bluetooth user-service handling

### 10.1 Problem [I]

Each signed-in user gets a per-user instance of the Bluetooth support service
(`BluetoothUserService_<suffix>`). With several seats signed in, the instances in remote
sessions can handle pairing prompts and take controllers that belong to the person at
the physical console.

### 10.2 Live configuration [L]

| Item | Value |
| --- | --- |
| Template `BluetoothUserService` | service type 0x60 (user service, shared process); start = demand; host group `BthAppGroup`; no `UserServiceFlags` value; recovery actions set; four start triggers (Bluetooth port device-interface arrival and three custom system-state events) |
| Instance `BluetoothUserService_<suffix>` | type 0xE0 (the template type + user-service-instance bit); same triggers and recovery actions; running in session 1 (the console user) at the time of the probe |
| Instance ↔ session | the instance's registry key has no session field. The suffix is per logon session and is shared by all per-user services of that logon. Only a running instance has a process id |
| Template `Parameters\ServiceDll` | points to the reference's wrapper DLL under Program Files |

### 10.3 Reference mechanism [S][L]

| Step | Behaviour |
| --- | --- |
| Install (service start) | takes ownership of the template's `Parameters` key, adds a full-control ACE for LocalSystem, sets `ServiceDll` (REG_EXPAND_SZ) to the wrapper |
| Restart instances | stops every running `BluetoothUserService_*` instance so it restarts with the wrapper |
| Uninstall (service stop) | sets `ServiceDll` back to the System32 Microsoft DLL; the ownership and ACL changes stay [S][I] |
| Wrapper | loads the real Microsoft DLL from System32 and forwards the two service entry points only when the hosting process's session is the active console session; otherwise the host process ends itself with exit code 0 |

Side effects [I]: the wrapper decides once, at service start. After a console switch,
instances keep the old decision until they restart. The self-termination looks like a
failure to the SCM and may trigger the recovery actions [L: recovery actions present].
Windows updates can reset the key. Self-signed code is loaded into a Microsoft svchost
group.

### 10.4 Pulsar Split approach (Bluetooth)

Do not replace a Microsoft service DLL. Do not change ownership or ACLs of system
service keys. Instead:
- Mapping instance → session: when a seat session logs on (service session-change
  notification), wait until any process of that session hosts a per-user service. Read
  the `_<suffix>` of that service's name. That suffix names all per-user services of the
  logon, including `BluetoothUserService_<suffix>`, even while it is stopped. Keep a
  suffix → session map; drop it at logoff.
- For each seat suffix (never the console session's suffix): set that **instance's**
  start type to disabled through the SCM configuration call, then stop it if it is
  running. Per-user instances are created at logon and deleted at logoff, so the change
  does not outlive the seat logon [I]. It also stops the trigger starts, which a plain
  stop would not do [I].
- Console user who is also a seat user: the console session's instance is left alone.
  Only instances whose suffix maps to a seat session (not the console session) are
  disabled. If a user moves from seat to console, the service re-enables that suffix's
  instance (start = demand) on the console-connect notification.
- Re-check every 60 s and on every session-change notification. Log each action.
- A per-host toggle in the manager, default on.
- Global kiosk alternative (documented by Microsoft for per-user services): template
  start = disabled with `UserServiceFlags` = 0 stops per-user instances for everyone.
  This is not suitable when the console user needs Bluetooth, so it is not the default.

---

## 11. Signing and Memory Integrity

| Fact | Ev. |
| --- | --- |
| Reference host state: virtualization-based security running, but **no security services running** (Memory Integrity off, policy value 0) and **Secure Boot off** | [L] |
| So the live box does not show whether a self-signed UMDF package installs and loads with Memory Integrity on | [L] |
| Memory Integrity (HVCI) enforces code integrity for **kernel-mode** code. All kernel components of the pad stack are inbox Microsoft binaries (`WUDFRd`, `mshidumdf`, `xinputhid`, HID class). The pad driver itself is a user-mode DLL in WUDFHost, so HVCI does not apply to it directly | [I] (Microsoft documentation) |
| The PnP driver-package install still requires a catalog signature that chains to a root the machine trusts for driver install, independent of HVCI | [I] |

**Pulsar Split approach (signing).** Ranked options:
1. Microsoft attestation signing through the Hardware Dev Center. It needs an EV
   code-signing certificate (paid) and installs cleanly everywhere. This is the target
   for releases.
2. Development only: test-signing on the maintainer's machines.
3. A per-machine self-signed root in the Root and TrustedPublisher stores (what the
   reference does). This weakens trust on the host, must be disclosed, and must
   **not be the default** until a test on a machine with Memory Integrity and Secure Boot on
   shows it works. If it is ever offered, generate the key at install time, sign
   locally and destroy the private key.

Before Phase 4 starts, run the test in §13 #1. If option 1 is not affordable and
option 3 fails under Memory Integrity, Phase 4 pads cannot ship under the rules.

---

## 12. Decisions needed (maintainer)

| # | Topic | Options | Recommendation |
| --- | --- | --- | --- |
| 1 | ViGEm in Phases 2–3 (`80` §4.2) vs none (this file) | keep upstream's ViGEm path with a warning / no pads until Phase 4 | no ViGEm; if kept, only when the user installed it themselves, never shipped |
| 2 | Driver language | Rust `windows-drivers-rs` / C from documentation | Rust prototype first; C if it does not load |
| 3 | Signing route | attestation (paid) / self-signed root | attestation for releases |
| 4 | Seat-user SID on the pad HID device (§8.2) | add to plan.md / shim-only isolation | add, with the scope "own pad device only" |
| 5 | `auto` mapping for non-PlayStation pads with motion/touchpad (§5.3) | `ds5` / `xone` | `ds5`; `80` §4.2 to follow |
| 6 | Devnode method (§3.6) | software device / root devnode | software device if the §13 tests pass |

---

## 13. Open questions and tests

| # | Question | How to answer | Ev. |
| --- | --- | --- | --- |
| 1 | Does a self-signed UMDF HID package install and load with Memory Integrity and Secure Boot on? | test VM or second PC with both on; install a minimal own package; needs maintainer approval | [?] |
| 2 | Does Windows hide session-tagged HID devices from other sessions by itself? | with a live pad: open its HID path from the console session and from another seat | [?] |
| 3 | Does PnP copy `DEVPKEY_Device_SessionId` to the HID child, or does something else set it? | create a pad and read the property on root and child during its life | [L: child has it] mechanism [?] |
| 4 | Read-report timing of the reference (immediate with last state, or only on new input) | HID read timing against a live reference pad | [?] |
| 5 | Xbox kind: HID child hardware ids; whether XInput reports the pad in the seat; duplicate in SDL because there is no `IG_` token in the path | inspect a live Xbox pad; `XInputGetState` in the seat | [?] / [I] |
| 6 | Meaning of `DevicePropertyFlags` = 4 in the device hardware key | public documentation search; compare with real Xbox HID pads | [?] |
| 7 | Method A: can the creator set `DEVPKEY_Device_SessionId`; do `WUDFRd`/`mshidumdf` bind to a software device; HID child id form | prototype on the Pulsar test machine (approval needed) | [?] |
| 8 | DualSense Edge extra feature/profile reports that Steam requests | HID trace of a real Edge pad, or public sources | [?] |
| 9 | Whether changing a per-user service instance's start type survives within the logon and blocks trigger starts | test on a seat logon | [I] |
| 10 | Which `gamepad` ids still go through ViGEm in the reference | `80` §13 #1 | [?] |
| 11 | WUDFHost account for this driver, checked live | process token of the WUDFHost hosting a live pad | [I: LOCAL SERVICE] |
