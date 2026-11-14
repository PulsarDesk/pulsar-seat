# Architecture (our design)

Index: [../../AGENTS.md](../../AGENTS.md). Behaviour targets are in `spec/` (esp.
`spec/local/00-architecture.md` §5); decisions in [plan.md](plan.md) "Decisions for phases 1–5".
This file is **how Pulsar Seat is built** and the contract every package builds on. The
types named here are defined (and doc-commented) in `crates/seat-core` and
`crates/seat-proto`; when this file and the code disagree, fix one of them.

## 1. Principles

1. **The service owns everything privileged**: config, seat state machine, device
   assignment, seat secrets, every system change, reboot requests. The manager is an
   unprivileged pipe client. Nothing talks manager-to-manager; the manager never opens a
   driver.
2. **User mode first.** Default backends are user mode (or existing signed,
   redistributable drivers after a licence and signature check). Our own drivers sit behind
   the same traits and stay unloaded until signed.
3. **Documented interfaces only.** No injection, no termsrv / LSM patching, no undocumented
   RPC, no registry views, no test-signing, no Memory Integrity / Secure Boot changes.
   *Maintainer's exception:* the experimental client-SKU module (`seat-session` feature
   `experimental-client-sku`, off by default, versioned Turkish warning) points TermService
   at a wrapper **the operator installed**; that wrapper changes termsrv's code in memory at
   run time, and the module writes two registry values Microsoft does not document
   (`EnableConcurrentSessions`, `AllowMultipleTSSessions`). Pulsar ships no wrapper and no
   patch (plan.md decisions; spec 30 §6 needs the matching analyst note).
4. **One abstraction per undecided question**: session, display, input, audio and device
   gating are swappable traits; the manager greys out what a backend cannot do.
5. **ARM64 and x64 natively**, same code, same layout.

## 2. Layout and crate ownership

| Path | Kind | Owner package | What |
| --- | --- | --- | --- |
| `Cargo.toml` | workspace | contracts | resolver 3, edition 2024, `GPL-3.0-only`, `workspace.dependencies`; `default-members` = every crate except the Tauri app |
| `package.json` | bun workspace | contracts | `apps/*`; root scripts `test` / `build` / `check` |
| `crates/seat-core` | lib | contracts | model, config, validation, effective masks, states, stores, PnP helpers, **backend traits**, mocks (feature `mock`), shared constants |
| `crates/seat-proto` | lib | contracts | pipe messages, framing, authorisation table |
| `crates/seat-session` | lib | session | `SessionBackend` impls (RDS loopback, experimental termsrv wrapper, fast user switch, desktops), WTS, accounts, launch-into-session, edition probe |
| `crates/seat-display` | lib | display | `DisplayBinding` impls (CCD topology, monitor keys, window presenter, Display Core, IddCx client), `FrameSink` presenters, identify overlays, DDC/CI; `remote` = the remote kind's `DisplayBackend` (was `split-display`); `kinds::SeatDisplayBackend` = one display trait over both kinds (§10.6) |
| `crates/seat-input` | lib | input | `InputRouter` impls (raw input host, KMDF filter client), unit enumeration + keys, hot-plug, activity, hotkey; `raw` reader for the seat host; `local_pads` (feature `local-pads`, on in seat-service; per seat `virtual_pads`, default off) = seat-owned virtual pads from `seat-pads` (§10.6) |
| `crates/seat-media` | lib | media | `AudioRouter` (RDP audio bridge; endpoint discovery from `seat-audio`), `DeviceGate`s (device security, session drive letters, camera consent, minifilter client), **`pnp::WinPnpTree`**, `bridge::AudioBridge` for the host |
| `crates/seat-compat` | lib | compat | preflight + coexistence checks, CPU sets + job launcher, reversible tweaks + journal, reference-config importer, app seat API helpers |
| `crates/seat-service` | bin `pulsar-seat-service` | service | SCM service, pipe server, controller / state machine, boot sequence, LSA secret store, events, logs, diagnostics, sub-commands; `src/remote/` = the remote seat worker (Split's controller + engine, §10.5) |
| `crates/seat-host` | bins `pulsar-seat-host`, `pulsar-seat-agent` | host | per-seat host (RDP client + presenter + input + audio bridge) and per-session agent |
| `apps/manager` | Tauri 2 + SvelteKit 3 + Svelte 5, `adapter-static` | manager | all screens of spec 10 (Turkish), tray, wizard UI; `src-tauri` = pipe client |
| `credprov/` | C++20 COM DLL (v145, SDK 10.0.28000), unsigned | credprov | credential provider + filter for `fast_user_switch` (auto-logon, switch link); registered only by the service — see [credprov/README.md](../../credprov/README.md) |
| `drivers/` | C (KMDF / FltMgr) + C++17 (IddCx), built unsigned with WDK 10.0.28000, never loaded | drivers | `seat-input-filter` (keyboard / mouse + HID roles), `seat-idd`, `seat-storage-filter` — see [drivers/README.md](../../drivers/README.md) |
| `installer/` | packaging | installer | see [installer/README.md](../../installer/README.md) |
| `.cargo/config.toml` | cargo config | contracts | `+crt-static` for x64 / x86 / ARM64 (from Pulsar Split, merge step 2-3) |

Remote seat kind (Pulsar Split, merged at steps 2-3 on 2026-11-14; design docs in
[../split/agents/](../split/agents/architecture.md) until the merge steps fold them in):

| Path | Kind | What | Merged at |
| --- | --- | --- | --- |
| (removed) `crates/split-proto`, `crates/split-core` | lib | Split IPC v2 and model | step 4 (done 2026-11-14): `seat_proto::remote`, `seat_core::remote`, `seat_core::error` (§10) |
| (removed) `crates/split-service` | bin `pulsar-split-service` | Split service | step 6 (done 2026-11-14): `seat-service/src/remote/` (§10.5); DPAPI + HKLM store + own pipe deleted |
| (removed) `crates/split-session`, `crates/split-termwrap` | lib | Server + RDSH sessions, wrapper integration | step 7 (done 2026-11-14): `seat_session::remote`, shared `edition` / `rds` / `wts` / `launch`; one `seat_session::termwrap` (§10.4) |
| (removed) `crates/split-rdp-helper` | bin `pulsar-split-rdp-helper` | headless IronRDP client | step 7 (done 2026-11-14): `seat_rdp::headless` + `pulsar-seat-host --headless-hold` (§10.4) |
| (removed) `crates/split-display` | lib | `DisplayBackend` (rdp_session / vdd / pulsar_idd) | step 8 (done 2026-11-14): `seat_display::remote` + `seat_display::SeatDisplayBackend` (§10.6) |
| `crates/seat-audio` | lib | the one endpoint-discovery crate for both kinds (`cfgmgr`, endpoint-id model) + the remote kind's Remote Audio rules and stream-server keys (was `split-audio`) | step 8 (done 2026-11-14, §10.6) |
| `crates/seat-stream`, `crates/seat-pads`, `crates/seat-shim-core`, `crates/seat-setup` | lib | stream backends, virtual pads, shim core, setup sub-commands (were `split-*`) | kept |
| (removed) `apps/split-manager` | Tauri app `pulsar-split-manager` | remote seat manager | step 9 (done 2026-11-14): `apps/manager/src/lib/remote/` (§10.7) |
| `shim/` | separate Cargo workspace, x64 + x86 only | per-process HID shim | kept |
| `drivers/split-idd`, `drivers/split-pad` | drivers | own IddCx display, own HID pad | round 4 (2026-11-14): `split-idd` merged into `drivers/seat-idd` (remote role); `split-pad` kept; both built by `build-drivers.ps1` for ARM64 + x64, unsigned, never loaded ([../../drivers/README.md](../../drivers/README.md)); the MSI ships no own driver |
| (removed) `installer/split/` | packaging pieces | WiX fragment, NSIS hooks, dep pins, `fetch-deps.ts`, licences | step 10 (done 2026-11-14): one MSI; pins in `installer/remote/`, `installer/scripts/fetch-deps.ts`, licences in `installer/licenses/` ([../../installer/README.md](../../installer/README.md) "Remote seat kind") |
| `spec/remote/` | analyst spec | remote kind ([../../spec/README.md](../../spec/README.md)); the local kind is `spec/local/` | — |

Dependency rule: package crates depend on `seat-core` (and `seat-proto` where they speak
the protocol), **never on each other** — except `seat-service` (wires everything) and
`seat-host` (uses display / input / media host-side helpers). Shared PnP access goes
through `Arc<dyn seat_core::pnp::PnpTree>` built by the service from `seat-media`.
Package crates add their own deps in their own `Cargo.toml` with explicit versions; only
the contracts owner edits the root `workspace.dependencies`. Shared crates (merge step 8)
are the exception: `seat-media` depends on `seat-audio` (endpoint discovery), `seat-input`
optionally on `seat-pads` (feature `local-pads`), `seat-stream` on `seat-audio`. No `split-*`
crate is left.

## 3. Processes

```
Kernel (only once signed; fail open)   seat-input-filter · seat-storage-filter(*)   (seat-idd: UMDF)
Session 0
  pulsar-seat-service (LocalSystem)    config · state machine · pipe server · secrets (LSA)
                                       backends: SessionBackend, DisplayBinding, InputRouter,
                                                 AudioRouter, DeviceGate[] (+ WinPnpTree)
  WUDFHost: drivers/idd (only once signed)
Console session (seat 1, or a dedicated host session)
  pulsar-seat-manager (user token)     UI, tray; pipe client
  pulsar-seat-host --seat N --token-stdin (one per extra seat, started by the service)
      Remote Desktop ActiveX control window over the seat's monitors (bounding box;
      presenter guards only, no FrameSink: the control draws itself)
      raw input of the seat's devices / input_packets events → RDP input
      RDP audio: played by the control on the host session's default endpoint (gap:
      no per-seat endpoint until a frame/audio-producing client exists)
      two pipe connections: requests (hello, host_get_assignment, host_take_credentials,
      host_report) and events (subscribe host: input_packets, host_command)
      token (round 3 host-containment, 2026-11-13; seat_session::host_account,
      seat-service win/containment.rs): a fresh **batch logon of the local account
      `PulsarSeatHost`** per launch, hardened like before (**no privileges, Administrators
      deny-only, Low integrity**, `hardened_primary`) and moved into the host session. The
      account: random 32-character password rotated at every service start (LSA private
      data `L$PulsarSeat.HostAccount`), only `SeBatchLogonRight`, deny interactive / remote
      interactive / network / service logon, hidden from the logon screen, member of
      `Users` only (the service refuses it, untouched, when it is in any other local
      group, directly or through a domain group, or when the membership cannot be read).
      An existing user of that name whose comment is not `ACCOUNT_COMMENT` was not made by
      Pulsar (`Lookup::Foreign`): `ensure` refuses it and `remove` leaves it (logged).
      A batch token is not admitted by the session's `WinSta0` / `Default`: the service
      journals the logon SID (`run\host-grants.json`, fsync + rename) and runs
      `pulsar-seat-host --desktop-acl grant --sid S [--clipboard]` as SYSTEM (no
      privileges) in that session. Masks: window station = enum desktops, read / write
      attributes, global atoms, enumerate, `READ_CONTROL`, `SYNCHRONIZE`, plus
      `WINSTA_ACCESSCLIPBOARD` only when the seat's policy redirects the clipboard at
      launch; desktop = read objects, create window / menu, enumerate, write objects,
      `READ_CONTROL`, `SYNCHRONIZE` (no hooks, journal, switch-desktop, screen read,
      exit-windows). Revoked (`--desktop-acl revoke`) once the host is gone: platform
      exit / reap (also on every service tick), a sweep at service start, uninstall (after
      the service is stopped). Revokes never run under the containment lock. A NULL DACL
      is never edited. The pipe gets one ACE for the account (`pipe_ace`). Process /
      thread DACL `D:P(A;;GA;;;SY)(A;;RC;;;OW)`; the token's default DACL (later threads,
      every object the host creates) = SYSTEM + this logon SID full access, `OW` =
      `READ_CONTROL`, nothing for the shared account SID, so one host cannot open another.
      Start-up check in the host (`--token-stdin`): exit **43** = token not contained
      (refused, no fallback), exit **44** = as the account it cannot reach its desktop or
      the pipe; 44 and `STATUS_DLL_INIT_FAILED` switch the service to SYSTEM Low for the
      rest of its run. Recovery `HostTokenCompat` (DWORD) under
      `HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters`: 0 / missing =
      account, `1` = the older privilege-less System-integrity SYSTEM token, `2` = SYSTEM at
      Low integrity (round-2 token, no account); the service also falls back to `2` by
      itself when the account cannot be set up or a grant fails. Not seat 1's user (could
      debug it and read another seat's password). The control pipe has a Low `NW` label
      (`seat_proto::PIPE_LOW_LABEL_SDDL`). Credential prompts off, clipboard redirection off by
      default (IronRDP: `SeatConfig.redirect`, §4.8), listener owner (TermService PID) checked before every Connect, control
      auto-reconnect off. HYPOTHESIS (untested on hardware): IronRDP, ActiveX, WASAPI, raw
      input sink, cursor clip, COM, smart cards and the IDD section reader work in a batch
      logon without a loaded profile at Low integrity.
Seat sessions 2..N (RDP sessions on RDSH / wrapped TermService)
  pulsar-seat-agent (user token)       drive letters, wallpaper, notices
  pulsar-seat-manager (user token)     status, own-seat actions
```

(*) needs the maintainer's explicit OK.

**Host topology** (`settings.backends.host_topology`, spec 00 T6):

- `seat_one_console` (default): hosts run in seat 1's console session. Seat 1 keeps native
  GPU speed and no Winlogon value changes. Costs: seat 1's lock / UAC / sign-out hide the
  other seats; with `raw_input_host` other seats' keys also reach seat 1's foreground
  window — the manager shows `input_not_isolated` until the signed filter exists.
- `dedicated_host` (opt-in): a dedicated host account owns the console and runs only seat
  hosts; every seat including seat 1 is an RDP session. Raw-input isolation is complete in
  user mode (the host session's foreground window is always ours, `RIDEV_NOLEGACY`). Needs
  the documented auto-logon of that host account (maintainer decision, spec 30 §6.2.1).

## 4. Backends

Defined in `seat-core::backend`; all `Send + Sync`, synchronous, errors as
`BackendError { kind: ErrorKind, reason, message, win32 }`, availability as
`Availability` (`available`, `needs_setup`, `driver_not_signed`,
`driver_version_mismatch`, `unavailable{reason}`). Hot-plug through
`set_hotplug_sink(HotplugSink)` (`HotplugEvent::{Arrived, Removed, Changed}`).

### 4.1 Session (`SessionBackend`, crate seat-session)

| `SessionBackendKind` | Where | Default? | Seat sessions |
| --- | --- | --- | --- |
| `rds_loopback` | Windows Server + RD Session Host (x64 in practice) | **yes** | RDP loopback to `127.0.0.2`, client name `PSEAT-NN` |
| `termsrv_wrapper` | client SKUs with an operator-installed TermService wrapper | **off; experimental**, needs `experimental_ack == EXPERIMENTAL_WARNING_VERSION` | same as above |
| `fast_user_switch` | everywhere, ARM64 too | runtime fallback | one active seat at a time |
| `desktops` | client SKUs, ARM64 | opt-in | kiosk apps on the seat's monitors in the console session (no per-user isolation) |

Seat ↔ session matching (seat-session, 2026-11-12): the documented RDP ActiveX control used by
the seat host has no client-name property (the session's WTS client name is this PC's name),
so `wait_session` accepts our `PSEAT-NN` name **or** the newest loopback remote session that did
not exist when the seat start began, is not bound to another seat and has the seat's user.
This relies on seats starting one at a time. Details: `crates/seat-session/README.md`.

Update 2026-11-13 (fix-rdp-client): the default seat host client is now our own IronRDP
client (`crates/seat-rdp`, `seat-host/src/ironrdp.rs`). It sends the real client name
`PSEAT-NN` and reports `HostState::SignedIn { session_id }` from the server's logon info.
The host also sends the owner's `InputSettings` (`HostAssignment.input`, additive) to its
forwarder.

Update 2026-11-13 (final-fix), **done:** every `host_report` carries `rdp_client`
(`"ironrdp"` / `"activex"`, additive field). `Service::on_host_report` stores the reported
session id on the slot (`Slot.reported_session`) and passes a
`seat_core::backend::session::HostHint { session_id, legacy_client }` to the backend
(`SessionBackend::host_hint`, default no-op). `RdpCore::wait_session` then binds: with a
reported id, only that session and only if WTS confirms it (loopback, unclaimed, our
`PSEAT-NN` client name, the account); without one, the exact client name; the "newest new
loopback session" fallback only for the ActiveX client (`SeatMatch.newest_fallback`).

Logon-screen seats (no stored password) on an NLA listener — **decision 2026-11-13
(fix-rdp-client), HYPOTHESIS until tested:** `rds.rs` keeps RDP-Tcp `UserAuthentication=1`
(NLA on) for password seats, so a TLS-only connection gets `HYBRID_REQUIRED_BY_SERVER` and
the host reports `Failed { nla_required }`. Chosen design: **(a) a second WinStation listener
bound to loopback only, NLA off, used only by passwordless seats** (no secret crosses it; the
pinned TLS certificate still authenticates the server; the RDP-Tcp listener stays NLA-only).
Rejected for now: (b) a seat-host-drawn credential prompt fed into CredSSP (a credential UI
running as SYSTEM on the seat's desktop, and a Turkish logon UI to maintain). Until (a) is
built (seat-session `rds.rs` + listener pins for the second listener + service selection),
`RdpCore::start` refuses a passwordless seat on an NLA listener up front (2026-11-13
final-fix): `Policy` / `nla_required` → `FailureReason::NlaRequired` with Turkish copy
("Koltuğa parola kaydedin"). Design (a) itself is **still open**.

Update 2026-11-13 (second-listener), design (a) **built, untested on hardware**
(`seat-session/src/loopback_listener.rs`, `win/loopback_listener.rs`):

- Config: `backends.loopback_listener { enabled (default true), port (default 33389),
  extra_accounts }` (seat-core, additive). Trait: `SessionBackend::set_listener_plan(
  &LoopbackListenerPlan { accounts, passwordless_seats })` (default no-op). The service
  (`seat-service/src/loopback_plan.rs`) sends it before every `prepare_host` and after a
  config apply, a password change and a reset: accounts = every RDP seat's account (seat 1
  only under `DedicatedHost`) + `extra_accounts`; `passwordless_seats` = any RDP seat with
  logon screen or no stored password.
- `rds_loopback` only (`termsrv_wrapper` builds its inner backend with
  `without_second_listener`). `prepare_host` ensures, `restore_host` removes, a changed
  plan re-ensures while prepared.
- Ensure order: refusals first (port < 1024 or = RDP-Tcp, port held by another process,
  policy `UserAuthentication=1` or `SecurityLayer<2`, firewall service stopped or
  `EnableFirewall` not 1 in every profile, no account resolves) — each **removes** an
  existing listener. Then the firewall block rule (`Pulsar Seat - loopback RDP (block
  remote)`, remote = all but `127.0.0.0/8` and `::1`, netsh), then the key
  `WinStations\PulsarSeat-Loopback` (copy of RDP-Tcp's values, then `PortNumber`,
  `UserAuthentication=0`, `SecurityLayer=2`, `Shadow=0`, `fEnableWinStation=1`, `Comment`,
  `SSLCertificateSHA1Hash` = RDP-Tcp's so the host's pins stay valid, `Security` = SD with
  SYSTEM all, Administrators manage-only (no logon / connect), allowed SIDs "user access").
  Then a TermService restart (SCM, dependants included) **only while no remote session
  exists**, else `pending_restart` (retried at each passwordless start).
- Journal `journal\loopback-listener.jsonl` (`registry_key`, `firewall_rule`). Both names
  are reserved, so `remove` works without a journal; the rule is removed only once nothing
  listens on the port (else kept, `firewall_rule_kept`). Uninstall:
  `seat_session::win::revert_session_host` (listener + rule, then the RDS rows of
  `session.jsonl`, which uninstall did not revert before).
- Health before each passwordless start: our values intact, policy not forcing NLA,
  firewall on, rule present, TermService (only) listening on `127.0.0.2:<port>`
  (`listener_untrusted_reason`). `RdpCore::start(.., second)`: passwordless seat on an NLA
  RDP-Tcp → `Some(Ok(port))` connects there; `Some(Err(note))` →
  `loopback_listener_unavailable` (`ErrorKind::NotReady` → `FailureReason::RdsNotReady`);
  `None` (off / not offered) → `nla_required` as before. Probe note
  `loopback_listener:<state>[:<port|reason>]`.
- HYPOTHESES: TermService loads WinStation keys only at start; no LANA for loopback (so no
  bind, firewall instead); access masks of the SD; Windows Firewall filters this inbound
  traffic for non-loopback remotes only as configured.

Update 2026-11-13 (fix-second-listener; supersedes the matching points above):

- Firewall: one rule **per port**, `Pulsar Seat - loopback RDP (block remote) <port>`
  (prefix reserved; the old single-name rule is matched by the prefix and replaced). The
  rule for the wanted port is added first and read back through `INetFwPolicy2`
  (`LoopbackHost::firewall_rules`, `FirewallRuleInfo`, `rule_protects`); a rule for another
  port is deleted only once nothing listens on that port (`drop_stale_rules`). Extra
  refusals: `firewall_local_rules_ignored` (policy `AllowLocalPolicyMerge = 0` in any
  profile), `port_in_use` also for a port another WinStation key uses
  (`Registry::subkeys`).
- Key: allowlist copy (`COPIED`, type-checked), owned values incl. `LanAdapter = 0`,
  `fInheritAutoLogon = 1`, `fPromptForPassword = 1`, `fInheritInitialProgram = 1`,
  `FORBIDDEN` values deleted; write order `fEnableWinStation = 0`, `Security`, the rest,
  copies, `fEnableWinStation = 1`; `recover()` deletes a key whose last `registry_key`
  record is an `intent`. Value updates are journaled (`registry_value`).
- Restart: never while a user session other than the console exists (remote flag or a user
  name); forced after a key change; otherwise only when no restart (`termservice_restart`
  record) happened since the last change and the last one is 10+ min old.
  `RestartError::NotStoppable` → `restart_needs_reboot`.
- Health compares every owned value (the descriptor byte for byte with the plan's SIDs),
  `FORBIDDEN` absent, firewall state, verified rule, no other WinStation on the port.
  A passwordless start re-ensures a listener that is not ready.
- Removal TermService cannot follow yet: `RemoveReport.kept_ports`, notes
  `loopback_listener_unload_pending:<port>` / `loopback_listener_exposed:<port>`;
  `SessionBackend::retry_deferred` (seat-core, additive) → `finish_unload` after every seat
  stop and session logoff / end. `restore_host` clears `prepared` first and
  `ensure_listener` re-checks it under `busy`.
- `loopback_listener_unavailable` → `FailureReason::LoopbackListenerUnavailable`; the seat
  `detail` gets `listener=loopback_listener:<state>[:<x>]` (manager shows per-state Turkish
  copy).

Autostart (2026-11-13 final-fix): `start_autostart_seats` (boot `at_boot`,
`after_seat_one_logon`, enable) does nothing when the active session backend has
`caps().concurrent == false` (`fast_user_switch`): each start there switches the console.
Only a manual `start_seats` starts seats on that backend.

`fast_user_switch` additions (2026-11-13 final-fix, spec 35 §6.2): `HideFastUserSwitching=1`
(policy) or Winlogon `AllowMultipleTSSessions=0` (HYPOTHESIS that it turns FUS off) →
`Unavailable{fsu_disabled}` and `start` refuses; a second seat with the same account is
refused (`same_account_denied`; seat-core `SameAccountFastUserSwitch` issue when the
configured backend is FUS); after `ConsoleConnect` the service queues
`AgentTaskKind::ApplyTopology { targets }` (the seat's present monitors as CCD targets of
this boot) and the agent applies `seat_display::topology::seat_only_request` (only those
targets in the desktop; never none).

**Credential provider + filter (2026-11-13, credprov; spec 35 §5.4 / §5.5; untested on
hardware).** `credprov/` (C++ COM DLL `pulsar-seat-credprov.dll`, native ARM64 + x64, no
ARM64X: CredUI is refused) gives this backend auto-logon and a "switch seat" link:

- Registration (`seat_session::credprov`, `Service::sync_credprov`, run with every tweak
  sync and backend reselect): only while `settings.enabled` **and** the active session
  backend is `fast_user_switch` **and** the DLL next to the service is native
  (`check_dll`); only in session 0 (a `--console` dev run never registers); removed
  otherwise and by `uninstall`. Our four exact keys only; filter written last, deleted
  first. Recovery value `CredProvDisabled=1` under `Services\PulsarSeat\Parameters`.
  `FastUserSwitchBackend::caps().auto_logon` = provider listed and not disabled.
- One-time grant (`seat-service/src/credprov.rs`): when a start of seat N (not seat 1)
  returns a fresh session at the logon screen and seat N has an account with a stored,
  non-failing password, and the start was **not** an unattended one (a switch-link or agent
  switch start, `start_sequence_with(.., unattended: true)` →
  `credprov.unattended[seat] = generation`: those authenticate nobody, so the seat user
  types the password on the seat tile), the service arms `Grant { seat, armed_ms }` for exactly that session
  id (TTL 120 s). Dropped on logon / logoff / terminate of that session, on seat cleanup and
  when the registration goes away.
- Caller check in `hello` (`ClientKind::CredentialProvider`): SYSTEM token, image
  `%SystemRoot%\System32\LogonUI.exe` (`Platform::process_image`, `is_logonui_image`),
  process session = token session; then `Caller.credprov_session` = that session and
  `Access::CredentialProvider` allows only the four credprov requests, for that session.
  The DLL checks the other direction: server PID = the `PulsarSeat` service PID (SCM).
- Requests: `credprov_query{scenario}` → `CredprovSession { seat, auto_submit, hide_others,
  account, user_sid, switch_targets, pending }` (auto / hide only for `logon`, only with a
  live, untaken grant of this session **and** an account; `user_sid` = the seat account's
  SID via `Platform::account_sid`, looked up outside the state lock, positive results
  cached; `pending` = a `fast_user_switch` hand-over to this console session is under way
  and no grant is armed yet: `note_credprov_handover` just before `SessionBackend::start`,
  cleared when the grant is armed or not, on seat cleanup, unregistration, or after
  `HANDOVER_MS` = 60 s); `credprov_take_credentials` → `credentials` once (TTL, seat match,
  `taken` is set under the state lock, the secret is read after the lock is released
  (`credentials_unlocked`), then the grant is re-checked; a failed read drops the grant;
  `credentials_taken`, `no_grant`, `no_password`);
  `credprov_report{status, sub_status}` (non-zero NTSTATUS → password failing, notice
  `credprov_logon_failed`); `credprov_switch{seat}` (seat session callers only, not the own
  seat, one per 3 s; a running seat is reconnected through `SessionBackend::start` **without
  the stored password**, so the backend's `session_not_locked` refusal always applies; an
  off seat goes through `start_sequence_with(unattended)`, no grant). Seat 1 is a target
  only while `SessionBackend::console_seat_away()` names its session (the FUS backend
  remembers seat 1's console session + user SID as `home` when it first moves the console
  away, re-checks the SID, keeps it across `adopt`; `start(seat 1)` connects it back with
  the same lock rules).
- Agent switch (spec 35 §5.5(a)): `agent_switch_targets` / `agent_switch{seat}`
  (`Access::Agent`, own seat only, same checks as `credprov_switch`, FUS backend, no
  secure desktop). `pulsar-seat-agent` shows a notification-area icon with Turkish
  "N. koltuğa geç" items (`agent.rs` `win::tray`). Untested on hardware.
- DLL behaviour: README in `credprov/`. Inert outside LogonUI and for every scenario but
  logon / unlock; the filter hides others only with `hide_others`, only when the answer
  gives a seat tile (`SeatTileWillExist`, shared with the provider) and only if our provider
  is in the list; no service answer = no tiles, nothing hidden. `pending` → the provider
  polls (250 ms, at most 30 s) and calls `ICredentialProviderEvents::CredentialsChanged`
  when the answer changes; tiles are rebuilt in `GetCredentialCount`.

Selection at boot: the service probes the configured kind; if it is not `available` it
falls back to `fast_user_switch`, keeps the config unchanged and reports the reason in
`ServiceStatus.fallback_notes`. Experimental kinds are never chosen as a fallback.

Methods: `kind`, `caps -> SessionCaps`, `probe -> HostProbe`, `prepare_host(&GlobalSettings)`,
`restore_host`, `start(&SeatStartRequest) -> StartStep` (`LaunchHost(HostLaunch)` |
`Started(SessionInfo)`), `wait_session(seat, client_name, timeout)`, `query(seat, id)`,
`log_off(seat, id)`, `adopt(&[KnownSession])`, `enumerate`. Start sequence: module doc of
`seat-core/src/backend/session.rs`.

#### 4.1.3 Kiosk shell seats (`desktops`, spec 35 §5.1 U3 / D′, 2026-11-13 kiosk-shell)

Concurrent kiosk seats inside the console session, for client SKUs on x64 and ARM64. Not
true multi-seat: one Windows user, profile and clipboard for every seat.

- **Config (additive):** `SeatConfig.kiosk_shell {apps: [{title, command, args, autostart,
  monitor}], idle_reset_minutes}` (≤ 16 apps, idle 0 = off or 1..=1440) and
  `settings.kiosk.seat1_desktop` (default `true`). Issues `invalid_kiosk_shell` (error),
  `kiosk_shell_ignored_on_seat_one` (warning). Manager: "Kiosk" column and dialog in the
  basic tab (only with the `desktops` method; with another method and a seat that has a
  kiosk config, the hint `kiosk.backendOnly` explains why it is ignored), seat-1 toggle
  under the seat table. `command` must be an absolute, trimmed Windows path
  (`seat_core::validate::is_absolute_windows_path`, same rule as the manager);
  `kiosk::plan_for` also trims and drops relative commands (fix-kiosk-shell).
- **Launch:** `desktops::start` for a seat with `kiosk_shell` (seat 1 only when
  `seat1_desktop` is off) launches `pulsar-seat-host --kiosk-shell --seat N --plan <hex>`
  into the console session with the console user's token, in the seat's job
  (`seat_session::kiosk`: plan = apps, idle, seat1_desktop, fallback monitors; hex JSON on
  the command line, ≤ 24 000 characters; install dir = the service's own folder when
  `DesktopsOptions::install_dir` is empty). `log_off` first asks the shell to stop
  (`ShellStopper`: sets `Session\<id>\PulsarSeat.KioskStop.NN`, the manual-reset event the
  shell created as `Local\…`; HYPOTHESIS that the `Session\<id>\` prefix works from
  session 0), waits up to `stop_timeout` (3 s), then kills the seat's job (shell + apps).
  A focus-sink shell that had to be killed with no other sink shell running is followed
  by `pulsar-seat-host --kiosk-release` (ClipCursor(NULL) + LSFW_UNLOCK). `query` reports
  a dead shell as `Gone` also for seat 1 (the service restarts it).
  `SessionBackend::kiosk_shells()` lists the running shells (seat, session, pid).
- **Shell process** (`seat-host/src/kiosk/`, one per seat): connects as client kind
  `kiosk_shell` with its seat; the service verifies PID + session against
  `kiosk_shells()` (`Caller::kiosk_seat`, a verified service process: status access also
  under `admin_only`). Polls `list_devices` + `get_backends` every 3 s and takes the units
  and monitors whose effective mask is exactly its seat. Windows: launcher per monitor, a
  topmost task bar on the first monitor (apps get the rest), a layered software cursor.
  Apps start suspended in the shell's own kill-on-close job; windows of job processes are
  kept inside the seat (`kiosk::place`, every 500 ms: any of the seat's monitors, the
  nearest one's work area; a maximised window that covers its monitor ± 16 px is left
  alone), new apps get the seat focus. Only job processes and the shell get input. Every
  `SetWindowPos` on an app window is `SWP_ASYNCWINDOWPOS` and skips hung windows; the job's
  process list grows its buffer on `ERROR_MORE_DATA` and keeps the last good set on
  failure.
- **Input** (`seat_input::kiosk::KioskRouter`, pure + tested): `RawReader::seat(units)`
  (`RIDEV_INPUTSINK | RIDEV_NOLEGACY`) → `SeatForwarder` → router → posted messages. Text
  keys without Ctrl/Alt go as `WM_CHAR` from the seat's own key state (the app's
  `TranslateMessage` would add an unshifted duplicate for a posted `WM_KEYDOWN`); other
  keys as `WM_(SYS)KEY*`, with the seat's key state set on the target thread via
  `AttachThreadInput` + `SetKeyboardState` when a modifier is down, and again for the next
  key message to a thread that still has a modified state (so modifiers come back up;
  HYPOTHESIS that apps see it). `wParam` carries the generic `VK_SHIFT` / `VK_CONTROL` /
  `VK_MENU` (side in the extended bit); a key's up always goes to the window its down
  went to, also after the shell took the focus; release keeps the toggle bits. Mouse: hit
  test over the z-order (cloaked, hidden and our cursor/sink windows skipped), deepest
  child, client coordinates, capture until all buttons are up, double clicks only for
  `CS_DBLCLKS`. Non-client area: `WM_NCHITTEST` (SendMessageTimeout, 50 ms, abort if hung)
  on button down only; no `WM_NC*BUTTON*` is posted (DefWindowProc's modal loops read the
  real cursor and would hang the app); the router drags caption / borders itself
  (`DragStart` / `DragTo`, host moves or sizes from the rect at the press), caption buttons
  act on release over the same button (`WM_SYSCOMMAND` close / minimise / maximise-restore),
  caption double click toggles maximise, menu bar and system menu open in keyboard mode
  (`SC_KEYMENU`); top-level scroll bars get nothing (wheel works); popup menus an app opens
  itself still follow the real cursor. Touch: a primary contact that vanishes without "up"
  is released. Back to the launcher: bar button or Ctrl+Alt+Home (consumed).
- **Input filter:** with `kmdf_filter` active the raw reader sees nothing; the shell
  subscribes to the `host` topic as a verified kiosk shell and gets only its own seat's
  `input_packets` (`Scope::kiosk_seat`; never host commands). Each source is used only
  while it is the active one. This makes `seat1_desktop = true` + filter isolated.
- **Focus:** `seat1_desktop = false`: the shells compete for `Local\PulsarSeat.KioskFocus`;
  the holder keeps a 1x1 sink window (blank cursor) in the foreground
  (`AttachThreadInput` + `SetForegroundWindow`, `LockSetForegroundWindow`) and clips the
  system cursor onto it every 250 ms, only while the sink really is the foreground window,
  so legacy input of every device goes nowhere. The leader releases clip, lock and mutex on
  a graceful stop (stop event, `WM_CLOSE`, `WM_ENDSESSION`) and stops clipping when the
  service has been unreachable for 30 s.
  `seat1_desktop = true`: no shell takes the foreground; without the KMDF filter the kiosk
  seats' keys and mice also reach seat 1 (mice move and click the system cursor), and seat
  1's cursor can activate kiosk apps, which then get seat 1's typing (bar notice, manager
  hint).
- **Reset:** "Oturumu kapat" or idle timeout (only after input since the last reset):
  release held keys, terminate the app job, show the launcher, start the autostart apps
  again.
- **Hard limits (not a lock-down):** Ctrl+Alt+Del on any keyboard is the session's secure
  attention sequence: every seat goes to the security screen, and "Sign out" there ends
  all seats (the filter's `Sas` packet only shows the seat's launcher afterwards). Windows
  system hotkeys (Win, Win+R, Win+E, Win+L, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc) work from
  every keyboard without the filter and act as the console user on seat 1's desktop;
  `RIDEV_NOLEGACY` / `RIDEV_NOHOTKEYS` cannot stop them and a `WH_KEYBOARD_LL` hook cannot
  tell keyboards apart. Shown in the manager limits text and `tr::HINT`.
- **Audio (round 4 kiosk-audio, 2026-11-14):** one session = one system default endpoint,
  so the shell gives each app process of the seat a **per-app** default endpoint (the one
  in Settings > Sound > App volume and device preferences) instead. Endpoints: the seat's
  audio devices whose effective mask is exactly the seat (`SeatDevices::audio`, per flow
  the first `active` one, else the first; `seat_media::kiosk_audio::pick_endpoints`).
  Planner `seat_media::kiosk_audio::KioskAudio` over the trait `AppEndpointPolicy`
  (fake: `kiosk_audio::fake::FakePolicy`): `sync(pids)` sets new job processes (called in
  `launch` while the app is still suspended, and every 250 ms tick for children),
  `set_endpoints` re-applies to every tracked pid when the device-list poll (3 s) reports
  another endpoint id (hot-plug, port move) and clears a flow that lost its endpoint,
  `clear_all` runs before the job is terminated (reset / log out / shutdown). Windows:
  `seat_media::win::policyconfig::WinAppEndpointPolicy`, the **undocumented** WinRT factory
  `Windows.Media.Internal.AudioPolicyConfig` (layout as in open-source EarTrumpet, MIT):
  vtable slot 25 `SetPersistedDefaultAudioEndpoint(pid, flow, role, HSTRING)` for the roles
  console, multimedia and communications, device string
  `\\?\SWD#MMDEVAPI#<endpoint id>#<DEVINTERFACE_AUDIO_RENDER|CAPTURE>`, empty = clear.
  Build from `HKLM\...\Windows NT\CurrentVersion\CurrentBuildNumber`: < 17134 none
  (`os_too_old`), 17134..21389 IID `2a59116d-…`, ≥ 21390 IID `ab3d4648-…`
  (`seat_core::consts::PER_APP_AUDIO_*`). Fallback for every failure (old build, factory
  missing, a call failing): log once, the app stays on the session default, bar line
  `tr::AUDIO_UNAVAILABLE` / `tr::AUDIO_FAILED` (only when the seat has its own endpoint).
  The `desktops` probe reports `kiosk_audio_per_app` (build ≥ 17134) or `kiosk_audio_shared`;
  the manager shows per kiosk seat "Ses: çıkış … / mikrofon …", "koltuğa ayrı ses aygıtı
  atanmamış" or "bu Windows sürümünde … yok" under the Kiosk column (`model.kioskAudio`), and
  warns when two kiosk seats list the same program. This is not the system default
  endpoint setter that spec 35 §5.3 / spec 60 §5 exclude; it is a maintainer decision
  (2026-11-14) to use the per-app one. **Risk:** the interface is not a public contract;
  a Windows update may change its id or slot order (a wrong slot would call another
  method); the build gates and the activation check catch only an id change.
  **HYPOTHESIS (all untested on hardware):** the call works from the non-packaged shell
  (console user, medium integrity) and for a suspended process; running apps pick it up for
  new streams only (most re-open streams on a device change); Windows persists it per
  executable, so two kiosk seats running the same exe share one entry (last writer wins);
  clearing works only while the pid lives, so an app that ended on its own or a crashed
  shell leaves a persisted entry for that exe until its next kiosk launch overwrites it;
  child processes (browser audio services) are covered by the job tick or by the per-exe
  entry; capture routing works the same as render.
- **Limits / open:** apps reading raw input, DirectInput, XInput, `GetAsyncKeyState`,
  `GetCursorPos`; exclusive full-screen games; packaged apps; apps that hand off to a running
  instance. Untested on hardware: everything in the shell, the stop event across sessions,
  the release helper, the filter packet stream to the shell.

### 4.2 Display (`DisplayBinding` service-side, `FrameSink` host-side; seat-display)

`window_presenter` (default: CCD keeps the seat's monitors in the console desktop, the host
covers them with a top-most fullscreen window; marked degraded), `display_core`
(specialized display scan-out; per-target fallback to window), `pulsar_idd` (our IddCx, not
signed), `console_only` (fast user switching). Methods: `enumerate`, `apply_topology(&TopologyPlan)`,
`bind_seat(seat, &[DeviceKey]) -> SeatOutputs`, `release_seat`, `identify`, `set_power`.
Host side: `FrameSink::{configure(&SeatOutputs), present(&FrameRef), cursor, blank}`.
Monitor key = adapter anchor + `sub=t<target id>` (+ EDID fallback inside the backend).

**Console helper (display).** CCD, GDI monitor handles, DDC/CI and overlay windows need the
console session's input desktop; from session 0 `QueryDisplayConfig` fails with
`ERROR_ACCESS_DENIED`. The service therefore builds the binding with
`seat_display::create_binding_with_runner(kind, pnp, runner)`, where `runner:
Arc<dyn seat_display::ConsoleRunner>` (implemented by the service with seat-session's
launch-into-session) runs `pulsar-seat-host.exe --display-helper snapshot [--display-core]`
or `--display-helper exec <ConsoleCommand JSON>` in the active console session and returns
its stdout (one JSON `HelperReply` line). Processes already in the console (seat host,
tools) use `create_binding` (direct calls). Adapter anchors per spec 40 §6.1: PCI = location
path, USB adapter = instance path if the USB device has a serial else `container|hwid`,
root / software = instance path. Our own IddCx monitors (`Root\PulsarSeatIdd`) are never
listed. Hot-plug from session 0 = `CM_Register_Notification` on the monitor and display
adapter interface classes (debounced, inventory diff). The seat host uses
`seat_display::win::{presenter::WindowPresenter, dcore::HybridPresenter}` (via
`crates/seat-host/src/display.rs`, which also computes the RDP monitor layout with
per-monitor DPI and re-sends it after `WM_DISPLAYCHANGE` / `WM_DPICHANGED`).

**Display contract details (fix-display, 2026-11-12).**

- *RDP client vs FrameSink.* **Update 2026-11-13:** the default client is now our IronRDP
  client, which produces frames per RDP monitor into the host's `FrameSink`
  (`seat_rdp::output::OutputSplitter`); `display_core` / `pulsar_idd` can be offered with it.
  `FrameRef.output` / `CursorState.output` are indices into the **full**
  `SeatOutputs::outputs` list (clones included); the splitter numbers RDP monitors (clones
  excluded), and the host's `SharedSink` translates (`ironrdp::output_map`). The absolute
  pointer uses the same full-list index (`ironrdp::input_rects`) and follows the geometry the
  splitter really cuts (`SessionEvent::Outputs`). The rest of this point describes the
  ActiveX fallback (`--rdp-backend activex`): the control draws
  into its own top-most window (class `PulsarSeatRdpHost`) over the bounding box of the
  seat's monitors (spanning, no `UseMultimon`); it produces no frames for a `FrameSink`.
  Decision: keep it for `window_presenter` only. The presenter guards (`ClipCursor`,
  WinEvent move-back) treat `PulsarSeatRdpHost` as a seat surface, and the host runs them
  with `SeatDisplay::guard_only(&outputs)`. `display_core` / `pulsar_idd` need a
  frame-producing client and must be reported unavailable while the ActiveX client is the
  session client (service selection). `display::layout_for` (per-monitor, DPI) is the
  layout for that future client.
- *Per-seat options* the trait methods do not carry: the service calls
  `DisplayBinding::configure_seat(seat, &SeatDisplayPrefs)` before `bind_seat`
  (`seats.rs::run_start`; `Binding` copies it into its `SeatDisplayOptions`;
  `Binding::set_seat_options` stays for tests / tools). `software_cursor`, `outputs`
  (clone) and `follow_session_mode` come from the configuration
  (`SeatDisplayPrefs::from_config`). `frame_reader_sid` (integrate, 2026-11-13) is filled
  by the service from `Platform::host_frame_reader_sid` = `win::containment::
  frame_reader_sid`: the `PulsarSeatHost` account's SID while hosts run as that account
  (not `HostTokenCompat`, account set up, not failed), else `None` (SYSTEM Low hosts read
  SY + BA sections anyway). Without it a contained seat host could not open the
  `pulsar_idd` frame sections. The SID is fixed at `bind_seat`; a host that falls back to
  SYSTEM Low later still reads. Untested on hardware (IDD driver unsigned, never loaded).
  `software_cursor` keys end up in `OutputPlacement::software_cursor` and the IddCx add
  flag.
- *IddCx gating.* `Binding::set_session_has_display(false)` is needed before `pulsar_idd`
  plugs virtual monitors (spec 00 T1; default true = RDP loopback has its own display).
  `pulsar_idd` reports `isolates_console: false` (experimental) until spec 40 open
  question 1 is proven. A re-add can fail with `STATUS_OBJECT_NAME_COLLISION` while a seat
  host still holds the old frame section: reconfigure the host (drop its readers) first.
- *Presenter targets* (no HMONITOR): `identify` / `set_power` go to the seat host through
  `Binding::set_presenter_router` → `PresenterCommand::{Identify, Power}` (service:
  `host_command`; host: `HybridPresenter::{identify, set_power}`). `set_power(off)` there
  blanks and returns `Unsupported` / `blanked_only`.
- *Display Core outputs* that fail to acquire, go stale or are lost are retried every 2 s
  and reported with `PresenterEvent::Output { output, present }` (`waiting_for_display`).
- *Topology (display-topology, 2026-11-13).* `apply_topology` plans with
  `seat_display::topology::plan_topology`: under `display_core` / `pulsar_idd` each seat
  target (not shared with the console) leaves the console desktop **only** when
  `GET_MONITOR_SPECIALIZATION` says specialization is available (bits 1+2); it is
  specialized (`DisplayConfigSetDeviceInfo`, type 13, Pulsar's own type GUID, app name
  "Pulsar Seat") and its path removed. Without specialization it stays in the desktop
  (`fallback`). The console always keeps at least one monitor. Every target taken out is
  written to `%ProgramData%\Pulsar Seat\journal\display-topology.json` **before** the
  change (`Binding::set_journal_path`, set by `create_binding_with_runner`); journaled
  targets the plan no longer reserves are given back (specialization off, path re-added),
  also after a crash or a kind change; `release_ownership` (all monitors to the console)
  gives everything back. Targets the user specialized in Settings are never touched. The CCD
  part runs in the console helper (`ConsoleCommand::ApplyTopology` →
  `HelperReply::Topology`, own 45 s helper limit `console::TOPOLOGY_TIMEOUT`): the active
  paths are snapshotted **before** any specialization change (rollback target);
  `SDC_VALIDATE`, apply with `SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES`, re-query and
  `topology::verify_topology` (removed targets gone, every kept console path still
  active). A planning error, refused validation, failed apply or failed verify undoes this
  call's specialization changes first, then puts the pre-change snapshot back exactly
  (no `SDC_ALLOW_CHANGES`; once more with it when refused); targets still missing after a
  failed restore are reported `deactivated` so the journal keeps them
  (`TopologyReport::rolled_back`). Every plan arm that sets specialization journals
  `specialized: true`. Journal trust (Windows, service): the folder gets
  `PRIVATE_DIR_SDDL` (owner Administrators) when not trusted, the file is used only when
  owned / writable by SYSTEM or Administrators, not a reparse point and with one hard
  link (`win/winsec.rs`, adapted from seat-compat); an untrusted file is moved to
  `display-topology.json.untrusted-<ms>`, an untrusted folder refuses the journal and no
  target is taken out. The temporary file is written with `create_new`. Removing the primary shifts the remaining sources so one
  sits at (0, 0). `TopologyReport` gained `removed`, `restored`, `rolled_back` (additive).
- *Clone.* `AssignmentOptions::outputs` (target ids on the surface monitor's adapter,
  needs `OutputSettings::allow_clone`) reaches the binding through
  `DisplayBinding::configure_seat(seat, &SeatDisplayPrefs::from_config(&config, seat))`
  (default no-op trait method; the service calls it in `run_start` right before
  `bind_seat`). `bind_seat` appends one `OutputPlacement { clone_of: Some(i) }` per further target
  after every non-clone entry, so RDP monitor `i` = `outputs[i]`
  (`SeatOutputs::layout_outputs`). Both presenters show output `i`'s frames and cursor on
  its clones (own mode, letterbox). The ActiveX client cannot clone: the host covers
  desktop clone targets with a presenter window and a Turkish notice. IddCx monitors are
  plugged only for non-clone entries. `caps.clone` is true for every kind except
  `console_only`.
- *Window presenter rendering.* D3D11 flip model per window (`win/flip.rs`:
  `FLIP_DISCARD`, 2 buffers, `DXGI_SCALING_STRETCH`, frame latency 1 + waitable object).
  The UI thread waits with `MsgWaitForMultipleObjectsEx` on messages and the waitables of
  outputs with new content, so presents are vblank-paced (`Present(1, 0)`, newest frame
  wins). Dirty rects are uploaded into a frame-sized texture with the cursor blended on the
  CPU; the buffer has the window's aspect ratio (`model::flip_buffer`), DXGI stretches it.
  Any D3D failure moves that output to the GDI blit; after `DXGI_ERROR_DEVICE_REMOVED /
  HUNG / RESET` (driver update, TDR) the flip chain is re-created on the next display
  change or every 2 s (at most 5 losses, 3 failed creations in a row, then GDI for good).
  At most 32 dirty rects are queued per GPU output, more = full upload.
  `PULSAR_SEAT_PRESENTER=gdi` forces GDI.
- *Mode follow.* `OutputPlacement::follow_mode` (default: presenter targets only;
  `SeatDisplayPrefs::follow_session_mode` overrides, from the additive
  `OutputSettings::follow_session_mode: Option<bool>`, manager "Görüntü çıkışları" dialog). When the frame size differs from the
  target for 1.5 s and the target has that exact mode (`model::pick_follow_mode`), the
  window presenter switches the desktop monitor dynamically (`ChangeDisplaySettingsExW`,
  reset to the registry mode when the presenter stops or the output stops following; each
  change is recorded first in `%TEMP%\Pulsar Seat\mode-follow\<DISPLAYn>.json` with owner
  pid + creation time, and the next presenter start of the same account resets records
  whose owner is gone; a host of another account cannot, so that mode stays until logoff
  or reboot) and Display Core re-acquires the
  target at that mode (previous mode restored on failure). Otherwise it scales.

### 4.3 Input (`InputRouter` service-side, `InputSink` host-side; seat-input)

`raw_input_host` (default, user mode; isolation complete only under `dedicated_host`),
`kmdf_filter` (our filters, not signed), `none`. Methods: `enumerate`,
`apply_routes(&[InputRoute]) -> RouteReport`, `set_wizard_capture`, sinks for hot-plug /
activity (`ActivityEvent`) / packets (`PacketSink`, KMDF path) / hotkey (`HotkeyEvent`).
Packets: `InputPacket::{Key{sc,f}, Move, Abs, Buttons, Wheel, Touch, Consumer, Sas}`.
Unit key: spec 50 §4.1.1 via `pnp::KeyParts` (`col=<n>:<page>:<usage>`). Seat mask is
mirrored to the private DEVPROPKEY `DEVPROP_SEAT_GUID` pid 2.

**Console listener (`raw_input_host`, input-reporter 2026-11-13; untested on hardware).**
Session 0 sees no input and each seat host reads only its seat's units, so the service
(`seat-service/src/listener.rs`) keeps one `pulsar-seat-host --console-listener --token-stdin`
in the console / host session while the product is enabled with `raw_input_host`
(`BackendFactory::console_listener`): restricted SYSTEM token + SYSTEM-only DACL like a
seat host, relaunched (5 s back-off) when it dies, the console session changes or it is
silent (no first report in 10 s, or none for 2 x `CONSOLE_LISTENER_TIMEOUT_MS`). It reads
raw input of every unit as a sink (`RawReaderConfig::listener()`, no `RIDEV_NOLEGACY`) and
runs `seat_input::console_report::ConsoleReporter`: activity per unit (200 ms throttle,
clicks always), Ctrl+hotkey and the emergency combination (same `UnitProcessor` as the
host), and key make codes **only while the capture wizard runs** and only of the wizard
keys (`console_report::is_wizard_key`: F1..F12, top-row digits 1..0, Esc / Backspace /
Enter / Shift / Alt; maintainer decision 2026-11-13: never every key; the monitor pick uses
the digits, "press any key" accepts an F-key or a digit; no auto-repeat, E0 as `0xE0xx`; outside the wizard no key code leaves the process, and the service never
logs scan codes). It sends `console_input_report{events}` every 100 ms when there is
something (in batches of `MAX_CONSOLE_REPORT_EVENTS`, hotkey / emergency first, nothing
dropped), else a heartbeat every 1 s; the reply
`ConsoleListenerState {wizard_running, input, stop, wizard_all_keys}` updates it
(`wizard_all_keys` is obsolete: the service always sends `false`, the listener ignores it). `hello`
refused with `access_denied` is retried 10 x 200 ms (the service stores the listener's PID
only after its launch call returns). The service maps `interface` to the unit key
(normalized path), then activity -> `on_activity` (wizard, idle saver, `device_activity`),
hotkey -> `on_hotkey` (from = override / effective seat; ignored while the hotkey setting is
off and while the wizard runs; moves the keyboard, mice + HID collections on the same parent
and, from seats 2+, the mice on its seat), emergency -> `on_emergency` (always: Ctrl+Shift +
the hotkey, or F12 when the hotkey is off, three times; cancels a running wizard and drops
pending hotkey overrides). The router flag is set under the service state lock. While
reports arrive, `InputControl::set_console_reporter(true)`
makes `RawInputRouter` report `activity`/`hotkey`/`wizard_capture` caps, drops the notes
`activity_needs_console_listener` / `hotkey_needs_filter` and accepts
`set_wizard_capture(true)` as **observe-only** (units still reach their sessions).
Seat hosts swallow the hotkey but do not report it (the listener does; no double move).
`host_get_assignment` honours pending hotkey overrides, so a moved keyboard is read by its
new seat's host at once. Limits: under `seat_one_console` the hotkey / wizard keys of seat-1
units also reach the console's foreground window; on the secure desktop (lock screen, UAC)
raw input does not reach the listener.

### 4.4 Audio (`AudioRouter`; seat-media)

`rdp_bridge` (default; RDP audio channels bridged by the seat host to the seat's endpoints
by MMDevice id), `console_shared`, `none`. Methods: `enumerate`,
`bind_seat(seat, &AudioBinding) -> AudioRoute`, `release_seat`, `test_tone(key, ms)`.
Seat 1 still sees every endpoint (no driver can hide them rule-clean); the manager says so.
**Test tone from the console session:** in session 0 the service routes `test_tone` through
`AudioRouterImpl::set_tone_player` to `win::console_runner::play_tone_in_console`, which
starts `pulsar-seat-service.exe --audio-helper tone <endpoint id> <ms>` with the restricted
SYSTEM token in the console session (same runner as the display helper); the helper calls
`seat_media::play_tone_on_endpoint` and prints one JSON line. A `--console` dev run plays
the tone itself. Endpoint ids are checked (`cli::valid_endpoint_id`) before they reach a
command line. HYPOTHESIS: WASAPI shared-mode playback works from that token; untested on
hardware.

### 4.5 Devices (`DeviceGate`; seat-media)

Several gates at once (`settings.backends.gates`, default `device_security` +
`session_drive_letters`); also `camera_consent`, `storage_minifilter` (exception, not
signed). Methods: `enumerate`, `apply(&GatePlan) -> GateReport`, `revert_all`. A
`GatePlan` = effective masks + current `SeatLogon`s (logon SIDs) + `admins_bypass` +
`class_defaults`. Re-run at service start and every seat logon / logoff; idempotent.

- **`printer_acl`** (optional, `gate::printer`): queues of a seat-owned USB printer (port from
  the `GUID_DEVINTERFACE_USBPRINT` interface's `Base Name` + `Port Number`, queues from
  `EnumPrintersW` level 5) get a DACL with SYSTEM, Administrators (repair-only unless
  `admins_bypass`), CREATOR OWNER on jobs and print-use for the owning seats' logon SIDs.
  Rewritten every apply (stale SIDs go), originals journaled in `gate-printer-acl.json`,
  restored on shared / `revert_all`. Isolation status `descriptor`. Per-seat use, not
  visibility. Several gates may report one device: the service keeps an enforcing status
  over another gate's `not_supported`.
- **Class-level default descriptors** (`gate::classdefault`, run by `device_security` when
  `settings.devices.class_default_descriptors`; classes `settings.devices.class_default_classes`,
  empty = `DeviceClassTarget::DEFAULT` = CD-ROM + Ports; **disk and volume are refused**
  with `class_not_supported` and never closed, records of them from older builds are
  released: their members include the system disk / volume, which stock Windows keeps
  admin-only for raw opens, and a fixed open descriptor would hand raw access to every
  user): first every present devnode of the class that the gate does not gate gets an
  explicit open descriptor (`classdefault::open_sddl`, `gate-class-open.json`; a value
  someone else wrote later is left alone), then the class gets `D:P(A;;GA;;;SY)[(A;;GA;;;BA)]`
  (`SPCRP_SECURITY_SDS`, `gate-class-defaults.json`). Leaving restores the class first, then
  the devnodes; a devnode that the gate started to gate meanwhile gets the true original
  handed to the gate's own record. Internal devnodes are never restarted, so an internal
  device that appears after enabling stays closed until its next start (documented limit).
  Off by default; system change needing the maintainer's sign-off. Untested on hardware.
- **Gate lifecycle in the service:** `reselect_backends` calls `revert_all` on every gate
  that leaves the active set (unticked or no longer available) before the swap; when the
  session backend kind changes while the product is enabled it also runs the old
  backend's `restore_host` before and `sync_listener_plan` + the new one's `prepare_host`
  after the swap (as disable / enable would; failures logged as `BACKEND_FAILED`);
  `uninstall` (service stopped) builds every gate from `run\` with
  `seat_media::create_gate_in` and calls `revert_all` when any gate journal is left (the
  minifilter always; "driver missing" is not a leftover), failures = `EXIT_PARTIAL`.

### 4.6 Effective masks and validation (seat-core)

`assign::effective_masks(&Config, &[Device])`: explicit assignment → the device's
**container** assignment (`cont;id=<guid>`, never the system container) → nearest first along
the USB chain: the device's own **port** (`port;loc=<location path up to its last USB(n)>`),
its hub, that hub's port, the next hub, … (exclusive kinds only from single-seat
assignments) → kind default (input and monitors seat 1, others shared, network always
shared). Port and container keys (`seat_core::pnp::{port_key, container_key, port_chain}`)
are assignments, not devices: the service inventory shows an assigned one as an entry that
is present while something is plugged in / a member is live, never lists an empty assigned
port as missing, and every `DeviceView` carries `port_key` / `container_key` (container
only when it groups more than one live device) so the manager can offer them. The manager's
own mask preview (`apps/manager/src/lib/model.ts`) does not know ports / containers yet. `validate::validate(&Config,
Option<&[Device]>) -> Vec<Issue>` with `Severity::{Error, Conflict, Warning}` and stable
`IssueCode`s; errors block `apply_config`, conflicts need `force: true`. The service adds
checks that need this PC (`Service::validate_full`): cores per seat via
`seat_compat::cpu::validate_sets` (`cpu_no_core` error, `cpu_unknown_id` /
`cpu_only_efficiency_cores` warnings). seat-core also warns `monitor_hub_mismatch` when a
monitor's same-container hub keeps a seat the monitor is not on.

### 4.7 Service-side system effects (seat-service)

- **Tweaks:** `Platform::sync_tweaks` / `revert_tweaks` (Windows: seat-compat
  `tweaks::sync_config` / `revert`, journaled in `journal\changes.jsonl`). Run at boot, on
  `apply_config` when `settings.tweaks` changed, and on `set_enabled` (disabled = revert
  all). Uninstall reverts through the same journal.
- **Seat images:** `seat-service/src/images.rs` draws `images\seat-NN.png` (seat colour +
  number) and `images\lock.png` (1-bit palette PNG, own art, ~260 KB). Written when
  missing at boot, before `set_wallpaper` and before the lock-screen tweak.
- **Keep awake:** `Platform::set_keep_awake` holds a `PowerRequestSystemRequired` power
  request while any extra seat is active (`Service::update_keep_awake`, every tick;
  released on shutdown).
- **Idle saver:** `timings.monitor_idle_off_s` (0 = off): an active extra seat without
  input for that long gets `DisplayBinding::set_power(false)` on its monitors; the next
  activity event on that seat wakes them. Needs activity events (not produced under
  `raw_input_host` yet).
- **Hotkey moves:** applied at once as `State.input_overrides` (input routes only), saved
  by `resolve_input_move{accept: true}`, dropped on decline or service restart; the
  manager asks through the `input_move_pending` notice.
- **Seat job (spec 80 §11.2):** at every seat logon the agent and the seat's
  `startup_apps` (absolute paths only) are started suspended, put into one job object per
  seat session (`seat_compat::launcher::SeatJob`, hard limit = job group affinity, basic
  affinity fallback only on a single-group PC with the limit in group 0; soft =
  `SetProcessDefaultCpuSets`), then resumed (`LaunchSpec.job`, `WinPlatform::confine`).
  **Launcher request** `start_seat_app { path, args }` (additive): any process of the seat
  session's signed-in user (user SID checked, caller's own seat only, absolute local path)
  asks the service to start a program the same way, answered `app_started { pid }`.
  Startup apps start once per logon; the job is dropped on every logoff / session end (the
  console seat's too: session ids are reused, a hard limit cannot be cleared) and when the
  seat stops. A config change of `cpu_sets` / `cpu_hard_limit` reaches the running job at
  once (`commit_config` -> `Platform::update_session_job`: hard limit for the whole job,
  soft limit re-set on the processes the service started; a hard limit cannot be removed
  until the session's job is dropped). Seat hosts and the console listener never get a
  job. Not covered: Explorer and anything started from it / the Start menu, UAC-elevated
  programs; the agent does not call the launcher request yet. Untested on hardware.
- **Source IP (spec 80 §11.3), best effort:** every program the service starts for the
  user (startup apps, launcher request) whose seat rule (`ip_binding` +
  `settings.ip_programs`) picks it gets `PULSAR_SEAT_SOURCE_IP=<address>`
  (`seat_compat::srcip::{from_config, launch_env}`); the agent never gets it (children
  inherit the environment whatever the rule says). Nothing forces the socket source
  address of an unmodified app; that needs a WFP bind-redirect callout driver (maintainer
  decision).
- **SYSTEM children:** seat hosts get `D:P(A;;GA;;;SY)` as process / thread DACL
  (`win/platform.rs` `launch`); the console helper and seat-session SYSTEM launches pass
  `SpawnSpec.object_sddl = SYSTEM_ONLY_SDDL`.
- **Host account (round 3 host-containment):** `host_account::ensure` at every service
  start (`AccountOps`, Windows `win/host_account.rs`), `remove` at uninstall; token choice
  and desktop grants in `win/containment.rs` (`Containment<HostSys>`, mock-tested), see §3.
  `LaunchSpec.host_clipboard` = the seat redirects the clipboard (window-station clipboard
  right of the grant). `Platform::reap_exited` runs every tick so a host still exiting
  after `terminate` loses its grant once it is gone. A contained launch without a
  readable creation time fails (its grant would look dead at the next sweep). Uninstall:
  stop the service, revoke every journaled grant, delete the journal, then remove the
  account (rights, account, logon-screen entry, secret). Untested on hardware.

### 4.8 Seat host RDP client (seat-rdp, 2026-11-13)

- **Backends:** `ironrdp` (default, `seat-host/src/ironrdp.rs` over `crates/seat-rdp`) and
  `activex` (fallback, `host.rs` + `rdp/win.rs`). They are chosen with `--rdp-backend` or
  `PULSAR_SEAT_RDP_BACKEND`. The host never falls back automatically: the control cannot
  pin the certificate.
- **Security:** listener owner check before each connect, peer check after the TCP
  connect. The TLS certificate SHA-1 must match `HostAssignment::listener_cert_sha1`
  (from the service, `seat_rdp::pin::listener_pins`: RDP-Tcp `SSLCertificateSHA1Hash`,
  or else the `Remote Desktop` machine store). The host reads the pins itself when the
  list is empty. No pins means no connection (`cert_unpinned`). With a password the
  host uses CredSSP only. Without one it uses TLS only, sends no secret, and reports
  `nla_required` when the listener refuses TLS-only.
- **Graphics:** BGRA framebuffer with a software pointer, cut per RDP monitor (=
  `SeatOutputs::layout_outputs()` index) into `display::SeatDisplay::new`'s `FrameSink`.
  The presenter clones on its own. Presenter layout changes go to display control.
  `host_command: blank` drops frames and shows the idle text. EGFX (MS-RDPEGFX) through
  `ironrdp-egfx` (`seat-rdp/src/gfx.rs`) with Media Foundation H.264 decode
  (`seat-rdp/src/h264/`, hardware first, software fallback at start and at run time)
  composites into the same framebuffer; `ResetGraphics` resizes follow without a
  reactivation; lost pictures trigger a redraw, then a graphics protocol reset. Decoded
  pictures are read back to CPU memory (zero-copy D3D11 feed deferred). Overrides
  `PULSAR_SEAT_GFX`, `PULSAR_SEAT_H264_DECODER`; metrics in `SessionEvent::Graphics`.
- **Input:** `SeatInputSink` = `RdpInputSink` over `seat_rdp::Handle` (fast-path, MS-RDPEI
  touch / pen). `Move` packets become relative events while the server hides the pointer
  and announced `INPUT_FLAG_MOUSE_RELATIVE`.
- **Audio:** rdpsnd (PCM) to `AudioBridge::start_playback/play`. `AUDIO_INPUT` from
  `AudioBridge::start_capture`. `rebind_audio` maps to `AudioBridge::set_route`. Dead
  streams are reopened every second.
- **Reports:** `connecting`, `connected`, `signed_in {session_id}` (from the server's logon
  info). The final report follows the ActiveX classification: the errorInfo code is
  used as the extended reason. `failed {logon_failed | cert_mismatch | cert_unpinned |
  rdp_peer_untrusted | nla_required | tls | network | protocol}`.
- **Reconnect:** in the host, only for network failures, at most 4 times (1/2/4/8 s),
  with every check run again. After that the host exits and the service backs off.
- **Token:** the service passes it on stdin (`--token-stdin`: `u32` LE length + UTF-8,
  max 512 bytes, read once) through an inherited anonymous pipe that is the child's only
  inherited handle (`seat_session::win::process::spawn`); `--token T` is still accepted
  for manual runs.
- **Redirection channels (2026-11-13, rdp-channels):** policy `SeatConfig.redirect`
  (`seat_core::config::SeatRedirect`: clipboard direction `off | to_seat | from_seat |
  both` + classes text / html / images / files; drives `off | read_only | read_write`;
  smart cards on/off; all off by default). The service resolves it per host into
  `HostAssignment.redirection` (`seat_core::backend::session::HostRedirection`, boxed,
  serde default) with `seat_rdp::policy::host_redirection`: drives = present volumes whose
  effective mask is this seat alone, by drive letter (pinned to its volume, see below); readers = present smart-card readers
  whose mask contains the seat (instance id + friendly name). The host
  (`ironrdp.rs::redirect_policy`) gives it to the engine; `seat_rdp::channels` joins
  `cliprdr` (filter around IronRDP's Windows clipboard back-end on a host STA thread,
  `channels::win::start_clipboard`) and `rdpdr` (`SeatRdpdr` around IronRDP's Windows
  drive / WinSCard back-end; system drive refused, read-only per policy, reader list
  filtered, other readers `SCARD_E_UNKNOWN_READER`, reader admin `SCARD_E_NO_ACCESS`,
  card cache not shared). `rdpsnd` is advertised with a silent sink when `rdpdr` needs it
  and audio is off. Printers / ports: never. Refusals: `channels::RefusalLog` (stderr,
  once per 10 s per reason). The clipboard bridge is the host session's clipboard (shared
  with seat 1 and every seat that has it on: note `clipboard_shared_with_host_session`).
- **Redirection hardening (2026-11-13, fix-rdp-channels).** Single source of truth:
  `seat_rdp::policy` (the old `seat_media::redirect::seat_redirection` rules are gone;
  `seat_media::redirect` keeps only `seat_cameras`).
  - *Drives.* The objection "drive redirection adds an ungated `\\tsclient\X` path" is
    answered by gating that path with the same ownership: only volumes whose effective mask
    is exactly the seat; each `RedirectDrive` carries the volume GUID path its letter named
    at resolve time (`volume`, serde default), re-checked with
    `GetVolumeNameForVolumeMountPointW` right before the root is opened on every connection
    and channel reset (`drive_letter_moved`; empty = `drive_volume_unknown`); open root
    handles stay bound to their volume. Only **FAT / FAT32 / exFAT**: drive IRPs run under
    the seat host's contained token (SYSTEM Low or the host account), not the seat user's,
    so on an ACL'd volume reads would follow the host's rights (confused deputy) and Low
    integrity would block writes; impersonating the seat user from a Low-integrity process
    is not possible (identification level only). NTFS / ReFS volumes get
    `drive_acl_volume_refused`; the seat session still reaches them directly through the
    gates. Live updates: `Reconfigure` re-reads the assignment into
    `channels::LiveRedirection`; the channel ticker removes drives no longer assigned (or no
    longer writable) from the session (`Rdpdr::remove_drive`, `drive_unassigned`); new
    drives appear at the next connection.
  - *Smart cards.* Readers are matched by device instance id
    (`SCardGetReaderDeviceInstanceIdW`); a known id never falls back to the name. The name
    is used only without an id, when the service marked it unique on the PC
    (`SmartCardReaderRef.name_unique`, serde default false) and only one present reader
    carries it; otherwise `reader_name_ambiguous` (two identical models on two seats are
    never told apart by name). `ListReaders` honours the requested groups. The PnP pseudo
    reader's count (high word) is translated to the PC count before the wait and rewritten
    to the filtered count in the reply (encoded `DR_CONTROL_RSP` patched in place), so a
    seat never learns other seats' reader count. Reader refs follow `LiveRedirection`.
  - *Clipboard.* An empty Format List goes to the seat only as the initial list of a
    connection or to withdraw host data the seat still holds; a refused console copy never
    empties the seat's own clipboard. A fully refused seat copy releases console ownership
    of the previous (unrenderable) seat copy. Console copies owned by another seat host
    (IronRDP clipboard window class in another process) are refused
    (`clipboard_from_other_seat`) unless `ClipboardRedirect.cross_seat`. OS-side answers are
    tagged with their connection (`ChannelCommand::Clipboard { connection, .. }`) and matched
    to pending requests (`stale_clipboard_response`).
  - *ActiveX backend.* `ClientPolicy::from_redirection`: clipboard only when the policy is
    unfiltered (both directions, all classes, `cross_seat`); drives / smart cards never
    (the control would offer every drive and reader); refusals reported.
  - *Reporting.* Notes go to `SeatRuntime.redirect_notes` (service, at
    `host_get_assignment`); host refusals go to the service (`host_redirect_refusal`) and
    to `SeatRuntime.redirect_refusals` (last 10). The manager shows both (Durum tab) and
    edits the policy per seat (Temel tab, column "Yönlendirme").
- Untested on hardware.

## 5. IPC contract (seat-proto)

- Pipe `\\.\pipe\PulsarSeat.Control`, byte mode, `PIPE_REJECT_REMOTE_CLIENTS`, first instance
  with `FILE_FLAG_FIRST_PIPE_INSTANCE`, DACL `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012019b;;;IU)`.
  Interactive users get `FILE_GENERIC_READ | FILE_GENERIC_WRITE` **without**
  `FILE_APPEND_DATA` (= `FILE_CREATE_PIPE_INSTANCE` on a pipe), so a standard user cannot
  create rogue server instances on the name. Every client opens with
  `seat_proto::PIPE_CLIENT_ACCESS` (0x0012018b, no `GENERIC_WRITE`) and
  `SECURITY_IDENTIFICATION`; production clients also check that the server PID is the
  running service.
- SACL `S:(ML;;NW;;;LW)` (`PIPE_LOW_LABEL_SDDL`, 2026-11-13): Low-integrity seat hosts and
  the console listener may write; the DACL still decides.
- Connection limits (`win::pipe`, 2026-11-13 final-fix): `MAX_INSTANCES` 128; ordinary
  clients at most `MAX_UNTRUSTED` 64 in total and `MAX_PER_SESSION` 16 per client session,
  and they must send their first request within `FIRST_REQUEST_TIMEOUT` (5 s). Live
  processes the service launched as infrastructure (`LaunchSpec::service_child`: seat hosts,
  console listener, agents; adopted hosts) are recognised by client PID right after the
  connect (`WinPlatform::is_service_child`) and not counted, so 16 seats with 2 host
  connections each in the console session are never refused, and idle user connections
  cannot lock them out. The exemption only affects counting, never authorisation.
- Launcher (`start_seat_app`): at most 10 starts per 10 s and 64 live launcher / startup
  apps per session; exited child handles are reaped on every launch (`WinPlatform::reap`).
- Frame = u32 LE length + UTF-8 JSON, ≤ 1 MiB. Client sends `Request {version, id, body}`;
  service sends `ServerFrame::Response(Response {version, id, result})` or, after
  `subscribe`, `ServerFrame::Event(EventFrame {seq, at_ms, event})`.
- `body` / `event` = `{type, data}` snake_case. Errors: `ProtoError {code, message, field?,
  reason?, win32?}`; `From<BackendError>` maps `ErrorKind` → `ErrorCode` and keeps `reason`.
- `PROTOCOL_VERSION = 3` (2026-11-14, seat kinds: see §10.2), `MIN_PROTOCOL_VERSION = 1`. First request must be
  `hello(Hello {client: ClientKind, client_version, protocol_version, seat?, token?})` →
  `HelloReply {service_version, protocol_version, min_protocol_version, caller: CallerInfo}`.
- Authorisation: `RequestBody::access() -> Access::{Any, Status, OwnSeat(seat), Admin,
  Host(seat), Agent, ConsoleListener}`, checked against the caller's token (impersonation); `Admin` needs an
  elevated token (Administrators enabled) or SYSTEM; `admin_only` restricts everything but
  `Any`; `Host` needs the PID the service started + the one-time `--token`.
- Requests: connection (`hello`, `subscribe{topics}`, `poll_events`), status (`get_status`,
  `get_backends`, `preflight`), config (`get_config`, `validate_config`, `apply_config{config,
  base_revision, force}`, `reset_config`, `import_reference`, `set_seat_password` write-only,
  `list_local_users`, `list_cpu_sets`), devices (`list_devices`, `compare_saved`,
  `identify_device`, `move_assignment`), seats (`set_enabled`, `start_seats{seats?}`
  async, `stop_seat`, `relogon_seat`, `request_reboot`, `resolve_input_move{accept}`:
  save or drop the hotkey moves the service applied as runtime route overrides), wizard (`wizard_start{variant}`,
  `wizard_command`, `wizard_stop{save}`; 2026-11-13 additive: commands `set_main_devices{keys}`
  and `target_hit{seat}`, `wizard_progress` gains `monitors`, `confirmed`, `keys_seen`,
  `last_unit`, `variant`; `console_listener_state.wizard_all_keys` is obsolete (always
  `false`, ignored; the listener reports only the wizard keys); see "Wizard and manager additions"), `diagnostic_report{mask_users}`, updates (all
  Admin, answer `update_status`: `get_update_status`, `check_for_update` and
  `download_update` async, `install_update{version, confirm}`; opt-in
  `settings.updates{check, weekly}`; `seat-service::update`, installer/README.md
  "Updates"), app API
  (`my_seat`, `seat_of_session`, `seat_of_process`), host (`host_get_assignment`,
  `host_take_credentials` once, `host_report{state}`), agent (`agent_report`), console
  listener (`console_input_report{events}` -> `console_listener_state`; client kind
  `console_listener`, hello with the one-time `token`, PID + session checked; §4.3),
  credential provider (`credprov_query`, `credprov_take_credentials`, `credprov_report`,
  `credprov_switch`; client kind `credential_provider`, SYSTEM + LogonUI image + session
  checked in hello; §4.1, 2026-11-13), kiosk shell (client kind `kiosk_shell` with its
  `seat`, PID + session checked against `SessionBackend::kiosk_shells()`; status access
  also under `admin_only`; may subscribe to `host` and then gets only its seat's
  `input_packets`; §4.1.3, 2026-11-13).
- Topics / events: `status` (`product_state`, `seat_state`, `config_changed`, `notice`),
  `devices` (`device_arrived/removed/changed`), `activity` (`device_activity`), `wizard`
  (`wizard_progress`), `host` (`input_packets`, `host_command`), `agent` (`agent_task`).
- The service never pushes events on a connection that did not subscribe; the manager's
  `src-tauri` keeps one subscription connection and re-emits Tauri events.

## 6. Configuration and state

| What | Where | ACL / notes |
| --- | --- | --- |
| Config document (`seat_core::config::Config`, `schema_version` 2 since 2026-11-14 (§10.1), `revision`) | `%ProgramData%\Pulsar Seat\config.json` | SYSTEM + Administrators full, Users read; atomic replace; migrations in `Config::from_json` |
| Seat passwords | LSA private data `L$PulsarSeat.Seat.NN` | SYSTEM; write-only from the manager; never in files, logs, reports |
| Remote seats' Pulsar secrets (identity, connect password, relay key) | LSA private data `L$PulsarSeat.Remote.<seat-id>.<slot>` (§10.3) | SYSTEM; only `pulsar-host`'s per-launch pipe gets them |
| Run state (crash counter, last boot, `KnownSession`s, last user SID per seat, failing passwords) | `%ProgramData%\Pulsar Seat\run\` | SYSTEM + Administrators only |
| Undo journal (every system change with its previous value, incl. "absent") | `%ProgramData%\Pulsar Seat\journal\changes.jsonl` | SYSTEM + Administrators only |
| Logs | `…\logs\service.log`, `host-NN.log`, `agent-<session>.log` | size-capped; errors also to Event Log source `PulsarSeat` |
| Diagnostic reports | `…\diag\` | no secrets, user names masked by default |
| Install facts | `HKLM\SOFTWARE\Pulsar Seat` (`InstallPath`, `Version`) | installer only; no settings in the registry |
| Driver parameters | `HKLM\SYSTEM\CurrentControlSet\Services\<driver>\Parameters` | only if a driver needs one; normally from the service via IOCTL |
| Per-device seat for filters | DEVPROPKEY `{0527596e-0b5b-4dcf-88b3-118399aa27b1}` pid 2 (UINT32 mask), pid 3 (UINT64 revision) | written by the service |
| UI preferences | manager's per-user app data / `localStorage` | never machine-wide |

Run-time state lives in the service's memory (`ProductState`, `SeatRuntime` with
`SeatState::{off, waiting_for_display, starting{phase}, at_logon, signed_in, locked,
stopping, error{reason}}`) and is served over the pipe; nothing in volatile registry keys.

## 7. Boot and seat life cycle (service)

1. Start (automatic, LocalSystem, depends on RpcSs + LSM). Windows Safe Mode →
   `safe_mode{windows_safe_mode}`, serve status only. Unclean previous shutdown (guard on) →
   `unclean_shutdown`; crash counter ≥ `crash_loop_limit` → `crash_loop`.
2. Wait for PlugPlay, AudioEndpointBuilder / AudioSrv (bridges only), TermService (RDP
   backends) through SCM / WTS notifications, never fixed sleeps.
3. Probe backends, build the inventory (`enumerate` of every backend), compute effective
   masks, apply input routes, gate plan and console topology.
4. `adopt` known sessions; then start seats per `start_mode` with `between_seats_ms`
   pauses and per-seat `seat_start_timeout_s`. A seat without a present monitor waits in
   `waiting_for_display`. One seat's failure never blocks the others.
5. Seat start (RDP backends): `bind_seat` → `SessionBackend::start` → launch host in
   `HostLaunch.target_session` with `--seat N --token-stdin` → host `hello` →
   `host_get_assignment` → `host_take_credentials` → `wait_session` → `audio.bind_seat` →
   gate re-plan with the new logon SID → agent launched in the seat session.
6. Sign-out with auto-logon → re-logon after `relogon_delay_ms`; logon failure marks the
   password failing (no retry loop); stop / pre-shutdown stop seats one by one.

## 8. ARM64 notes

- Every crate builds for `aarch64-pc-windows-msvc` and `x86_64-pc-windows-msvc`; CI checks
  PE machine 0xAA64 / 0x8664 of every artifact. No x64-only dependency.
- Windows Server + RDS is not generally available on ARM64 (HYPOTHESIS): there
  `rds_loopback` probes `unavailable{edition_unsupported}` and the service falls back to
  `fast_user_switch`; `desktops` and the experimental `termsrv_wrapper` (only if the
  operator's wrapper is a native ARM64 DLL — the gate checks the PE machine) are the other
  options.
- ARM64 PCs: one integrated GPU, I2C/SPI HID (no USB parent → ACPI location anchors), modern
  standby (test resume / display-on notifications), HVCI on by default — one more reason
  for user-mode defaults.
- Credential provider (`credprov/`, 2026-11-13): one native DLL per installer architecture
  (ARM64 LogonUI is native). No ARM64X: the provider refuses `CPUS_CREDUI`, so emulated x64
  apps never need it (HYPOTHESIS: they skip the unloadable ARM64 DLL).

## 9. Build and verify

```
set CARGO_TARGET_DIR=target\agent-<label>        (parallel agents)
bun install && bun run check && bun run test && bun run build   (root: both managers)
cargo build && cargo test --workspace             (aarch64; --workspace includes both apps,
                                                  which need their frontend build first)
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-pc-windows-msvc
cargo build --release                             then check PE machine 0xAA64 / 0x8664
bun test installer/scripts
cd shim && cargo test --target x86_64-pc-windows-msvc   (x64 + x86 only; x64 emulation on ARM64)
```

seat-service builds seat-session with `experimental-client-sku` (a default feature of
seat-service): the termsrv wrapper module is compiled in but stays off at run time until the
admin acknowledged the current warning version. `--no-default-features` leaves it out.

`Cargo.lock` was seeded from pulsar-split so offline resolution uses cached crates; at merge
step 2-3 Split's locked registry entries were unioned in.

### Wizard and manager additions (2026-11-13, wizard-manager)

- **Service wizard** (`seat-service::wizard`): steps pick seat 1's monitor (digit key on any
  keyboard or a page click; skipped with one monitor) → F<N> twice per seat (all pages at
  once) or any key twice for the seat next in line → two clicks per mouse (a page's
  `target_hit` within 1.5 s steers the click to its seat; without it the lowest seat with a
  keyboard but no mouse) → Enter / Backspace / Shift+Enter / Shift+Backspace / Shift+Esc /
  Alt+F4 from any unit (modifier = its make within 3 s before, no key between; HYPOTHESIS,
  units report make codes only). Before the pick Enter / Backspace do nothing (and the
  `confirm_seat` command is refused, `monitor_not_picked`); a unit with no seat of its own
  confirms only a seat that already has keyboard + mouse and resets nothing. `target_hit`
  outside the captured seats is refused (`seat_out_of_range`). All seats confirmed = save.
  The result clears mask bits of seats that no longer exist (a device left on no seat goes
  to seat 1). Result: monitor audio /
  cameras by container id, the monitor's own USB hub (same container id) and what sits on
  it join the monitor's seat unless a unit placed by hand on another seat sits there,
  built-in units stay on seat 1. Preconditions: a monitor, primary monitor ≥ 1024×600
  (`wizard::MIN_WIDTH/MIN_HEIGHT`, same numbers in `apps/manager/src/lib/wizard.ts`).
  Number overlays on every monitor through `display.identify`, up until hidden
  (`IDENTIFY_UNTIL_HIDDEN`; spec 10 §3.5.1, manual indication too). Wizard work from input
  events (overlays, progress, save / quit) goes through one ordered queue drained by a
  single worker, never on the input thread, so progress never arrives out of order.
  `wizard_start` / `wizard_stop` are serialised by `Service::wizard_op`; start turns capture
  on before the wizard is stored. A save validates and commits **before** the wizard leaves
  the state: on errors the wizard keeps running and a progress with step `invalid` and
  `issues` (issue codes; `store_failed` for a write error) is published. The activity trace
  log never contains scan codes.
- **Manager pages**: `src-tauri/src/wizard_pages.rs` opens one borderless top-most WebView
  window per monitor (`wizard-page-<generation>-<n>`, new generation per open; same
  WebView2 data folder as the main window; built from async commands only, a sync command
  would deadlock WebView2 on the main thread; desktop rect in physical px); the page asks
  `wizard_page_monitor` / `wizard_last_progress` and follows `service-event`. Keys pressed in
  our own windows are mapped only while `keys_seen` is false (no double handling).
- **Config** (seat-core, additive): `seats[].ip_binding{address, rule}`,
  `settings.ip_programs[]{title, path, enabled}` (shared list, ≤ `MAX_IP_PROGRAMS` = 256),
  `settings.log{info, debug, trace}` (service `log::set_levels`; `--debug` still forces
  debug), `settings.outputs{force_assign_at_start, allow_clone}`, `devices[].options.outputs`
  (CCD target ids; more than one = clone). New issue codes `invalid_ip_address`,
  `duplicate_ip_address`, `too_many_ip_programs`, `invalid_ip_program`, `clone_not_allowed`.
  The display binding applies `options.outputs` at every seat start (`SeatDisplayPrefs`,
  clones only with `allow_clone`); with `force_assign_at_start` the service applies the
  console topology plan again at every seat start before `bind_seat`
  (`Service::force_assign_outputs`, 2026-11-13 final-fix; untested on hardware).

- **Manager-local commands** (no service): `list_ipv4_addresses` (IP Helper; static =
  prefix origin manual), `list_user_pictures{category}` (`%ProgramData%\Pulsar Seat\pictures\<category>`,
  data URLs, ≤ 512 KB, ≤ 64 files; the folder inherits the data dir ACL: Administrators
  write, Users read), `pick_program` / `pick_program_folder` (IFileOpenDialog on the common
  Programs menu, `.lnk` targets via IShellLinkW, folder = its `.exe` files + shortcut
  targets, ≤ 200; untested on hardware), `set_ui_dirty` + `close-requested` / `hide_to_tray`
  (closing with unsaved changes asks save / discard / cancel), `wizard-page-close` (Alt+F4
  on a page quits the wizard). Pictures: `options.picture` = `builtin:<id>` (our own line
  drawings) or `user:<file>`; `name_off.ext` is the "not in use" picture. Webcam indication
  = WebView `getUserMedia` preview in the manager.

## 10. Seat kinds (merge contract delta, steps 1 + 4, 2026-11-14)

Joint contract for the merge with Pulsar Split ([merge-plan.md](merge-plan.md), maintainer
decisions §5). Step 4 put it into `seat-core` / `seat-proto`; the remote seat worker that
serves it is in `seat-service` since step 6 (§10.5). Split's old contract stays in
[../split/agents/contracts.md](../split/agents/contracts.md) for the packages.

### 10.1 Config document (`seat_core::config`, `CONFIG_SCHEMA_VERSION` = 2)

```text
SeatConfig {
  number, name, autostart, kind: local | remote,
  account,                                   // the seat user; password in the SecretStore
  cpu_sets, cpu_hard_limit, wallpaper, ip_binding, startup_apps,   // common
  local:  Option<LocalSeat>,                 // set iff kind == local
  remote: Option<RemoteSeat>,                // set iff kind == remote
}
LocalSeat  { redirect, kiosk, kiosk_shell }  // physical seat only (seat host, desktops backend)
RemoteSeat { id, display_name, port, stop_action, display, apps, rdp_client_name, audio,
             stream, stream_backend, last_mode }   // Split's per-seat record minus name / user
GlobalSettings.remote: RemoteSettings        // Split's old GlobalSettings (hid_isolation, ...)
```

- Migration 1 -> 2 (`Config::from_json`): every seat without `kind` becomes `kind = local`
  and its `redirect`, `kiosk`, `kiosk_shell` move into `local`. Split's registry config is
  **not** imported (decision §5: no migration of installs, no HKLM / DPAPI importer).
- Mapping from Split's record (`seat_core::remote::RemoteSeatRecord`, the old
  `split_proto::SeatConfig`): `name` = `SeatConfig.name`, `user_name` = the user of a
  `local` `account`, `start_with_service` = `autostart`; every other field is in
  `RemoteSeat`. `RemoteSeat::to_record` / `RemoteSeat::from_record` convert both ways (the
  Split controller works on records through `crate::remote::DocStore`, §10.5).
- Common fields (`cpu_sets`, `cpu_hard_limit`, `wallpaper`, `ip_binding`, `startup_apps`)
  are stored for both kinds; the remote worker ignores them for remote seats so far
  (step 6 kept Split's behaviour). `startup_apps` and `RemoteSeat.apps` overlap: remote
  seats use `RemoteSeat.apps` (Split's autostart), `startup_apps` stays local-only until
  the manager merge (step 9) folds the two editors together.
- Validation (`seat_core::validate`): exactly the kind's sub-struct is set
  (`seat_kind_mismatch`); seat 1 is always local (`console_seat_remote`); no device
  assignment names a remote seat (`device_on_remote_seat`); every remote seat passes Split's
  checks (`seat_core::remote::validate::seat_against`, issue `remote_seat_invalid` with the
  Split error code in `reason` and the Split field in `field`); a remote seat while the
  session backend has no concurrent sessions (`fast_user_switch`, `desktops`) is a warning
  `session_unavailable`. Device-readiness warnings skip remote seats.
- Ports: only remote seats reserve a port block (`seat_core::remote::ports`, base
  `48100 + k*100`, block `p-5 ..= p+21`). `seat_core::remote::assign_resources` fills a
  remote seat's missing `id`, `rdp_client_name` (`PSS` + 8 hex) and `port` (0 = allocate);
  local seats get none.

### 10.2 Protocol 3 (`seat_proto`, `PROTOCOL_VERSION` = 3, `MIN_PROTOCOL_VERSION` = 1)

One control pipe (`\\.\pipe\PulsarSeat.Control`), `hello` first, Seat's `Access` table.
Clients below 3 keep every request whose shape did not change (seat hosts, agents, the
credential provider, the console listener). `RequestBody::min_protocol()` is 3 for the
config requests (the seat shape changed) and for every new request; the service answers
`version_mismatch` when the caller's hello version is lower. `get_status` carries `kind`
per seat (`SeatRuntime.kind`).

Seat v1 requests are unchanged (§5). Split v2 requests, as v3 (all `Access::Admin`; remote
seat status is admin only, decision §5):

| v3 request | Split v2 | Seat kind |
| --- | --- | --- |
| merged into `hello`, `get_status`, `poll_events`, `list_local_users`, `request_reboot` | `hello`, `get_status`, `poll_events`, `list_local_users`, `reboot_host` | - |
| merged into `get_config` / `apply_config` (whole document) | `list_seats`, `get_seat`, `create_seat`, `update_seat`, `delete_seat`, `get_global_settings`, `set_global_settings` | - |
| merged into `set_seat_password{seat}`, `start_seats`, `stop_seat` | `set_seat_password`, `start_seat`, `stop_seat` | any |
| `test_seat_credentials{seat}`, `enable_unlock_account{seat, enable, unlock, confirm}` | same | any |
| `get_remote_runtime{seat}` | `get_seat_runtime` | remote |
| `submit_pin{seat, pin, client_name, request_id?}`, `get_pair_state{seat}`, `cancel_pair{seat}`, `list_paired_clients{seat}`, `unpair_client{seat, uuid}`, `unpair_all{seat}`, `regenerate_seat_identity{seat, confirm}`, `get_apps{seat}`, `set_apps{seat, apps}`, `answer_peer_request{seat, request_id, allow, view_only}`, `set_seat_connect_password{seat, password}`, `get_seat_otp{seat}`, `rotate_seat_otp{seat}`, `answer_relay_auth{seat, password, totp?}` | same, `name` -> `seat` | remote |
| `get_host_info`, `set_multi_session{enabled}`, `get_termwrap_status`, `set_termwrap{...}`, `list_render_adapters`, `get_pad_backends`, `get_shim_status`, `set_shim_targets{targets}`, `set_app_masking{exe_path, enabled}`, `set_dxvk{game_exe, enabled, dlls}`, `get_dxvk_status`, `get_log_tail{source, lines}`, `get_stream_backends`, `get_user_picture{user}`, `cleanup_audio_endpoints{confirm}` | same (`get_isolation_status` -> `get_shim_status`) | - |

New responses: `host_info`, `termwrap_status`, `remote_runtime`, `render_adapters`,
`pad_backends`, `pair_state`, `paired_clients`, `apps`, `shim_status`, `credential_test`,
`log_lines`, `stream_backends`, `otp`, `dxvk_status`, `user_picture`, `audio_cleanup`
(payload types in `seat_proto::remote`).

`RequestBody::seat_kind()` names the seat and kind a request needs; Seat's local-only
requests (`relogon_seat`, `host_*`, `credprov_switch`, `agent_switch`) need `local`. On the
other kind the service answers **`wrong_seat_kind`**. A remote seat start without a session
backend that has concurrent sessions (Server + RDSH, or the wrapper kind) fails with
**`session_unavailable`**; U1 (`fast_user_switch`) / U3 (`desktops`) are local only.
Since step 6 the remote seat worker answers them (§10.5). A service without a worker
(tests only; production always builds one) answers `unsupported`, reason
`remote_worker_unavailable`.

Errors: one `ErrorCode` (`seat_core::error`, re-exported by `seat_proto`) = Seat's codes,
plus Split's (`invalid_name`, `duplicate_name`, `invalid_display_name`, `invalid_port`,
`port_overlap`, `no_free_port`, `invalid_user`, `user_in_use`, `missing_credentials`,
`edition_unsupported`, `seat_failed`, `driver_missing`, `no_pending_request`, `wrong_pin`,
`hash_mismatch`, `firewall`), plus `wrong_seat_kind` and `session_unavailable`. `ProtoError`
keeps Seat's shape (`code, message, field?, reason?, win32?`).

Events (topic mapping, `seat_proto::remote::map_legacy_event`):

| Split v2 event | v3 |
| --- | --- |
| `seat_phase`, `seat_failed` | `seat_state{runtime}` (topic `status`; the caller supplies the runtime) |
| `host_warning` | `notice{code: "host_warning", seat, detail}` (topic `status`) |
| `pair_pending`, `pair_ended`, `stream_started`, `stream_ended`, `otp_changed`, `trust_changed`, `relay_auth_required` | `stream{seat, event: {type, data}}` (new topic **`stream`**) |

`poll_events` stays for simple clients.

Admin-only filter (merge review, 2026-11-14): a non-admin subscriber or poller never gets
`stream` events, nor `seat_state` / `notice` events of a remote seat (or a `host_warning`),
whatever topics it named; empty topics mean "every topic this caller may see"
(`events::Scope::admin`, the hub keeps the configured remote seat numbers). Naming the
`stream` topic explicitly is still refused with `access_denied`.

Deleting a remote seat with `apply_config` (Split's `delete_seat`): refused with
`seat_running` while the seat boots, runs or stops; after the save the worker's
`forget_seat` removes its stream backend secrets (LSA identity, connect password, relay
key), seat folder, logs and firewall rules (honouring `keep_pairing_on_delete`). Uninstall
deletes the LSA remote secrets of every remote seat in the config and every `seats\<id>`
folder (`remote::uninstall_remote_seat_ids`). LSA has no documented enumeration of private
data by prefix, so a secret whose seat is in neither place is not found.

Transitional: `seat_proto::remote::legacy` keeps Split's v2 request and response enums
(stamped with `PROTOCOL_VERSION` 3). Since step 6 no pipe serves them: only the remote seat
worker uses them internally (`crate::remote::to_legacy` / `from_legacy`). The manager speaks
protocol 3 since step 9 (§10.7). Split's `Controller` / `Platform` live in `crates/seat-service/src/remote/control.rs`.
Split's `ConfigStore` for records is `seat_core::remote::store` (the service implements it
over the config document; `MemoryStore` / `FileStore` stay for tests).

### 10.3 Secrets (`SecretStore`, step 5, done 2026-11-14)

One trait in `seat_core::store`, LSA private data for every secret (decision §5). No DPAPI and
no importer for old Pulsar Split installs (decision §5: no migration); Split's DPAPI code and
its `HKLM\SOFTWARE\PulsarSplit\Secrets` store were deleted at step 6.

```rust
pub trait SecretStore: Send + Sync {
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError>;
    fn has(&self, seat: SeatNumber) -> bool;
    fn get(&self, seat: SeatNumber) -> Result<Option<Secret>, StoreError>;
    // Remote seats' Pulsar secrets, keyed by `RemoteSeat::id` (GUID, survives renumbering).
    fn set_remote(&self, seat_id: &str, slot: RemoteSecretSlot, value: Option<&Secret>) -> Result<(), StoreError>;
    fn get_remote(&self, seat_id: &str, slot: RemoteSecretSlot) -> Result<Option<Secret>, StoreError>;
}
pub enum RemoteSecretSlot { Identity, ConnectPassword, RelayKey }
```

| Secret | LSA name | Size |
| --- | --- | --- |
| seat password (both kinds) | `L$PulsarSeat.Seat.NN` | <= 256 characters (`MAX_PASSWORD_CHARS`) |
| Pulsar identity | `L$PulsarSeat.Remote.<seat-id>.identity` | 44 characters (32 random bytes, base64) |
| connect password | `L$PulsarSeat.Remote.<seat-id>.connect_password` | <= 128 characters |
| relay access key | `L$PulsarSeat.Remote.<seat-id>.relay_key` | issued by the relay; short (HYPOTHESIS: < 256 characters) |

`remote_secret_name` lower-cases the id and accepts only 1-64 ASCII hex digits and `-`, so a
name can never leave the `L$PulsarSeat.Remote.` prefix. Implementations: `LsaSecretStore`
(`seat-service/src/win/lsa.rs`), `MemorySecretStore` (tests, `--console`; memory only). The
stream package reaches the slots through `crate::remote::StorePulsarSecrets` (its own
`PulsarSecretStore` trait).

LSA limits (checked 2026-11-14 against Microsoft's `LsaStorePrivateData` and "Private Data
Object" pages): a value is an `LSA_UNICODE_STRING`, so at most 65 534 bytes (32 767 UTF-16
units); Microsoft says only that "a limited number" of private data objects exist per system
(no figure; HYPOTHESIS from older documentation: a few thousand). Pulsar Seat uses at most
one password per seat plus three slots per remote seat (16 seats: fewer than 64 objects) and
every value is short, so no value needs the "encrypted file in the protected data dir, key in
LSA" fallback. Every store refuses values over `MAX_SECRET_CHARS` (4096 UTF-16 units) instead;
if a future secret is bigger, add that fallback then. Untested on hardware: LSA needs SYSTEM.

### 10.4 Sessions for remote seats (`SessionBackend`, step 7, done 2026-11-14)

Remote seats use the same session path as local RDP seats: `rds_loopback` (Server + RDSH)
or the experimental `termsrv_wrapper` kind (off by default;
`SessionBackendKind::hosts_remote_seats()`). One crate, `seat-session`:

- `seat_session::remote` = Split's session manager (seat users, start / stop / tick,
  `PSS` + 8 hex client names, WTS discovery by name + user + loopback, launch into the
  seat session with job `PulsarSeat.RemoteSeat.<seat-id>`, run state
  `run\remote-sessions.json`, sweeps). The remote seat worker in `seat-service` drives it
  through its `SessionManager` trait (the per-seat start / stop is not the local
  `SessionBackend::start` because a remote seat has no seat host, monitors or token).
- Shared with the local backends, one implementation each: edition probe and
  `HostClass` (wire name of `Client` stays `client_multi_session_unavailable`), the real
  probes (`win::probes`: WMI `TerminalServerMode` first, `TSAppCompat` fallback; listener
  TCP table + certificate thumbprint; licensing grace days), the registry trait, the WTS
  session model and both client-name prefixes (`wts::client_name_kind`), command-line and
  environment helpers.
- **One Terminal Server settings writer** (`seat_session::rds::RdsApplier`) and **one
  journal** (`journal\session.jsonl`, package `session`). Each kind claims it
  (`Claim::Local` from `prepare_host`, `Claim::Remote` from the remote worker's
  `prepare_host`); the rows written are the union of the current claims (`rows_for`);
  releasing one kind restores only the rows no other claim needs. Claims are per process
  (`TsClaims::process()`), the journal outlives them, so an uninstall restores everything.
  Created seat accounts are journaled there too (kind `account`, never replayed).
  Rows: common `fDenyTSConnections=0`, `RDP-Tcp\UserAuthentication=1`, `RDP-Tcp\Shadow=0`;
  local `fSingleSessionPerUser=0`; remote `SetGpuRealtimePriority=1` (optional),
  `RDP-Tcp\SecurityLayer=2`, `IgnoreRemoteKeyboardLayout=1`, policy
  `bEnumerateHWBeforeSW=1` / `Shadow=0` (only while absent). Split's `Originals` key is gone.
- **Fixed order** of the three Terminal Server writers: (1) these rows, (2) the second NLA-off
  listener `PulsarSeat-Loopback` (local passwordless seats; sets its own
  `UserAuthentication` / `SecurityLayer` / `Shadow`), (3) the termsrv wrapper (owns
  `ServiceDll` and the two wrapper rows `EnableConcurrentSessions` /
  `AllowMultipleTSSessions`, own journal `termwrap.jsonl`; the RDS writer no longer writes
  them). Restore runs in reverse; the setup `uninstall` hooks run termwrap before session.
- **Headless hold**: `pulsar-seat-host.exe --headless-hold <args>` (`seat_rdp::headless`;
  IronRDP without a frame sink) replaces `pulsar-split-rdp-helper.exe`. Protocol chosen:
  **Split's helper contract**, unchanged (argv `--protocol 1 …` after the sub-command,
  one stdin password frame then EOF, JSON Lines `HelperEvent`s on stdout, exit 0 / 1 / 2),
  not Seat's `host_report` pipe: a hold has no seat pipe, token or monitors. The service
  still starts it with a restricted Low-integrity token in a kill-on-close job; the
  seat host dispatches the sub-command before its containment checks.
- **Sweep**: `plan_sweep_for(kind, …)` recognises both prefixes. The remote worker sweeps
  `PSS…` sessions of remote seat users (or recorded id + SID) and never a `PSEAT-NN`
  session, also when a stale record names its id. The local kind keeps `adopt` and does
  not sweep. The local newest-session fallback never takes a `PSS…` session.
- **termsrv wrapper**: one module, `seat_session::termwrap` (Seat's port of
  `split-termwrap`; the two test suites were the same tests). Always compiled (status,
  disable, boot gate, uninstall restore work in every build); `enable` needs the cargo
  feature `experimental-client-sku` (`TermwrapPolicy::enable_built_in`, a default of
  `seat-service`), an **x64 host** (ARM64 is refused, `wrapper_arch`) and the warning ack
  (`EXPERIMENTAL_WARNING_VERSION` = 2 for both kinds; Split's version-1 text is gone).

The `pulsar-host` pipe contract
([../split/agents/pulsar-host-backend.md](../split/agents/pulsar-host-backend.md)) is
unchanged; its `PulsarSplit\seat\<id>` name prefix stays until pulsar-desktop changes with it.

### 10.5 One service: the remote seat worker (step 6, done 2026-11-14)

`pulsar-seat-service` is the only service (SCM name `PulsarSeat`, pipe
`\\.\pipe\PulsarSeat.Control`, data dir `%ProgramData%\Pulsar Seat`, ACLs unchanged).
`crates/split-service` and `pulsar-split-service.exe` are gone. Pulsar Split's controller and
engine are the remote seat worker in `crates/seat-service/src/remote/`:

| Module | What |
| --- | --- |
| `remote/control.rs` | Split's `Controller` + `Platform` trait (v2 enums inside), tests with `FakePlatform` |
| `remote/engine/` | Split's seat engine (session, display, audio, stream, pads, isolation), tests with the packages' fakes |
| `remote/mod.rs` | `RemoteWorker` trait (implemented by every `Controller`), `RemoteHost` (engine boot / stop hooks), `DocStore`, `merge_worker_fields`, `StorePulsarSecrets`, `to_legacy` / `from_legacy`, `runtime_of`, log tail + redaction |
| `remote/glue.rs` | the `Service` side: requests, starts / stops, boot / stop / power hooks, events |
| `remote/win/` | Windows `Platform`, package wiring, shim health pipe, user pictures, `remote-setup` sub-commands |

- **Requests.** Every protocol 3 remote request (`is_remote_kind_request`) goes, after the
  access table and the `wrong_seat_kind` check, to the worker: the seat number becomes the
  seat's name, `get_shim_status` becomes `get_isolation_status`, `get_log_tail`'s
  `seat:<N>` becomes `seat:<name>` (not a remote seat: `invalid_value`, field `source`).
  `test_seat_credentials` / `enable_unlock_account` on a **local** seat use the worker's
  account code with the local seat's user and stored password.
- **Starts and stops.** `start_seats` with a list hands its remote seats to the worker
  (`session_unavailable` without concurrent sessions; the worker's first refusal, e.g.
  `missing_credentials`, is the answer) and starts the local ones as before. `start_seats`
  without a list and the boot autostart (`start_autostart_seats`) also start the remote
  seats with `autostart` (product enabled, no safe mode, `start_mode` not `manual`, a session
  backend with concurrent sessions). `stop_seat` on a remote seat goes to the worker.
  Remote seat starts are asynchronous (Split v2): progress arrives as `seat_state` events.
- **Config.** The worker reads the config document through `DocStore` (Split's record store:
  remote seats only, `settings.remote` as Split's global settings, passwords from the
  `SecretStore` by seat number). Creating, renaming and deleting remote seats is
  `apply_config` only (the store refuses them). The worker writes back only what it owns:
  `rdp_client_name`, `last_mode` and `settings.remote.multi_session`; the service merges
  those (`merge_worker_fields`) and saves a new revision. `settings.remote.start_with_windows`
  no longer changes the service start type (one service, always automatic).
- **Status and events.** After every worker call the service reads the worker's status into
  `State.remote_rt` (`runtime_of`: stopped = `off`, booting = `starting{phase}`, running =
  `signed_in{user}`, failed = `error{reason, detail}` with Split's failure code mapped to a
  `FailureReason` when the names match, else `internal`; a streaming seat carries
  `degraded: ["streaming"]`). `get_status` shows these runtimes (admin only, decision §5).
  Worker events map with `map_legacy_event` onto the event hub (`seat_state`, `notice`,
  `stream`).
- **Boot and stop.** Boot (product enabled): `RemoteHost::service_start` (Split's
  coexistence check, termwrap boot check, RDS prepare, `PSS…` session sweep) before any
  autostart. Ordered stop: local seats, then the worker stops every remote seat, waits up to
  90 s until none is active, then `RemoteHost::service_stop` (final sweep).
- **Power and sessions (ported from Split into Seat's SCM skeleton, `win/scm.rs`).**
  `PBT_APMSUSPEND` stops the remote seats (`Service::on_suspend`; local seats keep their
  sessions). `PBT_APMRESUMEAUTOMATIC` re-checks devices (local) and, once the old remote
  seats finished stopping, starts the remote autostart seats again; `PBT_APMRESUMESUSPEND`
  is ignored (no double start). Every `SERVICE_CONTROL_SESSIONCHANGE` goes to the local
  seats (mapped reasons) and to the engine (raw reason, `SessionChange::from_raw`), in
  arrival order on one worker thread.
- **Runtime threads (`runtime.rs`).** Seat's 500 ms tick (which drains the worker's reports),
  the engine's own 1 s tick, the 4 ms pad pump (when pads exist) and the shim health pipe.
- **Pipes.** One control pipe (Seat's, ACL unchanged). The shim health pipe
  `\\.\pipe\PulsarSplit.ShimHealth` is served by `pulsar-seat-service` (name kept: the
  `shim/` workspace writes to it; DACL SYSTEM full, Authenticated Users write-only). The
  `pulsar-host` per-seat pipe (`\\.\pipe\PulsarSplit\seat\<id>\pulsar-host`) is unchanged
  ([../split/agents/pulsar-host-backend.md](../split/agents/pulsar-host-backend.md)).
- **Data dir.** The engine uses Seat's data dir: `seats\<seat-id>\` (seat-stream's own
  DACL), `run\sessions.json` next to Seat's `run\state.json`, `journal\<package>.jsonl` next
  to `journal\changes.jsonl` (both folders SYSTEM + Administrators only), `logs\service.log`
  is the one service log. The pad MAC salt is `PadHostSalt` under `HKLM\SOFTWARE\Pulsar
  Seat`. Package registry keys under `HKLM\SOFTWARE\PulsarSplit\{Shim,Termwrap,Runtime}`
  are unchanged until their packages merge (steps 7-8).
- **Setup.** Split's setup sub-commands are `pulsar-seat-service remote-setup install |
  reconcile | uninstall | check-coexistence [...]` (`seat_setup::cli`, restore hooks for
  session, shim, stream, pads, display, termwrap). Its old `config` reconcile hook is gone
  (the config document migrates itself). The one MSI (step 10) calls them.
- Untested on hardware: everything above that touches Windows (nothing of it ran on a real
  machine; the remote kind never did).

### 10.6 Audio, pads, display (step 8, done 2026-11-14)

- **Audio.** One crate for endpoint discovery: `seat-audio`. `seat_audio::cfgmgr`
  (Windows) holds the only copy of the CfgMgr32 read primitives. `seat_media::pnp::WinPnpTree`
  and the remote kind's `CfgMgrNodes` both use them. The endpoint-id model
  (`MMDEVAPI_PREFIX`, `mmdevapi_instance_id`, `parse_endpoint_id`, `is_session_endpoint_id`)
  is shared too: `seat_media::audio` uses it to skip other sessions' endpoints. Routing stays
  per kind. Local seats use `rdp_bridge` (`seat-media`, physical endpoints by MMDevice id).
  Remote seats use the session's Remote Audio endpoint, read by the stream server.
- **Pads + shim.** `seat-pads` and `seat-shim-core` are the shared crates. Local seats get
  seat-owned virtual pads through `seat_input::local_pads` (cargo feature `local-pads`, on in
  `seat-service` since round 4, 2026-11-14). Per seat the switch is
  `LocalSeat::virtual_pads` (config + manager "Sanal oyun kolu"), **default off**: every
  usable backend needs a driver install (ViGEmBus or usbip-win2) that Pulsar Seat never
  does, so the feature cannot be default-on. Service wiring: `crate::local_pads::targets`
  (seats with the switch on, a session and a signed-in user; HID pads whose effective mask
  or pending hotkey move names exactly that seat) feeds `LocalPadHub::sync` on the 1 s
  runtime tick. The hub runs on the remote engine's one `PadManager`
  (`Engine::local_pad_parts`), so both kinds share the ownership registry, the orphan
  cleanup at service start / stop and the securing pass (device SDDL with the seat user's
  SID + `DEVPKEY_Device_SessionId`); local seats use fixed pad seat ids
  `5053454c-4f43-4100-8000-<seat>`. One reader thread per physical pad
  (`win::pad_io::WinPadSource`, shared HID open from session 0) forwards reports
  (`forward_physical`: raw when the wire kind equals the physical kind, else parsed into a
  state). Feedback: `LocalFeedbackRouter` is the `PadManager`'s sink; same kind = the
  game's output report unchanged (adaptive triggers included), other kind = rumble +
  lightbar rebuilt into the physical pad's report; Bluetooth pads get no feedback yet
  (CRC). Only PlayStation pads are read; Xbox / generic HID pads are reported
  `Unsupported`. The hub shuts down first in `stop_with`. Limits: ViGEm only if already
  installed; DualSense via usbip-win2; the own pad driver is unsigned and only used on
  explicit preference. The physical pad stays visible to every session without the
  (unsigned) HID filter. HYPOTHESIS: every Windows path behind the hub (nothing ran on
  hardware). Details: `crates/seat-pads/README.md`. The
  per-process shim (`shim/`, x64 / x86) stays remote-only. Its registry key
  `HKLM\SOFTWARE\PulsarSplit\Shim` and its health pipe keep their names, because `shim/`
  depends on them.
- **Display.** `split-display` is now `seat_display::remote`; the crate is gone. Each kind
  keeps its operation trait: `DisplayBinding` for local, `remote::DisplayBackend` for remote.
  `seat_display::SeatDisplayBackend` is the one trait over both. It has `backend_id()`, a
  `DisplayBackendId` serialised as `{"kind", "backend"}` (per-kind wire names unchanged),
  and `backend_status()`, a `DisplayBackendStatus` (`available`, `signed`, `hdr`,
  `reason`). Adapters: `Local`, `Remote`, `statuses_for`. Both kinds have a `pulsar_idd`
  (both now served by the one driver `drivers/seat-idd`). Round 4 (2026-11-14) merged the
  two C++ IddCx drivers into `drivers/seat-idd` with two roles, picked per devnode from
  the hardware id (`Root\PulsarSeatIdd` = local, `PulsarSplit_IndirectDisplay` = remote;
  `src/roles.cpp`), so it is signed once. Both control interfaces and both Rust clients
  stay as they were (`idd_proto` / `remote::ioctl`); the remote `SET_SEAT_CONFIG` gained
  an additive v2 buffer with the frame reader SID (`SeatDisplay::frame_reader_sid`, `None`
  = SYSTEM only, the stream server runs as SYSTEM). Details: `drivers/README.md`.

### 10.7 One manager (step 9, done 2026-11-14)

`apps/manager` is the one manager for both kinds. Pulsar Split's manager pages moved to
`apps/manager/src/lib/remote/` (with git history); `apps/split-manager` and its Tauri crate
`pulsar-split-manager` are gone.

- **Tabs.** The Seat shell (tray, wizard, subscription connection, save / discard bar) gains
  the tab **Uzak koltuklar** (`remote/components/RemoteTab.svelte`, Split's old main page):
  the remote seat list with avatars, the seat editor (`SeatPage`, GPU warnings, apps,
  credential test), the pair dialog with QR code, host page (multi-session, experimental
  wrapper, audio cleanup), logs, DXVK games and remote settings (isolation, process
  patching, pad / display backend). The seats tab shows each seat's kind: local seats get a
  `Yerel` badge on their device container, remote seats have no container (devices go to
  local seats only) and are listed under the canvas with an `Uzak` badge and an edit button.
- **Protocol.** Every remote request is a protocol 3 body over the one pipe
  (`remote/api.ts`, `service_request`, `hello` with protocol 3). Split's `list_seats` /
  `create_seat` / `update_seat` / `delete_seat` / `set_global_settings` became config edits:
  `ManagerStore.applyRemote` validates and applies them at once on top of the saved config
  (like the update opt-in) and merges the remote part into an unsaved draft, so local edits
  stay unsaved. Validation errors come back as Split's error codes (`remote_seat_invalid`
  issue `reason` + `field`, `remote/model.ts` `issueError`). Seat start / stop, passwords and
  reboot use the Seat requests (`start_seats`, `stop_seat`, `set_seat_password`,
  `request_reboot`).
- **Mapping** (`remote/model.ts`, tested in `model.test.ts`): Split's record of a seat is
  built from `SeatConfig` + `SeatConfig.remote` (`toRecord` / `applyRecord`, mirrors
  `RemoteSeat::to_record` / `RemoteSeatRecord::to_seat`; `id`, `rdp_client_name` and
  `last_mode` stay service-owned). The seat status of the pages comes from the protocol 3
  runtime (inverse of `seat_service::remote::runtime_of`), and the pair dialog's long poll
  is `poll_events` with topics `stream` + `status`, mapped back to Split's event kinds
  (inverse of `map_legacy_event`). The pair dialog polls; the subscription thread keeps its
  topics, because `stream` is admin only and would refuse a standard user's subscription.
- **Gone with Split's Tauri crate:** service start / stop / restart through the SCM (the one
  service belongs to the product; the remote settings page shows reachability only) and the
  `tauri-plugin-dialog` file picker (the pages use Seat's `pick_program`). Kept from it: the
  LAN address order for the pair dialog (`net::order_lan`, Tauri command `lan_addresses`).
- **Copy.** One `i18n.ts`: Split's copy is its `rt` object plus `remoteErrorMessage`,
  `remoteFailureText`, `remoteFieldErrorText`; the remote components import those under
  their old names. Split's styles are `remote/remote.css`, scoped to `.remote-ui`.
