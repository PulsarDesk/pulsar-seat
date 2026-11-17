# Status / resume point

Index: [../../AGENTS.md](../../AGENTS.md). Update after every step.

- 2026-11-12: repo created (GPL-3, `main`, local only, no remote, nothing committed).
  Docs and clean-room rules only, no code yet.
- Latest reference installer (2.70.6) downloaded into the analyst workspace (see
  `docs/local/README.md`). The test PC has an older version installed.
- 2026-11-12: Phase 0 spec workflow `pulsar-seat-specs` running (run `wf_27d375bd-c64`):
  inventory, then write, critique and revise for each spec 10–90, then 00-architecture and
  a completeness pass. Analyst progress is in the analyst workspace `notes/progress.md`.
- 2026-11-12: **Phase 0 draft v1 done.** All 10 spec files are written, critiqued, revised
  and cross-checked (status table in [spec/README.md](../../spec/local/README.md)). The reference
  product was only ever seen **disabled** on the test PC, so the specs mark its
  enabled-state behaviour HYPOTHESIS.
- **Blocking maintainer decisions** (see [plan.md](plan.md) "Open questions"): which
  backend gives the concurrent sessions; whether a minifilter is allowed; driver signing
  route; whether to do an approved live run of the reference on the test PC.
- **Next:** once the backend is decided, start Phase 1: manager + service skeleton. It does
  not depend on the backend.
- 2026-11-12: **Phases 1–5 implemented in code** (workflow `pulsar-seat-implement`, no live
  test). Gates green on the dev box (ARM64): `cargo build --workspace`, `cargo test
  --workspace` (452 passed, 3 ignored), `cargo clippy --workspace --all-targets -D warnings`
  (aarch64 and x86_64), `cargo check --workspace --target x86_64-pc-windows-msvc`, `cargo
  build --release` (all four exes PE 0xAA64), manager `bun run test` (55) / `check` / `build`,
  `bun test installer/scripts` (14). Console smoke as a normal user: `--console
  --allow-current-user` serves `status` over the real pipe; admin verbs refused.
- Drivers (`drivers/`) **built** 2026-11-13 (WDK 10.0.28000, ARM64 + x64, Release + Debug, zero
  warnings, infverif + WDK code analysis clean; `installer/scripts/build-drivers.ps1`), not signed,
  never loaded. Driver status now reports the version query (drivers/README.md).
- **Next:** maintainer review; live test on a Windows Server + RDSH box (seat host, RDP
  loopback, gates) and on the x64 test PC only with approval. Open gaps:
  see the last entry (2026-11-13 final-fix, round 3, "Still open").
- 2026-11-13 (fix-drivers agent): **driver review findings fixed** (16 of 16; none
  skipped), built clean ARM64 + x64, Release + Debug, with `-Analyze`; infverif and the new
  ApiValidator step pass (all binaries Universal). Not signed, never loaded, untested on
  hardware. seat-idd: per-slot `SwapLock` for assign / unassign / remove, thread uses
  `SwapMonitor` (not `Object`), Parse no longer races AddMonitor, ready event name carries a
  random nonce from the section header (interface minor 2; seat-display reader updated),
  multi-GPU limit documented + warned (one adapter = one render GPU, open gap).
  seat-input-filter: control cleanup drains before dropping `ControlOpen` (under
  `UnitLock`), kbd / mouse pass route reports what kbdclass / mouclass really consumed, HID
  session 0 allowed only for service tokens, activity copies carry no key, the driver
  honours `ALLOW_SYSTEM_BUTTONS` (service puts 0x01/0x80 first). seat-storage-filter: IRQL
  check + `SeLockSubjectContext`, `SL_FORCE_ACCESS_CHECK` kernel creates judged, strict
  `SET_VOLUME` GUID form (seat-media validates too), Spectre on. drivers/README.md now says
  what runs (no SDV / Driver Verifier yet).
- 2026-11-12 (final-fix agent): **final review findings handled; phases 1–5 code-complete
  for this run, nothing tested on hardware.** Details in architecture.md §4.7 and §5.
  - Fixed: pipe DACL no longer gives interactive users `FILE_CREATE_PIPE_INSTANCE`
    (`PIPE_SDDL` IU mask `0x0012019b`; all clients open with `PIPE_CLIENT_ACCESS`; test
    `users_cannot_create_rogue_instances`); CLI client at `SECURITY_IDENTIFICATION`; seat
    host re-checks on `OnConnected` that its TCP connection ends at TermService
    (`verify_rdp_peer`); SYSTEM children of the console helper / seat-session get an
    explicit SYSTEM-only DACL (seat hosts already had one: that part of the finding was
    wrong); config tweaks applied / reverted by the service; seat + lock images generated;
    keep-awake power request; monitor idle saver (`timings.monitor_idle_off_s`, default
    off); hotkey moves are runtime overrides until confirmed (`resolve_input_move`, manager
    dialog); CPU-set validation in the service; `monitor_hub_mismatch` warning; touch / pen
    on hosted seats and seat-assigned printers shown as not isolated; diagnostic report
    `[displays]` + `[power]`; experimental warning has one source
    (`crates/seat-session/src/termwrap/warning_tr.txt`, manager copy checked by a test),
    `EXPERIMENTAL_WARNING_VERSION` 2 (earlier acks invalid); webcam "indicate" and the
    class-default-descriptor toggle not offered (not built); user recovery guide
    [docs/troubleshooting.md](../troubleshooting.md).
  - Verification (dev box ARM64, `CARGO_TARGET_DIR=target\agent-final-fix`):
    `cargo build --workspace` clean; `cargo test --workspace` 460 passed, 0 failed,
    3 ignored (20 suites); `cargo clippy --workspace --all-targets -- -D warnings` clean
    (aarch64 and `--target x86_64-pc-windows-msvc`); `cargo check --workspace --target
    x86_64-pc-windows-msvc` clean; `cargo build --release` + `-p pulsar-seat-manager`:
    agent, host, manager, service all PE 0xAA64; manager `bun install` (no changes),
    `bun run test` 57/57, `bun run check` 0 errors 0 warnings, `bun run build` ok;
    `bun test installer/scripts` 14 passed. Console smoke without `--allow-current-user`
    (non-elevated): `status` works over the pipe with the new DACL; `images\seat-01.png`
    and `lock.png` written and decode as 1920×1080.
  - **Still open (not done in this run):** per-seat audio and microphone (needs an RDP
    client that hands audio to us, e.g. FreeRDP-class; spec 60 §8.3); touch / pen /
    relative mouse into seats (same client); server certificate pinning / forced CredSSP
    (the ActiveX control has no certificate callback); console-side raw input reporter for
    hotkey / activity / wizard keys under `raw_input_host` (needs a seat-proto report
    request); per-monitor wizard pages, number overlays, any-keyboard control keys, hub
    rules in the wizard (spec 10 §9.2); CCD topology changes and clone (spec 40 §6.2,
    §6.3.6); job-object launcher with seat `cpu_sets` (validation done, launch not wired);
    test tone from the console session (still played in session 0); class-level default
    descriptors (spec 70 §5.2(2)); printer ACL gate (spec 70 §5.2(5), optional); per-port /
    per-container assignment keys (spec 70 §5.1); update check (spec 90 §11.10); per-seat
    IP binding, device pictures, log-level dialog, video-output dialog, wizard minimum
    resolution (spec 10); camera preview; seat host one-time token still on the command
    line (process DACL protects it).
  - **Not built (at that time):** drivers (built since 2026-11-13, see above), WiX / NSIS installers, CI
    workflow. Drivers are not signed and never loaded.
  - **Next:** (1) maintainer review of this run; (2) live test on a Windows Server + RDSH
    box: seat host + RDP loopback, listener / peer checks, gates, tweaks journal +
    uninstall revert, keep-awake (`powercfg /requests`), idle saver, hotkey confirm;
    (3) x64 test PC only with approval; (4) install the WDK (ARM64 host) and build the three
    drivers (`drivers/README.md`), then `infverif`; (5) choose the signing route
    (attestation vs. test VM) before any driver load; (6) pick an RDP client with audio /
    touch / certificate API for the open items above.
  - 2026-11-13: two workflows are running in parallel:
    - `pulsar-seat-complete` (run `wf_130fbafc-8ae`) finishes the open features and builds
      the drivers. WDK 10.0.28000 is installed; see docs/progress/complete-all.md.
    - `pulsar-seat-usermode-sessions` (run `wf_f5f5b7b5-049`) is analyst research on whether
      true local seat sessions can run on client x64 / ARM64 in user mode only. Output:
      spec/35. The maintainer asked that it cover functionality only and leave out security
      and Memory Integrity topics.
  - 2026-11-13: user-mode session research done, written up in spec/35 (draft v2).
    - Verdict, high confidence: **true concurrent local seats on client Windows 11 are not
      possible in user mode**, on x64 or ARM64. Two limits block it: the licence policy in
      termsrv/LSM, and the single global active-console id that the kernel uses to choose
      which session gets the physical display and input.
    - Recommended client / ARM64 product:
      - U1: real sessions taking turns (orchestrated fast user switching plus a credential
        provider).
      - U3: concurrent kiosk / app seats in one session.
      - Server x64: RDSH loopback.
    - Prototypes P1–P7 and the maintainer decisions are listed in spec/35 §5 and §7.
  - **Maintainer decisions open:** session backend default on client SKUs (experimental
    wrapper stays off by default); minifilter exception; signing route; whether to adopt a
    FreeRDP-class client (Apache-2.0, GPL-3 compatible) inside the seat host; update
    channel (GitHub Releases) policy.
- 2026-11-13 (rdp-client agent): **own RDP client in the seat host (IronRDP, `crates/seat-rdp`)**,
  default backend; the ActiveX control is an explicit fallback (`--rdp-backend activex`).
  It closes these earlier open items: per-seat audio + microphone (AudioBridge), touch / pen
  (MS-RDPEI), relative mouse, certificate pinning (fail closed), forced CredSSP, client
  name `PSEAT-NN`, multi-monitor frames into the presenter. Main flow wired
  (`seat-host/src/ironrdp.rs`). `HostAssignment.listener_cert_sha1` added (additive); the
  service fills it. The host accepts `--token-stdin`. Untested on hardware.
  - Verification (`CARGO_TARGET_DIR=target\agent-rdp-client`): `cargo test -p seat-rdp`
    29 ok, `-p seat-host` 27 + 4 ok, `-p seat-proto` 11 ok. `cargo clippy -p seat-host -p
    seat-rdp -p seat-proto --all-targets -D warnings`: clean for aarch64 and x86_64.
    `cargo check ... -p seat-service --target x86_64-pc-windows-msvc`: clean. Release host
    PE 0xAA64.
  - Still open: the service launcher must pass the token on stdin (`--token-stdin`, an
    inherited anonymous pipe) instead of `--token` (seat-service `seats.rs` launch_host
    + `win/platform.rs` launch; not done because another agent is changing those
    lines). No clipboard / drive / smart-card redirection in the IronRDP backend (since
    done: rdp-channels / fix-rdp-channels entry below). No EGFX / H.264 (since done:
    egfx-h264). First real run on Windows Server + RDSH.
- 2026-11-13 (fix-rdp-client): **review findings on the IronRDP client handled** (seat-rdp,
  seat-host; additive `HostAssignment.input` in seat-proto, filled in seat-service
  `requests.rs`). Untested on hardware.
  - Fixed: relative-mouse trigger (fast-path update codes instead of IronRDP's software-mode
    `PointerHidden`); captured-pointer buttons as `TS_FP_RELPOINTER_EVENT` buttons, wheel at the
    server pointer position, resync of the absolute path when capture ends; CredSSP-phase
    socket errors with a password = `auth_aborted` (no retry); no in-host reconnect with a
    stored password, `Network` end = `disconnected: transient` (service back-off); reconnect
    count reset only after logon / 30 s; capture / playback stop on connection drop; uplink
    messages tagged by connection; `set_format` takes the new packet size; frame + pointer
    output indices over the full `SeatOutputs` list (clones); `send_layout` stores the
    even-width layout as sent, only after a good write; server monitor layout keeps scale /
    orientation, reconnect uses the seat's requested layout; input queued during a reconnect
    dropped, layouts kept; pending layout waits at most 5 s, then a reconnect with the new
    monitor data (`End::Relayout`); listener pin read errors fail closed; pins re-read once on
    a reconnect cert mismatch; KLID via `GetKeyboardLayoutNameW` (Turkish F kept); owner input
    settings in the host forwarder (also ActiveX path); wave-confirm latency from the bridge
    queue; streams whose endpoint was missing are retried every 5 s; MS-RDPEI contact ids in
    free slots; `clientDir` = our own exe.
  - Not done (outside this task's dirs, or needs a design step): pen tilt / rotation /
    eraser / barrel (needs `RdpContact` fields in seat-input); manager notice for missing
    audio endpoints (needs an additive host report field + service + manager; the host only
    logs the notes); service binding of `SignedIn.session_id` and restricting the
    newest-session heuristic to the ActiveX backend (seat-service); `--token-stdin` in the
    service launcher (docs corrected instead); passwordless seats on an NLA listener: design
    chosen (second loopback-only listener, architecture.md §4.1); built since (see the
    second-listener / fix-second-listener entry below).
  - Verification (`CARGO_TARGET_DIR=target\agent-fix-rdp-client`): `cargo test -p seat-rdp`
    36 ok, `-p seat-host` 34 + 4 ok, `-p seat-proto` 11 ok, `-p seat-service` 134 ok;
    `cargo clippy -p seat-host -p seat-rdp -p seat-proto -p seat-service --all-targets -D
    warnings` clean for aarch64 and x86_64.
- 2026-11-13 (installer-update): **MSI installers build** (ARM64 + x64) with
  `installer/scripts/build-installer.ps1` (Tauri bundler + the WiX 3.14 it downloads into
  `%LOCALAPPDATA%\tauri\`), extracted with `msiexec /a` and PE-gated; `dist\PulsarSeat_<v>_<arch>.msi`
  + `SHA256SUMS.txt`. Signed driver packages are staged only with `-SignedDrivers` (catalog
  Authenticode-valid and from the Microsoft hardware publisher). **Update check** (spec 90
  §11.10): opt-in `settings.updates{check, weekly}`, service-side WinHTTP to GitHub Releases,
  SHA-256 from `SHA256SUMS.txt` + Authenticode signer pin (`ACCEPTED_SIGNERS`: key SHA-256 +
  subject + issuer, empty until the first signed release; HYPOTHESIS subject "PulsarDesk"), install only after admin confirmation in the manager
  (**Güncellemeler** dialog). CI rewritten (`.github/workflows/ci.yml`, never run). Details and
  open items: installer/README.md. Never installed; no release published, so the update path is
  untested end to end. The local ARM64 test MSI used the 2026-11-12 binaries and no ARM64EC helper.
- 2026-11-13 (input-reporter + fix-input-reporter): **console raw-input listener** under
  `raw_input_host` (`pulsar-seat-host --console-listener --token T`, launched and verified by
  the service; architecture.md §4.3). It supplies activity (identify by touch, idle saver),
  the Ctrl+hotkey, the emergency combination and the wizard's key / click counting. Review
  fixes: the router flag is set under the state lock; `hello` is retried on
  `access_denied` (launch race); drains go out in batches with hotkey / emergency first
  (nothing dropped); the emergency works with the move hotkey off (Ctrl+Shift+F12 x3 then),
  drops pending hotkey overrides and cancels a running wizard; the hotkey is ignored during
  the wizard and also moves same-parent HID collections and, from seats 2+, the seat's mice;
  `wizard_all_keys` only while a wizard step needs every key (otherwise F-keys + control
  keys); no scan codes in the log; Turkish labels for the input backend notes.
  - Verify (`CARGO_TARGET_DIR=target\agent-fix-input-reporter`): `cargo test -p seat-proto
    -p seat-input -p seat-host -p seat-service`; clippy `--all-targets -D warnings` on
    aarch64 + x86_64.
  - **Untested on hardware:** raw input sink as restricted SYSTEM on winsta0\default;
    delivery while seat hosts register `RIDEV_NOLEGACY` in the same session; secure-desktop
    gap (lock screen / UAC: no raw input reaches the listener); under `seat_one_console`
    seat-1 hotkey / wizard keys also reach the console's foreground window.
  - Open: with `kmdf_filter`, seat-1 keyboards get the emergency only while the hotkey is
    on (the filter's `hotkey_scan = 0` means off; the driver protocol has no separate
    emergency key). Extra-seat units already use the F12 fallback.
- 2026-11-13 (display-topology + fix): **CCD topology apply, clone, flip presenter, mode
  follow** (seat-display, seat-host; additive seat-core `SeatDisplayPrefs`,
  `DisplayBinding::configure_seat`, `OutputPlacement.clone_of / follow_mode`). Validate /
  apply / verify / roll back with a trusted journal, monitor specialization
  (feature-detected), D3D11 flip-model presenter with GDI fallback, clone outputs, mode
  follow on desktop targets. The service calls `configure_seat` before `bind_seat`.
  Untested on hardware (specialization GUID acceptance, SetDisplayConfig rollback, flip
  pacing, dynamic mode reset after a host crash is not journaled).
- 2026-11-13 (media-compat + fix): **test tone from the console session, class-level
  default descriptors (CD-ROM + Ports only; disk / volume refused), printer ACL gate,
  per-port / per-container assignment keys, seat job objects with `cpu_sets` for the agent
  and `startup_apps`, best-effort source IP** (`PULSAR_SEAT_SOURCE_IP`), launcher request
  `start_seat_app`. Class defaults stay off until the maintainer signs them off. Untested on
  hardware (spooler / USBPRINT mapping, WASAPI tone as restricted SYSTEM). Real source-IP
  enforcement needs a WFP callout driver (maintainer decision).
- 2026-11-13 (wizard-manager + fix): **spec 10 manager / wizard gaps** (per-monitor wizard
  pages and overlays, any-keyboard control keys, hub rules, min resolution 1024x600,
  per-seat IP binding UI, device pictures, log-level / video-output dialogs, camera preview,
  tab and close guards). Untested on hardware. HYPOTHESES: modifier detection from make
  codes; click fallback to the lowest seat with a keyboard but no mouse.
- 2026-11-13 (drivers + fix-drivers): see the fix-drivers entry above; all three drivers
  build clean (ARM64 + x64, Release + Debug, WDK 10.0.28000, `-Analyze`, infverif,
  ApiValidator Universal). Unsigned, never loaded.
- 2026-11-13 (integrate): **whole tree green, token off the command line, manager gaps
  closed.**
  - Seat hosts and the console listener now get their one-time token on stdin
    (`--token-stdin`): `LaunchSpec.stdin_secret` + `platform::token_frame` (u32 LE length +
    UTF-8, max 512 bytes); `win/platform.rs` launch goes through
    `seat_session::win::process::spawn` (explicit `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, the
    pipe's read end is the only inherited handle). `seat-host listener::parse_with` reads
    the frame too. `--token T` is still accepted for manual runs.
  - SYSTEM children (seat hosts, console listener) now run with the restricted SYSTEM token
    (`restricted_system_token_in_session`, `DISABLE_MAX_PRIVILEGE`) as the design and the
    listener comment already said; before, the service handed them its full-privilege token.
    HYPOTHESIS (untested on hardware): IronRDP / ActiveX host, raw input sink and WASAPI work
    without privileges.
  - Duplicates removed: service `quote_arg` and `env_block` (now seat-session
    `command_line` / `env_block` / `environment_of`), dead `seat_session::launch::
    {seat_host_spec, agent_spec}` (still said `--token`), unused `SeatDisplay.layout` and
    the module-wide `allow(dead_code)` on seat-host `display`.
  - Service: `--pipe` / `PULSAR_SEAT_PIPE` accept a bare name. Boot no longer warns about
    `PlugPlay`: on this Windows build it has no service entry (1060), now skipped.
  - Manager: types caught up with seat-core / seat-proto (`startup_apps`, `KioskApp`,
    `class_default_classes`, `DeviceView.port_key / container_key`, `printer_acl` gate).
    New startup-programs dialog (Basic tab column), class choice for the class defaults
    (switch still disabled, copy fixed), assign dialog targets "bu bağlantı noktası" /
    "bu aygıtın tamamı" (`setKeyMask`), `effectiveMasks` mirrors the container / port / hub
    walk of `seat_core::assign`, Turkish wording for port / container inheritance.
  - Proto drift check (Rust `RequestBody` / `ResponseBody` / `Event` tags and struct fields
    vs `apps/manager/src/lib/types.ts`): no mismatch left; the Rust-only messages are the
    host / agent / listener ones the manager never sends.
  - Verification (`CARGO_TARGET_DIR=target\agent-integrate`, dev box ARM64):
    `cargo build/test --workspace` green; `cargo clippy --workspace --all-targets -D
    warnings` clean for aarch64 and x86_64; `cargo check --workspace --target
    x86_64-pc-windows-msvc` ok; `cargo build --release` PE 0xAA64 for service, host, agent,
    manager; manager `bun run test` 92/92, `check` 0/0, `build` ok; `bun test
    installer/scripts` 18/18; `build-drivers.ps1 -Configurations Release -Analyze` clean for
    all 3 drivers ARM64 (0xAA64) + x64 (0x8664) incl. infverif + ApiValidator;
    `build-installer.ps1` ARM64 (`-NoArm64ec`: the arm64ec rustup target is not installed)
    and x64 MSIs built and payload-gated; console smoke `pulsar-seat-service --console
    --allow-current-user --pipe X` + `status` over the pipe ok (`report` refused for a
    non-admin, as intended).
  - Still open: everything above is untested on hardware; drivers unsigned; ARM64 MSI
    without the ARM64EC helper; `ACCEPTED_SIGNERS` empty; CI never ran; IronRDP had no
    clipboard / drive / smart-card channels and no EGFX at that point (both done since:
    rdp-channels, egfx-h264); manager `force_assign_at_start`
    stored only; per-seat IP enforcement best effort.
- 2026-11-13 (final-fix): **20 final-review findings handled** (none skipped as wrong; the
  four large features below stay open with a reason). Nothing committed, installed or run
  on hardware.
  - Done:
    - Control pipe: `MAX_INSTANCES` 128; ordinary clients ≤ 64 in total, ≤ 16 per session,
      first request within 5 s; the service's own children (hosts, listener, agents,
      adopted hosts; `LaunchSpec::service_child`, `WinPlatform::is_service_child` by client
      PID) are not counted. Tests `slots_limit_per_session_and_in_total`,
      `idle_client_is_dropped_and_trusted_pid_is_exempt`.
    - Host / listener token: `hardened_system_token_in_session` (no privileges,
      Administrators deny-only, **Low integrity**); pipe SACL `PIPE_LOW_LABEL_SDDL`;
      recovery value `HostTokenCompat=1` (troubleshooting §5). Test
      `hardened_token_is_low_integrity`.
    - Handle leak: exited children reaped on every launch (`WinPlatform::reap`, also from
      the job pid lists); `start_seat_app` ≤ 10 starts / 10 s and ≤ 64 live apps per session.
    - Seat host hello race: `host::connect` uses `listener::hello_with_retry`.
    - No autostart on a non-concurrent backend (`fast_user_switch`); manual start only.
      Test `no_autostart_on_non_concurrent_backend`.
    - Session binding: `host_report.rdp_client` (additive), `HostHint` →
      `SessionBackend::host_hint`; reported id must be confirmed by WTS + client name;
      newest-session fallback only for ActiveX. Tests in wts.rs and backend_tests.rs.
    - `nla_required` refused up front for passwordless seats on an NLA listener.
    - New `FailureReason`s (additive): `console_in_use`, `console_busy`,
      `session_not_locked`, `nla_required`, `kiosk_app_missing`, `console_not_ready`,
      `not_our_session`, `fsu_disabled`; Turkish texts (also `no_account`,
      `account_unresolved`).
    - `fast_user_switch`: `fsu_disabled` (policy or Winlogon), one account per seat (backend
      refusal + seat-core issue `same_account_fast_user_switch`, manager mirror),
      `AgentTaskKind::ApplyTopology` after `ConsoleConnect` (agent:
      `seat_display::win::ccd::apply_seat_only`).
    - `force_assign_at_start` now re-applies the console topology at each seat start.
    - Input filter interface minor 1: `EmergencyScan` (driver + `filter_proto`), router
      counts emergency presses with the move hotkey off.
    - Encoding: installer/README.md (14 lines) and one journal line repaired from cp1252
      mojibake; BOM stripped from seat-rdp `Cargo.toml` / `lib.rs`; SPDX first line on every
      crate manifest.
    - Release hygiene: service and host built in separate cargo invocations
      (build-installer.ps1, ci.yml; release service binaries contain no `ironrdp` string);
      `-Release` refuses when tag `v<version>` is not reachable at the source-offer URL;
      installer/README.md "Signing and release" items 7–8.
  - Verification (`CARGO_TARGET_DIR=target\agent-final-fix`, ARM64 dev box):
    `cargo build --workspace` ok; `cargo test --workspace` **622 passed, 0 failed, 3
    ignored**; `cargo clippy --workspace --all-targets -D warnings` clean for aarch64 and
    x86_64; `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc` ok;
    `cargo build --release` PE 0xAA64 for service, host, agent, manager; manager `bun run
    test` 93/93, `check` 0/0, `build` ok; `bun test installer/scripts` 18/18;
    `build-drivers.ps1 -Configurations Release -Analyze` clean for all three drivers, ARM64
    0xAA64 + x64 0x8664, infverif + ApiValidator; MSIs `PulsarSeat_0.1.0_arm64.msi`
    (`-NoArm64ec`, SHA-256 51bf3ed3…6c9; superseded, see the release entry at the end) and
    `PulsarSeat_0.1.0_x64.msi` (6107f4e7…b63; removed from dist by fix-release: built
    before release signing) built, payload clean outside and inside; console smoke `--console --allow-current-user
    --pipe PulsarSeatSmokeFF` + `status` ok (pipe with the Low label), `report --no-mask`
    refused for a non-admin. SDV: not available in WDK 28000 (drivers/README.md).
  - Untested on hardware: all of the above, in particular the Low-integrity host
    (IronRDP, ActiveX, WASAPI, raw input sink, cursor clip, IDD section), the PID-based pipe
    exemption, `ApplyTopology`, force-assign, `EmergencyScan` in the driver, FUS refusals.
  - Unsigned drivers: all three (never loaded); storage minifilter altitude still
    `UNALLOCATED`.
  - Still open (not built in this run):
    - ~~Second loopback-only NLA-off listener for passwordless seats~~ built since (see the
      fix-second-listener entry below).
    - ~~Credential provider + filter for `fast_user_switch` auto-logon~~ built since (see the
      credprov + fix-credprov entries below).
    - ~~U3 / D′ concurrent kiosk seat shell (spec 35 §5.1; per-seat windows, background raw
      input by device handle, software cursors)~~ built since (see the kiosk-shell +
      fix-kiosk-shell entry below).
    - ~~IronRDP EGFX / H.264, cliprdr / rdpdr / smart cards~~ built since (egfx-h264,
      rdp-channels + fix-rdp-channels entries below). Zero-copy D3D11 decode into the
      flip presenter built since (round 4 egfx-perf, untested on hardware).
    - ~~Read containment of the host (dedicated low-privilege account + window-station /
      desktop ACEs)~~ built since (host-containment + fix-host-containment entry below).
    - Release blockers: ~~arm64ec rustup target~~ (added, release entry), ~~`ACCEPTED_SIGNERS`
      code~~ (minisign release signature built; the real key itself is the maintainer's
      step, docs/release/checklist.md §2), CI never ran (no remote), public repo + tag for
      the source offer, SDV (x64 CI job, never ran), Driver Verifier and HVCI on a test VM,
      minifilter altitude request, Authenticode certificate (checklist §3).
  - Next: maintainer review; hardware test of the Low-integrity host first (fallback
    `HostTokenCompat`), then FUS on the ARM64 laptop with approval; decide the credential
    provider and the second listener.
- 2026-11-13 (second-listener + fix-second-listener): **second, loopback-only NLA-off
  RDP listener for passwordless seats built** (`rds_loopback` only; architecture §4.1 design
  (a)). Built and unit-tested; **untested on hardware** (needs Windows Server + RDSH);
  nothing committed.
  - seat-session `loopback_listener`: key `WinStations\PulsarSeat-Loopback` (allowlisted
    copy of RDP-Tcp's stack values with their types; NLA off, TLS on, `LanAdapter = 0`,
    `fInheritAutoLogon = 1`, `fPromptForPassword = 1`, no credential / program values,
    descriptor = seat accounts + extra accounts only, admins manage but cannot log on);
    crash-safe write order (`fEnableWinStation` 0 first, 1 last) + rollback of an interrupted
    build; one firewall block rule per port (`Pulsar Seat - loopback RDP (block remote)
    <port>`), added before the key, read back through `INetFwPolicy2` and verified
    (enabled, inbound, block, TCP, port, all profiles, `*` local, remote set covers every
    non-loopback address); a rule for an old port stays until nothing listens there.
    Refused (and removed) when the firewall is off in a profile, a policy sets
    `AllowLocalPolicyMerge = 0`, a policy forces NLA / turns TLS off, the port is invalid or
    used by another program or WinStation, or no account resolves. TermService restarts only
    when no user session exists (remote flag not trusted alone), forced after a key change,
    otherwise once per change and at most every 10 min; `restart_needs_reboot` when
    TermService refuses stop. A removal TermService has not caught up with is reported
    (`loopback_listener_unload_pending:<port>`, `loopback_listener_exposed:<port>` when the
    firewall does not protect it, event log error) and retried by
    `SessionBackend::retry_deferred` after every seat stop and session logoff / end.
  - seat-core (additive): `SessionBackend::retry_deferred`,
    `FailureReason::LoopbackListenerUnavailable`. seat-service: failure mapping, seat
    `detail` carries `listener=loopback_listener:<state>`; uninstall runs listener removal
    and RDS restore independently. Manager: Gelişmiş sekmesinde ayar (açık/kapalı, port, ek
    hesaplar), Turkish copy per listener state and note.
  - Open (deviation from the task): the listener is **not bound to loopback**; it listens
    on every adapter and the firewall rule keeps it loopback-only. HYPOTHESIS: a WinStation
    can only bind to a LAN adapter (`LanAdapter` = LANA number), and the loopback interface
    has no LANA. Hardware test: on Server + RDSH set `LanAdapter` of the listener key to each
    LANA listed by the RDS configuration tool / WMI `Win32_TSNetworkAdapterSetting` and
    check with `netstat -ano` whether any of them binds `127.0.0.1` / `127.0.0.2` only.
  - Open HYPOTHESES to test on hardware: TermService reloads WinStation keys only on
    restart; TermService accepts stop on the target builds; a disconnected RDP seat
    session reports `WTSIsRemoteSession`; `INetFwRule` reports the properties in the forms
    `rule_protects` parses; loopback traffic is not filtered by the block rule.
  - Not built: a forced TermService restart (with confirmation) from the manager when a
    removed listener is exposed; today the manager tells the user to stop the RDP seats or
    restart the PC.
  - Verification: see `docs/progress` note of fix-second-listener until merged.
- 2026-11-13 (credprov, round 3): **credential provider + filter for `fast_user_switch`**
  (spec 35 §5.4 / §5.5). Nothing registered, installed or run on hardware; nothing
  committed.
  - `credprov/`: C++20 COM DLL `pulsar-seat-credprov.dll` (provider with seat tile + switch
    tile, `ICredentialProviderCredential2`, `ICredentialProviderSetUserArray`, filter),
    own pipe client (server PID = `PulsarSeat` service), strict JSON reader, Negotiate
    `KERB_INTERACTIVE_UNLOCK_LOGON` packing with `CredProtectW`; inert outside LogonUI and for
    every scenario but logon / unlock; hides nothing when the service does not answer.
    `credprov-selftest` (44 checks). No `DllRegisterServer`, no ARM64X (CredUI refused).
  - seat-proto (additive): `ClientKind::CredentialProvider`, `Access::CredentialProvider`,
    `credprov_query{scenario}` → `credprov_session`, `credprov_take_credentials` (once),
    `credprov_report{status, sub_status}`, `credprov_switch{seat}`; test `credprov_shapes`
    pins the JSON the C++ side writes / parses.
  - seat-session: `credprov` (CLSIDs, `sync` / `register` / `unregister` of our four keys,
    `check_dll`, `is_logonui_image`, recovery value `CredProvDisabled`); FUS
    `caps().auto_logon` = provider listed. Uses the `Registry::delete_key` the
    second-listener package added at the same time.
  - seat-service: `credprov.rs` (one-time grant per fresh logon session, TTL 120 s; caller
    check SYSTEM + LogonUI image + session; query / take / report / switch; failed logon →
    password failing; switch rate limit 3 s), `sync_credprov` with every tweak sync and
    backend reselect, only in session 0 (`--console` never registers); `Platform::
    {process_image, system_root, sync_credential_provider}`; `uninstall` removes the keys.
    7 new service tests.
  - Installer: wxs component `PulsarSeatCredProvComponent` (file only), tauri-bundle ref,
    NSIS file line, `build-credprov.ps1` (called from `build-installer.ps1`).
  - Docs: credprov/README.md, architecture §4.1 / §2 / §5 / §8, troubleshooting §7 (Turkish:
    disable, `CredProvDisabled`, safe mode, `reg delete`, offline `reg load` from WinRE),
    installer/README.md, seat-session README; manager notice copy `credprov_failed`,
    `credprov_logon_failed`, `switch_refused`.
  - Verification (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo build` + `cargo test
    -p seat-proto -p seat-session -p seat-service` (12 + 166 + 157 passed); clippy
    `--all-targets -D warnings` for those crates on aarch64 and x86_64 clean; `cargo check
    --all-targets` workspace on both targets ok; msbuild credprov + selftest ARM64 / x64
    Release / Debug 0 warnings, PE 0xAA64 / 0x8664, self-test 44/44 on all four (x64
    emulated); exports only `DllCanUnloadNow`, `DllGetClassObject`; imports secur32,
    advapi32, ole32, kernel32 (static CRT); manager `bun run test` 98/98, `check` 0/0,
    `build` ok; `bun test installer/scripts` 27/27.
  - Untested on hardware: LogonUI loading the unsigned DLL, auto-logon with `autoLogonWith
    Default`, filter effect, V2 tiles without a SID, switch link from the lock screen (spec
    35 P7 HYPOTHESIS), `CredProtectW` blob accepted by Negotiate, `GetNamedPipeServerProcessId`
    from LogonUI, the SYSTEM pipe ACL for LogonUI.
  - Still open: seat 1 is not a switch target (FUS backend maps seat 1 to "the current
    console"); `user_sid` not filled (tile shows as "other user"); CI job for
    `build-credprov.ps1` not added (ci.yml owned by the release package); DLL signing.
    (Superseded in part by fix-credprov below.)
- 2026-11-13 (fix-credprov): review findings on the credential provider. Nothing registered,
  installed or committed.
  - Filter can no longer hide every tile: `auto_submit` / `hide_others` need an account
    (service `credprov_query`; C++ `Query` too); provider and filter share
    `SeatTileWillExist` / `FilterHides`.
  - Late grant (FUS race: LogonUI enumerates before the grant is armed): service
    `Handover` (`note_credprov_handover` in `run_start` before `SessionBackend::start`) →
    `CredprovSession.pending` (seat-proto, additive); provider takes part with no tiles,
    keeps `ICredentialProviderEvents`, polls (`Notifier`, 250 ms, ≤ 30 s, worker thread
    pins the DLL), `CredentialsChanged`, tiles rebuilt in `GetCredentialCount`. Service test
    `credprov_query_before_arm_is_pending_until_the_grant`.
  - After a failed auto-logon the provider refreshes once and calls `CredentialsChanged`;
    whether LogonUI re-filters is unverified → documented (README, troubleshooting §7).
  - Secrets: `Conn::Call` / `RecvFrame` wipe frame and tree on every path; `Ok` moved;
    JSON strings reserved on the heap up front, half-parsed values wiped; password decoded
    into a wiped vector; `SecretW::Assign` reserves.
  - `credprov_take`: TTL, seat match, `taken`, secret read in one critical section.
  - `is_logonui_image`: no byte slicing (non-ASCII panic), tests.
  - `user_sid`: `Platform::account_sid` (Win: `seat_session::win::wts::account_sid`), cached;
    the provider uses it only when LogonUI's user array (`SetUserArray`) lists it. Service
    test `credprov_no_auto_submit_without_an_account`.
  - Release: CI job `credprov` (native runners, self-test, artifact into the installers
    payload), `ci-local.ps1 -CredProv` (and with `-Payload`); `check-payload.ts --release`
    (`unsigned_system_dll` for `pulsar-seat-credprov.dll`), `sign-release.ps1` extracts each
    MSI and requires a Valid signature on the DLL inside, `build-installer.ps1
    -SignedCredProv <dll>`; `-Release` warns on an unsigned DLL (CI has no signing).
  - Finding rejected: `SwitchLinkText` seat 11 etc. was already right (`units[1]` is `'e`);
    checks for 1, 11, 20, 21, 40, 50, 61, 64 added.
  - Verification (`CARGO_TARGET_DIR=target\wf`): `cargo test -p seat-proto -p seat-session
    -p seat-service` 12 + all session + 172/173 (the one failure is
    `events::tests::host_events_are_scoped`, another package's in-progress kiosk change, not
    credprov); credprov tests 1 + 7 + 9 passed; clippy `-D warnings` for the three crates on
    aarch64 + x86_64 clean; `build-credprov.ps1` ARM64 + x64 Release 0 warnings (/WX),
    self-test 66/66 on both; `bun test installer/scripts` 28/28; `lint-ci.ts` ok;
    `check-payload --release` flags the real unsigned ARM64 DLL.
  - Untested on hardware: polling + `CredentialsChanged` from a worker thread, re-filtering,
    V2 tile with SID, everything else in the entry above.
  - Still open: seat 1 as a switch target (needs the FUS backend to remember seat 1's
    disconnected session and `WTSConnectSessionW` it back); DLL signing itself (maintainer,
    checklist §3); CI never ran (`credprov` job HYPOTHESIS on windows-11-arm).

- 2026-11-13 (egfx-h264 + fix-egfx-h264): **EGFX / H.264 in the IronRDP client**
  (`crates/seat-rdp/src/gfx.rs`, `h264/mod.rs`, `h264/mf.rs`, `session.rs`; host override
  `PULSAR_SEAT_GFX` in `seat-host/src/ironrdp.rs`). Untested against a real RDP server.
  - Built: MS-RDPEGFX V8 .. V10.7 through `ironrdp-egfx` 0.5 (surfaces, cache, solid fill,
    surface copies, ClearCodec / planar / RemoteFX / Progressive, AVC420, AVC444 v1/v2);
    Media Foundation H.264 MFT, D3D11 hardware first, software fallback at start and at
    run time (5 failures in a row or device removed); pictures paired with their access
    unit by sample time; bounded picture size; `ResetGraphics` of a new size moves the
    splitter geometry / `DesktopSize` / `Outputs` without a reactivation; damage (failed
    decode, dropped update, dropped picture) asks for a redraw, then a graphics protocol
    reset (V10.3+, fresh IDR), at most every 2 s; presents on EGFX frame boundaries;
    metrics (`SessionEvent::Graphics`: errors, dropped, pic_dropped, resyncs, fallbacks,
    coalesced, wait of a change for its present). Turkish troubleshooting section 8.
  - Verified: `cargo test -p seat-rdp` (hand-built PDU tests incl. AVC444, surface copy,
    cache round trip, DeleteSurface, 8.1 caps, truncated AVC420 metablock, ResetGraphics
    resize; MF hardware + software round trips on the ARM64 laptop); clippy `-D warnings`
    aarch64 + x86_64. x64 MF tests ran only under emulation on ARM64.
  - Still open: real-server test (Windows Server + RDSH); x64 hardware GPU test; zero-copy
    D3D11 texture path into the flip presenter on the same adapter (**deferred**, not
    delivered: needs a GPU `FrameRef` variant, the decode device on the presenter's
    adapter by LUID and a GPU NV12 to BGRA path); FrameAcknowledge after the present
    (ironrdp-egfx 0.5 acks inside EndFrame); the `ResetGraphics` monitor list (not passed
    to the handler; the seat layout as sent is used).
- 2026-11-13 (host-containment + fix-host-containment, round 3): **seat hosts and the
  console listener run as the local account `PulsarSeatHost`** (batch logon per launch, no
  privileges, Administrators deny-only, Low integrity; window-station / desktop ACEs for the
  logon SID, journaled in `run\host-grants.json`; default DACL without the shared account
  SID; pipe ACE; `HostTokenCompat` 0 / 1 / 2; host exit codes 43 / 44). Details in
  architecture.md §3 and §4.7, Turkish troubleshooting §5 / §6. **Untested on hardware.**
  - Fix round (15 findings, none skipped): default DACL of the host token (hosts could
    open each other's later threads); `GetExitCodeProcess` checked; account refused (fail
    closed) when its local groups (`LG_INCLUDE_INDIRECT`) hold anything but `Users` or
    cannot be read; password rotated as the last SAM step, stale secret deleted on failure;
    revoke helpers run outside the containment lock; no contained launch without a creation
    time; a host still exiting after `terminate` keeps its handle and loses its grant at
    the next reap (`Platform::reap_exited` every tick); uninstall stops the service, revokes
    the journal, then removes the account; journal fsync before rename, a left temp file
    merged at load; a NULL DACL is never edited; clipboard right only for hosts whose seat
    redirects the clipboard (`--clipboard`, decided at launch); `Containment<HostSys>` with
    mock tests.
  - HYPOTHESIS: IronRDP, the ActiveX control, WASAPI, raw input, cursor clip, COM, smart
    cards and the IDD section reader work in a batch logon without a loaded profile at Low
    integrity; services that open the host's objects while impersonating it still work
    with the restricted default DACL.
  - Still open: hardware test on a seat PC (account creation, grants on `WinSta0` /
    `Default`, fallback to SYSTEM Low on exit 44); a clipboard policy change reaches a
    running host only at its next start; uninstall from a non-SYSTEM prompt cannot run the
    revoke helper in other sessions (warned; ACEs go at logoff).

- 2026-11-13 (kiosk-shell + fix-kiosk-shell, round 3): **U3 / D′ concurrent kiosk seat
  shell built, review findings fixed** (architecture §4.1.3). Built and unit-tested;
  **untested on hardware**; nothing committed.
  - Built (kiosk-shell): `pulsar-seat-host --kiosk-shell --seat N --plan <hex>` per seat in
    the console session (desktops backend), launcher + task bar + software cursor per
    seat, apps in the shell's kill-on-close job, background raw input by device handle
    routed as posted messages (`seat_input::kiosk::KioskRouter`), focus sink when seat 1 is
    a kiosk seat, idle / logout reset, manager kiosk dialog (Turkish).
  - Fixed (fix-kiosk-shell, 24 findings, two pairs were duplicates; none skipped as
    wrong):
    - Graceful stop: shell stop event `Local\PulsarSeat.KioskStop.NN`, set by the service
      (`ShellStopper`, `Session\<id>\…`) before `kill_job`, 3 s timeout; `WM_CLOSE` /
      `WM_ENDSESSION` end the shell too; shutdown releases clip, foreground lock, mutex and
      the app job. Killed sink leader with no other sink shell: `--kiosk-release` helper.
      The leader clips only while the sink is foreground and stops after 30 s without the
      service.
    - Ctrl+Alt+Del no longer advertised; home = bar button or Ctrl+Alt+Home (consumed by
      the router). SAS and system hotkeys documented as hard limits (mod.rs, router doc,
      architecture, manager `kiosk.limits` / `kiosk.systemKeys`, shell `tr::HINT`). A
      `WH_KEYBOARD_LL` mitigation was not built: an LL hook has no device handle, and
      correlating with raw input timing would also eat seat 1's own Win / Alt+Tab
      (HYPOTHESIS that it could be made reliable; not tried).
    - `SWP_ASYNCWINDOWPOS` + `IsHungAppWindow` skip for every app window move / raise.
    - Key state re-applied per thread until the modifiers are up; generic `VK_SHIFT` /
      `VK_CONTROL` / `VK_MENU` in `wParam`; key ups reach the app after a shell focus
      change; release posts the VK and keeps Caps / Num / Scroll toggles.
    - Non-client clicks: `WM_NCHITTEST` on button down; caption / border drags done by the
      router + host (no modal loops), caption buttons, caption double click, menu bar /
      system menu in keyboard mode. Not built: top-level scroll bars (wheel works), popup
      menus still follow the real cursor.
    - Placement: any seat monitor, nearest work area; maximised windows covering their
      monitor are left alone. Idle reset only after input since the last reset. Lost touch
      contact released. `job_pids` grows on `ERROR_MORE_DATA`, keeps the last good set.
    - Validation: kiosk `command` must be absolute and trimmed (seat-core, mirrors the
      manager); `plan_for` trims and drops relative commands.
    - `admin_only`: new client kind `kiosk_shell` (seat-proto, additive), verified by PID +
      session against `SessionBackend::kiosk_shells()` (seat-core, additive default);
      `Caller::kiosk_seat` counts as a service process.
    - Filter forwarding: a verified shell may subscribe to `host` and gets only its seat's
      `input_packets` (`Scope::kiosk_seat`), so seat 1 as desktop + KMDF filter is
      isolated. Bar notice `FILTER_STREAM_LOST` while the stream is down.
    - `desktops::query` reports a dead seat-1 shell as `Gone`.
    - Copy: `NOT_ISOLATED`, `kiosk.seatOneDesktopHint` name the mouse leak and cross-seat
      activation; `kiosk.intro` qualified; `kiosk.backendOnly` shown in the basic tab when
      another session method is set and a seat has a kiosk config.
    - Spec 35 itself not edited (analyst-owned, CLEANROOM.md); the limits are recorded in
      architecture §4.1.3 for the analyst to fold in.
  - Verification (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo test -p seat-core
    -p seat-proto -p seat-session -p seat-input -p seat-host -p seat-service` all green
    (new: router 6, shell 3, desktops 1, auth 1, service 1, events scope, validate);
    clippy for the six crates on aarch64 + x86_64 (see fix-kiosk-shell final report);
    manager `bun run test` 103/103 (14 files), `check` 0/0.
  - Untested on hardware: the whole shell (windows, raw input, posted input, focus sink,
    clip), the cross-session stop event (`Session\<id>\` prefix is a HYPOTHESIS), the
    release helper, `WM_NCHITTEST` from a per-monitor-aware shell to DPI-unaware apps,
    the filter packet stream to the shell.
  - Still open: input filter is the only way to keep kiosk keyboards out of Windows system
    hotkeys and SAS; top-level scroll bar clicks; popup menus with the seat cursor.
- 2026-11-13 (rdp-channels + fix-rdp-channels): **clipboard, drive and smart-card
  redirection on the IronRDP client**, then 17 review findings on it handled (none skipped
  as wrong; one partly: drive identity is re-checked before every root open rather than
  before every create IRP, because the native back-end resolves every IRP relative to the
  root handle it opened, which stays bound to its volume). Nothing committed, installed or
  run on hardware.
  - rdp-channels: `SeatConfig.redirect` (`SeatRedirect`: clipboard direction + format
    classes, drives off / read-only / read-write, smart cards), resolved by the service
    into `HostAssignment.redirection` (`seat_rdp::policy::host_redirection`); seat-rdp
    `channels/` (cliprdr filter around IronRDP's Windows clipboard back-end, `SeatRdpdr`
    around its drive / WinSCard back-end, rate-limited refusal log, ticker); printers and
    ports never redirected.
  - fix-rdp-channels (architecture §4.8 "Redirection hardening"):
    - Smart cards: instance-id matching (`SCardGetReaderDeviceInstanceIdW`), name fallback
      only for `name_unique` refs with a single present reader of that name, else
      `reader_name_ambiguous` (test: two identical readers on two seats); `ListReaders`
      per requested group; `SCardListReadersW` retried on `SCARD_E_INSUFFICIENT_BUFFER`;
      PnP count translated / rewritten to the seat's filtered count; reader refs follow
      live updates.
    - Drives: volume GUID pinned per letter (`RedirectDrive.volume`, re-checked before each
      open, `drive_letter_moved`), FAT-family only (`drive_acl_volume_refused`: IRPs run as
      the host token, confused deputy otherwise; impersonation impossible at Low
      integrity), unassigned drives removed from the live session (`drive_unassigned`).
    - Clipboard: empty Format List only as the initial list or to withdraw host data;
      fully refused seat copy releases the console copy; copies from other seat hosts
      refused unless `cross_seat`; connection-tagged messages and pending-request matching.
    - ActiveX host derives `ClientPolicy` from the policy (unfiltered clipboard only, never
      drives / readers, refusals reported).
    - Reporting: `SeatRuntime.redirect_notes` / `redirect_refusals` (seat-core, additive),
      `host_redirect_refusal` request (seat-proto, additive); manager: `SeatConfig.redirect`
      type, per-seat "Yönlendirme" dialog in the Temel tab (pano / sürücüler / akıllı kart,
      shared-clipboard warning), notes and refusals in the Durum tab, Turkish strings.
    - seat-media `redirect`: old rule set removed (single source of truth is
      `seat_rdp::policy`); only `seat_cameras` stays.
  - Verification (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo test -p seat-rdp -p
    seat-host -p seat-service -p seat-core -p seat-proto -p seat-media` all green (seat-rdp
    93, seat-host 46, seat-service 173); clippy `--all-targets -D warnings` clean for those
    six on aarch64; `cargo check --target x86_64-pc-windows-msvc -p seat-rdp -p seat-host
    -p seat-service` clean; manager `bun run test` 108/108 (15 files), `check` 0/0.
  - Untested on hardware: every channel; HYPOTHESES: the reader instance id equals the
    inventory instance id (or a child of it); no other host-session program uses IronRDP's
    clipboard window class; `EmptyClipboard` after `OpenClipboard(NULL)` releases a
    delayed-render owner cleanly.
  - Still open: drives added while connected appear only at the next connection; card
    handles already open on a reader that gets reassigned stay valid until closed; the PnP
    wait still wakes on other seats' reader changes (count hidden, timing not);
    `ListReaderGroups` is forwarded unfiltered (PC-wide group names).
- 2026-11-13 (release + fix-release, round 3): **release signing and release gates**
  (journal has the details). Release entry: `arm64ec-pc-windows-msvc` rustup target added;
  the ARM64 MSI carries the ARM64EC API helper (`apihelper/arm64ec`, PE 0x8664 with CHPE
  hybrid metadata, checked by check-payload); update check needs `SHA256SUMS.txt.minisig`
  (minisign Ed25519, pre-hashed) from a key in `installer/release-signers.txt` (compiled
  in, **empty on purpose**: no real key exists, agents never create it); new crate
  `seat-release` (verify lib + maintainer tool keygen / sign / verify / check-signers);
  Authenticode pin optional (`AUTHENTICODE_PINS`); SDV moved to an x64 CI job (EWDK 22621
  does not run on the ARM64 host, docs/release/sdv.md); docs/release/ checklist, altitude
  request draft, sdv.
  - fix-release (13 review findings, 12 fixed, 1 corrected in substance):
    - `keygen` creates `minisign.key` with `CreateFileW` + protected DACL
      `D:P(A;;FA;;;<user>)(A;;FA;;;SY)` from the first byte, reads the DACL back, refuses
      FAT / exFAT unless `--allow-unprotected` (`crates/seat-release/src/keyfile.rs`);
      `release-keygen.ps1` throws on an icacls failure and checks the ACL.
    - `has_signer_lines` (release-build const assert) and `parse_signers` share one trim
      rule (Unicode White_Space + U+FEFF); test asserts both agree on BOM / NBSP inputs.
    - Secret key base64 encode / decode into pre-sized `Zeroizing` buffers.
    - Signed trusted comment now `timestamp:…<TAB>file:…<TAB>version:X.Y.Z<TAB>hashed`; the service
      requires `version` = release tag (`sums_mismatch`) and refuses a timestamp older than
      the newest accepted one (`sums_rollback`, `updates\state.json`
      `sums_signed_unix_s`). The freeze / older-but-newer-than-installed limit is
      documented (update.rs module doc, checklist §2, installer/README.md).
    - New reason ids `sums_unsigned`, `sums_untrusted`, `sums_mismatch`, `sums_rollback`,
      `no_release_key` with Turkish texts (apps/manager/src/lib/update.ts); `unsigned` /
      `untrusted_signer` stay Authenticode only.
    - `run-sdv.ps1`: fails on 0 passes, timeouts / spaceouts (`-AllowTimeouts` local
      only), missing counts; `Driver_SpectreMitigation` now really set false (throws if
      not); summary line carries the verdict. Checklist §7.1: the `sdv` job is advisory
      for CI artifacts (not in `installers` needs).
    - Licence texts: `check-payload.ts --notices` (LICENSE / NOTICES / SOURCE-OFFER +
      THIRD-PARTY-NOTICES-RUST / -JS ≥ 1 KiB under `licenses/`), run by
      `sign-release.ps1` and by `build-installer.ps1 -Release`, which also refuses to start
      without the generated notices.
    - Stale dist MSIs: `build-installer.ps1` writes `<msi>.build.txt` (version, arch,
      commit, dirty, release, service SHA-256); `sign-release.ps1` refuses an MSI without
      it, from no / another / a dirty commit, not matching tag `v<version>`, not `-Release`,
      or whose service binary differs from the record, and any MSI in the folder not in
      SHA256SUMS.txt. The stale x64 MSI was removed from `target/installer/dist`.
    - Checklist §1 leak check: must print nothing; `git grep -n -e RWQAAEIy -e RWRTY0Iy`.
      The finding's prefix was wrong for our keys: `RWRTY0Iy` is `EdScB2` (minisign
      password-protected); our unencrypted `Ed 00 00 B2` key starts `RWQAAEIy` (unit test).
  - Dist now: only `PulsarSeat_0.1.0_arm64.msi` (SHA-256 e2b6a495…e888, ARM64EC helper,
    built 14:33 **before fix-release**: its service lacks the version / rollback check and it
    has no build record, so sign-release.ps1 refuses it; test-only, unsigned). Rebuild both
    arches from the release tag for a release.
  - HYPOTHESIS: a CI tag build leaves no tracked file modified (the build record's `dirty`
    uses `git status --untracked-files=no`; generated notices are untracked).
  - Untested on hardware: FAT / exFAT refusal of keygen (NTFS path tested by a unit test
    and a scratch keygen + sign + verify run); SDV result parsing (SDV never completed a run).
- 2026-11-14: **merge step 7 done** (session layer, architecture.md 10.4). `split-session`,
  `split-rdp-helper` and `split-termwrap` are gone. `seat_session::remote` holds Split's
  session manager (seat users, start / stop / tick, `PSS…` discovery, launch, run state
  `run\remote-sessions.json`, job `PulsarSeat.RemoteSeat.<id>`, sweeps); edition probe,
  `HostClass`, real probes (WMI role first, listener TCP table + certificate thumbprint,
  grace days), registry, WTS model, launch helpers are one implementation for both kinds.
  **One Terminal Server writer + one journal** (`rds::RdsApplier`, `journal\session.jsonl`):
  `Claim::Local` / `Claim::Remote`, rows = union of claims, releasing one kind keeps the
  other kind's rows, created accounts journaled as kind `account`; Split's `Originals` key
  and its "path B" rows are gone (the wrapper owns those two rows). Fixed order: rows →
  second NLA-off listener → wrapper; restore reversed (setup `uninstall` hooks now run
  termwrap before session). **Headless hold**: `seat_rdp::headless` (proto always built,
  IronRDP client behind `client`) and `pulsar-seat-host --headless-hold`; protocol = Split's
  helper contract unchanged (argv after the sub-command, one stdin password frame, JSON Lines
  on stdout). **Sweep**: `plan_sweep_for(kind, …)` knows `PSEAT-NN` and `PSS…` and never
  plans the other kind's sessions (also with a stale record); the local newest-session
  fallback never takes a `PSS…` session. **termwrap**: one module `seat_session::termwrap`,
  always compiled, `enable` needs feature `experimental-client-sku` + an x64 host + ack
  version 2 (split-manager now shows Seat's warning text, version 2; its state no longer
  uses `HKLM\SOFTWARE\PulsarSplit\Termwrap`). Gates green: cargo 1489 passed (was 1566;
  **97 tests deleted on purpose with their code**, every one with an identical or
  rewritten copy in the merged crate: all 73 `split_termwrap` tests (the same tests as
  `seat_session::termwrap` / `acl` / `journal` / `pe` / `version` / `win::files`, Seat's
  port of that crate) and 24 `split_session` tests: edition `classify_matrix`,
  `product_type_parse`, `file_version_from_fixed_info`, `reads_live_box_values_from_registry`,
  `missing_or_unknown_values_are_errors`; journal `record_shape_matches_contract`, `rfc3339`,
  `jsonl_file_appends_with_sequence_and_skips_torn_lines`; win registry
  `encode_decode_round_trip`; rds `rows_are_the_write_rows_only`,
  `apply_saves_originals_once_and_restore_puts_them_back`,
  `restore_skips_values_changed_after_us`, `policy_values_are_reported_not_fought`,
  `gpo_change_after_our_write_is_reported_not_rewritten`, `wrapper_rows_only_on_request`
  (the RDS writer no longer writes the wrapper rows), `addins_are_verified_only`,
  `listener_values`, `licensing_mode_policy_first`, `stock_service_dll`; launch
  `quoting_matches_crt_rules`, `command_line_quotes_exe`, `env_block_merges_and_sorts`,
  `non_ascii_first_char_does_not_panic` (shared `crate::launch` copies); win wts
  `client_address_decoding`. 20 added (rds claims / both kinds / remote rows / GPO /
  add-ins, edition wire name + live box values, wts kinds + local fallback, sweep both
  prefixes, termwrap x64-only / feature gate / proto mapping, remote launch + WTS, headless
  sub-command, seat-host stdin round trip + no-sub-command); the other 53 Split session
  tests, 5 protocol units, 4 client units and 5 dry-run tests moved unchanged.
  clippy aarch64 + x86_64 clean, vitest 115 + 45, installer scripts 29 (+ Split's 15), shim 8
  (host; x64 check only). Untested on hardware (no Server + RDSH box): every
  `seat_session::{win, remote::win}` path, the headless hold against a real TermService.
  Open: `installer/split/` (WiX, NSIS conf, README) still names
  `pulsar-split-rdp-helper.exe` (step 10: the MSI ships only `pulsar-seat-host.exe`).
  **Next:** step 8 (audio, pads + shim, display traits).
- 2026-11-14: **merge step 8 done** (audio, pads + shim, display; architecture.md 10.6).
  **Audio**: `seat_audio::cfgmgr` is the one copy of the CfgMgr32 read primitives
  (`seat_media::pnp::WinPnpTree` and the remote kind's `CfgMgrNodes` use it). The endpoint-id
  model (`mmdevapi_instance_id`, `is_session_endpoint_id`, `parse_endpoint_id`) is shared with
  `seat_media::audio`. Routing stays per kind. **Pads**: `seat_input::local_pads` (feature
  `local-pads`, off by default, not wired into the service): physical pad, then `mirror_kind`,
  then `seat_pads::mapping::resolve`, then a seat-owned virtual pad (session id + user SID).
  Limits are in `crates/seat-pads/README.md`: ViGEm only if installed, usbip-win2 DualSense,
  own pad driver unsigned and only on explicit preference, physical pad still shared without
  the HID filter. `seat-shim-core` / `shim/` stay remote-only. **Display**: `split-display`
  is gone; it is now `seat_display::remote` (README merged into `crates/seat-display/README.md`).
  `seat_display::SeatDisplayBackend` is one trait over both kinds (`DisplayBackendId`,
  `DisplayBackendStatus`, `Local` / `Remote` adapters). drivers/README.md has the "one own
  IddCx driver later" direction and a merge sketch; the two C++ drivers are not merged. No
  `split-*` crate is left. Gates green: cargo 1501 passed / 5 ignored (was 1489; +12, none
  deleted: seat-audio's `multi_sz_split` moved into `cfgmgr::multi_sz_and_wide`), clippy
  aarch64 + x86_64 clean, x64 check, vitest 115 + 45, installer scripts 29, shim 8 (host;
  x64 + i686 check only). Read-only live probes (`seat-audio live_read_only_probe`,
  `seat-media live_tree`) pass on the ARM64 dev laptop. Untested on hardware: every pad and
  display path; `local_pads` never ran against a real backend.
  **Next:** step 9 (one manager).
- 2026-11-14: **merge review fixes** (9 findings, all confirmed). (1) Remote seat status is
  admin only for events too: `events::Scope::admin`; non-admins never get `stream`, or
  `seat_state` / `notice` of remote seats, also with empty topics. (2) Deleting a remote
  seat with `apply_config` is refused (`seat_running`) while it is active; after the save
  `Controller::forget_seat` removes its stream secrets, folder, logs and firewall rules.
  Split's unreachable `delete_seat` handler and the legacy `PIPE_NAME` are gone. (3)
  Uninstall deletes `L$PulsarSeat.Remote.<id>.*` for every remote seat in the config and
  every `seats\<id>` folder (no LSA enumeration by prefix: no documented API). (4) The
  `otp_changed` / `trust_changed` / `relay_auth_required` events are `stream` events again;
  the pair dialog reacts to them. (5) The `pulsar-host` hello capability is `split_pads`
  again (contract unchanged); contract note on the moved data and install roots. (6) CR
  byte in `remote/win/wiring.rs` fixed. (7) `SECRETS_KEY` marked legacy cleanup. (8) Stale
  names: `seat-setup` CLI docs, `.gitignore`; Sunshine UA / web UI user kept on purpose
  (comment). (9) The shim's data root is `%ProgramData%\Pulsar Seat\shim` (database copy in
  `sdb\`); the product root is only checked, never re-permissioned; `REMOVEUSERDATA=1` also
  deletes the old `%ProgramData%\PulsarSplit`; `FileRestoreLog::default_path` removed.
  Gates: cargo 1507 (was 1504, +3), clippy aarch64 + x86_64 clean, vitest 181 (+3), check,
  build, installer scripts 49, shim 8 (host; x64 + i686 check). None deleted (2 controller
  `DeleteSeat` assertions now call `forget_seat`; the legacy round-trip test uses
  `StopSeat`).
  **Resolved 2026-11-14 (pulsar-desktop ad6c9ca):** the seat host check
  (`desk:src-tauri/src/seat/win.rs`) now accepts `pulsar-seat-service.exe` and the old
  `pulsar-split-service.exe` image name. Several doc comments in seat-stream /
  seat-pads / seat-display still say `%ProgramData%\PulsarSplit` where the data root is a
  parameter (the service passes `Pulsar Seat`).
- 2026-11-14: **merge step 10 done** (one MSI for x64 and ARM64; installer/README.md "Remote
  seat kind"). `installer/split/` is gone. Pins moved to `installer/remote/{drivers,
  stream-server}.json`, `fetch-deps.ts` to `installer/scripts/` (output
  `target/installer-deps/<arch>/remote/`; new `drop_paths` leave out Sunshine's `scripts\`
  (its own service install, firewall allow rules, bundled ViGEmBus installer) and
  `tools\sunshinesvc.exe`). `check-payload.ts` is the union (web UI, rfxvmt / AcRes /
  d3dcompiler, DuoDriver, pinned `remote/drivers/<id>/` INF + CAT packages without
  `--allow-drivers`, x86 upstream installer stubs there, `shim/x64` / `shim/x86` machines, no
  shim in ARM64, `shim_outside_folder`). `bundle-config.ts` generates `PulsarSeatRemote.wxs`
  (one component per payload file below `<install>\remote\`); `PulsarSeat.wxs` adds the shim
  (x64 only, group `PulsarSeatShim`), `remote-setup install` (fresh) / `reconcile` (always)
  after `install`, `PULSARDRIVERS` / `PULSARCONSENT` with launch-condition validation, and a
  fresh-install default `PULSARDRIVERS=none` (decision: a local-only PC gets no virtual
  display or USB/IP driver; opt in with `PULSARDRIVERS=vdd`). seat-setup: data root is
  `%ProgramData%\Pulsar Seat` (Seat DACLs, owner SYSTEM), payload `<install>\remote\` (the
  service reads the stream server there), log `logs\remote-setup.log`, `install` /
  `reconcile` stop and delete a leftover `PulsarSplit` service, the generic replay skips
  seat-compat's `changes.jsonl`, `shared_data_root` leaves the data folder to the Seat
  uninstall. The Seat `uninstall` (MSI, rollback, CLI) now runs the remote uninstall first, so
  one action replays both journal formats. Licences and Sunshine's source offer merged into
  `installer/licenses/`. CI: the shim job uploads `shim-x64`, the installers job needs it;
  the `installer/split/scripts` step is gone. Also fixed: the stale
  `crates/seat-compat/apihelper/Cargo.lock` (missing `zeroize` of seat-core, broke
  `--locked` MSI builds).
  - Gates (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo test --workspace` 1504 passed
    (was 1501; +3), clippy `-D warnings` aarch64 + x86_64 clean, manager vitest 178 / check /
    build, shim 8 (host; x64 + i686 check), `bun test installer/scripts` 49 (was 29 + 15 in
    `installer/split/scripts`). Deleted on purpose: Split's 8 `installer/split/scripts/
    check-payload.test.ts` tests (its script is merged into Seat's; the rules are covered by
    9 new tests in `installer/scripts/check-payload.test.ts`). Split's 7 fetch-deps tests
    moved unchanged (+1).
  - MSIs (unsigned, `installer/scripts/build-installer.ps1`, never installed), extracted with
    `msiexec /a` and PE-checked:
    - `PulsarSeat_0.1.0_arm64.msi` SHA-256
      `1274cafaa1615422847b5c8aeefacb9d94eb74eb2bc1c4fed9398533a661e450` (51 265 536 bytes):
      service, host, agent, manager, credprov, Sunshine, VDD DLL all ARM64 0xAA64; API
      helpers x86 / x64 / arm64ec by folder; usbip-win2 installer stub x86; no shim.
    - `PulsarSeat_0.1.0_x64.msi` SHA-256
      `b0bdfa68a0dad467f370bd4bc08b71ba596b3db960bedab43088315cc181c77e` (65 101 824 bytes):
      everything x64 0x8664 except `shim\x86\` and the API helper x86 (0x014C) and the
      usbip-win2 stub (x86).
    - Built from the uncommitted step 10 tree on top of 4fc88d7 (build records say
      dirty); rebuild from the commit for anything beyond a smoke test.
  - Open: nothing installed or run elevated (remote custom actions, driver opt-in,
    leftover-service removal, chained uninstall). (Fixed by the merge review: the shim's
    journal and DXVK cache moved to `%ProgramData%\Pulsar Seat\shim`.) Split's NSIS hooks, the `Termwrap\Offered` MSI feature and the
    "reference seen" marker are dropped (no reader). `remote-setup --register-service` now
    registers the `PulsarSeat` service (NSIS only, not shipped).
  **Next:** step 11 (hardware tests per kind, then mixed; the maintainer runs them).
- 2026-11-14: **merge step 9 done** (one manager; architecture.md 10.7). `apps/split-manager`
  and its Tauri crate are gone; Split's pages moved with history to
  `apps/manager/src/lib/remote/`. New tab **Uzak koltuklar** (remote seat list with avatars,
  seat editor with GPU warnings, pair dialog with QR, host, logs, DXVK, remote settings); the
  seats tab shows each seat's kind (local containers get `Yerel`, remote seats are listed
  under the canvas with `Uzak` and an edit button; no devices on remote seats). One
  `i18n.ts` (Split copy = `rt` + `remote*` helpers, "Pulsar Split" now "Pulsar Seat"). The
  manager speaks protocol 3 only: remote requests in `remote/api.ts`; remote seat create /
  edit / delete and remote settings are config edits (`ManagerStore.applyRemote`: validate,
  apply on top of the saved config, merge the remote part into an unsaved draft);
  `remote/model.ts` maps record / status / events (inverse of `to_record`, `runtime_of`,
  `map_legacy_event`). Dropped with Split's Tauri crate: SCM start / stop buttons and
  `tauri-plugin-dialog`. Kept: LAN address order (`lan_addresses`). Gates green: cargo 1501
  passed / 5 ignored (unchanged: Split's 2 manager Rust tests ported into
  `pulsar-seat-manager`, `net::physical_adapters_come_first` and
  `pipe_client::secret_requests_fail_closed_and_never_print_the_secret`; none deleted),
  clippy aarch64 + x86_64 clean, manager vitest 178 (was 115 + 45; +18 mapping and tab
  tests), installer scripts 29 (+ installer/split 15), shim 8 (host; x64 check). Untested
  on hardware and against a real service. Open (fixed by the merge review: the three
  events are `stream` events now): protocol 3 drops Split's `otp_changed` /
  `trust_changed` / `relay_auth_required` events, so the Pulsar pair dialog no longer
  refreshes on them; installer/split bundle configs still name `apps/split-manager`
  (step 10).
  **Next:** step 10 (one MSI, x64 + ARM64).
- 2026-11-14: **merge steps 5 + 6 done** (merge-plan.md section 3). Step 5: `SecretStore`
  gained the remote seats' Pulsar slots (`RemoteSecretSlot` identity / connect password /
  relay key, `set_remote` / `get_remote`, LSA names `L$PulsarSeat.Remote.<seat-id>.<slot>`,
  `MAX_SECRET_CHARS` 4096); LSA limits checked and written down (architecture.md 10.3: no
  value needs the encrypted-file fallback). Split's DPAPI code and HKLM secrets store are
  deleted, no importer. Step 6: one service. `crates/split-service` and
  `pulsar-split-service.exe` are gone; Split's controller + engine are the remote seat
  worker in `crates/seat-service/src/remote/` (architecture.md 10.5): protocol 3 remote
  requests reach it by seat number, `start_seats` / `stop_seat` / autostart cover remote
  seats, it reads the config document (`DocStore`) and writes back only `rdp_client_name`,
  `last_mode`, `settings.remote.multi_session`; remote runtimes in `get_status`, events
  mapped to `seat_state` / `notice` / `stream`. Split's suspend / resume and session-change
  handling ported into Seat's SCM skeleton; engine 1 s tick, pad pump and the shim health
  pipe run in the Seat runtime; Split's setup sub-commands are `remote-setup ...`. Gates
  green: cargo 1566 (was 1564; 9 split-service tests deleted on purpose: DPAPI round trip,
  HKLM registry store x4, Split pipe SDDL + current SID, Split's own pipe round trip, Event
  Log text cap; 11 added), clippy aarch64 + x86_64 clean, vitest 115 + 44, installer scripts
  29, shim 8 (host; x64 check). Untested on hardware. Open: `apps/split-manager` has no
  service until step 9 (its pipe is gone); `installer/split/` still names
  `pulsar-split-service` (step 10); package registry keys under `HKLM\SOFTWARE\PulsarSplit`
  (shim, termwrap, runtime seat map) and the `PulsarSplit` pipe names (health, pulsar-host)
  are unchanged; `startup_apps` stays local-only (remote seats use `RemoteSeat.apps`).
  **Next:** step 7 (one `rds_loopback`, `seat-rdp` headless mode replaces
  `split-rdp-helper`, one wrapper kind).
- 2026-11-13 (integrate, round 3): **whole tree green after egfx-h264, rdp-channels,
  host-containment, kiosk-shell, credprov, second-listener and release** (with their fix
  rounds). Nothing committed, installed, registered or run on hardware.
  - Gates (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo build --workspace`; `cargo
    test --workspace` 818 passed / 3 ignored on aarch64 and on x86_64 (emulated); `cargo
    clippy --workspace --all-targets -D warnings` clean on both targets (the seat-proto /
    seat-rdp lints the package agents saw mid-run are gone); `cargo build --release`: agent,
    host, manager, release tool, service, manager lib all PE 0xAA64; manager `bun install` /
    `test` 109/109 (15 files) / `check` 0/0 / `build` ok; `bun test installer/scripts`
    29/29; `build-drivers.ps1 -Analyze` ARM64 + x64 Release + Debug clean (0xAA64 /
    0x8664, infverif, ApiValidator); `build-credprov.ps1` ARM64 + x64 Release, self-test
    0 failed; MSIs `PulsarSeat_0.1.0_arm64.msi` (ARM64EC helper, SHA-256 70802f46…5584)
    and `PulsarSeat_0.1.0_x64.msi` (SHA-256 059b52ca…11ec), payload clean outside and inside, both
    built after the integrate edits, unsigned, test only. Console smoke: `--console
    --allow-current-user --pipe PulsarSeatSmokeIG` + `status` over the pipe ok, `report
    --no-mask` refused for a non-admin (journals in a user-owned data folder refused fail
    closed, as designed).
  - Fixed (decision-free gaps from the package reports):
    - IDD frame reader SID: `SeatDisplayPrefs.frame_reader_sid` (seat-core, serde
      default) → `Binding::configure_seat`; the service fills it from
      `Platform::host_frame_reader_sid` = `win::containment::frame_reader_sid` (the
      `PulsarSeatHost` SID while hosts run as the account; `None` under `HostTokenCompat`
      or after an account failure). Tests in seat-display and containment. Untested on
      hardware (IDD driver unsigned). Architecture §4.2 "Per-seat options" rewritten (it
      still described the never-called `set_seat_options` path).
    - Manager: Turkish text for 18 backend probe / apply notes that were shown raw
      (`kiosk_shell_seat_aware_apps_only`, `one_kiosk_visible_at_a_time`,
      `no_per_user_isolation`, `two_admin_sessions_only`, `rds_licensing_not_configured`,
      `loopback_listener_created` …); test added.
    - seat-core `validate`: `LoopbackPortInvalid` (port < 1024 or 3389 under
      `rds_loopback` with the listener on), same code as the manager's local check.
    - troubleshooting §9 (Turkish): kiosk seats, clipped cursor / foreground lock recovery
      (Ctrl+Alt+Del + İptal, Ctrl+Alt+Home, `disable` + `sc stop`, log off) and the U3
      limits.
    - status: the final-fix "Still open" list updated (credprov, read containment, EGFX /
      channels struck); progress notes of the round merged and deleted.
  - Still open (all packages, after this run):
    - Hardware: nothing of round 3 ran on hardware. First runs needed: Windows Server +
      RDSH (EGFX AVC420 / AVC444 and pacing, second listener, channels), a seat PC for the
      host account (grants, exit 44 fallback, IDD reader SID), LogonUI for the credential
      provider, the kiosk shell on a multi-monitor console.
    - ~~EGFX: zero-copy D3D11 decode into the flip presenter (own GPU compositor for EGFX
      surfaces); decoder on the presenter's adapter; no FrameAcknowledge queue-depth
      back-pressure~~ built since (round 4 egfx-perf, untested); vendor async hardware MFTs
      unused.
    - Credential provider: seat 1 is not a switch target (FUS backend must remember seat
      1's session); temporary `std::string` copies may hold password pieces briefly
      (HYPOTHESIS); DLL unsigned.
    - Host containment: one shared account for all hosts (no per-seat accounts); no loaded
      profile (TEMP / HKCU fall back to the default profile); `--console` never uses the
      account.
    - Second listener: `reselect_backends` creates a backend without `prepare_host` /
      `restore_host` (older behaviour), so the listener follows only after a restart or
      disable / enable; logon-screen seats without an account need `extra_accounts`
      (secure default, maintainer may choose otherwise).
    - Kiosk shell: system hotkeys / SAS reach every seat without the input filter; U3
      limits (raw input / DirectInput / XInput apps, exclusive full screen, UWP / MSIX).
    - Channels: drives added while connected appear at the next connection; card handles
      survive a reader reassignment until closed; `ListReaderGroups` unfiltered; printers
      and ports never redirected (by design).
    - Release (maintainer steps, docs/release/checklist.md): real minisign release key
      (`release-keygen.ps1`, line into `installer/release-signers.txt`); Authenticode
      route + certificate (EV also for driver attestation); public repo
      `PulsarDesk/pulsar-seat` + tag (GPL-3 §6 source offer; CI never ran); SDV on the x64
      CI job (never ran; runner disk HYPOTHESIS); minifilter altitude request (draft ready,
      send after approval and the public repo); Driver Verifier / HVCI on a test VM;
      `DRIVER_BLOCKLIST_SHA256` pin; CodeQL driver queries; MSI install / upgrade /
      uninstall never run; update check never run against a signed release.
  - Next: maintainer review of round 3; release key + signing decision; first hardware
    run on a Server + RDSH box with approval.
- 2026-11-13 (final-fix, round 3): **20 round-3 final-review findings handled, wizard-key
  decision applied.** The first final-fix run was cut off by PC sleep; this run checked the
  half-done tree (all earlier edits were complete and compiled) and finished it. Nothing
  committed, installed, registered or run on hardware.
  - Done (verified findings):
    - Credential provider auth: switch-link / agent-switch starts are *unattended*
      (`start_sequence_with(.., true)`, `credprov.unattended[seat] = generation`) and arm
      no one-time logon grant, so the seat user types the password on the seat tile; a
      running target is reconnected without the stored password, so `session_not_locked`
      always applies. `credprov_take` sets `taken` under the lock and reads the LSA secret
      outside it (`credentials_unlocked`), re-checks the grant, drops it on a failed read.
    - Seat 1 as a switch target (U1 way back): `SessionBackend::console_seat_away()`; the
      FUS backend remembers seat 1's console session + user SID (`home`) when it first
      moves the console away, re-checks the SID, keeps it across `adopt`, and
      `start(seat 1)` connects it back with the lock rules of the other seats.
    - Spec 35 §5.5(a): `agent_switch_targets` / `agent_switch{seat}` (additive proto,
      `Access::Agent`, same checks as `credprov_switch`); `pulsar-seat-agent` tray icon with
      Turkish "N. koltuğa geç" items.
    - `reselect_backends`: on a session-backend kind change (product enabled) the old
      backend's `restore_host` runs before and `sync_listener_plan` + `prepare_host` after
      the swap; test switches rds_loopback → fast_user_switch → rds_loopback on the mock.
    - `host_account`: `Lookup::Foreign` (comment is not `ACCOUNT_COMMENT`): `ensure`
      refuses, `remove` leaves it.
    - seat-rdp: `fix_pnp` tests the encoded bytes (header magic + completion id), no Debug
      text; `nv12_planes` uses checked arithmetic (DecoderError instead of a panic).
    - seat-core validate `HideFsuWithFastUserSwitch` (error) + manager local check, Turkish
      text and an AdvancedTab note on the checkbox.
    - Manager mode labels per spec 35 §6.2 ("Eşzamanlı tam oturumlar (yalnızca Windows
      Server x64)", "Gerçek oturumlar, sırayla", "Eşzamanlı kiosk koltukları"); probe notes
      `fus_inactive_frozen`, `fus_audio_per_user`, `kiosk_audio_shared` (backend probes +
      i18n); kiosk limits text mentions audio.
    - credprov DLL: one 1 s budget per `ServiceClient` call (connect + hello + request);
      `ProviderFilter::Filter` reuses an answer younger than 2 s (`CachedQueryMaxAge`).
    - `spec/credprov.rs` (stray empty file) deleted; plan.md no longer names third-party
      products.
    - Release key (maintainer decision 2026-11-13): generated with `release-keygen.ps1`,
      key id `0E7EC5FC77170613`, secret in `%USERPROFILE%\.pulsar-seat\release-key\`
      (outside the repo), public line in `installer/release-signers.txt`; sign + verify ok;
      precise leak grep (`RW(QAAEIy|RTY0Iy)` + ≥ 100 base64 chars) prints nothing.
    - checklist §0b (first commit + tag, rebuild from the clean tag, then sign);
      `docs/agents/merge-plan.md` draft (seat kinds local / remote, shared crates, name left
      open) for the joint plan with the Pulsar Split session.
    - **Wizard keys (maintainer decision, plan.md 2026-11-13):** the console listener
      reports only F1..F12, top-row digits 1..0, Esc, Backspace, Enter, Shift, Alt
      (`seat_input::console_report::is_wizard_key`, `set_wizard(running)`); the
      `WizardKeys::All` mode and `Wizard::needs_all_keys` are gone; the service always
      sends `wizard_all_keys: false` and the listener ignores the field (kept, obsolete, so
      old peers still parse). Monitor pick by digit and "press any key" by F-key or digit
      keep working (service test `console_wizard_never_asks_for_every_key`).
  - Skipped / partly wrong (with reason):
    - Kiosk shell BackendReport `audio: console_shared`: the shell only *reads*
      BackendReport, it never sends one; the audio limit is in the probe note and the
      limits text instead.
    - pipe.cpp scrub: `credprov_query` carries no secret (wrong premise). The credentials
      frame is read into a buffer sized once and wiped on every exit, the parse tree is
      wiped, the password is decoded into a wiped `std::vector<wchar_t>`. Remaining
      HYPOTHESIS: the JSON parser may grow its string while parsing and leave a freed copy.
    - Release-key findings (two): done by the earlier part of this run, see above.
  - Verification (`CARGO_TARGET_DIR=target\wf`, ARM64 dev box): `cargo build --workspace`;
    `cargo test --workspace` 826 passed / 3 ignored on aarch64 and on x86_64 (emulated);
    `cargo clippy --workspace --all-targets -D warnings` clean on both targets; `cargo
    build --release` agent / host / manager / release tool / service / manager lib all PE
    0xAA64; manager `bun run test` 111/111 (15 files), `check` 0 errors / 0 warnings; `bun
    test installer/scripts` 29/29; `build-credprov.ps1` ARM64 + x64, self-test 0 failed;
    MSIs rebuilt after all edits (payload clean outside and inside, unsigned, test only):
    `PulsarSeat_0.1.0_arm64.msi` (ARM64EC helper, SHA-256 78068ba0…b862) and
    `PulsarSeat_0.1.0_x64.msi` (SHA-256 cc08aa82…be27). Drivers unchanged in this run
    (`build-drivers.ps1 -Analyze` result from integrate stands). `cargo fmt --check` is not a gate here and reports pre-existing
    drift (manager src-tauri and others), left alone.
  - Untested on hardware: everything above, in particular the unattended switch start,
    seat 1 `home` reconnect, the agent tray, the restore / prepare on reselect, the credprov
    deadlines in LogonUI, and the wizard with the listener's reduced key set.
  - Still open:
    - Hardware: all of round 3 (Server + RDSH, a seat PC, LogonUI, kiosk console).
    - EGFX: ~~zero-copy D3D11 decode, presenter adapter, FrameAcknowledge back-pressure~~
      (round 4 egfx-perf, untested), vendor async MFTs.
    - Credential provider: DLL unsigned; JSON parser string growth (HYPOTHESIS above).
    - Host containment: one shared account, no loaded profile, `--console` never uses it.
    - Second listener: seats without an account need `extra_accounts` (secure default).
    - Kiosk: system hotkeys / SAS reach every seat without the input filter; U3 app limits;
      no per-seat audio for kiosk apps (built 2026-11-14 round 4 kiosk-audio as a per-app
      default endpoint, untested; see the round 4 kiosk-audio entry).
    - Gamepads per seat: not built (HID pads only via the never-loaded KMDF filter, XInput
      always shared; `gamepads_shared`). **Decision item.**
    - Merge plan: draft only; needs the joint pass with the Pulsar Split session.
    - Release: first commit + tag `v0.1.0` (maintainer, §0b), key backups, Authenticode,
      public repo, CI, SDV, altitude request, MSI install / upgrade never run.
  - Maintainer decisions needed: gamepads per seat (adapt pulsar-split split-pads + HID
    shim "adapted from pulsar-split", or wait for the shared crate of the merge plan);
    first commit / tag; release-key backups; whether the agent tray / switch link should
    ever auto-logon (now: never, explicit per-seat opt-in would be needed); merge-plan
    review with the Split session.
  - Next: maintainer review; first commit + tag; first hardware run with approval.
- 2026-11-14: **merge steps 2 (finish) + 3 done** (merge-plan.md §3). `split/` is gone:
  crates in `crates/` (no-clash ones renamed `seat-stream`, `seat-pads`, `seat-shim-core`,
  `seat-setup`, `seat-audio`; clashing `split-*` kept for steps 4-8), `apps/split-manager`
  (until step 9), `shim/`, `drivers/split-idd`, `drivers/split-pad`, `installer/split/`
  (until step 10), `scripts/`, `spec/local/` + `spec/remote/`, `docs/split/` (Split history).
  One root Cargo + bun workspace, root `.cargo/config.toml` (+crt-static), `.gitattributes`.
  split-rdp-helper ported to seat-rdp's IronRDP versions (one `picky`). CI builds both
  managers, the shim workspace and both installer script sets. Gates green: cargo 1548
  passed / 5 ignored, clippy aarch64 + x86_64 clean, vitest 111 + 44, installer scripts
  29 + 15, shim 8 (host + x64 target), i686 check. Two services and two managers still.
  **Next:** step 4 (merge proto + core: `kind`, sub-structs, protocol v3).
- 2026-11-14: **merge steps 1 + 4 done** (merge-plan.md section 3). Step 1: contract delta in
  [architecture.md](architecture.md) section 10 "Seat kinds" (config schema 2, protocol 3
  request table with access and seat kind, `wrong_seat_kind` / `session_unavailable`,
  `SecretStore`, `SessionBackend` for remote seats, event topic mapping, ports only for
  remote seats). Step 4: `split-proto` and `split-core` removed. `seat-core` gained
  `error` (one `ErrorCode` / `ProtoError`), `spawn`, one `Secret`, `remote` (Split model as
  `RemoteSeatRecord` / `RemoteSettings`, validation, ports, stream config, watchdog, record
  store, `RemoteSeat`, `assign_resources`); `SeatConfig.kind` + `local: LocalSeat`
  (`redirect`, `kiosk`, `kiosk_shell` moved there) / `remote: RemoteSeat`,
  `settings.remote`, schema 1 -> 2 migration (old documents load as local seats),
  `kind_checks` validation, `SeatRuntime.kind`. `seat-proto`: `PROTOCOL_VERSION` 3, 32 Split
  requests as `Access::Admin` with `seat_kind()` / `min_protocol()`, topic `stream`,
  `seat_proto::remote` (wire types, `legacy` v2 enums for split-service / split-manager,
  `map_legacy_event`), zeroized framing. Split's Controller moved to
  `crates/split-service/src/control.rs`. seat-service: per-caller protocol check (config
  requests need a hello of 3), `wrong_seat_kind`, remote requests answer `not_implemented`
  (`remote_worker_pending`), remote seats never started by the local worker
  (`session_unavailable` without concurrent sessions), remote seat status and the `stream`
  topic admin only. Manager: `SeatConfig.kind` / `local`, protocol 3 in the demo, Turkish copy
  for the new codes. Gates green: cargo 1564 passed / 5 ignored (was 1548; 5 duplicate
  split-proto framing tests deleted on purpose, identical copies stay in
  seat-proto/src/framing.rs), clippy aarch64 + x86_64 clean, vitest 115 + 44, installer
  scripts 29 (+ Split's 15), shim 8 (host; x64 check).
  **Next:** step 5 (`SecretStore` slots + LSA for the remote seat's Pulsar secrets), then
  step 6 (Split engine as the remote seat worker; it then reads `SeatConfig.remote` instead
  of the record store and answers the protocol 3 requests).
- 2026-11-14: **round 4 kiosk-audio done** (architecture.md 4.1.3 "Audio"). Kiosk seats
  (one console session, one system default endpoint) now give each app process the seat's
  own endpoints as its **per-app** default: `seat_media::kiosk_audio` (trait
  `AppEndpointPolicy`, planner `KioskAudio`: set on launch while suspended, every tick for
  new job processes, re-applied on an endpoint id change, cleared before the job ends;
  fake `FakePolicy`), Windows `seat_media::win::policyconfig` (undocumented WinRT factory
  `Windows.Media.Internal.AudioPolicyConfig`, slot 25 `SetPersistedDefaultAudioEndpoint`,
  IID by build: < 17134 none, < 21390 downlevel, else current; `seat_core::consts::
  PER_APP_AUDIO_*`). Fallback: log + bar line (`tr::AUDIO_UNAVAILABLE` / `AUDIO_FAILED`).
  `desktops` probe note `kiosk_audio_per_app` (build ≥ 17134) instead of
  `kiosk_audio_shared`. Manager: "Ses: …" line per kiosk seat under the Kiosk column
  (`model.kioskAudio`), warning when two kiosk seats list the same program, new limits copy,
  troubleshooting.md section 10. Maintainer decision (this round): the per-app setter is
  allowed; spec 35 §5.3 / 60 §5 exclude only the system default setter.
  Gates green: cargo 1515 passed / 6 ignored (+1 ignored: live factory probe), clippy
  aarch64 + x86_64 clean, manager 183 / check / build, shim 8 (host) + x64 / i686 check,
  installer scripts 49.
  - Open (all HYPOTHESIS, nothing ran on hardware): the factory works from the
    non-packaged shell and for a suspended pid; running apps pick it up only for new
    streams; Windows keys the entry per executable (two seats with one exe share it);
    clearing needs a live pid (an app that ended by itself or a crashed shell leaves an
    entry until the next launch of that exe overwrites it); child processes; capture.
  - Risk: undocumented interface; a Windows update can change the IID (caught: factory
    missing -> fallback) or the slot order (not caught: a wrong method would be called).
    Recheck on each new Windows feature update.
  - Note: the unfinished kiosk-isolation work was parked in `git stash` during this
    package and restored afterwards (still uncommitted).
- 2026-11-14: **round 4 pads-drivers done** (architecture.md 10.6 "Pads + shim" and
  "Display"; drivers/README.md; drivers/seat-idd/README.md "Remote role";
  crates/seat-pads/README.md). (a) **Local pads wired into seat-service**: `seat-input`
  feature `local-pads` is now a normal dependency of the service; per seat
  `LocalSeat::virtual_pads` (config, manager "Sanal oyun kolu" in the seats table, Turkish
  hint), **default off**, because every usable backend (ViGEmBus, usbip-win2) is a driver
  install that Pulsar Seat never does. `crate::local_pads::targets` (seat with the switch,
  a session and a signed-in user; HID pads routed to exactly that seat, hotkey moves
  included) feeds `seat_input::local_pads::LocalPadHub::sync` on the 1 s runtime tick; the
  hub runs on the remote engine's one `PadManager` (`Engine::local_pad_parts`): one
  ownership registry, orphan cleanup at start / stop, device securing (SDDL with the seat
  user's SID + `DEVPKEY_Device_SessionId`); fixed local pad seat ids
  `5053454c-4f43-4100-8000-<seat>`. Physical pads: `win::pad_io::WinPadSource` (shared HID
  open, overlapped reads), `forward_physical` (raw report when kinds match, else parsed
  state). Feedback `LocalFeedbackRouter` (the manager's sink): same kind = output report
  unchanged (rumble, lightbar, LEDs, adaptive triggers), other kind = rumble + lightbar
  rebuilt; `seat_pads::reports` gained the DS4 / DualSense input parsers, the Bluetooth-to-USB report cut and
  the DualSense output builder. Hub stops first in `stop_with`. (b) **Drivers built** with
  `build-drivers.ps1` (VS 18 BuildTools + WDK 10.0.28000): seat-input-filter, seat-idd
  (both roles), seat-storage-filter and the Rust `split-pad` (cargo + stampinf + inf2cat,
  own `src/crt.rs`), ARM64 + x64, Release + Debug, zero warnings, infverif, ApiValidator,
  PE machine 0xAA64 / 0x8664; unsigned, never loaded. (c) **One IddCx driver**:
  `drivers/split-idd` merged into `drivers/seat-idd` (`src/remote/`,
  `public/PulsarIddShared.h`); `src/roles.cpp` has DriverEntry and picks the role per
  devnode from the hardware id (`Root\PulsarSeatIdd` local, `PulsarSplit_IndirectDisplay`
  remote, Windows 11 model sections only; unknown id refused); both control interfaces
  and both Rust clients unchanged; `DisplayBackendId::driver` = `drivers/seat-idd` for
  both. (d) **Frame reader SID end to end**: local path unchanged
  (`SeatDisplayPrefs.frame_reader_sid`, round 3); remote path new: `SeatDisplay::
  frame_reader_sid` -> `SET_SEAT_CONFIG` v2 (256 bytes, additive by size, `encode_v2`,
  SID charset checked on both sides) -> remote role frame objects DACL SYSTEM + reader;
  the fixed LOCAL SERVICE grant is gone. The service passes `None` (SYSTEM only), since
  the stream server runs as SYSTEM. Gates green: cargo 1527 passed / 6 ignored (+12),
  clippy aarch64 + x86_64 clean, manager 184 / check / build, shim 8 (host) + x64 / i686
  check, installer scripts 49. No test deleted (seat-display `kinds::ids_per_kind` now
  expects one driver for both kinds).
  - Open (HYPOTHESIS, nothing ran on hardware): the shared HID open from session 0 does
    not disturb the seat's readers; USB output report writes reach DS4 / DualSense; the
    securing pass on ViGEm / usbip-win2 devices; which pad games pick (physical pad still
    visible to every session without the unsigned HID filter); both IddCx roles loading
    from one package; IddCx 1.10 detection and FP16 frames; the reader SID DACL.
  - Open (work): Bluetooth feedback (output report CRC); Xbox / generic HID pads are not
    read (`Unsupported`); hub notes are only logged, the manager does not show them yet;
    remote role still needs a Remote Desktop protocol provider that names its hardware id.
  - Note: the kiosk-isolation partial work stayed parked in `git stash` during this
    package and is restored after the commit (still uncommitted).
- 2026-11-14 (round 4 remote-gaps): (a) **Pulsar pairing events**: already carried since
  the merge review (ddc1935): `map_legacy_event` maps `otp_changed`, `trust_changed`,
  `relay_auth_required` to `stream` events, the `stream` topic is admin only
  (`events::Scope`), and the pair dialog re-reads OTP / pairing state on them as Split's
  did (Split's host page never reacted to events, so nothing to restore there). Added a
  service test (admin sees the three on `stream`; a standard user is refused / sees none)
  and fixed the stale "no longer refresh" note in apps/manager/README.md. (b) **Microphone
  for remote seats** (spec/remote 70 §5.1), Pulsar seats only: `pulsar-host` mic pipe
  `\.\pipe\PulsarSplit\seat\<id>\pulsar-host-mic` (service-owned, created before the
  launch, same client check as the control pipe; additive contract §3.8 in
  `docs/split/agents/pulsar-host-backend.md`: capability `mic`, `config.apply` key `mic`
  sent only when true, binary start / pcm / stop frames, 48 kHz s16le; codec
  `seat_core::remote::mic`) -> `seat_stream::backend::pulsar::mic::MicLink` (reader thread,
  `seat_runtime.stream.mic` = off / unsupported / waiting / connected / failed / closed) ->
  `SessionManager::send_mic` -> the hold's stdin after the password frame (bounded 50-frame
  queue, drops when full) -> `seat_rdp::headless::mic::HoldMic` -> MS-RDPEAI `AUDIO_INPUT`
  over DRDYNVC with `INFO_AUDIOCAPTURE` (48 kHz only, mono / stereo converted; active loop
  on a non-blocking socket). Sunshine seats never set `INFO_AUDIOCAPTURE` and list the new
  limitation `mic_needs_pulsar`; `seat_audio::MICROPHONE_PATH_AVAILABLE` is gone (functions
  take `mic_path`). Manager: mic switch copy updated, live mic state line on the remote seat
  page. (c) **Service start / stop / restart** restored in the manager header
  (`ServiceControl.svelte`, Tauri `service_state` / `service_control`, `src-tauri/src/scm.rs`
  on windows-sys, `PulsarSeat` from `seat_core::consts`): the SCM checks rights, so the
  buttons work in the elevated manager and are disabled with the reason otherwise; stop and
  restart ask first. Gates green: cargo 1544 passed / 6 ignored (+17), clippy aarch64 +
  x86_64 clean, manager 192 / check / build (+8), shim 8 (host) + x64 / i686 check,
  installer scripts 49. No test deleted.
  - **Request for pulsar-desktop (not edited from here):** implement contract §3.8 and §6
    item 20 of `docs/split/agents/pulsar-host-backend.md` in the `--split-seat` host:
    announce capability `mic`; when `config.apply` has `mic: true`, connect as a client to
    `\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host-mic` (same server check as the control
    pipe), decode the Pulsar client's mic to 48 kHz s16le PCM and write `start {channels}`,
    `pcm` (10-20 ms) and `stop` frames, never blocking the stream (drop on a full pipe).
    The Pulsar client (pulsar-core) must send its microphone inside the existing session
    (no new port). Until then every Pulsar seat reports `mic` = `unsupported`.
  - Open (HYPOTHESIS, nothing ran on hardware): Windows creates the seat session's capture
    endpoint when the hold sets `INFO_AUDIOCAPTURE`; the server picks one of the offered
    48 kHz formats; apps in the seat see the microphone; the non-blocking hold loop over
    SChannel behaves like the seat host engine's; voice latency is acceptable; the SCM
    calls, the token elevation check and the 60 s state wait of the service buttons.
  - Open (work): pulsar-host side (above); no silence is sent while no client mic is open
    (apps may wait for data); mic for Moonlight clients is impossible (protocol has none).
- 2026-11-14 (round 4 local-gaps): **per-seat host accounts, kiosk filter session map,
  printer redirection, redirect UI, second-listener accounts.** Nothing ran on hardware.
  (a) Each seat host runs as its own local account `PulsarSeatHostNN` (LSA secret
  `L$PulsarSeat.HostAccount.NN`), all in the local group `PulsarSeatHosts` that the pipe
  admits with one ACE; created / rotated the first time a seat's host starts in a run,
  pruned at service start for seats without a host, all removed at uninstall
  (`host_account::{ensure_group, ensure_account, ensure_seat, prune, remove_all}`,
  `win::containment` provisioner + `HostRequest`). Per-seat fallback to the shared
  `PulsarSeatHost` account (setup / logon failure, exit 44 / `STATUS_DLL_INIT_FAILED`),
  then SYSTEM Low; `HostTokenCompat=3` = shared account for every host (round-3
  behaviour), 1 / 2 unchanged. Profile loaded for the host's lifetime when the seat
  redirects printers. IDD frame sections per seat account. Migration: shared account
  kept (listener, fallback, own pipe ACE), added to the group. (b) Kiosk shell under
  `kmdf_filter`: forwarding (`Scope::kiosk_seat`) already existed; the real gap was the
  filter's seat -> session map, which mapped kiosk seats to the console session and let
  console programs open kiosk-seat HID collections; `service::filter_session_map` now
  leaves out seats on the console session / a shared session. (c) Manager "Yönlendirme"
  dialog: printers (kapalı / koltuğun yazıcıları / bütün yazıcılar, warning for all),
  summary, Turkish notes and refusals. (d) Printer redirection: `SeatRedirect.printers`
  (`off` / `seat` / `all`, seat-core additive), `HostRedirection.printers` /
  `all_printers`, `seat_rdp::policy::host_redirection_with` (queue lookup
  `seat_media::printer_queues_for_device`), IronRDP 0.9's Windows printer back-end
  (MS-RDPEPC announce via the rdpdr product, `with_redirected_printer`), `SeatRdpdr`
  refuses IRPs for unannounced printer ids, ActiveX refuses (`printers_need_ironrdp`),
  `printer_acl` gate gives the seat's own host account the use right
  (`GatePlan.printer_hosts`). (e) seat-core `loopback_no_accounts` /
  `logon_screen_needs_listener` warnings per logon-screen seat (manager mirrors them and
  lists the accounts the listener admits). Gates green: cargo 1563 passed / 6 ignored,
  clippy aarch64 + x86_64 clean, manager 198 / check / build, shim tests (host) + x64 /
  i686 check, installer scripts 49. No test deleted. The kiosk-isolation WIP was parked in
  `git stash` during this package and restored afterwards (still uncommitted, still
  breaks the shim workspace build).
  - Open (HYPOTHESIS, untested on hardware): batch token of a seat account carries the
    `PulsarSeatHosts` SID so the pipe ACE admits it; `LoadUserProfileW` /
    `UnloadUserProfile` with the hardened Low token from the service; `DeleteProfileW` of
    a pruned account; printing from a Low-integrity batch token through the spooler;
    the seat-owned queue lookup by USB port; the server installs the redirected queue
    with the announced driver name (the queue's own driver must exist on the PC: it does,
    the session runs on the same PC); the kiosk filter map keeps console opens of
    kiosk-seat touch / consumer collections out while the service reader still opens them.
  - Open (work / limits): a seat added while the service runs gets its account at its
    first host start, but seats removed while it runs keep their account until the next
    service start; a config that fails to load at start prunes every seat account (they
    are recreated, profiles lost); under `kmdf_filter` HID game pads of kiosk seats are
    usable by nobody (not service-read; kiosk-isolation shim would be the way); a seat
    whose own account fails after its display was configured keeps the seat SID on its
    IDD frame sections until its next start; printer redirection changes apply at the
    next connection; `all` with the `printer_acl` gate cannot print to other seats'
    gated queues; spec 70 §3.8 (printers never redirected) needs the analyst to record
    the maintainer's opt-in.

- 2026-11-14 (round 4 egfx-perf): **EGFX zero-copy GPU path, decoder on the presenter's
  adapter, presenter back-pressure with held frame acknowledgements.** Nothing ran on
  hardware or against a server. (a) seat-core `FrameSink` gained optional
  `gpu_adapter` / `gpu_ready` / `present_gpu(GpuFrameRef)` / `backlog` (defaults: CPU
  only, never behind); `GpuFrameRef` = shared NT handle of a BGRA keyed-mutex texture
  (producer releases key 1, presenter copies its `src` and releases key 0). (b) seat-rdp
  `gpu/`: `GfxChannel` wraps IronRDP's `GraphicsPipelineClient` (registered in its place,
  own ZGFX decompress, PDUs split by byte range and passed on per frame); `route` (pure)
  promotes a surface mapped 1:1 over outputs on the GPU compositor's adapter at an AVC420
  IDR (decoded on both paths until the session seeds the GPU surface from its image
  between frames), copies other codecs' paints into the texture at `EndFrame` minus later
  GPU paints, and demotes on AVC444, progressive, scaled / window mapping, copies or
  cache stores reading AVC areas, GPU errors, layout changes (protocol reset when CPU
  pixels are stale); `ack` (pure) holds `FrameAcknowledge` while the presenter is behind
  (≤ 3 frames / 100 ms, queue depth = held bytes, never suspend); `d3d` = Media
  Foundation MFT per surface in D3D11 mode on the presenter's adapter (by LUID, refused
  elsewhere), video processor NV12 -> BGRA (full-range BT.709) per region into a surface
  texture, ring of three shared keyed-mutex textures (`publish` skips when none is free).
  The readback decoder also runs on the presenter's adapter (`MfH264Decoder::on_adapter`).
  Session: composites the channel's CPU output itself (`composite_graphics_pipeline` for
  resets, `composite_rgba32` for updates), splitter skips GPU outputs (`set_skip`), CPU
  presents wait while `backlog > 0` (≤ 50 ms), held acks are sent after presents and on
  idle. Metrics: `gpu`, `gpu_surfaces`, `gpu_frames`, `gpu_skipped`, `promotions`,
  `demotions`, `present_skipped`, `acks_held`, `acks_forced`; override
  `PULSAR_SEAT_GFX_ZERO_COPY=0`; troubleshooting.md section 8 (Turkish). (c) seat-display
  window presenter: GPU frames on flip-chain outputs that are not clone sources
  (`FlipChain::take_shared`, cursor drawn on the back buffer through a small staging
  copy), `backlog` = outputs whose last frame waits for vblank (cursor moves excluded);
  `HybridPresenter` forwards only for window-only outputs; seat-host `SharedSink`
  forwards with the RDP-to-seat output map (blank: GPU frames dropped). Gates green: cargo
  1591 passed / 6 ignored (+26 seat-rdp gpu, +1 splitter, +1 seat-host), clippy aarch64 +
  x86_64 clean, manager 198 / check / build, shim tests (host) 8 + x64 / i686 check,
  installer scripts 49. No test deleted. The kiosk-isolation WIP was parked in `git stash`
  during this package and restored afterwards (still uncommitted).
  - Open (HYPOTHESIS, untested): hardware MFT NV12 texture-array slices as video
    processor input views; the output target rectangle bounds each blit; cross-device
    keyed-mutex sharing on one adapter on x64 GPUs and Adreno (ARM64); colour of the
    video processor path vs the CPU path; Windows servers in AVC420 mode send an IDR when
    a surface starts and keep AVC / other-codec regions apart enough that surfaces stay
    on the GPU; servers throttle on held acknowledgements and read `queueDepth`; the
    flip chain's frame latency waitable keeps `backlog` moving when the monitor sleeps.
  - Open (limits): AVC444 surfaces stay on the CPU path; clone sources, Display Core
    targets and GDI outputs get CPU frames; one GPU compositor per connection (adapter of
    the seat's first output); a GPU error or presenter refusal turns the path off for the
    connection; the manager does not show the new metrics (log line only).
- 2026-11-14 (round 4 kiosk-isolation + review fixes, "fix: apply round 4 review"):
  **per-process isolation for local kiosk seats** (architecture.md §4.1.3 "Per-process
  isolation", "Hotkey guard"; shim/README.md "Kiosk seats"). (a) seat-shim-core `kiosk`:
  volatile kiosk seat map `HKLM\SOFTWARE\PulsarSplit\Runtime\KioskSeats\<NN>` (codec,
  diff, `RegKioskMap`, fake), env `PULSAR_SEAT_KIOSK`, role `kiosk` (`role_full`), origin
  `KioskDevice` (`classify_full`), access row for kiosk K, keyboard / mouse class paths
  while kiosk seats run (`rule_applies`), plan group `raw_input_data`
  (`hook_plan_full`). (b) Launcher: `--attach <pid>` (exit 0 / 3 / 4 / 5), `Arch`, **no
  ARM64 module** (decision: no ARM64 inline-hooking backend after an earlier attempt hit a
  safety block; native ARM64 targets start without it, `arm64_unsupported`). (c) Module:
  live snapshot carries the kiosk map, `rt/raw.rs` filters `GetRawInputData` and (64-bit
  only) `GetRawInputBuffer` (`filter::compact_raw_records`, tested), fault frames know
  `kiosk`. (d) Service publishes the map on the input tick (`kiosk_map_entries`,
  `Platform::publish_kiosk_map`). (e) Kiosk shell: sets the env, attaches the x64 launcher
  to each suspended app when our AppHelp database is registered and the shim is installed
  (restart without the module on exit 5), and runs the `WH_KEYBOARD_LL` hotkey guard
  (`seat_input::hotkey_guard`) on its own message-loop thread when `seat1_desktop` is off
  (Turkish limits copy updated). Installer: nothing new (the x64 MSI already ships
  `shim\x64` / `shim\x86`; the ARM64 MSI ships no shim, so kiosk isolation is off on ARM64).
  - Open (HYPOTHESIS, nothing ran on hardware): `--attach` into a suspended app, module
    load in kiosk apps, the kiosk map read path, raw-input data / buffer filtering,
    keyboard / mouse class enumeration filtering, the hotkey guard swallowing these keys
    without stalling input, the x64 module in x64 apps under emulation on ARM64 (and
    shipping a shim in the ARM64 MSI for that).
  - Open (limits): x86 / x64 processes only; 32-bit apps keep `GetRawInputBuffer`
    unfiltered (WOW64 layout, HYPOTHESIS); seat-1 console apps still get raw-input data of kiosk
    keyboards; the shell's UI thread blocks while the launcher attaches; the env variable
    is not a security boundary (one Windows user for every kiosk seat).
  - **Review fixes (round 4 review):** installer/README.md service row now says the MSI
    sets no recovery actions and `install --msi` (`scm::configure_existing`) sets
    5 / 30 / 60 s then none, reset 1 day, privileges and DACL; ci-local's debug shim and
    API helper gates build into the shared target dir (release builds keep their own
    sub-folders because Tauri's MSI bundler picks up DLLs next to the manager exe;
    reason in installer/README.md); spec deviation and contract status recorded below.
  - **50602cd (fix(installer): service recovery actions in the setup step):** the MSI's
    `ServiceConfigFailureActions` was removed after MSI error 1939 in the ARM64 test VM
    (Windows Installer opens the service without `SERVICE_START`, so restart actions fail
    with error 5). MSI-mode `install` now calls `scm::configure_existing`
    (`OpenService(ALL_ACCESS)`, failure actions, required privileges, service DACL); any
    failure there fails the install custom action and rolls the MSI back (before, MSI mode
    only registered the event source and created the data folders). HYPOTHESIS: the
    deferred no-impersonate custom action (LocalSystem) can open the service with
    `ALL_ACCESS` and apply all of it. Open: rerun the fixed MSI on the VM (install, repair,
    major upgrade) and check `sc qfailure PulsarSeat` / `sc qprivs PulsarSeat`.
  - **Spec deviation waiting for confirmation:** printer redirection
    (`SeatRedirect.printers`, round 4 local-gaps) is product behaviour that spec/local/70
    §3.8 says the reference product does not have. It stays default `off`. Open: the
    analyst records the maintainer's opt-in in spec/local/70 §3.8, or the maintainer
    confirms it; until then it is a deviation, not a decision.
  - **Contract status:** `pulsar-host-backend.md` §3.8 (microphone: capability `mic`,
    `config.apply` key `mic`, second pipe `...\pulsar-host-mic`) and §6 item 20 are a
    **proposed** additive extension (protocol stays 1, control pipe unchanged) waiting for
    pulsar-desktop acceptance. Until then every Pulsar seat reports `mic` = `unsupported`.
  - ci-msi package: `ci-local.ps1` fixes (gate clock variable, quoted `'--'`, `-Command`
    for array parameters, shim job) finished with this commit; a stray repo-root `Debug\`
    (credprov DLL of an earlier run) was removed.
  - Gates green: cargo 1602 passed / 6 ignored (+11), clippy aarch64 + x86_64 clean,
    manager 198 / check / build, shim tests (host) 10 (+2) + x64 tests + clippy + i686
    check, installer scripts 49, `ci-local.ps1` (default jobs) green. No test deleted.
  - MSIs rebuilt from the clean tree at 087cd09 (`build-installer.ps1 -Arch arm64` /
    `-Arch x64`, unsigned, test only, never installed; build records `dirty: false`;
    payload + PE machine gate clean outside and inside the MSI):
    - `PulsarSeat_0.1.0_arm64.msi` SHA-256
      `7e4655744088ef3548101cae669817ae36573cf525016696bb4834922a1ed772` (51 613 696 bytes),
      no shim.
    - `PulsarSeat_0.1.0_x64.msi` SHA-256
      `c23177ccc9e9ced7674232c44d2ed634a1079b44ff031cf2c83622edf2522118` (65 505 555 bytes),
      `shim\x64` + `shim\x86`.
- 2026-11-15 **VM tests (U1 + U3, ARM64 Hyper-V test VM, Windows 11 Pro 26300, not hardware).**
  Harness: `tools/vm/` (control pipe client `seatctl.ps1` speaking protocol 3, `vm.ps1`
  PowerShell Direct helpers, `ui.ps1` keyboard + screenshots, `scenarios.ps1` U1 / U3
  steps). Last MSI tested: 9debd69 (ARM64, unsigned).
  - **U1 (fast_user_switch) - PASS with fixes.** Config over the pipe (seat 2 = local user
    seat1, seat 3 = seat2, passwords via `set_seat_password`), enable -> credential provider
    registered (provider, filter, two CLSID keys, DLL path ok) and loaded by LogonUI. Lock
    console + `start_seats [2]` -> console session disconnected, fresh session, one-time
    logon armed / handed / accepted within ~0.6 s; quser `seat1 console 3`, `pstest Disc 2`.
    Lock screen of seat 2 shows "Koltuk 2" + links "Koltuk 1'e geç" / "Koltuk 3'e geç";
    the seat 1 link hands the console to a fresh logon screen; signing a user in there
    reconnects that user's existing session (Windows FUS).
  - **U3 (desktops kiosk) - BLOCKED in the VM (one monitor), partial.** Config, display
    enumeration (after the fix), start / stop of kiosk seats 2 / 3 (`waiting_for_display`
    -> `off`), `desktops` probe notes incl. `kiosk_audio_per_app`, no console windows.
    Not verifiable here: kiosk shell windows, apps, hotkey guard, per-seat audio (seat 1
    must keep the only monitor: `seat_one_no_display`; monitors are exclusive; the VM has
    no audio endpoint).
  - **Fixed (commits):** b5344ee installer shipped a stale MSI with a fresh build record
    (relative `-CargoTargetDir`; the 2026-11-14 VM manager test used that older MSI);
    082ee84 seat 1 took seat N's fresh logon session during a FUS hand-over (credential
    provider answered for seat 1: no tile, no auto-logon); a80ba4b / e6f07c7 agent and
    seat host opened conhost windows on user desktops (now GUI subsystem); 22215fd display
    helper got `--display-helper` twice ("unknown helper command": no monitor ever
    enumerated, kiosk seats waited forever, topology apply failed); c768d05 switch links
    unreachable on Windows 11 lock screens (now in the seat tile); **9d636c5 SECURITY:**
    `WTSConnectSessionW` from SYSTEM without a password shows a locked session unlocked
    (also reproduced with SYSTEM `tscon`), so the switch links / agent switch let anyone at
    a seat's lock screen into another seat's session (seat 1 admin included) without a
    password; the backend now never connects without the stored password and hands the
    console to the logon screen instead; 9debd69 switch tile of an account-less seat tied
    to the session user; 7ac6c1b log text.
  - **Open:** (1) seat 1's lock screen still shows no switch links (tile not listed for
    pstest; HYPOTHESIS: Windows 11 does not list a provider tile without a password /
    submit field in the unlock scenario, or LogonUI gives no user array there; needs DLL
    tracing). (2) Done since (see the follow-up below): seat 1's kiosk shell with
    `seat1_desktop = false`.
    (3) `start_seats` on a running FUS seat answers `ack` and does nothing (no pipe way to
    switch the console to a running seat; links / agent only). (4) After a switch away,
    seat 2's status shows `at_logon` although its user is signed in. (5) Contained console
    listener exits 0xc0000142 under the PulsarSeatHost account, the service falls back to
    SYSTEM Low (HYPOTHESIS: desktop grant for the restricted logon SID). (6) After a
    password-less switch the fresh logon screen preselects the last user, not the target
    seat's user (UX). (7) Credential tiles have no image (blank sign-in option icon).
    (8) Agent tray switch not exercised (synthetic mouse cannot click the tray).
  - **Follow-up 2026-11-15: seat 1 kiosk shell (maintainer decision, option a; plan.md).**
    bcdbf19: the service runs seat 1's kiosk shell itself while enabled with `desktops` and
    `seat1_desktop = false` (start on enable / boot / tick, crash restart with back-off and
    the crash limit, stop on disable, config change and shutdown; 3 tests with the mock
    `desktops` backend). VM (MSI 519c00f, clean tree, `Set-U3Seat1Config`): **PASS** for
    start, shell bar ("Uygulamalar", app button, "Oturumu kapat"), app launch (charmap,
    cmd), keystrokes of the seat keyboard reaching the focused kiosk app, stop / cleanup on
    disable (shell and apps gone, "kiosk shell stopped"). The first VM run (MSI bcdbf19)
    showed the restart and give-up rules working on a real crash: 43760b0 fixed it (the
    shell crashed in USER32 in WM_PAINT: `DrawTextW` got an empty `Vec<u16>`, whose pointer
    0x2 it reads, whenever the bar had no notice line, i.e. always with `seat1_desktop =
    false`; found from a WER LocalDumps dump of a debug-info build swapped into the VM).
    **Hotkey guard: FAIL in the VM** - the Windows key from the Hyper-V synthetic keyboard
    still opens Start (Win, Win+R, Win+E); Ctrl+Esc did not toggle Start (inconclusive).
    HYPOTHESIS: the `WH_KEYBOARD_LL` hook is removed or not called for this input; needs
    instrumentation (the shell logs nothing since it is a GUI binary). Also seen: `cmd.exe`
    opens in Windows Terminal (default terminal), which lives outside the shell's job, so
    it gets no bar button and no placement (apps that hand off to another process are a
    known limit). Per-seat audio not testable (no audio endpoint in the VM).
  - **Fix round 2026-11-15 (maintainer: fix the open issues, verify in the VM; MSI c2409b9).**
    Diagnostics first: babc3a7 kiosk shell log `logs\kiosk-shell-NN.log` (start facts,
    integrity, hotkey guard counters; only guarded / modifier keys by code), 55bc4a7
    `logs\credprov.log` (scenario, answer flags, tiles, SID, selection; no secrets),
    fa586e3 seat host / console listener stdout + stderr to their log files.
    - **Seat 1 lock screen links - FIXED, VM PASS (1cfb5e3).** credprov.log: the lock screen
      runs the logon scenario and our user-less switch tile was built and tied to the SID
      but never shown. An account-less seat now gets its session user's seat tile, which
      carries the links ("Koltuk 2'ye geç", "Koltuk 3'e geç" on pstest's lock screen).
    - **Switch gaps - FIXED, VM PASS (1cfb5e3, c2409b9).** `switch_seat{seat}` (protocol 3,
      admin; seatctl `switch`); a password-less switch preselects the target's tile on the
      logon screen it opens (seen for seat 1 via the link and seat 2 via the pipe; signing
      pstest in on that tile reconnected session 2); a seat off the console shows
      `signed_in`; tiles have a run-time image (seat number, seen under Sign-in options).
    - **Hotkey guard - PARTLY FIXED, VM FAIL.** d65314d: the kiosk router posted the seat's
      VK_LWIN to the focused app (DefWindowProc opens Start on a lone Windows key-up,
      HYPOTHESIS from the documented SC_TASKLIST behaviour); it now drops every guarded key.
      Start still opens on some presses: the shell's `WH_KEYBOARD_LL` hook was **not called**
      for those events (log: hook events 0 while the raw reader counted the key), later
      presses were caught and blocked. A standalone LL hook (no Pulsar code) blocks the
      Windows key on the same VM, also with a raw `NOLEGACY|INPUTSINK` registration;
      disabling the focus-sink loop did not help. HYPOTHESIS: the hook is skipped while
      certain windows (Windows Terminal, Start) are in the foreground. Documented
      alternative tested: per-user `NoWinKeys` policy needs an Explorer restart and then
      blocks Win+R but not the lone Windows key. **Maintainer decision:** NoWinKeys +
      Explorer restart for kiosk consoles, an Explorer-less kiosk console, or more hook
      diagnostics.
    - **Console listener 0xc0000142 - NOT FIXED, stopped (needs more rights).** VM
      experiments (temporary switches, reverted): USER32 init of a process under the host
      account needs `WINSTA_EXITWINDOWS` (0x40) on WinSta0 (0x4, 0x8, 0x200 alone fail; 0x40
      passes); Medium integrity or granting the logon SID on its own process object did not
      help. That right lets the process log the console user off, which the containment
      design withholds. With it, the listener then fails to open the control pipe
      (`ERROR_PIPE_BUSY`, HYPOTHESIS: pipe instance / Low-integrity client admission). The
      SYSTEM-Low fallback keeps working. **Maintainer decision** on granting
      WINSTA_EXITWINDOWS before the pipe part is investigated.
  - **Round 3 2026-11-15 (maintainer decisions in plan.md; MSI 620213e, clean tree).**
    - **Console listener under its own account - FIXED, VM PASS** (ffc499d, 46e8962).
      Three causes, one after the other: (1) USER32 init needs `WINSTA_EXITWINDOWS`
      (granted on the listener's own journaled ACE only, accepted risk: it could log the
      console user off; architecture.md); (2) `ERROR_PIPE_BUSY` at start (the listener
      starts together with the kiosk shell; host clients now `WaitNamedPipeW` and retry);
      (3) the pipe server check asked the SCM, which refuses a Low-integrity batch token
      (`OpenSCManagerW` access denied); the service now passes its PID in
      `PULSAR_SEAT_SERVICE_PID`. VM: listener runs as `PSEAT-VM\PulsarSeatHost`, integrity
      0x1000 (Low), "console listener reporting", no SYSTEM fallback. **Not verified:**
      wizard keys reaching the service - `wizard_start` never answers in this kiosk
      configuration, also with the SYSTEM-Low listener (`HostTokenCompat=2`), so it is an
      older, separate problem (open); `activity` events stay empty in both modes too.
    - **Kiosk hotkey guard - VM FAIL, stopped as decided** (02012bf hardening only).
      Measured over 66 presses (22 Win, 22 Win+R, 22 Win+E): 130 key events blocked, the
      hook was called with 0 ms lag every time, except for the **first** Windows-key
      press after the shell started, which never reached the hook (seen only by the raw
      reader) and opened Start; later letters of Win+R / Win+E then typed into Start's
      search, but no Run dialog or Explorer opened. Ruled out: time-critical guard thread,
      no I/O in the hook, the raw-input `NOLEGACY|INPUTSINK` registration, the focus-sink
      loop (disabled), hook removal (later presses caught without re-install). The miss
      occurred in every run where Windows Terminal (hosting the kiosk's cmd) held the
      foreground and the shell's sink did not (`clipped false`); in a run where the sink
      held the foreground, no press was missed, and a standalone pass-through hook beside
      the shell saw every press. HYPOTHESIS: Windows does not call low-level hooks for
      the first key routed to a newly foreground Windows Terminal window. Misses are now
      detected (raw input) and logged with a hook re-install. Next option per the decision:
      an Explorer-less kiosk console, **not built without asking**.
  - **Round 4 2026-11-15 (maintainer decisions in plan.md; MSI 1f1c065, clean tree).**
    - **Kiosk console apps in conhost - DONE, VM PASS** (1f1c065). With `desktops`,
      `seat1_desktop = false` and a kiosk-shell seat, the service sets the console user's
      `HKU\<SID>\Console\%%Startup` `DelegationConsole` / `DelegationTerminal` to the conhost
      CLSID and restores the old values from `journal\kiosk-console.json` on disable,
      shutdown, config change and uninstall. VM: both values set for pstest; `cmd.exe` and
      charmap are children of the shell, `conhost.exe` a child of cmd, no Windows Terminal;
      cmd has its bar button and is placed below the bar. Limit: with `seat1_desktop = true`
      the console user is seat 1's desktop user and is left alone.
    - **wizard_start hang - FIXED, VM PASS** (2c1e9ef). Root cause: the display helper's
      detached identify overlay child inherited the helper's stdout, which is the service's
      reply pipe, so the console runner waited for EOF forever. The helper now clears handle
      inheritance on its std handles before spawning the overlay, and the runner waits at
      most 3 s for EOF after the reply (`helper_output_open`). VM: `wizard_start` answers
      `ack`; F1 and 1 counted as key presses, the keyboard was assigned to seat 1 and the
      wizard moved to the mouse step; `wizard_stop` acked.
    - **Kiosk hotkey guard - still one miss (corrected by the warm-up round below).** cmd
      in conhost in the foreground: 130 blocked, **1 missed** (the first Windows-key press
      after the shell started). The "shell in the foreground, 0 missed" run of this round
      is **invalid**: Start was still open from the checkpoint / the run before, so the
      presses went to Start. The same-millisecond timestamps of key-down and key-up do not
      mean a late hook call: hook detail lines carry the time the UI tick wrote them.
  - **Warm-up round 2026-11-15 (maintainer: try a hook warm-up first; MSI 9f488be, clean
    tree).** 9f488be: after installing / re-installing the hook and when a window first
    takes the foreground, the shell injects `VK_F24` down + up tagged `WARMUP_TAG` in
    `dwExtraInfo`; the hook swallows exactly that key (`hotkey_guard::is_warmup`, unit
    test) and the shell logs `warm-up: sent / seen / failed, hook latency`.
    **VM: FAIL, stopped as instructed.** With Start closed before each part (checked by
    screenshot): cmd (conhost) in the foreground: 130 blocked, **1 missed**, Start opened;
    shell in the foreground (apps not autostarted, shell restarted): 130 blocked, **1
    missed**. No Run dialog or Explorer window in either. Log evidence (same in both
    shells): warm-up 1 right after the install was seen (latency 24-30 us); ~100 ms later
    the focus sink took the foreground ("seat 1 holds the focus sink"), warm-up 2 sent for
    that foreground change was **never seen** (`sent 2 seen 1`); the Esc presses and the
    first Windows key after it were not seen either (`hook events 0 ... missed 1, seat
    packets 4`); after the miss-triggered re-install (warm-up 3, seen in 14-26 us) every
    key reached the hook. So the hook is not slow on its first call: it stops being
    called shortly after the shell starts and stays deaf until it is re-installed.
    HYPOTHESIS: something around the focus sink taking the foreground at start (or the
    shell's start-up, e.g. the raw-input registration) makes Windows drop the hook
    silently. A cheap follow-up (not built, needs a go): re-install the hook once after
    the start-up settles, and/or on a timer, and check with the warm-up counters.
    Earlier "0 missed" runs with Start already open are not evidence either way.
  - **Hook-deaf round 2026-11-15 (maintainer: root cause + self-healing probe; guard-debug
    track). Result: cause narrowed, not fixed; all code reverted (2f7487f).**
    - **Bisect (temporary debug host, switches in a flags file, never committed; diff kept
      outside the repo).** With the probe logging every miss: baseline, `noattach` (no
      `AttachThreadInput`) and `nolock` (no `LockSetForegroundWindow` / `ClipCursor`) all
      fail the same way: from the moment the focus sink takes the foreground (+~255 ms
      after start, `SetForegroundWindow(sink)` ok) the hook sees **no** probe key, and
      re-installing it every 150 ms does not help (`probe sent 22 seen 2 re-installs 20`).
      `noraw` (no raw reader) and `nofocus` (no focus sink) have no miss. So the hook is not
      "dropped once": `WH_KEYBOARD_LL` hooks of the process whose window holds the
      foreground are not called while that process has its raw-input registration.
      HYPOTHESIS: a `RIDEV_NOLEGACY` / `RIDEV_INPUTSINK` registration of the foreground
      process makes Windows skip the legacy path including LL hooks. The guard runs "worked"
      only after the first Windows key opened Start: the sink then lost the foreground for
      good (`clipped false`).
    - **Tried: keyboard without `RIDEV_NOLEGACY`** (mice kept it; shell windows swallowed
      key messages so Alt+F4 could not close them, verified: the shell survived Alt+F4).
      39e7f69, MSI 39e7f69 clean tree. **VM: FAIL** - cmd in front: 130 blocked, 1 missed,
      194 probe re-installs; the leader shell's hook still saw almost nothing.
    - **Self-healing probe** (440b7e5: warm-up every 2 s and on foreground changes,
      re-install after 100 ms, `hotkey_guard::Probe` with 3 tests). Because a re-install
      cannot revive the hook in this state, it re-installed ~7 times a second forever and
      the warm-up latency reached 168 ms (1.9 s in a second shell): a risk for every key
      of the PC. **Reverted** together with 39e7f69 (2f7487f); the code is back to 9f488be.
    - **Found: two seat-1 kiosk shells after disable + enable** (same second, both
      `--kiosk-shell --seat 1`, one `leader false`). Separate service bug in the seat-1
      kiosk lifecycle (bcdbf19), open. The non-leader shell's hook worked, which fits the
      cause above.
    - **Next (needs a go):** bisect the registration itself: no `RIDEV_NOLEGACY` for mice
      either, no `RIDEV_INPUTSINK`, without the HID usages; or move the raw reader into a
      separate process so the hook's process never owns the foreground registration; or
      hold the foreground with a window of another process.
  - **Round 2026-11-15: seat-1 shell double start + raw flag bisect (maintainer order).**
    - **Two seat-1 kiosk shells after disable + enable - FIXED, VM PASS** (d203e07). Race:
      the enable thread and the tick both ran `sync_seat1_kiosk`; the backend start takes a
      while and `running` was recorded only afterwards, so both saw no shell and both
      started one; the desktops backend's own "already running?" check had the same gap.
      Now `sync_seat1_kiosk` is serialised (`Service::seat1_kiosk_op`) and the desktops
      backend holds one lock (`seat_op`) across "running? launch, record" and across a
      stop (no start while a stop is pending). Tests with a delayed mock start / fake
      launcher (the service test fails without the lock). VM (MSI d203e07, clean tree):
      exactly one seat-1 shell after setup, after each of 3 disable / enable cycles, after
      a service restart and 10 s later.
    - **Raw-input flag bisect - FAIL, stopped as instructed** (temporary debug host with a
      flags file, never committed; detection-only probe every 500 ms, no re-install).
      Every case keeps the hook deaf while the focus sink holds the foreground: the first
      Win press reached Start (`hook events 0 ... missed 1`), warm-ups unseen until the
      sink lost the foreground (`sent 64 seen 34` in each run). Cases (keyboard / mouse
      `dwFlags`): baseline 0x2130 / 0x2130; mice without NOLEGACY 0x2130 / 0x2100; no
      INPUTSINK 0x2030 / 0x2030; keyboard without NOLEGACY 0x2100 / 0x2130; no HID usages;
      keyboard with no flags (0, focus-following target) / 0x2130; neither NOLEGACY
      0x2100 / 0x2100; only DEVNOTIFY, no HID 0x2000 / 0x2000. Earlier: no raw reader at
      all passes. So the flags do not matter: an LL hook in the same process as the raw
      reader's registration is not called while that process's window is in the
      foreground (HYPOTHESIS, VM only). **Maintainer decision:** raw reader in a separate
      process, or the foreground held by another process's window.
  - **Round 2026-11-15: hotkey guard in a helper process (maintainer decision, plan.md) -
    DONE, VM PASS** (fae86f1, MSI fae86f1 clean tree). The shell starts `pulsar-seat-host
    --hotkey-guard --seat N` (no raw input; same user / session / desktop / integrity),
    watches it, restarts it with back-off and stops it; one-byte commands over the helper's
    stdin pipe (re-install per detected miss, warm-up, quit), no key data; protocol,
    arguments and supervisor unit-tested (4 tests). VM, Start closed before each part
    (screenshots): helper `PSEAT-VM\pstest`, session 2, integrity 0x2000, child of the
    shell; **cmd in front: 132 of 132 guarded key events blocked, 0 missed**; **shell in
    front: 132 blocked, 0 missed**; both with the focus sink in the foreground
    (`clipped true`), no Run dialog, no Explorer window, Start never opened; warm-up latency
    4-41 us, 0 re-installs. Helper killed -> "exited", started again 1 s later (restarts 1);
    disable -> "stopped (quit)", no helper left. The Explorer-less kiosk console is not
    needed for the guard. Untested on hardware.
  - **Driver round in the test VM 2026-11-15/16 (user's go relayed by the coordinator; VM
    only: Secure Boot off, testsigning on, test certificate trusted; Memory Integrity /
    HVCI / VBS untouched - VBS was not running in this VM before or after, so HVCI could not
    be exercised here).** Packages test-signed with `tools/vm/testsign.ps1` (TEST ONLY
    certificate under the user profile; `check-payload` rule `test_signed`), installed with
    `pnputil`; the MSI ships no driver. Checkpoint `testsigning-clean` (testsigning + cert in
    Root and TrustedPublisher). Last MSI tested: ba917a9 (ARM64, clean tree).
    - **Input filter: PASS.** `oem2.inf`, `PulsarSeatInput` running; stacks `kbdclass >
      PulsarSeatInput > hyperkbd` (TEST ONLY model line for the VMBus keyboard in the staged
      INF only, 2997c66) and `mouclass > PulsarSeatInput > mouhid`. Service: driver
      `loaded` 1.1 (keyboard / mouse and HID roles). Routing: keyboard assigned to seat 2
      -> keys never reached seat 1's Notepad; after disable they did.
    - **Storage minifilter: loaded, gating NOT testable here.** TEST ONLY altitude 409990
      (staged INF only), `fltmc` instances on every volume including a hot-added VHD;
      service: `loaded` 1.0. The inventory lists USB / SD / removable-bus disks only (spec
      70 3.4) and Hyper-V passes no USB storage, so no disk could be assigned to a seat.
    - **IddCx: device starts after a fix, monitors OPEN.** First start failed
      (CM_PROB_FAILED_START, 0xC000009E): `IddCxAdapterInitAsync` refused
      `MaxMonitorsSupported` 64 (17..64 refused, 16 accepted) - fixed e340421 (adapter
      reports 16, connector indexes shared). `ADD_MONITOR`: `IddCxMonitorCreate` refused
      `INDIRECT_VIRTUAL` - fixed efa0090 (`INDIRECT_WIRED`); `IddCxMonitorArrival` still
      returns STATUS_INVALID_PARAMETER with every variant tried (EDID / none, random
      container id, HDMI type, WIRED_OTHER transmission, ORIGIN_DRIVER modes, a display
      pipeline rate); the mode callbacks run and succeed, IddCx and DxgKrnl traces name no
      reason. Service: driver `loaded` 1.2. Tool: example `idd_monitor` (6ea5c61, run as
      SYSTEM). Also by design `pulsar_idd` mirrors a seat's physical monitors, so a
      one-monitor VM cannot give seats 2 / 3 their own display (U3 stays blocked here).
    - **split-pad:** in the driver store only (devnodes are made on demand by seat-pads).
    - **Found and fixed (ba917a9):** after a reboot the service starts before the console
      auto-logon, `desktops` reports needs_setup (console_sign_in), the service falls back
      to `fast_user_switch` and kept it for good: a later enable never activated
      `desktops` / `kmdf_filter`. Backends are now re-selected after a fallback on enable
      and on a console logon. VM after a real reboot: "backends re-selected after a
      fallback ... -> []", isolation held.
    - **Driver Verifier (standard flags) on seatinput.sys + pulsarseatstoragegate.sys:**
      reboot, routing test, 500 file writes / reads on the VHD, minifilter unload / reload:
      no violation, no bugcheck, no dump; Verifier reset afterwards. seat-idd (UMDF) was
      not under the kernel Verifier.
    - **Uninstall:** `msiexec /x` removes service, pipe, PulsarSeatHost account and the
      install folder (ProgramData logs / config stay). Drivers: `fltmc unload`,
      `pnputil /remove-device` (IDD root devnode), `pnputil /delete-driver ... /uninstall`
      for all four: store empty, stacks back to `kbdclass > hyperkbd` / `mouclass > mouhid`,
      no filter, service keys DeleteFlag=1 (gone at the next boot). No CodeIntegrity block
      events during the round.
    - **Open:** IddCx monitor arrival (driver bug, next: a UMDF / IddCx debugger session or
      the WDK IddCx sample behaviour as documentation); HVCI not exercised (VBS off in the
      VM); storage gating needs a USB disk (hardware or another hypervisor).
  - **IddCx monitor arrival - FIXED, VM PASS (2026-11-16, 8350e44).** Root cause: the local
    role filled monitor modes and target modes with the same signal info,
    `vSyncFreqDivider` 1; monitor modes take 0 (`DISPLAYCONFIG_VIDEO_SIGNAL_INFO`; found by
    reading Microsoft's IndirectDisplay sample as documentation - MS-PL, nothing copied, not
    built). VM (`testsigning-clean`, driver test-signed): `ADD_MONITOR` ok, the monitor is
    display 2 in Settings > Display ("Extend these displays"), `REMOVE_MONITOR` ok; fill test:
    16 monitors ok, the 17th refused with STATUS_INSUFFICIENT_RESOURCES (e340421 limit). The
    product with `desktops` + `pulsar_idd` runs `display=pulsar_idd` without fallback,
    driver `loaded` 1.2; its monitor list shows only the physical monitor while ours is
    plugged (own IddCx monitors are never seat targets, by design). The console screen went
    dark after the first plug and came back on a key press (display asleep, not a hang).
    Still not tested: frames through the seat host presenter (needs a seat with a physical
    target, i.e. a second monitor), the remote role.
  - **Remote IDD role + HVCI in the test VM (2026-11-16, plan.md; user's go relayed).**
    - **Remote role: NOT TESTABLE here (environment / design).** A root-enumerated
      `PulsarSplit_IndirectDisplay` devnode starts the role: `IddCxAdapterInitAsync`
      succeeds (IddCx version 0x1B01, the 1.10 functions present), but IddCx never calls
      `EvtIddCxAdapterInitFinished` for the `REMOTE_SESSION_DRIVER` adapter and fails the
      device about 9 s later (`CM_PROB_FAILED_POST_START`); IddCx traces show only the
      init call. The role needs its device from a Remote Desktop protocol stack in the
      seat session (README "Remote role"); that provider does not exist yet. No monitor,
      mode list, HDR or frame channel could be reached. Tool for later: example
      `idd_remote` (a9c1a05).
    - **HVCI: PASS.** This Hyper-V ARM64 guest can run VBS without nested virtualization
      (DeviceGuard AvailableSecurityProperties 1,3,5,7; the VM's
      `ExposeVirtualizationExtensions` stays False, `VirtualizationBasedSecurityOptOut`
      False, vTPM on). Memory Integrity turned on with the toggle's documented key
      (`Scenarios\HypervisorEnforcedCodeIntegrity` `Enabled` = 1, no Secure Boot
      requirement set; Secure Boot stays off for test-signing). After two reboots:
      Kernel-Boot 153 "VBS Enabled, VSM Required, Hvci", Secure Kernel started with
      "HvciEnabled, HvciStrictMode", `SecurityServicesRunning` 2, testsigning on.
      Checkpoint `testsigning-hvci` (no drivers). With HVCI running all four test-signed
      drivers install and load: `PulsarSeatInput` in the keyboard / mouse stacks,
      minifilter attached, IddCx device OK and a virtual monitor added / removed, split-pad
      in the store; CodeIntegrity shows no block / warning (no 3033 / 3087 / 3089, only
      3116 / 3099 / 3085 information). No driver change needed.
  - **Overnight batch 2026-11-16 (VM only; coordinator relay).**
    - **MSI lifecycle on clean-logged-on: PASS.** Install (MSI ba917a9) + U1 config (seat
      passwords) + repair `/fa`: service, config revision 2, 3 seats and the LSA secrets
      kept. Major upgrade to a scratch 0.1.1 (versions bumped in Cargo.toml / package.json /
      tauri.conf.json / both Cargo.lock only for that build, never committed): one product
      (0.1.1), config and secrets kept. Uninstall `REMOVEUSERDATA=0`: ProgramData and
      `HKLM\SOFTWARE\PulsarSplit` kept, a reinstall picks the config up; seat passwords and
      seat-host accounts are deleted on every uninstall (by design, now in
      installer/README.md). Uninstall `REMOVEUSERDATA=1`: nothing left (service, host
      accounts, ProgramData, journal, both HKLM keys, credential provider, LSA secrets,
      firewall rules, groups, tasks).
    - **Remote seat on client Windows 11: PASS.** The config applies with the warning
      `session_unavailable`; `start_seats` is refused with `session_unavailable` ("remote
      seats need concurrent sessions"); seat stays off; the Terminal Server registry is
      byte-identical (export hash), no RDP listener, firewall rule or session. Manager
      (elevated) lists the seat under "Uzak koltuklar" as "Kapalı"; the reason text exists
      in i18n but shows only on save / start (UX note).
    - **U1 regression: one bug found and fixed (3e1003d).** Seat 2's lock screen links and
      the password-less `switch_seat 1` preselect pass. `switch_seat 3` right after left
      the console at a logon screen: the old password-less switch record (seat 1) still
      answered LogonUI of the new console session, so the hand-over to seat 3 was never
      "pending" and its one-time logon (armed 16 ms later) was never taken. A new hand-over
      now supersedes an older switch record; regression test with a mutation check.
      **VM re-run 2026-11-16 (MSI 2530d7c, SHA-256 1a4b5b5a...4465, PE 0xAA64): PASS.**
      Seat 2's lock screen links ok; `switch_seat 1` (no password) preselects pstest; right
      after, `switch_seat 3`: the new logon screen answered `pending`, then the seat tile
      with auto-logon, accepted - seat 3's user holds the console. Other direction (console
      locked): `switch_seat 3` then at once `switch_seat 2`: the logon screen preselects
      seat 2's tile ("Koltuk 2", no stored password by design for `switch_seat` to a running
      seat), not seat 3's. (Without the lock, `switch_seat` is refused with
      `console_in_use`, as designed.)
    - **Driver Verifier under HVCI: PASS.** From `testsigning-hvci` (VBS 2 / HVCI 2):
      input filter + minifilter under standard flags, reboot, kmdf_filter isolation
      (keys never reached seat 1), 400-file I/O, minifilter unload / reload: no violation,
      bugcheck, dump or CodeIntegrity warning; Verifier reset.
    - **Gates:** `installer/scripts/ci-local.ps1` all green after 2530d7c (the API helper
      test assumed no Pulsar Seat service on the build machine; this PC runs one).
      Repairs of stray control characters left by earlier escaping slips (installer
      README, ci-local.ps1, a comment in remote/win/wiring.rs).
    - **MSIs:** ARM64 rebuilt from the clean tree 2530d7c: SHA-256
      `1a4b5b5a69d66a5387f7fdcca175ecb7660dc847ad5c95e692ada534d8de4465`. **x64 NOT
      rebuilt:** the build failed with "no space on device" (C: 0.0 GB free of 950 GB); the
      x64 MSI in target\installer\dist is still the old 087cd09 one.
    - **Morning items:** (1) C: is full: `Projects\pulsar\wt` holds ~296 GB (not touched);
      deleting this repo's regenerable caches (target\dbg, target\exp, target\wf\debug,
      ~13 GB) was denied by the permission system and skipped. (2) The test VM is
      **PausedCritical** (Hyper-V paused it on the full disk) and could not be restored to
      clean-logged-on. (3) x64 MSI rebuild and a VM re-run of U1 with 3e1003d once space is
      free.
  - 2026-11-16 (morning, done): disk freed: 21 worktrees of another session removed (the
    user allowed removing unneeded ones; the run went on after the user stopped it). The
    branches were kept; the owning session confirmed nothing was lost. VM restored to clean-logged-on and Off.
    x64 MSI rebuilt from the clean tree faf13c7 (includes a06d140): SHA-256
    `401ea47039a9dccf25ea1e9ffe6f2c84312277afbfaa6efadd1ce831cc6a1726`, payload clean
    outside and inside; stale 0.1.1 line dropped from SHA256SUMS.txt. U1 re-run: PASS
    (faf13c7). Manager: remote seat rows and seat page say "Başlatılamaz: eşzamanlı oturum
    yok" up front when the chosen or running session backend has no concurrent sessions
    (a06d140).
- 2026-11-17 (parity-arm64): **ARM64 gaps closed in code.** (a) **Native ARM64 shim
  module**: hook backend split into `shim/module/src/rt/backend.rs` (`retour` on x86 / x64
  unchanged; Microsoft Detours on `aarch64-pc-windows-msvc`, vendored subset of commit
  `1dbd4ea2` in `shim/third_party/detours` (MIT), SHA-256 checked and built by
  `shim/module/build.rs`); `threads.rs` reads the ARM64 `Pc`; launcher / plan know
  `shim\arm64\` (`Arch::Arm64` launcher, `native_launcher`, `system32_module`; ARM64 targets
  are delegated, no more `arm64_unsupported`); the service and the kiosk shell call the
  launcher of their own architecture; the x64 launcher on an ARM64 PC adds no AppHelp layer
  (the System32 copy there is ARM64); shimctl `install --arm64` + PE machine check. ARM64
  MSI: WiX `shim\arm64\` (module, launcher, shimctl) + `shim\x64\` (emulated apps, no
  shimctl) + `shim\x86\`; `build-installer.ps1` builds all three; `check-payload.ts`
  machine rules per package; CI job `shim-arm64` (windows-11-arm). (b) **Credential
  provider ARM64X**: `BuildAsX` in the ARM64 configuration with an ARM64EC half;
  `build-credprov.ps1` checks the CHPE metadata (`Test-Hybrid`) and runs the self-test as
  ARM64 + ARM64EC; `-NoArm64X` for toolchains without ARM64EC; `check-payload.ts`
  `not_arm64x` (error with `--release`). (c) **VDD on ARM64 stays opt-in** (decision with
  facts in architecture.md §8; test `vdd_default_is_x64_only`). Manager: two Turkish shim
  strings no longer say ARM64 is unsupported.
  - Verified on the ARM64 dev box: real Detours hook of `ntdll!NtCreateFile` in the test
    process (native) and retour under x64 emulation; launcher hand-offs arm64 / x64 / x86 from
    a scratch layout (module loaded, init ran, hooks off in the console); credprov Release +
    Debug ARM64X (`dumpbin`: "(ARM64X)"), self-tests ARM64 / ARM64EC / x64 "0 failed";
    `check-payload --arch arm64` on the real binaries: only the expected `unsigned_system_dll`
    with `--release`; `PulsarSeat.wxs` compiles with candle 3.14 for arm64 and x64 (only the
    old CNDL1150 warning).
  - HYPOTHESIS (not run): every hook inside a seat on ARM64; x64 / ARM64EC / x86 apps under
    emulation with their modules; AppHelp with the ARM64 System32 copy (and an x64 process
    skipping it); an emulated x64 CredUI host loading the ARM64EC half; the VS 18 ARM64EC
    component ids in `ci.yml`; the `shim-arm64` CI job on windows-11-arm. No MSI was built
    in this round.
  - Gates: cargo 1634 passed / 6 ignored (was 1629), clippy aarch64 + x86_64 clean, shim
    11 tests (was 10) + clippy aarch64 / x86_64 + check i686, manager 199 / check / build,
    installer scripts 54. No test deleted.
- 2026-11-17 (integrate arm64): merged `worktree-wf_b025704c-d7f-1` (a301559, 1dcaace) into
  main with a merge commit; no conflicts (the branch was based on main's head dbeff2e).
  - A stale pre-merge `credprov\build\ARM64\Release` DLL (plain ARM64) made the
    `check-payload` test "the real ARM64X build passes when present" fail; a fresh
    `build-credprov.ps1` run (ARM64 ARM64X + x64, self-tests 0 failed) fixed it. Rebuild
    credprov after pulling this change.
  - Gates on main: cargo 1634 passed (was 1629), clippy aarch64 + x86_64 clean, shim 11 tests
    on aarch64 and x86_64 + check x86_64 / i686 / aarch64 + clippy aarch64 / x86_64, manager
    199 / check / build, installer scripts 54. No test deleted. Open HYPOTHESIS items as in
    the parity-arm64 entry above.
- 2026-11-17 (parity net-wfp, done, not on hardware): **source IP per seat with real
  enforcement.** New driver `drivers/seat-wfp` (`seatwfp.sys`, service `PulsarSeatWfp`):
  KMDF non-PnP WFP callout, primitive INF class `WFPCALLOUTS`, ALE bind-redirect V4
  (wildcard binds of a seat process get the seat address, port kept) and connect-redirect
  V4 (local address of outbound TCP / UDP, loopback and other callouts' redirects left
  alone); own sublayer, callouts and filters in a dynamic engine session, BFE state
  subscription; session id -> address map with rule (all / only listed / all except
  listed) and NT-path program lists over `\\.\PulsarSeatWfp` (exclusive, DACL = the
  `PulsarSeat` service SID only); fail open before the first map and after the service
  handle closes; Safe Mode / `Parameters\PassThrough` = no filters. Built ARM64 + x64,
  Release + Debug, zero warnings, infverif /w, ApiValidator, `-Analyze` clean; unsigned,
  never loaded. Rust side: `seat_compat::wfp` (wire format pinned against the header,
  service SID derived from the name in a test, DOS -> NT paths, `Publisher`, `FakeWfp`),
  `seat_compat::drivers` reads `DefaultInstall.NT<arch>` (primitive packages pass the
  arch check; a new primitive install asks for a reboot), `DriverId::SourceIpCallout`,
  `Service::publish_source_ip` in every ownership run (shared sessions left out),
  `get_backends.source_ip`, new issue codes `loopback_ip_address` (error),
  `link_local_ip_address`, `ip_binding_shared_session` (warnings), class E invalid.
  Manager: network tab says driver / fallback / why, seat lists, dropped paths (Turkish),
  `localIssues` mirror. `PULSAR_SEAT_SOURCE_IP` fallback unchanged. build-drivers.ps1,
  run-sdv.ps1, testsign.ps1, ci.yml SDV step, release checklist list the driver.
  Gates: cargo test 1644 (baseline 1629), clippy aarch64 + x86_64 clean, manager 200
  (baseline 199) + check + build, installer scripts 51, shim workspace unchanged.
  **HYPOTHESIS / open (VM test plan in drivers/seat-wfp/README.md):** classify at
  PASSIVE_LEVEL on both layers; the connect-redirect local-address rewrite honoured for
  TCP and UDP; WFP app ids of programs on network shares (`\device\mup\…`); dual-stack
  sockets classified at the V4 layers; the SID-string SDDL accepted by
  `WdfControlDeviceInitAllocate`; `pnputil /install` not starting the primitive service.
- 2026-11-17 (integrate net-wfp): cherry-picked `worktree-wf_b025704c-d7f-5` (639c02c) onto
  main as 5599a16. Only conflicts: the status.md and journal.md entries of the arm64 and
  net-wfp rounds (both kept; the net-wfp status entry is now a top-level bullet). No code
  conflicts; the MSI driver fragment is generic per staged package, so seat-wfp needs no WiX
  change. All crate sources were touched before the gate runs (shared target dir hazard).
  - Gates on main: cargo 1646 passed / 6 ignored (1629 + 2 arm64 workspace tests + 15
    net-wfp; the arm64 entry's "1634" also counted shim tests), clippy aarch64 + x86_64
    clean, shim 11 tests + check x86_64 / i686 / aarch64, manager 200 / check / build,
    installer scripts 54, `build-drivers.ps1 -Projects seat-wfp` ARM64 + x64 Release + Debug
    clean (PE 0xAA64 / 0x8664). No test deleted. Open HYPOTHESIS items as in the net-wfp
    entry above (VM test plan in drivers/seat-wfp/README.md).
- 2026-11-17 (parity round, package kiosk, branch worktree-wf_b025704c-d7f-2): U3 kiosk gaps closed
    in code, **all runtime behaviour HYPOTHESIS (nothing ran on hardware or in the VM)**.
    (a) per-seat Windows users for kiosk seats 2+ (`seat_session::kiosk_account`,
    seat-service `win::kiosk_logon`, `host_account::ensure_kiosk` / `prune_kiosk`,
    `SessionLauncher::launch_as`; migration: missing `settings.kiosk.accounts` = per_seat;
    fallback to the console user with probe note `kiosk_account_fallback`; focus mutex DACL
    for every interactive user; conhost delegation in Pulsar kiosk accounts' hives);
    (b) seat-1 console processes get the shim's raw-input data filter while kiosk seats run,
    and the hotkey guard attributes legacy keys / clicks per device with `seat1_desktop =
    true` (`--attribute`, `hotkey_guard::Attributor` / `MouseAttributor`); (c) kiosk map
    includes USB hubs / other USB devices (XInput pads), kiosk apps' raw-input registrations
    get `RIDEV_INPUTSINK`; (d) packaged apps by AUMID (`KioskShellApp.app_id`,
    `IApplicationActivationManager`), hand-off / break-away / frame-window tracking
    (`seat-host/src/kiosk/track.rs`), `KioskShellApp.fullscreen` (foreground + clip for the
    focus leader, restore after focus-loss minimise); (e) router drives top-level scroll bars
    (`WindowQuery::scroll_bar`, auto-repeat on the tick), seat cursor section +
    `GetCursorPos` / `GetMessagePos` hooks (`seat_shim_core::kiosk_cursor`, `rt/cursor.rs`)
    so popup menus open at the seat cursor; (f) Ctrl+Alt+Del / Win+L documented as not
    blockable in user mode (manager limits text). Manager: per-seat-user toggle, "Kullanıcı:"
    line, AUMID + full-screen fields. Gates green: cargo test 1650 (baseline 1629), clippy
    aarch64 + x86_64, manager 202 (199) / check / build, shim test + clippy x64 + check i686,
    bun test installer/scripts 51. No driver touched. Hardware tests needed: everything above.
- 2026-11-17 (integrate kiosk): merged branch worktree-wf_b025704c-d7f-2 (7d4bf0d) into main.
  Conflicts only in `shim/module/src/rt/mod.rs` (kept both `mod backend` from arm64 and
  `mod cursor` from kiosk; the cursor hooks go through `rt::hook::install`, so they use
  Detours on native ARM64 and retour on x86 / x64) and in the doc logs (architecture.md
  §4.1.3 limits now state the ARM64 module from parity-arm64 plus the seat-1 raw-input
  filter from kiosk; status / journal keep both sides). Gates on main: cargo test 1664
  passed / 6 ignored (1646 + 18 kiosk tests; the kiosk report's 1650 was measured against
  its own base), clippy -D warnings aarch64 + x86_64 clean, shim tests (1 + 8 + 3) host and
  x86_64, clippy x86_64 + aarch64, check i686, debug build aarch64 + x64, manager 203 / check
  / build, bun test installer/scripts 54. No driver touched; no test deleted. Kiosk runtime
  behaviour stays HYPOTHESIS (list in the kiosk entry above); new HYPOTHESIS: the seat
  cursor hooks under the ARM64 Detours backend.
- 2026-11-17 (parity round, package pads-pen; branch of this worktree; untested on
  hardware, every runtime path HYPOTHESIS):
  - **Local seats read every HID pad**: PlayStation and Switch Pro (`seat_pads::switch_pro`,
    init at open) by their public layouts, Xbox HID collections and generic HID
    joysticks / gamepads through their own report descriptor (`HidP_*` value and button
    caps, `seat_pads::generic` profiles). `Unsupported` only when a descriptor cannot be
    read.
  - **Bluetooth feedback**: DualSense 0x31 / DualShock 4 0x11 with CRC-32 (`seat_pads::bt`),
    Xbox Bluetooth 0x03 (rumble + impulse triggers), Switch Pro rumble 0x10 + player
    lights; output reports zero-padded to `OutputReportByteLength`.
  - **Physical pad scoping** (`seat_input::pad_scope`, `seat-service/src/pad_runtime.rs`,
    setting `input.pad_device_scope`, default on, manager "Fiziksel oyun kollarını koltuğa
    kilitle"): device SDDL (owner user) + session id on the HID collection and its parent,
    journaled before every change, restored at start / stop / setting off. Owner = the
    local seat a pad is routed to, else the console user while remote seats run (spec/remote
    60 console pad scoping without the shim). **XInput per seat**: local seats with their
    own session are in the shim seat map with their pads (and parents) as `PadInstances`.
  - **SDL hints**: `SeatConfig::pad_ignore_devices` (manager column "SDL’nin yok sayacağı
    kollar") plus the ids of pads routed elsewhere, as `SDL_GAMECONTROLLER_IGNORE_DEVICES`
    / `SDL_JOYSTICK_IGNORE_DEVICES` for every process the service starts in a seat.
  - **Controller audio (remote)**: protocol 2 media section per pad (`seat_pads::media`;
    contract `docs/split/agents/pulsar-host-backend.md` §5), `pad.attach.media`, usbip
    DualSense mic fed from it, speaker / haptics PCM into it; remote DualSense audio
    interfaces only with `RemoteSettings::pad_audio` (default off). Sunshine stays
    HID-only (Moonlight has no controller audio). Needs a pulsar-desktop change to use it.
  - **Pen**: tilt X / Y, rotation, barrel, eraser, invert from HID (`PenInfo` on the wire)
    into MS-RDPEI pen contacts (hosted seats; hosted digitizers are routable now) and into
    kiosk seats as `PT_PEN` injection (`seat-host/src/kiosk/pen_win.rs`).
  - Gates (2026-11-17): cargo test --workspace 1663 passed (baseline 1629), clippy
    -D warnings aarch64 + x86_64 clean, manager 200 tests (199) / check / build, shim
    tests + x64 / x86 check, installer scripts 51, split-pad driver ARM64 + x64 clean
    (it includes `seat-pads/src/layout.rs`). No test deleted; one test changed on purpose
    (`hub_other_kind_parses_input_and_rebuilds_feedback`: Bluetooth now gets feedback).
  - Open: hardware tests of every item above; pulsar-desktop media support; Xbox USB
    rumble (XInput only) and generic HID force feedback (PID page) are not done.
- 2026-11-17 (integrate pads-pen): merged branch worktree-wf_b025704c-d7f-3 (7df1632) into
  main. Conflicts: `apps/manager/src/lib/components/BasicTab.svelte` (one import line: kept
  kiosk's per-seat account helpers and added pads-pen's `setPadIgnoreDevices` /
  `parsePadIgnoreList`), `crates/seat-host/src/kiosk/win.rs` `run_shell` (kept kiosk's
  attributing hotkey guard `GuardHelper::new(seat, hotkey_guard::mode(..))` and added
  pads-pen's synthetic pen device + `router.set_pen_injection`), and the doc logs (status /
  journal keep both sides). Gates on main: cargo test --workspace 1695 passed (1664 + 31
  pads-pen tests), clippy -D warnings aarch64 host + x86_64-pc-windows-msvc clean, shim tests
  (1 + 7 + 3) + check x86_64 / i686 / aarch64, manager 204 tests / check 0 errors / build,
  bun test installer/scripts 54, build-drivers.ps1 -Projects split-pad ARM64 + x64 Release +
  Debug clean (PE 0xAA64 / 0x8664). No test deleted. Learned: build-drivers.ps1 must be
  started with the repository as the current directory, else cargo misses
  `.cargo/config.toml` (crt-static) and wdk-build fails with `StaticCrtNotEnabled`. Runtime
  behaviour stays HYPOTHESIS (list in the pads-pen entry above); new HYPOTHESIS: kiosk pen
  injection together with the per-seat kiosk user and the attributing hotkey guard.
- 2026-11-17 (parity round, package display-gpu, branch worktree-wf_b025704c-d7f-4). Built,
  unit-tested, nothing ran on hardware (every runtime claim below is HYPOTHESIS):
  - **One IddCx adapter per render GPU** (seat-idd interface minor 3): adapters bind to a
    render LUID, `GET_ADAPTER_INFO`, `STATUS_DEVICE_BUSY` for another GPU; the service
    creates `SWD\PulsarSeatIdd\gpuN` software devnodes (`PulsarSeatIdd_Adapter`) and
    routes each virtual monitor to its GPU's adapter (`seat_display::idd_adapters`,
    `win::swdevice`, `win::idd::WinIdd`). The old "multi-GPU unsupported" warning is gone.
  - **HDR / VRR for seats 2..N:** per-monitor `hdr` / `vrr` options (manager "Görüntü
    çıkışları"); local IddCx role with IddCx 1.10 callbacks, FP16 / 10-bit frames +
    HDR10 metadata; Display Core HDR10 scan-out (FP16 scRGB primaries, HDR10 wire
    format) and tearing presents for VRR; flip chain VRR (tearing); SDR fallbacks
    tone-mapped. HDCP documented as not available (troubleshooting, READMEs).
  - **Per-seat GPU preference:** `local.gpu_preference` → seat user's
    `UserGpuPreferences` at logon, journaled (`Change::UserGpuPreference`), undone on
    disable / reset / uninstall; manager column "Ekran kartı tercihi".
  - **EGFX:** AVC444 / AVC444v2 composed on the GPU (compute + pixel shaders; model
    tested against IronRDP, HLSL tested on WARP), vendor async hardware H.264 MFT first
    with fallback ladder (dev box has none: fallback path ran), latency percentiles.
  - Gates: cargo test --workspace, clippy aarch64 + x86_64, manager test (201) / check /
    build, bun test installer/scripts, build-drivers seat-idd ARM64 + x64 clean.
    Shared target dir note: worktrees share artifact hashes in `target\wf` (cargo hashes
    path deps relative to the workspace), so this round built with
    `RUSTFLAGS=-C target-feature=+crt-static -C metadata=wfdisplaygpu` to keep its own
    artifacts.
  - Open (hardware): extra adapter devnodes start next to the root one; one render GPU
    per IddCx adapter honoured; FP16 frames / HDR10 scan-out on real panels; VRR through
    tearing presents on Display Core targets; NV12 slice copies for the AVC444 shaders
    on x64 GPUs / Adreno; vendor async MFT behaviour (x64 GPUs).
- 2026-11-17 (integrate display-gpu): merged branch worktree-wf_b025704c-d7f-4 (f74aa1a) into
  main with a merge commit. Conflicts, all resolved keeping both sides:
  `apps/manager/src/lib/components/BasicTab.svelte` (one import line: kiosk / pads-pen helpers
  plus display-gpu's `setGpuPreference`), `apps/manager/src/lib/model.test.ts` (pads-pen's pad
  ignore list suite and display-gpu's display / GPU options suite side by side),
  `crates/seat-core/src/validate.rs` (kiosk's `is_app_user_model_id` and display-gpu's
  `gpu_preference_checks`), `crates/seat-service/src/platform.rs` (mock platform keeps the
  net-wfp `wfp` / `wfp_publisher` fakes and adds the `gpu_ranks` / `gpu_prefs` /
  `gpu_pref_reverts` fields), `crates/seat-service/src/tests.rs` (net-wfp source-IP tests and
  the GPU preference logon test), and the doc logs (status / journal keep both sides; the
  display-gpu status entry is now a top-level bullet). All workspace `.rs` sources were
  touched before the gate runs (shared target dir hazard noted in the display-gpu entry).
  - Gates on main: cargo test --workspace 1725 passed / 6 ignored (1695 + 30 display-gpu
    tests; the package's 1662 was measured against its own base), clippy -D warnings aarch64
    host + x86_64-pc-windows-msvc clean, shim tests (1 + 8 + 3) + check x86_64 / i686 /
    aarch64, manager 206 tests / check 0 errors / build, bun test installer/scripts 54,
    build-drivers.ps1 -Projects seat-idd ARM64 + x64 Release + Debug clean (PE 0xAA64 /
    0x8664, output target\wf\drivers-display). No test deleted. Runtime behaviour stays
    HYPOTHESIS (list in the display-gpu entry above); new HYPOTHESIS: GPU preference writes at
    logon together with the per-seat kiosk users (kiosk accounts get `UserGpuPreferences` in
    their own hive).
- 2026-11-17 (parity round, package i18n, on main). Umbrella rule "English everywhere,
  Turkish only as an app i18n locale". Built and unit-tested; nothing ran on hardware
  (runtime claims below are HYPOTHESIS):
  - **Manager:** English default UI, Turkish locale with the same keys:
    `apps/manager/src/lib/locales/en.ts` (key set, type `Locale`) + `tr.ts`; `i18n.ts`
    keeps the helpers and fills the exported tables with `applyLocale`. Language = stored
    choice (Basic settings > Appearance > Language: Windows display language / English /
    Türkçe, `localStorage`) else the Windows user UI language (Tauri `ui_language`, then
    `navigator.languages`); a change reloads the window and tells the shell
    (`set_ui_language`). Strings moved out of components / helpers (describe, update,
    pictures, hotkey key names, demo data, kiosk / store seat prefixes, placeholders,
    avatar upper case, update dates). Tests in English plus locale key parity, no Turkish
    letters in `en`, Turkish switch tests.
  - **Rust:** `seat_core::i18n` (`Lang`, `Text { en, tr }`, `{n}` fill, `check_table`,
    `text_table!`, `Text::matches`; `PULSAR_SEAT_LANG` else `GetUserDefaultUILanguage`).
    Localized: seat agent tray + notices, kiosk shell bar, RDP connecting texts, presenter
    idle text, Event Log, service CLI, install / uninstall console, host / kiosk account full
    names and comments (recognised in either language), SCM / setup service descriptions and
    display name, firewall rule descriptions, power request reason, reboot messages, stream
    default app name, input fallback names, restore point name, tweak help, known-app
    advice, audio state copy, preflight titles (console), Tauri shell (`src-tauri/src/copy.rs`:
    tray menu, window titles, file dialogs). English only now: preflight / coexistence
    details, setup log (`BLOCKED` / `WARNING`, English titles), redacted log line
    (`[line hidden]`).
  - **Experimental warning:** `crates/seat-session/src/termwrap/warning_en.txt` next to
    `warning_tr.txt` (version unchanged: same substance); manager `WARNING_EN` / `WARNING_TR`
    checked word for word; the remote status sends `warning_text` (English) and
    `warning_text_tr`.
  - **Credential provider:** `credprov/src/copy.h` (tile labels, statuses, "Switch to seat N" /
    "Koltuk N'ye geç"); build-credprov.ps1 ARM64 (ARM64X) + ARM64EC self-test + x64 clean,
    self-test 0 failed.
  - **Installer:** `installer/wix/locales/{en-US,tr-TR}.wxl` with every fragment string
    (`!(loc.Id)`), `tauri-bundle.json` builds `en-US`, `build-installer.ps1 -Language tr-TR`
    (`bundle-config.ts --language`) a Turkish MSI; NSIS hooks LangStrings 1033 / 1055 and
    languages English + Turkish; `installer/scripts/wix-locales.test.ts`.
  - **Docs:** rules.md copy rule, AGENTS.md, CLEANROOM.md, architecture.md "UI language"
    section, READMEs; `docs/troubleshooting.md` in English (new section 11 Language), the
    Turkish copy is `docs/troubleshooting.tr.md`.
  - Gates: cargo test --workspace 1739 passed / 6 ignored, clippy -D warnings aarch64 host +
    x86_64-pc-windows-msvc clean, shim tests + check x86_64 / i686 / aarch64, manager 214 tests /
    check 0 errors / build, bun test installer/scripts 58. No test deleted (tests that
    asserted Turkish copy now assert English or switch to the Turkish locale; agent / termwrap
    tests renamed).
  - Open (HYPOTHESIS, hardware / builds not run here): WebView2 reports the Windows display
    language; the service (LocalSystem) and LogonUI pick the machine default UI language;
    tray relabel at runtime; Tauri merges its default WiX strings with our locale file and
    names the MSI per culture; NSIS LangStrings with numeric ids where the hooks are
    included.
- 2026-11-17 (parity review fixes, on main; commits "fix: apply parity review (part 1/2)").
  Built and unit-tested; nothing ran on hardware (runtime claims are HYPOTHESIS; the list
  is in architecture.md "Parity review fixes"):
  - **Confirmed findings fixed:** kiosk seat pads owned by the seat's own Windows user;
    pad scoping opt-in (`input.pad_device_scope` default off), failed nodes retried with
    back-off, one warning per error; `pad.attach.media` only with audio endpoints; hotkey
    guard raw-first key behind a held-back hotkey; shim cursor hooks DPI-convert for
    unaware threads and only override a successful real call; vendor async MFT flush
    restarts the stream, `Drop` shuts the MFT and its activation down; seat-wfp leaves
    explicitly bound sockets alone (endpoint table); every driver INF has English
    `[Strings]` + Turkish `[Strings.041f]`; unused kiosk accounts are disabled (profiles
    kept), deleted only at uninstall (profiles with `REMOVEUSERDATA`) or by the new
    `remove_kiosk_user` request; probe notes `kiosk_users_share_desktop` /
    `no_per_user_isolation` + manager warning for personal accounts on kiosk seats; x64
    targets kept out of the AppHelp database on ARM64 and the manager copy corrected;
    test fixes (kiosk masks vs windows-sys, devtree test name / doc, CAPTUREMOUSE case);
    `.claude/` ignored, merged worktrees 1-4 removed (5 kept: its commit is not an
    ancestor of main).
  - **Gaps implemented:** kiosk seats' key state, `SetCursorPos` / `ClipCursor` and a
    per-seat clipboard (sections `kiosk_cursor` v2, `kiosk_clip`); `reset_graphics`
    request (agent: Win+Ctrl+Shift+B) + manager seat menu; IddCx frame source for seats
    without a session display (kiosk shell `IddPresenter`, HDR frames to the presenter);
    Explorer joins the seat job and opt-in `cpu_seat_wide`; WNF focus runtime check;
    opt-in GameInputSvc focus filter; remote seats' SDL list from the PC's pads; Xbox USB
    (XUSB) and HID PID rumble, Switch Pro SPI stick calibration; smart-card readers per
    seat (RDP auto-redirect of the seat's own reader, WinSCard filter in patched
    processes, gate status `seat_filter`); cameras per seat in patched processes
    (`Shim\CameraRules`); Hyper-V helper (`hyper_v` request, Network tab); Pulsar
    stream-server options (`pulsar_*` gamepads, `pulsar_idd` capture, stock fallback);
    stale docs (manager README, plan.md).
  - Gates: cargo test --workspace 1773 passed, clippy -D warnings aarch64 host +
    x86_64-pc-windows-msvc clean, shim tests (1 + 8 + 3) + clippy x86_64 / i686 / aarch64,
    manager 217 tests / check 0 errors / build, bun test installer/scripts 63,
    build-drivers.ps1 all five drivers ARM64 + x64 Release + Debug clean (PE 0xAA64 /
    0x8664, output target\wf\drivers-review). No test deleted; changed on purpose:
    `failures_never_lose_the_original` (a repeated identical error is reported once),
    `hub_generic_and_xbox_bluetooth_pads` (generic pads now rumble through the fallback),
    `hub_switch_pro_init_input_and_rumble` (two SPI reads after init),
    `masks_are_the_interactive_users` (compares windows-sys constants, Windows only),
    `physical_node_reads_are_read_only` renamed `missing_node_reads_fail`, manager pad
    scope default test (off).
  - Open (not built in this round): the Pulsar stream-server patch set (spec/remote 80 §0:
    pulsar_idd capture, seat control pipe with the web UI off, pairing timeout, pulsar_pad
    pads) and its MSYS2 build: only the config side exists; sandbox / VM seats (spec/remote
    10 §8); per-seat smart-card readers and cameras on the logon screen and in processes
    without the module; WinRT camera enumeration; Ctrl+Alt+Del / Win+L from kiosk
    keyboards (Windows handles them before user mode). Hardware tests for everything
    above.
- 2026-11-17 (parity round, package vm-tests, on main). VM round on `PulsarSeatTest-arm64`
  only (maintainer approval relayed: Secure Boot off + test-signing, TEST ONLY certificate,
  our drivers loaded there, Driver Verifier, Memory Integrity). Checkpoint
  `testsigning-hvci` (VBS 2, HVCI running, testsigning on, Secure Boot off). ARM64 MSI built
  from main with `build-installer.ps1 -Arch arm64` (last one 6058fa64...1500, tree b2d4679),
  ARM64 drivers with `build-drivers.ps1` into `target\wf\drivers-review`, test-signed with
  `tools/vm/testsign.ps1 -Source` into `target\vm\testsigned\ARM64` only. Verifier:
  `verifier /standard /driver seatwfp.sys` (standard flags incl. IRQL, pool, DDI) + WDF
  verifier (`Parameters\Wdf\VerifierOn`) for `PulsarSeatWfp` and `PulsarSeatIdd`; no
  violation, bugcheck, dump or CodeIntegrity block (3033 / 3077 / 3087 / 3089) in the round.
  At the end: MSI uninstalled, driver packages deleted, Verifier reset, VM restored to
  `clean-logged-on` and Off.
  - **Bugs found and fixed on main (each with gates green):**
    - 027e66c `seat-host` alone did not build (`seat_pads` named in seat-input `hid.rs`
      without the `local-pads` feature): `build-installer.ps1` failed with E0433.
    - 0251da8 stray DLLs of the shared target dir (`split_pad.dll`, a `PulsarSplitShim.dll`)
      landed in the MSI root (check-payload `shim_outside_folder`): held aside while tauri
      bundles.
    - 955be0c seat-idd local role failed to start with FP16 caps (`CM_PROB_FAILED_START`,
      0xC000009E): `EvtIddCxMonitorSetGammaRamp` was missing.
    - c7ba2f7 seat-wfp: TCP clients of a seat could not reach `127.0.0.1`
      (`WSAEADDRNOTAVAIL`): TCP client sockets go to the connect layer now.
    - 534a690, 61ce466, 27c0277 fast user switching from a logon-screen console: the
      provider was told "seat 1, not pending" 26 ms before the grant (seat stayed at the
      logon screen); seat 1 bound itself to the seat's session; the seat's Logon was handled
      as seat 1's (agent refused, no startup apps); two pending seats swapped the console
      every 5 s for ever. Four regression tests, each failing without its fix.
    - b2d4679 kiosk: with charmap + cmd autostarted, keys went to charmap behind cmd (the
      z-order walk's last window got the focus); the app launched last gets it now.
  - **PASS / FAIL per item:**
    - seat-wfp under Verifier + HVCI: **PASS** (loads at boot, two WFP filters, service
      `loaded` 1.0 over the service-SID-only control device, `pnputil /install` does not
      start the primitive service, 4 unload / reload cycles clean).
    - Source-IP enforcement, two seats (fast user switching, seat 2 .231, seat 3 .232):
      **PASS** for listeners (both seats on port 5000 at once), UDP server, TCP SYN and UDP
      datagrams to a remote address (pktmon: seat addresses on the wire), explicit binds,
      TCP loopback (after c7ba2f7). **FAIL (documented limit):** UDP to `127/8` from a
      socket the program never bound (`WSAEADDRNOTAVAIL`; the connect layer's local address
      is not honoured for unconnected UDP sends; program-list exception).
    - Multi-adapter seat-idd: **PASS** (after 955be0c): 2 monitors add / remove on the root
      adapter; `idd_monitor multi` with two LUIDs: root adapter bound to LUID A, the client
      created `SWD\PulsarSeatIdd\gpu1` bound to LUID B (READY | BOUND | FP16), removes
      routed, devnode gone when the client closed; service reports `idd` `loaded` 1.3.
    - Remote IDD role: **NOT TESTABLE** (no provider on main creates the
      `PulsarSplit_IndirectDisplay` device; a root devnode still ends in
      `CM_PROB_FAILED_POST_START`, as on 2026-11-16).
    - ARM64 shim inject + seat map: **PASS.** With the opt-in AppHelp database (shipped
      ARM64 `shimctl install --arm64`), the kiosk shell attached `shim\arm64\PulsarSplitShim.dll`
      to native ARM64 charmap and cmd; seat keys reached the shimmed cmd (`echo ok`). Shim
      seat map: `Runtime\Seats\<session>` for the off-console FUS seat (seat id, user SID,
      `hid_isolation`), `Runtime\KioskSeats\01` for the kiosk seat (devices, flags 7). The
      seat on the console is left out by design. MSI uninstall removed the AppHelp
      database and the System32 / SysWOW64 modules.
    - Kiosk per-seat users: **NOT TESTABLE here** (a kiosk seat 2..N needs a monitor of its
      own; the VM has one, which seat 1 must keep: seat 2 stays `waiting_for_display`, so no
      `PulsarKiosk02` logon happens). Seat 1 kiosk shell, console delegation to conhost and
      its restore at uninstall: PASS.
    - Compat tweaks apply / revert: **PASS** (lock-screen image, driver-update exclusion,
      hide fast user switching under `desktops`; refused under `fast_user_switch` by
      validation as designed; per-user palm check `AAPThreshold` = 2 in the seat user's
      hive; reverted on disable, re-applied on enable, reverted at uninstall).
    - Deny-logon option (`same_account = deny`): **NOT TESTABLE on client Windows** (fast
      user switching reconnects the same account's existing session, so one account never
      has two sessions; unit test `same_account_deny_logs_the_second_seat_off` stands).
      No other deny-logon option exists on main.
    - Per-seat environment: **PASS** (startup apps of seat 2 / 3 got their own
      `PULSAR_SEAT_SOURCE_IP` and `SDL_GAMECONTROLLER_IGNORE_DEVICES` /
      `SDL_JOYSTICK_IGNORE_DEVICES`).
    - Credential test (`test_seat_credentials`): **PASS** (`ok` for both stored passwords,
      `1326` "user name or password is incorrect" for a wrong one, `missing_credentials`
      for seat 1).
    - MSI install / reinstall / uninstall: **PASS**; `REMOVEUSERDATA=1` left no service,
      install dir, ProgramData, HKLM keys, accounts, groups, tasks, firewall rules,
      credential provider / filter CLSIDs, tweak values or console delegation.
  - New tools: `tools/vm/devnode.ps1` (root devnodes over SetupAPI), `testsign.ps1 -Source`,
    `idd_monitor gpus` / `multi`; notes in tools/vm/README.md.
  - Gates (main, after b2d4679 + docs): cargo test --workspace 1777 passed / 6 ignored (was
    1773; +4 regression tests), clippy -D warnings aarch64 host + x86_64-pc-windows-msvc
    clean, shim tests (1 + 8 + 3) + check x86_64 / i686 / aarch64, manager 217 tests / check
    0 errors / build, bun test installer/scripts 63, build-drivers.ps1 seat-idd + seat-wfp
    ARM64 + x64 Release + Debug clean (PE 0xAA64 / 0x8664). No test deleted.
  - Open: x64 VM / hardware run of both drivers; UDP loopback limit; kiosk per-seat users
    and seats 2..N kiosk need a second monitor (hardware); remote IDD role needs a protocol
    provider; `same_account = deny` needs concurrent sessions (Server / RDS).
- 2026-11-17 (parity ship, on main). `installer/scripts/ci-local.ps1 -Payload -Drivers
  -Installers` end to end on the ARM64 dev box, then both MSIs rebuilt from the clean
  committed tree.
  - **Bug found and fixed:** 8097f7f the first run stopped at "drivers x64 (WDK code
    analysis)": `build-drivers.ps1 -OutRoot target/drivers-analyze` (relative) was resolved
    by msbuild against each project folder, so the drivers were built into
    `drivers/<name>/target/drivers-analyze/...`; seat-wfp had no package dir in the
    repo-level folder and the other four drivers were checked against stale packages of an
    earlier run. `-OutRoot` is now made absolute against the repository root; the stray
    `drivers/*/target` folders were removed.
  - **Second run: all 37 gates green** (workflow lint, bun install, cargo build / test /
    clippy aarch64 + x86_64, manager frontend build, manager Rust test + clippy, API helper
    test + clippy, manager check / test, bun test installer/scripts, shim x64 test + clippy
    + x86 check, shim arm64 test + clippy, credprov ARM64 + x64 build / self-test / PE,
    release binaries + API helper DLLs x86 / x64 / arm64 / arm64ec + payload / PE gate for
    arm64 and x64, drivers x64 + ARM64 build / infverif / ApiValidator / WDK code analysis /
    machine check, MSI arm64).
  - **Not run locally (need GitHub or real signing):** the `-Release` tag gate (release key
    list `check-signers --require`: no release key is listed yet), `PULSAR_SEAT_RELEASE=1`
    builds, `actions/upload-artifact` steps, the x64 runner's native test pass (here the x64
    shim tests ran under emulation; the root workspace tests ran native ARM64 only),
    Authenticode signing of the MSIs, `write-sums.ps1` after signing and
    `sign-release.ps1` (minisign of SHA256SUMS.txt). Scripts and checklist for these exist
    (docs/release/checklist.md).
  - **Gates (explicit, main 8097f7f):** cargo test --workspace 1777 passed / 6 ignored,
    clippy -D warnings host aarch64 + x86_64-pc-windows-msvc clean, manager 217 tests /
    check / build, bun test installer/scripts 63, shim tests + check x86_64 / i686 /
    aarch64, build-drivers.ps1 ARM64 + x64 Release + Debug clean (zero warnings, infverif,
    ApiValidator, code analysis). No test deleted.
  - **MSIs** (`build-installer.ps1 -Arch arm64` / `-Arch x64` from the repo root, unsigned,
    build records `commit: 8097f7f82dc0ccb1ba47c48d67ee3b9dcb91174c`, `dirty: false`,
    `release: false`; `target/installer/dist/SHA256SUMS.txt` lists exactly these two):
    - `PulsarSeat_0.1.0_arm64.msi`
      SHA-256 `db9c144c028ea28b77c177ebcda4034c51d8dd51196777bc96dde844b5d05db7`
    - `PulsarSeat_0.1.0_x64.msi`
      SHA-256 `73112c1ea33348cd8d6971324d12b91f2fd7561b494d408673d92eaebd98e3f1`
  - **PE machines inside the MSIs** (msiexec /a extraction + check-payload, no warning):
    ARM64 MSI: service / host / agent / manager / credprov 0xAA64 (credprov ARM64X: CHPE
    metadata present), apihelper arm64 0xAA64, arm64ec 0x8664 with CHPE metadata (ARM64EC),
    x64 0x8664, x86 0x14C; shim arm64 0xAA64, x64 0x8664, x86 0x14C; stream server and its
    tools 0xAA64; MttVDD 0xAA64. x64 MSI: every first-party binary 0x8664 except apihelper
    x86 and shim x86 (0x14C). The usbip-win2 setup exe is the upstream x86 (0x14C)
    bootstrapper in both MSIs (third party, pinned). Our own kernel drivers are not in either
    MSI (unsigned; `driver_not_allowed` until a signed package exists).
  - **Final parity summary (this round, 2026-11-17).**
    - Complete in code, built and unit-tested on both arches: ARM64 parity (native ARM64
      shim with Detours backend, ARM64EC helper, ARM64X credprov), per-seat source IP
      (seat-wfp callout), kiosk seats (per-seat kiosk users, raw-input filter, seat cursor,
      packaged / full-screen apps), every HID pad on local seats with Bluetooth feedback, pad
      scoping, SDL ignore lists, controller audio section, pen fields, one IddCx adapter per
      render GPU, HDR10 / VRR for seats 2..N, per-seat GPU preference, AVC444 on the GPU,
      English default UI with Turkish locale, the review fixes (kiosk state / clipboard,
      reset_graphics, remove_kiosk_user, hyper_v, frame source for kiosk seats, CPU
      coverage, GameInputSvc opt-in, reader / camera rules, rumble fallbacks).
    - Verified in the ARM64 VM (vm-tests entry above): seat-wfp under Verifier + HVCI,
      multi-adapter seat-idd, ARM64 shim inject + seat map, compat tweaks, per-seat
      environment, credential test, MSI install / reinstall / uninstall.
    - HYPOTHESIS pending hardware: every x64 run of the drivers; physical pads, Bluetooth
      feedback and controller audio; pen on real digitizers; HDR / VRR and multi-GPU on real
      GPUs; vendor H.264 MFTs; kiosk seats 2..N and per-seat kiosk users (need a second
      monitor); the remote IDD role (needs a protocol provider); `same_account = deny`
      (needs concurrent sessions, Server / RDS); everything listed as HYPOTHESIS in the
      package entries above.
    - Documented limits: UDP to 127/8 from an unbound socket under seat-wfp.
    - Still open in code (listed in the review-fixes entry): the Pulsar stream-server patch
      set and its MSYS2 build (config side only), sandbox / VM seats.
    - Out of scope by the round's rules (not built): concurrent sessions on client SKUs by
      changing TermService / LSM, kernel patching / BYOVD / DSE workarounds, signing with
      real certificates.
    - Refused by the model provider's safety system: none recorded in this round.
- 2026-11-17 (parity round, package compat-tweaks, branch worktree-wf_41b135b3-32a-2).
  Visible opt-in compatibility options, each off by default, journaled and undone on
  disable / uninstall (architecture.md "Compatibility options"):
  - Built: Windows Update for Business deferral / active hours / no automatic restart
    while signed in (`TweakId::WindowsUpdate`), fast startup off from the config,
    Device Installation Restrictions per setup class with a "no seat uses" suggestion
    (`TweakId::DeviceInstallDeny`, operator entries kept), Defender exclusions for our
    folders / processes (`seat_compat::defender`, WMI `MSFT_MpPreference`), the class-level
    default descriptors switch enabled in the manager plus Modem / multi-port serial /
    floppy / tape / medium changer classes, per-seat environment variables
    (`seat_compat::user_env`, agent `refresh_environment`, launch env, remote autostart env),
    "Deny log on locally" for RDP-signed-in seat accounts (`seat_compat::rights`,
    `LsaAddAccountRights`), Steam per local seat (seat map flag), per-seat named-object
    rewrite in the shim (`seat_shim_core::objects`, `rt/objects.rs`, `Shim\ObjectRules`),
    Steam legacy proxy-DLL report (pre-flight `steam_proxy_dll`) with journaled quarantine /
    restore (`steam_proxy_dll` request), DPI read-back (agent `report_dpi` /
    `AgentEvent::Dpi`, one-shot `--report-dpi` for remote seats with a static scale, notice
    `dpi_mismatch`), USB dock wizard in the manager (generic, spec 10 §8.8), manager README
    "not built" list fixed (the minimum-resolution gap was already closed).
  - HYPOTHESIS (nothing ran on hardware): every Windows effect above (policy values on Pro,
    Defender changes under Tamper Protection, deny right without reboot, Explorer reloading
    the environment, object hooks reached, Steam IPC override on local seats, DPI values,
    dock detection on real docks).
  - Documented limits: Steam library is per Steam install (a seat needing its own library
    uses its own install); object isolation only for shim-target programs (never
    anti-cheat games); domain Group Policy may overwrite the deny right.
  - Gates (worktree): cargo test --workspace 1805 passed / 6 ignored (was 1777), doc tests
    green, clippy -D warnings host aarch64 + x86_64-pc-windows-msvc clean, manager 229
    tests (was 217) / check 0 errors / build, shim tests (1 + 8 + 3) + check x86_64 / i686 /
    aarch64 + clippy aarch64, bun test installer/scripts 63. No driver touched. No test
    deleted; changed on purpose: `same_account_deny_logs_the_second_seat_off` counts the
    agent tasks without the new per-logon `report_dpi` task.
  - Open: hardware / VM run of every option; the shared target dir hazard showed again
    (another worktree's `seat-core` replaced ours mid-run; touching and re-running helped).
  - Refused by the model provider's safety system: none.
- 2026-11-17 (parity round 2, package hyperv-seats, worktree branch). Hyper-V VM seats
  (spec/remote 10 §8, 90 §2.4), the gap left open by the previous round. Contract:
  architecture.md "Hyper-V VM seats".
  - **Built:** config schema 3 (`remote.vm`, `settings.remote.vm`, migration 2 -> 3,
    guest addresses assigned at save), validation (mount point rule, forwards vs stream
    blocks, memory, GPU / save), `vm_seats` request, VM seat start / stop / poll / delete /
    autostart / suspend / service stop in `seat-service/src/vm_seats.rs` (worker never
    sees VM seats), PowerShell Hyper-V layer `seat_compat::hyperv_vm` (base disk from WIM /
    ESD / VHDX with payload + unattend + boot task, differencing disk per seat, Gen 2 VM
    with vTPM, GPU-P + driver store copy, NAT forwards, firewall rule, SMB shares, reset,
    remove, uninstall journal), Hyper-V sockets (`seat_compat::hvsock`) and the guest link
    hub, guest mode `pulsar-seat-service vm-guest` (network, user, shares, automatic sign-in
    via LSA secret, stream server in the VM console, PIN / unpair forwarding), manager
    "VM seats" page (en + tr), preflight check `hyperv_vm` (presence only), uninstall hook.
  - Tests: cargo +23 (vm module, hyperv_vm scripts / parsing / journal, hvsock GUIDs, guest
    hub, preflight, validation, schema migration, CLI, three service flows with the mock
    platform: start refusals in order, full run through VM + guest link + pairing + stop +
    disk reset + delete, autostart / suspend / GPU restart / failures), manager +7.
  - Gates (worktree branch): cargo test --workspace 1800 passed / 6 ignored, clippy
    -D warnings host aarch64 + x86_64-pc-windows-msvc clean, manager 224 tests / check 0
    errors / build, bun test installer/scripts 63, shim tests (1 + 8 + 3) + check x86_64 /
    i686 / aarch64. No driver touched. No test deleted or changed.
  - Rust layout note: `GlobalSettings.remote`, `RemoteSettings.vm` and `RemoteSeat.vm` /
    `RemoteSeatRecord.vm` are boxed (clippy `large_enum_variant` on `ServerFrame`); the JSON
    is unchanged.
  - Shared target dir hazard seen again: a full `cargo test --workspace` linked other
    worktrees' `seat_core` test binary / rlib (95 instead of 104 tests, doctest import
    errors); the counts above come from a coherent run (changed crates re-run right after a
    touch, per-binary counts compared).
  - **HYPOTHESIS (nothing ran on Hyper-V):** see the list at the end of the architecture
    section. Needs a Hyper-V capable PC (x64 and ARM64) with a Windows image.
  - Open: pairing requests other than PIN / unpair-all (client list, single unpair, apps,
    Pulsar backend OTP / passwords) answer `vm_seat` for VM seats; the guest has no
    pad / display backends of its own beyond what the stream server provides in the VM.
- 2026-11-17 (integration, hyperv-seats into main after compat-tweaks). Merged
  worktree-wf_41b135b3-32a-5 (eab8383). Conflicts were all additive on both sides and kept
  both: protocol requests / responses (`steam_proxy_dll` and `vm_seats`), manager api /
  types / demo / store tests, preflight report fields (`steam_proxy_dlls` and `hyperv`),
  `RemoteSeatRecord` (`environment` and `vm`), service request dispatch and tests, and
  `GlobalSettings` (`apps` kept, `remote` boxed as `Box<RemoteSettings>`).
  - Gates (main checkout, sources touched first): cargo test --workspace 1834 passed /
    6 ignored (test attributes 1813 + 23, consistent), clippy -D warnings host aarch64 +
    x86_64-pc-windows-msvc clean, manager 236 tests / check 0 errors / build, bun test
    installer/scripts 63, shim tests (1 + 8 + 3) + check x86_64 / i686 / aarch64. No driver
    touched, no test deleted.
  - Open: the HYPOTHESIS lists of both packages (architecture.md) are unchanged.

- 2026-11-17 (rdp-listener, worktree branch). RDP seat gaps closed in code; unit-tested,
  untested on hardware.
  - **NTFS / ReFS drive redirection** (`seat_rdp::channels::broker`): ACL'd volumes the
    seat owns alone are served **as the seat user** by the seat session's agent (drive
    broker), not by the Low-integrity host. The host asks `host_drive_broker`; the service
    makes two anonymous pipes (`Platform::broker_pipes`, one per direction, ends moved with
    `DuplicateHandle(DUPLICATE_CLOSE_SOURCE)`), gives the agent the allowlist (agent task
    `drive_broker {read, write, drives}`) and the host its ends. The broker refuses roots
    off the allowlist and moved letters; the host forwards only re-encoded drive IRPs and
    passes on only completions of its own broker device ids. `RedirectDrive.acl`, note
    `drive_served_as_seat_user`; `drive_acl_volume_refused` now only when a FAT volume
    turned ACL'd after resolve.
  - **Live drive and printer changes** (`SeatRdpdr::live_changes`,
    `channels::apply_live_changes`): unassigned drives / printers leave the open channel,
    newly assigned ones are announced (MS-RDPEFS 3.2.5.1.9). FAT drives get a back-end of
    their own, printers a back-end per discovery batch with device-id remapping, ACL'd
    drives join once the broker is linked. The channel and its drive / printer
    capabilities are joined whenever the policy is on (`HostRedirection.drives_on`,
    `printers_on`). Dynamic printers need a patched `ironrdp-rdpdr` 0.9.0, vendored in
    `third_party/ironrdp-rdpdr` (`PATCHES.md`, `[patch.crates-io]`, workspace `exclude`).
  - **ListReaderGroups** answered by the host: only groups holding one of the seat's
    readers (`seat_groups`, `SCardListReaderGroupsW`).
  - **Second listener:** no documented per-address bind exists (only `LanAdapter` /
    `SetNetworkAdapterLanaID`, a LANA per adapter), so the firewall design stays and is
    documented. New admin request `restart_term_service {confirm}`: stops every running
    seat but seat 1 (signed out), then `SessionBackend::restart_termservice`
    (`LoopbackListener::operator_restart`, journaled) and re-checks the listener; manager
    Advanced tab button with a confirmation dialog.
  - **Audio notice:** host request `host_audio_report` -> `SeatRuntime.audio_notes`
    (`render_absent`, ...); `degraded` says `audio_no_endpoint` / `audio_not_bound` /
    `audio_service_not_running` for an RDP-bridge seat instead of `audio_console_shared`;
    manager Status tab shows both with English / Turkish labels.
  - **Credential test:** interactive logon first, network logon only after 1385 (logon
    right denied) (`credential_test_order`).
  - Gates (worktree): cargo test --workspace 1797 passed / 6 ignored (was 1777; +20),
    clippy -D warnings host aarch64 + x86_64-pc-windows-msvc clean, manager 218 tests /
    check / build, bun test installer/scripts 63, shim tests (1 + 8 + 3) + check x86_64 /
    i686 / aarch64. No driver touched. No test deleted.
  - HYPOTHESIS pending hardware: a Low-integrity seat host can use pipe ends the service
    duplicated into it; the agent's token opens the seat user's files with the user's
    rights; Windows servers accept device announcements (drives and printers) on an open
    rdpdr channel after logon; the interactive logon of an account denied local logon
    returns 1385 after the password check; TermService restarts cleanly with the seats
    stopped; the loopback interface has no LANA.
  - Open: a policy switched on while connected and smart-card changes still need the next
    connection (static channels and the pre-logon smart-card device are fixed at connect).

- 2026-11-17 (integration, rdp-listener into main after compat-tweaks and hyperv-seats).
  Merged worktree-wf_41b135b3-32a-1 (8236d48). Conflicts were additive on both sides and
  kept both: protocol requests / responses (`steam_proxy_dll`, `vm_seats` and
  `restart_term_service`), agent tasks (`RefreshEnvironment` / `ReportDpi` and
  `DriveBroker`, with both helper functions in seat-host `agent.rs`), the service
  `Platform` trait and its mock (Hyper-V / guest link and `broker_pipes`), request
  dispatch and service tests, manager api / types / demo. Manager Advanced tab: imports of
  both `CompatSettings` and `ConfirmDialog`; the device-class target list stays the
  compat-tweaks one (disks and volumes never offered) next to the TermService restart
  dialog state.
  - Gates (main checkout, sources touched first): cargo test --workspace 1849 passed /
    6 ignored (1834 + 15 from rdp-listener; the worktree entry above says 1797 / +20, a
    stale seat-stream test binary from the shared target dir; its clean run was 1792 / +15),
    clippy -D warnings host aarch64 + x86_64-pc-windows-msvc clean, manager 237 tests /
    check 0 errors / build, bun test installer/scripts 63, shim tests (1 + 8 + 3) + check
    x86_64 / i686 / aarch64. No driver touched, no test deleted.
  - Open: the rdp-listener HYPOTHESIS list above is unchanged (nothing run on hardware).
