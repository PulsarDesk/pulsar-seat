# 80 — Per-seat stream server

How the reference product runs one game-stream server per seat, and how that server
differs from upstream Sunshine in what can be observed: config keys, capture
backend, controller backend, app lists, pairing / PIN, state and credential files,
ports, discovery, firewall and web UI. Each mechanism ends with a **Pulsar Split
approach**. Evidence markers as in `00-architecture.md`: **[S]** static, **[L]** live
(read-only), **[I]** inferred, **[?]** unknown, plus **[U]** = public upstream source or
documentation. No code, no decompiled material.

Related specs: service seat lifecycle and config-file generation in `20-service.md`
§3–§6; the frame source in `40-display-hdr.md` (pending; the Pulsar-defined frame
channel contract is in §3.4 of this file until 40 exists); virtual pads in
`60-controllers.md` (pending); audio in `70-audio.md`.

"Upstream" means the public LizardByte Sunshine project (GPL-3). Apollo is a public
GPL-3 fork of Sunshine and is discussed only as a design option.

Revision 2 (2026-11-12): reworked after review. Adds seat-id plumbing, the frame channel
contract, the seat control-pipe protocol, display-mode handling, discovery, TLS
identity decision, base pin and phase ordering. The web-UI fallback is removed.

---

## 0. Decisions fixed by this spec (Pulsar Split)

| Topic | Decision |
| --- | --- |
| Upstream base | Sunshine tag `v2026.516.143833` (commit `14ffa6fdaa53f7b51512be2b3d24f3939695403c`). This is the upstream build that runs on the test PC [L]. Rebase to newer tags deliberately, never track a branch |
| Toolchain | upstream's Windows build: MSYS2 **UCRT64** environment, CMake + Ninja, GCC [U]. Build on an x64 CI runner |
| Host architecture | **x64 only.** Upstream ships no Windows ARM64 build [U], and the multi-seat host is the x64 test PC. ARM64 hosts are out of scope |
| Seat id | a lowercase GUID without braces (36 characters), created by the service at seat creation and never changed. Renaming a seat does not change it |
| Seat-id key | `pulsar_seat_id = <seat-id>` in the seat config file. The fork's Pulsar code reads only this key. It never derives the seat from the session id |
| Names that carry the seat id | frame channel objects (§3.4), seat control pipe (§8.4), firewall rules (§9.2), seat directory (§2.3) |
| Manager ↔ service | the service named pipe only (`20-service.md`). No HTTP, no loopback web API, at any phase |
| Browser web UI | never started (`pulsar_web_ui = disabled`, §8.4). Base+1 is never bound and never opened |
| Fork patch set | four isolated patches on upstream: (1) `pulsar_idd` capture backend, (2) seat control pipe + web UI off, (3) pairing timeout, (4) `pulsar_pad` gamepad backend (Phase 4). Upstream backends stay in the build |

## 1. Identity of the bundled server

| Item | Reference behaviour |
| --- | --- |
| Binary | `sunshine.exe` in the install folder, about 36 MB, x64, MinGW/GCC build (libstdc++ symbols, boost, FFmpeg, OpenSSL, x264/x265, Opus statically linked) [S] |
| Base | a vendor fork of upstream Sunshine [S]. The config-key table and the web-UI bundle match upstream's 2025 line: display-device (`dd_*`) keys, `max_bitrate`, `minimum_fps_target` and `stream_audio` are present, and `system_tray` is absent [S][I]. The exact upstream commit is [?] |
| Version reported | the server logs a version of the form `0.0.0.<git short hash>` plus vendor publisher fields [L] (2024 seat log). Moonlight sees the normal upstream `appversion` / `GfeVersion` fields in `/serverinfo` [S] |
| Process count | one process per running seat, each in its seat's Windows session, each with its own config file [S][L] |
| Start | started by the seat service with one argument, the relative path `config\<seat>.conf`. The working directory is the install folder [S]. It runs with a duplicated SYSTEM token whose session id is set to the seat session (`20-service.md` §6.2) |
| Upstream Windows service | not used. The fork is never registered as `SunshineService`. On the live box an upstream Sunshine 2026.516 also runs as `SunshineService` and listens on 0.0.0.0 TCP 47984, 47989, 47990, 48010 [L]. Both coexist |
| Upstream shortcut guard | the upstream refusal of a manual start is still in the fork. It does not trigger when the service launches the process [S][I] |

## 2. Per-seat configuration file

### 2.1 What the reference service writes

The service regenerates the whole file at every seat start (`20-service.md` §4). The
format is upstream's plain `key = value` lines [L][S].

| Key | Value | Upstream default [U] | Effect |
| --- | --- | --- | --- |
| `min_log_level` | a level word (observed `none`), from the global `Verbosity` value (0–4 maps to none / error / warning / info / debug) [I] | `info` | log level |
| `log_path` | `<seat>.log` (relative) | `sunshine.log` | ends up as `config\<seat>.log` [L] |
| `file_apps` | `<seat>_apps.json` | `apps.json` | app list (§5) |
| `file_state` | `<seat>_state.json` | `sunshine_state.json` | pairing state (§6) |
| `credentials_file` | `<seat>_state.json` (same file) | same file as `file_state` | web-UI login |
| `sunshine_name` | seat `DisplayName`; a sandbox seat gets a ` (VM)` suffix unless `HideVMSuffix` is set [S] | computer name | host name in Moonlight and the mDNS instance name (§7.3) |
| `port` | seat `Port` (observed 17994, 50162, 54062) | 47989 | base of the port block (§7) |
| `install_steam_audio_drivers` | `disabled` | `enabled` | never installs Steam's virtual speakers |
| `virtual_sink` | `Remote Audio` | empty | capture from the RDP session's audio endpoint |
| `minimum_fps_target` | `1` | `0` (automatic) | static desktops can fall to about 1 fps |

Missing keys take the fork's defaults: `capture` (§3), `gamepad` (`auto`, §4),
`encoder` / `adapter_name` (auto), `origin_web_ui_allowed` (upstream default `lan`),
`upnp` (off), `address_family` (`ipv4`), `pkey` / `cert` (default relative paths, §6.1)
[S][I]. A 2024 seat file lacks `min_log_level` and `minimum_fps_target`; the key set
grew between versions [L].

### 2.2 Keys whose meaning the fork changes

The fork's config parser has **no new key names** compared with upstream's 2025 list
[S]. It changes the meaning of two keys:

| Key | Upstream values on Windows [U] | Fork |
| --- | --- | --- |
| `capture` | `ddx` (DXGI desktop duplication), `wgc` (Windows Graphics Capture), empty = auto | `ddx` is reused for the private frame source that reads the display module's shared section; upstream desktop-duplication and graphics-capture log strings are gone. Empty = the private source [S][I] |
| `gamepad` | `auto`, `x360`, `ds4` (all ViGEmBus) | `auto`, `ds4`, `ds5`, `ds5e`, `xone` (§4); upstream's Linux-only ids `ds5` / `xone` reused on Windows [S] |

The active web-UI bundle has no capture selector. An older leftover bundle has one
selector with the single option `ddx` [S].

### 2.3 Pulsar Split approach

- Seat directory: `%ProgramData%\PulsarSplit\seats\<seat-id>\` holding `server.conf`,
  `apps.json`, `state.json`, `server.log` and `credentials\`. ACL: protected (no
  inheritance), SYSTEM full, Administrators full, nobody else. SDDL
  `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`.
- Regenerate `server.conf` from the seat record at every start.
- **All file keys get absolute paths**: `log_path`, `file_apps`, `file_state`,
  `credentials_file`, `pkey`, `cert`. Do not depend on how upstream resolves relative
  paths (working directory or config folder).
- Keys always written:

| Key | Pulsar value |
| --- | --- |
| `pulsar_seat_id` | seat id (§0) |
| `sunshine_name` | seat display name (unique across seats, §7.3) |
| `port` | seat base port (§7.2) |
| `file_apps`, `file_state`, `credentials_file`, `log_path`, `pkey`, `cert` | absolute paths in the seat directory; `credentials_file` = same path as `file_state` (§8.4) |
| `min_log_level` | seat setting, default `info` |
| `capture` | Phase 2: per §3.3 decision gate; Phase 3+: `pulsar_idd` |
| `dd_configuration_option` | `disabled` (§3.5) |
| `gamepad` | Phase 2–3: `ds4` or `x360`; Phase 4: Pulsar ids (§4.2) |
| `virtual_sink` | per `70-audio.md` |
| `install_steam_audio_drivers` | `disabled` |
| `minimum_fps_target` | seat setting, default `1` |
| `upnp` | `disabled` |
| `origin_web_ui_allowed` | `pc` (defence in depth; the listener is off anyway) |
| `pulsar_web_ui` | `disabled` |
| `pulsar_pair_timeout` | `120` (seconds, §8.4) |

- New backend ids for Pulsar code (`pulsar_idd`, `pulsar_ds4`, …). Never reuse an
  upstream id with another meaning.
- Optional advanced settings in the manager: encoder, bitrate cap,
  `minimum_fps_target`, HEVC/AV1 mode, gamepad type, audio sink. Everything else keeps
  the upstream default.

## 3. Capture backend for the seat display

### 3.1 Reference behaviour

```
 seat session N
 +--------------------------------------------------------------+
 | WUDF host: display module -> session N's virtual monitor     |
 |   writes frame ->  section  Global\DuoIdd<N>                  |
 |   signals      ->  event    Global\DuoIdd<N>FrameReady        |
 |                                                              |
 | sunshine.exe (session N)                                     |
 |   waits FrameReady, reads section, converts/encodes          |
 |   signals      ->  event    Global\DuoIdd<N>FrameEncoded      |
 +--------------------------------------------------------------+
```

- `<N>` is the decimal session id. The server gets its session id from its process id
  and opens the objects by name [S][I] (imports `ProcessIdToSessionId`,
  `OpenFileMappingA`, `MapViewOfFile`, `OpenEventA`). It never creates them. If they
  are missing the seat has no picture and the service treats the seat as failed
  (`20-service.md` §9).
- Colour handling is upstream's: SDR Rec.709 up to HDR10 PQ / Rec.2020 and FP16
  scRGB, switched as the client asks [S].
- Mode changes use upstream's display-configuration path (`QueryDisplayConfig` /
  `SetDisplayConfig` and the display-device library) against the seat monitor [S][I].
- 2024 seat log [L] (older product version): the upstream helper `ddprobe.exe` is run
  per GPU before capture. GPU 1 returns `0x887A0026` (DXGI "access lost"), GPU 2
  returns success; output `\\.\DISPLAY1` inside the seat session; a client request
  switches capture from 1920x1080 to 1280x720; refresh reported 30 Hz. So the 2024
  build captured the seat through a desktop-duplication-style path on the second GPU.
- 1.6: `tools\ddprobe.exe` is still referenced but the package ships no `tools`
  folder [S]. What 1.6 does without it is [?] (no 1.6 seat log exists on the box; the
  current seat logs are 0 bytes [L]).
- Section byte layout is [?] and not needed; Pulsar defines its own (§3.4).

### 3.2 Can a stock server capture a seat?

A seat is a loopback RDP session whose monitor is the per-session Microsoft Remote
Display Adapter (RdpIdd, one device per session, seen live as
`SWD\REMOTEDISPLAYENUM\RDPIDD_INDIRECTDISPLAY&SESSIONID_000N`) [L].

- 1.6 replaced the upstream capture backends [S], which suggests that upstream capture
  is not reliable in that setting [I].
- The 2024 log (§3.1) shows that desktop duplication did return frames in a seat
  session on one GPU in an older build [L]. Whether this needed the reference's
  display module, and whether upstream `ddx` or `wgc` works unmodified there, is [?].

### 3.3 Phase ordering (Phase 2 vs Phase 3)

`plan.md` puts the per-seat streaming host in Phase 2 and the IDD in Phase 3. This
spec resolves it with a **decision gate** at the start of Phase 2:

1. **Experiment (needs maintainer approval on the test PC):** in a loopback-RDP seat
   session, run an unmodified upstream build of the pinned tag twice, once with
   `capture = ddx`, once with `capture = wgc`, started the way the service starts it
   (SYSTEM token, session id = seat session). Repeat with the RDS policy "use the
   hardware graphics adapter for all Remote Desktop Services sessions" on and off.
   Record: encoder probe result, `ddprobe` results, first-frame success, frame rate,
   behaviour on lock / UAC.
2. **If one combination streams:** Phase 2 ships that upstream backend (SDR only,
   session-native resolution, no per-client mode changes). Phase 3 adds `pulsar_idd`.
3. **If none streams:** Phase 2 delivers the full server plumbing (config,
   pairing, control pipe, ports, firewall, supervision) and the stream is limited to
   the console seat. The `pulsar_idd` backend and a minimal SDR IDD move into Phase 2;
   HDR stays in Phase 3. This needs a `plan.md` change by the maintainer.

Either way the plumbing in §5–§10 is Phase 2 work and does not depend on the capture
path.

### 3.4 Pulsar Split approach: the `pulsar_idd` frame channel (Pulsar-defined)

The Pulsar IDD (`40-display-hdr.md`) is the **producer**. It runs inside a WUDF host
process. The `pulsar_idd` backend in the stream server is the **consumer**.

**Identities.** UMDF hosts for IddCx drivers run as `NT AUTHORITY\LOCAL SERVICE` in
session 0. On the test PC every WUDF host, including one for a third-party IddCx
virtual display, runs as LOCAL SERVICE [L]. The consumer runs as SYSTEM in session N.

```
 session 0                                  session N
 +------------------------------+           +-------------------------------+
 | WUDFHost (LOCAL SERVICE)     |           | stream server (SYSTEM)        |
 |  Pulsar IDD                  |           |  pulsar_idd backend           |
 |   creates control section ---+--name---->|   opens section, reads LUID   |
 |   creates texture ring       |           |   duplicates ring handles     |
 |   (shared NT handles)    <---+--dup------+   from producer pid           |
 |   copies each IddCx frame    |           |   keyed mutex, encode on LUID |
 |   sets FrameReady -----------+---------->|   waits FrameReady            |
 |   reads mode request    <----+-----------+   writes mode request         |
 +------------------------------+           +-------------------------------+
```

**Kernel object names** (all in the `Global\` namespace; a kernel object name may not
contain a backslash after the namespace prefix, so the parts are joined with `_`):

| Object | Name | Creator | Kind |
| --- | --- | --- | --- |
| control section | `Global\PulsarSplit_Seat_<seat-id>_Ctl` | producer | file mapping, 4096 bytes (only the first 256 are defined) |
| frame ready | `Global\PulsarSplit_Seat_<seat-id>_FrameReady` | producer | auto-reset event, set after each published frame |
| frame consumed | `Global\PulsarSplit_Seat_<seat-id>_FrameConsumed` | producer | auto-reset event, set by the consumer when it releases a slot (optional for the producer to wait on) |
| mode request | `Global\PulsarSplit_Seat_<seat-id>_ModeRequest` | producer | auto-reset event, set by the consumer after writing a mode request |

The producer learns the seat id from the seat's device instance (the service passes it
when it creates the seat monitor; detail in `40-display-hdr.md`). The `Global\`
create needs `SeCreateGlobalPrivilege`, which LOCAL SERVICE holds [I].

**Security descriptor of every object:** `O:LSD:P(A;;GA;;;LS)(A;;GA;;;SY)`: the owner
LOCAL SERVICE (producer), SYSTEM full (all seat servers and the service). No access
for seat users, Administrators or Authenticated Users. Isolation between seats rests
on the seat id in the name only: every seat server runs as SYSTEM, so a misbehaving
server could open another seat's objects. This is a correctness separation, not a
security boundary. A future hardening option is to run each seat server with a
restricted token that carries a per-seat SID and to grant that SID instead of SYSTEM;
it is out of scope until upstream runs correctly under such a token [?].

**Control section layout, version 1** (little-endian, all fields naturally aligned):

| Offset | Size | Field | Writer | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | magic | P | ASCII `PSCF` |
| 4 | 2 | version | P | 1 |
| 6 | 2 | header_size | P | 256 |
| 8 | 4 | session_id | P | seat session id (informational only) |
| 12 | 4 | flags | P | bit 0 producer running, bit 1 HDR active, bit 2 ring valid |
| 16 | 4 | adapter_luid_low | P | render adapter LUID, low part |
| 20 | 4 | adapter_luid_high | P | render adapter LUID, high part (signed) |
| 24 | 4 | producer_pid | P | process id of the WUDF host |
| 28 | 4 | ring_count | P | 2–4 |
| 32 | 32 | ring_handles[4] | P | 4 × u64: NT handle values of the shared textures, valid in the producer process |
| 64 | 4 | width | P | pixels |
| 68 | 4 | height | P | pixels |
| 72 | 4 | format | P | 1 = `B8G8R8A8_UNORM`, 2 = `R10G10B10A2_UNORM`, 3 = `R16G16B16A16_FLOAT` |
| 76 | 4 | colour_space | P | numeric `DXGI_COLOR_SPACE_TYPE` (0 = sRGB/Rec.709 G2.2, 1 = scRGB linear, 12 = HDR10 PQ Rec.2020) |
| 80 | 8 | frame_counter | P | u64, +1 per published frame, never wraps in practice |
| 88 | 4 | latest_index | P | ring slot of the newest frame |
| 92 | 4 | generation | P | +1 whenever the ring is recreated (mode or format change); the consumer reopens all handles |
| 96 | 8 | producer_heartbeat_ms | P | `GetTickCount64` value, refreshed at least every 500 ms |
| 104 | 8 | consumer_heartbeat_ms | C | same, written by the consumer |
| 112 | 4 | refresh_mhz | P | current refresh in millihertz (60000 = 60 Hz) |
| 116 | 4 | reserved | – | 0 |
| 120 | 16 | hdr_primaries | P | 8 × u16: red x,y, green x,y, blue x,y, white x,y in units of 0.00002 (same scale as `DXGI_HDR_METADATA_HDR10`) |
| 136 | 4 | hdr_max_mastering | P | 0.0001 nit units |
| 140 | 4 | hdr_min_mastering | P | 0.0001 nit units |
| 144 | 2 | hdr_max_cll | P | nits |
| 146 | 2 | hdr_max_fall | P | nits |
| 148 | 4 | hdr_valid | P | 1 when the HDR fields are meaningful |
| 152 | 4 | req_seq | C | +1 per mode request |
| 156 | 4 | req_width | C | pixels |
| 160 | 4 | req_height | C | pixels |
| 164 | 4 | req_refresh_mhz | C | millihertz |
| 168 | 4 | req_flags | C | bit 0 HDR wanted |
| 172 | 4 | ack_seq | P | `req_seq` that was last handled |
| 176 | 4 | ack_status | P | 0 applied, 1 mode not supported, 2 HDR not available on this OS / GPU, 3 busy, 4 internal error |
| 180 | 76 | reserved | – | 0 |

P = producer, C = consumer. Each side writes only its own fields. Multi-field groups
(size/format/ring, HDR block) are written before `generation` or `frame_counter` is
incremented, with a release barrier; the consumer reads the counter, then the fields,
then the counter again and retries on a change.

**Texture ring.**

- The producer creates `ring_count` D3D11 textures on the render adapter, each with
  the `SHARED_NTHANDLE` and `SHARED_KEYEDMUTEX` flags, size and format as in the
  header, and an NT handle per texture with the same security descriptor as above.
- For each frame that IddCx hands over, the producer acquires a free slot's keyed
  mutex with key 0, copies the frame, releases it with key 1, updates
  `latest_index` and `frame_counter`, and sets FrameReady. If no slot is free it
  overwrites the oldest key-1 slot that the consumer has not acquired (newest frame
  wins; frames are dropped, never queued).
- The consumer reads `producer_pid` and the handle values, opens the producer
  process with `PROCESS_DUP_HANDLE` and duplicates the handles into itself
  (`DuplicateHandle`), then opens them with `ID3D11Device1::OpenSharedResource1`.
  SYSTEM can open a LOCAL SERVICE process for handle duplication [I]. Fallback if
  that fails: the producer also names each handle (`IDXGIResource1::CreateSharedHandle`
  with a name) and the consumer opens it by name
  (`OpenSharedResourceByName`). Whether such names resolve across sessions from
  session 0 is [?]; test it before relying on the fallback.
- The consumer acquires key 1 on `latest_index`, copies or converts into its own
  encoder texture, releases with key 0, and sets FrameConsumed.
- Dirty semantics: IddCx delivers a frame only when the desktop changed, so an
  unchanged `frame_counter` means no change. The consumer re-encodes its last
  frame to meet `minimum_fps_target`.

**Adapter.** The backend creates its D3D11 device and encoder on the adapter whose
LUID is in the header. If the config also sets `adapter_name` and it names another
adapter, the backend ignores it and logs a warning. If no adapter with that LUID can
be opened, the backend fails to start and reports `adapter_mismatch` in `status`
(§8.4). There is no cross-adapter or CPU copy path.

**Liveness.** Either side treats the other as gone when its heartbeat is more than
3000 ms old. Consumer side: stop sending frames, keep the client connected, poll for
a new `generation` / heartbeat every 500 ms for 10 s, then end the stream with an
error in `status`. Producer side: keep producing; it never waits on the consumer.
Section missing at start: the backend reports `no_display` and the encoder probe
fails (§3.6). The service then treats the seat as failed.

**HDR.** The backend passes through what the IDD's IddCx HDR path provides (`format`,
`colour_space`, HDR block). It never turns HDR on any other way. If the client asks for
HDR and `ack_status` = 2, the stream continues in SDR.

### 3.5 Display-mode path (Pulsar)

- Write `dd_configuration_option = disabled`. Upstream's display-device library then
  makes no topology or mode changes [U]. Leave the other `dd_*` keys at their
  defaults; they are inert when the option is disabled [U][I].
- `output_name`: leave empty. The `pulsar_idd` backend ignores it and always uses
  the seat's channel. Upstream's startup display enumeration may list no devices
  inside an RDP session; that is harmless [I].
- Client mode request path: when a client launches or resumes, the backend writes
  the client's width, height, fps (as millihertz) and HDR wish into the request
  fields, increments `req_seq` and sets ModeRequest. The IDD makes that mode the
  monitor's preferred mode and has it applied in the session (mechanism in
  `40-display-hdr.md`). The backend waits up to 2 s for `ack_seq` to match.
  On `ack_status` ≠ 0 or timeout it streams the current mode scaled to the client.
- On stream end the backend does not request a mode change back. The seat keeps the
  last mode until the next request.
- Reference behaviour of `dd_*` inside an RDP seat session in 1.6 is [?] (no 1.6 log
  on the box).

### 3.6 Encoder probe and `ddprobe`

- Upstream probes encoders at start by creating a display through the configured
  capture backend [U]; the 2024 seat log shows this sequence [L].
- `pulsar_idd` must make this probe succeed. As soon as the control section exists
  it returns a display of the current size, and it supplies a black frame until the
  first real frame arrives. The probe uses the header's LUID.
- `ddprobe.exe` is only used by upstream's desktop-duplication backend to choose a
  GPU [U][L]. `pulsar_idd` does not use it. The Pulsar package ships `tools\ddprobe.exe`
  only if the Phase 2 gate (§3.3) selects `ddx`.

## 4. Controller backend

### 4.1 Reference behaviour

| Aspect | Fork |
| --- | --- |
| Types offered (web UI, Windows) | `auto`, DualShock 4 (`ds4`), DualSense (`ds5`), DualSense Edge (`ds5e`), Xbox One / Elite (`xone`) [S] |
| Xbox 360 | removed from the UI and the selection log message [S]. ViGEm client code for Xbox 360 and DS4 is still linked [S]. A 2024 log shows ViGEmBus found at start (`ROOT\SYSTEM\0006`) [L]. Mapping of ids to ViGEm vs. the private driver is [I]: DS4 most likely ViGEm; DualSense, Edge and Xbox One the private driver |
| Automatic choice | DS4 when the client reports a PlayStation pad, motion sensors or a touchpad, and DS4 also as the general default (upstream's default is Xbox 360) [S] |
| Private driver | the server loads `DuoController\DuoController.dll` relative to the working directory on demand and resolves five exports: *Initialize*, *Uninitialize*, *CreateController*, *RemoveController*, *SendReport*. Missing library, failed initialisation or unsupported version: no controllers, error logged [S]. Feedback return path is [?] |
| Feature warnings | upstream-style log messages when a client pad has motion, touchpad or LED features the emulated type cannot carry [S] |
| Keyboard / mouse / touch / pen | upstream unchanged: `SendInput` and synthetic pointer devices (Windows 10 1809+) in the seat session [S] |
| Per-seat isolation | not done by the server. The shim and the pad driver keep pads per seat |

### 4.2 Pulsar Split approach

- Phase 2–3: upstream's ViGEm path unchanged (`x360`, `ds4`). No fork change.
  **Known limitation, not a bug:** ViGEm virtual pads are system-wide devices. Every
  seat session (and the console) sees every seat's pads until Phase 4 isolation
  lands. Test with one pad-using seat at a time. ViGEmBus is signed but end-of-life
  upstream; do not depend on it long-term.
- Phase 4: a `pulsar_pad` backend (patch 4) talking to Pulsar Split's UMDF HID pad
  driver (`60-controllers.md`) through a per-pad channel with Pulsar-owned names and
  an ACL limited to SYSTEM + the driver host. Gamepad ids `pulsar_ds4`,
  `pulsar_ds5`, `pulsar_ds5e`, `pulsar_xone`; `auto` maps to `pulsar_ds5` for
  PlayStation clients and `pulsar_xone` otherwise. Feedback (rumble, triggers, LEDs)
  returns through the same channel.
- The driver binds each pad to the session that created it. The server does not rely
  on hooks in other processes for isolation.
- Driver signing: the UMDF pad driver needs attestation or WHQL signing for end-user
  installation with Secure Boot (`00-architecture.md` §8).

### 4.3 Input on secure desktops

- Upstream on Windows re-attaches its input thread to the current input desktop
  before sending input. This is why it runs as SYSTEM: it can then reach the
  Winlogon desktop (lock screen, UAC prompt) [U].
- In a seat session the server is SYSTEM in session N, so the same should work for
  that session's Winlogon desktop [I]. Whether lock, Ctrl+Alt+Del screen and UAC
  prompts in a seat session receive input and appear in the captured picture is [?]
  (live test, both for the reference and for the §3.3 experiment).
- Pulsar: keep the server as SYSTEM in the seat session for this reason. The IDD sees
  every desktop that the session's display shows, including secure desktops [I].

## 5. App lists

### 5.1 Reference behaviour

- Format is upstream's `apps.json`: top-level `env` (object) and `apps` (array).
  Per-app fields observed: `name`, `image-path`, `cmd`, `output`, `working-dir`,
  `prep-cmd` (list of `do`/`undo`), `exclude-global-prep-cmd`, `elevated`,
  `auto-detach`, `wait-all`, `exit-timeout`. Booleans are sometimes the strings
  `"true"`/`"false"`, which upstream accepts [L][S].
- Per seat `config\<seat>_apps.json`, written by the service only if missing; after
  that the operator edits it through the seat's web-UI Apps page [L][I].
- Seed: `config\apps.json` is a template; the bare word `InstanceName` is replaced
  with the seat name. The template adds a Steam folder to `PATH` and a `VPROJECT`
  variable, and has *Desktop* and *Steam Big Picture* (Steam started through
  `cmd /c` with a per-seat IPC-name override, user chooser = seat name, gamepad UI,
  working dir = Steam folder, auto-detach and wait-all on, exit timeout 5 s) [L].
  Missing template: a built-in two-entry list [S].
- `image-path` values are bare file names resolved against `assets` [I].
- Upstream's per-launch environment variables (`SUNSHINE_APP_ID`, `SUNSHINE_CLIENT_*`)
  are unchanged [S].

### 5.2 When upstream re-reads the app list

- Upstream reads the app list at start and again after its own web-UI save handler
  writes the file [U]. It does not watch the file [U][I]. An external edit therefore
  shows up only after an explicit reload or a restart.
- The reference relies on its web-UI save path for this [I].

### 5.3 Pulsar Split approach

- Keep the upstream file format.
- The manager edits the list as a form and sends it to the service over the service
  pipe. The service validates it and writes the seat's `apps.json` atomically
  (write temp file in the seat directory, then replace).
- Then: if the server is running, the service sends `apps.reload` (§8.4), which runs
  upstream's own refresh routine. A running app is not affected; its entry changes
  apply to the next launch. If the server is not running, nothing more is needed.
  A restart is never needed for app-list changes.
- Template placeholders use explicit syntax (`{seat}`, `{seat_id}`), never a bare word.
- Steam isolation arguments belong to `50-shim-isolation.md`.

## 6. Pairing state and credentials

### 6.1 Reference behaviour

`<seat>_state.json` holds the pairing state and the web-UI login [L]. Key layout
(names only; values are secrets and were not read):

| Path | Type | Meaning |
| --- | --- | --- |
| `username` | string | web-UI user name |
| `salt` | string | salt of the password hash |
| `password` | string | salted password hash |
| `root.uniqueid` | string | host unique id seen by Moonlight; one per seat, so each seat is a separate host |
| `root.named_devices[]` | array | one entry per paired client |
| `root.named_devices[].name` | string | client name given at PIN entry |
| `root.named_devices[].cert` | string | client certificate (PEM) |
| `root.named_devices[].uuid` | string | id used for unpairing |

This is upstream's layout unchanged [U].

TLS identity: `pkey` / `cert` are not set, so seats use upstream's default relative
paths `credentials/cakey.pem` and `credentials/cacert.pem` [S]. Live state [L]:

| Folder | Files | Written |
| --- | --- | --- |
| `<install>\config\credentials\` | `cacert.pem` + `cakey.pem` | 2024-11-09, same minute as the first seat start in the 2024 log |
| `<install>\credentials\` | `cacert.pem` + `cakey.pem` | 2026-01-17 |

Both folders hold a full pair. Which one 1.6 uses is [?]: no 1.6 seat log shows it.
The `<install>\credentials\` certificate (public part only was read): self-signed,
subject = issuer = upstream's default host common name, RSA 2048, SHA-256 signature,
20-year validity. Its validity start (2022) is earlier than the file's write date, so
this pair was most likely copied from elsewhere, not created on this box [L][I].
Either way **all seats share one server certificate and key** [L][I].

File permissions: state files and `cakey.pem` inherit Program Files ACLs and are
**readable by BUILTIN\Users** [L]. Any local account, including other seats' users, can
read the server private key, the web-UI password hashes and paired-client
certificates.

### 6.2 Effects

- Seats differ by `uniqueid` and port. A client that pins certificates sees the same
  server certificate on every seat [I].
- Deleting a seat leaves its state file; re-creating a seat with the same name keeps
  its pairings [L][I].

### 6.3 Pulsar Split approach

- One key pair **per seat**. The service does **not** generate it. It writes absolute
  `pkey` / `cert` paths into the seat's `credentials\` folder, and upstream creates
  a self-signed RSA-2048 pair there at the first start when the files are missing
  [U]. This keeps crypto out of the service.
- Moonlight does not check the subject or validity of the host certificate; it pins
  the certificate it received during pairing [U][I]. The fork changes the generated
  subject to a neutral Pulsar name (no upstream or vendor name), keeps RSA-2048,
  SHA-256 and a 20-year validity.
- Regenerate (delete the pair, restart) only on explicit operator action. This unpairs
  every client of that seat; the manager says so before doing it.
- Seat directory ACL as in §2.3. The server runs as SYSTEM, so it can read and write.
- The manager never shows key, certificate or hash contents. It shows paired clients
  (name, uuid prefix) with *unpair* and *unpair all*.
- Seat deletion asks whether to keep or remove pairing state. Default: remove.
- No web-UI credentials exist. The service never passes `--creds` and never writes
  `username` / `password`. `credentials_file` is still written (same path as
  `file_state`) because upstream reads it at start; with the web UI off, missing
  credentials only produce a log line [U][I].

## 7. Port block and discovery

### 7.1 Reference behaviour

Upstream's fixed offsets from `port` are unchanged [U][L]:

| Offset | Proto | Purpose | Seen live |
| --- | --- | --- | --- |
| −5 | TCP | HTTPS: pairing challenge, app list, launch / resume / cancel | 47984 (upstream console instance) [L] |
| 0 | TCP | HTTP: `/serverinfo`, start of pairing | 47989 [L] |
| +1 | TCP | web UI (HTTPS) | 47990, bound on 0.0.0.0 [L]; seat log reports base+1 (54063 for 54062) [L] |
| +9 | UDP | video | [U] |
| +10 | UDP | control | [U] |
| +11 | UDP | audio | [U] |
| +21 | TCP | RTSP session setup | 48010 [L] |

- Seat ports are random 10000–65534 at creation, editable 1029–65514, no overlap
  check [S]. Two bases within 26 of each other collide [I].
- `upnp` off, `address_family` `ipv4` [S][I].
- The manager checks a seat with plain HTTP `GET /serverinfo` [S].
- Upstream binds its listeners on all interfaces [L] (console instance); the reference
  seats run the same code, so their web UI also listens on all interfaces [I]. Not
  observed on a running seat [?].

### 7.2 Pulsar Split port approach

- Allocate bases deterministically: first free 48100 + k·100 (k = 0, 1, …), skipping a
  block that intersects a port in use (TCP or UDP listen) or the default block
  47984–48010. Validate an operator base to 1029–65514. Reject any overlap of
  [base−5, base+21] with another seat's block.
- Store the base in the seat record; show `<LAN IP>:<base>` (Moonlight's manual add
  accepts `host:port`).
- `upnp = disabled`. IPv6 (`address_family = both`) only as a seat option.

### 7.3 mDNS discovery

- Reference: each seat server registers its own mDNS service at start (2024 seat log
  "registered" line) [L], through the Windows DNS client service API in `dnsapi.dll`
  [S]. UDP 5353 is owned by the system DNS client service (`svchost`), not by any
  server process [L]. Several seat servers therefore do not fight over the socket.
- Upstream registers service type `_nvstream._tcp` with instance name =
  `sunshine_name` and port = the base port [U]. So each seat appears in Moonlight as
  its own host under its display name.
- Two seats with the same display name would register the same instance name; how
  Windows resolves that conflict is [?].
- Pulsar: keep mDNS on (it is how Moonlight finds seats). The service enforces unique
  seat display names (case-insensitive). No Pulsar firewall rule for 5353: inbound
  mDNS is handled by Windows' built-in mDNS rule for the DNS client service [I].

## 8. Web UI, pairing and the seat control channel

### 8.1 Reference behaviour

- The fork keeps upstream's web UI except the capture and gamepad selectors. Pages:
  Home, Apps, Config, Password, PIN, Troubleshooting, Welcome [S].
- HTTPS on base+1 with the shared certificate. `origin_web_ui_allowed` stays `lan`
  [S][I], so any LAN host can reach every seat's login page.
- API routes are upstream's (`/api/pin`, `/api/apps`, `/api/clients/*`, `/api/config`,
  `/api/restart`, and others); page route `/pin` [S].
- Login: Basic authentication against the state file; first use requires creating a
  user on the Welcome page. A 2024 seat log shows "not authorized" requests from
  127.0.0.1 [L].
- The manager does not call the API. It opens `https://127.0.0.1:<web port>/` and
  `/pin` in the default browser [S][L].
- Settings changed in the web UI for keys the service writes are lost at the next seat
  start [I].

### 8.2 Pairing flow (client view; upstream unchanged) [U][S]

```
Moonlight                    seat stream server                 operator
   |  HTTP :base  /pair (getservercert, salt, client cert)  |        |
   |------------------------------------------------------->|        |
   |              (request held until a PIN is supplied)    |  PIN   |
   |                                                        |<-------|
   |  <- server cert                                        |        |
   |  HTTP /pair clientchallenge -> serverchallengeresp      |        |
   |  HTTP /pair clientpairingsecret                         |        |
   |  HTTPS :base-5 /pair pairchallenge -> paired=1          |        |
   |  (client cert stored in named_devices)                  |        |
```

- PIN: 4 decimal digits; other lengths and non-digits rejected [S].
- What the pending request carries [U]: the client's `uniqueid` query parameter, the
  `devicename` query parameter, the salt and the client certificate. Moonlight sends
  a fixed placeholder for both `uniqueid` and `devicename` [U][I], so neither
  identifies the client. The display name stored in `named_devices[].name` comes
  from the operator at PIN entry, not from the client.
- Several pending requests can exist (one per `uniqueid`), but upstream's PIN entry
  completes only one of them, the first it finds [U][I]. With Moonlight's fixed
  `uniqueid`, a second request replaces the first [I].
- Pending-request timeout: upstream has none; the client's HTTP request waits until
  the client gives up [U][I]. Reference behaviour is the same [I]; not measured [?].
- Upstream can also read the PIN from standard input (`-0`) and prints a prompt when a
  request waits [S][U].

### 8.3 Options considered

| Option | Summary | Verdict |
| --- | --- | --- |
| A. upstream unmodified, PIN through stdin (`-0`) | web UI listener stays on base+1; unpair needs a restart; no device name | **rejected**: plan.md forbids a local web server, and upstream has no switch to turn the listener off |
| Loopback HTTPS API with `--creds` | service drives upstream's `/api/*` | **rejected**: same reason (plan.md: no local web server / HTTP API) |
| B. small fork with a seat control pipe, web UI off | §8.4 | **chosen** |
| C. Pulsar's own host per seat | Moonlight compatibility would need rebuilding | long-term option for Pulsar-only seats; not for Phases 2–4 |

Apollo (public GPL-3 fork) adds console-session virtual displays and per-client
permissions. Its virtual display does not cover RDP seat sessions and it carries a
larger change set; basing on it gives no clear gain [I]. Upstream is the base.

### 8.4 Pulsar Split approach: seat control pipe (patch 2)

**Config keys added by the fork**

| Key | Values | Default | Effect |
| --- | --- | --- | --- |
| `pulsar_seat_id` | GUID | empty | seat id; required for `pulsar_idd`, the pipe and `pulsar_pad` |
| `pulsar_web_ui` | `enabled` / `disabled` | `enabled` (upstream behaviour) | `disabled`: the HTTPS config server on base+1 is not created at all. The Moonlight protocol listeners (base−5, base, base+21, UDP) are unaffected |
| `pulsar_control_pipe` | `enabled` / `disabled` | `enabled` when `pulsar_seat_id` is set | creates the seat pipe |
| `pulsar_pair_timeout` | seconds, 10–600 | `120` | a pending pair request older than this is failed towards the client and removed |

With `pulsar_web_ui = disabled`: no web-UI credentials are needed, `--creds` is not
used, the "UI available" log line is not printed, and `origin_web_ui_allowed` has no
effect.

**Pipe**

| Item | Value |
| --- | --- |
| Name | `\\.\pipe\PulsarSplit\seat\<seat-id>\server` |
| Created by | the stream server, at start, before it opens network listeners |
| Flags | first-instance only (fail to start if the name exists), reject remote clients, byte mode, max instances 1 |
| DACL | `D:P(A;;GA;;;SY)`: SYSTEM only |
| Client | the service only. A second concurrent client is refused (pipe busy) |
| Client check | the service confirms that the pipe's server process id equals the child it started |
| Server check | the server accepts only a client running as SYSTEM (`ImpersonateNamedPipeClient` or the client process token) |

**Framing.** Each message is a 4-byte little-endian length followed by that many bytes
of UTF-8 JSON (one object). Maximum message size 1 MiB; larger frames close the
connection. Both directions use the same framing.

**Envelope.**

| Kind | Fields |
| --- | --- |
| request (service → server) | `id` (u32, unique per connection), `type` (string), `body` (object, may be empty) |
| response (server → service) | `id` (same as request), `ok` (bool), `body` (object) when ok, `error` {`code`, `detail`} when not |
| event (server → service) | `event` (string), `body` (object); no `id` |

Requests may be pipelined; responses carry the request `id` and may arrive out of
order. Events are sent on the same connection at any time after the handshake.

**Handshake.** The first request must be `hello` {`protocol`: 1, `service_version`}.
Response {`protocol`: 1, `server_version`, `seat_id`, `capabilities`: list of
strings, for example `pulsar_idd`, `pulsar_pad`}. A protocol mismatch returns error
`unsupported_protocol` and the server closes the pipe. Any other request before
`hello` gets `bad_request`.

**Requests**

| `type` | Request body | Response body |
| --- | --- | --- |
| `hello` | see above | see above |
| `status` | – | `state` (`starting` / `ready` / `streaming` / `stopping`), `stream_active` (bool), `clients_connected` (u32, count of active stream sessions), `app` ({`id`, `name`} or null), `encoder` (string), `video` ({`width`, `height`, `fps`, `hdr`} or null), `pair_pending` (bool), `capture_error` (null or `no_display`, `adapter_mismatch`, `producer_lost`) |
| `pair.submit` | `request_id` (optional; default = the pending request), `pin` (4 digits), `name` (1–64 characters) | `uuid` of the new paired client |
| `pair.cancel` | `request_id` (optional) | – (the pending request fails towards the client) |
| `clients.list` | – | `clients`: list of {`uuid`, `name`}. Certificates are never sent |
| `clients.unpair` | `uuid` | – (also ends that client's active stream, as upstream does) |
| `clients.unpairAll` | – | – |
| `apps.reload` | – | `count` of apps loaded; error `bad_request` with detail when the file does not parse (the previous list stays active) |
| `session.close` | `quit_app` (bool) | – . `false`: end the active stream; the app keeps running and the client can resume. `true`: end the stream and stop the running app with its undo / exit-timeout handling, as upstream's "close app" does |
| `server.shutdown` | `grace_ms` (default 5000) | – (sent before the server stops: ends streams, unregisters mDNS, removes the pipe, exits 0) |
| `log.level` | `level` (upstream level word) | – (applies until next start) |

There is no `log.tail`: the service reads `server.log` directly (absolute path, §2.3).

**Events**

| `event` | Body | When |
| --- | --- | --- |
| `pair.pending` | `request_id` (server-assigned u32), `client_uniqueid`, `device_name`, `received_at` (UTC ISO 8601) | a `/pair` request is held for a PIN. `client_uniqueid` / `device_name` are informational only (§8.2) |
| `pair.ended` | `request_id`, `result` (`paired` / `wrong_pin` / `timeout` / `cancelled` / `client_gone`) | a pending request finished |
| `stream.started` | `app`, `video` | a stream session starts |
| `stream.ended` | `reason` | a stream session ends |
| `capture.error` | `code` (as in `status`) | capture failed or recovered (code null) |

**Error codes:** `bad_request`, `unsupported_protocol`, `no_pending_request`,
`wrong_pin`, `not_found`, `busy`, `internal`. `wrong_pin` is reported when the client's
next pairing step fails the PIN check; the pending request is then gone and the
client must start again.

**Pending requests in the fork.** At most one pending request is exposed. A new
request while one is pending replaces it (`pair.ended` `cancelled` for the old one).
The fork fails a request after `pulsar_pair_timeout` seconds (`pair.ended` `timeout`).
The service's own timeout is the same 120 s.

**Manager flow**

```
manager ──service pipe──> service ──seat pipe──> stream server
 "Pair client" (seat)                              (waits for Moonlight /pair)
 shows host:port, QR, PIN field    <── event pair.pending {request_id}
 PIN + name entered      ─────────> pair.submit {request_id, pin, name}
 result shown            <───────── response / event pair.ended {result}
```

- PIN: 4 digits. Name: required, 1–64 characters; the default name is Turkish copy
  defined by the manager UI spec (`10-manager.md`), not here.
- Results shown: paired, wrong PIN, no request pending, timeout, server not running.
- Settings changes go into the seat record. The service rewrites `server.conf` and
  restarts the server only when `status.stream_active` is false; otherwise it marks
  "applies on next start".

**Fork maintenance.** Keep the patches rebased on pinned upstream tags. Ship the
fork's full source (GPL-3 §6) with each release and keep upstream copyright and
licence notices. Name the binary with Pulsar's name and remove vendor/upstream
publisher strings from logs and `/serverinfo` branding fields that are not protocol
values; keep upstream attribution in About and the licence files.

## 9. Firewall

### 9.1 Reference behaviour [L]

| Rule name | Program | Direction / action | Proto / ports | Profiles |
| --- | --- | --- | --- | --- |
| `Duo Sunshine` | `<install>\sunshine.exe` | inbound allow | any / any | all (incl. Public) |
| `Duo` (twice) | `<install>\Duo.exe` | inbound allow | any / any | all |

- No per-seat or per-port rules. The program rule opens every port the server binds,
  including each seat's web UI [L].
- The upstream install has its own `Sunshine` TCP and UDP program rules [L].
- The service enables the built-in Remote Desktop rules (3389) at start
  (`20-service.md` §5).

### 9.2 Pulsar Split approach

- One rule pair per seat, created and removed by the service with the seat (Windows
  Firewall COM API `INetFwPolicy2` / `INetFwRule`). Names
  `PulsarSplit seat <seat-id> TCP` and `PulsarSplit seat <seat-id> UDP`; grouping
  string `PulsarSplit`; description carries the seat display name.
  - TCP: base−5, base, base+21. **Not** base+1.
  - UDP: base+9, base+10, base+11.
  - Program = the Pulsar stream-server binary; inbound allow; profiles Private (+
    Domain if the operator ticks it); never Public by default.
- No any-port program rule; no rule for the service (no network listener).
- No 5353 rule (§7.3). Do not open 3389 to the network (`30-session-rds.md`).
- Port change: rules updated at once. Uninstall: all rules in the group removed
  (`90-install.md`).

## 10. Process supervision details relevant to the server

| Item | Reference | Pulsar Split |
| --- | --- | --- |
| Restart on exit | immediate, no back-off, unlimited (`20-service.md` §6.3) [L] (live event log: a restart every ~2 s) | exponential back-off (1 s doubling to 60 s); give up after 5 failures in 5 min; report to the manager with the last log lines |
| Log | `config\<seat>.log`, recreated each start [L] | `server.log` in the seat directory, rotated by the service before each start (keep 3) |
| Stop | terminate by PID [S] | `server.shutdown` over the seat pipe, wait `grace_ms` + 1 s, then terminate |
| Health | manager polls `/serverinfo` [S] | service keeps the seat pipe open and calls `status` every 5 s; a broken pipe = server gone; the manager subscribes to service events; no HTTP polling |
| Coexistence | upstream `SunshineService` on the default ports runs next to the seats [L] | detect an upstream Sunshine/Apollo install and keep seat ports out of its block; warn if it also captures the console |

## 11. Legal and signing notes (factual)

- Stream server: GPL-3. Distributing a modified build requires offering the
  corresponding source, keeping the notices and licensing the changes under GPL-3.
  The reference fork's source availability was not checked [?].
- Statically linked parts carry their own licences (FFmpeg LGPL/GPL by configuration,
  x264/x265 GPL, OpenSSL Apache-2, Opus BSD, boost BSL). A GPL-3 product may combine
  them; ship their notices.
- The stream server is a user-mode program and needs no driver signing. Authenticode
  signing is advised for SmartScreen. The IDD and pad driver it talks to need signing
  (`00-architecture.md` §8).
- Do not ship the vendor's ViGEm installer bundle. If ViGEmBus is used, install it from
  its official signed release under its licence (BSD-3).

## 12. Open questions

| # | Question | How to resolve |
| --- | --- | --- |
| 1 | Which `gamepad` ids reach ViGEm and which the private driver in the reference; whether `x360` is accepted from the config file | runtime test on the reference, one value per seat start (needs approval to start the reference service) |
| 2 | Does upstream `ddx` or `wgc` capture a loopback-RDP seat session (with/without the RDS hardware-adapter policy)? Decides Phase 2 (§3.3) | the §3.3 experiment on the test PC (needs approval: installs/starts a test build) |
| 3 | 1.6 behaviour without `tools\ddprobe.exe`, and its `dd_*` behaviour inside a seat session | start a reference seat and read its log (needs approval) |
| 4 | Whether a running reference seat binds its web UI on all interfaces | `netstat` while a seat runs (needs a running seat) |
| 5 | Which of the two credential folders 1.6 uses | log or file-access trace of a running 1.6 seat |
| 6 | Reference pairing timeout of a pending request | runtime test with Moonlight |
| 7 | Whether SYSTEM can duplicate handles out of the IddCx WUDF host on all target builds, and whether named shared handles created in session 0 open from session N | prototype with the Pulsar IDD (Phase 3, or Phase 2 if the gate fails) |
| 8 | Input and capture on lock / UAC / Ctrl+Alt+Del screens in a seat session | live test on a reference seat and in the §3.3 experiment |
| 9 | How Windows' mDNS registration handles two instances with the same name | not needed if names are unique (§7.3); test once for the error message |
| 10 | Exact upstream commit of the reference fork | not needed for Pulsar |
| 11 | Whether the vendor publishes the fork's source | check the vendor's public repositories |
| 12 | Pad feedback return path in the reference | `60-controllers.md` |

### Notes for other spec owners (outside this file)

- `00-architecture.md` §7.1 should reduce the termsrv description to "multi-session is
  enabled by in-memory patching of termsrv.dll (RDP Wrapper family); not specified".
- `20-service.md` §8 still offers loopback HTTP as an alternative to the named pipe;
  plan.md allows the named pipe only.
- `20-service.md` §4 says relative paths resolve against the working directory; this
  file avoids the question by requiring absolute paths (§2.3).
