# seat-pads

Virtual controllers, shared by both seat kinds (package **pads**,
`docs/split/agents/contracts.md` §5, behaviour in `spec/remote/60-controllers.md`). Licence
GPL-3.0-only. Remote seats get pads from the stream client; local seats can get a seat-owned
virtual pad for a routed physical pad (see "Local seats" below).

## Local seats (merge step 8; wired into the service in round 4, per seat `virtual_pads`, default off)

`seat_input::local_pads` is the bridge. A physical pad that `seat-input` routes to a local
seat gets a virtual pad from this crate:

- `mirror_kind`: PlayStation pads keep their kind (DS4 / DualSense / Edge by USB id);
  everything else becomes an Xbox One pad.
- Every HID pad is read since 2026-11-17 (`RoutedPad::layout`): PlayStation by its public
  report layout (USB and Bluetooth), Switch Pro by `switch_pro` (reports 0x30 / 0x3F; init
  sequence written at open: USB 0x80 handshake, full report mode, IMU, vibration, player
  light), everything else (Xbox HID collections, generic joysticks / gamepads) through its
  own report descriptor (Windows `HidP_*` in `seat_input::win::pad_io`, mapped by
  `generic`: XInput-compatible view with the combined Z trigger, Xbox Bluetooth view,
  standard DirectInput-style view).
- `plan`: the same `mapping::resolve` rules as remote seats. ViGEm and usbip-win2 are used
  only when installed. PulsarPad is used only when the seat prefers it.
- `LocalPads::attach` creates the pad with the seat's session id and user SID.
  `forward_state` sends an XInput-shaped state (`xinput_to_state`). `forward_report` passes a
  raw HID report through, but only when the layouts match.
  `detach` / `detach_seat` remove pads.

Design limits:

| Topic | Limit |
| --- | --- |
| ViGEmBus | **only if already installed**; never installed or bundled by us (X360 / DS4) |
| DualSense | usbip-win2 (signed, must be installed) + our loopback USB/IP DualSense server |
| Own pad driver (`drivers/split-pad`) | **unsigned**; only on explicit preference, never default |
| Physical pad | scoped to its seat since 2026-11-17 (`seat_input::pad_scope`, setting `input.pad_device_scope`, default on): device SDDL with the seat user's SID + `DEVPKEY_Device_SessionId` on the HID collection **and its parent** (where the XUSB interface of Xbox pads lives), property-change restart, journaled in `<data>\pads\scope.json` and restored at stop / next start / setting off. In shimmed processes (x64 / x86) the local seat map entry lists the pad and its parent as the seat's `PadInstances`, so XInput / HID opens follow the seat there too. Without either (ARM64 apps, the shared GameInput service in session 0) a pad can still be shared: HYPOTHESIS. |
| Which pad a game picks | HYPOTHESIS: many games take the first XInput slot, which may be the physical pad |
| Service wiring (round 4) | `LocalPadHub` on the service's one `PadManager` (shared with remote seats): device SDDL with the seat user's SID + device session id, ownership registry, orphan cleanup at start / stop, one reader thread per physical pad. Every HID pad is read (2026-11-17); `Unsupported` only when the pad's report descriptor cannot be read. |
| Feedback to the physical pad | `LocalFeedbackRouter`: PlayStation same kind = output report unchanged, other kind = rumble / lightbar / player LEDs / triggers rebuilt; Bluetooth PlayStation pads get the report rewrapped as 0x31 / 0x11 with its CRC-32 (`bt`); Switch Pro: rumble (0x10) and player lights; Xbox on Bluetooth: rumble + impulse triggers (0x03). Xbox on USB (rumble only through XInput) and generic HID pads: none (no documented HID output report). |
| Default | per seat **off** (`LocalSeat::virtual_pads`, manager "Sanal oyun kolu"): every usable backend needs a driver install (ViGEmBus / usbip-win2) that Pulsar Seat never does |
| Per-process HID shim (`seat-shim-core`, `shim/`) | remote kind only for now; x64 / x86 only, so off on ARM64 |

Untested on hardware. `cargo test -p seat-input --features local-pads local_pads` runs the
bridge and hub tests against a fake backend. `seat-service` enables the feature in its
normal dependencies since round 4, so release builds carry the hub. HYPOTHESIS (untested on
hardware): the shared HID open from session 0, the USB output report writes, the securing
pass on ViGEm / usbip-win2 devices and every pad's behaviour in games.

## What it does

- **`PadBackend` / `VirtualPad`**: one trait pair with three backends.
  | Backend | Kinds | Driver | Default |
  | --- | --- | --- | --- |
  | `VigemBackend` | Xbox 360, DualShock 4 | ViGEmBus 1.22 (BSD-3), **must already be installed**: never bundled or installed by us | yes (if present) |
  | `UsbipDs5Backend` | DualSense, DualSense Edge | usbip-win2 (BSD-2), plus our in-process USB/IP device server | yes (if present) |
  | `PulsarPadBackend` | Xbox One, DS4, DualSense, Edge | our own UMDF driver in `drivers/split-pad` (unsigned) | **never**: only when `pad_backend = pulsar_pad` *and* the package is staged |
- **`PadManager`**: the service-side owner of all pads. It follows the spec 60 §3.5 rules:
  - one live pad per (seat, slot), and a `pad.wanted` of the same kind re-attaches the pad;
  - a different kind replaces the pad, and `generation` filters late messages;
  - pads linger 30 s after `stream_ended`; `pad.detach` is answered or times out after 2 s;
  - at most 16 slots per seat.

  It also writes the ownership registry and applies device security (SDDL with the seat
  user's SID) plus `DEVPKEY_Device_SessionId` to the pad node and its children. It runs the
  channel pump (input section to backend) and routes feedback.
- **Shared channel** (spec 60 §4.5): `layout.rs` (constants), `shm.rs` (seqlock input,
  16-entry output ring), `channel.rs` (`PadChannel`, `ChannelFactory`). The service creates
  the named sections and events (`win::WinChannelFactory`) with DACL SYSTEM + LOCAL SERVICE
  only. It refuses names that already exist.
- **Reports** (`reports.rs`): `PadState` to the USB report layouts of DS4, DualSense, Edge,
  Xbox HID and ViGEm XUSB. It keeps real counters and native-unit timestamps, and parses
  output reports into `Feedback` (rumble, trigger rumble, lightbar, adaptive triggers,
  player LEDs).
- **Descriptors / features** (`descriptors.rs`, `features.rs`): our own HID report
  descriptors and feature-report answers (calibration, pairing MAC, firmware). These files
  are `core`-only and are shared with the driver via `#[path]`.
- **Markers** (`ids.rs`, contracts §5.3):
  - device-id part `…&PULSARPAD&<seat32>&<slot>`;
  - USB serial `PSPAD<seat32><slot>`;
  - `marker_of()` recognises only our exact tokens, never `DUOCONTROLLER`;
  - `pad_sddl()` and `mac_for()` (SHA-256 of seat, slot and host salt, with the locally
    administered bit set).
- **Ownership registry** (`owners.rs`): `%ProgramData%\PulsarSplit\pads\owners.json` with
  atomic replace. Each row holds the creator pid and creation FILETIME. `plan_orphans()` is
  pure; `PadManager::cleanup_orphans()` removes devices children-first and never touches
  unmarked devices.
- **Bluetooth** (`bluetooth.rs`, spec 60 §10.4): `plan_bt()` (pure) plus `BtController`.
  The controller disables only the per-user `BluetoothUserService_<suffix>` **instances**
  of seat logons through the SCM, and journals each change (`journal\pads.jsonl`) for
  `restore_all()`. No DLL is replaced and no ACL is changed.
- **Wire messages** (`control.rs`): `PadWanted`, `PadAttach`, `PadDenied`, `PadReleased`
  and `PadDetach`, copied field for field from spec 60 §3.5. `PadAttach.media` is a
  Pulsar addition (2026-11-17, absent = false).
- **Media section** (`media.rs`, shared-memory protocol 2, 2026-11-17): `<base>_Media`
  + `<base>_MediaEvt` per pad, two SPSC PCM rings (out = speaker / headset / haptics,
  4 ch 48 kHz s16le; in = client microphone, 2 ch). `ManagerSink::haptics_pcm` fills the
  out ring; `PadManager::pump` moves the in ring into `VirtualPad::submit_mic` (usbip
  DualSense: its mic iso-IN endpoint plays it, silence when empty, 200 ms buffer). Layout
  in `docs/split/agents/pulsar-host-backend.md` §5. Remote seats' DualSense pads get the
  audio interfaces only with `RemoteSettings::pad_audio` (default off). Sunshine seats
  stay HID-only (Moonlight has no controller audio).
- **Physical pads of local seats**: `generic.rs`, `switch_pro.rs`, `bt.rs` (pure) and
  `win::devtree::{parent_instance, security_sds, set_security_sds, session_id_of,
  set_session_id_of, property_change_restart}` (any node; only for nodes `pad_scope`
  journaled).

## Contract deviations (for the integrator)

- `PadKind` serde names are the spec wire ids `x360`, `ds4`, `ds5`, `ds5e`, `xone`. Plain
  snake_case would give `ds5_edge` / `x_one`, which the seat pipe does not use.
- Additions:
  - `VirtualPad::submit_report` and `VirtualPad::driver_reads_channel`;
  - `FeedbackSink::raw_output` and `FeedbackSink::haptics_pcm` (both have defaults);
  - `PadManager` methods `take_outgoing`, `detach_acked`, `attach_failed`, `detach_seat`,
    `pump`, `input_events`, `refresh_backends`, `set_preferred` and `shutdown`;
  - `PadStatus` / `PadPhase`.
- `PulsarPadBackend::new(api, host_salt)` takes the host salt so that the driver gets
  the MAC through an extra property `PKEY_PulsarPad_Mac` (pid 7). This property is not in
  spec 60 §3.7 yet (see open-questions).
- `UsbipTool::attach(host, port, busid)` takes a port. The real tool only accepts 3240,
  because the usbip-win2 CLI documents no port option.

## Integration sketch

```rust
let (pid, start) = seat_pads::win::current_process_identity();
let tree = Arc::new(seat_pads::win::CmDeviceTree);
let backends: Vec<Arc<dyn PadBackend>> = vec![
    Arc::new(VigemBackend::new(Arc::new(win::WinVigemDriver), tree.clone())),
    Arc::new(UsbipDs5Backend::new(Arc::new(win::WinUsbipTool::default()),
        Arc::new(win::WinConsoleSession), UsbipConfig { host_salt, ..Default::default() })),
    Arc::new(PulsarPadBackend::new(Arc::new(win::WinSwDevice), host_salt)),
];
let mut m = PadManager::new(backends, Box::new(FileOwnerStore::under(&data_root)), tree,
    Arc::new(win::WinProcessProbe), clock, Arc::new(win::WinChannelFactory), sink,
    PadManagerConfig { preferred, creator_pid: pid, creator_start_time: start, ..Default::default() });
m.cleanup_orphans()?;              // service start (and stop)
// seat pipe: pad.wanted -> m.wanted(), pad.released -> m.released(); send m.take_outgoing()
// pad loop: wait on m.input_events() (or 1 ms) -> m.pump(); every 100 ms m.tick(now)
// shim seat map: m.seat_instance_prefixes(seat)
```

## Build and test

```
cargo test   -p seat-pads
cargo clippy -p seat-pads --all-targets
cargo check  -p seat-pads --all-targets --target x86_64-pc-windows-msvc
cargo check  -p seat-pads --features fake
```

The tests use fakes only (`fake.rs`, feature `fake`):
- no test touches HKLM, services, drivers or devices;
- the `win` tests are read-only: liveness of the test process itself, SDDL parsing, and
  service/class-key lookups;
- the USB/IP tests run our server on an ephemeral `127.0.0.1` port and play the client
  side over a real loopback socket.

## Untested (needs hardware / installed drivers / signing)

- **2026-11-17 additions (HYPOTHESIS, nothing ran on hardware):** the generic HID
  decoding of real Xbox / generic pads (button order per firmware, combined-trigger
  direction), Switch Pro init / rumble amplitude encoding / stick calibration (no SPI
  factory calibration read), every Bluetooth output report (DS4 0x11 / DS5 0x31 with
  CRC, Xbox 0x03, Switch 0x10) and the zero padding to `OutputReportByteLength`, the
  physical-node scoping (property-change restart of physical pads, effect on XUSB
  interfaces), the media section with any stream server, and the usbip mic path.

- **ViGEmBus**: IOCTL layouts were taken from the public ViGEmClient headers (MIT) and
  checked against them on 2026-11-12. They were never run against a real bus. Not verified:
  - plug-in serial retry semantics;
  - `WAIT_DEVICE_READY`;
  - `DS4_AWAIT_OUTPUT_AVAILABLE` behaviour;
  - finding the new child by its serial among the bus children (`HYPOTHESIS`, pads-11;
    the racy before/after difference is gone).
- **usbip-win2**: never attached for real. Unchecked points:
  - whether `usbip.exe attach -r 127.0.0.1 -b 1-<random u32>` accepts our busids;
  - whether the importing socket is owned by System (kernel client) or by `usbip.exe`
    (`WinPeerAuth`, pads-10);
  - whether isochronous audio works through usbip-win2;
  - whether Windows binds `usbccgp` + `usbaudio` + HID to our composite;
  - whether the USB serial becomes the instance id (`HYPOTHESIS`, and 0.9.8.1 has a serial
    regression);
  - the full-speed timing assumptions.
- **Audio "where the session allows"**: `AudioPolicy::ConsoleOnly` (the default) exposes
  the speaker and microphone interfaces only for pads owned by the console session. Seat
  (RDSH) pads are HID-only. `HYPOTHESIS`: USB audio endpoints are not usable from
  redirected remote sessions.
- **Device security / session id on real devnodes** (`CmDeviceTree`):
  - `SPDRP_SECURITY_SDS` on ViGEm and USB children;
  - `CM_Uninstall_DevNode` for phantoms.
- **PulsarPad / `SwDeviceCreate`**:
  - whether the creator may set `DEVPKEY_Device_SessionId`;
  - whether `mshidumdf` binds to a software device (spec 60 §13 #7).

  The driver itself is unbuilt; see `drivers/split-pad/README.md`.
- **Bluetooth instance disabling** (spec 60 §13 #9) was never run. `WinUserServices` is
  written from SCM documentation.
- **Shim classification of ViGEm pads**: these pads carry no marker, so they rely on the
  `instance_id` we record. Pads that an unmodified upstream server creates stay
  `ViGEmUnowned` (global).

## Third-party notices

- No third-party code is included.
- The ViGEmBus IOCTL numbers and struct layouts are interoperability facts. They come
  from the public ViGEmClient headers (MIT, © Nefarius Software Solutions e.U. and
  Contributors).
- The USB/IP wire format follows the Linux kernel documentation of the protocol.
- Runtime components (installed separately, never bundled by this crate):
  - ViGEmBus: BSD-3-Clause;
  - usbip-win2: BSD-2-Clause.

## Review fix (2026-11-12)

- USB/IP server: `PeerAuth` owner-pid check before any byte (`WinPeerAuth`: System or
  our own `usbip.exe` child), one-shot arm window per busid, atomic import claim, random
  busids, empty `OP_REQ_DEVLIST`, at most 8 pre-import connections with a 1 s op timeout,
  mic iso-IN queue bounded (`MAX_SCHEDULED`).
- Channel names are per pad instance (`ids::channel_name`, pads-8).
- `PadManager`: usbip creates/removes run on worker threads (`Ok(None)` + late
  `Outgoing::Attach` / `Outgoing::Denied`); securing runs for the pad's whole life (on
  device arrival via `win::DeviceArrivalWatch` and every 250 ms from `tick`); pads stay out
  of the shim seat map until their node is secured; owner/session change re-secures.
- `FileOwnerStore::under` checks / protects its directory (`win::fsacl`).
