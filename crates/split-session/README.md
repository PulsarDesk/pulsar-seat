# split-session

Multi-session support for Pulsar Split, **Path A**: Windows Server with the RD Session
Host role. Licence: GPL-3.0-only.

Spec: `spec/remote/30-session-rds.md`, `spec/remote/20-service.md` §5–§7. Contract:
`docs/split/agents/contracts.md` §1. The client-SKU wrapper (Path B) is a separate, experimental
crate (`split-termwrap`); this crate only writes the two "path B" registry rows when the
caller asks for them.

## What it does

| Module | Job |
| --- | --- |
| `edition` | EditionID / InstallationType / ProductType / build / `termsrv.dll` fixed version; `classify` → `Home`, `ClientMultiSessionUnavailable`, `ServerWithoutRdsh`, `ServerRdsh`. Home is refused. |
| `rds` | The "write" rows of spec 30 §2.3, `RdsApplier` (saves each original once under `HKLM\SOFTWARE\PulsarSplit\Session\Originals`, journals intent/done, reports policy overrides instead of fighting them, restores only values still ours), add-in check (verify only), listener / licensing / ServiceDll facts, `termservice_is_stock`. |
| `users` | Seat user checks via Net* APIs only: facts, SID, create (`NetUserAdd` level 1, normal user), password (`NetUserSetInfo` 1003), Remote Desktop Users membership resolved from `S-1-5-32-555` (`NetLocalGroupGetMembers` / `NetLocalGroupAddMembers`). Never the SAM hive. Profiles: `LogonUserW` + `LoadUserProfileW` in the service. |
| `discover` | Generated client names (`PSS` + 8 hex), WTS enumeration, match on client name + user + loopback address, never Services/Console/listeners. |
| `launch` | Launch into a seat session as SYSTEM-in-session or with the seat user's token (`WTSQueryUserToken`, retried), user environment block, one kill-on-close job `PulsarSplit.Seat.<seat-id>` per seat; command-line quoting and environment-block helpers. |
| `helper_proto` | argv / stdin password frame / stdout JSON Lines shared with `split-rdp-helper`. |
| `sweep` | `RunState` (`%ProgramData%\PulsarSplit\run\sessions.json`, atomic, ACL-checked), `plan_sweep`: only sessions of configured seat users that have a `PSS…` client name or whose id + user SID match a record; only PIDs whose (pid, creation time) match a record. |
| `acl` | Trust rules for files the service reads back (owner and every allow ACE = SYSTEM / Administrators; protected DACL `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` on `run\` and `journal\`). |
| `manager` | `SessionManager` trait + `Manager` (all logic over trait objects) + `WindowsSessionManager` (real wiring). Start: listener re-read → helper spawn (password on its stdin pipe) → wait for `Connected` (fails at once on `client_auth` / `cert_mismatch`) → discover session → RunState. Seat cap = min(host class cap from the last `host_info`, listener `MaxInstanceCount`): Home 0; client SKU without wrapper, Server without RDSH and RDSH without licensing 1. The console user's SID and the SIDs of other running seats are refused. Stop: ownership check (client name + user + loopback) while our client is still connected, then kill helper, end the seat job, log off by id (still remote, same user) and wait for it to vanish. `tick` restarts the helper after an unexpected exit, 3 failures in 5 min → `Failed(client_exited)` and log-off; it holds the seats lock only for non-blocking checks (spawn, kill, logoff, WTS enumeration run without it and commit only if the seat's start generation and phase are unchanged). |
| `journal` | Restore records (JSON Lines, write-through) in `%ProgramData%\PulsarSplit\journal\session.jsonl`; read and append fail closed when the directory or file is not SYSTEM / Administrators only. |
| `win` | Real Windows implementations (`windows-sys` 0.61, `winreg` 0.56, `windows` 0.62 for WMI). |
| `fake` | In-memory fakes for every trait + `fake_deps()` (feature `fake`, for the integrator's tests). |

Failure reason codes (`FailureReason`, snake_case): `edition_unsupported`, `coexistence`,
`rds_not_ready`, `seat_limit`, `account`, `password_expired`, `rdu_membership`, `profile`,
`client_auth`, `cert_mismatch`, `session_timeout`, `client_exited`, `token`. Every
`SessionError` has `code()`, and `ProtoError::from` puts the code first in the message.

Default timeouts (`SessionTimeouts::default()`): session appear 60 s, log-off 30 s, RDS
ready 120 s, client restart window 5 min / 3 failures.

## Security notes

- The seat password never goes to argv, environment, files or logs. The manager keeps one
  `Zeroizing` copy per running seat (needed to restart the helper) and hands it to the
  helper as one frame on an anonymous stdin pipe that is closed right after.
- The helper child inherits exactly three handles (stdin pipe, stdout pipe, NUL) through
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. The pipes and files are created non-inheritable;
  `win::process::spawn` makes them inheritable only around the one `CreateProcess` call,
  under the process-wide `win::process::spawn_lock()`, which is the shared
  `seat_core::spawn` lock. **Every other spawner in the service process holds it**
  (contracts §0.1a): `usbip.exe`, `sdbinst.exe` and the setup runner spawn through
  `seat_core::spawn::spawn_locked`, so no `Command` can inherit the pipe in that window.
- The helper runs with a `DISABLE_MAX_PRIVILEGE` restricted copy of the service token at
  Low integrity. If that token cannot be made or used, the start fails with `token`
  (fail closed); the full service token is used only after an explicit
  `WindowsHelperSpawner::with_unrestricted_fallback(true)`. It sits in a kill-on-close job
  so it dies with the service.
- A spawned process is a kill-on-drop guard until it is in its job and resumed: no error
  path leaves a suspended process behind.
- The service must not rely on this crate for process COM security: `win::wmi` never calls
  `CoInitializeSecurity` (the service process owns it, `pulsar-seat-service`) and sets impersonate + packet
  privacy on each WMI proxy.
- `run\sessions.json` (the sweep kills the PIDs it lists) and the journal are trusted only
  when owned by SYSTEM / Administrators with no other allow ACE. seat-setup creates the
  data folder that way (open question setup-11); this crate creates `run\` / `journal\`
  with a protected DACL and checks directory + open file before use.
- `TermSrvReadyEvent` is opened with `SYNCHRONIZE` only, never created. TermService is
  never restarted. A foreign `ServiceDll` stops `prepare_host` before any write.
- Log-off only after `WTSIsRemoteSession` and a client-name + user + loopback check while
  our client is connected; after the helper is gone only the stored id with the same user
  is logged off (Windows may blank client name / address of a disconnected session).

## Build and test

```
cargo test -p split-session
cargo clippy -p split-session --all-targets --features fake
cargo check -p split-session --target x86_64-pc-windows-msvc
```

Tests use only the fakes (fake clock for every timeout): no registry, account, session,
service or process of the machine is touched.

## Untested (needs the maintainer and an RDSH test host)

Everything in `src/win/`: registry writes, WMI queries (RDSH role, certificate thumbprint,
grace days), TCP listener table, `TermSrvReadyEvent` wait, Net* account calls, profile
creation (network-logon fallback is a HYPOTHESIS), WTS enumeration / log-off / user
token, console user SID, SYSTEM-in-session token, `CreateProcessAsUserW` with the
restricted Low-integrity token (HYPOTHESIS that it suffices for the helper), handle
inheritance toggling, job objects, data-dir DACL creation, WMI packet privacy. See open
questions session-1..13 in `docs/split/agents/open-questions.md`.

## Third-party

`serde`, `serde_json`, `thiserror`, `log`, `zeroize` (MIT/Apache-2.0), `windows-sys`,
`windows` (MIT/Apache-2.0, Microsoft's Rust bindings — no Microsoft DLL is shipped),
`winreg` (MIT).
