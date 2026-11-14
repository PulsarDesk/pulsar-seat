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
  - Verification (`CARGO_TARGET_DIR=targetgent-fix-rdp-client`): `cargo test -p seat-rdp`
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
      rdp-channels + fix-rdp-channels entries below). Still open there: zero-copy D3D11
      decode into the flip presenter (see the egfx entry).
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
    - EGFX: zero-copy D3D11 decode into the flip presenter (own GPU compositor for EGFX
      surfaces); decoder on the presenter's adapter; no FrameAcknowledge queue-depth
      back-pressure; vendor async hardware MFTs unused.
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
    - EGFX: zero-copy D3D11 decode, presenter adapter, FrameAcknowledge back-pressure,
      vendor async MFTs.
    - Credential provider: DLL unsigned; JSON parser string growth (HYPOTHESIS above).
    - Host containment: one shared account, no loaded profile, `--console` never uses it.
    - Second listener: seats without an account need `extra_accounts` (secure default).
    - Kiosk: system hotkeys / SAS reach every seat without the input filter; U3 app limits;
      no per-seat audio for kiosk apps.
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
