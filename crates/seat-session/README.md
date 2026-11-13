# seat-session

Session backends of Pulsar Seat (Phase 2). Licence: GPL-3.0-only. Spec: `spec/30-sessions.md`
§6, `spec/00-architecture.md` §5.2 / §5.2.1. Contract: `docs/agents/architecture.md` §4.1.

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
`wts::client_name_for` (`PSEAT-NN`), `experimental_warning_tr()` (Turkish warning),
`backends::termsrv_wrapper::TermsrvWrapperBackend::boot_check()`.

## Backends

| Kind | Module | What a seat is |
| --- | --- | --- |
| `rds_loopback` (default) | `backends::rds_loopback` | RDP session on Windows Server + RDSH; seat host connects to `127.0.0.2:<listener port>` |
| `termsrv_wrapper` (experimental, off) | `backends::termsrv_wrapper` + `termwrap` (feature `experimental-client-sku`) | same, on a client SKU through an operator-installed TermService wrapper |
| `fast_user_switch` | `backends::fast_user_switch` | own session per seat, one on the console at a time (`WTSConnectSessionW` / `WTSDisconnectSession`) |
| `desktops` | `backends::desktops` | kiosk app in the console session on the seat's monitors (shared desktop) or on a per-seat desktop object (`CreateDesktopW`, one visible at a time) |

- **Probe** (`HostProbe.availability`): edition / RDSH role (`TSAppCompat`), ServiceDll stock
  (else `foreign_service_dll`), reference product (`reference_product_present`), TermService
  running, listener port, `fDenyTSConnections` policy (`rdp_denied_by_policy`), licensing
  notes (`rds_licensing_not_configured`), listener cap → `caps().max_seats`.
- **prepare_host** (RDP backends): journaled rows `fDenyTSConnections=0`,
  `fSingleSessionPerUser=0` (machine-wide, spec 30 §6.2.13), `RDP-Tcp\UserAuthentication=1`,
  `RDP-Tcp\Shadow=0` in `journal\session.jsonl`; policies reported, never fought.
  **restore_host** = guarded reverse replay (only these rows; values changed after us stay).
- **Seat ↔ session matching**: the documented RDP ActiveX control cannot set a client name, so
  `wait_session` takes the loopback remote session with our `PSEAT-NN` name **or** the newest
  loopback session that did not exist when the start began, not claimed by another seat, with
  the seat's user (if any). Seats start one at a time (service rule), which makes this safe.
- **log_off** only after an ownership check (bound id + same user + remote loopback session);
  seat 1 / console sessions are never logged off by us.
- **adopt** after a service restart: same id, still remote loopback, same user SID.

### Experimental client-SKU module (`termwrap`, adapted from pulsar-split)

Ships no wrapper, no patch, no offsets; never modifies Windows files, the kernel or LSM.
Detects an operator-installed Apache-2.0 RDP Wrapper layout, refuses an unknown
`termsrv.dll` build (must be listed exactly in the wrapper's own support file), checks client
SKU, PE machine, Authenticode, local fixed path, allow-list ACLs; enable/disable through a
journal (`journal\termwrap.jsonl`) and our state file `run\termwrap-state.json`; TermService is
restarted through the SCM. Needs `experimental_ack == EXPERIMENTAL_WARNING_VERSION`.

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
cargo test   -p seat-session --features experimental-client-sku     (116 tests)
cargo test   -p seat-session                                         (67 tests)
cargo clippy -p seat-session --all-targets --features experimental-client-sku,fake -- -D warnings
cargo check  -p seat-session --all-targets --target x86_64-pc-windows-msvc
```

Backend tests use `fake::FakeWorld` (fake clock, WTS, launcher, registry). The `win::*` tests are
read-only probes (session enumeration, local users, edition, a session-local event, a private
desktop that is never switched to).

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

`serde`, `serde_json`, `thiserror`, `log`, `zeroize`, `windows-sys`, `winreg` (MIT/Apache-2.0).
No Microsoft DLL is shipped. The RDP Wrapper Library (github.com/stascorp/rdpwrap, Apache-2.0)
was consulted only for public file names, install locations and config section naming; it is
not bundled.
