# Status / resume point

Index: [../../AGENTS.md](../../../AGENTS.md). Update after every step.

- 2026-11-12: repo created (GPL-3, `main`, local only, no remote, nothing committed).
- **Phase 0 (spec): draft v1 done** for `spec/00-architecture.md`, `10-manager.md`,
  `20-service.md`. Pending rows `30`–`90` listed in [spec/README.md](../../../spec/remote/README.md).
  - Gaps: no live IPC captured (reference service was stopped); certificate stores / boot
    settings not read (probe timed out); frame/pad shared-section layouts unknown (we
    define our own); stopped seat = logoff or disconnect? unknown; LAN reachability of the
    control API unknown.
  - Things to do better than the reference: authenticated local IPC, watchdog with
    back-off, no reversible password storage, no shipped Microsoft DLLs, no self-installed
    root certificate.
- **Next: Phase 1** (implementer reads `spec/` only):
  1. Service skeleton + registry/config model + secure local IPC.
  2. Seat create/delete with name, port and user validation.
  3. Per-seat stream-server config generation, start/stop, back-off watchdog.
  4. Manager screens (settings, seat page, new seat, pair dialog).
  Then `30-session-rds.md` opens Phase 2.
- 2026-11-12 (ultracode): spec workflow `pulsar-split-specs` (run `wf_6e9f173f-6cd`)
  writing specs 30–90 with critic + revise per spec; Phase 1 implementer running in
  parallel. After both: implementation workflow for phases 2–4 (implementers read
  `spec/` only), then adversarial review.
- Phase 2 decisions taken 2026-11-12 (see plan.md): multi-session both paths; drivers =
  signed open-source third-party now, own drivers unbuilt/untested.
- 2026-11-12: specs 30–90 done. Implementation workflow `pulsar-split-implement`
  (run `wf_d5910055-d95`) running: research + contracts → 8 packages (session, termwrap,
  display, shim, pads, audio, stream, installer) → 2-lens review → fix → integrate →
  final review + completeness → fix. Resume with the run id if interrupted.
- 2026-11-12: **Phase 1, increment 1 done (implementer), untested by the maintainer, not
  committed.** Monorepo skeleton (Cargo + bun workspaces; `drivers/`, `shim/`, `installer/`
  reserved). Design: [architecture.md](architecture.md); open points:
  [open-questions.md](open-questions.md).
  - `split-proto` (pipe messages + framing), `split-core` (validation, ports, stores,
    stream-server config, watchdog, controller), `split-service` (SCM service +
    `--console`, pipe with SYSTEM+Administrators ACL, HKLM registry store, DPAPI secrets
    key), `apps/manager` (Tauri 2.12 + SvelteKit 3.2 + Svelte 5.57, Turkish UI: settings,
    seat page, new-seat, delete, pair dialogs).
  - Green: `cargo test` (51), `cargo clippy --workspace --all-targets` (clean),
    `bun run test` (13), `bun run check` (0/0), `bun run build`,
    `cargo build --release -p pulsar-split-manager -p split-service`,
    `cargo check -p split-service --target x86_64-pc-windows-msvc`. Binaries ARM64 (0xAA64).
  - Manual: console service over the real pipe (create/duplicate/start/users/status) ok;
    Medium-IL client denied by the pipe ACL.
  - Stubbed (Phase 2): seat start after writing config (`not_implemented`), stop, PIN
    forwarding, isolation toggles (stored only), firewall rules.
  - **Next:** maintainer runs the manager elevated against `--console` service; then
    spec `30-session-rds.md` / `80-stream-server.md` and Phase 2 session creation.
- 2026-11-12: **Integration done (integrator step), nothing committed, nothing run on a
  real host.** Resume point for the final review.
  - `split-proto` **v2** (contracts §9): new requests/responses, error codes + `reason`,
    seat `rdp_client_name` / `audio` / `stream`, display `render_adapter_id` /
    `refresh_hz` / `luminance`, global `multi_session` / `pad_backend` /
    `bluetooth_console_only` / `display_backend`, status `failure` / `session_id` /
    `phase` / `passive` / `host_class`.
  - `split-core` Controller: asynchronous start/stop with `SeatEvent` reports, event ring
    + `poll_events`, passive-mode guards, `set_multi_session`, termwrap switch stops seats.
  - `split-service`: real seat start/stop (no more `not_implemented`): `engine/` (start
    order user → session → display → audio → stream → pads → seat map → autostart, undo in
    reverse), `wiring.rs` (WindowsSessionManager, RdpSessionDisplay + VDD + own IDD probe,
    DXGI adapters, audio probe, SeatServer with SYSTEM-in-session launcher + loopback
    control, firewall, PadManager with ViGEm / usbip-win2 / own pad, AppHelp isolation,
    RegSeatMap, WindowsTermwrap, coexistence probe), timer / pad pump / health pipe,
    session-change forwarding, setup sub-commands (`install|reconcile|uninstall|
    check-coexistence`) with restore hooks. Registry schema 2.
  - Manager: host page (multi-seat toggle, driver backend status, HDR availability,
    coexistence, experimental module behind the warning dialog), settings (isolation
    targets / masking, pad + display backend), seat runtime card, adapter dropdown, audio /
    stream settings, app list editor, identity reset, in-app pairing dialog.
  - Green 2026-11-12: `cargo test --workspace` (all crates incl. 19 service tests with the
    packages' fakes), `cargo clippy --workspace --all-targets` (0 warnings),
    `cargo check --workspace --target x86_64-pc-windows-msvc`, `bun run test` (19),
    `bun run check` (0/0), `bun run build`.
  - Untested on hardware: everything under the packages' `win/` modules, the helper's
    real RDP connection, stream server launch in a seat session, firewall COM, AppHelp
    install, pads, termwrap enable/disable, health pipe, SCM session-change path.
  - Open: integrator-1..6 in open-questions.md (spawn lock across packages, pad input
    wait, restore hooks for display/pads, contract deviations, unpersisted last mode +
    Bluetooth, console mode needs elevation).
  - **Next:** final review; maintainer test on a Windows Server + RDSH host (one seat,
    then two), then on a client SKU with the experimental module.
- 2026-11-12: **Final fixer done (whole repo), nothing committed, nothing run on a real
  host.** Findings of the final review fixed as listed below; open points in
  open-questions.md final-1..11.
  - Green 2026-11-12: `cargo test --workspace` (680 passed, 0 failed, 2 ignored),
    `cargo clippy --workspace --all-targets` (0 warnings),
    `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc`, shim
    workspace `cargo test` (8) + `cargo check` x64 / i686 + clippy x64, `bun run test`
    (manager: 7 files, 23 tests), `bun run check` (0/0), `bun run build`,
    `bun test installer/scripts` (15). Debug `pulsar-split-service.exe` /
    `pulsar-split-rdp-helper.exe` import no VC runtime / UCRT (root `.cargo/config.toml`
    +crt-static). The manager exe imports only the inbox UCRT api-sets
    (`api-ms-win-crt-*`, part of Windows 10/11; nothing to ship), no `vcruntime140`.
  - **Done (code + unit tests with fakes):**
    - Manager pipe client: SQOS identification; server pid in session 0, LocalSystem
      token, pipe owner SYSTEM/Administrators; secrets (`set_seat_password`,
      `submit_pin`) refused otherwise; release builds refuse all requests.
    - Ports: spec 80 §7.2 allocation (48100 + k·100, listeners and 47984–48010 skipped,
      `Platform::listening_ports`); pre-start `check_free` → `port_in_use` + ports;
      watchdog sliding window 5 failures / 5 min.
    - Display names unique case-insensitively (service + manager validation + i18n).
    - Spawn rule: shared `split_proto::spawn` lock (helper, usbip, sdbinst, setup runner);
      contracts §0.1a.
    - Installer wiring: bundle config merged into `tauri.conf.json` (MSI default, WiX
      fragment, upgradeCode, tr-TR, NSIS hooks); `installer/tauri.release.conf.json`
      (payload resources), `tauri.x64.conf.json` (shim components),
      `tauri.nsis.conf.json` (service + helper as externalBin); bun scripts
      `installer:msi`, `installer:msi:x64`, `installer:nsis`; `tauri-bundle.json` removed.
    - `Secret` zeroized on drop; control-pipe frame buffers wiped.
    - Health pipe: 2 s read deadline (`CancelSynchronousIo` watchdog), slots freed.
    - Flaky `vigem::x360_lifecycle_and_feedback` fixed (`RecordingSink::wait_for_raw`).
    - Test User Credentials (spec 10 §4.4): `test_seat_credentials` (network logon with
      the stored password, Windows error text, disabled / locked / expired flags),
      `enable_unlock_account` only after an in-dialog confirmation; manager dialog.
    - Process patching flow (spec 10 §3.2): toggle reverted, direction-specific confirm
      (3 bullets, red warning), progress (≥ 3 s), reboot dialog → `reboot_host`.
    - Log viewer (spec 10 §10): `get_log_tail` (service log / seat `server.log`,
      redacted), manager "Günlükler" page.
    - Power events: suspend stops seats, resume (`PBT_APMRESUMEAUTOMATIC`) starts the
      enabled ones after the old ones stopped.
    - Event log: service-mode errors to Application log, source `PulsarSplit`.
    - Last applied mode persisted (`LastWidth/LastHeight/LastRefreshRate`, service-owned
      `SeatConfig.last_mode`), used as session size at next start.
    - Bluetooth per-user services driven (60 s + session change + setting change),
      restored at service stop and by the new `pads` uninstall hook.
    - Upstream log → `split_audio::LogScanner` → `audio.state` (no-audio status for the
      default flavour).
    - Pair dialog: physical adapters first, recommended address marked; controller-audio
      limitation hint next to the pads runtime line.
  - **Untested on hardware:** pipe server identity check (incl. that an elevated admin
    may open the SYSTEM service token with `TOKEN_QUERY`), IP Helper probe in create /
    start, `LogonUserW` network test + `NetUserSetInfo` 1008, `InitiateShutdownW`
    restart, SCM power events, event-log writes, Bluetooth SCM calls, MSI / NSIS builds
    with the merged config (Tauri template assumptions still HYPOTHESIS), crt-static
    release builds, plus everything already listed untested above.
  - **Missing:** stream-server fork patch set (final-8, high); WRDS protocol provider
    for the own IDD (final-9); remaining final-10 items and the microphone path
    (final-12). OQ 28, most final-10 items and the display restore hook were done on
    2026-11-13 (entry below).
  - **Next:** maintainer decides final-9; maintainer test on a Windows Server + RDSH host
    (one seat, then two; check the pipe identity check and port checks first).
- 2026-11-12: maintainer: Pulsar becomes a second stream-server backend per seat (Sunshine stays, unforked, for Moonlight). Own drivers stay optional, never default. Workflow `pulsar-split-stream-backends` (run `wf_e22fa7f9-77a`): design note docs/agents/pulsar-host-backend.md, `StreamBackend` trait (Sunshine + PulsarHost), manager selector, review, fix. pulsar-core / pulsar-desktop read-only. Resume with the run id if interrupted.
- 2026-11-12: design note [pulsar-host-backend.md](pulsar-host-backend.md) done (contract service <-> `pulsar-host`; launch, per-seat isolation, control pipe + message set, manager mapping, capture unknowns, pads owned by split-pads, change list for pulsar-core / pulsar-desktop). Open: pulsar-host-1..12. Design only.
- 2026-11-13: **stream backends implemented (impl-stream-backends), nothing committed, nothing run on a real host.**
  - split-stream `backend`: `StreamBackend` / `SeatStream` traits (contracts §7.4);
    `SunshineBackend` wraps the unchanged `SeatServer` path; `PulsarHostBackend`
    (argv `--split-seat/--data-dir/--log`, data dir `seats\<id>\pulsar\`, service-owned
    pipe created before launch, `hello`/`secrets.load`/`config.apply`/`apps.set`, events,
    watchdog, one UDP rule, DPAPI per-seat secrets via `PulsarSecretStore`); missing
    `pulsar-host` = `backend_missing`, never a Sunshine fallback. `win::WinHostPipes`.
  - split-proto: `SeatConfig.stream_backend` (default `sunshine`), `PairMode` + Pulsar
    fields on `PendingPair`, `PairState.backend/relay_auth_required`, requests
    `get_stream_backends`, `answer_peer_request`, `set_seat_connect_password`,
    `get_seat_otp`, `rotate_seat_otp`, `answer_relay_auth`; registry `StreamBackend`.
  - Engine picks the backend per seat; Pulsar app launches run as the seat user.
  - Manager: "Yayın sunucusu" dropdown ("kurulu değil"), pair dialog PIN / approve modes.
  - Green 2026-11-13: `cargo test --workspace` (704 passed, 0 failed, 2 ignored),
    `cargo clippy --workspace --all-targets` (0 warnings), `cargo check --workspace
    --target x86_64-pc-windows-msvc`, `bun run test` (9 files, 32 tests), `bun run check`
    (0/0), `bun run build`.
  - Untested: `WinHostPipes` and everything with a real `pulsar-host` (it does not exist).
    Open: stream-backends-1..5 in open-questions.md.
- 2026-11-13: **stream-backend review fixes (fix-stream-backends), nothing committed,
  nothing run on a real host.**
  - Done: host pipe accept after a disconnect (nonzero = listening); `Secrets\Pulsar` own
    protected DACL, in-memory in dev file-store mode, non-fatal when unopenable; engine
    preflight before logon (`backend_missing` / `server_hash_mismatch`); seat delete
    removes Pulsar secrets even when the firewall step fails, and Pulsar logs with
    `keep_pairing`; "Masaüstü" answers `ok` without pid; no secret values in host-response
    parse errors; `update_seat.stream_backend` optional (absent = kept); manager: backend
    dropdown rolls back on a failed save, capture shown for both backends, connect password
    removed only via a confirmed "Şifreyi kaldır"; log filter hides OTP / relay key /
    identity lines; design note status + implemented values; §7.4 references.
  - Green 2026-11-13: `cargo test --workspace` (708 passed, 0 failed, 2 ignored),
    `cargo clippy --workspace --all-targets` (0 warnings), `cargo check --workspace
    --target x86_64-pc-windows-msvc`, `bun run test` (9 files, 32 tests), `bun run check`
    (0/0), `bun run build`.
  - Untested: the real pipe reconnect path and the HKLM DACL repair on a real host (unit
    tests cover the decision logic and an HKCU key).
  - Not done by this run: `caps.rs` / `caps_guard.rs` in pulsar-desktop (newer than the
    design note, unrelated to stream backends) were not written here; the maintainer
    should confirm their owner.
  - Next: maintainer test (Sunshine seat unchanged, then a Pulsar seat once `pulsar-host`
    exists).
- 2026-11-13: stream-backends workflow done (708 cargo tests, 32 vitest, clippy 0; verified by orchestrator). Own termsrv wrapper workflow `pulsar-split-own-termwrap` (run `wf_20718968-e91`) running: research (RDP Wrapper Apache-2.0, community ini licence) -> termwrap/ x64 DLL + ini parser + split-termwrap default-on + manager/installer notice -> 2-lens review -> fix. Resume with the run id if interrupted.
- 2026-11-13: own termsrv wrapper workflow (`wf_20718968-e91`) **stopped by the model provider's safety system**; research and implementation agents produced nothing, no files changed. Not retried. Current state unchanged: `split-termwrap` only integrates an RDP Wrapper the operator installed (experimental). Server + RDSH stays the supported multi-session path. Plan.md 'own wrapper / default on' decisions are recorded but **not implemented**.
- 2026-11-13: **final low items (final-low-items), committed locally on `main`, nothing run
  on a real host.**
  - OQ 28 **accepted** by the maintainer: the loopback upstream `/api/pin` path is the
    Sunshine default (service-only client, credentials hashed per server start, pinned
    certificate, `127.0.0.1` only, no firewall rule for base+1). No longer a release
    blocker; rules.md "Local services" keeps the strict rules.
  - Display restore hook: `setup_cmd::DisplayHook` (uninstall **and** reconcile) restores
    the journaled `vdd_settings.xml` (which holds the display count) and reloads the
    driver; a missing driver only skips the reload, a missing settings folder closes the
    journal epoch (nothing left to restore, next run is a no-op). `wiring::vdd_backend` is shared with
    the service. Closes the integrator-3 remainder.
  - IPC (protocol 2, new variants only): `get_dxvk_status`, `get_user_picture`,
    `cleanup_audio_endpoints`; `SeatStatus.streaming` / `stream_client`. DXVK games are
    remembered in `HKLM\SOFTWARE\PulsarSplit\Shim\DxvkGames`. The status read of each
    game's marker (SYSTEM, user-writable folder) refuses reparse points / non-files and
    caps it at 64 KiB (`engine::read_dxvk_marker`).
  - Manager: pair dialog QR code (pure TS encoder `src/lib/qr.ts`, checked module for
    module against qrcodegen 1.8.0 for 48 cases, versions 1-9; capped at version 9 /
    180 bytes); account-picture avatars (nav + seat
    page, initial fallback); DXVK page; microphone toggle stored (path not built,
    final-12); `pick_exe` file dialog (tauri-plugin-dialog 2.8, MIT / Apache-2.0) for
    autostart apps and DXVK games; session id + "yayında" in the nav; GPU warnings; host
    page stale audio endpoint cleanup with confirm.
  - Skipped: deny-log-on-locally seat option (LSA rights + journal + uninstall hook; medium),
    DPI readback warning (needs a service-side readback), connected client *name* for
    Sunshine (upstream logs none). Listed in open-questions final-10 / final-12.
  - Progress notes of the 2026-11-12 package runs (architect-contracts, audio, display,
    installer, pads, phase1-core, session, shim, stream, termwrap, third-party-research)
    merged here and deleted: their results are in the entries above, their build / test
    commands and "Untested" lists in each package README (`crates/*/README.md`,
    `shim/README.md`, `installer/README.md`, `drivers/*/README.md`), their open points in
    open-questions.md (audio-*, display-*, setup-*, pads-*, session-*, shim-*, stream-*,
    termwrap-*, 19-30). Still true from them: nothing under any `win/` module ran on a
    real seat host; driver sources (`drivers/split-idd`, `drivers/split-pad`) are unbuilt.
  - Untested: `picture::read` (registry + file), `pick_exe`, `CfgMgrRemover` through the
    new request, DXVK list on a real game folder, `DisplayHook` against the real VDD.
  - Review fixes (2026-11-13): the above two hardening points, QR cap at version 9, and
    CRLF restored in `crates/split-service/src/platform.rs` + `crates/split-stream/README.md`
    (b891979 had converted them to LF). Line-ending policy is mixed (346 LF / 46 CRLF
    files); a `.gitattributes` is deferred to a quiet moment, since renormalising would
    touch files other agents edit on main.
