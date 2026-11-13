# 60 — Audio per seat

Status: draft v1 (2026-11-12, critiqued, cross-checked). Analyst. Behaviour and interfaces only; see
[../CLEANROOM.md](../../CLEANROOM.md).

This file describes how the reference product gives each seat its own playback (render) and
recording (capture) endpoints, how it hides the other seats' endpoints, and how it works with
the Windows audio services. Sources: static inspection of the shipped files, the product's own
configuration and diagnostic files, and read-only registry reads on the x64 test PC. The product
is installed there but disabled. Nothing here was seen while the product was running. Items
marked **HYPOTHESIS** are not verified.

Related specs: `20-service.md` (start order, reset action, in-process work inside Windows
services), `30-sessions.md` (how seat sessions are created, client-SKU question), `40-display.md`
(monitor ownership, used to place HDMI/DP audio), `50-input.md` (device identity and seat-mask
model), `70-usb-devices.md` (USB headsets are also USB devices), `90-install.md` (cleanup).

## 1. Summary

- An audio endpoint is a device like any other in the reference product. It has a seat mask
  (one bit per seat; several bits = shared). The default for every endpoint is "all seats".
- The seat mask is turned into **per-seat copies of the endpoint's state value** in the
  Windows endpoint registry (`tN_DeviceState`, one for each seat N that must *not* see the
  endpoint). **HYPOTHESIS:** the kernel driver's registry filter shows each seat's processes
  the copy for their seat instead of the real state, so an endpoint owned by another seat looks
  "not present" inside a seat.
- There is **no per-seat default-device setting**. A seat's default playback and recording
  device is whatever Windows picks from the endpoints visible in that seat. The product never
  calls the Windows default-endpoint or volume APIs.
- Separately, the reference product changes the running Windows Audio service while it starts
  seats. **HYPOTHESIS (strong, shared with `20-service.md` §3.1 and `30-sessions.md` §3.10):**
  this is in-process hooking of the audio service and the audio policy module, set up by code
  that the kernel driver maps into the audio service host. **HYPOTHESIS (purpose):** this lets
  non-console seat sessions use local audio hardware and keeps per-seat audio state. Pulsar
  Seat must not do this.
- Packaged (Store) apps enumerate audio through Plug and Play, not through the endpoint
  registry. The driver also knows the session-ownership device property of the endpoint
  devnodes. **HYPOTHESIS** for its use with audio.
- The session-query library (the small "which seat is this process on" API) has no audio role.

## 2. Components involved

| Component | Audio role |
| --- | --- |
| Manager (GUI) | Shows render and capture endpoints as device icons in each seat's panel and in the shared/unassigned panel. Drag and drop moves an endpoint to a seat; an endpoint can belong to several seats. Runs the service host's reset action as a child process (see §4.7). Uses the control library for everything else. |
| Control library | Enumerates endpoints (MMDevice API and SetupDi), watches for endpoint changes, builds each endpoint's composite identity, plays an identification sound on one endpoint, and writes or deletes the per-seat state copies in the registry when settings are applied. |
| Setup wizard | First-run automatic distribution: endpoints that share a ContainerID with a monitor go to that monitor's seat. |
| Seat service logic | At boot, waits for the Windows audio services (and others, §4.3) before it starts seats. While it starts seats, it changes the running Windows Audio service (§4.3). Keeps an audio-progress value in its volatile registry key. |
| Service host (maintenance actions) | Its reset action deletes all per-seat state copies from both endpoint trees (see `20-service.md` §4.3). |
| Kernel driver | Registry filter for the endpoint store (§4.1). Knows the session-id property path of endpoint devnodes and can write the session-id property of devnodes (§4.1 step 5). Lists the MEDIA device class and the KS audio/render/capture interface categories in its tables. **HYPOTHESIS (strong):** maps the service logic into the audio service host (`20-service.md` §3.1). |
| Session-query library | Only the public "which seat is this process or session on" API. No audio role. |
| Recovery driver (2.70.6) | Clears the main driver's own settings at boot. No audio role found. |
| Licence component | Exists. Out of scope. |

## 3. Data model

### 3.1 Endpoint types

The reference product uses two type identifiers of its own for audio endpoints. They are not
Windows GUIDs. They appear as `DeviceTypeGUID` in the configuration file and in the diagnostic
report:

- render: `{b5b6dcec-79e1-483e-a46d-1e1f85eedf2f}`
- capture: `{f3f459b9-597a-4f0a-9e46-71642eff8048}`

In the seat panel, render endpoints are drawn as a left/right speaker pair and capture endpoints
as a microphone. The tooltip names the device and says if it plays or records.

### 3.2 Endpoint identity (saved configuration)

Windows endpoint IDs have the form `{0.0.0.00000000}.{guid}` (render) and
`{0.0.1.00000000}.{guid}` (capture). The reference product does not key its saved entries by this
ID. **HYPOTHESIS (reason):** the endpoint GUID can change when Windows sees the device as a new
device instance. That happens for example when a USB audio device without a serial number moves to
another port, or when the driver is reinstalled. A USB device with a serial number normally keeps
its device instance, and so its endpoint GUID, on any port. This was not tested.

Each audio entry in the configuration file has these fields (key names and type tokens are
interop identifiers, same structure as `50-input.md` §2.2):

- `CanonicalName.CanonicalName` (short name)
  - `Type` = `USB_ConnectedDevice`: the USB port chain from the root hub, dash-separated port
    numbers (for example `0-4`, `0-5-1-4`). Bluetooth endpoints get the port chain of the
    Bluetooth radio.
  - `Type` = `Assorted` (all non-USB endpoints): the endpoint's friendly name as Windows shows
    it.
- `CanonicalName.CanonicalName2` (long name). All parts are separated by `|`, in this order:
  1. `A` when the parent audio device has a location path, `B` when it is root-enumerated;
  2. for `A`: the parent device's Windows location path, with each `#` between path segments
     replaced by `|` (for example `PCIROOT(0)|PCI(0801)|PCI(0003)|USBROOT(0)|USB(4)`); for
     `B`: the root-enumerator device ID (for example `ROOT\<NAME>`);
  3. `&` followed by the first 8 hex digits of `DeviceTypeGUID`, upper case (`&B5B6DCEC` for
     render, `&F3F459B9` for capture);
  4. only for an interface of a USB composite device: `USBMI(n)` (same token as in
     `50-input.md`; **HYPOTHESIS** that it is used for audio, no audio sample had it);
  5. `FR` followed by the decimal `EndpointFormFactor` value (for example `FR3` headphones,
     `FR5` headset, `FR9` digital display);
  6. the KS node-type GUID of the endpoint in braces, upper case (speaker, headphones,
     microphone, headset and so on);
  7. the KS filter reference string, `/`, and the 8-hex-digit pin ID, taken from the endpoint's
     parent-device property (for example `WAVE/00010001`, `TOPOLOGY/00010002`).
- `CanonicalName.DeviceTypeGUID`: one of the two IDs in §3.1.
- `CanonicalName.InitialName`: the friendly name when the endpoint was first recorded. Display
  only.
- `CanonicalName.Type`: `USB_ConnectedDevice` or `Assorted`.
- `StationMask.Initialized` (bool) and `StationMask.Mask` (seat mask, §3.3).

The data comes from these endpoint properties:

- `{b3f8fa53-0004-438e-9003-51a46e139bfc},2`: the parent KS device and filter path of the
  endpoint. It links the endpoint to its parent devnode and gives the filter reference and pin.
- The endpoint's friendly name, form factor and GUID property.
- The parent devnode's ContainerID and location path.

**Collision (verified in the saved configuration):** two different Bluetooth headphones on the
same radio have identical short and long names (same port chain, same form factor, same node
type, same filter/pin). They are separate entries that cannot be told apart by key.
**HYPOTHESIS:** which entry an endpoint matches then depends on order.

### 3.3 Seat mask

- Stored per endpoint like every other device. The mask is a binary string. Its **rightmost
  character is seat 1**. For example `"1"` = seat 1 only, `"10"` = seat 2 only, 12 ones = all
  seats in 2.70.3 (16 in 2.70.6: **HYPOTHESIS**, `30-sessions.md` §2).
- An endpoint never touched by the user is in all seats (shared).
- The diagnostic report lists each endpoint with its Windows endpoint ID, the configured mask
  and the effective mask, its current Windows state (active / disabled / not present /
  unplugged) and its form factor.

### 3.4 Registry footprint (Windows endpoint store)

Under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{endpoint-guid}` and
`...\Capture\{endpoint-guid}`:

| Value | Type | Written by | Meaning |
| --- | --- | --- | --- |
| `DeviceState` | DWORD | Windows | Real state. Low bits = Windows `DEVICE_STATE_*` (1 active, 2 disabled, 4 not present, 8 unplugged). The high bits are Windows-internal flags (seen: `0x10000000`, `0x20000000`, `0x22000000`, `0x32000000`) and are not touched. |
| `t0_DeviceState` … `t11_DeviceState` (`t15` in 2.70.6) | DWORD | control library on apply | Per-seat replacement state. `tN` is for the seat in mask bit N (t0 = seat 1). Set to **4 (not present)** for every seat that does **not** own the endpoint. Owning seats have no value; that means "show the real state". |

Verified on the test PC: each endpoint's set of `tN` values is exactly the complement of its
configured mask, and shared endpoints have no `tN` values. The values **stay in the registry
after the product is disabled**, but they have no effect without the driver. The same values
also appear in Windows' copy of the endpoint store from a previous OS install
(`...\Audio\PreviousOs\...`), so they survive an in-place Windows upgrade.

No other audio-related value was found in the product's own registry keys or service keys,
except the audio-progress value in the service's volatile key (§4.3).

## 4. Behaviour

### 4.1 How another seat's endpoint is hidden

1. The user assigns an endpoint to one or more seats in the manager (drag and drop), or the
   wizard does so.
2. On apply, the control library writes `tN_DeviceState = 4` for each non-owning seat N and
   deletes the `tN` values of owning seats.
3. **HYPOTHESIS:** the kernel driver's registry callback watches
   `...\MMDevices\Audio\Render\*` and `...\Capture\*`. When a process in a seat reads
   `DeviceState`, the driver returns the data of that seat's `tN_DeviceState` copy, if one
   exists. Evidence: the paths and value names sit in table form in the driver, which imports
   the registry-callback, key-object-ID and registry value read/write routines. Nothing was
   observed live. Not shown:
   - whether the driver substitutes the data (likely: it imports a value-query routine) or
     rewrites the value name;
   - which operations are covered. A complete view must cover query-value (behind
     `RegQueryValueEx` and `RegGetValue`), multi-value query, and enumerate-values (behind
     `RegEnumValue`). The driver imports no value-enumeration routine; how it handles
     enumeration is unknown;
   - whether the `tN_*` values are themselves hidden from processes in a seat.
4. **HYPOTHESIS:** the driver finds the reader's seat from the calling process's session id, or
   from the impersonated client's session when the reader is a Windows audio service working for
   a client. The driver imports both process-session and impersonation-token queries.
5. Session-id device property: the driver knows the registry path of `DEVPKEY_Device_SessionId`
   under an endpoint devnode
   (`Enum\SWD\MMDEVAPI\<endpoint>\Properties\{83da6326-97a6-4088-9453-a1923f573b29}\0006`) and
   compares key paths against it in its registry callback. Separately, the driver **writes**
   `DEVPKEY_Device_SessionId` on devnodes through the documented device-property API; this is
   proven for HID devnodes (`50-input.md`). **HYPOTHESIS:** for audio, it changes, hides or
   writes this value per seat, so that the PnP-based enumeration used by packaged apps shows each
   seat its own endpoints. Not proven for audio. On the test PC (product disabled):
   - most local endpoint devnodes carry session id 1;
   - some local endpoints (a game controller's audio) have no session-id property;
   - Windows' per-session remote-audio endpoints (`{3.0.0.0000000N}.{…}`) carry their own session
     number (sessions 2 to 5 seen; another multi-session product is also installed on that PC,
     so they cannot be attributed).
6. The Windows MMDevice client library reads endpoint state in-process from this registry store
   (the client DLL contains the value name). So a classic Win32 app in seat N would enumerate a
   hidden endpoint as "not present", and its default-device choice and the Sound control panel
   would skip it. **HYPOTHESIS** (depends on step 3).

Result: a seat sees only its own endpoints plus the shared ones. Changing a mask needs an apply.
**HYPOTHESIS:** an apply also needs the seat to restart its audio clients, because apps keep the
endpoint handles they already have.

**Assigned endpoint absent.** When a seat's only endpoint is unplugged, the seat sees it with
the real state (unplugged or not present). Other seats' endpoints stay hidden, so there is no
fallback to them. Shared endpoints remain visible and become the seat's default.
**HYPOTHESIS** (follows from the value layout; not observed).

**Session 0 and seat 1.** Seat 1 is the console session (`30-sessions.md`), and its copy is
`t0`. **HYPOTHESIS:** processes in session 0 (services) that do not impersonate a seat client
see the real state of all endpoints, because no seat maps to session 0. Not verified (open
question 10).

### 4.2 How a seat gets its own default device

- There is no "default device per seat" field in the GUI, the configuration file or the
  registry.
- The endpoint keys carry `Level:0`, `Level:1`, `Level:2` values with no per-seat copies.
  **HYPOTHESIS:** these are Windows' global default-role bookkeeping (console, multimedia,
  communications). Their meaning was inferred, not checked.
- Effective behaviour: in a seat, the default endpoint is the best endpoint the seat can see.
  The usual setup gives each seat one render endpoint (monitor HDMI/DP audio, a USB headset, or
  one analog jack) and at most one capture endpoint, so that endpoint is the seat's device.
- **HYPOTHESIS:** this works only together with the audio-service change in §4.3. Without it,
  the audio service would still return the global default for a seat even when that endpoint is
  hidden from the seat.
- What happens when a user in one seat changes the default in Sound settings was not observed
  (open questions).

### 4.3 Interaction with the Windows audio services

- **Boot order.** Before it starts seat sessions, the seat service waits for these services to
  be running: Broker Infrastructure, Plug and Play, RPC Endpoint Mapper, RPC, Windows Audio
  (`AudioSrv`) and Windows Audio Endpoint Builder (`AudioEndpointBuilder`). It polls the
  service control manager and sleeps between polls. No timeout or error branch was found; it is
  unknown what happens when a service never starts. The configurable delays are in
  `20-service.md`. A changelog entry fixes a problem where Windows Audio started late.
- **Change to the running Windows Audio service.** The reference product changes the running
  Windows Audio service while it starts seats. Pulsar must not do this. **HYPOTHESIS (strong):**
  the mechanism is in-process hooking. The kernel driver maps the service logic into the audio
  service host, and the logic then hooks the audio service and audio-policy modules. Evidence is
  in `20-service.md` §3.1 (hook table naming both modules, section mapping in the driver) and
  `30-sessions.md` §3.10. The service process also has cross-process memory access to that host.
  Whether code or data is changed was not determined.
- **Audio-progress value.** The service logic keeps an audio-progress value in its volatile
  registry key. The routine that writes it also asks the RPC runtime for the calling client's
  thread, and its caller is in the multi-seat code path. **HYPOTHESIS:** the value is written
  from inside the hooked audio service while it handles an RPC call from a client, so that the
  service can tell for which seat's client a call is made.
- **HYPOTHESIS (purpose):** Windows plays local audio hardware for the console session. Other
  sessions get a per-session virtual remote endpoint or nothing. The change makes the audio
  service treat seat sessions as local and keep per-seat state, so that they may open the
  physical endpoints. Changelog entries about missing sound devices on additional seats and
  session-handling problems causing sound issues on build 19041 fit this.
- The product does **not** install an audio driver and does **not** add an audio class filter
  (the live MEDIA class has only Windows' own `ksthunk` upper filter). It does not change the
  Windows Audio service's configuration (start type, dependencies, image). It does change the
  running service as described above.

### 4.4 Automatic assignment (wizard)

- The wizard first makes one seat per monitor (`40-display.md`). Then, for each monitor, it reads
  the monitor's ContainerID and looks for endpoint devnodes (`SWD\MMDEVAPI\…`) with the same
  ContainerID. A match (the monitor's own speakers over HDMI/DP) goes to that monitor's seat.
- Monitors without a real container (the all-`F` container ID that means none, for example a laptop panel)
  match every internal endpoint. **HYPOTHESIS:** these then stay with seat 1 or stay shared.
- USB headsets follow the USB-hub rule used for keyboards and mice (`50-input.md`).
  **HYPOTHESIS** for audio: the wizard log shows only the ContainerID pass.

### 4.5 Identification and status in the manager

- The device context menu can play a short built-in sound on one specific endpoint, so that the
  user can hear which physical device it is. The control library uses the legacy waveOut API,
  finds the waveOut device that matches the endpoint, and plays an embedded wave resource.
  **HYPOTHESIS:** the match uses the waveOut function-instance (endpoint ID) query. This is the
  only audio output the product makes itself.
- The control library registers for endpoint change notifications (MMDevice notification client)
  and device-interface notifications. A newly plugged device is highlighted for a few seconds
  (`secondsToFlashInsertedDevice`, `10-manager.md`).

### 4.6 Hot-plug

- An endpoint that disappears keeps its configuration entry. When it comes back with the same
  composite identity, it gets its mask again.
- The `tN` values are tied to the Windows endpoint GUID. While Windows keeps that GUID (same
  device instance), the hiding keeps working after a re-plug.
- **HYPOTHESIS:** when Windows creates a new endpoint GUID (see §3.2), the endpoint has no `tN`
  values and is visible in all seats until the control library applies the settings again (on
  its change notification, or at the next manual apply).

### 4.7 Disable / reset / uninstall

- Disabling the product stops the driver, so the `tN` copies stop having any effect. They are not
  removed (verified on the test PC).
- The service host's reset action deletes all `tN_DeviceState` values from both the Render and
  the Capture trees, together with the other per-device markers (`20-service.md` §4.3). The
  manager runs the reset action as a child process and reports the exit code if it fails. The deletion
  routine was found in the service host; its assignment to the reset action rather than the recovery action
  follows `20-service.md`.

## 5. Windows interfaces used (reference product)

- MMDevice API: `IMMDeviceEnumerator`, `IMMNotificationClient`, endpoint property store
  (friendly name, form factor, endpoint GUID, parent-device property `{b3f8fa53-…},2`). Device
  topology interface in the wizard.
- SetupDi / CM: `SWD\MMDEVAPI` devnodes, `KSCATEGORY_AUDIO` interfaces, ContainerID, location
  paths, parent walk. `RegisterDeviceNotification`.
- Legacy waveOut (open, prepare, write, reset, close, device message) for the identification
  sound.
- Registry: `MMDevices\Audio\Render|Capture\{guid}` values `DeviceState`, `tN_DeviceState`.
- Kernel: registry callback (legacy `CmRegisterCallback`), registry value read/write, device
  property read and write (`IoSetDevicePropertyData` on `DEVPKEY_Device_SessionId`), process and
  impersonation session queries, section mapping into user processes.
- Service control manager: wait for `AudioSrv` and `AudioEndpointBuilder`.
- In-process changes to the Windows Audio service host (§4.3). **Must not be copied.**
- RPC runtime client-thread query (inside the audio-progress routine).
- Not used: `IPolicyConfig` or any other default-endpoint setter, endpoint volume APIs, audio
  drivers, audio class filters.

## 6. Error cases and edge behaviour

| Case | Behaviour in the reference product |
| --- | --- |
| Audio services not running yet at boot | The seat service polls and waits before it starts seats. No timeout or error branch was found (unknown what happens if a service never starts). |
| Endpoint owned by seat 2, seat 2 switched off | **HYPOTHESIS:** seat 1 still sees it as not present, because the copies are written on apply regardless of the seat's run state. |
| Seat's assigned endpoint unplugged | **HYPOTHESIS:** the seat sees it unplugged; no fallback to other seats' endpoints; shared endpoints remain (§4.1). |
| Endpoint shared by several seats | Visible in all of them. **HYPOTHESIS:** Windows mixes the streams of several sessions on the device. Whether non-console sessions may open local endpoints at all is what the audio-service change is believed to address. |
| Two identical Bluetooth endpoints on one radio | Identical keys (§3.2). **HYPOTHESIS:** order-dependent match. |
| Bluetooth endpoints | Treated like USB devices under the radio's port. The owner sees the real state (often unplugged). Other seats see not present. |
| Packaged (Store) apps see no audio devices | Fixed in an older release for build 15063+. **HYPOTHESIS:** related to the session-id device property. |
| Product disabled or uninstalled | `tN` values stay but have no effect. The reset action removes them. |
| Windows in-place upgrade | `tN` values are carried into Windows' previous-OS copy of the endpoint store. |

## 7. ARM64 notes

- The reference product has no ARM64 build, so nothing in this area works on ARM64 today. The
  reasons (aligned with `30-sessions.md` §5):
  - its x64 kernel driver cannot load on ARM64 Windows;
  - its audio-service hooks are x64 code, and Windows never loads x64 code into a native ARM64
    system process such as the audio service host. x64 emulation exists only for whole user-mode
    processes. (Cross-process memory APIs as such exist under emulation; that is not the
    blocker.)
- The Windows audio stack is the same on ARM64: MMDevice API, WASAPI, `AudioSrv`,
  `AudioEndpointBuilder`, the same endpoint registry layout, the same PnP endpoint devnodes, the
  same remote-audio virtual endpoints. All documented user-mode APIs above exist natively.
- ARM64 machines often use ACX/SoundWire internal audio, USB-C/DisplayPort alt-mode monitor
  audio, and USB or Bluetooth headsets. The composite identity (§3.2) must accept location paths
  that are not PCI, such as ACPI or SoundWire paths. **HYPOTHESIS:** some internal endpoints have
  no PCI path.
- Any kernel component (design 4 only) needs a native ARM64 build and its own attestation
  signature for ARM64. The registry-callback routine is available on ARM64.
- Use WASAPI, not waveOut, for any sound Pulsar plays itself.
- The RDP client library chosen for design 3 (§8) must build natively for ARM64.

## 8. Pulsar Seat approach

Rules (`docs/agents/rules.md`): no vulnerable drivers, no kernel patching, no code injection,
everything works with Memory Integrity (HVCI) and Secure Boot on, native ARM64, user mode first.

1. **Keep the data model.** Audio endpoints are devices with a seat mask, same semantics (bit N
   = seat N+1; all bits = shared; default shared). Identify endpoints by the composite key of
   §3.2, not by the endpoint GUID. Add a tie-breaker that the reference lacks (for example the
   Bluetooth device address or the USB serial number, when present) to avoid the collision in
   §3.2. Store the friendly name for display only. The importer reads the fields of §3.2
   (`DeviceTypeGUID`, `CanonicalName`, `CanonicalName2`, `InitialName`, `Type`,
   `StationMask.Mask`) and maps them to Pulsar's own keys.
2. **Do not modify the Windows Audio service.** No in-process hooks, no code injection, no DLL
   mapping into `svchost`, no cross-process memory writes. Hard no.
3. **Primary design: per-session audio of remote-protocol seat sessions.**
   - **Precondition.** This design exists only if seat sessions are remote-protocol (RDP
     loopback) sessions, option A in `30-sessions.md` §6.1. On client SKUs (the test PC runs
     Windows Pro; ARM64 Windows is client only), several concurrent sessions are not available
     through documented paths. The only known ways around that patch or wrap termsrv, which
     conflicts with rules.md and is a release blocker (`30-sessions.md` §4). So design 3 is
     driver-free and rule-clean **only on Windows Server with RDS**, or after the maintainer
     decides the client-SKU question. It is not a rule-clean answer on client Windows today.
   - **Behaviour given the precondition.** Windows gives each remote session its own virtual
     playback and recording endpoint. That endpoint is the session's only visible device and its
     default; the session cannot see local hardware. This solves hiding and "own default device"
     with no driver.
   - **Bridge.** Pulsar runs one audio bridge per seat. The stock RDP client control plays
     redirected audio on the default device of the session it runs in and has no documented way
     to choose an output endpoint; its audio channel is internal to the control. So the bridge
     needs its own client-side audio redirection stack ([MS-RDPEA] for output, [MS-RDPEAI] for
     input). Candidate: FreeRDP (Apache-2.0, compatible with GPL-3), whose audio output and input
     client channels take pluggable back-ends. Pulsar writes a WASAPI back-end that opens the
     seat's assigned endpoint by endpoint ID (render) and the seat's assigned microphone by ID
     (capture), and sends captured audio back over the audio-input channel. **HYPOTHESIS:** this
     can be done inside the seat host process that already shows the seat (`30-sessions.md`
     option A). If the stock control must stay for video and input, a second connection used only
     for audio is an alternative to check (open question 11).
   - **Which session runs the bridge.** The bridge runs in the seat host process in the console
     session (session 1), not in the service in session 0. **HYPOTHESIS:** rendering from session
     0 is restricted or unreliable for this use. The service only starts, stops and supervises
     the bridges.
   - A shared endpoint receives streams from several bridges; WASAPI shared mode mixes them.
     Volume and mute inside the seat act on the seat's own virtual endpoint.
   - **Assigned endpoint absent.** The seat's virtual endpoint stays present. The bridge drops the
     stream and the manager tells the user that the seat has no audio device. There is no automatic fallback
     to another seat's endpoint. A fallback to a shared endpoint is a user option, off by default.
   - **Console seat.** Seat 1 is the console session and sees local hardware directly.
     Endpoints given only to other seats stay visible in seat 1 under this design (Pulsar cannot
     hide them without design 4). The manager must say so. Open question 12.
4. **Design 4 (registry view), not viable as it stands.** A kernel registry callback
   (`CmRegisterCallbackEx`, a WDM routine callable from a KMDF driver) could present a per-seat
   endpoint state the way §4.1 describes. It changes registry views only, never code.
   - Why not user mode: the MMDevice client reads the endpoint store inside every app process.
     A per-process view in user mode would need hooking each process's registry calls, which is
     code injection. There is no documented user-mode per-session view of `HKLM`. So only a
     kernel callback can do it.
   - It must be built for x64 and ARM64, attestation-signed for each, and HVCI-compatible. That
     needs to be checked (HVCI-compatible build settings, Driver Verifier, HVCI readiness test).
   - **The open dependency.** A view alone is not enough. The Windows Audio service still lets
     only the console session use local hardware (§4.3 hypothesis). The only known way to close
     that gap is to hook or patch the audio service, which rules.md forbids. No documented
     mechanism is known. Therefore **design 4 is not viable unless a documented route is found**.
     Implementers must not try in-process changes to the audio service to make it work.
5. **Identification sound:** play a short tone with WASAPI on the selected endpoint (open it by
   endpoint ID). Never play it on the default device.
6. **Wizard rules:** endpoints with the same ContainerID as a seat's monitor go to that seat.
   USB audio devices under a seat's USB hub go to that seat. Everything else stays shared.
   Bluetooth audio stays shared until the user assigns it.
7. **Hot-plug:** listen with `IMMNotificationClient` and PnP device-interface notifications.
   When an endpoint's composite key matches a saved entry, re-bind that seat's bridge to it
   within about a second and do not restart the session.
8. **Startup:** the service waits for `AudioEndpointBuilder` and `AudioSrv` before it starts the
   seat audio bridges (not before it starts sessions), with a configurable timeout. A missing
   audio service means "seat has no sound" and is shown in the manager. It is not fatal.
9. **Cleanup:** design 3 writes nothing into the Windows endpoint store, so there is nothing to
   clean up. If a view design is ever used, all per-seat values live under Pulsar's own key,
   not under `MMDevices`, so an uninstall cannot leave hidden devices behind.

## 9. Open questions

1. How does the reference product answer "default endpoint" for a seat whose global default is
   hidden: by the audio-service change, by the registry view (audio services reading
   `DeviceState` under client impersonation), or not at all?
2. What exactly does the audio-service change alter? Only behaviour matters: which sessions may
   open local endpoints. A live test with the product enabled would answer it (compare what a
   seat session sees with and without the service). Needs the maintainer's OK to enable the
   product on the test PC.
3. Does the driver change, hide, write or pass through the endpoint devnodes' session-id property
   per seat? Why do most local endpoint devnodes carry session id 1, and some none?
4. Which registry operations does the driver cover (query, multi-query, enumerate)? Are the
   `tN_*` values hidden from seat processes?
5. When a user in one seat sets a different default device in Sound settings, does it change the
   default for the other seats?
6. Do per-app output-device preferences (kept per user) cross seats when the same account is
   logged on to two seats?
7. Does a USB audio device with a serial number keep its endpoint GUID on another port on the
   test PC? Is the hiding re-applied right away when a new endpoint GUID appears?
8. Design 3 latency: which remote audio mode gives acceptable latency for games and voice chat
   (target ≤ 60 ms end to end, **HYPOTHESIS**)? Does the virtual endpoint expose more than stereo
   48 kHz?
9. Design 3: is microphone redirection on by default for such sessions, or does it need a policy
   value set by the installer (`90-install.md`)?
10. Which seat (if any) do session-0 processes and the console session's processes get in the
    reference product's view?
11. Design 3: can one FreeRDP-based seat host carry video, input and audio, or does audio need a
    second connection next to the stock client control? Which choice fits `30-sessions.md`?
12. Design 3: is it acceptable to the maintainer that seat 1 (console) can still see endpoints
    assigned to other seats?
13. Exclusive-mode apps and spatial audio on a seat act on the virtual endpoint under design 3,
    not on the hardware. Acceptable?
