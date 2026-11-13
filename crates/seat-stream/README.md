# seat-stream

Per-seat stream server integration for Pulsar Split (package "stream", contract
`docs/split/agents/contracts.md` §7, behaviour spec `spec/remote/80-stream-server.md`).

Each running seat gets one game-stream server process inside its Windows session. The
server is **upstream LizardByte Sunshine** (GPL-3.0), pinned tag `v2026.516.143833`,
unmodified for Phase 2 (`docs/split/agents/third-party.md`), or later the Pulsar fork with the
spec 80 patches. The binary is **downloaded and SHA-256 verified, never vendored** in
this repository: the installer writes `<install>\stream-server.json`
(`dist::ServerDist`), and the service re-checks the hash before every launch.

## What the crate does

| Module | Purpose |
| --- | --- |
| `backend` | `StreamBackend` / `SeatStream` traits (contracts §7.4); `SunshineBackend` (the path below, unchanged) and `PulsarHostBackend` (`pulsar-host.exe`, docs/split/agents/pulsar-host-backend.md): argv, data dir `seats\<id>\pulsar\`, service-owned control pipe client (`backend::pulsar::control`), per-seat secrets trait, one UDP firewall rule, `backend_missing` when not installed |
| `dist` | manifest parse/validate, streamed SHA-256 check, `verify_installed`; `HostDist` / `verify_installed_host` for `pulsar-host.json` |
| `seatdir` | `%ProgramData%\PulsarSplit\seats\<seat-id>\` layout; `SecureFs` trait (owner SYSTEM + protected DACL `O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)` on the data root, `seats\`, the seat folder and `journal\`, created top down; an existing folder that is a reparse point or has a foreign owner is refused; atomic write, log rotation keep 3) |
| `conf` | `server.conf` generator (spec 80 §2.3). Builds on `seat_core::remote::streamconf` (log level, parser; was split-core). Absolute paths, CR/LF rejected, `pulsar_*` keys only for the fork |
| `apps` | upstream `apps.json` model (unknown keys preserved), validation, `{seat}` / `{seat_id}` placeholders, shim-launcher wrapping, default "Masaüstü" list |
| `control` | `SeatControl` trait; `PipeSeatControl` (fork seat pipe, u32-LE + JSON frames, 1 MiB cap, pipelined ids, events); `UpstreamWebUi` (unmodified upstream); `https::PinnedHttps` (loopback-only, pinned certificate) |
| `firewall` | per-seat TCP/UDP rule pair (never base+1), RDP block rules, `Firewall` trait, journaled apply/remove/restore |
| `ports` | `ListenerProbe` trait, `conflicts`, spec 80 §7.2 allocation (48100 + k·100) |
| `supervise` | `SeatServer`: write files, launch as SYSTEM in the seat session, connect control, back-off watchdog (`seat_core::remote::watchdog`), events, 5 s status, graceful stop, apps reload/restart-when-idle |
| `journal` | restore records (JSON Lines, intent before / done after) |
| `win` (Windows) | real impls: `WinSecureFs`, `PipeClient`/`connect`, `SessionLauncher` (token + per-seat job object), `ComFirewall` (`INetFwPolicy2`), `IpHelperProbe`, `SystemRandom` (BCryptGenRandom), `WinConnector`, `WinHostPipes` (server end of the `pulsar-host` pipe: first instance, no remote clients, `PIPE_NOWAIT` while listening, client pid + creation time check) |
| `fake` (`cfg(test)` / feature `fake`) | in-memory fakes of every trait for the integrator's tests; `MemSecrets`, `FakeHostPipes`, `pulsar_responder` for the Pulsar backend |

## Pairing without a browser UI (and the "no local web server" rule)

- **Fork (preferred, spec 80 §8.4):** `pulsar_web_ui = disabled`, so the server opens no
  HTTP listener at all; the service talks to `\\.\pipe\PulsarSplit\seat\<seat-id>\server`
  (SYSTEM-only DACL, server pid verified with `GetNamedPipeServerProcessId`).
- **Unmodified upstream (default; OQ 28 accepted 2026-11-13):** upstream cannot turn its web-UI
  listener off. The service, and nothing else, uses that listener's JSON API on
  `127.0.0.1:<base+1>`:
  - it is the stream server's own listener; Pulsar Split adds **no** HTTP server and the
    manager only ever talks to the service pipe `\\.\pipe\PulsarSplit.Control`;
  - `origin_web_ui_allowed = pc` (upstream refuses non-loopback peers) and the firewall
    rules never open base+1;
  - credentials are **random per server start**, kept only in memory (zeroised), written
    to `credentials_file` as upstream's salted hash before launch; never `--creds`/argv,
    env, log or manager. This is stricter than the contract's DPAPI-stored credential: no
    long-lived secret exists;
  - TLS: the end-entity certificate must equal the seat's `credentials\cacert.pem`
    byte for byte; no proxy, no redirects, only `/api/` paths;
  - upstream facts (routes, body shapes, hash format, log lines) were checked against
    the pinned tag's GPL-3 source; see `control/upstream.rs`.
  - Limits: no `apps.reload` (the service restarts the server when no stream is active),
    no graceful shutdown (terminate), `pair.cancel` unsupported, pending-request events
    only when the operator chose `debug` (upstream has no info-level line for it; the
    service raises `none`/`error`/`warning` to `info` so stream state stays visible, but
    never forces `debug`, which logs LAN request query values). Without a pending event
    the manager can still submit a PIN; upstream answers `no_pending_request` if
    Moonlight is not waiting. Log lines are matched anchored to `[ts]: Level: message`
    (exact message), so client-controlled text cannot fake stream or pairing state.
    Lines mentioning `Authorization` are redacted before they reach the manager.

## Process launch

The server runs with a **duplicated SYSTEM token whose session id is the seat session**
(contract `launch_system_in_session`, spec 80 §4.3: upstream needs SYSTEM to inject input on
the seat's lock screen / UAC desktop). The seat user's token is not used for the server;
upstream itself launches apps as the session user. `SessionLauncher` puts every server in
a per-seat job object (kill-on-close, breakaway allowed), so orphaned servers die with the
service. Only the absolute config path is on the command line.

## Binary check

Before each launch `SessionLauncher::lock_binary` opens the exe with read-only sharing,
hashes it through that handle and holds it until `CreateProcessAsUserW` returns. It
refuses an exe or install folder that anyone but SYSTEM / Administrators /
TrustedInstaller can modify. Only the exe is in the manifest; DLLs next to it are
covered by the folder ACL check only.

## Build and test

```
cargo test -p seat-stream
cargo clippy -p seat-stream --all-targets
cargo check -p seat-stream --target x86_64-pc-windows-msvc
```

Tests never touch HKLM, services, the firewall configuration, drivers or accounts. A few
Windows tests are read-only or local: a temp-dir ACL/atomic-write test, an IP Helper read
(binds an ephemeral 127.0.0.1 listener), a firewall *read* of a non-existent group, a
BCryptGenRandom call, and a loopback connect to port 1 that must fail.

## Untested (needs the x64 test PC, SYSTEM, a real seat session or a server build)

- Everything Pulsar on a real host: `pulsar-host.exe` does not exist yet, so
  `WinHostPipes` (CreateNamedPipe / `PIPE_NOWAIT` accept / client check / switch to
  `PIPE_WAIT`), the handshake against a real host, capture in a seat session and the
  UDP rule are untested. The control client runs only against `pulsar_responder`.

- `SessionLauncher` (needs `SeTcbPrivilege` and an RDP seat session).
- `ComFirewall::upsert/remove/remove_group` (would change the live firewall).
- `PipeClient`/`connect` against a real fork server (the fork does not exist yet).
- `PinnedHttps` + `UpstreamWebUi` against a running upstream server (TLS pinning, Basic
  auth, `/api/pin` flow, log tail signals); the hash format is derived from source, not
  observed.
- `WinSecureFs::ensure_dir` with the production SDDL (SYSTEM + Administrators only; the
  test uses an owner-rights SDDL so it can clean up).
- A real upstream run with our `server.conf`. Every key we write exists in the pinned
  tag's `src/config.cpp` (read, not run); the parser cuts lines at `#` and treats a value
  starting with `[` as a list, so `conf::generate` rejects both. The spec 80 §3.3 capture
  gate (`ddx` / `wgc` in a seat session) is an experiment for the test PC.

## Licence

GPL-3.0-only. Sunshine is GPL-3.0 (LizardByte); it is not part of this repository. When
it is shipped, ship its licence, notices and corresponding source (spec 80 §11).
Rust dependencies: serde, serde_json, thiserror, log, zeroize, sha2, base64, ureq, rustls
(ring), windows, windows-sys (MIT / Apache-2.0 / ISC family; GPL-3 compatible).
