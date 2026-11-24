# seat-service (`pulsar-seat-service.exe`)

GPL-3.0-only. The Pulsar Seat service: LocalSystem, session 0. Owns the config, the seat
state machine, device assignment, seat secrets (LSA), every system change and the control
pipe. Spec: `spec/local/20-service.md` §13, `spec/local/00-architecture.md` §5.5–5.7; contract:
`docs/agents/architecture.md`. Module map: `src/main.rs`.

## Build and test

```
set CARGO_TARGET_DIR=target\agent-service
cargo build -p seat-service
cargo test -p seat-service
cargo clippy -p seat-service --all-targets -- -D warnings
cargo check -p seat-service --all-targets --target x86_64-pc-windows-msvc
```

Dev run without the SCM (memory-only secrets, data folder of your choice, the current
user added to the pipe DACL). Console mode serves `\\.\pipe\PulsarSeat.Control.dev`
unless `--pipe` names another pipe, so a dev run never poses as the service; the client
sub-commands use `PULSAR_SEAT_PIPE` when it is set:

```
pulsar-seat-service --console --data-dir %TEMP%\seat-dev --allow-current-user
set PULSAR_SEAT_PIPE=\\.\pipe\PulsarSeat.Control.dev
pulsar-seat-service status
```

On the production pipe the CLI first checks that the pipe server is the running service
(`GetNamedPipeServerProcessId` == the PID the SCM reports); every other client (manager,
app API) must do the same before sending anything (the manager and `seat-compat` do).

Elevated: `install [--exe PATH]`, `uninstall`, `enable`, `disable`, `reset`, `report`.

## What it does

- SCM: start-pending with wait hint, accepts stop, pre-shutdown, power events
  (`PBT_APMRESUMEAUTOMATIC` → re-read devices, re-apply ownership, hosts reconfigure) and
  session changes. Session changes and resume are queued and handled in order on one
  worker. Stop runs on a worker and the stop-pending checkpoint advances every 3 s and
  after each seat. Install sets description (localized), recovery actions (restart 5 s,
  30 s, 60 s, then none, reset after 1 day: one restart per allowed crashed start, so the
  third restart enters the crash-loop safe mode with the default `crash_loop_limit` 3),
  unrestricted SID type, required privileges, pre-shutdown timeout, an explicit service
  DACL without user-defined control rights for users, the Event Log source and the data
  folders.
- Data folders: `%ProgramData%\Pulsar Seat` and every sub-folder are taken over safely
  (`win::security::ensure_dir`): new folders are created with `O:BA` + the protected
  DACL in one call; an existing junction / symbolic link, a non-folder or a folder whose
  owner is not SYSTEM / Administrators is renamed aside (`*.untrusted-<pid>-<n>`) and
  recreated; trusted folders get owner + DACL by handle, and children are reset to
  inherit. `diag\`, `run\`, `journal\` are SYSTEM + Administrators only. Then
  `config.json`, `run\state.json` and the logs are checked: a reparse point or a hard
  link is moved aside.
- Pipe: `\.\pipe\PulsarSeat.Control`, `PIPE_SDDL`, first-instance flag, reject remote,
  overlapped I/O (a subscribed connection is read and written from two threads). Caller
  = PID + token user SID + token session + Administrators enabled / SYSTEM, read after the
  first frame by impersonation. Every request is checked against `RequestBody::access()`.
  Seat hosts are verified by PID + one-time token, agents by the PID the service launched
  in that session; host / agent topics are scoped; `admin_only` never blocks their status
  access. Limits: 64 instances, 16 connections per client session, 10 s write timeout
  (a client that stops reading is disconnected), 4096 queued events per subscriber (a
  lagging subscriber is dropped and its connection closed), no `FlushFileBuffers` on
  close. Until boot phase 1 (run state, safe mode) has run, only status is served; phase
  1 runs before the pipe is claimed.
- Config: `%ProgramData%\Pulsar Seat\config.json` (atomic, `FileStore`), validation with
  `seat_core::validate`, revision check, conflicts need `force`, experimental backends
  need the acknowledgement. Run state in `run\state.json` (SYSTEM + Administrators only).
- Safety: Windows Safe Mode → status only; unclean previous shutdown (our own `running`
  flag across boots) and crash loop (`unstable_starts` ≥ `crash_loop_limit` in one boot)
  → product safe mode, no autostart, manual start allowed, `set_enabled(true)` clears it.
  OS build change → notice, not blocking.
- Seats: start (display bind → session backend → host launch with token → wait session →
  audio bind → ownership re-plan → agent at logon), stop (host exit, log off, release),
  re-logon, session end → back to the logon screen after `relogon_delay_ms`, same-account
  policy (warn = degraded + notice; deny = notice, then the service itself logs the
  session off after 15 s whatever the agent does), watchdog: host exits restart with
  back-off 2 s, 4 s, … ≤ 60 s, 3 exits in 10 min → `error{host_exited}`; a start still in
  `starting` after `seat_start_timeout_s` + 60 s → `error{session_timeout}`. Failures carry
  `step=<phase> reason=<reason> [win32=<code>]`. Logon failure marks the stored password
  failing (no retry loop) until it is set again. A stale start worker (its generation was
  bumped) only logs off the session it got and never touches the slot; a failure the host
  reports bumps the generation too. After a host exit a signed-in seat keeps its session
  (the restarted host is reconnected to it and the seat follows the host's report; startup
  apps are not started again), a seat at the logon screen is logged off; this recovery runs
  off the watchdog thread. A manual start clears the crash window. No restart while
  `SM_SHUTTINGDOWN` reads true (HYPOTHESIS that it does during the session log-offs of a
  shutdown). The session poll catches up a logon the seat missed (gaming PC E2E round 2,
  bug B); every SCM session change is logged with the seat sessions at that moment.
- Topology: `seat_one_console` (seat 1 = console session) and `dedicated_host` (every
  seat, seat 1 included, is started; console session changes are host-session events).
- Processes: seat hosts run with the service's SYSTEM token moved into the target
  session and a SYSTEM-only process DACL (they receive the seat credentials; the session
  user cannot read their memory or command line); agents run with the session user's
  token. The platform acts only on process handles it holds: a host recorded in
  `state.json` is adopted only if its PID still has the same creation time and image
  path. Exited processes' handles are closed. The host token comes from `BCryptGenRandom`
  and a failure stops the start.
- Backends: `wiring::PackageFactory` builds the package backends (session, display with a
  console helper runner, input, audio, gates, `seat_media` PnP tree) once per kind;
  `import_reference` uses `seat_compat::importer`.
- Inventory: every backend's `enumerate()` plus a CfgMgr / CCD baseline for kinds no
  active backend covers; absent saved devices are kept (present = false); a known device
  on a new port gets `moved_from`.
- Logs: `logs\service.log` (4 MiB, 3 rotations, UTC), errors and start / stop also to the
  Event Log with our own ids (`src/eventid.rs`): localized text per id (`seat_core::i18n`), English detail
  after it. The diagnostic report masks user names (seat table and log tail) and never
  contains a password or token. `get_config` never decrypts secrets ("password set" is
  cached per seat; `has` wipes the LSA buffer without copying it). Passwords are limited
  to 256 characters. CLI output is localized (English default, Turkish locale; `cli::copy`).
- Console lock guard (OSMS parity G4, `src/console_guard.rs`): decides whose per-user
  `DisableLockWorkstation` / `ScreenSaveActive` values `seat_compat::console_guard` writes
  (the `dedicated_host` console account, only when it is the host account: with the host
  console preset wanted only `PulsarSeatConsole`, otherwise never a seat's account; seat
  1's user only with the opt-in, while other seats run), syncs `NoClose` for seats with `no_power_menu` and adds the `host_sleep_off` /
  `console_lock` checks to `Preflight`. All writes are compat-journal entries, undone on
  off, disable, the first sync after a crash (seat 1) and uninstall. Details in
  docs/agents/architecture.md "Console lock guard".
- Boot waits for services with `NotifyServiceStatusChangeW` and alertable waits (no
  polling). A service that does not exist is reported at once (`PlugPlay` does not exist
  on Windows build 28000, the dev box: the boot logs `services not running after wait`).
- Host console preset (`host_console`, `win::host_console`; OSMS parity G6, 2026-11-18):
  under `dedicated_host` with the opt-in `settings.backends.host_autologon`, the service
  creates `PulsarSeatConsole` and turns on the documented Winlogon auto-logon for it
  (password only as LSA `DefaultPassword`), journaled in `journal\host-console.json` and
  restored when the preset, the topology or the product goes off and at uninstall. Someone
  else's auto-logon is refused untouched. Design: architecture.md "Host console preset".

## Gaps

- Seat hosts and the console listener get their one-time token on stdin (`--token-stdin`,
  an inherited anonymous pipe, `platform::token_frame`) and run with a restricted SYSTEM
  token (no privileges) and a SYSTEM-only process DACL. Untested on hardware.
- `audio` shared fallback: `seat_media::AudioRouterImpl::set_endpoint_masks` is not called
  yet (the factory hands out the trait object).
- Installer CLI verbs requested by the installer (`preflight`, install flags) are not here.
- Baseline audio keys use the endpoint instance (`inst=SWD\MMDEVAPI\…`), not the media
  package's physical anchor; baseline keys are a fallback only.
- Modern standby: only the automatic-resume power event is handled (no console display
  on/off notifications yet).
- In `--console` dev mode the gate journals refuse the user-owned `run\` folder (fail
  closed by `seat-media`), so gates are inactive there.

## Untested on hardware

- In-session capture broker (OSMS-parity G1, `capture.rs`, `win/handle_broker.rs`): the
  agent's capture offer, `host_capture_ring` and `DuplicateHandle` from the agent into the
  contained seat host (HYPOTHESIS: a low-integrity host can use the duplicated texture,
  section and event handles). Unit-tested with the mock platform only.

Everything that needs LocalSystem or elevation: running under the SCM, install /
uninstall, folder take-over as SYSTEM (owner `BA`, rename aside), LSA private data,
`WTSQueryUserToken` / `CreateProcessAsUserW` launch of agents, SYSTEM-token launch of
hosts into a session (HYPOTHESIS: the RDP client in the seat host works as SYSTEM in the
user's session), the display helper with a restricted SYSTEM token (HYPOTHESIS: CCD works
without privileges on the console desktop), `WTSLogoffSession`, reboot, Event Log
messages (message DLL path, ARM64 `FrameworkArm64` is a HYPOTHESIS), required-privilege
list completeness (HYPOTHESIS), pre-shutdown ordering and checkpoints, resume handling,
session-change mapping on real RDP sessions, every package backend.
Console lock guard (HYPOTHESIS): `DisableLockWorkstation` stops Win+L for an auto-logon
account, the per-user screen-saver policy, `NoClose` in a running Explorer, the removed
`BUILTIN\Users` shutdown right from the next sign-in.
Host console preset (HYPOTHESIS, all of it): Winlogon signs the hidden `PulsarSeatConsole`
in automatically with the password only in LSA `DefaultPassword`; `DisableLockWorkstation`
in the user's own policy key blocks Win+L under auto-logon; the screen-saver policy is read
at logon; the console account's session is enough for the seat hosts after every boot; the
uninstall restore and the "signed in, removed later" path.

## Physical pad pass-through and Guide-button guard (OSMS parity G5)

Under the kill switch `settings.input.pad_device_scope` (default off). `src/local_pads.rs`
finds pads by setup class (XUSB, GIP, HID collections with `HID_DEVICE_SYSTEM_GAME`) with
their whole pad subtree (`seat_input::pad_scope::discover`), routes them by their HID unit
or by the inventory device / USB port above them, and scopes them to local seats in
`PadMode::Passthrough` (per seat, the default). The logon SID replaces the user SID when
one user is signed in to two sessions. `src/pad_runtime.rs` re-applies the cosmetic
`DEVPKEY_Device_SessionId` after each device arrival and logs whether HidHide is installed
(`seat_input::pad_jail`, detect only: never installed, loaded or used). `src/guide_guard.rs`
drives `seat_compat::guide_guard`: Game Bar's controller button off for the seat users and
the console user, `ControllerToVKMapping\Enabled` = 0, journaled in the seat-compat
journal, restored on seat stop, service stop and the first tick after a crash. No code was
copied from OpenStreamMultiSeat (MIT); it was read as a behaviour reference only.
HYPOTHESIS end to end (never ran on hardware, x64 or ARM64); details in
`docs/agents/architecture.md`, section "Physical pad pass-through and Guide-button guard".
