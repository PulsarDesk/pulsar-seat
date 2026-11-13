# Architecture (our design)

Index: [../../AGENTS.md](../../AGENTS.md). Behaviour targets are in `spec/`; this file is
how Pulsar Split is built. Open design questions: [open-questions.md](open-questions.md).

## Layout (monorepo)

| Path | What |
| --- | --- |
| `Cargo.toml` | Cargo workspace: `crates/*` + `apps/manager/src-tauri`. `default-members` = every crate under `crates/` (not the manager), so `cargo test` at the root does not need the UI built. Package crates are also `[workspace.dependencies]`. |
| `package.json` | bun workspace (`apps/*`), root scripts `test` / `build` / `check` run in every workspace. One `bun.lock`. |
| `crates/split-proto` | IPC messages (serde JSON) and pipe framing. No platform code. |
| `crates/split-core` | Pure logic: seat/global model validation, port arithmetic + allocation, `ConfigStore` trait (memory + JSON file impls), Phase 1 config helpers, watchdog state machine, `Controller` (request handling over a `Platform` trait, async seat reports, event ring). |
| `crates/split-service` | Windows service binary `pulsar-split-service.exe`: SCM glue, control pipe, shim health pipe, registry store, DPAPI, `engine/` (seat orchestration over the package traits, tested with the packages' fakes), `wiring.rs` (real package implementations), `WindowsPlatform`, setup sub-commands (`setup_cmd.rs`). |
| `crates/split-session`, `split-rdp-helper` | Path A multi-session (Server + RDSH), loopback RDP helper. |
| `crates/split-termwrap` | EXPERIMENTAL client-SKU module (operator-installed wrapper), off by default. |
| `crates/split-display`, `split-audio`, `split-stream`, `split-pads`, `split-shim-core`, `split-setup` | Packages per [contracts.md](contracts.md). |
| `apps/manager` | Tauri 2 + SvelteKit 3 + Svelte 5 (runes), `adapter-static` SPA. `src-tauri` = Rust side. |
| `drivers/`, `shim/`, `installer/` | Own drivers (unbuilt, unsigned), shim workspace (x64/x86), installer scripts. |

## Processes

```
manager (elevated, console session)            seat service (LocalSystem, session 0)
  Svelte UI --invoke--> Tauri commands             PipeServer (thread per client)
                          | pipe_client  ---JSON frames--->  Controller<Store, Platform>
                          | scm (start/stop service)          |-- RegistryStore (HKLM\SOFTWARE\PulsarSplit)
                                                              |-- WindowsPlatform (users, SCM start type)
                                                                    `-- Engine (seat workers)
                                                                          |-- split-session (helper child, sessions)
                                                                          |-- split-display, split-audio
                                                                          |-- split-stream (StreamBackend per seat: Sunshine | pulsar-host)
                                                                          |-- split-pads, split-shim-core (seat map)
                                                                          `-- split-termwrap, split-setup (coexistence)
  timer thread (1 s): Engine::tick (session restarts, server poll, pads) -> Controller::drain_reports
  pad pump thread (4 ms); shim health pipe PulsarSplit.ShimHealth (write-only for users)
```

No HTTP server anywhere. The stream server's browser UI is not used (see below).

## IPC

- Pipe `\\.\pipe\PulsarSplit.Control`, byte mode, `PIPE_REJECT_REMOTE_CLIENTS`.
- Frame = u32 LE length + JSON payload, max 1 MiB. Request `{version, id, body}`, response
  `{version, id, result: {Ok|Err}}`. `body` = `{type, data}` (snake_case). Errors carry a
  machine `code` (UI maps it to Turkish) + English `message` + optional `field`.
- `PROTOCOL_VERSION` (now **2**) is checked on every request; mismatch -> `version_mismatch`.
- v1 requests: hello, get_status, list/get/create/update/delete seat, set_seat_password
  (write-only), start/stop seat, get/set global settings, list_local_users, submit_pin.
- v2 requests (contracts §9.2): get_host_info, set_multi_session, get_termwrap_status,
  set_termwrap, get_seat_runtime, list_render_adapters, get_pad_backends, get_pair_state,
  cancel_pair, list_paired_clients, unpair_client, unpair_all, regenerate_seat_identity,
  get_apps, set_apps, get_isolation_status, set_shim_targets, set_app_masking, set_dxvk,
  poll_events.
- Errors carry `reason` (package code, e.g. `client_auth`) besides `code`; the manager maps
  `reason` first. Package-owned structures (host warnings, coexistence report, termwrap
  status, backend infos, shim health) travel as JSON values: split-proto does not depend on
  the packages.
- `start_seat` / `stop_seat` / `submit_pin` answer as soon as the work is under way;
  progress comes from the status (`state`, `phase`, `failure`, `session_id`) and from
  `poll_events` (ring of 256: seat_phase, seat_failed, pair_pending, pair_ended,
  stream_started, stream_ended, host_warning). `poll_events` waits at most 2 s, outside the
  controller lock.
- Passwords/PINs travel as `Secret` (never printed by `Debug`).

## Access control

- Pipe DACL `D:P(A;;GA;;;SY)(A;;GA;;;BA)`: SYSTEM + Administrators only. A non-elevated
  admin token has Administrators as deny-only, so the manager must be elevated (release
  build manifest `requireAdministrator`; debug builds run as invoker for `tauri dev`).
- First pipe instance uses `FILE_FLAG_FIRST_PIPE_INSTANCE`: the service refuses to start if
  someone else already owns the name.
- Dev: `pulsar-split-service --console --allow-current-user` adds the current user's SID to
  the DACL. Never use it in production.
- Seat passwords: DPAPI machine scope, stored in `HKLM\SOFTWARE\PulsarSplit\Secrets`, whose
  DACL is set to `D:P(A;OICI;KA;;;SY)(A;OICI;KA;;;BA)`. The dev file store never writes
  passwords (memory only).
- Pulsar seat secrets (identity, connect password, relay key): DPAPI under
  `Secrets\Pulsar` (inherits that DACL), handed to `pulsar-host` only over its pipe.
  The pipe DACL is SYSTEM + Administrators; the service accepts only the launched child
  (pid + creation time).

## Registry layout

See the module doc of `crates/split-service/src/registry_store.rs`. Root
`HKLM\SOFTWARE\PulsarSplit` (own root, never the reference product's), seats under `Seats\<name>`,
each with an `Id` GUID so renames keep identity.

## Ports

Seat base `p` reserves `p-5 ..= p+21` (27 ports). Valid bases 1029–65514. Blocks may not
overlap (bases at least 27 apart). Allocation (spec 80 §7.2): first free 48100, 48200, …
(step 100) whose block holds no listening port and avoids the upstream default block
47984–48010. Seat start re-checks the block (`port_in_use`). External ports (for firewall rules, Phase 2): TCP p-5, p, p+21; UDP p+9..p+11. The
web-UI port p+1 is never opened and the stream server gets `origin_web_ui_allowed = pc`.

## Seat lifecycle

`Controller` keeps runtime state per seat id: stopped / booting (+ phase) / running /
stopping / failed (+ reason code). A booting, running or stopping seat's config is locked
(update/delete -> `seat_running`). The engine runs one worker per start:

1. `user`: `ensure_seat_user` (facts, Remote Desktop Users, profile; admin = warning).
2. `session`: helper with the password on its stdin pipe, `start_session` (cancelable).
3. `display`: `choose_backend` (override `display_backend`; default `RdpSession` for remote
   seats), `attach` + `wait_ready` (30 s), else `display_timeout`.
4. `audio`: `server_keys` (default device = the session's Remote Audio endpoint).
5. `stream`: the seat's `StreamBackend` (`SeatConfig::stream_backend`, contracts §7.4)
   prepares and launches in the seat session (SYSTEM token, own job, same watchdog).
   Sunshine: `seats/<id>/server.conf` (`conf::generate`), TCP+UDP rule pair, `SeatServer`.
   Pulsar: service-owned pipe `\\.\pipe\PulsarSplit\seat\<id>\pulsar-host` created
   first, `pulsar-host.exe --split-seat <id> --data-dir … --log …`, then `hello` →
   `secrets.load` → `config.apply` → `apps.set`; one UDP rule. Firewall failure = warning.
   A backend that is not installed fails with `backend_missing` (no fallback).
6. `pads`: seat context registered on the first `pad.wanted` from the server.
7. `isolation`: seat map entry (`Runtime\Seats\<session>`) published (fatal only when
   isolation is on).
8. `autostart`: every app as the seat user (`run_as_system` ignored with a warning).

Pulsar seats: `auth.request` becomes the pair dialog's approve mode, apps picked by a
client are launched by the service as the seat user (`app.launch` → `app.launched`), the
relay key is stored DPAPI per seat. Changing `stream_backend` needs the seat stopped (the
config is locked while active).

Any failure undoes the finished steps in reverse and reports `Failed{reason}`. Stop, and a
failure found by the tick (helper gave up, session gone, server gave up after 5 crashes),
runs the same undo on a worker. Preconditions: not passive (coexistence), multi-seat on,
password stored. Service start (background thread): coexistence, termwrap `boot_check`,
`host_info`, `prepare_host` + `wait_rds_ready` (only with multi-seat on), session sweep,
pad orphans, seat map clear, then the seats marked "start with service". Shutdown: stop
all, wait up to 60 s, sweep, pad orphans, seat map clear. Stream-server crashes are handled
by split-stream's own watchdog (`server_policy`: 1 s to 60 s back-off, give up after 5).

## Manager UI

Single route (`src/routes/+page.svelte`): left pane (new seat, start/restart/stop service,
seat list, host page, settings). Seat page: "Yayın sunucusu" dropdown (Sunshine
(Moonlight uyumlu) / Pulsar, "kurulu değil" from `get_stream_backends`, locked while the
seat runs); the pair dialog has a PIN mode (Sunshine) and an approve mode (Pulsar: İzin
ver / Yalnızca izle / Reddet, one-time password on demand, write-only connect password,
relay sign-in). Host page: edition / class / RDS / licensing / warnings,
multi-seat toggle, coexistence findings, display + pad driver status with HDR availability,
experimental module card. Settings page: start type, isolation toggles + targets + per-app
masking with the anti-cheat warning, pad / display backend preference, Bluetooth. Seat
page: runtime card, settings incl. adapter dropdown, refresh, audio, stream, app list
editor, identity reset. Dialogs: new seat, delete confirm, pair (pending request via
`poll_events`, in-app PIN, paired clients, unpair), experimental warning (verbatim
`WARNING_TR`, explicit acknowledgement checkbox, wrapper choice). Every field saves on change (no Save
button), like the spec. Copy in `src/lib/i18n.ts` (Turkish). Polls status every 3 s.
SvelteKit 3 notes: config is passed to `sveltekit({...})` in `vite.config.ts` (no
`svelte.config.js`); `#lib/...` via package.json `imports`, with explicit `.ts`
extensions; tsconfig extends `$app/tsconfig`.
