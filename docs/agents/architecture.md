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
   `experimental-client-sku`, off by default, versioned warning (English + Turkish, `warning_{en,tr}.txt`)) points TermService
   at a wrapper **the operator installed**; that wrapper changes termsrv's code in memory at
   run time, and the module writes two registry values Microsoft does not document
   (`EnableConcurrentSessions`, `AllowMultipleTSSessions`). Pulsar ships no wrapper and no
   patch (plan.md decisions; spec 30 §6 needs the matching analyst note).
   *External wrapper mode (detect-only, 2026-11-24):* when another installed product has
   already pointed TermService at its own wrapper (`%ProgramFiles%\Duo\TermWrap.dll`,
   spec/remote 30 §2.4), the same module only *reads* ServiceDll, the DLL's version /
   Authenticode / PE machine / ACL and the two rows above, and the `termsrv_wrapper`
   backend uses that wrapper for seats. It never writes ServiceDll or the two rows for it,
   never restarts TermService for it and never restores it (§10.4).
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
| `apps/manager` | Tauri 2 + SvelteKit 3 + Svelte 5, `adapter-static` | manager | all screens of spec 10 (English default, Turkish locale: `src/lib/locales/`), tray, wizard UI; `src-tauri` = pipe client |
| `credprov/` | C++20 COM DLL (v145, SDK 10.0.28000), unsigned | credprov | credential provider + filter for `fast_user_switch` (auto-logon, switch link); registered only by the service — see [credprov/README.md](../../credprov/README.md) |
| `drivers/` | C (KMDF / FltMgr) + C++17 (IddCx), built unsigned with WDK 10.0.28000; loaded only test-signed in the test VM (2026-11-16) | drivers | `seat-input-filter` (keyboard / mouse + HID roles), `seat-idd`, `seat-storage-filter` — see [drivers/README.md](../../drivers/README.md) |
| `crates/seat-bootstrap` | bin `pulsar-seat-setup` | installer | native per-arch `setup.exe` (ARM64 / x64): embedded or sibling MSI of the PC's native arch, SHA-256 (+ Authenticode in release builds), validated msiexec command line — see [crates/seat-bootstrap/README.md](../../crates/seat-bootstrap/README.md) |
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
| `shim/` | separate Cargo workspace, x64 + x86 + ARM64 (Detours) | per-process HID shim | kept |
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
      exit-windows). **Console listener exception (maintainer decision 2026-11-15):** its
      window-station ACE also gets `WINSTA_EXITWINDOWS` (`--exit-windows`,
      `HostRequest.exit_windows`, `LaunchSpec.host_exit_windows`). Why: USER32
      initialisation of a process under the host account fails with
      `STATUS_DLL_INIT_FAILED` without it (bisected in the ARM64 VM: the clipboard,
      create-desktop and read-screen rights do not help, exit-windows does), so the
      listener always fell back to SYSTEM at Low integrity. Scope: only the listener's own
      logon-SID ACE, journaled like every grant and revoked when the listener is gone and at
      uninstall. **Accepted risk:** a process holding that ACE could log the console user
      off (`ExitWindowsEx(EWX_LOGOFF)`); shutdown / restart still need
      `SeShutdownPrivilege`, which the hardened token does not have. **Seat hosts
      (2026-11-24, gaming PC E2E bug 7; pending the maintainer's OK, untested on
      hardware):** until then seat hosts did not get it, and on the gaming PC every seat
      host start as `PulsarSeatHostNN` and then as the shared account exited 0xc0000142
      while the console listener ran under the same shared account with it; the host only
      ran on the third launch, as SYSTEM at Low integrity (about 7 s lost per seat start,
      containment lost for the run). Seat hosts now get the same ACE right
      (`LaunchSpec.host_exit_windows = true` in `launch_host`), with the same scope and
      accepted risk. `win::containment` logs the token of every host launch (`host token =
      account … window-station mask 0x…` or `host token = SYSTEM at Low integrity …`). The
      service also passes its PID in `PULSAR_SEAT_SERVICE_PID` to every process
      it launches: the pipe server check of a contained host / listener used the SCM,
      which refuses a Low-integrity batch token (`OpenSCManagerW` access denied), and host
      clients retry `ERROR_PIPE_BUSY` with `WaitNamedPipeW` (the listener starts together
      with the kiosk shell and met a busy pipe). Revoked (`--desktop-acl revoke`) once the host is gone: platform
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
      per-seat accounts (below), `3` = the shared account for every host (round-3
      behaviour), `1` = the older privilege-less System-integrity SYSTEM token, `2` = SYSTEM
      at Low integrity (round-2 token, no account); the service also falls back to `2` by
      itself when the shared account cannot be set up or a grant fails.
      **Per-seat accounts (round 4 local-gaps, 2026-11-14, untested on hardware):** each
      seat host runs as its own local account `PulsarSeatHostNN` (`seat_account_name`,
      password in LSA private data `L$PulsarSeat.HostAccount.NN`, same rights, flags and
      group rules as the shared account), created / maintained with a rotated password the
      first time the seat's host starts in a service run (`Containment::seat_account`,
      provisioner = `win::host_account::ensure_seat`). All host accounts are members of the
      local group `PulsarSeatHosts` (comment-marked, created at service start,
      `ensure_group`; a foreign group of that name turns per-seat accounts off); the control
      pipe admits the group with one ACE (plus the shared account's own ACE, kept for hosts
      adopted across the upgrade). `check_groups` allows `Users` + that group. Fallbacks:
      a seat whose account cannot be set up, cannot log on, or whose host exits 44 /
      `STATUS_DLL_INIT_FAILED` uses the shared account for the rest of the run (only that
      seat); the console listener always uses the shared account. Service start prunes the
      accounts of seats that no longer run a host (`host_account_seats`: local seats, seat
      1 only under `dedicated_host`): rights, `DeleteProfileW`, account, logon-screen entry,
      secret; uninstall removes every host account and the group (`remove_all`). Profile:
      when the seat redirects printers (`LaunchSpec::host_profile`) the seat account's
      profile is loaded (`LoadUserProfileW`, `PI_NOUI`) before the launch and unloaded when
      the host exits or the launch fails; a load failure is logged, the host runs without
      it. `pulsar_idd` frame sections of a seat grant that seat's account
      (`Platform::host_frame_reader_sid(seat)`). Migration from the shared account: nothing
      to convert (group created, shared account added to it, journal grants keyed by logon
      SID). HYPOTHESES: a batch token carries the local group SID so the pipe ACE admits
      it; the hardened token can load / unload its own profile from the service; a seat
      whose own account fails after `configure_seat` keeps that SID on its frame sections
      until its next start (the shared-account fallback host could not read them). Not seat 1's user (could
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
  other seats (the manager says so; opt-in `console_guard.seat_one_lock_off` blocks seat 1's
  lock while other seats run, see "Console lock guard"); with `raw_input_host` other seats' keys also reach seat 1's foreground
  window — the manager shows `input_not_isolated` until the signed filter exists.
- `dedicated_host` (opt-in): a dedicated host account owns the console and runs only seat
  hosts; every seat including seat 1 is an RDP session. Raw-input isolation is complete in
  user mode (the host session's foreground window is always ours, `RIDEV_NOLEGACY`). Needs
  the documented auto-logon of that host account (maintainer decision, spec 30 §6.2.1).
  The console account gets lock and screen saver off (`console_guard.host_lock_off`, default
  on) and the preflight asks for `sleep_off` ("Console lock guard").
  The opt-in `settings.backends.host_autologon` ("host console" preset, OSMS parity G6,
  2026-11-18) does that sign-in; see "Host console preset" at the end of this file.

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
| `termsrv_wrapper` | client SKUs with an operator-installed TermService wrapper (enabled by us, or another product's already active wrapper in detect-only external mode, §10.4) | **off; experimental**, needs `experimental_ack == EXPERIMENTAL_WARNING_VERSION` | same as above |
| `fast_user_switch` | everywhere, ARM64 too | runtime fallback | one active seat at a time |
| `desktops` | client SKUs, ARM64 | opt-in | kiosk apps on the seat's monitors in the console session (per-seat Windows users since 2026-11-17, one session) |

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

Update 2026-11-24 (gaming PC E2E bug 1, reconnect loop), **untested on hardware:** a new
connection of an account that already has a session first gets a transient session, and
Windows then reconnects the connection to the account's existing one. `wait_session` binds
the transient session (our `PSEAT-NN` name, user still empty) about 2 s before the host
reports the real id, so the seat ended with the transient session and restarted every
~11 s. Now `SessionBackend::rebind(seat, id)` (additive, default `Unsupported`;
`RdpCore::rebind`) moves the binding to the reported session when WTS confirms it with the
reported-id rules above; `Service::follow_reported_session` calls it from `on_host_report`
and at the end of `run_start`, `reported_instead` right after `wait_session`, and the seat
records the new id (run file included) and handles its logon. An unconfirmed report keeps
the bound session (warning). `Slot.reported_session` is cleared at each host launch. Also
(HYPOTHESIS: Windows blanks the client name and address of a **disconnected** session) the
bound session of a seat with an account counts as the seat's while disconnected, by its
bound id and the seat's user, never the console winstation: the host-exit log-off ends it
(with `require_remote` off) instead of leaving it for the next connection to reconnect to.
`log_off` refusals and failures are logged with the session's WTS view (state, remote flag,
client name, address, user).

Logon-screen seats (no stored password) on an NLA listener — **decision 2026-11-13
(fix-rdp-client), HYPOTHESIS until tested:** `rds.rs` keeps RDP-Tcp `UserAuthentication=1`
(NLA on) for password seats, so a TLS-only connection gets `HYBRID_REQUIRED_BY_SERVER` and
the host reports `Failed { nla_required }`. Chosen design: **(a) a second WinStation listener
bound to loopback only, NLA off, used only by passwordless seats** (no secret crosses it; the
pinned TLS certificate still authenticates the server; the RDP-Tcp listener stays NLA-only).
Rejected for now: (b) a seat-host-drawn credential prompt fed into CredSSP (a credential UI
running as SYSTEM on the seat's desktop, and a localized logon UI to maintain). Until (a) is
built (seat-session `rds.rs` + listener pins for the second listener + service selection),
`RdpCore::start` refuses a passwordless seat on an NLA listener up front (2026-11-13
final-fix): `Policy` / `nla_required` → `FailureReason::NlaRequired` with localized copy
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
- Accounts check (round 4 local-gaps): seat-core `validate` warns per logon-screen RDP
  seat with `loopback_no_accounts` when no RDP seat has an account and no
  `extra_accounts` entry is valid (the listener would be `unavailable:no_accounts` and the
  seat cannot start), and with `logon_screen_needs_listener` when the listener is off; the
  manager mirrors both (`loopbackAccountIssues`) and lists the admitted accounts under the
  extra-accounts field (`loopbackAllowedAccounts`). One parser:
  `seat_core::validate::parse_loopback_account`.
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
  `detail` gets `listener=loopback_listener:<state>[:<x>]` (manager shows per-state localized
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
  the stored password**, so the backend's `session_not_locked` refusal always applies, and
  the backend never calls `WTSConnectSessionW` without a password: a SYSTEM connect without
  one showed a locked session unlocked (VM test 2026-11-15), so the console goes to a fresh
  logon screen where the target user signs in and Windows reconnects the session; an
  off seat goes through `start_sequence_with(unattended)`, no grant). Seat 1 is a target
  only while `SessionBackend::console_seat_away()` names its session (the FUS backend
  remembers seat 1's console session + user SID as `home` when it first moves the console
  away, re-checks the SID, keeps it across `adopt`; `start(seat 1)` connects it back with
  the same lock rules).
- Agent switch (spec 35 §5.5(a)): `agent_switch_targets` / `agent_switch{seat}`
  (`Access::Agent`, own seat only, same checks as `credprov_switch`, FUS backend, no
  secure desktop). `pulsar-seat-agent` shows a notification-area icon with localized
  "N. koltuğa geç" items (`agent.rs` `win::tray`). Untested on hardware.
- DLL behaviour: README in `credprov/`. Inert outside LogonUI and for every scenario but
  logon / unlock; the filter hides others only with `hide_others`, only when the answer
  gives a seat tile (`SeatTileWillExist`, shared with the provider) and only if our provider
  is in the list; no service answer = no tiles, nothing hidden. `pending` → the provider
  polls (250 ms, at most 30 s) and calls `ICredentialProviderEvents::CredentialsChanged`
  when the answer changes; tiles are rebuilt in `GetCredentialCount`.

Selection at boot: the service probes the configured kind; if it is not `available` it
falls back to `fast_user_switch`, keeps the config unchanged and reports the reason in
`ServiceStatus.fallback_notes`. Experimental kinds are never chosen as a fallback. A fallback
is reconsidered on enable and on a console logon (`Service::reselect_after_fallback`,
2026-11-16): the service starts before a console auto-logon, so `desktops` (needs a
signed-in console) fell back at boot and stayed there; it re-selects only when a fresh
selection differs in a backend kind.

Methods: `kind`, `caps -> SessionCaps`, `probe -> HostProbe`, `prepare_host(&GlobalSettings)`,
`restore_host`, `start(&SeatStartRequest) -> StartStep` (`LaunchHost(HostLaunch)` |
`Started(SessionInfo)`), `wait_session(seat, client_name, timeout)`, `query(seat, id)`,
`log_off(seat, id)`, `adopt(&[KnownSession])`, `enumerate`. Start sequence: module doc of
`seat-core/src/backend/session.rs`.

#### 4.1.3 Kiosk shell seats (`desktops`, spec 35 §5.1 U3 / D′, 2026-11-13 kiosk-shell)

Concurrent kiosk seats inside the console session, for client SKUs on x64 and ARM64. Not
true multi-seat: one session (one clipboard, one secure attention sequence) for every seat;
since 2026-11-17 every kiosk seat 2+ runs as its own Windows user (see "Per-seat users").

- **Config (additive):** `SeatConfig.kiosk_shell {apps: [{title, command, args, autostart,
  monitor}], idle_reset_minutes}` (≤ 16 apps, idle 0 = off or 1..=1440) and
  `settings.kiosk.seat1_desktop` (default `true`). Issues `invalid_kiosk_shell` (error),
  `kiosk_shell_ignored_on_seat_one` (warning). Manager: "Kiosk" column and dialog in the
  basic tab (only with the `desktops` method; with another method and a seat that has a
  kiosk config, the hint `kiosk.backendOnly` explains why it is ignored), seat-1 toggle
  under the seat table. `command` must be an absolute, trimmed Windows path
  (`seat_core::validate::is_absolute_windows_path`, same rule as the manager);
  `kiosk::plan_for` also trims and drops relative commands (fix-kiosk-shell).
- **Seat 1's shell** (maintainer decision 2026-11-15, `seat-service/src/seat1_kiosk.rs`):
  seat 1 is never started through `start_sequence` under `seat_one_console`, so the service
  runs its shell itself while the product is enabled, `desktops` is active and
  `seat1_desktop` is off: started on enable, at boot and on every tick when missing (a
  backend `NotReady`, e.g. no console user yet, retries after 5 s), restarted after a crash
  with the seat-host back-off and crash limit (`CRASH_LIMIT` in `CRASH_WINDOW_MS`, then
  given up until it is unwanted and wanted again), stopped with `log_off` on disable, when
  the config no longer wants it and at service shutdown. Seat 1's status still mirrors the
  console session. `sync_seat1_kiosk` is serialised (`Service::seat1_kiosk_op`; the enable thread and
  the tick both call it) and the desktops backend holds one lock across "running? launch,
  record" and across a stop, so a seat never gets two shells (VM 2026-11-15, d203e07).
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
  (`SC_KEYMENU`); top-level scroll bars are driven by the router and popup menus follow the
  seat cursor since 2026-11-17 (see "Top-level scroll bars", "Seat cursor for apps"). Touch: a primary contact that vanishes without "up"
  is released. Back to the launcher: bar button or Ctrl+Alt+Home (consumed).
- **Filter session map (round 4 local-gaps):** kiosk seats report the console session as
  their session; mapping them in the filter's `SET_SESSIONS` would let every console
  program (seat 1's desktop included, and win32k's raw input thread) open the kiosk seat's
  HID collections. `service::filter_session_map` leaves out every seat but seat 1 whose
  session is the console session (unless `dedicated_host`) or is shared with another
  seat, so those collections stay denied to the console and reach the kiosk shell only
  through the service reader (`input_packets`). Limit: HID game pads of a kiosk seat are
  then usable by no app under the filter (pads are not service-read); HYPOTHESIS,
  untested.
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
  **Hotkey guard (round 4 kiosk-isolation; helper process since 2026-11-15):** with
  `seat1_desktop = false` every shell starts a helper, `pulsar-seat-host --hotkey-guard
  --seat N` (`kiosk::guard`, `kiosk::guard_win`), that runs a `WH_KEYBOARD_LL` hook on its
  own message-loop thread (`seat_input::hotkey_guard`) and swallows both Windows keys,
  Alt+Tab, Alt+Esc, Ctrl+Esc and Ctrl+Shift+Esc from every keyboard. With
  `seat1_desktop = true` the helper runs in its attributing mode since 2026-11-17 (raw input
  tells the keyboards apart; see "Seat 1 and kiosk input without the filter"). Ctrl+Alt+Del
  and Win+L stay out of reach of user mode.
  - **Why a separate process:** an LL hook in the shell itself is not called while the
    shell's focus sink holds the foreground and the shell has its raw-input registration,
    whatever the registration flags (VM bisect 2026-11-15: NOLEGACY, INPUTSINK, HID usages,
    keyboard with no flags, only DEVNOTIFY all fail; no raw reader passes). The helper
    registers no raw input. HYPOTHESIS: Windows skips the LL hooks of the process that owns
    the foreground window and a raw-input registration (seen in the ARM64 VM only).
  - **Process:** a plain child of the shell: same user, session, desktop and integrity, no
    extra rights; it inherits the seat's job, so the service's stop ends it too. The shell
    starts it on its first tick, watches it every tick and starts it again with a back-off
    (1 s doubling to 30 s, reset after 60 s of stable run; `guard::Supervisor`), and stops
    it on its own exit (quit command, pipe closed, kill after 1 s).
  - **Channel:** the helper's stdin is an anonymous pipe from the shell (no name, only the
    two processes hold it), one byte per command (`guard::GuardCmd`): `R` re-install the
    hook (the shell's raw reader saw a guarded key the hook should have swallowed; once per
    detected miss, never a loop), `W` one warm-up key (a window took the foreground for the
    first time), `Q` quit; end of file ends the helper. No key data travels. The helper
    writes its log lines (`hotkey guard: ...`) to the stderr it inherits from the shell, from
    a logger thread; the hook thread does no I/O.
  - **Warm-up:** after installing or re-installing the hook and on `W`, the helper injects
    `VK_F24` down + up with `dwExtraInfo = hotkey_guard::WARMUP_TAG`; the hook swallows
    exactly that key and the helper logs `warm-up: sent / seen / failed` and the latency.
  - VM 2026-11-15 (MSI fae86f1): 0 misses in 3 x 22 presses (Win, Win+R, Win+E, first press
    included) with cmd and with the shell in front, the focus sink holding the foreground
    (`clipped true`); helper killed -> back after 1 s; disable -> helper gone. HYPOTHESIS:
    untested on hardware.
- **Kiosk console apps in conhost (round 4, `seat_service::kiosk_console`):** while
  enabled with `desktops`, `seat1_desktop = false` and a kiosk-shell seat, the service sets
  the console user's `HKU\<SID>\Console\%%Startup` `DelegationConsole` and
  `DelegationTerminal` to the conhost CLSID `{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}`, so
  console apps stay inside the shell's job (bar button, placement) instead of handing off to
  Windows Terminal. The old values (absent or not) go to `journal\kiosk-console.json`
  first and are restored on disable, shutdown, a config change that no longer wants it,
  and uninstall. The service only writes loaded hives (`HKEY_USERS\<SID>`), never loads
  one. Never for seat 1's desktop user (`seat1_desktop = true`) or the machine default.
- **Per-process isolation (round 4 kiosk-isolation, 2026-11-14):** all kiosk seats share
  one session, so Windows hands every HID device and every raw-input record to any app of
  any seat. Kiosk apps can now load the remote kind's per-process module
  (`shim/README.md` "Kiosk seats"):
  - **Kiosk seat map:** the service publishes `HKLM\SOFTWARE\PulsarSplit\Runtime\KioskSeats\<NN>`
    (volatile, Everyone read, SYSTEM write; `seat_shim_core::kiosk`) on the input tick:
    one key per kiosk-shell seat on the console session (never under `dedicated_host`)
    with the instance ids of the keyboards, mice and HID devices whose effective mask is
    exactly that seat (`service::kiosk_map_entries`, `Platform::publish_kiosk_map`; the
    first publish of a service run clears the key, later ones apply a diff).
  - **Shell:** sets `PULSAR_SEAT_KIOSK=<seat>` in its own environment (inherited by every
    app and its children) and, when our AppHelp database is registered and
    `shimd\pulsar-split-launch.exe` exists, runs `pulsar-split-launch --attach <pid>`
    on each app while it is still suspended (x86 targets are handed to the x86 launcher).
    Exit 5 (remote thread timed out) = kill the app and start it again without the module;
    any other result = resume (pad device security and the router still apply).
  - **Module:** role `kiosk` only when the variable names a published seat of the
    process's own session (`decide::role_full`; not a security boundary: every kiosk
    seat shares the console's window station and desktop, also with per-seat Windows
    users, see "Parity review" below). Kiosk processes get the HID open / enumeration groups
    and the raw-input data group (`GetRawInputData`, `GetRawInputBuffer` in 64-bit
    processes): devices of other seats and unassigned console devices are denied or
    dropped; while a kiosk seat runs, enumeration also looks at keyboard / mouse class
    interfaces (PS/2). Seat-1 console processes get the HID groups while a kiosk seat of
    their session wants HID isolation (kiosk devices denied to them).
  - **Limits:** since parity-arm64 (2026-11-17) native ARM64 apps take the ARM64 module
    (Microsoft Detours backend) and the ARM64 MSI ships `shim\arm64\`, `shim\x64\` and
    `shim\x86\`; the shell calls its own architecture's launcher
    (`seat_shim_core::launch::native_launcher`), which hands x64 / x86 apps to theirs (x64
    and x86 modules under emulation on ARM64: HYPOTHESIS). 32-bit apps:
    `GetRawInputBuffer` is passed through (WOW64 buffer layout reported to differ, HYPOTHESIS). Seat-1 console apps get the
    raw-input data filter too since 2026-11-17 (kiosk devices dropped for them). The
    launcher blocks the shell's UI thread while it attaches (≤ 15 s per remote thread).
    **HYPOTHESIS: nothing of this ran on hardware.**
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
- **Per-seat users (parity 2026-11-17, `seat_session::kiosk_account`, seat-service
  `win::kiosk_logon`):** `settings.kiosk.accounts` (`per_seat` default, `shared` = the old
  behaviour; a missing field reads as `per_seat`, which is the migration of older configs).
  `desktops::start` asks `DesktopsOptions::logons` (wired by the service, not in console dev
  mode) for the seat's identity (`kiosk_account::identity_for`): seat 1 and `shared` = the
  console user; the seat's configured account when a password is stored for it; otherwise
  Pulsar's own local account `PulsarKiosk<NN>` (`host_account::ensure_kiosk`: random
  password in LSA `L$PulsarSeat.KioskAccount.NN`, `Users` only, interactive logon granted,
  remote / network / service / batch logon denied, hidden from the logon screen, comment
  marks it as Pulsar's; created or maintained once per service run; pruned at service start
  for seats that are no longer kiosk seats and at uninstall, profile included). The logon:
  `LogonUserW(LOGON32_LOGON_INTERACTIVE)` + `TokenSessionId` = console
  (`win::host_account::interactive_token_in_session`), the logon SID journaled
  (`run\kiosk-grants.json`) before `pulsar-seat-host --desktop-acl grant --sid S --kiosk`
  grants the interactive user's rights on `WinSta0` / `Default`
  (`KIOSK_WINSTA_ACCESS` / `KIOSK_DESKTOP_ACCESS`) **and on the session's named-object
  directory** `\Sessions\<id>\BaseNamedObjects` (`KIOSK_BNO_ACCESS` = query, traverse, create
  object, create subdirectory, read control: the session owner's own mask;
  `win::host_account::edit_session_named_objects`, `NtOpenDirectoryObject` +
  `SetSecurityInfo`; added 2026-11-24 after the gaming PC E2E, bug 6: everyone else only gets
  query + traverse there, so the seat user could not create any `Local\` object and the
  shell's stop event failed; a revoke always clears it, best effort, since the ACE dies with
  the session), `LoadUserProfileW`, then
  `SessionLauncher::launch_as` (`CreateProcessAsUserW` with that token, the seat's job). The
  logon lives as long as the shell (dropped on `log_off` or when a dead shell is restarted:
  profile unloaded, grant revoked; a crashed run's grants are swept at the next start). Any
  failure falls back to the console user (log line, probe note `kiosk_account_fallback`;
  with logons wired the probe reports `kiosk_per_seat_accounts` instead of
  `no_per_user_isolation`). The shell's apps inherit the seat user; the focus mutex now has
  a DACL for every interactive user (`SYNCHRONIZE | MUTEX_MODIFY_STATE`) so shells of
  different users still compete for it. `SessionBackend::kiosk_users` lists the shells'
  user SIDs; the service sets conhost delegation in the hives of Pulsar's own kiosk accounts
  (kiosk-only users, no journal) and leaves a seat's configured account alone. Data the apps
  kept in the console user's profile before the migration is not moved. Manager: toggle
  "Her kiosk koltuğu kendi Windows kullanıcısıyla çalışsın" and a "Kullanıcı: …" line per
  kiosk seat. `SessionCaps::per_user` stays `false` (one session).
- **Seat 1 and kiosk input without the filter (parity 2026-11-17):** (1) per-process module:
  seat-1 console processes get the raw-input data filter while a kiosk seat of their
  session runs (`decide::hook_plan_full`, `Role::Console`), so apps that read raw input never
  see kiosk keyboards / mice; (2) hotkey guard **attributing mode** (`seat1_desktop = true`,
  `pulsar-seat-host --hotkey-guard --seat N --attribute`): the helper also reads raw input
  (keyboards + mice, `RIDEV_INPUTSINK`, own message-only window and thread) and tells kiosk
  devices (kiosk map, parent chain) from seat 1's; `seat_input::hotkey_guard::Attributor`
  matches raw records with `WH_KEYBOARD_LL` calls: raw first = decided at once (every key of
  a kiosk keyboard is kept from seat 1), hook first = guarded hotkeys are held back and
  dropped (kiosk) or re-injected with `REPLAY_TAG` (seat 1, or no record within 40 ms), in
  order. `WH_MOUSE_LL` + `MouseAttributor`: clicks and wheel of kiosk mice found in the raw
  list are swallowed (moves always pass: kiosk mice still move the system cursor without the
  filter). Not run while the KMDF filter is active (the kiosk devices never reach the
  session then). Ctrl+Alt+Del (SAS) and Win+L cannot be blocked from user mode at all
  (Winlogon / the secure desktop handle them before any hook); documented in the manager.
  **HYPOTHESIS:** the order of raw input and low-level hooks, the timings and the helper's
  hooks being called while it owns a raw-input registration (it never owns the foreground).
- **Per-seat devices in apps (parity 2026-11-17):** the kiosk map now lists the seat's USB
  hubs / ports and other USB devices as well (`service::kiosk_map_entries`), so XInput pads
  (XUSB, not HID) and pads on a seat's hub are classified as the seat's; kiosk processes'
  `RegisterRawInputDevices` get `RIDEV_INPUTSINK` (`kiosk_cursor::kiosk_raw_flags`), since a
  kiosk app is almost never the foreground window; DirectInput / XInput go through the HID /
  XUSB open and enumeration filters. The module side needs the shim on the app's
  architecture (x64 / x86, and ARM64 once that module ships; the shell has no arch gate).
- **Seat cursor for apps:** the shell publishes its cursor in `Local\PulsarSeat.KioskCursor.NN`
  (`seat_shim_core::kiosk_cursor`, seqlock record); kiosk processes' `GetCursorPos`,
  `GetPhysicalCursorPos`, `GetCursorInfo` and `GetMessagePos` answer from it (flag 4 of the
  kiosk map), so popup menus opened at "the cursor" (`TrackPopupMenu(Ex)`) and tooltips
  follow the seat's mouse. `WM_CONTEXTMENU` already carried the seat position.
- **Top-level scroll bars:** `WindowQuery::scroll_bar` (`GetScrollBarInfo` +
  `GetScrollInfo`); the router posts `WM_VSCROLL` / `WM_HSCROLL` itself: `SB_LINE*` /
  `SB_PAGE*` on the press with auto-repeat (400 ms, then every 50 ms, driven by the 250 ms
  tick), `SB_THUMBTRACK` while the thumb is dragged, `SB_THUMBPOSITION` + `SB_ENDSCROLL` on
  release. 16-bit positions only (as in the message itself).
- **Hand-offs, packaged and full-screen apps (best effort, `seat-host/src/kiosk/track.rs`):**
  a process / window snapshot every second adds break-away children of seat processes
  (created after their parent), and, when the shell runs as a per-seat user, every process
  of that user in the session except shell infrastructure; seat windows owned by other
  processes (packaged apps' `ApplicationFrameWindow` hosting a seat process, owned windows)
  are routed and placed too. A reset ends the tracked processes outside the job (creation
  time checked). Packaged apps (`KioskShellApp.app_id`, AUMID) are started with
  `IApplicationActivationManager::ActivateApplication`, their pid tracked and offered to the
  seat's job. `KioskShellApp.fullscreen`: while it is the seat's focused app and covers a
  seat monitor, the focus leader gives it the real foreground (instead of the sink) and clips
  the cursor onto it; a minimised full-screen window is restored unless the seat user
  minimised it. One exclusive full-screen window per PC at a time (one foreground); with
  `seat1_desktop = true` no kiosk app gets the foreground (seat 1 has it).
- **Focus at launch / shim attach (VM 2026-11-17):** when several autostart apps show
  their first window in one pass, the app launched last gets the focus
  (`kiosk::latest_arrival`); before, the z-order walk's last window won and keys went to an
  app behind the visible one. The shell attaches the per-process module only when the
  opt-in AppHelp database is registered (`shimctl install`); in the VM the native ARM64
  module (`shim\arm64\`) loaded into ARM64 charmap / cmd and seat keys reached cmd.
- **Limits / open:** since the parity review the module answers `GetAsyncKeyState` /
  `GetKeyState` / `GetKeyboardState` from the seat's own key table and turns
  `SetCursorPos` / `SetPhysicalCursorPos` / `ClipCursor` into seat-cursor requests (section
  `kiosk_cursor` v2), and keeps a per-seat clipboard (`kiosk_clip`); processes without the
  module still see the console's state. Scroll positions
  above 65 535; packaged apps for a per-seat user without Explorer may be refused
  (HYPOTHESIS); in `shared` mode a hand-off to an instance already running for the console
  user stays with that instance. Untested on hardware: everything in the shell, the stop
  event across sessions, the release helper, the filter packet stream to the shell, the
  seat-user logon, the attributing guard.

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
  frame_reader_sid(seat)`: the seat's own `PulsarSeatHostNN` SID while hosts run as
  per-seat accounts, else the shared `PulsarSeatHost` SID while hosts run as it (not
  `HostTokenCompat` 1 / 2, account set up, not failed), else `None` (SYSTEM Low hosts read
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
- *Attach (gaming PC E2E bug 9, 2026-11-24).* A plan target (console list, which is every
  monitor in the wizard's extend step, or a seat target the plan does not take out) that
  is connected (`targetAvailable`) but outside the desktop and not specialized gets a CCD
  path: `plan_topology(.., console, attach, ..)` adds `RestoreTarget { activate: true }`
  and journals `JournalEntry { attached: true }` first (serde default false, old journals
  parse). `plan_paths` takes a free source on the target's own adapter first
  (`PathCandidate { source_luid, available }`; `ActivePath.source_luid`), another
  adapter's only as a last resort, never a path without a monitor; the apply is the same
  validated, verified, rolled-back `SetDisplayConfig` (explicit paths,
  `SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE`). Once no plan names an attached target, its
  path is removed again (never the console's last monitor) and the entry dropped; a target
  the user detached meanwhile just loses its entry. Not under `console_only` or with an
  untrusted journal. Why: on the hybrid gaming PC the user's CCD database kept the DP
  monitor detached, so `DisplaySwitch /extend` and `SDC_TOPOLOGY_EXTEND`
  (`ccd::extend_desktop`, database driven, not used by the service) left it out, and the
  wizard's extend step (`apply_topology` with every wizard monitor as console) only
  reported it `missing`. HYPOTHESIS on hardware until round 2 sees seat 2 bind that monitor
  without a manual `ccd.ps1` attach.
- *Clone.* `AssignmentOptions::outputs` (target ids on the surface monitor's adapter,
  needs `OutputSettings::allow_clone`) reaches the binding through
  `DisplayBinding::configure_seat(seat, &SeatDisplayPrefs::from_config(&config, seat))`
  (default no-op trait method; the service calls it in `run_start` right before
  `bind_seat`). `bind_seat` appends one `OutputPlacement { clone_of: Some(i) }` per further target
  after every non-clone entry, so RDP monitor `i` = `outputs[i]`
  (`SeatOutputs::layout_outputs`). Both presenters show output `i`'s frames and cursor on
  its clones (own mode, letterbox). The ActiveX client cannot clone: the host covers
  desktop clone targets with a presenter window and a localized notice. IddCx monitors are
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
- *Hybrid GPU and presenter diagnostics (gaming PC E2E bug 3, 2026-11-24).* On the x64
  hybrid laptop the rdp_decode picture stayed black on both dGPU monitors while the host
  counted frames and presents; `session_capture` frames (same flip chain, `take_shared`)
  showed. Root cause not proven: the host had **no `log` logger**, so every presenter
  warning (flip chain adapter fallback, flip failure, GDI fallback) was dropped. Changes:
  (1) the seat host installs a stderr logger (`seat-host/src/hostlog.rs`,
  `PULSAR_SEAT_HOST_LOG`, default `info`; not in `--display-helper` mode); (2) the flip
  chain device goes on the adapter DXGI lists the window's monitor under
  (`win::dxgi_output_owner`, `model::pick_flip_adapter`), not blindly on the CCD target's
  adapter, logged when they differ; an `EnumAdapterByLuid` fallback to the default adapter
  is a warning now; (3) a flip chain that fails for good is dropped and the output gets a
  **new window** for the GDI blit (HYPOTHESIS: GDI on a window that had a flip-model swap
  chain shows nothing; `gdi_only`, re-created through `WM_APP_RECONFIGURE`, which now
  resets every output's GPU state first); (4) diagnostics: per output the adapter choice,
  the first RDP frame's sampled brightness (`model::nonblack_samples`, step 16, B+G+R >
  30), and every 10 s when a counter moved: CPU / GPU frames in, flip presents, present
  errors, `DXGI_STATUS_OCCLUDED` presents, GDI paints, last frame brightness; seat-rdp
  logs the graphics adapter per connection. Reading them: frames not black + monitor
  black = presentation (try `PULSAR_SEAT_PRESENTER=gdi`); frames black = the RDP client's
  EGFX composition (try `PULSAR_SEAT_GFX=no-h264` or `bitmap`). Round 1 stats had
  `h264=0` with hundreds of EGFX frames, so no H.264 picture was decoded at all.
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
- *One IddCx adapter per render GPU (2026-11-17, driver interface minor 3).*
  `IddCxAdapterSetRenderAdapter` is adapter-wide, so each render GPU gets its own adapter:
  the installer's `Root\PulsarSeatIdd` plus software devnodes `SWD\PulsarSeatIdd\gpuN`
  (hardware id `PulsarSeatIdd_Adapter`, same INF and DLL) that `win::idd::WinIdd`
  creates with `SwDeviceCreate` when no adapter fits (lifetime = the client's handle: a
  service stop removes them). The driver binds an adapter to the GPU of its first
  monitor (or `SET_RENDER_ADAPTER`), reports it with `IOCTL_PSIDD_GET_ADAPTER_INFO`
  (`{size, flags READY|BOUND|FP16, render_luid, monitor_count, iddcx_version}`) and
  refuses a monitor of another GPU with `STATUS_DEVICE_BUSY` while it still has monitors;
  an empty adapter is re-bound. Adapter choice is pure (`idd_adapters::pick_adapter`:
  bound to the GPU, else an empty unbound one, else an empty one, else create, at most
  `PSIDD_MAX_ADAPTERS` = 8); removes go to the adapter that took the monitor (else every
  adapter, after a service restart). The driver keeps a registry of device contexts
  (several local devnodes may share a host process) instead of the old single global.
- *HDR / VRR for seats 2..N (2026-11-17).* Per monitor `AssignmentOptions::{hdr, vrr}`
  (additive, off by default) reach `OutputPlacement::{hdr, vrr}` through
  `SeatDisplayPrefs` (never for seat 1: Windows' own settings apply there). `pulsar_idd`:
  with IddCx 1.10 at run time the adapter sets `CAN_PROCESS_FP16`; a monitor added with
  `PSIDD_ADD_FLAG_HDR` reports `HIGH_COLOR_SPACE | WIDE_COLOR_SPACE` target caps and 8 + 10
  bit modes (`ParseMonitorDescription2` / `QueryTargetModes2`), takes its frames from
  `ReleaseAndAcquireBuffer2` and publishes FP16 / 10-bit pixels with the DXGI colour space,
  SDR white level and HDR10 metadata (frame header minor 3, `PSIDD_FORMAT_*`, section sized
  8 bytes per pixel); SDR monitors that get an FP16 surface receive a BGRA8 conversion.
  `IddFrameReader` yields `HdrFrameRef`s (`seat_core::backend::hdr`), `FrameSink::
  present_hdr` (default: tone-mapped to SDR, SDR white = display white, highlights clip).
  Display Core targets with `hdr` pick an FP16 source mode with an HDR10 wire format
  (`model::pick_presenter_mode`), set the wire format and scan out scRGB primaries;
  `vrr` takes the highest refresh of the size and presents with sync interval 0 +
  `AllowTearing`. The flip-model window presenter does VRR through
  `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING` / `Present(0, ALLOW_TEARING)` where DXGI reports
  the feature; it stays SDR (HDR frames tone-mapped). HDCP: indirect displays and our
  presenter never carry protected content; documented limitation (troubleshooting).
  HYPOTHESIS throughout (no hardware run). **Frame source (parity review 2026-11-17):**
  `seats.rs` tells the binding whether the session backend has its own display
  (`DisplayBinding::set_session_has_display(caps.remote_display)`); without one (desktops
  kiosk seats) `pulsar_idd` plugs a virtual monitor per output (frame sections granted to
  INTERACTIVE, the seats share the console desktop anyway), `DisplayBinding::idd_frames`
  says so and the outputs travel in `SeatStartRequest::idd_outputs` -> `KioskPlan::
  idd_outputs` to the kiosk shell, which runs `seat_host::display::IddPresenter`: one
  `IddFrameReader` per output (connector = output index) `pump()`ing frames, HDR included,
  into a `HybridPresenter` over the seat's targets. RDP seats keep their RDP frames.
- *Per-seat GPU preference (2026-11-17).* `LocalSeat::gpu_preference {mode: off |
  seat_gpu | high_performance | power_saving, apps}`: at each seat logon the service
  writes `HKU\<sid>\Software\Microsoft\DirectX\UserGpuPreferences\<exe>` =
  `GpuPreference=N;` (other pairs kept) through `seat_compat::gpu_pref::sync_user`, every
  value journaled first (`Change::UserGpuPreference`), programs that left the list undone,
  all undone on disable / reset / uninstall. `seat_gpu` = 2 or 1 depending on how Windows
  ranks the GPU of the seat's first monitor (`win::gpu_ranks`, `EnumAdapterByGpuPreference`);
  a single GPU or a third GPU writes nothing (notice `gpu_preference_unavailable`). A
  preference, not enumeration isolation (spec 40 §6.4). Validation
  `invalid_gpu_preference` (absolute `.exe` paths, at most 64).
- *Latency hooks (2026-11-17).* `seat_core::latency::LatencyStats` (p50 / p95 / p99 /
  max over the last 512 samples): the IddCx frame header carries QPC stamps (acquired,
  OS present, written) and `win::idd::pump` logs driver-copy and to-presented latency
  every 600 frames; `GfxStats` carries H.264 decode and change-to-present latency.
- *Split screen (OSMS-parity G8, 2026-11-20).* Up to four local seats share one physical
  monitor, each in a half or a quarter of a 2x2 grid (`seat_core::area`). Config:
  `AssignmentOptions::areas: Vec<SeatArea {seat, area: ScreenArea}>` on a monitor whose
  explicit mask names two to four seats (additive; old configs load unchanged; ignored for
  one seat). Validation (`area::check_split`): `invalid_screen_area` (not a monitor, shared
  mask, more than four seats, a seat of the mask without exactly one area, clone outputs on
  a split), `screen_area_overlap`, `console_share_not_half` (seat 1 only half and half with
  one other seat); a valid split is exempt from `exclusive_on_several_seats`. HDR, VRR, mode
  and software cursor stay per monitor, so the seats of a split share them.
  Topology: a split monitor is reserved for nobody (`Service::display_plan` puts it in no
  seat list), so it stays in the console desktop and is never specialized; with seat 1 in
  the mask it is a console monitor. Binding: `SeatDisplayPrefs::{areas, console_share}`
  -> `Binding` keeps one bound entry per seat, so a split target is "reference counted by
  seat" (released when its last seat stops); `bind_seat` refuses overlapping cells and
  whole-monitor users (`target_busy`) and a split target outside the console desktop
  (`split_needs_desktop`). A split output is always `Fallback` (one top-most presenter
  window per seat on a shared monitor: no independent flip, about one frame more latency,
  shown in the manager), `rect` = the area's part of the desktop rect, `follow_mode` off
  (the other seats use the mode), `pulsar_idd` gets the area size as the only mode
  (HYPOTHESIS: Windows picks it although the EDID's preferred timing is the full panel).
  The RDP monitor layout uses `rect`, so the seat session is the size of its part.
  Presenter: `relayout` cuts the live desktop rect with `area.rect_in`; `console_clip` and
  the guard home (`model::console_home`) use `area::free_part` (the console part of a
  monitor next to seat surfaces when it is a rectangle). Console share: an output with
  `console_share` (seat 1 shares that monitor) makes the seat host register a hidden
  AppBar (`seat_display::win::appbar`, `SHAppBarMessage` `ABM_NEW` / `ABM_QUERYPOS` /
  `ABM_SETPOS` / `ABM_REMOVE`, shell32, documented API) docked to the seat half's edge, so
  the console work area shrinks to its half; the cursor clip keeps the console pointer
  there. Halves only (an AppBar docks to one edge). No persistent state: the reservation
  lives with the AppBar window; `ABM_REMOVE` on presenter reconfigure / drop, and a killed
  seat host takes its window with it (HYPOTHESIS: Explorer drops the reservation of a dead
  AppBar window on its next work-area check). Kiosk shell seats: `DeviceView::areas` (valid
  splits only, `area::split_areas`) and `kiosk::seat_devices` give the seat its part as
  its monitor rect. The idle saver never powers off a split monitor
  (`Service::idle_monitors` = the seat's own monitors only). No driver, no SudoVDA, no
  virtual console display yet: our `pulsar_idd` as the console's virtual display stays a
  later step once it is signed. Same code on x64 and ARM64. Untested on hardware: all of it.

### 4.3 Input (`InputRouter` service-side, `InputSink` host-side; seat-input)

`raw_input_host` (default, user mode; isolation complete only under `dedicated_host`),
`winusb_capture` (raw input plus USB keyboards / mice of running extra seats on the inbox
WinUSB driver, below), `kmdf_filter` (our filters, not signed), `none`. Methods: `enumerate`,
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

**`winusb_capture` (osms G2, 2026-11-20; untested on hardware, HYPOTHESIS throughout).**
User-mode isolation of USB keyboards / mice without a driver of ours (works with HVCI /
Secure Boot; clean room from Microsoft's SetupAPI / newdev / WinUSB docs, the USB HID 1.11
spec, the HID Usage Tables and the Keyboard Scan Code Specification; no OSMS code ported).
Hybrid router `seat_input::winusb_router::WinusbCaptureRouter`: every unit is handled as with
`raw_input_host` (seat hosts read raw input, the console listener runs), and in addition
the USB devnode under a seat-routed unit (`usb_capture::usb_node_of`: the HID collection's
parent `USB\…&MI_xx` interface node, or the device node of a single-interface device; never
a hub) is moved to the **inbox** `winusb.inf` while that seat has a session:

- *Plan* (`usb_capture::plan`, pure): units are grouped by that node, because a swap takes
  every HID collection under it. A group is captured only when all its keyboards / mice go
  to one seat ≥ 2 that runs (`InputControl::set_running_seats`, from the logons in
  `apply_ownership`), its consumer / system-control collections are on seat 1 or the same
  seat (they follow), and it has no other collection (pad, touch, pen). Refusals are notes:
  `usb_capture_mixed_seats`, `usb_capture_unsupported_collection`,
  `usb_capture_last_console_input` (never the last keyboard or mouse the console still has;
  a PC with none is not made worse). No capture while the capture wizard runs
  (`set_wizard_capture(true)` releases everything first, so the units reach the listener).
- *Swap* (`win::usb_swap::WinUsbSwap`): `Device Parameters\DeviceInterfaceGUIDs` = our own
  GUID `{6f1d3a52-9c47-4b8e-a5d0-2e7c41b98f36}`, `SPDRP_CLASSGUID` = `USBDevice`, then
  `DI_ENUMSINGLEINF` with `%windir%\INF\winusb.inf`, `SetupDiBuildDriverInfoList`,
  `newdev!DiInstallDevice` on that one devnode. The journal records the exact original
  package (`DEVPKEY_Device_DriverInfPath` / `DriverInfSection` / `DriverVersion` /
  `DriverProvider` / `MatchingDeviceId`, and the extension INFs of
  `DEVPKEY_Device_ExtendedConfigurationIds`). Release sets the journaled class, installs the
  driver node of the journaled INF whose section and version match (fallback: the driver
  Windows ranks best, `SetupDiSelectBestCompatDrv`, never a fixed `input.inf`), deletes the
  GUID value, then re-applies each journaled extension INF the node did not get back by
  itself (`DiInstallDriverW`, like `pnputil /add-driver /install`; its software components
  come back with it). After every restore `UsbCapture` compares the node with the journal
  and logs `restore check: exact` or `restore check: NOT exact: …` (note
  `usb_restore_inexact`). Gaming PC 2026-11-24 (bug 5): the old restore put `input.inf` on a
  Logitech interface that ran `oem128.inf` and lost the extension `oem185.inf`; extension
  re-apply and section pick are HYPOTHESIS until round 2. A node that is not present is
  removed (`SetupDiRemoveDevice`) so Windows enumerates it fresh on its own driver when it is
  plugged in again. windows-sys structs sized with `size_of` (same code on x64 / ARM64;
  `DiInstallDevice` on ARM64 not yet run).
- *Journal* (`journal\usb-capture.json`, `usb_capture::FileJournal`): written **before** each
  swap (temporary file + `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`); a journal write
  failure blocks the swap. A new `UsbCapture` loads it as pending releases, so a later save
  never drops an earlier run's entry. Restore: at service start (`Service::boot`, before
  anything else and also in Windows Safe Mode, `BackendFactory::restore_usb_input`), on seat
  stop (next `apply_ownership`), on disable (`apply_routes(&[])`), when the input backend is
  deselected and at service stop (`release_usb_input`), on arrival of a captured node
  (`CM_Register_Notification` on our GUID retries pending releases), on the admin request
  `restore_usb_input{confirm}` (manager: Advanced > methods) and at uninstall
  (`seat-service uninstall`, `usb_capture::restore_at_start`). Nodes that still carry our
  capture but are not journaled (lost journal) get the driver Windows ranks best. "Still
  carry" = our GUID in `DeviceInterfaceGUIDs` or the `USBDevice` class
  (`UsbSwap::still_captured`): Windows keeps the interface registration after the release,
  so the interface list alone also names nodes restored long ago (proven on the gaming PC:
  the sweep reinstalled `input.inf` over the just-restored vendor driver, then at every
  start). The journal entry stays (as pending) until its swap-back is done. The operator's
  `restore_usb_input` first holds captures off (`UsbCapture::hold`, note `usb_capture_held`,
  answer `capture_held: true`) until a seat starts or the input backend / its settings are
  chosen again (a new router); before, the next route apply captured again 1-3 s later.
- *Reader* (`win::winusb_reader`, one thread per captured node, in the service):
  `CM_Get_Device_Interface_List` on our GUID, `CreateFile` + `WinUsb_Initialize`, the first
  interrupt-IN pipe, the HID report descriptor by `GET_DESCRIPTOR(0x22)` (boot-protocol
  layout when it fails). `hid_layout` parses the descriptor, `hid_translate` maps reports to
  `RawUnitEvent`s (`hid_keys`: keyboard page → set-1), then the same `UnitProcessor` as the
  other paths (hotkey, emergency combination, SAS, key-up after a gap) and
  `PacketSink` → `input_packets` → seat host → RDP fast path. Activity of captured units goes
  to the activity sink from here (they are gone from raw input). The seat host under
  `winusb_capture` runs its raw reader **and** forwards `input_packets` (`seat-host/src/input.rs`).
- Limits: LED output reports (`TS_SET_KEYBOARD_INDICATORS_PDU` → `SET_REPORT(Output)`) not
  done (keyboard LEDs stay dark); Bluetooth, I2C / SPI (most ARM64 laptops) and PS/2 units
  stay on raw input (note `input_not_isolated_non_usb` under `seat_one_console`); about a
  second of PnP restart per capture; a seat-start capture follows the next ownership apply
  (hot-plug: a new USB keyboard on a running seat is captured when the inventory refresh
  re-applies routes). Notes: `usb_input_captured`, `usb_capture_failed`, `usb_capture_held`,
  `usb_restore_inexact`. VM test with USB
  passthrough first (docs/progress/osms-local-parity.md G2 point 8).

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
  job. Parity review 2026-11-17: every tick (`Service::cover_seat_sessions`,
  `Platform::cover_session`) the seat session's `explorer.exe` joins the seat job (so the
  Start menu and Explorer launches inherit it), and with the opt-in `cpu_seat_wide` every
  other new process of the session (UAC-elevated ones included) gets the soft limit once
  (`SetProcessDefaultCpuSets`; protected processes are logged and skipped; seats that share
  a session are never limited session-wide). Untested on hardware.
- **Source IP (spec 80 §11.3), two layers (net-wfp, 2026-11-17):**
  1. **Enforced by the WFP callout `drivers/seat-wfp`** (KMDF, non-PnP, ALE bind-redirect +
     connect-redirect V4; `drivers/seat-wfp/README.md`). `Service::publish_source_ip` runs
     in every `apply_ownership` (boot, config apply, hot-plug, each logon / logoff): the
     signed-in seats (local logons + running remote seats) with a valid `ip_binding`,
     minus seats that share their session (`source_ip_entries`; kiosk shell seats in the
     console session), go to `Platform::publish_source_ip_map`. The Windows platform
     converts the program list to NT paths (`seat_compat::wfp::build_seats`,
     `QueryDosDeviceW`; unconvertible entries are reported as `dropped_paths`) and sends
     the session -> address map over `\\.\PulsarSeatWfp` (`seat_compat::wfp::Publisher`:
     only on change, rising generation, re-sent when the driver lost it). The handle stays
     open for the service's lifetime; the driver drops the map when it closes (fail open:
     session ids are reused). Multiseat off / Windows Safe Mode = empty map.
     `get_backends` carries `source_ip` (`SourceIpReport`: `driver` with the enforced
     seats, or `environment` with the reason) and the driver row `source_ip_callout`
     (probe through the same handle). Config validation: loopback seat address = error
     (`loopback_ip_address`), link-local = warning, kiosk shell seat with an address =
     warning (`ip_binding_shared_session`), class E = invalid.
  2. **Fallback, always on:** every program the service starts for the user (startup
     apps, launcher request) whose seat rule (`ip_binding` + `settings.ip_programs`) picks
     it gets `PULSAR_SEAT_SOURCE_IP=<address>` (`seat_compat::srcip::{from_config,
     launch_env}`); the agent never gets it (children inherit the environment whatever the
     rule says). Without a loaded (Microsoft-signed) driver this is the only path.
  Driver limits: IPv4 only; explicit binds to an address and loopback traffic untouched;
  per session, not per seat. VM round 2026-11-17 (ARM64, test-signed, HVCI, Verifier; two
  fast-user-switch seats): listeners and UDP sockets get the seat address at bind, TCP
  clients at connect (SYN and datagrams left with the seat addresses, pktmon), loopback
  TCP works, the SID-string SDDL is accepted. Limit: UDP to `127/8` from a socket the
  program never bound fails (`WSAEADDRNOTAVAIL`; exception via the program list).
  HYPOTHESIS: app ids of network paths, dual-stack sockets.
- **Fast user switching from a logon-screen console (VM 2026-11-17):** when the console
  has nobody signed in, seat 1 has no session of its own and used to follow the console
  into a seat N hand-over session. Now: the credential provider's answer for a seat 1
  without a user stays `pending` during a hand-over; `refresh_console_seat` never binds
  seat 1 to a session a seat N holds (and gives it back once the seat's session is known);
  session events of such a session go to the seat, not to seat 1; and a seat whose logon
  screen went to another seat's hand-over before a sign-in is not restarted (it stopped
  two seats from swapping the console for ever).
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
  reactivation; lost pictures trigger a redraw, then a graphics protocol reset. The
  decoder runs on the adapter of the seat's first output (`FrameSink::gpu_adapter`).
  **Zero-copy (round 4, `seat-rdp/src/gpu/`):** `GfxChannel` wraps the IronRDP client;
  surfaces mapped 1:1 over GPU-capable outputs move to the GPU at an AVC420 IDR (seeded
  once from the image), are decoded and blitted NV12 to BGRA on the presenter's adapter
  and reach the presenter as shared keyed-mutex textures (`FrameSink::present_gpu`,
  `GpuFrameRef`: producer releases key 1, presenter copies and releases key 0); other
  codecs' paints are copied up per frame; anything that reads AVC pixels sends the
  surface back to the CPU path (protocol reset when its CPU pixels are stale).
  **Back-pressure:** `FrameSink::backlog` (outputs whose last frame is not on screen)
  defers CPU presents (≤ 50 ms) and holds `FrameAcknowledge` (≤ 3 frames / 100 ms, queue
  depth = held bytes). Overrides `PULSAR_SEAT_GFX`, `PULSAR_SEAT_H264_DECODER`,
  `PULSAR_SEAT_GFX_ZERO_COPY=0`; metrics in `SessionEvent::Graphics`. GPU behaviour is
  HYPOTHESIS until the first hardware run (crate README, Known limits).
- **Input:** `SeatInputSink` = `RdpInputSink` over `seat_rdp::Handle` (fast-path, MS-RDPEI
  touch / pen). `Move` packets become relative events while the server hides the pointer
  and announced `INPUT_FLAG_MOUSE_RELATIVE`.
- **Audio:** rdpsnd (PCM) to `AudioBridge::start_playback/play`. `AUDIO_INPUT` from
  `AudioBridge::start_capture`. `rebind_audio` maps to `AudioBridge::set_route`. Dead
  streams are reopened every second. Since 2026-11-24 (gaming PC E2E, bug 4: no wave ever
  reached the host with the static channel alone) playback is also offered on
  `AUDIO_PLAYBACK_DVC` + `AUDIO_PLAYBACK_LOSSY_DVC` (`seat_rdp::audio::dvc_playback`, one
  shared state, own handler on the same sink; Windows 8 and later try these first,
  HYPOTHESIS that the tested Windows 11 build did not fall back). Every format exchange is
  logged (`seat host: audio formats on <transport>: ...`, or `none of ours matches`), as are
  `playback started ... on the seat's endpoint` / `seat endpoint NOT open (notes ...)` and
  `playback stopped`; no such line while sound plays in the seat = the server never opened
  audio for this connection.
- **Seat audio jitter buffer (2026-11-20, OSMS parity G7):** the playback FIFO
  `seat_media::bridge::PcmRing` is also the jitter buffer. Its policy is
  `JitterConfig` (`AudioBridge::jitter`, default `JitterConfig::SEAT`: target 30 ms,
  cap 100 ms, fade 5 ms, drift on; `PcmRing::new` stays a plain FIFO for the
  identification tone). The ring is pure Rust and unit-tested in `bridge.rs`.
  - `push`: over the cap it drops the oldest audio back to the target in one jump (whole
    frames), counted in `drops` and `dropped_bytes`.
  - `fill` (render side) always writes the whole device buffer. It writes silence until
    the target is queued, then plays with a fade-in. A short read while playing is an
    underrun: it fades out the tail, writes silence for the rest, counts `underruns` and
    prebuffers again. The fades handle 8/16/24/32-bit integer PCM (rdpsnd negotiates
    16-bit), not only float as in OSMS.
  - Drift: over each 100 ms window, if the queue depth stays more than 10 ms above the
    target, one frame is skipped; if it stays more than 10 ms below, one frame is
    repeated (`drift_skipped_frames` / `drift_repeated_frames`). At most one frame per
    window, so about 200 ppm at 48 kHz. OSMS has no drift correction.
  - WASAPI render (`win/wasapi.rs`): shared mode, `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` +
    `SetEventHandle`, 20 ms engine buffer, one buffer of silence pre-rolled. Every wake
    fills the whole free space from `fill`. If the endpoint refuses event mode it falls
    back to 5 ms polling with a 40 ms buffer. Same code on x64 and ARM64.
  - Reporting: the host sends `host_audio_report.playback` (`AudioPlaybackStats`,
    additive optional field) at most every 10 s and only when a counter moved or the
    stream appeared or went away. The service stores it in `SeatRuntime.audio_playback`
    (cleared on seat stop) and logs new underruns / drops. The manager status page shows
    "Sound dropouts" on the seat row once a counter is above 0.
  - HYPOTHESIS: about 60 ms playback latency (30 ms ring + 20 ms buffer + engine
    period) against about 70 ms in OSMS, and a 30 ms target that covers rdpsnd's packet
    cadence over loopback. Nothing here has run on hardware with an audio endpoint.
  - No device state changes: nothing to journal or restore.
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
  and audio is off. Printers (round 4 local-gaps, opt-in `SeatRedirect.printers` =
  `off` / `seat` / `all`): the service resolves `seat` to the queues of printers whose
  effective mask is exactly this seat (`seat_media::printer_queues_for_device`, USB port
  match) into `HostRedirection.printers`, `all` into `all_printers`; the host builds the
  rdpdr product of IronRDP's Windows back-end with `WindowsPrinterSelection::Named` /
  `All` (MS-RDPEPC announce with the queue's own driver, jobs written with
  `StartDocPrinter` / `WritePrinter` under the host token), announces them with
  `Rdpdr::with_redirected_printer`, and `SeatRdpdr` refuses printer IRPs for any other
  device id (`printer_not_redirected`). The `printer_acl` gate gives the seat's own host
  account the use right on the seat's queues (`GatePlan.printer_hosts`). ActiveX:
  `printers_need_ironrdp`. Changes apply live on the open channel (rdp-listener
  2026-11-17, see *Drive broker and live device changes* below). Untested on hardware;
  HYPOTHESES: printing from a Low-integrity batch token works. **Spec deviation, not yet
  confirmed:** spec/local/70 §3.8 says the product does nothing with printers; this
  opt-in (default `off`) waits for the analyst to record the maintainer's opt-in there,
  or for the maintainer to confirm it (status.md, review 2026-11-14). Ports: never. Refusals: `channels::RefusalLog` (stderr,
  once per 10 s per reason). The clipboard bridge is the host session's clipboard (shared
  with seat 1 and every seat that has it on: note `clipboard_shared_with_host_session`).
- **Drive broker and live device changes (2026-11-17, rdp-listener).**
  - *Why a broker.* The seat host's token is contained (batch logon of its host account or
    SYSTEM, Low integrity, no privileges): it cannot impersonate the seat user, and its own
    rights on an ACL'd volume are not the user's. The seat session's agent
    (`pulsar-seat-agent`) already runs with the seat user's token, so it serves those
    volumes and Windows checks the user's own rights on every open.
  - *Link.* The host asks `host_drive_broker {seat}` (Access::Host) when an ACL'd drive is
    assigned. The service needs the seat's host, session and agent; it computes the seat's
    redirection again, keeps the ACL'd drives as the **allowlist**, makes two anonymous
    pipes (`Platform::broker_pipes`: `CreatePipe` twice, one per direction, because
    synchronous handles serialise I/O per file object; each end moved into the host or the
    agent with `DuplicateHandle(DUPLICATE_CLOSE_SOURCE)`), queues the agent task
    `drive_broker {read, write, drives}` and answers `drive_broker {read, write}`. No pipe
    name exists, so nothing can be squatted or opened by another process. `not_ready` while
    the seat has no agent (no user signed in); the host retries every 5 s.
  - *Protocol* (`seat_rdp::channels::broker::Frame`, u32 LE length + kind byte): host to
    broker `activate {device_id, read_only, root, volume}`, `deactivate`, `irp` (the
    `DR_DEVICE_IOREQUEST` re-encoded from IronRDP's decoded request), `reset`; broker to
    host `pdu` (an encoded `DR_DEVICE_IOCOMPLETION`), `refused {device_id, reason}`,
    `deactivated`. The broker refuses a root not on the allowlist (`drive_not_allowed`), a
    letter that names another volume (`drive_letter_moved`) and keeps the allowlist's
    read-only. The host passes on only I/O completions of the device ids it gave the
    broker (`drive_broker_answer_dropped` otherwise). The broker runs IronRDP's Windows
    drive back-end per drive in the agent (`serve_as_user`). The host waits up to 50 ms
    for an IRP's completion, later ones go out with the channel ticker (20 ms).
  - *Live changes* (`SeatRdpdr::live_changes`, applied by `channels::apply_live_changes`):
    removed drives / printers leave (`Rdpdr::remove_drive`, `Rdpdr::remove_printer`), new
    FAT drives get a back-end of their own (`channels::win::drive_maker`), new ACL'd drives
    join once the broker is linked, new printers are discovered once per live revision
    (`printer_maker`) and served by a back-end per batch whose own device ids are mapped to
    the session's (IRP and completion device id rewritten). New devices are announced with
    a `ClientDeviceListAnnounce` of only the new device (MS-RDPEFS 3.2.5.1.9:
    `Rdpdr::add_dynamic_drive`, and Pulsar's `Rdpdr::add_dynamic_printer`). Live device ids:
    drives from `0x3000_0000`, printers from `0x4000_0000`. A drive that fails its check is
    not offered again for 10 s. The rdpdr channel and its drive / printer capabilities are
    joined whenever the drive / printer policy is on (`HostRedirection.drives_on` /
    `printers_on`), so a connection that started with nothing still takes devices later; a
    policy switched on later and smart cards still need the next connection (static
    channels and the pre-logon smart-card device are set up at connect).
  - *Vendored IronRDP rdpdr.* `third_party/ironrdp-rdpdr` (0.9.0, MIT OR Apache-2.0) adds
    `with_printer_capability`, `add_dynamic_printer` and `remove_printer`
    (`third_party/ironrdp-rdpdr/PATCHES.md`); the root `Cargo.toml` patches crates.io with
    it and excludes it from the workspace.
  - **Untested on hardware** (HYPOTHESES: a Low-integrity host can use pipe ends the
    service duplicated into it; Windows servers accept device announcements after logon
    on an open channel and printers announced late; the agent's token opens the user's
    files with the user's rights).
- **Redirection hardening (2026-11-13, fix-rdp-channels).** Single source of truth:
  `seat_rdp::policy` (the old `seat_media::redirect::seat_redirection` rules are gone;
  `seat_media::redirect` keeps only `seat_cameras`).
  - *Drives.* The objection "drive redirection adds an ungated `\\tsclient\X` path" is
    answered by gating that path with the same ownership: only volumes whose effective mask
    is exactly the seat; each `RedirectDrive` carries the volume GUID path its letter named
    at resolve time (`volume`, serde default), re-checked with
    `GetVolumeNameForVolumeMountPointW` right before the root is opened on every connection
    and channel reset (`drive_letter_moved`; empty = `drive_volume_unknown`); open root
    handles stay bound to their volume. FAT / FAT32 / exFAT volumes are served by the seat
    host itself (no ACLs: the host and the seat user have the same access). Volumes with
    ACLs (NTFS, ReFS, unknown) are marked `RedirectDrive.acl` and served **as the seat
    user** by the drive broker (below); before 2026-11-17 they were refused
    (`drive_acl_volume_refused`, now only when a FAT volume turned ACL'd after resolve).
    Live updates: `Reconfigure` re-reads the assignment into `channels::LiveRedirection`;
    the channel ticker applies it (`SeatRdpdr::live_changes`, below).
  - *Smart cards.* Readers are matched by device instance id
    (`SCardGetReaderDeviceInstanceIdW`); a known id never falls back to the name. The name
    is used only without an id, when the service marked it unique on the PC
    (`SmartCardReaderRef.name_unique`, serde default false) and only one present reader
    carries it; otherwise `reader_name_ambiguous` (two identical models on two seats are
    never told apart by name). `ListReaders` honours the requested groups. The PnP pseudo
    reader's count (high word) is translated to the PC count before the wait and rewritten
    to the filtered count in the reply (encoded `DR_CONTROL_RSP` patched in place), so a
    seat never learns other seats' reader count. Reader refs follow `LiveRedirection`.
    `ListReaderGroups` (rdp-listener 2026-11-17) is answered by the host too: only groups
    that hold at least one of the seat's readers (`channels::rdpdr::seat_groups`).
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
- *AVC444 on the GPU path, vendor MFTs (2026-11-17).* AVC444 / AVC444v2 surfaces stay
  on the zero-copy path when the compositor has the AVC444 shaders
  (`GpuCompositor::supports_avc444`): promotion only at an IDR with both views covering
  the whole surface; AVC420 and AVC444 on one GPU surface demote it. Each view is copied
  into a shader-readable NV12 texture and merged by a compute shader into an `R32_UINT`
  4:4:4 picture; a pixel shader converts the painted rectangles (reverse chroma filter +
  MS-RDPEGFX transform) into the surface texture (`gpu::avc444`, rules in
  `gpu::avc444_model`, tested against IronRDP; HLSL compiled at run time with the in-box
  `d3dcompiler_47.dll`, tested on WARP). H.264 decoder ladder: the GPU vendor's
  asynchronous hardware MFT (`MFTEnumEx` HARDWARE | ASYNCMFT, `MF_TRANSFORM_ASYNC_UNLOCK`,
  event-driven), then Microsoft's MFT with D3D11, then the CPU; failures step down one
  level (`h264::next_decoder`); `PULSAR_SEAT_H264_DECODER=microsoft|software`.
  `DecoderKind::HardwareVendor`.
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
  async, `stop_seat`, `relogon_seat`, `switch_seat{seat}` (2026-11-15, protocol 3, admin,
  `fast_user_switch` only: console to a running seat or to seat 1's own session; no stored
  password, the logon screen it opens preselects the seat's tile), `request_reboot`, `resolve_input_move{accept}`:
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
- Credential provider (`credprov/`): x64 DLL in the x64 package, **ARM64X** DLL in the
  ARM64 package (parity-arm64, 2026-11-17; `BuildAsX`, checked by `build-credprov.ps1` and
  `check-payload.ts`): native LogonUI uses the ARM64 half, x64 processes under emulation
  that enumerate providers (CredUI hosts) load the ARM64EC half, where the provider is inert
  (not LogonUI, `CPUS_CREDUI` refused). HYPOTHESIS: never loaded by an emulated host here.
- Per-process shim (`shim/`, parity-arm64): native ARM64 module with the vendored Microsoft
  Detours backend (`shim/module/src/rt/backend.rs`, `shim/third_party/detours`, MIT, SHA-256
  pinned in `build.rs`); x86 / x64 keep `retour`. The ARM64 MSI ships `shim\arm64\` (module,
  launcher the service calls, shimctl), `shim\x64\` (x64 apps under emulation) and
  `shim\x86\`. AppHelp: the `System32` copy is the ARM64 build, `SysWOW64` the x86 one; x64
  apps on ARM64 are covered by the launcher path only (an x64 process cannot load the ARM64
  `System32` copy, so the x64 launcher there adds no AppHelp layer). Verified here: the real
  Detours hook of `ntdll!NtCreateFile` in a test process and the launcher hand-offs
  arm64 <-> x64 / x86 (console session, hooks off). HYPOTHESIS: every hook in a seat;
  ARM64EC apps take the x64 module (their images carry the x64 machine).
- **VDD on ARM64** (parity-arm64 decision, 2026-11-17): the ARM64 Virtual-Display-Driver
  package stays **opt-in** (`default_selected: false` in `installer/remote/drivers.json`,
  test `vdd_default_is_x64_only` in seat-setup). Facts behind it: (1) the package is signed
  by SignPath Foundation, a public CA, not by the Microsoft hardware publisher; (2) upstream
  states that ARM64 on 24H2+ may need test-signing (Split OQ 24); (3) our rules forbid
  test-signing and require Memory Integrity + Secure Boot on; (4) a fresh install defaults
  to `PULSARDRIVERS=none` on both arches anyway, and remote seats default to the RDP session
  display, which needs no driver. x64 keeps VDD as the default selection. Flip the ARM64
  flag only after the ARM64 laptop shows the package installing and the monitor arriving
  with Memory Integrity and Secure Boot on (HYPOTHESIS until then: it would need
  test-signing there). Operators can opt in with `PULSARDRIVERS=vdd`.

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
cd shim && cargo test && cargo test --target x86_64-pc-windows-msvc   (ARM64 natively with Detours; x64 under emulation)
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
  The display helper (`--display-helper exec Identify`) spawns its overlay child with
  non-inheritable std handles, and the service's console runner waits at most 3 s for
  EOF after the reply (`helper_output_open`): a detached child holding the reply pipe
  hung `wizard_start` before (2c1e9ef).
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
  = WebView `getUserMedia` preview in the manager: one camera (device menu), every camera at
  once (system menu) or the cameras a seat uses (seat menu; `cameras.ts`,
  `CameraGrid.svelte`, review 2026-11-17). Help links (Settings > Help: manual, website,
  issue tracker, security report) open fixed URLs through the shell (`open_help_link`,
  `shell::HelpLink`); the UI never passes a URL, and they are disabled with the reason while
  the WebView is offline. Path fields of the VM settings use `pick_path` (image, language
  pack, folder).

## 10. Seat kinds (merge contract delta, steps 1 + 4, 2026-11-14)

Joint contract for the merge with Pulsar Split ([merge-plan.md](merge-plan.md), maintainer
decisions §5). Step 4 put it into `seat-core` / `seat-proto`; the remote seat worker that
serves it is in `seat-service` since step 6 (§10.5). Split's old contract stays in
[../split/agents/contracts.md](../split/agents/contracts.md) for the packages.

### 10.1 Config document (`seat_core::config`, `CONFIG_SCHEMA_VERSION` = 3)

```text
SeatConfig {
  number, name, autostart, kind: local | remote,
  account,                                   // the seat user; password in the SecretStore
  cpu_sets, cpu_hard_limit, wallpaper, ip_binding, startup_apps,   // common
  environment,                               // per-seat user environment variables
  deny_local_logon,                          // the seat user may not sign in at the console
  local:  Option<LocalSeat>,                 // set iff kind == local
  remote: Option<RemoteSeat>,                // set iff kind == remote
}
LocalSeat  { redirect, kiosk, kiosk_shell }  // physical seat only (seat host, desktops backend)
RemoteSeat { id, display_name, port, stop_action, display, apps, rdp_client_name, audio,
             stream, stream_backend, last_mode,      // Split's per-seat record minus name / user
             pulsar,                                 // Pulsar backend options (relay, node port, ...)
             vm }                                    // Some = Hyper-V VM seat (VM section below)
DisplaySettings (remote) { ..., fixed_width, fixed_height, lock_refresh }
GlobalSettings.remote: RemoteSettings        // Split's old GlobalSettings (hid_isolation, ..., vm)
GlobalSettings.apps                          // per-app compatibility entries
GlobalSettings.tweaks { ..., fast_startup_off, windows_update, device_install_deny,
                        defender_exclusions, inactivity_lock_off, users_shutdown_right_off }
GlobalSettings.console_guard { host_lock_off, seat_one_lock_off }   // 2026-11-18, G4
SeatConfig.no_power_menu                                            // 2026-11-18, G4
```

- Migration 1 -> 2 (`Config::from_json`): every seat without `kind` becomes `kind = local`
  and its `redirect`, `kiosk`, `kiosk_shell` move into `local`. Split's registry config is
  **not** imported (decision §5: no migration of installs, no HKLM / DPAPI importer).
- Migration 2 -> 3: additive (VM seats, Pulsar options, compat tweaks); details in the
  "Hyper-V VM seats" section below.
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
config requests (the seat shape changed), every remote-kind request, `switch_seat` and the
requests the parity rounds added (`reset_graphics`, `steam_proxy_dll`, `vm_seats`,
`restart_term_service`, `host_audio_report`, `host_drive_broker`; pinned by a seat-proto
test since the review of 2026-11-17); the service answers `version_mismatch` when the
caller's hello version is lower. `get_status` carries `kind`
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
  `RDP-Tcp\SecurityLayer=2`, `IgnoreRemoteKeyboardLayout=1`, policy `Shadow=0` (only
  while absent); common policy `bEnumerateHWBeforeSW=1` (only while absent; was remote only
  before OSMS parity G3); local computed `WinStations\DWMFRAMEINTERVAL` (see "Session frame
  rate" below). Split's `Originals` key is gone.
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
- **External wrapper mode** (detect-only, 2026-11-24). `WrapperKind::External { product }`
  = a wrapper another installed product set up itself; today only
  `%ProgramFiles%\Duo\TermWrap.dll` (discovery location; the raw ServiceDll is expanded,
  then must equal that location, so `D:\Duo\TermWrap.dll`, UNC, `..` and unknown `%VAR%`
  stay `ForeignActive`). When ServiceDll points there, `status()` reports
  `TermwrapState::External { wrapper, blocked }` instead of `ForeignActive`. `blocked` is
  the first failing read-only check: host (`reference_product_present` only while the
  reference product is active, see "Coexistence: reference product installed but disabled";
  `not_client_sku`,
  `edition_unsupported`), x64 host + PE machine (`wrapper_arch`), Authenticode
  (`wrapper_unsigned`; `allow_unsigned` policy as for our own mode), ACL of the DLL and
  every ancestor (`wrapper_insecure_acl`), `EnableConcurrentSessions` and
  `AllowMultipleTSSessions` both already `1` (`concurrent_sessions_off`). No `termsrv.dll`
  build gate (the product has no public support table; a broken wrapper shows up as seats
  without a session). `TermwrapState::wrapper_usable()` = `On` or `External { blocked: None }`.
  Writes: none. `enable` refuses (`external_wrapper`, also for the external DLL as the
  target while ServiceDll is stock), `disable` answers `foreign_wrapper`, `boot_check` and
  `restore_all` leave it alone, nothing is journaled. Backend `termsrv_wrapper`: probe =
  RDP readiness with note `wrapper_managed_externally` (or `unavailable{<blocked code>}`);
  `prepare_host` (ack still required) claims only the documented RDP rows (`Claim::Local`)
  and adds note `termsrv_wrapper_external`; `restore_host` releases those rows only. An
  installed but inactive external wrapper with no other candidate gives
  `unavailable{external_wrapper_inactive}` and is never turned on. Remote seats still look
  at `On` only (`seat-service` remote engine `wrapper_active`): their ack is stored apart,
  so using an external wrapper there is a separate decision. Manager: state `external`,
  reason codes in `en.ts` / `tr.ts`.

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
- **Microphone (remote, round 4, 2026-11-14; spec/remote 70 §5.1).** Pulsar seats only:
  `pulsar-host` mic pipe (`\\.\pipe\PulsarSplit\seat\<id>\pulsar-host-mic`, additive
  contract §3.8 in `docs/split/agents/pulsar-host-backend.md`) -> `seat_stream::backend::
  pulsar::mic::MicLink` (reader thread) -> `SessionManager::send_mic` -> the hold's stdin
  (after the password frame, bounded queue) -> `seat_rdp::headless::mic::HoldMic` ->
  MS-RDPEAI `AUDIO_INPUT` -> the session's capture endpoint. Codec
  `seat_core::remote::mic`. Sunshine seats never set `INFO_AUDIOCAPTURE` (Moonlight has no
  mic) and list `mic_needs_pulsar`. Status `seat_runtime.stream.mic`. Needs a pulsar-desktop
  change (status.md request). HYPOTHESIS end to end (no host, no RDP target ran).
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
  lightbar rebuilt into the physical pad's report; Bluetooth pads get it rewrapped with
  its CRC (2026-11-17). Every HID pad is read since 2026-11-17 (see the next bullet). The
  hub shuts down first in `stop_with`. Limits: ViGEm only if already
  installed; DualSense via usbip-win2; the own pad driver is unsigned and only used on
  explicit preference. Since 2026-11-17 the physical pad is scoped to its seat
  through its device nodes (next bullet). HYPOTHESIS: every Windows path behind the hub
  (nothing ran on hardware). Details: `crates/seat-pads/README.md`.
- **Pads and pen parity (2026-11-17, package pads-pen; HYPOTHESIS end to end).**
  - *Reading.* `RoutedPad::layout`: PlayStation (public layouts, USB + Bluetooth), Switch
    Pro (`seat_pads::switch_pro`, init sequence at open), every other HID pad through its
    report descriptor (`HidP_*` value / button caps in `seat_input::win::hid`, mapped by
    `seat_pads::generic`: XInput-compatible view of Xbox pads with the combined Z
    trigger, Xbox Bluetooth view, standard view). `RoutedPad::bus` from the unit's bus.
  - *Feedback.* `LocalFeedbackRouter` per layout: PlayStation USB / Bluetooth
    (`seat_pads::bt`: 0x31 / 0x11 with CRC-32, seed 0xA2), Switch Pro rumble 0x10 +
    player lights, Xbox Bluetooth 0x03 (rumble + impulse triggers). Xbox USB and generic
    HID pads have no documented HID output report: no feedback.
  - *Scoping* (`seat_input::pad_scope`, `crate::pad_runtime`, setting
    `input.pad_device_scope`, default **off** since the parity review 2026-11-17: opt-in,
    spec/remote 50 §4.4). Owner of each physical pad: the local seat it is
    routed to alone (session + user), else, while remote seats run, the console user;
    only while a second session exists. Nodes: the HID collection and its parent (since
    OSMS parity G5: the whole pad subtree found by class, see "Physical pad pass-through
    and Guide-button guard" below).
    Original SDDL and session id are journaled (`<data>\pads\scope.json`) **before** a
    change; restore at service start, stop and when the setting goes off. Device calls:
    `seat_pads::win::devtree` (`SPDRP_SECURITY_SDS`, `DEVPKEY_Device_SessionId`,
    `DIF_PROPERTYCHANGE`). This is the "physical console pad scoping" of spec/remote 60
    for processes without the shim.
  - *XInput per seat.* `crate::local_pads::local_seat_map` publishes shim seat map
    entries for local seats with their own session (seat id `5053454c-4f43-4100-8000-<n>`,
    `PadInstances` = routed HID units + pad parents), written by `pad_runtime` to the same
    volatile map (keys are session ids, so no clash with remote entries; republished every
    30 s because the remote engine clears the map at its start). Shim x64 / x86 only.
  - *SDL hints.* `SeatConfig::pad_ignore_devices` (both kinds, `0xVVVV/0xPPPP`, validation
    `invalid_pad_ignore_device`) plus, for local seats, the ids of physical pads routed
    elsewhere that no pad of the seat shares, as `SDL_GAMECONTROLLER_IGNORE_DEVICES` and
    `SDL_JOYSTICK_IGNORE_DEVICES` in every process the service starts in the seat (startup
    apps, launcher apps, remote autostart / client-launched apps).
  - *Controller audio (remote).* Shared-memory protocol 2 media section per pad
    (`seat_pads::media`, contract in `docs/split/agents/pulsar-host-backend.md` §5):
    speaker / headset / haptics PCM out, microphone PCM in; `pad.attach.media`. Remote
    seats' DualSense pads get the audio interfaces only with `RemoteSettings::pad_audio`
    (default off). Sunshine seats stay HID-only (Moonlight has no controller audio).
  - *Pen.* `RawContact` / wire `PenInfo` (tilt X / Y, rotation from twist or azimuth,
    barrel, eraser, invert; HID digitizer usages 0x3D, 0x3E, 0x41, 0x3F, 0x44 / 0x5A,
    0x45, 0x3C). Hosted seats: MS-RDPEI `RDPINPUT_PEN_CONTACT` optional fields
    (`seat_rdp::input`); hosted digitizers are no longer marked not routable. Kiosk
    seats: `KioskAction::Pen` over the seat's own app windows (capture from pen down),
    injected by the shell as `PT_PEN` (`seat-host/src/kiosk/pen_win.rs`,
    `CreateSyntheticPointerDevice` / `InjectSyntheticPointerInput`); without a synthetic
    device the pen keeps emulating the left button. The
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
  the remote seat list with avatars (the seat user's Windows account picture on each row and
  in the seat page head, generic fallback; restored after redesign P6 by the review of
  2026-11-17), the seat editor (`SeatPage`, GPU warnings, apps,
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
  Pulsar pairing events (`otp_changed`, `trust_changed`, `relay_auth_required`) are on the
  `stream` topic too (since the merge review, ddc1935): the pair dialog re-reads the OTP and
  the pairing state on them, as Split's did (Split's host page never reacted to events).
- **Service buttons (restored, round 4, 2026-11-14):** start / stop / restart of the one
  `PulsarSeat` service through the SCM, in the manager header (`ServiceControl.svelte`,
  Tauri commands `service_state` / `service_control`, `src-tauri/src/scm.rs`, windows-sys
  only). The SCM does the access check, so they work in the elevated manager; an
  unelevated one shows them disabled with the reason. Stop and restart ask first (every
  seat ends). The manager never changes the service's configuration or start type.
- **Gone with Split's Tauri crate:** the
  `tauri-plugin-dialog` file picker (the pages use Seat's `pick_program`). Kept from it: the
  LAN address order for the pair dialog (`net::order_lan`, Tauri command `lan_addresses`).
- **Copy.** One `i18n.ts` (locale tables in `locales/en.ts` / `tr.ts` since 2026-11-17): Split's copy is its `rt` object plus `remoteErrorMessage`,
  `remoteFailureText`, `remoteFieldErrorText`; the remote components import those under
  their old names. Split's styles are `remote/remote.css`, scoped to `.remote-ui`.

## Parity review fixes (2026-11-17)

Contract and behaviour changes of the parity review round (all built and unit-tested,
nothing ran on hardware; HYPOTHESIS where it depends on Windows behaviour):

- **Pads.** A kiosk seat's virtual and scoped pads belong to the seat's own Windows user
  (`local_pads::KioskSids` from `SessionBackend::kiosk_users`). Pad scoping is opt-in
  (`input.pad_device_scope` default off), retries failed nodes with back-off and logs a
  repeated error once (`PadScoper::sync_at`). `pad.attach.media` is true only when the
  pad has audio endpoints (`VirtualPad::has_audio`). Remote seats' SDL ignore list gets
  the PC's physical pad ids (`Engine::set_physical_pads`, virtual pad kinds' ids stay
  visible). Rumble for Xbox USB (XUSB) and HID PID pads, Switch Pro SPI calibration
  (`seat_pads::ff`, `switch_pro::StickCal`).
- **Kiosk seats.** Seat section `Local\PulsarSeat.KioskCursor.<NN>` v2 (360 bytes: cursor,
  clip, 256-byte key state, app request block, shell thread id); per-seat clipboard
  `Local\PulsarSeat.KioskClip.<NN>` (32 MiB, `seat_shim_core::kiosk_clip`); module hooks
  in `shim/module/src/rt/{cursor,clip}.rs`. Logical cursor APIs convert physical pixels for
  threads that are not per-monitor DPI aware. Probe notes `kiosk_users_share_desktop`
  (always) and `no_per_user_isolation` (a seat 2+ runs as the console user); the manager
  warns when a seat's own (personal / domain) account is used. Unused kiosk accounts are
  disabled at service start (`host_account::disable_unused_kiosk`), deleted at uninstall
  (profiles only with `REMOVEUSERDATA`) or by `remove_kiosk_user` (admin, refused while
  the seat has a kiosk shell). The hotkey guard resolves a raw-first key queued behind a
  held-back hotkey.
- **Requests (need protocol 3, admin):** `reset_graphics { seat }` (agent task
  `reset_graphics`: Win+Ctrl+Shift+B into the session's input desktop; HYPOTHESIS that
  injected input triggers it), `remove_kiosk_user { seat }`, `restart_term_service
  { confirm }` (rdp-listener 2026-11-17: every running seat but seat 1 stopped and signed
  out first, local seats on the request thread and remote seats through their worker; then
  `SessionBackend::restart_termservice` (rds_loopback / termsrv_wrapper:
  `LoopbackListener::operator_restart`, journaled, no "no user session" rule) and the
  second listener re-checked; answer `term_service_restart {stopped_seats, restarted,
  reason}`). Host requests (Access::Host, additive): `host_audio_report {seat,
  render_open, capture_open, notes, playback?}` (the bridge notes go to
  `SeatRuntime.audio_notes`; `playback` (2026-11-20, G7) is the jitter-buffer counters,
  stored in `SeatRuntime.audio_playback`;
  seat runtime `degraded` now says `audio_no_endpoint` / `audio_not_bound` /
  `audio_service_not_running` for an RDP-bridge seat without a playback endpoint instead of
  `audio_console_shared`), `host_drive_broker {seat}` (above). The credential test
  (`test_seat_credentials`) uses an interactive logon first (spec/remote 10 §4.4), a
  network logon only when the interactive logon right is denied (1385).
- **Shim.** On ARM64 PCs x64 executables with a known path stay out of the AppHelp
  database (`install::apphelp_targets`, PE machine check); launcher path only. Focus WNF
  filter: runtime check of the candidate states (`decide::wnf_runtime_plausible`,
  `NtQueryWnfStateData`, off switch `Shim\WnfRuntimeCheck` = 0). Listing
  `GameInputSvc.exe` as a target sets `Shim\GameInputFocus` = 1: the module then runs in
  that session-0 service with the focus hook only (`decide::filter_focus_service`).
  Smart-card readers: `Shim\ReaderRules` (`seat_shim_core::readers`) published by the
  service from the assignment; `SCardListReaders` / `SCardConnect` filtered in every
  patched process; gate status `seat_filter`. RDP seats redirect a reader assigned to them
  alone even with the smart-card switch off.
- **Cameras.** `Shim\CameraRules` (same format as the reader rules, rule name = device
  instance id) published from the assignment of webcams that are not shared; the module's
  HID open / enumeration hooks also hide and refuse camera interface paths
  (`KSCATEGORY_VIDEO_CAMERA` / `CAPTURE` / `VIDEO`) of cameras of other seats in every
  patched process (console processes get the HID groups while camera rules exist). The
  frame server opens cameras from session 0, so the per-user camera consent
  (`camera_consent` gate) stays the second layer; WinRT enumeration through the broker is
  not covered (HYPOTHESIS: never run).
- **Hyper-V helper.** Request `hyper_v { action }` (admin; `seat_proto::hyperv`):
  `status`, `enable_feature { confirm }` (`Enable-WindowsOptionalFeature … -NoRestart`),
  `set_seat_nic { seat, address, prefix_len }` (internal switch "Pulsar Seat" + host
  adapter "Pulsar Seat NN" + the address, `seat_compat::hyperv`), `remove_seat_nic`.
  Adapters are journaled (`journal\hyperv.json`) before creation and removed at uninstall;
  the feature stays. Manager: Network tab section. HYPOTHESIS: never run.
- **Remote stream options.** `GamepadPref::{PulsarDs4, PulsarDs5, PulsarDs5e, PulsarXone}`
  and `CapturePref::PulsarIdd` reach the Pulsar stream-server build's keys
  (`gamepad = pulsar_*`, `capture = pulsar_idd`); a stock server falls back to `auto` /
  its default. The patch set exists since the stream-server round (below).

## Stream server patch set (Pulsar build, 2026-11-17)

- **Source of truth:** `stream-server/sunshine-patches/0001-0006` against Sunshine
  `v2026.516.143833` (`14ffa6fd`), pins in `stream-server/sunshine.json`, build
  `stream-server/build-sunshine.ps1` (MSYS2 UCRT64 / CLANGARM64; portable MSYS2 under
  `target\wf\msys2`), CI job `stream-server`. Details: `stream-server/README.md`.
- **Wire:** seat pipe protocol 1 as spec 80 §8.4 (both directions, `hello` first) plus the
  shared events of the service (`display.mode.request`, `pad.wanted`, `pad.released`,
  `audio.state`); frame channel = spec 40 §6.4 (`PulsarIddShared.h`); pad channel =
  `seat_pads::layout` protocol 1.
- **Capability gating (service, `seat_stream::supervise`):** `hello` capabilities decide what
  the service uses. `SeatServerConfig::conf_for_caps` rebuilds `server.conf` for the
  reported features (`pulsar_idd`, `pulsar_pad`, `pair_timeout`; missing ones fall back to
  the stock capture / `auto` / no key) and restarts once (`StreamNotice::Fallback`, event
  `host_warning` `stream_fallback`); `upstream_conf_text` drives a Pulsar build that never
  opens its seat pipe like upstream (loopback web UI, per-start credentials) instead of a
  crash loop; `apps_reload` / `pair_cancel` missing from a listed capability set = upstream
  paths; a `bad_request "unknown request type"` answer maps to `Unsupported`.
- **Pulsar backend options:** `RemoteSeatRecord::pulsar` (`PulsarSeatOptions`: relay,
  network mode, node port base / ephemeral / fixed, port mapping, LAN discovery, access,
  max clients) → `config.apply`; validated in `seat_core::remote::validate` (relay syntax,
  fixed port 1024-65535 and outside other seats' blocks, 1-4 clients); manager seat page
  "Pulsar connection". LAN discovery: per-seat rule `PulsarSplit seat <id> discovery`
  (UDP 21117, `pulsar-host.exe`, local subnet, Private [+ Domain]) created with the option,
  removed when it is off, when the seat switches to Sunshine and with the seat.
- **CPU.** See §4 "CPU limits": Explorer in the seat job, opt-in `cpu_seat_wide`.
- **Display.** IddCx frame source for seats without a session display (see HDR section).
- **Network.** `seat-wfp` remembers explicitly bound endpoints (bind classify, endpoint
  handle + address, 1024 direct-mapped slots) and the connect backstop leaves them alone.
- **H.264 (vendor MFT).** `flush()` zeroes both event counters and sends
  `MFT_MESSAGE_NOTIFY_START_OF_STREAM`; `Drop` shuts the async MFT down (`IMFShutdown`)
  and the activation object (`ShutdownObject`).
- **Drivers' INF copy:** English `[Strings]` + Turkish `[Strings.041f]`
  (`installer/scripts/driver-inf-locales.test.ts`).

## Hyper-V VM seats (hyperv-seats, 2026-11-17)

Spec: `spec/remote/10-manager.md` §8 (sandbox seats, shared folders, forwarded ports,
storage, reset), §3 hidden sandbox values, `20-service.md` (`Sandboxed`, `SandboxMemory`,
`HideVMSuffix`, `.vmconf`), `90-install.md` §2.4 (optional features). Our design, built and
unit-tested with fakes; nothing ran on a Hyper-V host (HYPOTHESIS throughout).

- **Config (schema 3).** A VM seat is a remote seat with `remote.vm: VmSeat` (memory,
  processors, GPU partition `off | auto | adapter`, shared folders, forwarded ports, stop
  mode `shut_down | save | turn_off`, `hide_vm_suffix`, service-assigned `guest_address`).
  Machine settings `settings.remote.vm: VmSettings` (storage folder, memory share %,
  base disk size, image `none | wim | vhdx` + path / index / language CAB, GPU restart on
  stop, NAT prefix, default `172.31.240.0/24`). `CONFIG_SCHEMA_VERSION` = 3; migration
  2 -> 3 drops a stray `vm` from schema-2 documents (only pre-release builds wrote one). The
  bump keeps a schema-2 build from silently running a VM seat as an RDP seat.
  `seat_core::remote::assign_vm_addresses` (in `commit_config`) gives each VM seat
  network + 10 + seat number.
- **Validation** (`seat_core::remote::vm`, hooked into Split's `seat_alone` /
  `seat_against` / `global`): spec mount point rule (drive letter other than `C:`, or a
  `C:\` path with a folder and no trailing backslash), absolute local host paths without
  quotes / `$` / `..`, forwarded host ports outside every remote seat's stream block and
  unique per protocol, memory 0 or >= 2048 MB, `save` refused with a GPU partition (Hyper-V
  cannot save such a VM), settings ranges and prefix. Issues reuse `remote_seat_invalid`
  with `field = remote.vm.*` / `settings.remote.vm.*` and the reason. VM seats need no
  concurrent host sessions: no `session_unavailable` warning for them.
- **Service** (`seat-service/src/vm_seats.rs`). The remote seat worker never sees VM seats
  (`DocStore::load` leaves them out); `glue.rs` routes them: `start_seats`, autostart,
  resume -> `start_vm_seat`; `stop_seat`, service stop, suspend -> `stop_vm_seat`;
  `apply_config` delete -> refused while active, else `VmOp::Remove`. Start checks, in this
  order (first refusal answers): Hyper-V feature + `vmms` (`hyperv_unavailable`), network
  (`vm_network_missing`), base disk (`vm_base_missing`), id / address (`vm_not_assigned`),
  chosen GPU partitionable (`vm_gpu_missing`), user + stored password
  (`missing_credentials`). Then `VmOp::Ensure` (the seat's own share account password = a
  fresh random token, on the script's stdin), guest config offered on the guest link,
  `VmOp::Start`. While Ensure / Start run the seat is marked `starting`, and a status read
  that began before Start returned never takes the VM's "off" for a finished stop (review
  2026-11-17: a concurrent poll used to drop the new seat).
  The tick polls the VM states every 2 s while a VM seat is active (off the tick thread) and
  maps VM state + guest phase to the seat runtime (`vm_seat_state`: running + `streaming`
  / `signed_in` = `signed_in{user}`, `streaming` adds `degraded: ["streaming"]`, guest
  `failed` = `error{internal, detail}`, off / saved = off). `restart_gpus_on_shutdown`
  restarts the display adapters after a GPU-P VM stopped.
- **Seat page requests** (review 2026-11-17, full parity with host remote seats; `glue.rs`
  `vm_kind_request`). Kept on the host and handed to the guest with every config (and at
  once to a running guest): the stream app list (`set_apps` / `get_apps`;
  `<data>\vm-seats\<id>\apps.json`, defaults when unset), the Pulsar connect password
  (host secret store, slot `connect_password`), a pending identity reset
  (`regenerate_seat_identity`, seat off only, done once per start by `start_nonce`).
  Forwarded to the guest (`GuestMsg::Request` with the protocol 3 `RequestBody`, answered
  by `GuestMsg::Reply` with the `ResponseBody` or the error, 15 s): `cancel_pair`,
  `get_pair_state`, `list_paired_clients`, `unpair_client`, `unpair_all`,
  `answer_peer_request`, `get_seat_otp`, `rotate_seat_otp`, `answer_relay_auth`,
  `get_remote_runtime`, `get_log_tail` (`seat:<N>`), `set_seat_display_mode`,
  `test_seat_credentials`, `enable_unlock_account`. `submit_pin` acks at once (as on the
  host) and the outcome arrives as the `pair_ended` stream event. With the VM off the answers
  match an off host seat (`service_unavailable` for server requests, the default pair state
  and runtime, a credential test that only checks the stored password, enable / unlock
  applied at the next start). The guest's stream notices (`GuestMsg::Notice`) become the
  seat's `stream` events (pair dialog, streaming state).
- **Request** `vm_seats { action }` (needs protocol 3, admin): `status`,
  `prepare_host{confirm}` (internal switch `Pulsar Seat VM`, host address = network + 1,
  NAT `Pulsar Seat VM`, guest link registration), `build_base{confirm}` (background;
  `building` in the status), `reset_base{confirm}` (all VM seats off), `reset_sandbox{seat,
  confirm}` (seat off). Answer `VmHostStatus` (feature, vmms, network, base, building, last
  error, storage, partitionable GPUs, host memory / CPUs, per seat VM state, guest phase).
- **Hyper-V side** (`seat_compat::hyperv_vm`, PowerShell cmdlets only). Base disk from a
  `.wim` / `.esd`: dynamic VHDX of `storage_gb`, GPT with EFI / MSR / Windows partitions,
  `Expand-WindowsImage`, optional `Add-WindowsPackage` (language CAB), `bcdboot`; from a
  `.vhdx`: copied. Then the host's install dir is copied to `C:\Program Files\Pulsar Seat`,
  an unattend file without any account or password skips OOBE, and `SetupComplete.cmd`
  registers the boot task `PulsarSeatGuest` (`pulsar-seat-service vm-guest`, SYSTEM); the
  base disk is made read-only. Per seat: `seats\<id>\sandbox.vhdx` differencing over
  `zygote.vhdx` (host GPU driver packages copied into its `HostDriverStore` when a GPU
  partition is used), Generation 2 VM `PulsarSeat-<id>` (Secure Boot template
  MicrosoftWindows, local key protector + vTPM, Guest Service Interface, static memory,
  checkpoints off, automatic start off), `Add-VMGpuPartitionAdapter` (+ MMIO space),
  `Add-NetNatStaticMapping` for the stream ports (`p-5`, `p`, `p+9..11`, `p+21`; never the
  web UI `p+1`) and the seat's forwards, one inbound firewall rule per protocol (group
  `Pulsar Seat VM`, Private + Domain), SMB shares `PulsarVm<id8>-<n>$` for the host account
  of the seat's own share account `PulsarVm<id8>` (review 2026-11-17; one account per seat,
  so a guest's credentials open only its own shares and a start rotates only its own
  password) with a matching NTFS grant; the shared `PulsarSeatVmShare` of older builds
  loses its grants and is removed at uninstall. Journal `journal\hyperv-vm.json`
  (written before the change); uninstall (`hyperv_vm_uninstall`, before the Hyper-V helper's
  own) removes VMs, disks, base disk, shares + NTFS grants, firewall group, NAT, switch,
  the share accounts (`PulsarVm<id8>`, removed with their seat too) and the registration.
  The Hyper-V feature is never turned on here: that
  stays the Hyper-V helper's visible opt-in (Network tab, reboot notice).
- **Guest link.** Hyper-V sockets (`seat_compat::hvsock`, `AF_HYPERV`), service id
  `6c3c2b9e-5f1a-4f7e-9d2b-8a41c0e7b513` registered under
  `...\Virtualization\GuestCommunicationServices`. Host: `guest_link::GuestHub` + listener
  thread (`HV_GUID_WILDCARD`); a VM gets a config only when the service offered one for its
  VM id (the hypervisor supplies the id with `accept`). JSON lines (`GuestMsg`: `hello`,
  `config`, `state`, `submit_pin`, `unpair_all`, `result`, `shutdown`, and since protocol 2
  `request` / `reply` / `notice`; 64 KiB per line). Each connection has a generation: a
  reconnect replaces the entry and the old reader's late disconnect leaves the new one
  alone. A request waits for the reply with its id (only the asked VM may answer); a
  disconnect fails waiting requests (`vm_guest_unreachable`), silence times out
  (`vm_guest_timeout`). Passwords travel only on the link, in memory.
- **Guest mode** (`win/vm_guest.rs`, `pulsar-seat-service vm-guest`). Connects to
  `HV_GUID_PARENT`, applies the config (`script_guest_apply`: static address, the host's
  DNS servers, seat user created / password set / Users member, shares as global SMB
  mappings with drive letters or symbolic links for `C:\...` mount points; passwords on
  stdin), turns on automatic sign-in of the seat user inside the VM (`AutoAdminLogon`,
  `DefaultUserName`, password as the LSA secret `DefaultPassword`, never in the registry;
  the VM is the seat, so the host rule of never writing Winlogon values for seat users is
  untouched) and restarts once if nobody is signed in. When the seat user owns the console
  session it runs the seat's stream server there with the same `seat_stream` backends as
  host remote seats (`remote::win::wiring::stream_env`), reports `signed_in` / `streaming`.
  A failed start is retried after 5 s, doubling up to 5 min, and `failed` stays reported
  until a start works. It answers the forwarded seat page requests (`vm_guest_rq.rs`,
  tested over a fake stream server), sends the stream notices to the host, applies client
  display mode requests and `set_seat_display_mode` to the VM console through the helper
  `pulsar-seat-service vm-guest-mode W H HZ OUT` (SYSTEM token in the seat session,
  `ChangeDisplaySettingsExW`, SDR only), starts Pulsar client app picks as the seat user,
  and on `shutdown` stops the server, which stays down until the next `config`.
- **Manager.** Remote tab page "VM seats" (`remote/components/VmPage.svelte`,
  `remote/vm.ts`): host state, network / base disk actions behind confirmations, settings,
  per remote seat VM mode with memory, processors, GPU, stop mode, suffix, shared folders,
  forwarded ports, disk reset. A seat page save keeps the VM part (`applyRecord`).
- **Preflight** `hyperv_vm` (info only): Hyper-V present (`Services\vmms` registered) or
  not. Feature presence only; nothing is enabled at install (spec 90 §2.4 Pulsar approach).
- **HYPOTHESIS / unverified:** every script on a real Hyper-V host (client SKUs: Pro /
  Enterprise / Education; ARM64 Hyper-V for ARM64 guests), GPU-P on client Windows (driver
  copy into `HostDriverStore`, MMIO sizes), OOBE skip by the unattend file on current
  builds, SMB global mappings + symbolic links as mount points, NAT forwarding of UDP stream
  ports and whether the host firewall applies to NAT traffic, Hyper-V sockets from a
  LocalSystem boot task, automatic sign-in through the LSA `DefaultPassword` secret, the
  stream server's capture in a VM console session (with and without GPU-P), the display
  mode helper changing the VM console's mode, a non-admin guest user reusing the global
  SMB mapping session.

## UI language (i18n, 2026-11-17)

English is the default UI language everywhere; Turkish is the one extra locale, with the
same keys. Logs, wire codes, `detail` strings and technical messages stay English and are
never translated; the manager maps codes to its own copy.

| Part | Table | Language choice |
| --- | --- | --- |
| Manager web UI | `apps/manager/src/lib/locales/en.ts` (type `Locale`) + `tr.ts`; `i18n.ts` fills the exported tables (`t`, `rt`, `failureText`, ...) with `applyLocale` | stored choice (`localStorage` `pulsar-seat.manager.lang.v1`, Basic settings "Language") else the Windows user UI language (Tauri `ui_language`, fallback `navigator.languages`); a change reloads the window |
| Manager shell (tray menu, window titles, file dialogs) | `apps/manager/src-tauri/src/copy.rs` | OS language at start, then `set_ui_language` from the UI (relabels the tray) |
| Service (Event Log, CLI, install / uninstall console, service + SCM description, account full names / comments, firewall rule descriptions, power request, reboot message, stream default app name, input fallback names) | `seat_core::i18n::Text` pairs next to the code | `PULSAR_SEAT_LANG`, else `GetUserDefaultUILanguage` of the process (LocalSystem: the machine default, HYPOTHESIS) |
| Seat host, kiosk shell, seat agent tray | same | the seat user's UI language |
| Credential provider tiles | `credprov/src/copy.h` | LogonUI's UI language (SYSTEM: machine default, HYPOTHESIS) |
| MSI / NSIS | `installer/wix/locales/*.wxl`, `hooks.nsh` LangStrings | one MSI per culture (`build-installer.ps1 -Language`, en-US default); NSIS picks the OS language |
| Experimental warning | `crates/seat-session/src/termwrap/warning_{en,tr}.txt`; manager `warning.ts` (`WARNING_EN` / `WARNING_TR`, checked word for word) | the manager's language; the service status carries `warning_text` (English) and `warning_text_tr` |

Account comments mark Pulsar's own accounts and group: `Text::matches` accepts the comment
in either language, so a language change never turns our account into a "foreign" one.
Preflight check titles are localized on the console (`seat_compat::preflight::title`), the
setup log uses English titles (`title_en`, `BLOCKED` / `WARNING` prefixes).


## Install-time optional features and publisher certificate (remote-smalls, 2026-11-17)

Spec/remote 90 §2.4 / §2.5. Both are unchecked installer choices on the MSI page "Optional
Windows components" and reach `pulsar-seat-service install` as `--features=<ids>` and
`--import-cert=<0|1>` (`seat_compat::setup::InstallOptions`). Order in `setup::install`:
gate, restore point, tweaks, **features**, **certificate**, driver packages.

| Piece | Module | Journal change | Uninstall |
| --- | --- | --- | --- |
| VirtualMachinePlatform, Microsoft-Hyper-V-All, Containers-DisposableClientVM (allow list) via `dism.exe /Online /Enable-Feature /All /NoRestart /LimitAccess` | `seat_compat::features` (+ `win_certs::feature_*`) | `OptionalFeature { name }`, only when it was off | kept (`closed`), logged |
| `certs\publisher.cer` (shipped only by `build-installer.ps1 -PublisherCert`) into `LocalMachine\TrustedPublisher` (+ `Root` when self-signed); leaf code-signing certificate only | `seat_compat::certs` (+ `win_certs::{cert_*, inspect_cert}`) | `Certificate { store, thumbprint }`, only for a store that lacked it | deleted from that store |
| Driver catalogs signed by that certificate | `seat_compat::drivers::signer_accepted_with` (thumbprint, only while it is in `TrustedPublisher`) | `DriverPackage` as before | as before |

`SystemOps` gained `cert_thumbprints / cert_add / cert_remove / feature_state /
feature_enable` (defaults: unsupported; `MemoryOps` fakes them). HYPOTHESIS: never run.

## Remote pad pump (remote-smalls, 2026-11-17)

`runtime::start_pad_pump` waits on duplicated copies of the pad channels' auto-reset input
events (`RemoteEngine::with_pad_events`, rebuilt when `channels_generation` changes, at
most 64) with a 4 ms timeout, then pumps. Input from a stream server wakes it at once;
feedback, media rings and outgoing messages keep the 4 ms cadence (Split OQ
integrator-2). HYPOTHESIS: latency gain not measured.

## Compatibility options (compat-tweaks, 2026-11-17)

Every option is a visible opt-in in the manager (Advanced > Compatibility, seat menu
"Environment and sign-in…", system menu "USB dock wizard…"), off by default, written by
the service only, journaled before each change (`seat_compat::journal`) and undone when
the option is turned off, when the product is disabled (`Service::sync_tweaks` /
`sync_compat(false)`) and at uninstall (`Journal::revert_all`). Nothing is written
silently and nothing blocks Windows Update.

| Option | Config | Mechanism (documented) | Journal entry |
| --- | --- | --- | --- |
| Fast startup off | `settings.tweaks.fast_startup_off` | `HiberbootEnabled` = 0 (`TweakId::FastStartupOff`, restart needed) | `Registry` |
| Windows Update deferral / active hours / no restart while signed in | `settings.tweaks.windows_update` | Windows Update for Business policies under `HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate` (+ `\AU\NoAutoRebootWithLoggedOnUsers`); values that a changed config no longer wants are reverted (`Journal::revert_owner_except`) | `Registry` |
| Device-install deny | `settings.tweaks.device_install_deny` (classes from `DeviceInstallClass`, never display / input / audio / USB / disk / network) | Device Installation Restrictions: `DenyDeviceClasses` = 1, numbered `{GUID}` values in `...\Restrictions\DenyDeviceClasses` (operator entries are reused, never removed), `AllowAdminInstall` | `Registry` (numbered names allowed by `is_tweak_target`) |
| Defender exclusions | `settings.tweaks.defender_exclusions` | WMI `MSFT_MpPreference` (`root/Microsoft/Windows/Defender`): read lists, static `Add` / `Remove` through `Invoke-CimMethod`; install folder, data folder, service / host / agent executables by full path; only items not already listed | `DefenderExclusion` |
| Class-level default descriptors | `settings.devices.class_default_descriptors` + classes | switch enabled in the manager; classes CD-ROM, Ports, Modem, Multi-port serial, Floppy, Tape, Medium changer (disk / volume refused, §4.5) | gate journal (seat-media) |
| Seat environment variables | `seats[].environment` (both kinds) | `HKEY_USERS\<seat SID>\Environment` at each logon (`seat_compat::user_env`), agent task `refresh_environment` (`WM_SETTINGCHANGE("Environment")`), launch env of startup apps / launcher / remote autostart | `UserEnvironment` |
| Deny log on locally | `seats[].deny_local_logon` | `SeDenyInteractiveLogonRight` through `LsaAddAccountRights` / `LsaRemoveAccountRights`, only for RDP-signed-in seats with an account (validation `deny_local_logon_unsupported`), only when the account lacked it | `AccountRight` |
| Steam per local seat | `settings.apps.steam_per_seat` | local seat map flag `steam` (shim adds `-master_ipc_name_override` to `steam.exe` in a seat's own session) | none (volatile seat map) |
| Single-instance programs | `settings.apps.object_rules` | `Shim\ObjectRules` (`seat_shim_core::objects`), module hooks `Create*/Open*` of mutex / event / semaphore / file mapping in listed programs: per-seat name suffix (`Global\` names for own-session seats, every match for kiosk seats) | none (cleared on disable) |
| Steam add-on DLL | — | pre-flight check `steam_proxy_dll` (warning); request `steam_proxy_dll` (`status` / `quarantine` / `restore`) moves a reported `version.dll` into `<data>\quarantine\` and back | `QuarantinedFile` |
| DPI read-back | — | agent task `report_dpi` → `AgentEvent::Dpi` (`GetDpiForMonitor`, per-monitor-aware thread); remote seats with a static scale: one-shot `pulsar-seat-agent --report-dpi` in the seat session, output line `PULSAR_SEAT_DPI <json>`; notice `dpi_mismatch` (local) / host warning `dpi_mismatch` (remote) beyond 12 % points | none |
| USB dock wizard | — | manager only (`compat.ts` `usbDocks`, `docks.ts` `assignDocks`): hubs with a monitor plus input / sound behind them; assigns the dock's container / hub key and each device | — |

Protocol: `RequestBody::SteamProxyDll` (admin) / `ResponseBody::SteamProxy`,
`AgentTaskKind::{RefreshEnvironment, ReportDpi}`, `AgentEvent::Dpi`, all additive.
`ServerFrame` carries `#[allow(clippy::large_enum_variant)]` (the config view grew).

HYPOTHESIS (nothing ran on hardware): every Windows effect in the table: policy values
honoured on Pro (Home ignores them), Defender changes allowed for LocalSystem under Tamper
Protection, the deny right effective without a reboot, Explorer reloading the environment
on the broadcast, the module's object hooks reached by the listed programs, Steam honouring
the IPC override on local seats, the DPI read-back values, dock detection on real
multi-seat USB docks (container ids and parent hubs as assumed).

Documented limits: the Steam library folder is per Steam install (two seats sharing one
install share the library and the machine-wide Steam Client Service; a seat that needs its
own library uses its own Steam install folder, started from its startup apps); named-object
isolation only reaches programs on the shim target list (never anti-cheat games); the deny
right is ordinary local policy that domain Group Policy may overwrite.

## Console lock guard and seat power policies (OSMS parity G4, 2026-11-18)

Local seats are presenter windows on the console's desktop. A locked console (Win+L,
Ctrl+Alt+Del > Lock, screen-saver lock, machine inactivity limit) switches to the secure
desktop and every seat monitor shows the lock screen. The values below are documented
per-user / machine policies, written by the service only, journaled before each write in
the compat journal (no journal of their own) and undone through it.

| Option | Config | Mechanism | Journal entry (owner) |
| --- | --- | --- | --- |
| Host account lock + screen saver off | `settings.console_guard.host_lock_off` (default **on**), only under `dedicated_host` | `HKU\<console SID>\Software\Microsoft\Windows\CurrentVersion\Policies\System` `DisableLockWorkstation` = 1 and `HKU\<SID>\Software\Policies\Microsoft\Windows\Control Panel\Desktop` `ScreenSaveActive` = "0" (`seat_compat::console_guard::sync_host`, `with_user_hive`) | `UserPolicy` (`console_guard:host`) |
| Seat 1 lock off while seats run | `settings.console_guard.seat_one_lock_off` (default off), only under `seat_one_console` | `DisableLockWorkstation` = 1 for the console (seat 1) user: engaged when a seat other than the console seat is in use, released on the last seat stop, on service stop, on disable, and at the first sync of every service run (crash leftover) (`sync_seat_one`) | `UserPolicy` (`console_guard:seat_one`) |
| No power menu for a seat user | `seats[].no_power_menu` (needs the seat's account) | `HKU\<seat SID>\...\Policies\Explorer` `NoClose` = 1 (`sync_no_close`, in `sync_compat` and again at the seat user's sign-in, when a new profile exists) | `UserPolicy` (`seat_policy:no_close`) |
| Users shutdown right off | `settings.tweaks.users_shutdown_right_off` | `LsaRemoveAccountRights(BUILTIN\Users, SeShutdownPrivilege)` only while `BUILTIN\Administrators` holds it (else refused); given back on off / disable / uninstall; a tampered entry naming another right is refused | `AccountRightRemoved` (`seat_policy:users_shutdown`) |
| Inactivity lock off | `settings.tweaks.inactivity_lock_off` | `HKLM\...\Policies\System` `InactivityTimeoutSecs` = 0 (`TweakId::InactivityLockOff`) | `Registry` (`tweak:inactivity_lock_off`) |
| Displays and PC stay on | existing `TweakId::SleepOff` (`--tweak=sleep_off`) | no new power code; preflight check `host_sleep_off` (`seat_compat::tweaks::sleep_off_state`) warns under `dedicated_host` when the active plan's AC `STANDBY_IDLE` / `VIDEO_IDLE` are not 0 | — |

Rules: nothing is written to HKLM `DisableLockWorkstation` (Windows reads it per user only,
OSMS notes the same). A value the user already holds is never recorded and never undone;
a value changed by someone else since our write is left alone (`ChangedSince`). The host
guard follows the account signed in at the console under `dedicated_host` only when it is
the host account (`console_guard::console_owner`): with the host console preset wanted
(`host_autologon`) only Pulsar's console account `PulsarSeatConsole`; without it any
console user except a seat's configured account. Anybody else gets nothing written and an
earlier host entry is undone (gaming PC 2026-11-24, bug 11: preset refused for
`other_autologon`, the owner's own hive got `DisableLockWorkstation` / `ScreenSaveActive`).
A different host account takes it over and the old one is undone; while nobody is signed
in the last state is kept. The service syncs from the tick
(only on a change) and after every config change (`Service::update_console_guard`,
`seat-service/src/console_guard.rs`); `Platform::sync_console_guard` /
`sync_no_power_menu` / `sleep_off_state` are the seams (fake in `platform::mock`).
Preflight adds `host_sleep_off` and `console_lock` (`seat_compat::preflight::console_checks`)
to the `Preflight` response. The manager shows the options in Advanced > Compatibility >
Console lock (plus the "locking seat 1 hides every local seat" notice under
`seat_one_console`) and the per-seat switch in "Environment and sign-in…". `PromptOnSecureDesktop`
is never written (a UAC prompt still blanks the presenters; changing it is a security
trade-off left to the maintainer). x64 and ARM64 share every code path.

HYPOTHESIS (nothing ran on hardware): `DisableLockWorkstation` blocks Win+L and
Ctrl+Alt+Del > Lock for an auto-logon account without a sign-out; the per-user
`ScreenSaveActive` policy stops the screen saver for that account; `NoClose` is honoured by
a running Explorer (else from the next sign-in); the removed shutdown right applies from
each user's next sign-in; `InactivityTimeoutSecs` = 0 clears a policy-set limit at once.

## Remote seats on the own display: protocol provider, modes, HDR (2026-11-17)

Package remote-idd-hdr. A remote seat's session gets our IddCx remote-role display
(`drivers/seat-idd`, hardware id `PulsarSplit_IndirectDisplay`) only when the session's
protocol stack names that driver (spec/remote 40 §2.3). Pulsar Seat therefore ships its own
Remote Desktop protocol provider; mode changes and HDR metadata of such a seat go through
our IDD (`REQUEST_MODE`, `SET_SEAT_CONFIG`) instead of RdpIdd. Built and unit-tested;
nothing ran (needs Windows Server with RD Session Host). HYPOTHESIS list:
`crates/seat-wrds/README.md`.

| Part | Where |
| --- | --- |
| Provider DLL (`pulsar-seat-wrds.dll`, COM in-proc, `ThreadingModel = Both`) | `crates/seat-wrds` (`state.rs` pure, `com.rs`, `pipe.rs`) |
| Pipe contract service <-> provider (`\\.\pipe\PulsarSeat.Wrds`, length-prefixed JSON, `hello` / `welcome`, `connect` with the password as `Secret`, events `accepted` / `session_id` / `logged_on` / `driver_loaded` / `logon_error` / `refused` / `disconnected` / `closed`) | `seat_proto::wrds` |
| Registration: COM class `{6D3B1E54-7C2A-4F19-9E8B-2A51C4D07F36}` + WinStation `PulsarSeat-Idd` (`LoadableProtocol_Object`, seat-account-only `Security`, enable last); recovery `WrdsDisabled` | `seat_session::wrds` |
| Plan per seat start, hub (pure), `WrdsPort` | `seat-service` `remote::wrds` |
| Pipe server (one instance, TermService process check), registration sync, logoff | `seat-service` `remote::win::wrds` |
| Engine | `remote::engine`: `boot_steps` (provider path first, fallback), `start_provider`, `attach_display(force, wait_device)`, `apply_mode`, `set_display_mode`, `sync_wrds`, lost connections in `tick` |

Rules:

- **When.** `RemoteSettings::display_backend = pulsar_idd`, or automatic with a signed own
  driver (`OWN_DRIVER_SIGNED`, false today). Then the registration is kept in place
  (`Engine::sync_wrds` at service start, on a display setting change and when a new seat
  account starts), else removed. Coexistence passive mode never touches it.
- **Path.** `remote::wrds::plan`: provider status usable (registered, provider connected,
  listening, DLL native, no recovery switch) -> provider session (`WrdsPort::connect`,
  blocks until `LoggedOn`, 90 s) -> `PulsarIdd` attach on that session (retries
  `driver_not_installed` while the device arrives) -> first frame. Any failure undoes the
  provider session (`WTSLogoffSession`) and the seat starts on the loopback RDP client with
  `rdp_session`; notice `host_warning {kind: display_fallback, from, to, reason}` and
  `seat_runtime.display.{host, fallback, device_instance}`.
- **Stop / loss.** Provider sessions end through `WrdsPort::end` (log off or disconnect by
  the seat's stop action, then `disconnect` to the provider); a connection that ends
  without the engine asking (`take_lost`) takes the seat down as `client_exited`.
- **Modes.** Per-seat `DisplaySettings` gained `fixed_width` / `fixed_height` (0 x 0 =
  follow the client; validation 640-8192) and `lock_refresh`. `seat_mode_request` applies
  them to every client `display.mode.request` and to the operator's
  `set_seat_display_mode` (new protocol 3 request, admin, remote seats; answers
  `mode_result`); super-sampling, scale, refresh clamps and the HDR gate stay in the
  backend's `resolve_mode`. The seat session starts at the fixed size when set. Operator
  request ids have the top bit set.
- **HDR.** Unchanged rules (spec/remote 40 §8.4): our IDD's IddCx 1.10 path with the seat's
  SDR white level and luminance (`SET_SEAT_CONFIG`); the HDR facts of a provider seat are
  probed after its device exists.
- **Manager.** Seat page: stream resolution (follow / presets / custom), lock refresh,
  runtime "session path" and fallback reason, "display mode now" (width, height, Hz, HDR,
  apply, result). Host page: protocol provider status. en / tr keys
  (`seat.fixed*`, `seat.mode*`, `seat.host*`, `modeStatus`, `wrdsReason`, `host.wrds*`,
  `hostWarning.display_fallback`).
- **Installer.** `pulsar-seat-wrds.dll` next to the service (WiX component
  `PulsarSeatWrdsComponent`, file only); `check-payload` holds it to `MUST_BE_SIGNED` in a
  release; `uninstall` removes our two keys.
- **Not on this path.** RDP virtual channels (audio redirection, microphone hold, clipboard,
  drives): a provider session has none. Open item in status.md.

## Session frame rate (OSMS parity G3, 2026-11-18)

Windows composes a Remote Desktop session at about 30 frames per second unless
`HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\DWMFRAMEINTERVAL`
(DWORD, ms; Microsoft Learn "Remote Desktop frame rate is limited to 30 FPS", 15 = 60 fps)
says otherwise. Every local seat except a console seat 1 runs in such a session, so without
the value a seat monitor shows about 30 fps.

- **Config** (additive): `settings.outputs.session_frame_rate = { mode, fps }`, mode
  `auto` (default, not written) / `smooth` (4 ms) / `fixed` (`fps` 30..=500, else
  `frame_rate_out_of_range` error in seat-core `validate` and the manager) / `off`
  (nothing written, Windows' default).
- **Interval** (`seat_core::frame_rate`): `auto` uses the highest refresh rate among live
  monitors whose effective seat is a local RDP seat (every local seat except seat 1 under
  `SeatOneConsole`), 60 Hz when none reports a mode. `ms = floor(1000 / Hz)` clamped to
  2..=33: 60 Hz gives 16 ms, 144 Hz 6 ms. This deliberately differs from the plan's
  `round()` (17 ms for 60 Hz, 58.8 compositions per second): rounding down keeps the session
  at or above the monitor's rate, and Microsoft's own 60-fps example (15) is also below
  16.7. Never slower than Windows' default.
- **Flow**: the service computes the interval (`Service::sync_frame_interval`,
  `seat-service/src/frame_plan.rs`) with every listener plan (so before each
  `prepare_host` and after each config apply) and after an inventory change that touches a
  monitor, and hands it to `SessionBackend::set_frame_interval` (default no-op; both RDP
  backends implement it). The RDS writer (`seat_session::rds`, row scope `LocalFrame`, data
  filled in by `rows_for`) remembers it in `TsClaims` and writes it only while the local
  claim is held; a remote claim alone never writes it.
- **Ownership**: written only while absent or still holding our own last journaled value;
  any other value (an admin, a tuning tool) is reported as `PolicyOverride` and left alone.
  Turning it off restores only that row; release (`restore_host`: product disabled, backend
  deselected), uninstall (`restore_all`) restore it with the other rows, guarded (a value
  changed after us stays). Like the other host rows it stays across a plain service stop and
  is re-claimed (or trimmed, if turned off meanwhile) at the next boot from the journal. It
  never touches console input devices.
- **Also**: policy `bEnumerateHWBeforeSW=1` moved from remote-only to common, so local seat
  sessions use the hardware GPU too (only while absent, like before).
- **Not written** (not Microsoft-documented, unlike in OpenStreamMultiSeat): `MaxFrameRate`,
  the `Policies` / `SOFTWARE\Microsoft` copies of `DWMFRAMEINTERVAL`, `VRefresh` on RDP
  adapters, TermDD `FlowControl*`, `SystemResponsiveness`.
- **Takes effect** when a seat's session next connects (stop and start the seat); the
  manager says so. Mode changes stay on the DisplayControl channel (no reconnect).
- **Manager**: Display outputs dialog, machine-wide group: one select (automatic, smooth,
  fixed 60/75/90/100/120/144/165/240 plus a saved custom rate, Windows default) with a
  note that it applies to the whole PC. Keys `outputs.sessionFps*`,
  `issueText.frame_rate_out_of_range` (en + tr).
- **HYPOTHESIS** (not run on hardware, x64 or ARM64; same code path on both): that the
  `WinStations` value alone lifts the cap for our loopback sessions on Server + RDSH and
  under the wrapper, that a reconnect is enough (no TermService restart), and that the
  floor interval is smooth without `smooth`. If `WinStations` is not enough, add
  `RDP-Tcp\DWMFRAMEINTERVAL` behind its own HYPOTHESIS note.

## Host console preset (OSMS parity G6, 2026-11-18)

"All seats equal": the physical console belongs to a Pulsar account that only runs the seat
hosts, and every seat, seat 1 included, is a session of its own (OSMS's recommended "host
console user with autologon", `docs/puestos-locales.md` there). Opt-in, with a visible
consent step. **Everything here is untested on hardware (HYPOTHESIS).**

- **Config.** `settings.backends.host_autologon: bool` (additive, default `false`, omitted
  when false). Wanted = product enabled + `host_topology = dedicated_host` + this flag
  (`Service::host_console_wanted`). Ignored under `seat_one_console`.
- **Wizard.** `WizardVariant::HostConsole` (`host_console`) captures seats 1..N like
  `all_seats`; its saved result also sets `dedicated_host` and `host_autologon = true`. The
  manager shows the trade-offs and needs a ticked consent box before it starts this variant.
  Advanced settings: under `dedicated_host` a switch "Sign the host console in automatically"
  that asks the same consent in a dialog, the trade-off list, and the last sync state.
- **Account.** `PulsarSeatConsole` (`seat_session::console_account`), not `PulsarSeatHost`.
  Deviation from the plan's "autologon of PulsarSeatHost": that account is batch-logon only
  and explicitly denied interactive logon (contained host token, `host_account::DENY_RIGHTS`),
  and Winlogon's auto-logon needs exactly interactive logon. Giving it that right would widen
  the contained seat-host account. The console account is a standard user (`Users` only,
  never `Administrators`, so no UAC prompt), hidden from the logon screen, marked by its
  comment (a same-named account with another comment is never taken over or deleted),
  granted `SeInteractiveLogonRight` only, denied RDP / network / batch / service logon.
- **Winlogon.** `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon` (64-bit view on
  x64 and ARM64 alike): `DefaultDomainName = "."`, `DefaultUserName = PulsarSeatConsole`,
  `AutoAdminLogon = "1"` last. The password goes only into the LSA private data
  `DefaultPassword` (`LsaStorePrivateData`), never into the registry; the same freshly
  generated password is in SAM and in Pulsar's `L$PulsarSeat.ConsoleAccount`, written in one
  step so they never differ. It is rotated each time the preset is switched on, not per boot.
- **Refusals.** Somebody else's auto-logon is never replaced or read: `AutoAdminLogon = 1`
  for another user, a clear-text registry `DefaultPassword` (presence checked by size only),
  an LSA `DefaultPassword` Pulsar did not write (presence only, buffer wiped), or
  `AutoLogonCount` give the state `refused` with a reason code.
- **Kiosk values.** `sleep_off` (the existing journaled compat tweak: display / sleep
  time-outs 0 on the active plan) while the preset is on, reverted only when the preset
  changed something. In the console account's own hive while it is loaded:
  `DisableLockWorkstation = 1`, screen saver off (policy keys). `PromptOnSecureDesktop` is
  **not** turned off machine-wide (that would weaken the human seats too).
- **Journal.** `journal\host-console.json` (previous Winlogon values, no password, phase
  `engaging` / `on` / `pending_removal`, whether `sleep_off` was ours). Written before the
  first change. A failure mid-engage rolls back; a crash mid-engage (phase `engaging`) is
  rolled back by the next sync before anything else.
- **Restore.** When the preset, `dedicated_host` or the product goes off, and at uninstall:
  Winlogon values back (only while they still name our account; values someone changed since
  are left alone), LSA `DefaultPassword` deleted, `sleep_off` reverted, the account deleted
  with its profile and secret. If the account is the signed-in console user at that moment it
  is disabled and its secret forgotten; it is deleted at a later sync (next boot at the
  latest; auto-logon is already off). Not restored at seat stop or service stop on purpose:
  the service stops at every shutdown and the preset must sign the console in at the next
  boot. Not touched in any safe mode (a power loss must not take the auto-logon away).
  The console keyboard and mouse keep working: the host session is an ordinary interactive
  session, and an administrator can always use "Other user" at the logon screen.
- **When it syncs.** Service start (with the tweaks), product enable / disable, a change of
  `host_topology` or `host_autologon`, every console logon under `dedicated_host`.
- **Status.** `ServiceStatus.host_console: Option<HostConsoleReport>` (additive; `None` =
  off): `on`, `refused`, `failed`, `drifted` (values changed outside Pulsar, left alone),
  `pending_removal`, plus a reason code. `refused` / `failed` / `drifted` also raise the
  notice `host_console`.
- **Not in the preset.** G4 (guard) and G2 (WinUSB capture) are separate items; under
  `dedicated_host` `raw_input_host` already isolates input fully in user mode, so G2 is extra
  hardening. Unlike OSMS, a full primary monitor may still be a seat (our console presents
  seat 1 like seats 2..N).
- **Open.** The maintainer's OK for spec 30 §6.2.1 (auto-logon of a service-owned account) is
  needed before shipping. The VM guest writer (`win/vm_guest.rs`) keeps its own Winlogon code:
  it runs inside a guest with a guest-side user and different restore rules; sharing it was
  not worth coupling the two paths.
- **Code.** Pure policy `seat_session::console_account`; order and restore
  `seat_service::host_console` (mock-tested: order, refusals, rollback, crash, drift, signed-in
  account, `sleep_off` ownership); Windows calls `seat_service::win::host_console`; uninstall
  in `win::setup::uninstall`.

## Physical pad pass-through and Guide-button guard (OSMS parity G5, 2026-11-20)

Extends the pad scoping of 10.6 (`seat_input::pad_scope`); no new subsystem, no driver.
Behaviour reference: OpenStreamMultiSeat (MIT), read only, no code copied. Everything here
is **HYPOTHESIS**: nothing ran on hardware or in the VM, on x64 or ARM64. Same code paths
on both architectures; the only arch-specific value is the PE machine HidHide must match.

- **Kill switch.** `settings.input.pad_device_scope` (default **off**) gates everything
  below. Its default may flip to on only after a VM / hardware check.
- **Per-seat mode.** `LocalSeat::pad_mode` (`seat_core::config::PadMode`, additive, not
  written when default): `passthrough` (default) = the physical pads routed to the seat are
  scoped to the seat's session (descriptor + restart, no virtual pad); `shared` = they are
  left alone (not scoped, not even to the console). Virtual pads (`virtual_pads`) stay for
  remapping and DualSense feature cases. Manager: seat menu entry (`settingsPage.padsShare`
  / `padsPassthrough`, seats 2..N), seat fact `padsSharedFact`, and the
  `advanced.padDeviceScopeHint` text names the guard.
- **Discovery** (`pad_scope::discover` over the `PadTree` trait; Windows:
  `pad_runtime::WinPadTree` over `seat_pads::win::devtree`: `CM_Get_Device_ID_List` with a
  class filter, `CM_Get_Child` / `CM_Get_Sibling`, `SPDRP_CLASSGUID`,
  `SPDRP_COMPATIBLEIDS`, all read only). Controller nodes: class XUSB
  `{d61ca365-5af4-4486-998b-9db4734c6ca3}`, GIP `{05f5cfe2-4733-4950-a6bb-07aad01a3a84}`,
  HIDClass with compatible id `HID_DEVICE_SYSTEM_GAME`. Pad root = the topmost ancestor
  still of class XUSB / GIP / HID (a hub, a USB composite parent or a Bluetooth radio stops
  the walk). Scoped set = root + every XUSB / GIP / HID node below it. Console-input
  safety: every node above a Keyboard or Mouse class node is dropped, so a combo pad's
  keyboard is never restarted or put under a narrowed descriptor. An inventory pad unit
  the class walk did not find keeps the older "HID collection + parent" set.
- **Routing** (`seat_service::local_pads::scope_pads`): a pad group goes to the seat one of
  its HID units is routed to, else to the seat of the nearest inventory device on its
  ancestor chain (its own USB device or the hub / port it is plugged into), so GIP and
  XUSB pads without a HID unit follow port assignment.
- **Owner principal** (`pad_scope::principal`): the user SID; when the same user SID owns
  two sessions (two seats, or a seat and the console), each session's logon SID
  (`S-1-5-5-X-Y`, `SessionUser::logon_sid`). HYPOTHESIS (OSMS measurement, not ours): the
  descriptor is checked per token, so a logon SID separates two sessions of one user.
- **Handles.** HYPOTHESIS (OSMS measurement): the descriptor applies only after the
  `DIF_PROPERTYCHANGE` restart, and the restart closes handles other sessions had open.
- **`DEVPKEY_Device_SessionId`** is cosmetic only (HYPOTHESIS, OSMS measurement: it hides
  enumeration with CR 13, an open by path still works, Windows clears it on every device
  start). `PadScoper::reassert_session_ids` re-applies it after each device-interface
  arrival (`devtree::DeviceArrivalWatch`; every 10 s when the watch could not be
  registered); no restart, no journal change.
- **Guide-button guard** (`seat_compat::guide_guard`, driven by
  `seat_service::guide_guard` from the 1 s pad tick). While the switch is on and a second
  session could open the pads, for the users of running seats with their own session
  (local seats in passthrough, remote seats) plus the console user:
  `HKEY_USERS\<sid>\Software\Microsoft\GameBar\UseNexusForGameBarEnabled` = 0, and machine
  `HKLM\SOFTWARE\Microsoft\Input\Settings\ControllerProcessor\ControllerToVKMapping\Enabled`
  = 0. The seat running in the console session counts as the console user, not as a seat
  that needs the guard. Journal first in `%ProgramData%\Pulsar Seat\journal\changes.jsonl`
  (`Change::UserGameBarGuide` per user, owner `guide_guard:<SID>`; the machine value as
  `Change::Registry`, owner `guide_guard_vk`, in the revert allowlist). A user value is
  written only while the hive is loaded (a sign-in still loading is retried after 30 s);
  a value already 0 is not ours and never journaled; a value changed after us is left
  alone at revert. Restore: the want shrinks (seat stop, mode shared), service stop (empty
  sync), and the first tick after a crash or reboot (syncs to what is running now, so
  leftovers go back); uninstall's journal `revert_all` knows both kinds. OSMS keeps the
  previous value only in the user hive and has no start-up restore. HYPOTHESIS: Game Bar
  reads the user value at button press (no sign-out needed) and the VK mapping value
  stops cross-session navigation input.
- **Coexist.** The ASTER footprint row for `ControllerToVKMapping\Enabled` (expected absent,
  repairable) skips the value while the service journal holds it as an active change
  (`seat_setup::journal::compat_registry_targets`, read into `SetupCtx::own_targets`), so
  our own 0 is not reported or repaired as residue. `spec/remote/90-install.md` is
  analyst-owned and was not changed.
- **HidHide** (`seat_input::pad_jail`, `pad_jail_hidhide`): **detect only**. At service
  start the file `%SystemRoot%\System32\drivers\HidHide.sys` is read as data (never loaded
  or run); its version resource and PE machine are logged; "usable" = version >= 1.4.181
  and machine = this build's arch. Never bundled, downloaded or installed; the jail IOCTL
  path is not implemented. HYPOTHESIS: an ARM64 HidHide build exists.
- **Limit.** GameInput and Windows.Gaming.Input open pads from a system broker service, so
  no device descriptor isolates them (OSMS states the same). Mitigation is limited to the
  SDL hints and the shim's XInput seat map.
- **Cost.** Discovery runs on the 1 s pad tick only while the switch is on and a second
  session exists (three class lists plus SetupAPI reads per node). Not measured.

## In-session capture for local seats (OSMS-parity G1, 2026-11-20)

Opt-in per local seat: `local.frame_source = "session_capture"` (default `rdp_decode`;
additive, absent = default, not written when default). The seat's picture comes from DXGI
Desktop Duplication inside the seat's own RDP session instead of the RDP codec, so the
local display path has no H.264 encode / decode and no RDP flow control. Clean room: written
from the Microsoft Learn pages on Desktop Duplication, `IDXGIKeyedMutex`,
`IDXGIResource1::CreateSharedHandle`, `DuplicateHandle`, `CreateFileMappingW`; no code of
another multi-seat product was copied (no third-party notice needed). **Everything below is
a HYPOTHESIS on hardware** (spec/remote/40-display-hdr.md §6.5 is the deciding experiment;
our spec still cites "unavailable on RDP indirect displays"); the default stays
`rdp_decode` until that test passes on x64 and Adreno ARM64.

- **Pure contract (`seat_core::capture`).** A mailbox of `u64` words in an unnamed section:
  header (magic, generation, state `starting / live / lost / stopped`, output count, QPC
  heartbeat, QPC frequency), one seqlocked record per output (frame count, ring slot,
  `LastPresentTime`, publish QPC, size) and one seqlocked pointer record (output, visible,
  position, hotspot, shape up to 128x128 straight BGRA). Single writer (the agent); readers
  check `live` (valid, state live, heartbeat at most 2 s old). Also: `RingCursor` (never
  reuse the published slot, oldest first), `shape_to_bgra` (monochrome / colour / masked
  colour pointer shapes), `choose_output` / `map_outputs` (RDP monitor to captured output by
  normalized rectangle, never "the first attached output"), `output_usable` (same adapter
  LUID as the presenter, else fall back), `Fallback` reasons.
- **Agent (`pulsar-seat-agent`, user token, `crates/seat-host/src/capture/agent_win.rs`).**
  Task `start_capture` (queued by the service for every new agent of a `session_capture`
  seat, and for a running agent that offers nothing): a coordinator thread enumerates up to
  four attached, unrotated outputs, creates mailbox + auto-reset event, starts one worker per
  output (own D3D11 device on the output's adapter, `SetGPUThreadPriority(7)` best effort,
  `DuplicateOutput`, three `SHARED_NTHANDLE | SHARED_KEYEDMUTEX` BGRA textures) and a
  pointer poller (`GetCursorPos` on a 2 ms high-resolution waitable timer). Per frame: take
  a slot with key 0 (or key 1 = a frame the presenter never took), `CopyResource`, release
  key 1, publish, set the event. Every capture thread switches to the input desktop first. A
  lost duplication marks the mailbox `lost`, tears the ring down and rebuilds every 250 ms
  (new generation `pid << 32 | n`); `DXGI_ERROR_UNSUPPORTED` 8 times in a row ends the
  capture (`capture_stopped {reason: dxgi_unavailable}`). Reports go to the service only
  (`agent_report capture_ready {offer}`), never to the host. `stop_capture` ends it.
- **Service (`crates/seat-service/src/capture.rs`, `win/handle_broker.rs`).** Accepts an
  offer only from the seat's launched agent PID in the reporting session, checks its shape
  (`offer_ok`: 1-4 outputs, full rings, sizes, non-zero handles) and keeps it on the slot
  (dropped when the agent is relaunched, the seat stops, or `capture_stopped`).
  `host_capture_ring {seat}` (protocol 3, host access) duplicates the handles from the agent
  into the seat host (`OpenProcess(PROCESS_DUP_HANDLE)` + `DuplicateHandle`; textures same
  access, mailbox `FILE_MAP_READ`, event `SYNCHRONIZE`; all or nothing) and answers
  `capture_ring` with values valid in the host. Nothing on the system changes, so nothing is
  journaled: the handles die with the processes. Switching a running seat's `frame_source`
  is reported in `restart_seats` (the EGFX offer is fixed at connect); switching it off also
  queues `stop_capture`.
- **Seat host (`crates/seat-host/src/capture/`).** `HostAssignment.frame_source` (additive).
  A `session_capture` seat connects without EGFX (`graphics_for`: bitmap updates only;
  `PULSAR_SEAT_GFX` still wins). A reader thread fetches the ring, maps the mailbox
  read-only, waits on the event (100 ms), routes RDP monitors onto captured outputs every
  2 s (rectangle + presenter adapter LUID) and, while live, sets the `CaptureGate`:
  `SharedSink` then drops the engine's frames and pointer updates for covered monitors and
  the reader presents the slots as `GpuFrameRef` (generation `1 << 63 | fetch`) and the
  pointer as `CursorState`. Presenter (`present_gpu`, `flip.rs` keyed mutex) unchanged. Not
  live / stale / stopped: gate cleared, engine asked for a full redraw, the RDP picture
  shows; a new ring is fetched at most once a second. Per-span QPC latency (present ->
  publish -> present) is logged every 600 frames.
- **Fallbacks.** Before sign-in (no agent), lock / UAC / Winlogon desktop (a user-token
  agent cannot duplicate the secure desktop), adapter mismatch, no GPU presenter on a
  monitor, rotated outputs, Desktop Duplication unsupported: the RDP picture (bitmap
  updates).
- **Manager.** Seat menu toggle (`settingsPage.captureOn` / `captureOff`, "restarts the
  seat") and the seat fact `captureFact`; en / tr.
- **x64 / ARM64.** Same code on both (windows / windows-sys DXGI, D3D11, Win32 only).
  HYPOTHESIS: Desktop Duplication works on RdpIdd outputs on Adreno drivers.
- **Not built.** A SYSTEM agent for lock-screen capture (wider attack surface, would need its
  own opt-in and a pipe ACL of SYSTEM + PID check), discarding bitmap updates before IronRDP
  decodes them (they are decoded, then dropped at the presenter), `TS_SUPPRESS_OUTPUT_PDU`
  (a second, measured experiment: it may starve Desktop Duplication), reusing the agent for
  the split-screen console path.

## Coexistence: reference product installed but disabled (2026-11-24)

Maintainer decision: the reference multi-seat product may stay **installed** (for example to
keep its licence) as long as it is fully turned off. Pure detection; Pulsar Seat never writes
to its keys (read: key existence, `Start`, SCM state).

- **States** (`seat_core::reference`, pure): `Absent` (no key of
  `consts::REFERENCE_SERVICE_KEYS` exists), `Disabled` (at least one exists and **every**
  existing key has `Start` = 4 **and** the service control manager reports it stopped; for the
  driver key: not loaded), `Active` (anything else). Every fact that cannot be read
  (`Start` missing or not a DWORD, SCM query failure, key existence unknown) counts against
  `Disabled` (fail closed). The keys are the two the guard already knew (`MUTESV_SERVICE`,
  `MUTENX_SERVICE`, spec 90 §11 item 4); there are no other interop identifiers for it.
- **Install gate** (`seat_compat::coexist`): `Coexistence.reference_keys` (name, start, active)
  and `reference_disabled` (additive, serde default false). Check `reference_product`: pass /
  **warn** (`Disabled`, detail starts with `reference_product_disabled:`) / fail (`Active`).
  `gate_exit_code` and `install_with` refuse only on fail, so `preflight`, `install --msi` and
  the setup log carry the warning. SCM state through the new read-only
  `SystemOps::service_active` (`QueryServiceStatus` with `SERVICE_QUERY_STATUS`; works for
  services and drivers; a name the SCM does not know counts as stopped).
- **MSI**: the launch condition also reads `Start` of both keys (`PSREFSEATSTART`,
  `PSREFDRIVERSTART`, raw `#4`) and passes when every existing key is 4; `PsRefDisabledDlg`
  warns in the full UI. The running / loaded state cannot be read by a launch condition: the
  deferred `install --msi` action's gate (SCM) refuses then (exit 4, the install rolls back).
  `Installed OR ...` as before, so a repair is never refused; a major upgrade is.
- **setup.exe**: the shipped `setup.exe` is the native `seat-bootstrap`, which runs the
  embedded MSI (full UI unless `/quiet` / `/passive`), so it behaves as the MSI above. The
  unshipped Tauri NSIS hooks (`installer/nsis/hooks.nsh`) got the same rule: PREINSTALL
  refuses unless every existing key has `Start` = 4, then asks to continue
  (`PsCoexistenceDisabled`, `/SD IDOK`); the `preflight` gate from `$PLUGINSDIR` checks the
  SCM state and refuses (exit 4) if one still runs.
- **Run time** (`seat_session`): `SessionDeps.reference_product` and
  `HostFacts::reference_product` return the state (Windows: `win::reference_product_state`,
  registry `Start` + `QueryServiceStatus`, evaluated at each probe). `rdp_readiness` refuses
  only `Active` (`reference_product_present`, unchanged) and adds the note
  `reference_product_disabled` for `Disabled`; the termsrv wrapper host gate blocks only on
  `Active`. The service's preflight list (manager Status / diagnostics) shows the warn check.
- **Manager**: `checkText.reference_product.warn`, `notes.reference_product_disabled` (en, tr);
  the fail text says how to turn the product off.
- **Not handled**: the product turned back on while seats run. The next probe refuses the RDP
  backends (running seats are not stopped by this check); HYPOTHESIS that this is enough,
  because the product's own service then fights for the same sessions anyway.
