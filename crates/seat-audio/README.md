# seat-audio

The one audio crate for endpoint discovery, shared by both seat kinds, plus the remote kind's
audio rules (spec `spec/remote/70-audio.md`, contract `docs/split/agents/contracts.md` §6).
GPL-3.0-only.

## Shared endpoint discovery (merge step 8)

| What | API | Used by |
| --- | --- | --- |
| CfgMgr32 read primitives: device id lists, devnode lookup (present / phantom), typed properties (string, list, UINT32, GUID), UTF-16 helpers | `seat_audio::cfgmgr` (Windows) | `seat_media::pnp::WinPnpTree` (local kind's device tree), `CfgMgrNodes` (remote kind's endpoint list) |
| Endpoint-id model: `SWD\MMDEVAPI\` prefix, `{a.b.f.hhhhhhhh}.{guid}` parsing, session id field | `MMDEVAPI_PREFIX`, `mmdevapi_instance_id`, `parse_endpoint_id`, `is_session_endpoint_id` | `seat_media::audio` (skips other sessions' endpoints), this crate (finds a remote seat's own endpoint) |
| `DEVPKEY_Device_SessionId` on endpoint devnodes | `DeviceNodes::list` / `AudioProbe::endpoints` | remote kind |

Routing stays per kind:
- **local**: `seat-media` bridges the RDP audio channels to the seat's physical endpoints
  (`rdp_bridge`, MMDevice / WASAPI).
- **remote**: the stream server records the session's Remote Audio endpoint (this crate's
  keys and state).

`is_session_endpoint_id` is the coarse marker of the local kind: the leading number is 3.
The remote kind uses the strict form, `parse_endpoint_id(..).is_rdp()`.

## Remote kind

## Purpose

Pulsar Split ships **no audio driver**. Every seat is an RDP session, and Windows creates
a session-scoped *Remote Audio* render endpoint for that session. The seat's stream server
runs inside the session and records it with WASAPI loopback. This crate holds the logic
around that model:

| Area | API |
| --- | --- |
| Endpoint discovery (CfgMgr32, `DEVPKEY_Device_SessionId`) | `AudioProbe::endpoints`, `session_endpoint`, `session_capture_endpoint`, `parse_endpoint_id` |
| Terminal Services policy (read only: `fDisableCam`, `fDisableAudioCapture`, `AllowedAudioQualityMode`) | `AudioProbe::policy`, `read_policy`, `resolve_policy` |
| Stream-server keys | `server_keys` (`stream_audio`, sinks omitted unless pinned to a live endpoint id; `install_steam_audio_drivers = disabled` is written by `seat-stream`, open question audio-7) |
| Headless RDP client flags | `rdp_client_flags` -> `RdpAudioFlags::{info_flags, helper_args}` |
| Seat state for the manager | `seat_audio_state`, `scan_log_line`, `LogScanner`, `seat_audio_status` |
| Documented limitations | `limitations` (`no_microphone`, `no_controller_audio`, `steam_remote_play_unsupported`, `surround_is_stereo`, `policy_blocks_audio`, `mic_policy_blocked`) |
| Steam drivers | `AudioProbe::steam_devices_present` (report only; never installed/removed) |
| Phantom-node maintenance (operator-triggered only) | `plan_stale_endpoints` / `plan_stale_nodes`, `rdp_guid_suffixes`, `remove_stale_endpoints`, `EndpointRemover` |

`AudioState::text_tr` / `AudioLimitation::text_tr` give suggested Turkish copy; the
integrator owns the manager's `i18n.ts`.

## Design

- Layers: `DeviceNodes` (CfgMgr32) and `RegistryRead` (HKLM DWORD) are thin system
  traits. `SystemProbe<N, R>` builds the contract's `AudioProbe` on top of them, so the
  probe logic is tested with fakes. Real impls (`#[cfg(windows)]`, `src/win.rs`):
  `CfgMgrNodes`, `WinRegistry`, `CfgMgrRemover`; `system_probe()` returns the production
  probe.
- An endpoint is a seat's only when its id has the RDP form `{3.0.f.hhhhhhhh}.{guid}`
  (`guid` a well-formed hex GUID)
  **and** its session id matches. Console endpoints also report `Device_SessionId = 1`
  (seen on the dev laptop), so the session id alone is not enough.
- Nothing matches on the friendly name (it may be localized, spec 70 OQ 1). The GUID
  suffix is never hard-coded: a pin uses the endpoint id read from the live node.
- Policy is read, never written. `fDisableCam != 0` => `blocked_by_policy`.
- `seat_audio_state` order: seat setting off -> `disabled`; playback blocked ->
  `blocked_by_policy`; stream-server report (if any, not `unknown`); missing or
  non-present endpoint -> `no_device`; present endpoint without a report -> `unknown`.
- `RdpAudioFlags.playback` is always true (never `INFO_REMOTECONSOLEAUDIO` /
  `INFO_NOAUDIOPLAYBACK`); `capture` only when the seat's backend can deliver a mic
  (`mic_path`, Pulsar seats only), the mic feature is on and capture is not blocked (no
  capture endpoint that nobody can feed).
- Microphone (2026-11-14, spec 70 §5.1): Pulsar seats only. `pulsar-host` mic pipe ->
  service -> headless hold stdin -> MS-RDPEAI (pulsar-host-backend.md §3.8). `limitations`
  / `seat_audio_status` take `mic_path`; a Sunshine seat with the switch on lists
  `no_microphone` + `mic_needs_pulsar`. Untested on hardware (HYPOTHESIS: the session gets
  a capture endpoint and apps see a microphone).
- Stale-node removal (spec 70 §2.6) plans only non-present `SWD\MMDEVAPI\{3.0.f.<sid>}.{guid}`
  nodes whose session id came from `DEVPKEY_Device_SessionId` (never the id fallback),
  whose `guid` equals the suffix of a present, live, property-matched seat endpoint (read
  live, never hard-coded; none connected => nothing planned), and whose session is not
  live. The WTS list must contain session 0 and is re-read before each removal; the
  remover re-checks presence and the session-id property just before uninstalling. The
  caller must hold its seat-start lock meanwhile (open question audio-8). It writes no
  restore record (not reversible; Windows recreates the node; open question audio-3).
- `scan_log_line` matches only messages that start with known text after the upstream
  `[timestamp]: Level: ` prefix, so client / app / sink names cannot flip the status.
  Strings come from the public upstream Sunshine source (GPL-3,
  `src/audio.cpp`, `src/platform/windows/audio.cpp`, master 2026-11). Best effort and
  version-dependent (open question audio-4).

## Build and test

```
cargo test -p seat-audio
cargo test -p seat-audio --all-features   # includes tests/stream_conf.rs (needs seat-stream)
cargo clippy -p seat-audio --all-targets
cargo check -p seat-audio --all-targets --target x86_64-pc-windows-msvc
# Read-only smoke test against this machine's device tree and HKLM (changes nothing):
cargo test -p seat-audio live_read_only_probe -- --ignored --nocapture
```

Feature `fake` exports `seat_audio::fake` (`FakeAudioProbe`, `FakeDeviceNodes`,
`FakeRegistry`, `FakeRemover`) for the integrator's tests.

## Untested

- Everything on a real seat host: discovery of `{3.0.0.<sid>}` endpoints, the session-id
  property on them, and the id fallback. The dev laptop has no RDP seat sessions; the
  read-only smoke test there sees only console endpoints.
- `CfgMgrRemover` (`CM_Uninstall_DevNode`): needs SYSTEM/admin and phantom RDP nodes;
  never run.
- `scan_log_line` against real logs of the pinned upstream tag.
- Whether loopback on the session endpoint gives non-silent audio when the helper joins
  the playback channel (spec 70 OQ 2) — session / stream packages.

No third-party code is included. Dependencies: `serde`, `thiserror`, `log`, `winreg`,
`windows-sys` (MIT/Apache-2.0), `seat-proto` (was `split-proto`); dev-only `seat-stream`, `serde_json`.
