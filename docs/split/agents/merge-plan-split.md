# Merge plan: Pulsar Split side

Index: [../../AGENTS.md](../../../AGENTS.md). Status: **plan only (2026-11-14)**. No code moves.
Counterpart: the Pulsar Seat draft, `../pulsar-seat/docs/agents/merge-plan.md` (Seat half).
Decision source: [plan.md](plan.md), "Merge with Pulsar Seat" (2026-11-13). Nothing here is
decided until the maintainer and both sessions agree.

Goal (maintainer): ONE product. Each seat has a kind, and both kinds run side by side on one PC:

| Kind | What it is | Comes from |
| --- | --- | --- |
| `local` | physical monitors, keyboards, mice, audio of this PC | pulsar-seat |
| `remote` | virtual display + virtual pads, streamed with Moonlight / Pulsar | pulsar-split (this repo) |

Placeholder for the unnamed product: `<prod>` (crate prefix `<prod>-`).

## 1. Packages in pulsar-split

Workspace: root `Cargo.toml` (`crates/*` + `apps/split-manager/src-tauri`; `default-members` =
every crate, not the manager), root `package.json` (bun workspace `apps/*`). Details:
[architecture.md](architecture.md) "Layout", [contracts.md](contracts.md) §0–§8.

| Path | Output | Role |
| --- | --- | --- |
| `crates/split-proto` | lib | IPC messages (serde JSON), pipe framing, `Secret`, `spawn_lock`. No platform code. |
| `crates/split-core` | lib | Seat/global model validation, port allocation, `ConfigStore` trait, watchdog, `Controller` (requests over a `Platform` trait, event ring). |
| `crates/split-service` | `pulsar-split-service.exe` | SCM service, control pipe, health pipe, registry store, DPAPI, `engine/` (seat workers), `wiring.rs`, setup sub-commands. |
| `crates/split-session` | lib | Server + RDSH multi-session: edition classify, RDS rows, seat users, WTS discovery, launch into session, sweep. `crates/split-session/README.md` |
| `crates/split-rdp-helper` | `pulsar-split-rdp-helper.exe` | Headless IronRDP loopback client that creates and holds one seat session. `crates/split-rdp-helper/README.md` |
| `crates/split-termwrap` | lib | EXPERIMENTAL, off by default: integrates a wrapper the operator installed. `crates/split-termwrap/README.md` |
| `crates/split-display` | lib | `DisplayBackend` trait + `rdp_session` / `vdd` / `pulsar_idd`, mode / HDR / GPU rules. `crates/split-display/README.md` |
| `crates/seat-audio` | lib | Session Remote Audio endpoint discovery, TS policy, stream-server audio keys, limitations. `crates/seat-audio/README.md` |
| `crates/seat-stream` | lib | `StreamBackend` (Sunshine, `pulsar-host`), seat dirs, `server.conf`, apps, pairing, firewall, watchdog. `crates/seat-stream/README.md` |
| `crates/seat-pads` | lib | `PadBackend` (ViGEm, usbip-win2 DualSense, own pad), `PadManager`, device security. `crates/seat-pads/README.md` |
| `crates/seat-shim-core` | lib | Pure part of per-process isolation: seat map, decisions, health frame, AppHelp install, DXVK. `crates/seat-shim-core/README.md` |
| `crates/seat-setup` | lib | `install` / `reconcile` / `uninstall` / `check-coexistence` sub-commands, driver manifests, restore replay. `crates/seat-setup/README.md` |
| `apps/split-manager` | `PulsarSplitManager` (Tauri 2 + SvelteKit 3 + Svelte 5) | Elevated manager UI (Turkish), pipe client, SCM control. |
| `shim/` | `PulsarSplitShim.dll`, `pulsar-split-launch(32).exe`, `pulsar-split-shimctl.exe` | Separate Cargo workspace, x64 + x86 only. `shim/README.md` |
| `drivers/split-idd` | UMDF IddCx driver (C++) | Own remote-session IDD. Unbuilt, untested, unsigned. `drivers/split-idd/README.md` |
| `drivers/split-pad` | UMDF HID minidriver | Own virtual pad. Unbuilt, untested, unsigned. `drivers/split-pad/README.md` |
| `installer/split/` | scripts + manifests | WiX fragment, NSIS hooks, Tauri bundle overrides, `fetch-deps.ts`, `check-payload.ts`, `drivers.json`, `stream-server.json`. `installer/split/README.md` |
| `scripts/` | `coverage-rust.ts` | cargo-llvm-cov runner (x64 target, see [coverage.md](coverage.md)). |

## 2. Process and IPC model

### 2.1 Processes

| Process | Account / session | Started by |
| --- | --- | --- |
| `PulsarSplitManager` | elevated admin, console session (`requireAdministrator` in release) | user |
| `pulsar-split-service` (SCM name `PulsarSplit`) | LocalSystem, session 0 | SCM |
| `pulsar-split-rdp-helper` | child of the service, one per running seat | service (`spawn_lock`, contracts §0.1a) |
| `sunshine.exe` or `pulsar-host.exe` | SYSTEM token in the seat session, kill-on-close seat job | service (`seat_stream` launcher) |
| Autostart apps | seat user token in the seat session | service |
| Shim module / launcher (opt-in) | inside seat-user processes | service / AppHelp |

Service threads: pipe server (thread per client), 1 s timer (`Engine::tick` then
`Controller::drain_reports`), 4 ms pad pump, health pipe. No HTTP server of ours
(architecture.md "Processes").

### 2.2 Pipes

| Pipe | Server | DACL | Notes |
| --- | --- | --- | --- |
| `\\.\pipe\PulsarSplit.Control` | service | `D:P(A;;GA;;;SY)(A;;GA;;;BA)` (`security::PIPE_SDDL`) | byte mode, `PIPE_REJECT_REMOTE_CLIENTS`, `FILE_FLAG_FIRST_PIPE_INSTANCE`. Manager checks the server (SQOS identification, session 0, LocalSystem, owner). Dev only: `--allow-current-user` adds the caller's SID. |
| `\\.\pipe\PulsarSplit.ShimHealth` | service | SYSTEM full, Authenticated Users write-only | one ≤ 4 KiB frame per connect, 2 s read deadline; informational only (`split-service/src/health.rs`). |
| `\\.\pipe\PulsarSplit\seat\<id>\pulsar-host` | **service**, created before launch | `D:P(A;;GA;;;SY)(A;;GA;;;BA)` (`HOST_PIPE_SDDL`) | 1 instance; only the launched pid + creation time is accepted; secrets only here ([pulsar-host-backend.md](pulsar-host-backend.md) §3). |
| `\\.\pipe\PulsarSplit\seat\<id>\server` | stream server (Sunshine fork) | spec 80 §8.4 | **fork not built**. Upstream Sunshine uses its loopback HTTPS web-UI API instead (OQ 28 exception, plan.md). |
| `\\.\pipe\MTTVirtualDisplayPipe` | third-party VDD | Everyone (driver's own) | we are a client only (third-party.md). |

### 2.3 Protocol and events

- Frame = u32 LE length + JSON, max 1 MiB. Request `{version, id, body}`, response
  `{version, id, result}`; `body = {type, data}` snake_case.
- `split_proto::PROTOCOL_VERSION = 2`, checked on every request (`version_mismatch`). No
  `hello`-negotiated minimum; no per-request authorisation table (every caller is admin).
- Events: `poll_events` long-poll (waits up to 2 s, outside the controller lock) over a ring
  of 256: `seat_phase`, `seat_failed`, `pair_pending`, `pair_ended`, `stream_started`,
  `stream_ended`, `host_warning`. The manager also polls status every 3 s. No push
  subscription.
- `pulsar-host` pipe: own `protocol: 1`, `hello` → `secrets.load` → `config.apply` → `apps.set`.

## 3. Config and state

| What | Where | Notes |
| --- | --- | --- |
| Global settings | `HKLM\SOFTWARE\PulsarSplit` values | `SchemaVersion` (= **2**, `registry_store::SCHEMA_VERSION`), `StartWithWindows`, `HidIsolation`, `ProcessPatching`, `SteamIsolation`, `Verbosity`, `MultiSession`, `BluetoothConsoleOnly` (DWORD); `PadBackend`, `DisplayBackend` (SZ) |
| Seat record | `HKLM\SOFTWARE\PulsarSplit\Seats\<name>` | SZ: `Id` (GUID, survives renames), `DisplayName`, `UserName`, `RenderAdapter`, `RenderAdapterId`, `RdpClientName`, `StreamBackend` (`sunshine` \| `pulsar`, missing = sunshine). DWORD: `Port`, `StartWithService`, `StopAction`, `IsScaleFactorStatic`, `ScaleFactor`, `SuperSamplingFactor`, `SDRWhiteLevel`, `ForceSDR`, `RefreshRate`, `MinLuminance`, `MaxLuminance`, `MaxFullFrameLuminance`, `StreamAudio`, `Microphone`, `Gamepad`, `Capture`, `MinFps`, `FirewallDomain`, `KeepPairingOnDelete`; plus `LastWidth/LastHeight/LastRefreshRate` |
| Autostart apps | `Seats\<name>\AutoStartApplications\<guid>` | `ApplicationPath`, `Arguments`, `RunAsSystem` |
| Seat passwords | `HKLM\SOFTWARE\PulsarSplit\Secrets`, one REG_BINARY per seat | DPAPI machine scope; key DACL `D:P(A;OICI;KA;;;SY)(A;OICI;KA;;;BA)` |
| Pulsar seat secrets | `Secrets\Pulsar\<seat-id>.{identity,connect_password,relay_key}` | DPAPI, own protected DACL |
| Other keys | `Runtime\Seats\<session>` (volatile seat map), `Shim\` (incl. `DxvkGames`), `Termwrap\`, `Session\Originals` | |
| Seat dirs | `%ProgramData%\PulsarSplit\seats\<seat-id>\` (`server.conf`, `apps.json`, `state.json`, `server.log`, `credentials\`, `pulsar\`) | `O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`, reparse points refused (`seat-stream/src/seatdir.rs`) |
| Run state, journals, logs | `%ProgramData%\PulsarSplit\{run\sessions.json, journal\<package>.jsonl, logs\}` | same protected DACL; journals are JSON Lines intent/done (contracts §0.3) |
| Install root | `%ProgramFiles%\Pulsar Split` | |

Source: module doc of `crates/split-service/src/registry_store.rs`.

## 4. Session backend

- Supported path: **Windows Server + RD Session Host**. Per seat the service starts
  `pulsar-split-rdp-helper` (IronRDP 0.17, NLA/CredSSP only, certificate SHA-1 pinned) to
  `127.0.0.1`/`::1`. The password goes over the helper's **stdin** (one frame), never argv.
  The helper reports JSON Lines on stdout.
- Discovery: WTS enumeration, match on generated client name (`PSS` + 8 hex) + user +
  loopback address; never Services / Console / listeners (`split_session::discover`).
- Stop: ownership check, kill helper, end the seat job, **log off** the session by id and
  wait for it to vanish. Service start sweeps leftover `PSS…` sessions (`sweep`).
- Client SKUs: only the existing experimental `split-termwrap` integration with an
  operator-installed wrapper.

## 5. Display, pads, audio, stream backends

| Area | Backends (default first) | State |
| --- | --- | --- |
| Display | `rdp_session` (default for remote seats, no HDR) · `vdd` (Virtual-Display-Driver, MIT, console seat only) · `pulsar_idd` (own `drivers/split-idd`, never auto-chosen while unsigned; needs a WRDS protocol provider) | `crates/split-display/README.md` |
| Pads | ViGEmBus (X360 / DS4, only if already installed) · usbip-win2 + in-process USB/IP DualSense server · own `split-pad` (only on explicit `pad_backend = pulsar_pad`) | `crates/seat-pads/README.md` |
| Isolation | pad device security first; AppHelp shim + launcher opt-in, x64/x86 only | `shim/README.md` |
| Audio | no driver: the session's Remote Audio endpoint, captured by the stream server (WASAPI loopback); no microphone path yet | `crates/seat-audio/README.md` |
| Stream | upstream Sunshine pinned `v2026.516.143833` (Moonlight, default) · `pulsar-host` (Pulsar clients, not built yet; missing = `backend_missing`, no fallback) | contracts §7.4, [pulsar-host-backend.md](pulsar-host-backend.md) |

## 6. Installer and signing

| Item | Pulsar Split | Pulsar Seat |
| --- | --- | --- |
| Package | Tauri bundle: MSI default (WiX fragment `installer/split/wix/PulsarSplit.wxs` with `ServiceInstall`, deferred custom actions), NSIS alternative (`hooks.nsh`) | own WiX MSI under `installer/split/` |
| Third-party pins | `installer/split/drivers.json` (URL + SHA-256 + licence per driver), `installer/split/stream-server.json` (Sunshine zip per arch + SHA-256); `fetch-deps.ts` verifies, `check-payload.ts` blocks MS DLLs / certs / web UI | check-payload gate |
| CRT | `+crt-static` for x64 / x86 / ARM64 (`.cargo/config.toml`) | — |
| Code signing | **none yet**; README asks for a public CA cert + RFC 3161 timestamp later; drivers unsigned | none yet for code; test-signing VM for drivers, attestation before release |
| Release signing | none | minisign release key (key id 0E7EC5FC77170613), public key in `installer/split/release-signers.txt`, private key outside the repo, `seat-release` crate, update check |

## 7. Overlap with Pulsar Seat

| Concern | pulsar-split | pulsar-seat | Recommendation |
| --- | --- | --- | --- |
| SCM service skeleton | `split-service/src/service.rs`, power + session events | `seat-service` boot sequence, safe mode, crash loop | **Keep one (Seat's)**; port Split's power-event and session-change handling into it. |
| Control pipe + ACL | SY+BA only, every caller admin, no hello | SY+BA+IU (restricted mask), Low NW label, `hello`, `Access` table, connection limits | **Keep one (Seat's)**. Split requests enter its table as `Access::Admin`. |
| Secrets | DPAPI machine scope in `HKLM\...\Secrets` | LSA private data | **Merge behind a `SecretStore` trait**; LSA as target (agree with Seat). Check LSA limits for the per-seat Pulsar secrets first (open question). |
| Config store | HKLM registry, schema 2 | `config.json`, schema 1 + `revision` | **Keep one (Seat's JSON document)**; remote seat fields go into a kind-specific sub-struct. |
| Manager shell | Tauri 2 + SvelteKit 3 + Svelte 5, polls | same stack, subscription connection, tray, wizard | **Keep one (Seat's shell)**; Split pages become the remote seat editor, host page, pair dialog, logs, DXVK. One `i18n.ts`. |
| Session creation (RDS loopback) | `split-session` + headless `split-rdp-helper` (IronRDP, `PSS…`, 127.0.0.1) | `seat-session` `rds_loopback` + `seat-host`/`seat-rdp` (IronRDP with graphics, `PSEAT-NN`, 127.0.0.2) | **Merge**: one `rds_loopback` behind Seat's `SessionBackend`, one IronRDP crate (`seat-rdp` base). Headless hold = a mode of that client (no frame sink). Sweep recognises both prefixes during migration. |
| Client-SKU wrapper integration | `split-termwrap` | `seat-session` `termsrv_wrapper` | **Keep one**, as a `SessionBackend` kind; pick by the union of both test suites. |
| Fast user switch / kiosk | — | U1 / U3, `credprov/` | Local only; remote seats refuse them. |
| Display | `rdp_session` / VDD / `split-idd` | CCD / window presenter / `display_core` / `seat-idd` | **Keep both behind one trait** per kind now; one own IddCx driver later (one signing cost). |
| Pads + HID shim | `seat-pads`, `seat-shim-core`, `shim/` | `seat-input` (raw input, KMDF filter; XInput pads shared) | **Merge pads + shim into shared crates**; `seat-input` stays for keyboard / mouse / HID routing of local seats and can feed a seat-owned virtual pad. |
| Audio | session Remote Audio endpoint, read by the stream server | `rdp_bridge` to the seat's physical endpoint (`seat-media`) | **Merge** endpoint discovery (session id, CfgMgr) into one audio crate; routing stays per kind. |
| proto / core | `split-proto` v2, `split-core` | `seat-proto` v1, `seat-core` (traits, masks) | **Merge into one** proto + core; new protocol version above both. |
| Journals / restore | per-package `journal\<package>.jsonl` | `journal\changes.jsonl` | **Merge** into one record format; uninstall replays both until old records are gone. |
| Installer | Tauri MSI/NSIS, dep pins | WiX MSI, release key | **Keep one MSI** with Seat's signing / update gates plus Split's pinned-dependency fetch and payload rules (union). |
| Streaming | `seat-stream` | — | Remote only, moves as is. |

## 8. Proposal

### 8.1 Monorepo layout

```
<prod>/                       one Cargo workspace + one bun workspace, GPL-3.0-only
  crates/
    <prod>-proto  <prod>-core  <prod>-service        shared (from seat-*, plus split-* content)
    <prod>-session  <prod>-rdp  <prod>-audio  <prod>-pads  <prod>-shim-core  <prod>-setup
    <prod>-display                                   shared trait, per-kind backends
    <prod>-host  <prod>-input  <prod>-media  <prod>-compat  <prod>-release   local kind
    <prod>-stream                                    remote kind
  apps/split-manager                one manager
  shim/                       separate workspace (x64 + x86)
  credprov/                   local kind
  drivers/                    seat-input-filter, seat-storage-filter, IddCx driver(s), pad driver
  installer/split/                  one MSI
  spec/local/  spec/remote/   analyst specs, kept apart
  docs/split/agents/                one index, one rules.md, one CLEANROOM.md
```

### 8.2 Crate names

Use the placeholder `<prod>-` until the maintainer names the product, and rename once at
the rename step (8.5 step 3). Avoid `ms-`: it reads as "Microsoft". Kind-only crates keep a
kind word (`<prod>-stream`, `<prod>-host`).

### 8.3 Seat kind in config and IPC

- Config: `SeatConfig { id, name, kind, user, start_mode, …, local: Option<LocalSeat>,
  remote: Option<RemoteSeat> }`, validated so exactly the sub-struct of `kind` is set.
  `RemoteSeat` = Split's fields from §3 (`port`, `stream_backend`, `display`, `audio`,
  `stream`, `rdp_client_name`, apps, `last_mode`). Schema version bump with a migration in
  `Config::from_json` (old Seat documents load as `kind = local`).
- IPC: one pipe, one `PROTOCOL_VERSION` greater than both (3). `hello` stays first.
  Split's v2 requests are added under `Access::Admin`; seat-scoped ones fail with a new
  `wrong_seat_kind` code when used on the other kind. `get_status` returns `kind` per seat.
- Events: Seat's `subscribe` topics; new topic `stream` (`pair_pending`, `pair_ended`,
  `stream_started`, `stream_ended`); `seat_phase` / `seat_failed` map to `seat_state`;
  `host_warning` maps to `notice`. `poll_events` stays for simple clients.
- Ports: only remote seats reserve a port block; local seats reserve none.
- Session path: a remote seat needs a concurrent session (Server + RDSH or the wrapper
  kind); with only U1 / U3 available the service refuses remote seats (`session_unavailable`).

### 8.4 Base repo

**Recommendation: pulsar-seat is the base; Split is folded in.** Reasons:

1. Its service already serves several client kinds (manager, seat hosts, agents, credential
   provider, kiosk shells) through an authorisation table. Split's admin-only pipe would
   need that work anyway.
2. It already has the release key, the update path, the WDK driver build and the
   credential provider. Split has no signing at all.
3. Its config is one versioned JSON document with `revision`. That is easier to extend with
   a `kind` than a registry tree.
4. The local kind needs more process roles (host, agent, listener). The remote kind fits into
   them as one more seat worker type.

Split brings the larger tested package set (708 cargo tests, 32 vitest on 2026-11-13) for
streaming, pads, shim, display and setup. Those packages move in mostly unchanged.

### 8.5 Migration steps

Every step ends with all gates green: `cargo test --workspace`,
`cargo clippy --workspace --all-targets` (0 warnings), `cargo check --workspace --target
x86_64-pc-windows-msvc`, `bun run test` / `check` / `build`, the `shim/` workspace tests,
`bun test installer/split/scripts`, and PE machine checks of the artefacts. Test counts may only
go up, except for tests a step deletes on purpose (list them).

| # | Step | Extra tests |
| --- | --- | --- |
| 0 | Freeze both repos; record test counts and coverage baselines. | baseline file in the merged repo |
| 1 | Joint contract delta (docs only): seat model with `kind`, protocol v3 message list, `SecretStore`, `SessionBackend` for remote seats. Agree in both repos. | review only |
| 2 | Import Split into the Seat repo with full history (`git mv` to final paths on a Split branch, then `merge --allow-unrelated-histories`). Old crate names, two managers, two services still. | both suites run unchanged in one workspace |
| 3 | Rename crates to `<prod>-*` (mechanical; also pipe, service, registry, data-dir names behind constants). | same counts; grep for leftover `split_` / `seat_` paths |
| 4 | Merge proto + core: `kind`, sub-structs, schema migration, protocol v3. | config round trip per kind, migration fixtures, request access table tests |
| 5 | Secrets: `SecretStore` trait, LSA impl, DPAPI reader for migration. | fake LSA store; Pulsar per-seat secrets round trip |
| 6 | One service: Split's `engine/` becomes the remote seat worker under Seat's controller; one pipe; health pipe kept. | Split engine tests with fakes + Seat service tests; mixed local + remote seat start/stop with fakes |
| 7 | Session layer: one `rds_loopback`, one IronRDP crate, headless mode replaces `split-rdp-helper`; one wrapper kind. | helper protocol round trip (stdin password), sweep with both prefixes, discovery tests from both |
| 8 | Audio, then pads + shim, then display traits (one concern per step). | each package's own suite; pads for local seats behind a feature flag |
| 9 | One manager: Seat shell + Split pages; one i18n. | vitest of both; status / event mapping tests |
| 10 | One MSI: Seat's signing and update gates, Split's dep pins and payload rules; uninstall replays both journal formats; removes a leftover `PulsarSplit` / `PulsarSeat` service. | installer script tests, payload check, MSI build on x64 + ARM64 |
| 11 | Hardware tests per kind, then mixed (one local + one remote seat). | maintainer runs, results in status.md |

### 8.6 Risks

- Neither repo's newest rounds ran on hardware; merging two untested stacks hides the
  source of a failure. Mitigation: hardware tests on each side before step 6 if possible.
- Two RDS loopback implementations write the same Terminal Server values (Seat adds a second
  NLA-off listener). Wrong order or two journals can leave the host in a mixed state.
- The session sweep could log off the other kind's sessions while prefixes differ.
- Remote seats need concurrent sessions; on client SKUs without the wrapper, local U1 / U3
  modes and remote seats cannot run together.
- `shim/` is x64 / x86 only; the merged product is native ARM64. Remote isolation stays off
  on ARM64.
- Data-dir and registry renames change ACL-checked paths (`seatdir`, `run\`, `journal\`);
  a half-migrated tree must be refused, not trusted.
- `pulsar-host` derives its pipe name from `PulsarSplit\seat\<id>`; it is unbuilt, so change
  the name before it exists.
- LSA private data may have size or count limits for many per-seat secrets (HYPOTHESIS).
- Mixed line endings in Split (LF / CRLF); a mass `git mv` + rename makes a good moment for
  `.gitattributes`, but only with no other agent on `main`.

### 8.7 Open questions for the maintainer

1. Product name, repo name, service name, install and data paths.
2. Base repo: pulsar-seat (recommended) or a fresh repo.
3. Secrets: LSA private data for everything (incl. Pulsar identity / relay key)?
4. One IronRDP client binary for both kinds, or keep a separate headless helper?
5. One own IddCx driver for both kinds (one signing), or two?
6. Is any existing install worth migrating (neither repo is published)? If not, skip the
   HKLM / DPAPI importer.
7. May standard users read status of remote seats (Seat's IU access), or admin only as in Split?
8. Do remote seats need to coexist with U1 / U3 local modes on client SKUs at all?
9. Pads for local seats (Seat's option B): do them in the shared pads crate at step 8?
10. One protocol pipe for manager and stream control, or keep `pulsar-host` / seat pipes separate (Split: separate)?
