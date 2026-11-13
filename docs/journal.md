# Journal

Index: [../AGENTS.md](../AGENTS.md). One dated line per finding or session; newest last.

- 2026-11-12: repo created (GPL-3, `main`, local only). Clean-room rules written. Phase 0
  analyst started on architecture, manager and service specs.
- 2026-11-12: reference product's HDR feature relies on a known-vulnerable signed driver to
  patch kernel memory (only with Memory Integrity off). Decision: Pulsar Split never does
  this; HDR goes through IddCx.
- 2026-11-12: switched to `AGENTS.md` index + notes under `docs/`.
- 2026-11-12: analyst finished spec draft v1 (00 architecture, 10 manager, 20 service).
  Reference weaknesses noted: unauthenticated localhost control API, crash-restart loop
  without back-off (~20k log entries / 16 h), reversibly encrypted seat passwords, ships
  Microsoft system DLLs, installs its own root certificate.
- 2026-11-12: maintainer: no localhost web server, app only. Named-pipe IPC; stream-server
  web UI not used. Latest Tauri 2 + SvelteKit + Svelte 5. Phase 1 started.
- 2026-11-12: maintainer: Pulsar Split is a monorepo (Cargo + bun workspaces at root).
- 2026-11-12: Phase 1 increment 1 (implementer): workspace + proto/core/service crates +
  manager app. Named pipe `\.\pipe\PulsarSplit.Control`, DACL SYSTEM+Administrators,
  first-instance flag; seat passwords DPAPI in an ACL'd `Secrets` key. Tests green (cargo 51,
  vitest 13).
- 2026-11-12: SvelteKit 3.2 changes: no `svelte.config.js` (options go to `sveltekit({...})`),
  `$lib` removed in favour of package.json `imports` `#lib/*` with explicit `.ts`
  extensions, tsconfig must extend `$app/tsconfig` and keep `$app/types`. Peer: TypeScript 6.
- 2026-11-12: Tauri 3 is alpha on crates.io; we stay on Tauri 2.12 (latest 2.x) as planned.
- 2026-11-12: specs 30–90 written (workflow, critic + revise each). 00 §7.1 trimmed (no
  termsrv patch method). Maintainer: multi-session both paths (Server default, Pro
  wrapper experimental); drivers: signed open-source third-party now, own drivers
  written but untested until signed.
- 2026-11-12: release manifest `requireAdministrator` only in release builds so `tauri dev`
  works unelevated with `pulsar-split-service --console --allow-current-user`.
- 2026-11-12: third-party research -> docs/agents/third-party.md. Defaults: VDD (display), ViGEmBus if present + usbip-win2 (pads), upstream Sunshine, IronRDP. SudoVDA rejected (installs root cert), libvirtualhid excluded (paid licence). VDD pipe is Everyone-writable (open question 19).
- 2026-11-12: `docs/agents/contracts.md` written (architect): per-package Rust APIs,
  IPC v2 additions, integration order; open questions 25–30. Display caveat: third-party
  IDD serves console seats only until OQ 20; pairing uses fork pipe or loopback `/api/pin`
  (OQ 28).
- 2026-11-12: audio package: `crates/split-audio` (endpoint discovery via CfgMgr32 + Device_SessionId, read-only TS policy, stream-server keys, RDP audio flags, state/limitations, stale-node plan). Live read-only probe on the dev laptop: console endpoints carry Device_SessionId=1 (open question audio-2).
- 2026-11-12: termwrap package: `crates/split-termwrap` (EXPERIMENTAL, path B1 only:
  operator-installed RDP Wrapper detection, fail-closed gate on its `rdpwrap.ini` section
  names, journaled ServiceDll redirect + restore, SCM restart, boot-time auto-block). Public
  facts from the Apache-2.0 upstream repo: DLL loads `rdpwrap.ini` from its own folder;
  x86/x64 only. Open questions termwrap-1..4.
- 2026-11-12: stream package: `crates/split-stream` (conf generator, apps.json, seat dir +
  ACL, fork seat-pipe client, upstream loopback web-UI client with pinned cert + per-start
  hashed credentials, firewall rule pair via INetFwPolicy2, IP Helper port probe, SYSTEM
  launcher with per-seat job object, back-off supervisor). Upstream facts checked in the
  GPL source of the pinned tag: `/api/pin` has no `pairing_id`, hash format, `#` comments in
  config. Open questions stream-1..9.
- 2026-11-12: audio review fix: stale-node removal now needs a property session id, `{3.0.…}`
  ids, a well-formed GUID equal to a live seat endpoint's suffix, a plausible WTS list
  (session 0) re-read before each removal, and a session-id re-check in the remover;
  `server_keys` no longer writes `install_steam_audio_drivers` (split-stream owns it);
  no `--audio-capture` while the mic path is unbuilt; log scan anchored on the upstream
  line layout. Open questions audio-6..9.
- 2026-11-12: installer package: `crates/split-setup` (coexistence signals 1-18 +
  passive/residue modes, driver manifest + SHA-256 + pnputil argv/parse, journal replay,
  generic restore applier, install/reconcile/uninstall/check-coexistence over traits with
  real Win32 impls + fakes) and `installer/` (drivers.json + stream-server.json with
  GitHub-digest hashes, WiX fragment, NSIS hooks, Tauri bundle snippet, fetch-deps /
  check-payload bun scripts, licence notices). Nothing executed; open questions setup-1..9.
- 2026-11-12: session package implemented (implementer): `split-session` (edition/class, RDS rows with saved originals + journal, Net* seat users, WTS discovery, launcher with per-seat job, sweep, `SessionManager` over traits + fakes) and `split-rdp-helper` (IronRDP 0.17, NLA, SHA-1 cert pin, password on stdin only, rdpsnd acks, `--dry-run`). Unit + dry-run tests green; nothing run against a real RDS host. Open questions session-1..9.
- 2026-11-12: termwrap review fixes: journal trusted only with SYSTEM/Admins owner + DACL
  (protected dir `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`, handle check), replay allow-listed to
  our 3 values, ServiceDll `previous` re-validated, ownership from HKLM state only;
  allow-list ACL gate (owner + ancestors to volume root, rename/plant rights; pure `acl.rs`);
  local fixed drive + final-path check; revocation from cache; `RestorePending` so a crash in
  disable never re-enables; SCM pending states + stop unwind. Open question termwrap-5.
- 2026-11-12 (display package): `crates/split-display` (DisplayBackend + RdpSession / VDD /
  own-IDD backends, pure modes / HDR / adapters / frame header / IOCTL buffers, fakes, 51 unit
  tests + read-only smoke test green on ARM64; x64 check clean) and `drivers/split-idd`
  (IddCx 1.10 remote-session driver source, unbuilt / untested / unsigned). Finding:
  `D3DKMTOpenAdapterFromDeviceName` needs `GUID_DISPLAY_DEVICE_ARRIVAL` interface names
  (display-adapter interface names give STATUS_INVALID_PARAMETER). VDD pipe: one UTF-16 command
  per connection, server disconnects after the reply. Open questions display-1..6.
- 2026-11-12 (shim package, implementer): `crates/split-shim-core` (seat map, pure decisions,
  AppHelp installer over a `ShimSystem` trait, journal, DXVK download/deploy/undo; 63 tests)
  and `shim/` (module `PulsarSplitShim.dll` with retour detours, launcher, shimctl). Findings:
  retour 0.3.1 fails on i686 with current Rust (use 0.4 alpha); Rust exports stdcall entry
  points undecorated on i686 (`GetHookAPIs = _GetHookAPIs@12`); launcher injection +
  `PulsarSplitInit` work in x64 and x86 processes under emulation on the ARM64 dev box (no
  hooks active: console session, empty seat map). DXVK 3.1.1 tarball: `x32`/`x64` with
  d3d8/d3d9/d3d10core/d3d11/dxgi, no LICENSE file inside. Open questions shim-1..7.
- 2026-11-12 (pads, implementer): `crates/split-pads` (ViGEm + usbip-win2 DualSense + own
  PulsarPad backends, PadManager, owners.json, orphan plan, markers, shm protocol 1,
  reports/descriptors/features, Bluetooth plan) and `drivers/split-pad` (UMDF2 HID
  minidriver source, unbuilt/untested/unsigned). ViGEmBus IOCTL layout checked against the
  public MIT ViGEmClient headers; usbip-win2 CLI from its README (no port option). 95 unit
  tests with fakes, incl. a real loopback USB/IP session. Open questions pads-1..7.
- 2026-11-12: stream review fixes: SYSTEM-owned data/seat/journal dirs (no-follow, foreign owner refused), validated firewall restore records (`sz`), anchored upstream log matching + info floor, terminating slot, locked binary hash + install-folder ACL check, non-blocking pipe frames (open-questions stream-10..12).
- 2026-11-12 display fix round: hardened SYSTEM file access (win/secure.rs: folder owner +
  DACL + reparse checks, CREATE_NEW temp + MoveFileEx), VDD pipe (SQOS identification,
  full WUDFHost path + session 0 + LOCAL SERVICE, overlapped I/O cancelled at deadline),
  journal epochs (`restored` marker), VDD seats bound to (LUID, target id), device handle
  in-flight drain before CloseHandle; driver cancel / lifecycle / event / channel fixes
  (source only, still unbuilt). Open questions display-7..10.
- 2026-11-12 shim review fixes (nothing committed): HKLM always 64-bit view; data folders
  locked (protected SY+BA DACL, owner / reparse checks), SDB written via temp in the locked
  folder, journal checked, restore allowlist, DXVK release pinned in HKLM `Shim\Dxvk`;
  module hooks `NtCreateFile`/`NtOpenFile` + kernelbase-first, deferred hooks for unloaded
  modules, threads frozen around patches, `live()` refresh outside locks, console id from
  KUSER_SHARED_DATA, lock-free fault report, health pipe `FILE_WRITE_DATA|SYNCHRONIZE` +
  SQOS, verbosity gating; launcher timeout restart, System32 copy when layered, full-path
  module match. `is_filtered_device_path` now matches GLOBALROOT form. Open questions
  shim-8..12.
- 2026-11-12 integrator: protocol v2 (split-proto: 21 new requests, 12 error codes,
  `ProtoError.reason`, v2 seat / global / status fields with serde defaults); split-core
  Controller asynchronous (SeatEvent reports, 256-event ring, `poll_events`), dev stores
  keep passwords in memory only; split-service `engine/` orchestrates session → display →
  audio → stream → pads → seat map → autostart with reverse undo, `wiring.rs` builds the
  real package implementations, 1 s timer + 4 ms pad pump + shim health pipe, SCM session
  change forwarding, setup sub-commands with restore hooks; manager host page, experimental
  warning dialog (text checked against `split_termwrap::WARNING_TR` by a test), in-app
  pairing with pending request + paired clients, runtime card, app list editor, isolation
  targets / masking. Nothing ran on a real host. Open questions integrator-1..6.
- 2026-11-12 final fixer: fixed the final-review findings. Manager pipe client verifies
  the server (SQOS identification, session 0, LocalSystem token, owner SYSTEM/Admins)
  and never sends secrets otherwise. Ports follow spec 80 §7.2 (48100 + k·100, listeners
  and 47984–48010 skipped) with a pre-start `port_in_use` check; watchdog gives up after
  5 failures in 5 min. Display names unique (core + manager). Flaky ViGEm test fixed
  (`wait_for_raw`). One shared spawn lock `split_proto::spawn` (usbip, sdbinst, setup
  runner). Root `.cargo/config.toml` (+crt-static), tauri bundle config merged (MSI
  default; release / x64 / NSIS overrides). `Secret` zeroized on drop, frame buffers
  wiped; health pipe 2 s read deadline. New requests `test_seat_credentials`,
  `enable_unlock_account`, `get_log_tail`, `reboot_host`; manager credential test
  dialog, log page, process-patching confirm/progress/reboot flow. Power events
  (suspend stops seats, resume starts enabled ones), event-log errors, last mode
  persisted, Bluetooth controller driven + `pads` restore hook, upstream log feeds the
  audio state, pair dialog orders addresses. Still open: OQ 28 decision, fork patch set,
  WRDS protocol provider, low items (final-7..11). Nothing ran on a real host.
- 2026-11-12: coverage added (`bun run coverage`; cargo-llvm-cov 0.9.1 + @vitest/coverage-v8 5.1.0). aarch64-windows profraws are all malformed (LLVM 23.1.1), so Rust coverage runs x64 under emulation; baseline 76.2 % lines (docs/agents/coverage.md). Emulation exposed a timing race in split-pads `blocking_backend_creates_in_background`; fixed with a `hold_create` gate in the fake.
- 2026-11-12: maintainer: own termsrv wrapper (from RDP Wrapper Apache-2.0 + pinned community ini), experimental/off by default; queued after the stream-backends workflow.
- 2026-11-12: maintainer: own termsrv wrapper on by default on client SKUs (install-time notice, off switch kept, fail-closed on unknown builds).
- 2026-11-12 (design-pulsar-host): wrote docs/agents/pulsar-host-backend.md, the contract between the service and a future headless `pulsar-host` (second stream backend per seat, Sunshine stays unforked). Launch via the Sunshine launcher/job, per-seat data dir under seats\<id>\pulsar, service-owned pipe \\.\pipe\PulsarSplit\seat\<id>\pulsar-host, secrets only over the pipe, split-pads owns all pads. Open: pulsar-host-1..12. Nothing implemented.
- 2026-11-13 (impl-stream-backends): `StreamBackend` / `SeatStream` in split-stream; Sunshine moved behind it with no behaviour change (all old tests green), `PulsarHostBackend` against pulsar-host-backend.md with a fake pipe; per-seat `stream_backend` in split-proto / registry; engine routes per seat; manager selector + approve-mode pair dialog. Learned: Pulsar and Sunshine share the UDP rule name, so a backend switch must drop the Sunshine TCP rule (done in `apply_pulsar_seat_rules`). Open: stream-backends-1..5.
- 2026-11-13 (fix-stream-backends): applied the two stream-backend reviews. Learned: with `PIPE_NOWAIT`, `ConnectNamedPipe` returns nonzero after `DisconnectNamedPipe` meaning "listening", not "connected"; `RegSetKeySecurity` on a parent does not re-protect an existing subkey, so each secrets subkey sets its own DACL; serde_json error text can quote the offending value, so secret-bearing parse errors report only category + position.
- 2026-11-13: own termsrv wrapper work stopped by the model provider's safety system; not implemented, not retried.
- 2026-11-13 (final-low-items): maintainer accepted OQ 28 (loopback upstream `/api/pin`). VDD restore hook for uninstall + reconcile; DXVK status / user picture / audio cleanup requests; manager QR, avatars, DXVK page, file dialog, nav session + streaming, GPU warnings, mic toggle (plumbing only). Learned: the pure TS QR encoder matched qrcodegen 1.8.0 bit for bit on 6 texts x 8 masks (versions 1-9), so mask choice is the only place the two may differ; Sunshine upstream gives no client name on connect, so "connected client" is a streaming flag for Sunshine seats. Stale 2026-11-12 progress notes merged into status.md and deleted.
