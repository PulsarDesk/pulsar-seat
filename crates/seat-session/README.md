# seat-session

Session backends of Pulsar Seat (Phase 2). Licence: GPL-3.0-only. Spec: `spec/local/30-sessions.md`
§6, `spec/local/00-architecture.md` §5.2 / §5.2.1. Contract: `docs/agents/architecture.md` §4.1.

## Public API (for `seat-service`)

```rust
let mut deps = seat_session::SessionDeps::system(&data_dir);   // Windows, SYSTEM, session 0
deps.geometry = Some(Arc::new(|seat| /* seat monitor rects from SeatOutputs */ vec![]));
deps.last_user = Some(Arc::new(|seat| /* run state */ None));
let backend = seat_session::create_backend(kind, deps);          // Arc<dyn SessionBackend>
// fallback: seat_session::FALLBACK_KIND (fast_user_switch), never an experimental kind
```

Helpers: `launch::{SessionLauncher, command_line, env_block}` and `win::process::spawn`
(explicit handle list; the service's own launcher uses it to hand seat hosts their token on
stdin), kill-on-close job `PulsarSeat.Seat.NN`, `autologon::{credentials_for,
classify_disconnect, after_session_end}` (re-logon policy, never a retry loop after a logon
failure), `accounts::Accounts` + `win::accounts::NetAccounts` (`list_local_users`),
`wts::client_name_for` (`PSEAT-NN`), `experimental_warning(lang)` (warning in English or Turkish),
`backends::termsrv_wrapper::TermsrvWrapperBackend::boot_check()`.

## Backends

| Kind | Module | What a seat is |
| --- | --- | --- |
| `rds_loopback` (default) | `backends::rds_loopback` | RDP session on Windows Server + RDSH; seat host connects to `127.0.0.2:<listener port>` |
| `termsrv_wrapper` (experimental, off) | `backends::termsrv_wrapper` + `termwrap` (feature `experimental-client-sku`) | same, on a client SKU through an operator-installed TermService wrapper |
| `fast_user_switch` | `backends::fast_user_switch` | own session per seat, one on the console at a time (`WTSConnectSessionW` / `WTSDisconnectSession`) |
| `desktops` | `backends::desktops` | kiosk app in the console session on the seat's monitors (shared desktop) or on a per-seat desktop object (`CreateDesktopW`, one visible at a time); kiosk shells of seats 2+ run as the seat's own Windows user (`kiosk_account`, 2026-11-17) |

- **Per-seat kiosk users** (`kiosk_account`, 2026-11-17): pure identity rule
  (`identity_for`: console user for seat 1 and `accounts = shared`, the seat's account with
  a stored password, else `PulsarKiosk<NN>`), account names / LSA secret names / logon
  rights / desktop masks, the `SeatLogons` / `SeatLogon` traits the backend uses
  (`DesktopsOptions::logons`; fake `fake::FakeSeatLogons`), `SessionLauncher::launch_as`
  (Windows: duplicated primary token, same job and spawn path) and
  `win::host_account::interactive_token_in_session`. The Windows logon (password, grant,
  profile) is seat-service `win::kiosk_logon`. **Untested on hardware.**
  The grant also covers the session's named-object directory (`KIOSK_BNO_ACCESS`,
  `session_bno_path`, `win::host_account::edit_session_named_objects`; 2026-11-24, gaming
  PC E2E bug 6: without it the seat user cannot create `Local\` objects in the console
  session). Proven on the dev box: the directory DACL gives everyone but the session's
  user, SYSTEM and elevated admins only query + traverse; the grant / revoke round trip
  (`--ignored` test) leaves the DACL identical. That the shell then runs is HYPOTHESIS.

- **Probe** (`HostProbe.availability`): edition / RDSH role (`TSAppCompat`), ServiceDll stock
  (else `foreign_service_dll`), reference product (`reference_product_present` while active;
  installed but every key disabled and stopped = note `reference_product_disabled`, no block,
  `seat_core::reference`), TermService
  running, listener port, `fDenyTSConnections` policy (`rdp_denied_by_policy`), licensing
  notes (`rds_licensing_not_configured`), listener cap → `caps().max_seats`.
- **prepare_host** (RDP backends): claims the one Terminal Server writer (`rds`,
  `Claim::Local`): journaled rows `fDenyTSConnections=0`, `fSingleSessionPerUser=0`
  (machine-wide, spec 30 §6.2.13), `RDP-Tcp\UserAuthentication=1`, `RDP-Tcp\Shadow=0` in
  `journal\session.jsonl`; policies reported, never fought. **restore_host** releases the
  claim: guarded reverse replay of the rows no remote seat still needs (values changed
  after us stay). Writer order with the second listener and the wrapper: `rds.rs` module
  doc and architecture §10.4. Since OSMS parity G3 the local claim also writes policy
  `bEnumerateHWBeforeSW=1` (only while absent) and, when the service sends one
  (`set_frame_interval`), `WinStations\DWMFRAMEINTERVAL=<ms>` (journaled; written only while
  absent or still our own last value, someone else's value is reported as
  `PolicyOverride`; changed in place while prepared, restored when turned off, on release,
  uninstall and the boot replay). Notes: `dwm_frame_interval_set`,
  `dwm_frame_interval_restored`, `session_reconnect_for_frame_rate`,
  `dwm_frame_interval_failed`.
- **Seat ↔ session matching**: the documented RDP ActiveX control cannot set a client name, so
  `wait_session` takes the loopback remote session with our `PSEAT-NN` name **or** the newest
  loopback session that did not exist when the start began, not claimed by another seat, with
  the seat's user (if any). Seats start one at a time (service rule), which makes this safe.
- **rebind** (2026-11-24, gaming PC E2E bug 1, untested on hardware): the host reported a
  session other than the bound one (Windows reconnected its connection to the account's
  existing session; the bound one was transient). The binding moves only when WTS confirms
  the reported session (loopback, our client name, the account, not another seat's).
- **log_off** only after an ownership check (bound id + same user + remote loopback session,
  or the bound id + the seat's account user while the session is disconnected and so has no
  client name or address, HYPOTHESIS); seat 1 / console sessions are never logged off by us.
  Refusals are logged with the session's WTS view.
- **adopt** after a service restart: same id, still remote loopback, same user SID.

### Remote seats (`remote`, merged from Pulsar Split's `split-session` at step 7)

Server + RDSH (or the wrapper kind) only; `fast_user_switch` and `desktops` are local only.
The remote seat worker in `seat-service` uses `remote::manager::SessionManager`:

| Module | Job |
| --- | --- |
| `remote::manager` | `Manager` over trait objects + `WindowsSessionManager`. Start: listener re-read → headless hold spawn (`pulsar-seat-host --headless-hold`, password on its stdin pipe) → wait for `Connected` (fails at once on `client_auth` / `cert_mismatch`) → discover the session → run state. Seat cap = min(host class cap, listener `MaxInstanceCount`). The console user's SID and the SIDs of other running seats are refused. Stop: ownership check while the hold is connected, kill it, end the seat job, log off by id and wait. `tick` restarts the hold after an unexpected exit (3 failures in 5 min → `Failed(client_exited)`). `prepare_host` / `restore_host` = `Claim::Remote` on the shared writer; a created seat account is journaled (kind `account`). |
| `remote::discover` | `PSS` + 8 hex client names (FNV-1a of the seat id + salt), match on client name + user + loopback, never Services / console / listeners; session model = `crate::wts::WtsSession`. |
| `remote::sweep` | `run\remote-sessions.json` (atomic, ACL-checked), `plan_sweep_for(kind, …)`: both prefixes, never the other kind's sessions, only PIDs whose (pid, creation time) match a record. |
| `remote::users` | seat user checks via Net* APIs only (facts, SID, create, password, Remote Desktop Users from `S-1-5-32-555`), profiles in the service. |
| `remote::launch` | launch into the seat session (SYSTEM-in-session or the seat user's token), job `PulsarSeat.RemoteSeat.<seat-id>`; quoting / environment helpers are `crate::launch`'s. |
| `remote::acl` | trust rules for `run\` files the service reads back. |
| `remote::error` | `SessionError` + `FailureReason` codes (`edition_unsupported`, `coexistence`, `rds_not_ready`, `seat_limit`, `account`, `password_expired`, `rdu_membership`, `profile`, `client_auth`, `cert_mismatch`, `session_timeout`, `client_exited`, `token`). |
| `remote::win` | accounts, files, hold spawner (restricted Low-integrity token, three inherited handles, kill-on-close job, shared `seat_core::spawn` lock), launcher, process table, WTS (over `crate::win::wts`). |
| `remote::fake` | fakes for every remote trait + `fake_deps()` (feature `fake`). |

Security notes kept from Split: the password never goes to argv, environment, files or
logs; the hold inherits exactly three handles; it runs with a `DISABLE_MAX_PRIVILEGE`
restricted copy of the service token at Low integrity (fail closed with `token`); WMI
never calls `CoInitializeSecurity`; log-off only after `WTSIsRemoteSession` and a
name + user + loopback check. **Everything in `remote::win` is untested on hardware.**

### Experimental client-SKU module (`termwrap`, adapted from pulsar-split)

One module for both seat kinds (step 7: `split-termwrap` removed; its tests were the same
tests as this module's). Always compiled (status, disable, boot gate and uninstall restore
work in every build); `enable` needs the cargo feature `experimental-client-sku`
(`TermwrapPolicy::enable_built_in`) and an **x64 host** (ARM64 hosts are refused with
`wrapper_arch`). It owns `ServiceDll` and the two wrapper rows; the RDS writer never
writes them.

Ships no wrapper, no patch, no offsets; never modifies Windows files, the kernel or LSM.
Detects an operator-installed Apache-2.0 RDP Wrapper layout, refuses an unknown
`termsrv.dll` build (must be listed exactly in the wrapper's own support file), checks client
SKU, PE machine, Authenticode, local fixed path, allow-list ACLs; enable/disable through a
journal (`journal\termwrap.jsonl`) and our state file `run\termwrap-state.json`; TermService is
restarted through the SCM. Needs `experimental_ack == EXPERIMENTAL_WARNING_VERSION`.

**External wrapper mode (detect-only, 2026-11-24).** If another installed product already
pointed ServiceDll at its own wrapper at a fixed location (`%ProgramFiles%\Duo\TermWrap.dll`,
`support::WrapperKind::External`), `status()` reports `TermwrapState::External { wrapper,
blocked }`: read-only checks (client SKU, reference product absent or fully disabled, x64 host + PE machine,
Authenticode, ACLs, `EnableConcurrentSessions` and `AllowMultipleTSSessions` already `1`),
no `termsrv.dll` build gate, no write of any kind (no ServiceDll, rows, journal or
TermService restart; `enable` refuses with `external_wrapper`). The `termsrv_wrapper`
backend runs seats on it while `blocked` is `None` and the ack is given; `prepare_host` /
`restore_host` touch only the documented RDP rows. Tests: `termwrap::engine_tests::external`,
`backends::backend_tests::termsrv_wrapper_*external*`. **Untested on hardware** (the real
`WinHostFacts::expand`, the Authenticode / ACL verdict on the test PC's DLL, seats on that
wrapper).

### Second listener for passwordless seats (`loopback_listener`, `rds_loopback` only)

RDP-Tcp keeps NLA on. Seats without a stored password (logon screen) connect to a second
WinStation listener `PulsarSeat-Loopback` (port `backends.loopback_listener.port`, default
33389): copied from RDP-Tcp, NLA off only there, TLS on, same certificate hash as RDP-Tcp,
a security descriptor that lets only the seat accounts (+ `extra_accounts`) log on, and a
Windows Firewall block rule for every non-loopback remote address. It exists only while the
service's plan says a passwordless seat exists; it is refused / removed while the firewall
is off, a policy forces NLA or turns TLS off, the port is taken or no account resolves.
Journal `journal\loopback-listener.jsonl`; key and rule names are reserved, so removal also
works without a journal. TermService is restarted only while no remote session exists
(`pending_restart` otherwise). Health is checked before each passwordless start. Details:
module doc and architecture §4.1 "Update 2026-11-13 (second-listener)". **Untested on
hardware.**

## Build and test

```
set CARGO_TARGET_DIR=target\agent-session
cargo test   -p seat-session --features experimental-client-sku     (254 tests, 2026-11-14)
cargo test   -p seat-session                                         (253 tests)
cargo clippy -p seat-session --all-targets --features experimental-client-sku,fake -- -D warnings
cargo check  -p seat-session --all-targets --target x86_64-pc-windows-msvc
```

Backend tests use `fake::FakeWorld` (fake clock, WTS, launcher, registry). The `win::*` tests are
read-only probes (session enumeration, local users, edition, a session-local event, a private
desktop that is never switched to).

`console_account` (OSMS parity G6, 2026-11-18): the pure policy of the host console preset's
`PulsarSeatConsole` account (name, comment, logon rights, Winlogon value names, per-user
kiosk values, `check_engage` refusals of somebody else's auto-logon). No Windows calls; the
service applies it. Its Windows behaviour is untested on hardware (HYPOTHESIS).

## Untested on hardware (needs a Server + RDSH box / the maintainer)

- Every Windows path that changes state: registry writes of `prepare_host`, `WTSLogoffSession`,
  `WTSDisconnectSession`, `WTSConnectSessionW` from session 0 (HYPOTHESIS: needs the target
  user's password unless the caller has Connect rights), `CreateProcessAsUserW` with
  SYSTEM-in-session / user tokens, job objects, window placement of kiosk apps, the desktop
  keeper (`CreateDesktopW` in the console + global ready event).
- `TSAppCompat` as the RDSH role signal (HYPOTHESIS; WMI `TerminalServerMode` is the
  documented alternative), licensing grace days (not read).
- Loopback to `127.0.0.2` accepted by the listener (HYPOTHESIS, spec 30 §6.1.1 point 2).
- All termwrap Windows paths (see `src/termwrap/mod.rs`).
- Seat ↔ session matching with the real ActiveX client name (computer name).
- Host token `RestrictedSystemInSession` (no privileges) for the RDP ActiveX control, the
  control's `PromptForCredentials` / `PromptForCredsOnClient` vtable slots (HYPOTHESIS),
  `seat_session::win::listener::verify_rdp_listener` against a real TermService listener.
- Fast user switching: the `WTSINFOEX` lock flag of the console / target session as the
  switch gate, `LookupAccountNameW` SID matching.
- `open_trusted_append` (reparse / hard-link / owner checks) for seat-host and agent logs.
- `DWMFRAMEINTERVAL` (OSMS parity G3, x64 and ARM64): HYPOTHESIS that the `WinStations`
  value alone lifts the about-30-fps cap of our loopback sessions on Server + RDSH and under
  the wrapper, that a new value needs only a session reconnect (no TermService restart),
  and that `floor(1000 / Hz)` is smooth on a seat monitor without the 4 ms `smooth` mode.
  If `WinStations` alone is not enough, `RDP-Tcp\DWMFRAMEINTERVAL` (as OpenStreamMultiSeat
  writes) is the next step, behind its own HYPOTHESIS note.

## Not implemented end to end (gap)

- The seat host's main flow is wired (`crates/seat-host/src/host.rs`, decisions in
  `flow.rs`): hello → `host_get_assignment` → `host_take_credentials` →
  `rdp::win::RdpClient` configure / connect → `host_report` per control event, with
  disconnects classified by `autologon::classify_disconnect`; the host then exits and the
  service restarts it with back-off. **Untested on hardware** (needs a Server + RDSH box).
  Reconnect on resume (spec 30 §6.2.16) relies on that restart; nothing host-side yet.
- Per-seat clipboard policy on the ActiveX host: `rdp::ClientPolicy::from_redirection`
  derives it from `HostAssignment.redirection`. The control cannot filter, so the
  clipboard is on only when the seat policy is unfiltered (both directions, every format
  class, `cross_seat`); drives and smart cards are never redirected by the control (it
  would offer every drive and reader of the PC). Each refused part is reported
  (`clipboard_filter_needs_ironrdp`, `drives_need_ironrdp`, `smartcards_need_ironrdp`).
- The service should not auto-start seats on `fast_user_switch` (`caps().concurrent ==
  false`); the backend refuses to switch an unlocked signed-in console away
  (`console_in_use`) as a safety net.
- `credprov` (2026-11-13): CLSIDs, file name and the HKLM registration of the Pulsar
  credential provider + filter (`credprov/`), `sync` / `register` / `unregister` (our four
  keys only, filter last in / first out), `check_dll` (PE machine = native),
  `is_logonui_image` (the service's caller check), recovery value `CredProvDisabled`.
  `fast_user_switch` reports `caps().auto_logon` from `credprov::active`. The grant / handoff
  logic is in seat-service. **Untested on hardware.**

## Third-party

`serde`, `serde_json`, `thiserror`, `log`, `zeroize`, `windows-sys`, `windows` (WMI, firewall), `winreg`
(MIT/Apache-2.0; Microsoft's Rust bindings, no Microsoft DLL shipped).
The RDP Wrapper Library (github.com/stascorp/rdpwrap, Apache-2.0)
was consulted only for public file names, install locations and config section naming; it is
not bundled.
