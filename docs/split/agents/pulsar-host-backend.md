# Pulsar host backend (`pulsar-host`) — contract

> **Superseded (merge step 10, 2026-11-14).** Pulsar Split is merged into Pulsar Seat. This file is kept as history and is not updated: names such as `pulsar-split-service`, `PulsarSplit.Control`, IPC v2, `installer/split/` and `split-*` crates are gone. Current facts: [AGENTS.md](../../../AGENTS.md), `docs/agents/`, `installer/README.md` and the package READMEs. **Still binding:** the `pulsar-host` launch and pipe contract below stays unchanged (merge-plan.md §5); only names outside that contract may be out of date.

> **Merge review note (2026-11-14).** Wire strings stay as written here (hello capability `split_pads`, pipe `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host`, mutex `Global\PulsarSplit_Seat_<seat-id>_PulsarHost`). Two facts moved: the data root is `%ProgramData%\Pulsar Seat` (seat data dir `...\Pulsar Seat\seats\<seat-id>\pulsar\`) and the binary is under `<install>\pulsar-host\`. Both reach `pulsar-host` only via argv; pulsar-desktop checks only that `--data-dir` / `--log` are absolute (`desk:src-tauri/src/headless_host.rs:97`), so no change is needed for them. **Open:** §3.1's server check compares the pipe server's image with `pulsar-split-service.exe` (`desk:src-tauri/src/seat/win.rs:18`), and the merged service is `pulsar-seat-service.exe`: a real `pulsar-host` would refuse the pipe. That needs a pulsar-desktop change (accept both names) or a maintainer decision; see docs/agents/status.md.

Index: [../../AGENTS.md](../../../AGENTS.md). Status: designed 2026-11-12; **service side
implemented 2026-11-13** (contracts §7.4, `seat_stream::backend`), untested on a real
host. `pulsar-host` itself is **not built** yet. The changes it needs in pulsar-core /
pulsar-desktop (§6) are future work in those repos. Where the implementation differs
from this note, the place says so (OQ stream-backends-1..5).

## 0. Decision and scope

- Maintainer decision (2026-11-12): Pulsar becomes a **second stream-server backend**
  next to upstream Sunshine. Sunshine stays, **unforked**, for Moonlight clients.
- Each seat picks **one** backend: `sunshine` (Moonlight) or `pulsar` (Pulsar clients).
  No seat runs both at once (one capture, one input owner per session).
- `pulsar-host` = a headless process built from our own GPL-3 repos pulsar-core and
  pulsar-desktop. Reading those repos is allowed; they are not third-party (CLEANROOM.md).
- The service treats `pulsar-host` like the Sunshine child: same launcher, same job,
  same watchdog, same seat directory ACL. Only the control channel and the config
  delivery differ.
- Own drivers (IDD, pad driver) stay optional and are **never the default** (plan.md).

File:line citations below are relative to `../pulsar-core/crates/pulsar-core/src/`
(prefix `core:`) and `../pulsar-desktop/` (prefix `desk:`), read 2026-11-12.

## 1. Launch

| Item | Value |
| --- | --- |
| Binary | `pulsar-host.exe`, installed under `%ProgramFiles%\Pulsar Split\pulsar-host\`. Not vendored. Pinned in a manifest like the Sunshine one (`seat_stream::dist`: version, file, SHA-256, source URL, licence). Built for x64 and ARM64 (pulsar-desktop already ships both; Sunshine is x64 only). |
| Started by | the service, through `seat_stream::supervise::ServerLauncher::launch_system_in_session` (the same `crate::win::launch::SessionLauncher` as Sunshine): service SYSTEM token duplicated, `TokenSessionId` = seat session, `CreateProcessAsUserW` on `winsta0\default`, suspended, no inherited handles, assigned to the seat's kill-on-close job, then resumed. `lock_binary` verifies the hash and the install-folder ACL first. |
| argv | `pulsar-host.exe --split-seat <seat-id> --data-dir "<abs data dir>" --log "<abs data dir>\pulsar-host.log"`. Nothing else. No secret on argv, ever; `pulsar-host` refuses to start if it sees any secret-carrying flag (pulsar-desktop has one today: `--connect-pw`, `desk:src-tauri/src/lib.rs:378`). |
| Environment | the SYSTEM environment block (`CreateEnvironmentBlock`), like Sunshine. No `PULSAR_*` overrides are set; `pulsar-host` ignores the dev env switches in split mode (see §6, item 11). |
| Working dir | the install folder `%ProgramFiles%\Pulsar Split\pulsar-host\`. |
| Data dir | `%ProgramData%\PulsarSplit\seats\<seat-id>\pulsar\`. It is created by the service inside the existing seat dir, so it inherits `SEAT_DIR_SDDL` = `O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` (`crates/seat-stream/src/seatdir.rs:30`). The seat user has no access. |
| Data-dir contents | `paired_devices.json` (trusted client public keys + names, not secret but integrity-critical), `pulsar-host.log` (+ rotated `.1`..`.3`, rotated by the service before each start as for `server.log`). Nothing else. **No** `identity.key`, **no** `config.json` with passwords: secrets come over the pipe (§3.3). |
| Pipe | created by the service **before** the launch (§3.1). `pulsar-host` derives the name from `--split-seat`; it is not passed separately. |
| Stop | `server.shutdown {grace_ms}` over the pipe, wait `grace_ms` + 1 s, then terminate (same as spec 80 §10). Closing the seat job kills it if the service dies. |
| Restart | `seat_stream::supervise::server_policy()` (1 s doubling to 60 s, give up after 5 failures). |
| One process per seat | yes. No machine-wide lease (§2). |

The service writes no config file for `pulsar-host`. All settings arrive with
`config.apply` right after `hello`. This keeps one source of truth (the seat record in
`HKLM\SOFTWARE\PulsarSplit`) and avoids a writable config that could grant access.

## 2. Per-seat isolation inside `pulsar-host`

Today's desktop app assumes one Pulsar per Windows user, or one per machine in shared
mode. A seat host must assume **one per seat, all running as SYSTEM**, and a seat user
may also run their own Pulsar desktop app inside the same session.

| Today (pulsar-desktop / pulsar-core) | Problem in a seat | `pulsar-host` rule |
| --- | --- | --- |
| `Local\PulsarSingletonMutex` (`desk:src-tauri/src/lib.rs:195`) | `Local\` = the session namespace: `pulsar-host` and the seat user's own Pulsar app would block each other | not taken. Instead `Global\PulsarSplit_Seat_<seat-id>_PulsarHost`, DACL `D:P(A;;GA;;;SY)`, created with "fail if exists" semantics: a second `pulsar-host` for the same seat exits with code 3 |
| `Local\PulsarShowEvent` (`desk:src-tauri/src/lib.rs:207`) | no UI | not created |
| `Global\PulsarSharedHost` lease + `%ProgramData%\Pulsar\shared\` (`desk:src-tauri/src/shared_mode.rs:11`, `:599`, `:392`) | one host per machine; the lease is dropped when the session is not Active (`:452`, `:509`) | split mode **bypasses shared mode completely**: no lease, no `mode.json`, no `%ProgramData%\Pulsar\` access, `may_host` always Ok |
| Identity file `identity.key`, 32 raw secret bytes, written in plaintext (`core:crypto.rs:138`, `:150`) | secret in a file | identity **per seat**, generated by the service (32 random bytes), stored DPAPI machine scope under `HKLM\SOFTWARE\PulsarSplit\Secrets` (same store and DACL as seat passwords, architecture.md "Access control"), sent in `secrets.load`; `pulsar-host` uses `Identity::from_secret_bytes` (`core:crypto.rs:127`) and never writes it |
| Relay registration (`desk:src-tauri/src/host.rs:569`, `:585`; `core:connection/node.rs:353`) | — | one registration per seat; per-seat identity gives each seat its own stable 9-digit relay ID |
| Relay access key `relay_keys` in `config.json` (`core:config.rs:55`) | secret in a file | issued key goes to the service (`relay.key_issued`), stored DPAPI per seat, returned in `secrets.load` |
| Device name: `announce_name` replaces a name equal to the host name or the default by the OS display name (`desk:src-tauri/src/util.rs:896`) | all seats would show the same name | announced name = **seat display name**, verbatim (unique case-insensitive already, spec 80 §7.3) |
| Node UDP port: `node_port`, 0 = ephemeral (`core:config.rs:111`) | — | default: **seat base port, UDP** (inside the seat's reserved block p−5..p+21, so `split_core::ports` already keeps it free). Option: ephemeral (`0`), then the firewall rule is program-scoped (§3.6, OQ pulsar-host-10) |
| LAN beacon UDP 21117 multicast with address reuse (`core:discovery.rs:30`, `:9`) | several seats on one box | fine: each seat beacons its own ID and name. Option per seat, default on |
| NAT-PMP / UPnP port mapping, default on (`core:config.rs:66`; `core:connection/portmap.rs:28`) | N mappings from one host; spec 80 keeps `upnp = disabled` for Sunshine | default **off** for seats; seat option |
| In-app relay (`desk:src-tauri/src/local_relay.rs`) | a relay inside a seat host | disabled in split mode |
| `%ProgramData%\Pulsar\duo-pads.json` pad registry + reaper (`core:input/pad_cleanup.rs:7`, `:629`) | a second machine-wide pad reaper next to seat-pads | not used: pads belong to seat-pads (§5) |
| `pulsar-sysagent` helper pipe `\\.\pipe\pulsar-sysagent-<session>` (`desk:crates/pulsar-sysagent/src/helper.rs:2`) | not needed: `pulsar-host` already runs as SYSTEM in the session | not used; the injector logic runs in-process (§6) |
| Elevation relaunch (`desk:src-tauri/src/main.rs:107`) | — | skipped (SYSTEM) |

Separation between seats rests on the seat id in every name plus the per-seat data dir.
All seat hosts run as SYSTEM, so this is a correctness separation, not a security
boundary (same caveat as spec 80 §3.4).

## 3. Control channel

### 3.1 Pipe

| Item | Value |
| --- | --- |
| Name | `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host` |
| Server | **the service** (different from Sunshine's spec 80 §8.4 pipe, where the server process owns it). The service creates the pipe before it launches `pulsar-host`, with `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS`, byte mode, max 1 instance. Reason: no squatting window, and secrets are handed over before `pulsar-host` opens any network socket |
| DACL | Designed `O:SYD:P(A;;GA;;;SY)(A;;GA;;;BA)`; **implemented** `D:P(A;;GA;;;SY)(A;;GA;;;BA)` (`HOST_PIPE_SDDL`, no owner; OQ stream-backends-3). SYSTEM + Administrators, as rules.md requires for control pipes |
| Client check (service side) | `GetNamedPipeClientProcessId` = the launched pid **and** its creation time = `Launched::creation_time`. Any other client (an elevated admin tool, a second process) is disconnected before `hello`. The Administrators ACE therefore grants no control |
| Server check (`pulsar-host` side) | `GetNamedPipeServerProcessId` = a process running as SYSTEM in session 0 whose image is `pulsar-split-service.exe` |
| Framing | seat-stream framing, unchanged: u32 LE length + UTF-8 JSON, max 1 MiB (`crates/seat-stream/src/control/frame.rs:7`). Reuse `write_frame` / `read_frame` / `FrameBuf` |
| Envelope | spec 80 §8.4 envelope, unchanged: request `{id, type, body}`, response `{id, ok, body \| error{code, detail}}`, event `{event, body}`. Requests go service → `pulsar-host`; `pulsar-host` speaks only in responses and events (as the Sunshine fork does for `pad.wanted`) |
| Protocol | `protocol: 1`, separate from the spec 80 seat-pipe protocol |
| Timeouts | request 10 s (`REQUEST_TIMEOUT`), `pulsar-host` must send `hello` response within 30 s of launch (`CONNECT_WINDOW_MS`) |
| Loss | broken pipe = `pulsar-host` exits at once (it must not run uncontrolled); the service treats a broken pipe as a crash |

Secrets travel only in `secrets.load`, `auth.otp.*`, `relay.auth` and `relay.key_issued`,
as `split_proto::Secret` strings; both sides zeroize buffers after use and never log
them (the service already wipes control-pipe frames).

### 3.2 Handshake

1. Service accepts the connection and checks the client (§3.1).
2. Service sends `hello {protocol: 1, service_version, seat_id}`.
3. `pulsar-host` answers `{protocol: 1, host_version, core_version, seat_id,
   capabilities}`. Capabilities (strings): `approve`, `connect_password`, `otp`,
   `trust_store`, `apps`, `app_launch_via_service`, `display_mode`, `audio_pinned`,
   `split_pads`, `split_idd` (only when built with the frame-source hook, §4).
   Seat-id mismatch or protocol mismatch: error `unsupported_protocol`, both close.
4. Service sends `secrets.load`, then `config.apply`. Only after both succeed does
   `pulsar-host` bind its node, start the beacon and register with the relay.

### 3.3 Requests (service → `pulsar-host`)

| `type` | Body | Response body |
| --- | --- | --- |
| `hello` | §3.2 | §3.2 |
| `secrets.load` | `identity` (Secret, 32 bytes base64), `connect_password`? (Secret), `relay_key`? (Secret), `totp_secret`? (Secret, reserved, OQ pulsar-host-7) | – |
| `config.apply` | `display_name`, `relay` (host[:port], default port 21116), `network_mode` (`auto` / `p2p-only` / `relay-only`), `node_port` (u16, 0 = ephemeral), `port_mapping` (bool), `lan_discovery` (bool), `access` (`approve` = popup only, `approve_or_password` = default), `transmit_audio` (bool), `audio_endpoint_id`? (string), `render_adapter_luid`? (u64), `capture` (`auto` / `dxgi` / `wgc` / `split_idd`), `codecs` (list), `max_clients` (u8, default 1) | `applied`: list of keys applied live, `restart_needed`: list of keys that need a restart (the service then restarts only when no stream is active, as for Sunshine) |
| `status` | – | `state` (`starting` / `registering` / `online` / `offline` / `stopping`), `relay` {`state`: `ok` / `auth_required` / `unreachable` / `version_mismatch`, `id`?}, `node_port`, `clients_connected`, `stream_active`, `capture` {`source`: `dxgi` / `wgc` / `dxgi-mf` / `ffmpeg` / `split_idd`, `error`?}, `encoder`, `video`? {`width`, `height`, `fps`, `hdr`}, `audio_state`, `pending_auth` (bool) |
| `auth.decide` | `request_id`, `allow` (bool), `view_only` (bool) | – |
| `auth.otp.get` | – | `otp` (Secret) |
| `auth.otp.rotate` | – | `otp` (Secret) |
| `trust.list` | – | `devices`: [{`key` (64 hex, public key), `name`, `label` (relay id or address at pairing time), `paired_at`, `last_seen`}] (shape of `desk:src-tauri/src/pairing.rs:24`) |
| `trust.revoke` | `key` | – (also ends that device's live sessions) |
| `trust.revoke_all` | – | – |
| `clients.list` | – | `clients`: [{`sid`, `label`, `device_name`, `mode` (`remote` / `game`), `view_only`, `app`?}] |
| `clients.kick` | `sid` | – |
| `clients.set_view_only` | `sid`, `view_only` | – |
| `apps.set` | `apps`: [{`id`, `name`, `image`?}] (ids and names only; the command lines stay in the service, §3.5) | `count` |
| `app.launched` | `request_id`, `ok`, `pid`?, `error`? | – (answer to the `app.launch` event) |
| `relay.auth` | `password` (Secret), `totp`? (Secret) | – (outcome via `relay.state`) |
| `display.mode.result` | `request_id`, `applied` (bool), `w`, `h`, `hz`, `hdr`, `error`? | – |
| `pad.attach` / `pad.denied` / `pad.detach` | spec 60 §3.5, unchanged | spec 60 §3.5 |
| `log.level` | `level` (`error` / `warn` / `info` / `debug`) | – |
| `server.shutdown` | `grace_ms` (default 5000) | – then exit 0 |

Error codes: `bad_request`, `unsupported_protocol`, `not_found`, `no_pending_request`,
`busy`, `not_ready` (request before `secrets.load` / `config.apply`), `internal`.

### 3.4 Events (`pulsar-host` → service)

| `event` | Body | When |
| --- | --- | --- |
| `auth.request` | `request_id` (u32), `kind` (`connect` / `pair`), `label`, `device_name`, `key_fp` (first 16 hex of the public key), `password_state` (`none` / `wrong` / `wait`), `received_at`, `expires_in_s` (connect: 120, pair: 40; from `desk:src-tauri/src/auth.rs:522`, `:651`) | an incoming session (or pair request) needs the operator. Replaces the Allow/Deny popup (`desk:src-tauri/src/auth.rs:358`, `:472`, `:636`) |
| `auth.ended` | `request_id`, `result` (`allowed` / `allowed_view_only` / `denied` / `password` / `paired` / `timeout` / `client_gone` / `locked_out`) | the request finished, whichever way |
| `otp.changed` | – (no secret) | the one-time password rotated (after use, or after the rotation cooldown `desk:src-tauri/src/auth.rs:79`; `:59` is the global failure throttle); the manager re-reads it with `auth.otp.get` only while its dialog is open |
| `trust.changed` | – | a device was paired or revoked |
| `relay.state` | `state`, `id`? | registration changed |
| `relay.auth_required` | `password` (bool), `totp` (bool) | the relay asks for credentials (`RELAY_AUTH_REQUIRED`, built in `desk:src-tauri/src/host.rs:2616`, tested in `headless.rs:64`) |
| `relay.key_issued` | `key` (Secret) | the relay issued an access key; the service stores it |
| `stream.started` / `stream.ended` | `sid`, `label`, `mode`, `app`?, `video`? / `sid`, `reason` | a client stream starts / ends |
| `capture.error` | `code` (`no_output` / `access_lost` / `no_frames` / `encoder_failed` / `adapter_mismatch` / `producer_lost`, or null = recovered) | capture failed or recovered |
| `display.mode.request` | `request_id`, `w`, `h`, `hz`, `hdr` | a client wants a mode (same shape as `seat_stream::control::ServerEvent::DisplayModeRequest`) |
| `audio.state` | `state` (`capturing` / `no_device` / `silent` / `disabled`), `endpoint_id`? | audio capture changed |
| `app.launch` | `request_id`, `app_id`, `sid` | a client picked an app (§3.5) |
| `pad.wanted` / `pad.released` | spec 60 §3.5; additionally `kind` may be `x360`, and `client_type` may be `pulsar` | a client controller arrived / left |

### 3.5 Apps

- The seat keeps **one** app list in `apps.json` (upstream format, `seat_stream::apps`)
  for both backends. The manager form does not change.
- For `pulsar`, the service sends only ids and names (`apps.set`). When a client picks one,
  `pulsar-host` sends `app.launch`; the **service** starts it as the seat user with the
  existing autostart path (shim launcher wrap when isolation is on) and answers
  `app.launched {pid}`. `pulsar-host` uses the pid only to find the window for per-window
  capture. Reason: today `launch_host_game` spawns as the host process's own user
  (`desk:src-tauri/src/process.rs:1200`, `:1214`), which would be **SYSTEM** here.
- The "Masaüstü" entry (no command) launches nothing; the service answers
  `app.launched {ok: true}` without `pid`.

### 3.6 Firewall

Per seat, like spec 80 §9.2, program = `pulsar-host.exe`, profiles Private (+ Domain),
never Public by default. Rule `PulsarSplit seat <seat-id> UDP`: the node port (default
= base). TCP: none (the Pulsar transport is UDP). No rule for 21117 is added by default;
LAN discovery needs one inbound UDP 21117 rule shared by all seats
(`PulsarSplit pulsar-host discovery`), created only when some seat has discovery on.

### 3.7 Mapping onto the existing manager flow

The manager keeps one pair dialog and one seat page. The service adds a `backend` field
and routes each request to the seat's backend.

| Manager → service (split-proto) | Sunshine seat (spec 80 / contracts §7) | Pulsar seat (this note) |
| --- | --- | --- |
| `get_pair_state {name}` | pending `/pair` request | pending `auth.request`; `pending` gains `mode`: `pin` (Sunshine) or `approve` (Pulsar), plus `kind`, `key_fp`, `password_state` |
| `submit_pin {name, pin, client_name, request_id}` | `pair.submit` | refused with `unsupported` (reason `backend_pulsar`) |
| **new** `answer_peer_request {name, request_id, allow, view_only}` | refused (`backend_sunshine`) | `auth.decide` |
| `cancel_pair {name}` | `pair.cancel` | `auth.decide {allow: false}` |
| `list_paired_clients {name}` | `clients.list` → `{uuid, name}` | `trust.list` → `{uuid: key, name}` (+ `label`, `last_seen` as optional fields) |
| `unpair_client {name, uuid}` / `unpair_all` | `clients.unpair` / `unpairAll` | `trust.revoke {key: uuid}` / `trust.revoke_all` |
| `regenerate_seat_identity {name, confirm}` | delete TLS pair, restart | new identity secret from the service, trust store cleared, restart. **The seat's relay ID changes**; the confirm text says so |
| `set_apps` | write `apps.json`, `apps.reload` or restart | write `apps.json`, `apps.set` |
| **new** `set_seat_connect_password {name, password: Secret}` (write-only, empty = none) | refused | DPAPI store, then `secrets.load` (live) |
| **new** `get_seat_otp {name}` / `rotate_seat_otp {name}` | refused | `auth.otp.get` / `auth.otp.rotate`; response carries `Secret`, the manager shows it, never stores it |
| **new** `answer_relay_auth {name, password: Secret, totp?: Secret}` | refused | `relay.auth` |
| `get_seat_runtime` → `stream` | `{flavour, status, capabilities}` | `{backend: "pulsar", relay_state, relay_id, node_port, status, capabilities}` |
| `poll_events` kinds | `pair_pending`, `pair_ended`, `stream_started`, `stream_ended` | same kinds, `data.backend = "pulsar"`; plus `relay_auth_required`, `otp_changed` |
| Seat setting | `stream.gamepad`, `stream.capture`, `min_fps` | Designed: `stream.backend` + `pulsar` block (`relay`, `network_mode`, `node_port_mode`, `port_mapping`, `lan_discovery`, `access`). **Implemented:** top-level `SeatConfig.stream_backend` (`sunshine` default / `pulsar`), no `pulsar` block yet; fixed values in `host_config` (OQ stream-backends-1). `stream.capture` is used by both backends |

Pair dialog copy (Turkish, goes to `i18n.ts`): Sunshine mode keeps the PIN field. Pulsar
mode shows "Bağlantı isteği", the device name and key fingerprint, and buttons
"İzin ver", "Yalnızca izle", "Reddet"; below it "Tek kullanımlık şifre" (shown on
demand, "Yenile" button) and "Kalıcı bağlantı şifresi" (write-only field). Seat backend
selector: designed "Yayın altyapısı" with "Moonlight (Sunshine)" and "Pulsar";
**implemented** "Yayın sunucusu" with "Sunshine (Moonlight uyumlu)" and "Pulsar"
(OQ stream-backends-4). Changing it needs the seat stopped. The connect password is
removed only with a separate, confirmed "Şifreyi kaldır" action.

Nobody sits at a remote seat, so the operator at the manager is the one who approves.
If no manager is open, a request times out (`timeout`); paired devices and the
password path still work (OQ pulsar-host-6).

### 3.8 Microphone (additive, 2026-11-14; service side implemented, `pulsar-host` side not built)

**Status: proposed.** This section and §6 item 20 extend the binding contract and wait
for pulsar-desktop to accept them. Until then nothing above changes for existing hosts and
every Pulsar seat reports `mic` = `unsupported`.

Spec/remote 70 §5.1. **Additive:** protocol stays `1`, nothing above changes, and a host
without the capability behaves exactly as before. It still needs a pulsar-desktop change
(§6 item 20); the request is in `docs/agents/status.md`.

| Item | Value |
| --- | --- |
| Capability | `mic` in the `hello` answer: this `pulsar-host` can forward a Pulsar client's microphone |
| `config.apply` key | `mic` (bool). The service sends it **only** as `true`, only to a host with `mic`, only for a seat whose microphone switch is on. Missing = off. Older hosts never see the key |
| Pipe | `\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host-mic`. Server = the service, created **before** the launch with the same rules as §3.1 (first instance, `PIPE_REJECT_REMOTE_CLIENTS`, byte mode, one instance, `HOST_PIPE_SDDL`, client = the launched pid + creation time). Created only for seats with the mic on. A failure to create it costs the mic only, the seat starts |
| Direction | `pulsar-host` writes, the service reads. No request / response, no JSON |
| Framing | u32 LE length + payload (same length prefix as §3.1; payload at most 32 KiB). Payload = one kind byte + data: `0x01` start (1 byte channels, 1 or 2), `0x02` pcm (signed 16-bit LE, interleaved, **48 kHz**), `0x03` stop. Source of truth: `seat_core::remote::mic` |
| When | `start` when a client's mic opens (one client per seat, `max_clients` 1), pcm frames of about 10–20 ms as they are decoded, `stop` when the mic closes or the client leaves. The host connects to the pipe once, after `config.apply` with `mic: true`, and keeps it open |
| Loss | A broken mic pipe ends the mic only (the service sends `stop` to the seat). A bad frame closes the pipe. The control pipe is unaffected. The host must never block its stream on the mic pipe: drop frames if a write would block |
| Service side | the frames go to the seat's headless hold on its stdin (after the password frame; the hold runs with `--audio-capture`, so `INFO_AUDIOCAPTURE` is set and MS-RDPEAI `AUDIO_INPUT` is open) and from there into the seat session's capture endpoint. Queue at most about 1 s; more is dropped |
| Status | `seat_runtime.stream.mic`: `off` / `unsupported` (host without `mic`) / `waiting` / `connected` / `failed` / `closed` |

Why a second pipe: the service reads the control pipe about once a second (§3.4 events
are polled), so audio on it would arrive in 1 s bursts and could fill the pipe buffer and
block the host's event writes.

**Pulsar Seat side complete (2026-11-17, package remote-smalls).** Nothing more is needed
in this repo once a host implements the table above: the pipe, the `mic` capability check,
the `config.apply` key, the hold's `AUDIO_INPUT` channel and the status values exist and are
tested with a fake host. Since 2026-11-17 the hold also sends 20 ms of silence while the
server has the capture open and no client PCM arrived for 100 ms (before the client opens its
microphone, after it closes it, during gaps), as an idle RDP client would, so capture
readers in the seat never stall (`seat_rdp::headless::mic::HoldMic::tick`). The host must
not send silence itself. DualSense microphones (pad mic) go through the media section of §5,
not this pipe.

**HYPOTHESIS (untested, no host and no RDP target here):** Windows creates the seat
session's capture endpoint when the hold sets `INFO_AUDIOCAPTURE`; the server picks one of
the offered 48 kHz formats; apps in the seat see a microphone; latency stays low enough for
voice chat.

## 4. Capture in a non-console RDP seat session

Known (pulsar-desktop today, console or user sessions only):

- Native path: DXGI Desktop Duplication (`desk:crates/pulsar-capture/src/dxgi/device.rs:433`)
  or Windows Graphics Capture of a monitor (`desk:crates/pulsar-capture/src/wgc.rs:204`)
  or of one window, then NVENC, or Media Foundation on the display GPU (`dxgi-mf`)
  (`desk:crates/pulsar-capture/src/lib.rs:80`). Fallback: ffmpeg `ddagrab`
  (`desk:src-tauri/src/process.rs:207`).
- The capture thread re-picks the adapter per output (`lib.rs:631`), runs a hybrid-host
  probe between DXGI and WGC (`lib.rs:655`), and a no-frame watchdog switches to the other
  source once (`lib.rs:672`).
- The forced source is read from the environment (`path::forced_source`, `lib.rs:657`).

Unknown (nothing has been run in a seat session):

- Whether DDA or WGC returns frames in a loopback-RDP session whose monitor is the
  per-session RdpIdd adapter, for a SYSTEM process in that session. Same open point as
  spec 80 §3.2 / §12 #2 for Sunshine. **Unverified.**
- Which GPU renders the session: without the RDS policy "use the hardware graphics
  adapter", RDP sessions may render on a software adapter, so NVENC / MF on the real GPU
  needs a cross-adapter copy (pulsar-capture has a keyed-mutex BGRA bridge,
  `desk:crates/pulsar-capture/src/encode/d3d.rs:1`) or is not available. **Unverified.**
- Secure desktops (lock, UAC, Ctrl+Alt+Del) of a seat session: capture and input.
  **Unverified** (spec 80 §12 #8).
- WGC needs a WinRT apartment and a capture item; whether that works for SYSTEM in a
  non-console session is **unknown**.

Plan:

1. The spec 80 §3.3 decision-gate experiment is extended: run `pulsar-host` the same way
   (SYSTEM, seat session) with `capture = dxgi` and `capture = wgc`, policy on and off.
   Record first frame, fps, encoder, adapter, lock / UAC behaviour. Needs maintainer
   approval on the test PC.
2. `config.apply.capture` replaces the environment switch; `auto` keeps today's probe.
3. **Frame-source hook (later, optional):** pulsar-capture gets a `FrameSource` seam
   (DXGI, WGC, and a third, `split_idd`). `split_idd` is a consumer of the Pulsar-defined
   frame channel of spec 80 §3.4 (`Global\PulsarSplit_Seat_<seat-id>_Ctl`, texture ring
   by handle duplication, `ModeRequest` event). It is offered only when the service
   reports that the own IDD is attached to that seat, and `capture = split_idd` is set
   explicitly. The own IDD is unsigned and unbuilt (contracts §3.2); it is **never the
   default**. The default display backends stay the signed third-party ones (plan.md).
4. Display modes: `pulsar-host` does **not** change modes itself (today
   `desk:src-tauri/src/display_mode.rs:1` changes the host monitor). It sends
   `display.mode.request`; the service applies it through seat-display (`seat_display::remote`) and answers
   `display.mode.result`. On failure it streams the current mode scaled.

## 5. Pads

Today `pulsar-host`'s code would create pads itself: ViGEm X360 / DS4
(`core:input/vigem.rs:1`), and a virtual wired DualSense as an in-process USB/IP server
attached with usbip-win2 (`core:input/ds5/virtual_pad.rs:1`, `core:input/ds5/attach.rs:5`),
plus its own machine-wide leftover-pad registry (`core:input/pad_cleanup.rs:7`).

**Recommendation: seat-pads (the service) is the only owner of virtual pads for both
backends. `pulsar-host` never creates a device.**

Why:

1. Spec 60 §3.5 and §8.2 already fix it: the service is the only creator; the stream
   server asks with `pad.wanted` and moves input and feedback through the per-pad
   sections of spec 60 §4.5 (`Global\PulsarSplit_Pad_<seat32>_<slot>_In` / `_Out`). The
   service pad pump already reads those channels (`crates/split-service/src/engine/mod.rs:1119`).
   One rule for both backends means one code path to test.
2. Seat isolation needs facts only the service has: the seat user's SID for the device
   security descriptor, the session id property, the shim seat map's instance prefixes
   (`PadManager::seat_instance_prefixes`). `pulsar-host` would have to reimplement them.
3. One orphan reaper. seat-pads keeps `%ProgramData%\PulsarSplit\pads\owners.json`;
   pulsar-core keeps `%ProgramData%\Pulsar\duo-pads.json`. Two reapers on one machine
   can remove each other's devices or miss them.
4. Device markers: seat-pads marks USB/IP pads with the USB serial `PSPAD<seat32><slot>`
   (contracts §5.3). pulsar-core's DualSense has no serial string descriptor
   (`core:input/ds5/device.rs:206`), so its pads cannot be told apart per seat.
5. `pulsar-host` then needs no SetupAPI rights, no usbip.exe child, no driver install
   checks; the spawn lock of contracts §0.1a stays inside the service.

Cost, and how to pay it:

- DualSense **media** (HD haptics, pad speaker, pad mic) today flow in-process between the
  USB/IP device and the stream (`desk:src-tauri/src/ds5/host.rs:6`). Through seat-pads
  they need a channel the spec 60 §4.5 protocol 1 does not have.
  - v1: DualSense via seat-pads `UsbipDs5Backend` with HID only: input, rumble,
    adaptive triggers (raw), LEDs, player LEDs (`seat_pads::Feedback`). Haptics, speaker
    and mic are off for split seats; the manager says so.
  - Later: protocol 2 of spec 60 §4.5 adds a per-pad media ring (`<base>_Media`,
    Pulsar-defined, 48 kHz PCM blocks both ways). OQ pulsar-host-4.
  - **Done on the service side (2026-11-17, additive, `seat_pads::media`):** every pad
    channel also has the section `<base>_Media` (131072 bytes, same DACL as `_In` /
    `_Out`) and the auto-reset event `<base>_MediaEvt` (set after each out write).
    `pad.attach` carries `media: true` only when the virtual pad really has the controller
    audio endpoints (DualSense / Edge on usbip-win2 **and** the audio interfaces were
    exposed: `pad_audio` / console session; parity review 2026-11-17). Absent = false:
    the host must then not open the client's controller microphone; the service reads
    nothing from the in ring of such a pad. Header (little-endian): magic `PPMD` at 0, version 2 at 4,
    header size 64 at 6, rate 48000 at 8, out channels 4 at 12, in channels 2 at 14; out
    ring: capacity 65536 at 16, write counter at 20, read counter at 24, dropped blocks at
    28; in ring: capacity 32768 at 32, write at 36, read at 40, dropped at 44; flags at 48
    (bit 0 speaker streaming, bit 1 mic open); data: out ring at 64, in ring at 64 + 65536.
    Counters only grow (u32 wrapping), offset = counter % capacity, single producer and
    single consumer per ring; a producer drops a whole block when it does not fit.
    `pulsar-host` reads the **out** ring (4-channel s16le: speaker / headset L / R, haptics
    L / R) and sends it to the client's DualSense; it writes the client's microphone
    (2-channel s16le) into the **in** ring. The service plays the in ring into the usbip
    DualSense's mic endpoint and fills the out ring from its speaker endpoint. Remote
    seats' pads expose the audio interfaces only with the global setting `pad_audio`
    (default off; HYPOTHESIS: USB audio of a locally attached device may not be usable
    from a seat session). Request to pulsar-desktop: open the section when
    `media: true`, carry the two PCM streams over the existing session (no new port).
    Sunshine seats stay HID-only: the Moonlight protocol has no controller-audio channel
    (rumble, trigger rumble, LEDs and adaptive triggers only).
- To avoid two DualSense emulations, seat-pads may reuse pulsar-core's `input::ds5`
  device and USB/IP server (our GPL-3 code) as a git dependency. Decide in OQ pulsar-host-4.
- Mapping in `pulsar-host`: a client `GamepadState` / `Ds5Report` becomes a
  `seat_pads::PadState` written to `_In`; `_Out` entries become the client's rumble /
  `Ds5Output` messages. Kind choice: client X360 / XOne / generic → `x360` (ViGEm);
  DS4 → `ds4` (ViGEm); DualSense → `ds5` (usbip-win2). Unavailable backend →
  `pad.denied`, and the client is told no pad exists.

Rejected alternative: `pulsar-host` creates pads with pulsar-core and reports instance
ids to the service. It keeps DS5 media for free, but breaks points 2–4 and leaves two
creators per machine.

## 6. Changes needed in pulsar-core / pulsar-desktop (future work, not done now)

| # | Change | Where |
| --- | --- | --- |
| 1 | A Tauri-free `pulsar-host` binary. The host role is bound to `AppHandle` (events, popups, state, paths): `go_online` / `bring_online` take it (`desk:src-tauri/src/host.rs:386`, `:411`), the trust store path comes from it (`desk:src-tauri/src/pairing.rs:229`), data paths too (`desk:src-tauri/src/util.rs:37`, `:49`). Extract a `HostEnv` trait (event sink, operator prompt, data dir, app list, launcher, mode setter) and move the host core into a crate both the app and `pulsar-host` use. `headless.rs` (`desk:src-tauri/src/headless.rs:84`) shows the loop to keep (retry 3 s → 12 s, stop on `needs_user`) | `desk:src-tauri/src/{host.rs, host/handlers.rs, auth.rs, pairing.rs, headless.rs, util.rs}`, new `desk:crates/pulsar-host` |
| 2 | Split mode `--split-seat <id>`: bypass `shared_mode::resolve` and the lease (`desk:src-tauri/src/shared_mode.rs:192`, `:475`), never take `Local\PulsarSingletonMutex` / `Local\PulsarShowEvent` (`desk:src-tauri/src/lib.rs:195`, `:207`); take `Global\PulsarSplit_Seat_<id>_PulsarHost` | `shared_mode.rs`, `lib.rs`, new main |
| 3 | Pipe client for §3 (reuse the framing; tiny, no split crate dependency needed) | new module in `pulsar-host` |
| 4 | Identity from `secrets.load` via `Identity::from_secret_bytes`; never `load_or_create` (`core:crypto.rs:138`) | `host.rs:569` |
| 5 | `Config` built from `config.apply`; `Config::save` disabled; `connect_password`, `totp_secret`, `relay_keys` (`core:config.rs:78`, `:84`, `:55`) held in memory only (zeroized) | `core:config.rs`, `host.rs` |
| 6 | Announced name = given name, no fallback (`desk:src-tauri/src/util.rs:896`) | `util.rs` |
| 7 | Operator prompt over the pipe instead of webview popups: `open_approval_window`, `race_host_auth`, `ask_operator_pair` (`desk:src-tauri/src/auth.rs:358`, `:472`, `:636`); keep the throttles, popup cap 3 and budgets as they are | `auth.rs` |
| 8 | OTP rotation notifies `otp.changed` instead of the `session-password` UI event (`desk:src-tauri/src/commands.rs:321`) | `commands.rs` |
| 9 | App launch through the service (`app.launch`), never `launch_host_game` as SYSTEM (`desk:src-tauri/src/process.rs:1200`) | `process.rs`, `host.rs:1333` |
| 10 | Input: inject in-process as SYSTEM following the input desktop (reuse `desk:crates/pulsar-sysagent/src/inject.rs` as a library) instead of `SysInput` over the helper pipe (`desk:src-tauri/src/host/sysinput.rs:1`); disable the local-mouse takeover hook (`desk:src-tauri/src/host/takeover.rs:1`) and the remote-cursor overlay (no local user) | `host/sysinput.rs`, `host/takeover.rs`, `remote_cursor.rs` |
| 11 | Capture source and adapter from `config.apply` instead of env (`desk:crates/pulsar-capture/src/lib.rs:657`); `FrameSource` seam for `split_idd` (§4) | `desk:crates/pulsar-capture/src/{lib.rs, path.rs}` |
| 12 | Display mode via `display.mode.request` instead of `set_best_mode` on the host monitor (`desk:src-tauri/src/display_mode.rs:1`) | `display_mode.rs`, `host/handlers.rs` |
| 13 | Audio: loopback pinned to `audio_endpoint_id` (`core:audio/loopback.rs:94`, `run_loopback_capture_pinned`) instead of the default endpoint (`:201`) or process loopback (`core:audio/process_loopback.rs:7`) when the service gives an id | `core:audio/*`, `host/handlers/ds5_route.rs` |
| 14 | Pads: a `SplitPadChannel` implementing `VirtualGamepad` (`core:input/types.rs:180`) over spec 60 §4.5 sections, driven by `pad.wanted` / `pad.attach`; in split mode no ViGEm, no USB/IP, no third-party pad DLL, no `pad_cleanup::startup` (`core:input/pad_cleanup.rs:629`) | `core:input/`, `desk:src-tauri/src/ds5/host.rs` |
| 15 | Port mapping default off, node port from config, in-app relay off (`core:config.rs:66`, `:111`; `desk:src-tauri/src/local_relay.rs`) | `host.rs:561`, `:635` |
| 16 | Clipboard and file transfer: off in split mode until they run as the seat user (SYSTEM would write into the user's profile as SYSTEM) | `desk:src-tauri/src/{clipboard_sync.rs, files.rs}` |
| 17 | Logging to `--log` path, redacted (no OTP, passwords, keys, relay keys); no stdout console | `desk:src-tauri/src/logging.rs` |
| 18 | Ignore dev env switches in split mode (`PULSAR_NO_ELEVATE`, `PULSAR_MMCSS`, forced capture source, test hooks) | `main.rs`, capture crate |
| 19 | Release artefact `pulsar-host.exe` (x64 + ARM64) with SHA-256 for Pulsar Split's manifest; GPL-3 source offer as for the app | pulsar-desktop CI |
| 20 | Microphone (§3.8, 2026-11-14): announce `mic`; on `config.apply` `mic: true` decode the client's mic stream to 48 kHz s16 PCM and write it to the mic pipe of §3.8 (start / pcm / stop frames), never blocking the stream. The Pulsar client must also send its mic (a client-side change; no new port, inside the existing session) | `pulsar-host`, pulsar-core client + host audio |

The job object of `desk:src-tauri/src/job.rs` nests inside the service's seat job
(Win8+ nested jobs), so media children still die with `pulsar-host`.

## 7. Open questions

Listed as `pulsar-host-1` … `pulsar-host-12` in [open-questions.md](open-questions.md).

## 8. Implemented in pulsar-desktop ddc0403 (2026-11-13, untested on a host)

`pulsar.exe --split-seat <id> --data-dir <abs> --log <abs>` (the same binary as the app,
not yet a separate `pulsar-host.exe`; §6 item 19 open). Details: pulsar-desktop
`docs/split/agents/modes.md` "Split seat host". Done: §1 argv (strict, exit 2), §2 seat mutex
(exit 3), singleton / shared mode bypassed, per-seat data dir, name verbatim, local relay
and pad cleanup off; §3.1 server check (SYSTEM, session 0, `pulsar-split-service.exe`),
broken pipe = exit 5; §3.2 handshake, bind only after `secrets.load` + `config.apply`;
§3.3 `hello`, `secrets.load`, `config.apply`, `status`, `auth.decide`, `auth.otp.*`,
`trust.*`, `relay.auth`, `server.shutdown`; §3.4 `auth.request` / `auth.ended`,
`otp.changed`, `trust.changed`, `relay.state`, `relay.auth_required`, `relay.key_issued`.
`not_supported` (error code): `clients.*`, `apps.set`, `app.launched`,
`display.mode.result`, `pad.*`, `log.level`, and `config.apply` with `access: approve` or
`capture: split_idd`. hello capabilities: `approve`, `connect_password`, `otp`,
`trust_store`. Exit codes: 4 pipe open / server check failed, 6 seat mutex not creatable.
Still open from §6: 1 (smallest seam: a window-less Tauri app, no `HostEnv`), 9, 10, 12,
13, 14 (pads are still created by pulsar-core), 19; `status.capture` / `encoder` empty.

Follow-up fix in pulsar-desktop 15a685e: the seat pipe reader is now one long-lived future
feeding whole frames to the serve loop (a frame split across reads no longer desyncs when
an event is sent meanwhile), and the host-side caps cache (`last-stream-caps.json`) is
written under `--data-dir` instead of the shared SYSTEM profile.
