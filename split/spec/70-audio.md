# 70 — Per-seat audio

How the reference product (1.6.0.0) gives every seat its own sound: where audio is
produced, how it is captured for that seat only, what is configured, and what does
**not** work (microphone, controller speaker/haptics). Each mechanism ends with a
"Pulsar Split approach" subsection. Evidence markers as in `00-architecture.md`:
**[S]** static, **[L]** live system, **[I]** inferred, **[?]** unknown. No code.

Related specs: `00-architecture.md` §4 (sessions/tokens), §5.3 (mappings) and §7.6,
`20-service.md` §4 (stream-server config keys) and §6.2 (seat start),
`30-session-rds.md` (headless RDP client, pending), `60-controllers.md` (virtual pads,
pending), `80-stream-server.md`.

---

## 1. Summary

| Topic | Reference behaviour |
| --- | --- |
| Audio device per seat | none of its own. Each seat is an RDP session; Windows creates a session-scoped render endpoint named *Remote Audio* for it at RDP logon [L] |
| Capture | the per-seat stream server (Sunshine fork) runs inside the seat session and records that endpoint with WASAPI loopback, then encodes Opus and sends it to the client. This is the standard upstream Sunshine audio path [S][L] |
| Configuration | two keys written into every `<seat>.conf`: `virtual_sink = Remote Audio`, `install_steam_audio_drivers = disabled` [S][L] |
| Own audio driver | none. No kernel or user-mode audio driver ships with the product [S][L] |
| Shim / hooks for audio | none found. The per-process shim and the verifier DLLs import no audio APIs [S] |
| WRDS protocol provider | ships (`DuoRds.dll`) but is not loaded; the stock Windows RDP provider decides the audio mode (§2.3) [S][L] |
| Microphone into the seat | not supported (no path from the client, no RDP audio-input channel) [S][L] |
| Controller speaker / headset / audio haptics in a seat | not supported; virtual pads are HID-only, and no binary handles haptics [S][L] |
| Console (session 1) | untouched; physical devices stay with the console user [L][I] |

```
 seat session N (RDP, headless)                          session 0
 +----------------------------------------------+        +------------------------+
 | games / apps --> Windows audio engine         |        | headless RDP client    |
 |        (default render device of session N    |        | (loopback 127.0.0.1)   |
 |         = "Remote Audio" endpoint)            |        | joins no audio channel |
 |                     |                         |        | -> redirected PCM is   |
 |                     v WASAPI loopback         |  RDP   |    never played [I]    |
 | stream server (SYSTEM token, session N)       |<------>|                        |
 |   Opus encode -> UDP base+11 -> Moonlight     |        +------------------------+
 +----------------------------------------------+
 console session 1: physical speakers/headsets/HDMI; not visible inside session N [I]
```

## 2. The per-session "Remote Audio" endpoint

### 2.1 Observed facts

| Item | Value | Ev. |
| --- | --- | --- |
| Device class | `AudioEndpoint` (`{C166523C-FE0C-4A94-A586-F1A80CFBBF3E}`), software device under `SWD\MMDEVAPI`, installed from the inbox `audioendpoint.inf` (no driver) | [L] |
| Friendly name | `Remote Audio` (seen on the live box; localization on other display languages is [?]) | [L] |
| PnP instance id | `SWD\MMDEVAPI\{3.0.0.<sid>}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}`; `<sid>` = RDP session id as 8 hex digits (`00000002` to `00000005` seen, matching the RDP display adapters `RDPIDD_INDIRECTDISPLAY&SESSIONID_000N` of the same sessions) | [L] |
| IMMDevice endpoint id | `{3.0.0.<sid>}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}`, that is the instance id without the `SWD\MMDEVAPI\` prefix. This follows the public rule that an MMDEVAPI software-device instance id is `SWD\MMDEVAPI\` plus the endpoint id. Not read with `IMMDevice::GetId()` inside a seat yet | [I] |
| GUID suffix | identical for every session on the live box. Whether it is fixed by the RDP audio enumerator on every machine, or generated once per machine, is not known (only one machine with seat sessions was available) | [L] / [?] |
| Flow | render only. In public endpoint ids, the last of the three leading numbers is the flow (0 render, 1 capture). Reading `3.0.0` the same way gives render. No `3.0.1` node exists, which fits "no audio capture redirection" | [I] (from public id format), [L] (no `3.0.1` node) |
| Device properties | `Device_SessionId` (`{83DA6326-97A6-4088-9453-A1923F573B29}` pid 6) = the RDP session id; parent `HTREE\ROOT\0`; container id is the "local machine" null container. No user, seat or creator-product property | [L] |
| Global endpoint list | not listed under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render`; the endpoint is session-scoped | [L] |
| Provider | the RDP listener's audio enumerator: `WinStations\RDP-Tcp\AudioEnumeratorDll = rdpendp.dll` (Windows default, not changed by the product) | [L][S] |
| Format seen by the capture | 32-bit float, 48 kHz, 2.0 stereo. Only source: a log of an older (2024) build of the product; no 1.6 audio log exists on the box | [L] (old build) |

### 2.2 Lifetime

Derived from device-node dates, Kernel-PnP configuration events and Local Session
Manager events on the live box [L]:

| Event | What happens to the endpoint | Ev. |
| --- | --- | --- |
| Seat logon (RDP connection from 127.0.0.1) | endpoint node arrives in the same second as the logon | [L] |
| Same session id used again later | the existing node is reused (same instance id, new arrival date, first-install date unchanged) | [L] |
| Seat disconnect / logoff | the node becomes non-present. Unlike the RDP display adapter, it gets no removal date | [L] |
| Node deletion | rare: three deletions in the box's history, all of the session-2 node, one at the second of a seat logoff just before a shutdown. Other logoffs left the node in place. Which process deletes it is [?] | [L] |
| Phantom nodes | nodes of ended sessions stay as non-present nodes for months (oldest seen: March) | [L] |
| RDP client killed while the session stays alive | [?]. Expected: the session becomes disconnected and the endpoint goes away with the connection; a reconnect that keeps the session id brings back the same endpoint id [I] |

### 2.3 Why the endpoint exists, and who decides the audio mode

**Session-side decision.** The RDP server decides the audio mode from the client's
info packet (MS-RDPBCGR, TS_INFO_PACKET flags), limited by policy:

| Info flag (MS-RDPBCGR) | Value | Meaning | Reference client |
| --- | --- | --- | --- |
| `INFO_REMOTECONSOLEAUDIO` | `0x00002000` | play audio on the server ("leave at remote") | not set [S] |
| `INFO_NOAUDIOPLAYBACK` | `0x00080000` | no audio at all | not set [S] |
| `INFO_AUDIOCAPTURE` | `0x00200000` | redirect client microphone | not set [S] |
| `INFO_MOUSE`, `INFO_DISABLECTRLALTDEL`, `INFO_UNICODE`, `INFO_MAXIMIZESHELL`, `INFO_ENABLEWINDOWSKEY` | `0x1`, `0x2`, `0x10`, `0x20`, `0x100` | input / shell flags | set [S] |

With neither of the first two flags, the mode is "bring audio to this computer" (redirect
to client), and the server creates the *Remote Audio* endpoint [I, public protocol];
[L] confirms the endpoint for every seat session.

**Client channels.** The reference's headless client (`DuoRdp.exe`, a frozen Python
program built on the open-source *aardwolf* RDP library [S]) offers only clipboard
(MS-RDPECLIP) and the dynamic-channel transport (MS-RDPEDYC) with an echo channel [S].
It offers no `rdpsnd` static channel, and it opens neither the `AUDIO_PLAYBACK_DVC` /
`AUDIO_PLAYBACK_LOSSY_DVC` dynamic channels (MS-RDPEA) nor `AUDIO_INPUT` (MS-RDPEAI) [S].
The live box shows that the server creates the endpoint anyway: endpoints exist for all
seat sessions made by this client [L]. The session's applications render into it, and
the PCM is never played anywhere [I]. The host speakers stay silent.

**WRDS protocol provider (`DuoRds.dll`).** Its type library contains the names
`fNoAudioPlayback`, `fRemoteConsoleAudio` and `AudioDriverName` [S]. They sit among the
other members of the standard Windows protocol-provider structures (user config and
client data from the public `wtsprotocol.h`: mouse, maximise shell, client product id,
display driver name, socket address, time zone and so on). So they are the public
structure layout, not product-specific settings [S][I]. A provider would fill them in its
client-data callback, and then it could change the audio mode or driver name of a
session. On the live box this cannot happen: the only RDP listener (`RDP-Tcp`) loads the
stock Windows provider (`LoadableProtocol_Object = {5828227c-20cf-4408-b73f-73ab70b8849f}`),
and the product's provider class is not registered [L]. The effective audio mode of
seats therefore comes only from the headless client's info flags and the policies below.

**Isolation effects.**

- Each seat's sound is isolated by Windows itself: an RDP session sees its own
  redirected endpoint and not the console's physical devices [I, documented RDS
  behaviour]. Seats cannot hear each other, and the console never plays seat audio [I].
- Whether the audio engine paces the endpoint correctly when the client never opens a
  sound channel, and whether loopback then yields non-silent audio on 1.6, is [?]. The
  only proof of working capture is the 2024 log (§2.1).

### 2.4 Policies that affect the endpoint

Effective values on the live box [L], value meanings from the public
`TerminalServer.admx` [I]:

| Location | Value | Live | Meaning |
| --- | --- | --- | --- |
| `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services\fDisableCam` | 0 allow / 1 block audio playback redirection | absent | not configured; listener value applies |
| `...\Terminal Services\fDisableAudioCapture` | 0 allow / 1 block audio capture redirection | absent | not configured; listener value applies |
| `...\Terminal Services\AllowedAudioQualityMode` | 1 dynamic, 3 medium, 7 high | 3 (medium) | not set by the product (value name not in its binaries [S]) |
| `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp\fDisableCam` | as above | 0 | default |
| `...\RDP-Tcp\fDisableAudioCapture` | as above | 0 | default |

A policy value, when present, overrides the listener value [I, documented]. The quality
mode selects how the server encodes audio for the client on the wire. The 48 kHz float
format seen by loopback is the endpoint's shared-mode mix format inside the session, so
the quality mode should not change what the stream server records [I]. The 2024 log
does not show which quality mode was active then [?].

### 2.5 Device bookkeeping in the service

The service binary enumerates device nodes with these instance-id prefixes / classes:
`USB\COMPOSITE`, `HIDClass`, `XboxComposite`, `XnaComposite`, `DISPLAY`,
`TERMINPUT_BUS`, `SWD\MMDEVAPI\`. It reads `Device_SessionId` (pid 6) and imports the
SetupAPI device-removal call [S]. Its purpose is probably to map or clean up per-session
devices by session id [I]. The live evidence (§2.2) shows that audio endpoints are
usually **not** removed at seat stop or at boot [L]. If the service removes them at all,
it does so rarely [?].

### 2.6 Pulsar Split approach

- Keep the same model: **no audio driver**. Use the session-scoped RDP render endpoint.
  It needs no signing and no kernel code, and it works with Memory Integrity on.
- Headless RDP client requirements (owned by `30-session-rds.md`; listed here because
  audio depends on them):
  - Info packet: do **not** set `INFO_REMOTECONSOLEAUDIO` or `INFO_NOAUDIOPLAYBACK`.
    Set `INFO_AUDIOCAPTURE` only when the mic feature (§5.1) is on for the seat.
  - Channels: join the audio playback channel (static `rdpsnd`, or the
    `AUDIO_PLAYBACK_DVC` dynamic channel, MS-RDPEA). Answer the format negotiation,
    acknowledge every wave PDU with correct timestamps, and discard the samples. Then
    the server runs its normal, paced pipeline and does not depend on the untested
    behaviour of an unopened channel. Some clients announce `rdpdr` together with a
    static `rdpsnd`; check what the server needs during implementation [I].
  - Use a maintained RDP client library under a GPL-3-compatible licence (for example
    a Rust implementation under MIT/Apache-2.0), not a frozen interpreter.
  - Still to confirm: a client that joins the sound channel still gets a session
    endpoint that WASAPI loopback can read. Test with `mstsc` to `127.0.0.1` with
    "play on this computer" (expect the endpoint) and "leave at remote computer"
    (expect no *Remote Audio*; the session uses the console devices) (Open question 2).
- Do not register a WRDS protocol provider for audio. The stock provider plus correct
  client flags is enough.
- Policies: Pulsar Split does **not** write the `Terminal Services` policy keys. At
  seat start the service reads `fDisableCam` (policy first, then listener). If the
  effective value is 1, it marks the seat "audio blocked by policy" in its status and
  does not try to fix it; the operator owns policy. Leave `AllowedAudioQualityMode`
  alone (§2.4).
- Optional later: take the PCM straight from the audio playback channel instead of
  WASAPI loopback. This saves one copy and gives an exact per-session source. Keep
  loopback as the default, because upstream Sunshine already supports it.
- Device cleanup (Pulsar-own design; the reference gives no usable rule):
  - Seat identity is not stored on the node. The service keeps its own
    seat-to-session-id table (`20-service.md`) and the times each session started
    and ended.
  - Do not delete nodes after every seat stop. Windows reuses the node for the next
    session with the same id (§2.2), so deletion gains nothing.
  - Optional maintenance action, operator-triggered only: remove non-present
    `SWD\MMDEVAPI\{3.0.*}` nodes with the RDP GUID suffix whose `Device_SessionId` has
    no live session. Log every removal. Never touch present nodes, console devices or
    nodes of other classes.

## 3. Stream-server audio settings

### 3.1 Keys written by the reference service

Written into `config\<seat>.conf` at every seat start, same value for every seat [S][L]:

| Key | Value | Upstream meaning | Why the product sets it [I] |
| --- | --- | --- | --- |
| `virtual_sink` | `Remote Audio` | device used when the client does **not** want audio played on the host: the stream server makes it the default render device for the stream's duration and records it | make sure the capture targets the session's RDP endpoint even if the session default changes |
| `install_steam_audio_drivers` | `disabled` | upstream default would install Steam's *Steam Streaming Speakers* driver (needs admin) and use it as the virtual sink | a single, machine-wide Steam virtual device would be shared by all seats and mix their audio; it would also change devices on the console |

Not written, so the fork's (upstream) defaults apply [S][L]:

| Key | Upstream default | Effect in a seat |
| --- | --- | --- |
| `audio_sink` | empty = current default render device of the process's session | in a seat session this is the *Remote Audio* endpoint anyway [I] |
| `stream_audio` | enabled | audio is always streamed |

The fork's binary contains exactly these four audio keys (`audio_sink`, `virtual_sink`,
`stream_audio`, `install_steam_audio_drivers`); no product-specific audio keys were
added [S]. Upstream's Steam driver install step and default-device switching are still
present in the fork [S]; both stay idle in seats [I]. Upstream accepts either an
endpoint id or a friendly name in `audio_sink` / `virtual_sink` (public upstream
documentation) [I].

### 3.2 Client-driven parameters (upstream protocol, unchanged)

| Item | Where | Meaning |
| --- | --- | --- |
| `localAudioPlayMode` | RTSP launch/resume parameters | client's "play audio on host" choice; selects `audio_sink` (on) or `virtual_sink` (off) [S][I]. In a seat both resolve to *Remote Audio*, so the choice has no audible effect [I] |
| `surroundParams`, `x-nv-audio.surround.numChannels`, `x-nv-audio.surround.channelMask`, `x-nv-audio.surround.AudioQuality` | RTSP / SDP | requested channel layout (stereo, 5.1, 7.1) and quality [S] |
| `SUNSHINE_CLIENT_HOST_AUDIO`, `SUNSHINE_CLIENT_AUDIO_CONFIGURATION`, `SUNSHINE_CLIENT_AUDIO_SURROUND_PARAMS` | environment of launched apps | upstream app-environment variables describing the audio choice [S] |
| Codec | Opus (library 1.6.1 in the fork), low-delay mode; stereo at 96 kbps total observed [S][L] |
| Transport | UDP `base + 11`, with FEC [S][I] (see `20-service.md` §4) |

Upstream's source calls its loopback recorder a "microphone" internally (see the public
Sunshine source). That is the **loopback** capture, not microphone input [I]. Do not read
it as mic support.

### 3.3 Surround requests on a stereo endpoint

| Item | Behaviour |
| --- | --- |
| Endpoint mix format in a seat | stereo, 48 kHz, float (old-build log) [L]; not measured on 1.6 [?] |
| Client asks for 5.1 / 7.1 | upstream tries a list of capture formats, starting with the requested layout, and lets the Windows audio engine convert when the endpoint format differs (public upstream source) [I]. Expected result: a 5.1/7.1 Opus stream that holds the stereo mix on the front channels, with no real surround content [I]. Exact result on the RDP endpoint is [?] |
| Fork changes | none found in the audio keys or surround parameter names [S] |

### 3.4 Error behaviour (upstream, seat log)

Upstream behaviour, described in our own words; for the exact log text see the public
Sunshine source (`src/audio.cpp` and the Windows audio backend) [I]:

| Situation | Result |
| --- | --- |
| `virtual_sink` name does not match any endpoint of the session | an error is logged and capture falls back to the default device |
| No usable capture device or format | a warning says the stream has no audio; video and input continue |
| Endpoint invalidated (device change, session reconnect) | capture is re-initialised automatically |

The reference sets `min_log_level = none` for seats, so none of this is visible by
default [L]. The current seat logs on the box are empty (0 bytes) [L]. Only a 2024 test
configuration without that key produced audio log lines. The reference has no other
"no audio" signal: no status field, no event, no manager display [S][L].

### 3.5 Pulsar Split approach

- Write `install_steam_audio_drivers = disabled` for every seat (same reason).
- **Do not rely on the English friendly name.** It may be localized [?], and a
  non-English host would then fail the name lookup. Preferred order:
  1. Leave `virtual_sink` and `audio_sink` empty, so the stream server records the
     session's default device. In a seat that device is the session endpoint.
  2. If a sink must be set, write the **endpoint id** from §2.1 (session id of the
     seat as 8 hex digits). Build it only after reading the GUID suffix from the live
     node with that session id; never hard-code it (§2.1 GUID row is [?]).
- "No audio" status in the manager is a **Pulsar-own design** (the reference has
  none):
  - Our stream server reports its audio-capture state (capturing / no device / blocked
    by policy / disabled) to the service over the existing named-pipe IPC (`20-service.md`).
    The service puts the state into the seat status that the manager shows (Turkish copy).
  - Fallback for an unmodified upstream build: keep the seat log level at least
    `warning`, and let the service watch `config\<seat>.log` for upstream's
    "no audio" warning (match is version-dependent, so this is best effort).
  - Logs and status must never contain pairing material, PINs, certificates or keys.
- Expose to the manager (Turkish UI) at most: audio on/off per seat (`stream_audio`).
  No device picker: the seat has exactly one device.
- Surround: advertise and accept 5.1/7.1 as upstream does. Mark it in the docs as
  "stereo content" until a test shows otherwise.

## 4. Steam audio driver handling

| Aspect | Reference behaviour |
| --- | --- |
| Per-seat install by the stream server | disabled by config (§3.1) [S][L] |
| Global Steam devices | Steam itself may install *Steam Streaming Speakers* and *Steam Streaming Microphone* (`ROOT\STEAMSTREAMINGSPEAKERS\0000`, `ROOT\STEAMSTREAMINGMICROPHONE\0000`); present on the live box, installed by Steam, not by the product [L][I] |
| Visibility in seats | those are console/global endpoints; an RDP session with redirected audio sees only its own endpoint [I] |
| Steam isolation | per-seat Steam instances (shim, `50-shim-isolation.md`) do not change audio [S] |

Pulsar Split approach: never install, remove or reconfigure Steam's drivers. If the
operator has them, leave them alone. Document that Steam Remote Play inside a seat is
not a supported audio path.

## 5. Microphone

| Path | Status |
| --- | --- |
| Client to host mic over the Moonlight protocol | the protocol has no microphone stream; the fork adds none [S][I] |
| RDP audio input (MS-RDPEAI `AUDIO_INPUT` dynamic channel, server-side capture endpoint) | the headless client neither sets `INFO_AUDIOCAPTURE` nor implements the channel [S]; no capture-flow *Remote Audio* node exists for seat sessions [L] |
| `fDisableAudioCapture` | 0 on the listener, no policy value (defaults, not changed by the product) [L][S] |
| Physical mics | belong to the console; not visible in seats [I] |

Result: apps in a seat (voice chat) see **no microphone** [I].

### 5.1 Pulsar Split approach

Possible later feature, no driver needed:

```
 Pulsar client mic --(Pulsar extension stream, Opus)--> stream server of seat N
        --> service-owned headless RDP client for seat N
        --> RDP audio-input channel (MS-RDPEAI) --> Windows creates the session's
            capture endpoint --> apps in seat N record it
```

- The headless client sets `INFO_AUDIOCAPTURE` and implements the audio-input dynamic
  channel (PCM 16-bit 48 kHz mono/stereo), feeding it the decoded mic samples. Windows
  then provides a per-session capture endpoint (expected id form `{3.0.1.<sid>}...`,
  [I]); no virtual audio driver, no signing [I].
- The extension stream stays inside the existing streaming protocol and its existing
  ports (for example as an extra packet type on the control or audio channel). It adds
  **no** HTTP or web endpoint and no new listening port, in line with the
  "no local web server" decision.
- Stream server to headless client: local named-pipe IPC, ACL limited to the service.
- Needs a protocol extension between Pulsar client and our stream server; Moonlight
  clients stay mic-less. Off by default; per-seat toggle; never forward mic audio to any
  session other than the owner's. The feature fails if `fDisableAudioCapture` is 1 by
  policy; report that in seat status.
- Rejected alternative: a virtual audio (portcls/AVStream) driver per seat. It is
  kernel-mode, needs attestation or WHQL signing to load with Secure Boot, and adds
  per-session routing problems.

## 6. Controller audio: speaker, headset jack, audio haptics

| Aspect | Reference behaviour |
| --- | --- |
| Virtual pads in seats | UMDF HID minidriver devices only (DualSense, DualSense Edge, DualShock 4, Xbox One Elite identities) [S][L]; no USB audio function, so no DualSense speaker or headset endpoints exist for virtual pads [S][I] |
| Physical DualSense pads on the host | expose USB audio endpoints (speaker, headset microphone) in the console only [L] |
| Per-pad output mapping (`Global\Duo_<padId>_output` + event) | carries the HID **output report** the game sent to the virtual pad: rumble, trigger effects, light bar, player LEDs [S]. Forwarded to the client through Moonlight's control channel (see `60-controllers.md`) [S][I] |
| Haptics handling | no binary of the product, and not the fork, contains any haptics-specific logic or naming [S]. Audio-driven haptics need the pad's 4-channel USB audio endpoint, which does not exist in a seat [I] |
| Haptic samples inside HID output reports (Bluetooth-only report variants) | [?]. If a game sends them, they would be inside the forwarded report, but nothing in the reference decodes them, and Moonlight's protocol has no haptics stream [I] |
| DualSense speaker, headset jack, mic | not available in seats [S][I] |

**Consistency note for `00-architecture.md` §5.3.** That table lists "audio-haptics"
among the contents of the output mapping. The static evidence above supports only
"HID output report (rumble, trigger effects, LEDs)". The 00 row should be read that way
until a live dump of the mapping during a haptics-capable game shows otherwise
(Open question 9). This spec does not change 00; the owner of 00 should reword it.

### 6.1 Pulsar Split approach

- v1: same limitation; the manager states it (Turkish copy) on the controller page.
- Possible later research (Phase 4, **no commitment**), only with the Pulsar client:
  carry DualSense haptic/speaker PCM from the seat to the client as an extension
  stream (same rule as §5.1: inside the existing protocol, no web endpoint). The open
  problem is the **source** inside the seat. Games look for a DualSense-named
  4-channel audio endpoint, which only an audio driver can provide, and an RDP session
  has one redirected render endpoint [?]. A kernel audio driver is out of scope unless
  it is properly signed (attestation or WHQL), HVCI-compatible and works with Memory
  Integrity on. Test-signing is never acceptable for users. This is a signing and cost
  question, not a technical blocker.

## 7. Interaction with other components

| Component | Audio relevance |
| --- | --- |
| Session creation (`30`) | must keep audio mode "redirect to client" (§2.3 flag table); ending or reconnecting the RDP connection invalidates the endpoint, and the stream server re-initialises capture [I] |
| Display module (`40`) | none |
| Shim (`50`) | none in the reference [S]; Pulsar Split's isolation must not hook audio APIs either |
| Stream server process (`80`) | must run **inside the seat session**, because the session-scoped endpoint is only visible there. The reference uses a duplicated SYSTEM token with the session id set [S][I]. WASAPI loopback from a SYSTEM process in that session works; upstream service mode does the same in the console [I] |
| Firewall | audio UDP port `base + 11` per seat must be in the seat's rule set |
| Coexistence with the reference product | no shared audio state; both use Windows' per-session endpoint [I]. An upstream Sunshine install in the console (seen on the live box) is unaffected [L][I] |

## 8. Legal / signing notes (factual)

- The RDP redirected audio endpoint uses only Windows components. Audio needs no
  redistribution or patching of Microsoft binaries. Multi-session on client SKUs
  remains the licence issue described in `00-architecture.md` §8.
- The MS-RDPBCGR, MS-RDPEA and MS-RDPEAI specifications are public Microsoft Open
  Specifications. Implementing a client from them is covered by Microsoft's Open
  Specifications Promise [I]; check the current terms before release.
- Any future virtual audio or haptics driver is kernel-mode. Secure Boot systems need
  Microsoft attestation signing (paid EV certificate) or WHQL. Test-signing mode is not
  acceptable for users. The RDP-channel designs above avoid this.
- Steam's drivers are Valve's; Pulsar Split must not bundle or install them.

## 9. Open questions

| # | Question | How to resolve |
| --- | --- | --- |
| 1 | Is the *Remote Audio* friendly name localized? | run a seat on a Turkish-display-language Windows; read the friendly name of the new node |
| 2 | Does an RDP session whose client never joins a sound channel pace audio correctly, and does loopback on 1.6 give non-silent audio? A client that joins `rdpsnd` / `AUDIO_PLAYBACK_DVC`: does it still get an endpoint that loopback can read? | start a seat with `min_log_level` raised (needs maintainer approval, changes config), play a test tone, record; repeat with `mstsc` to 127.0.0.1 in both audio modes |
| 3 | `IMMDevice::GetId()` value inside a seat, and whether the GUID suffix `{6C26BA7D-...}` is the same on other machines | enumerate render endpoints in a seat on two machines |
| 4 | Real `3.0.1` node for capture redirection | `mstsc` session with microphone redirection on; check for a `{3.0.1.<sid>}` node |
| 5 | Endpoint behaviour when the headless client dies but the session stays | kill the client of a running seat; watch the node and the stream server's re-initialisation |
| 6 | Mix format of the session endpoint (`GetMixFormat`) and the actual result of 5.1/7.1 requests | runtime test in a seat |
| 7 | Does `AllowedAudioQualityMode` change the in-session mix format? (expected: no, §2.4) | runtime test with the policy at 1/3/7 |
| 8 | Which process deleted the session-2 node (three times), and whether the service removes any audio nodes at seat stop | process-level trace (Procmon / SetupAPI log) of a seat stop and of a shutdown with seats running |
| 9 | Content of the output mapping while a haptics-capable game runs (do Bluetooth-style haptic reports reach it?) | dump the mapping during play |
| 10 | Can any seat-side path feed DualSense audio haptics without a kernel audio driver? | Phase 4 research |
