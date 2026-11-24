# E2E prep on the gaming PC (2026-11-24)

Machine access: `docs/local/README.md` (Test PC, LAN address). Evidence:
`target/e2e/gaming-pc-2026-11-24/`. The PC auto-logs on ahmet after a reboot.

## Goal

- A1: detect-only support for an operator-installed Duo TermWrap in the termsrv_wrapper
  backend (`crates/seat-session/src/termwrap`). Never write ServiceDll or wrapper rows for it.
- A2: identify "Pluralinput" on the gaming PC read-only; judge conflict with seat-input.
- A3: gates (clippy -D warnings, cargo test --workspace, CARGO_TARGET_DIR=target\wf-e2e).

## Done

- Read-only probes (evidence `probe-duo.txt`, `probe-pluralinput*.txt`,
  `probe-filters-services.txt`): ServiceDll = `%ProgramFiles%\Duo\TermWrap.dll`
  (REG_EXPAND_SZ), TermWrap.dll 1.6.0.0, Authenticode Valid (CN=Black-Seraph), PE 0x8664,
  file/dir ACL admin-only, termsrv.dll 10.0.26100.9818, EnableConcurrentSessions=1 +
  AllowMultipleTSSessions=1, DuoService stopped/manual. ASTER service keys MUTESV_SERVICE /
  MUTENX_SERVICE exist (both disabled): reference gate blocks termwrap, rdp_readiness and MSI
  install until ASTER is gone.
- Pluralinput = `pimoukbd.sys` 0.15.17104.3 (signer Christian Gulden, GlobalSign, cert
  expired 2018, timestamped), per-device UpperFilter on kbdhid/mouhid nodes (oem134 pikbd.inf,
  oem3 pimou.inf), no class filter, no user-mode part running. Class filters: HIDClass
  Upper=HidHide (running, no Parameters key); USB Upper=UsbDk,TsUsbFlt,USBPcap.
- A1 code committed: 2afc181 (seat-session external wrapper mode + tests), 223960c
  (manager state `external` + en/tr copy). Manager check 0 errors, 376 tests.
- Docs: architecture §1.3 + §2 table + §10.4, seat-session README, journal, local README.

- Gates: `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test
  --workspace` 2102 passed, 0 failed (`target/e2e/gaming-pc-2026-11-24/cargo-test.log`);
  manager `bun run check` 0 errors, `bun run test` 376 passed. Docs commit 47d3266.

## In progress

- Nothing. Task A done; hardware steps below wait for the maintainer.

## Next

- Hardware: on the test PC, check `get_termwrap_status` reports `external` with
  `blocked: null` (after ASTER is removed; today `reference_product_present`).
- Before the input test: keep DuoService / MUTESV stopped; verify raw input arrives through
  `pimoukbd`; if not, journal and remove `pimoukbd` from the four devices' UpperFilters for the
  test and restore afterwards.
- Remote seats: decide whether the remote engine may use an External wrapper (own ack).

## Task B (2026-11-24): release installers + ARM64 VM regression

- Goal: build x64 + ARM64 MSI and setup.exe from main HEAD (`build-installer.ps1 -Arch <a>
  -UpstreamStreamServer`, unsigned, `CARGO_TARGET_DIR=target\wf`), record SHA-256; VM
  regression on `PulsarSeatTest-arm64` (tools/vm) against the VM round 2 PASS list in
  docs/agents/status.md (2026-11-17): setup.exe install, service up, manager screenshot,
  compat tweaks apply + revert, single-instance isolation, deny-logon, per-seat env, WRDS
  register / unregister, uninstall leaves nothing. End: restore `clean-logged-on`, VM Off.
- Evidence: `target/e2e/gaming-pc-2026-11-24/vm/`. Previous dist moved to
  `target/installer/dist-prev-4f325ec/`.
- Status: B1 first x64 attempt (HEAD 071b984) was stopped midway (log
  `build-x64-attempt1-stopped.log`). B1 rerun as a **fast test build** (not releasable:
  `CARGO_PROFILE_RELEASE_LTO=false`, `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`, installer/README.md
  "Fast test build") from HEAD d87eead, same `target\wf`, `-Arch x64 -UpstreamStreamServer`,
  unsigned; log `target/e2e/gaming-pc-2026-11-24/build-x64.log`. `target/installer/dist` was
  empty before. ARM64 build is a later step (never two builds at once).
- B1 DONE (2026-11-24, exit 0, TEST BUILD, not releasable, unsigned; `.build.txt` says
  dirty: true = only this progress note was modified):
  - `target/installer/dist/PulsarSeat_0.1.0_x64.msi`
    `e09015ef62c0877b0146ead3cae6ae40633585cf9dafd9148022b5a5ce948d81`
  - `target/installer/dist/PulsarSeat_0.1.0_x64_setup.exe`
    `40a71752e44f9e8b2e7b04b8fe68db683788a7f6587e848d7e8e4d506296673f`
  - service SHA-256 `67c7558aa102eaf4eea0bf82c4f45b296cca94262b8dc7a855f8b768c215f8ba`.
- B2 build DONE (exit 0, 2026-11-24 15:27, TEST BUILD, unsigned, dirty: true = this note):
  - `target/installer/dist/PulsarSeat_0.1.0_arm64.msi`
    `673a79bf239f002ee01d2b374c2fd67569b17c77ab09a227985636f0eb910171`
  - `target/installer/dist/PulsarSeat_0.1.0_arm64_setup.exe` (PE 0xAA64)
    `8da56332e665f528057980650e989f3ba6126b3ea48809b017831f59db199e7c`
  - service SHA-256 `75b45948f56077ed278b23647658cc9507a95b765b8d90b8dad977225ff03662`.
  Built from HEAD d87eead with `-Arch arm64 -UpstreamStreamServer` and
  `CARGO_TARGET_DIR=target\wf`. That dir was reused because B1 was finished and its host build
  scripts were fresh with the same profile env. Log: `target/e2e/gaming-pc-2026-11-24/build-arm64.log`.
- B2 VM regression DONE (2026-11-24): every item PASS, no regression, no code change. Evidence
  is in `target/e2e/gaming-pc-2026-11-24/vm/` (numbered files, `shots/`).
  - setup.exe `/quiet`: exit 0. Service Running/Auto, pipe up, own binaries 0xAA64.
  - Manager: opens unelevated (admin banner) and elevated (seat card). Screenshots
    `shots/02-manager.png` and `shots/02c-manager-elevated.png`.
  - Compat tweaks: they apply only while the product is enabled (by design,
    `Service::sync_tweaks`). Each tweak was checked:
    - lock-screen image
    - driver-update exclusion
    - Windows Update deferrals, active hours and no auto restart
    - device-install deny for printer and cdrom, with admin override
    - Defender exclusions
    - fast startup off
    - inactivity lock off
    - Users' shutdown right off
    - CDROM / Ports `Properties\Security` (read as SYSTEM)

    All tweaks off at once with the product still on: values and created keys are gone.
    The same holds after a disable and after an uninstall.
  - Single-instance (`object_rules` pseat-single.exe / `PSeatSingleTest`):
    - Console: `first`, then `second` 183.
    - Seat 2 (FUS, seat1): `first`, with the shim loaded through the launcher.
  - Per-seat env: the seat 2 startup app saw `seat2-value`, which is also in seat1's
    `Environment`. It is gone after a disable.
  - Deny-logon (seat 3 remote, seat2, port 48100): seat2 is added to SeDenyInteractiveLogonRight.
    The credential test reports 4625 type 2 `0xc000015b`, then a 4624 type 3 network logon.
    The seat2 entry is removed on disable and on uninstall.
  - WRDS: `display_backend = pulsar_idd` registers the listener and CLSID, and TermService
    loads `pulsar-seat-wrds.dll` without a restart. Automatic removes both, and pulsar_idd
    adds them again. RDP-Tcp is untouched. Uninstall removes both.
  - Uninstall (`msiexec /x` with `REMOVEUSERDATA=1`, every option on): exit 0. Nothing is left:
    - no service, dirs, PulsarSeatHost account, WinStation or WRDS CLSID
    - no Run value, credprov registration, `HKLM\SOFTWARE\PulsarSplit` or tweak keys
    - no Defender exclusion; deny is back to `Guest`; the class Properties match the baseline

    The baseline diff shows only Windows background changes: Store apps' firewall rules,
    OneDrive tasks and defaultuser0.
  - VM restored to `clean-logged-on` and Off.
  - Harness notes:
    - The VM runs on battery, so a scheduled task needs `-AllowStartIfOnBatteries`, or it stays
      Queued.
    - The earlier SYSTEM tscon tasks never ran for that reason. `seatctl switch -Seat 1`
      left the console at the lock screen, which was not needed for the rest.

## Task G (2026-11-24): "reference product installed but fully disabled" state

- Goal: maintainer keeps ASTER installed (licence) but disabled for the test. New state
  `reference_product_disabled`: every known reference service/driver key (MUTESV_SERVICE,
  MUTENX_SERVICE) has Start = 4 AND none is running/loaded (SCM) -> MSI / setup.exe install
  with a warning (en + tr), service runtime reports a warning issue instead of blocking,
  manager shows it. Any key not disabled or running -> unchanged (refuse). Pure detection,
  never a write to those keys. Plus read-only probe of the gaming PC ->
  `target/e2e/gaming-pc-2026-11-24/aster-services.txt`.
- Owner: Task G workflow (another workflow runs Task B VM regression in parallel; this note
  section is Task G's only).
- Done: gaming PC probe (read-only) -> `target/e2e/gaming-pc-2026-11-24/aster-services.txt`: exactly
  MUTESV_SERVICE (service, Start 4, STOPPED, C:\Program Files\ASTER) and MUTENX_SERVICE (kernel
  driver, Start 4, STOPPED, system32\DRIVERS); both never started since boot (1077); no filter
  lists name them. The PC is already in the disabled state.
- Done, committed on main (not pushed): 500b4b8 (seat-core `reference` + seat-compat coexist,
  `SystemOps::service_active`), c2e70cf (seat-session note `reference_product_disabled`, termwrap
  host gate), e995efa (MSI launch condition + `PsRefDisabledDlg`, NSIS hooks), 14f412b (manager
  en/tr), fc13dbe + 4c27eb0 (docs). The shipped setup.exe is seat-bootstrap running the MSI, so
  it gets the MSI dialog.
- Gates (before the docs commits): clippy --workspace --all-targets -D warnings clean (also
  seat-session/seat-service --all-features); cargo test --workspace 2112 passed, 0 failed
  (`target/e2e/gaming-pc-2026-11-24/task-g-cargo-test.log`, CARGO_TARGET_DIR=target\wf-gate);
  manager check 0 errors, test 376 passed; `bun test installer/scripts` 94 passed (run from the
  repo root: lint-ci.test.ts reads `.github/workflows/ci.yml` relative to cwd).
- In progress: nothing. Task G done.
- Next: the installers in `target/installer/dist` (from d87eead) still refuse beside ASTER;
  rebuild the x64 MSI + setup.exe from HEAD before the gaming PC test. On the PC: nothing to
  disable (both keys Start 4, stopped); confirm with `sc.exe queryex MUTESV_SERVICE` /
  `MUTENX_SERVICE` right before installing. Expect the MSI warning dialog, setup.log WARNING
  line, manager check `reference_product` = warn, backend note `reference_product_disabled`.

## Task B-rebuild (2026-11-24): x64 test installer from HEAD after Task G

- Goal: x64 MSI + setup.exe from main HEAD 4c27eb0a5042655b938f728d6d3ab77a8bbe814e (FAST TEST BUILD: CARGO_PROFILE_RELEASE_LTO=false,
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16; `build-installer.ps1 -Arch x64 -UpstreamStreamServer`,
  which also runs build-setup.ps1; `CARGO_TARGET_DIR=target\wf` reused), unsigned. Copy MSI +
  setup.exe + build records + hashes to `target/e2e/gaming-pc-2026-11-24/x64-installer/`
  (`target/installer/dist` is shared with the ARM64 VM work; ARM64 files untouched).
- DONE (2026-11-24 16:26 local, exit 0, TEST BUILD, not releasable, unsigned; `.build.txt`
  commit 4c27eb0, dirty: true = only this progress note modified). Log
  `target/e2e/gaming-pc-2026-11-24/build-x64-taskG.log`. Use the copies in
  `target/e2e/gaming-pc-2026-11-24/x64-installer/` (own `SHA256SUMS.txt`):
  - `PulsarSeat_0.1.0_x64.msi` `caf915b366744b2abbd02a67608b9ef84e9bd8c36d5e9c8e7dd7bb4fa4adc849`
  - `PulsarSeat_0.1.0_x64_setup.exe` (PE 0x8664)
    `bb5122102a31427c13a8d4a1620dd7e42b352a231d350c984caa232fc2c35808`
  - service SHA-256 `bdca07b90a87bb9180a2a15fd17077ef69017fecead718ac44b5eb6f5b27af4d`.
  - MSI tables checked: `PsRefDisabledDlg` present; launch condition accepts both reference
    keys absent or `Start` = `#4`.
  - `target/installer/dist` ARM64 files (d87eead) untouched; its `SHA256SUMS.txt` now lists the
    new x64 hashes next to the old ARM64 ones.
- Next: gaming PC test with these x64 copies (`sc.exe queryex MUTESV_SERVICE` / `MUTENX_SERVICE`
  right before installing).

## Task C (2026-11-24): autonomous E2E on the x64 gaming PC

- Goal: install the Task B-rebuild x64 copies (4c27eb0) on the gaming PC beside the disabled
  ASTER (never uninstall it, never touch its files/licence/settings), run local seats (console +
  VG27A DP + VG27A HDMI), G1..G8 checks, remote seat with Moonlight on the same PC, collect logs,
  write `target/e2e/gaming-pc-2026-11-24/REPORT.md`. No restore here (Restore phase does it).
- Journal of every change: `target/e2e/gaming-pc-2026-11-24/changes.jsonl` (before the change).
- Helpers: `target/e2e/gaming-pc-2026-11-24/scripts/` (`rps.sh` copies + runs a .ps1 on the PC
  under `C:\ProgramData\tmpsshd\pse2e\`; `uirun.ps1` runs a script in ahmet's console session via
  a one-shot interactive task; `ishot.ps1` = monitor topology + per-monitor screenshots; `jl.sh`
  appends a journal line).
- Done: step 0 snapshot (`before/`; System Restore enabled on C: + restore point #1, journaled).
  Desktop before: DISPLAY1 = internal SHP 1920x1200@120 (AMD, primary), DISPLAY6 = VG27A DP
  (AUS2722) 2560x1440@144 (NVIDIA). VG27A HDMI (AUS2723) is connected but NOT in the desktop.
- Done: step 1 SunshineService + Parsec stopped, set Manual (journaled). DuoService already
  stopped/manual, no Duo.exe running. PulsarSysAgent + Pulsar desktop app left running.
- Done: step 2 MSI install exit 0; setup.log WARNING `reference_product_disabled` (as designed).
  FINDING: the remote kind (`crates/seat-setup/src/coexist.rs` evaluate, REF_SERVICE =
  DuoService / Duo ARP key) is PASSIVE because Duo is installed: VDD driver skipped, remote
  seats refuse to start -> step 4 expected BLOCKED (Duo must stay installed).
- Done: step 2 service Running/Auto; preflight `reference_product` = warn; termsrv_wrapper
  available (notes wrapper_managed_externally, reference_product_disabled); get_termwrap_status
  = external, blocked null. Config rev1 termsrv_wrapper + ack 2 + debug log; product enabled.
  Monitors only listed while enabled (session-0 CCD, by design). CCD truth: desktop = internal
  (t256) + VG27A HDMI (t4354, DISPLAY6); VG27A DP (t4352) not in desktop.
- Done: users pstest-s2/s3 created (journaled), config rev3 seats 2/3 applied, passwords set.
- Done: credential test ok (2, 3). First start: seat 3 signed in (session 2); seat 2 failed
  `no_display` (DP not in desktop; DisplaySwitch /extend and the product's SDC_TOPOLOGY_EXTEND
  do not attach it -> attached with `scripts/ccd.ps1 -Mode attach -Target 4352`, journaled,
  now DISPLAY7 at 1920,0 @59 Hz). Seat host as PulsarSeatHostNN exits 0xc0000142 twice, third
  try runs (service.log). tools/vm/seatctl.ps1 times out when WMI is slow (Win32_Service query
  60 s > FIRST_REQUEST_TIMEOUT): use `scripts/seatctl.ps1` (sc.exe PID check) on the PC.
- Done: second start: sessions 3 (pstest-s2) and 4 (pstest-s3) Active beside console 1.
- FINDINGS so far: (a) seat 2 reconnect loop: after RDP error_info 17 the new host connection
  reconnects pstest-s2 to its old session 3, but `rdp.rs wait_session` binds the transient new
  session (not in `before` excluded) -> "host reports session 3, but session 5 is bound" ->
  not_our_session -> relaunch every ~11 s; recovered by stop + `logoff 3` + start
  (`logs/loop-evidence/`). (b) rdp_decode presenter shows BLACK on both VG27As (DDA from the
  console, `scripts/dda.ps1`; control with the seat stopped shows the desktop) although the host
  counts frames/presents and the session renders OOBE (in-session shot). (c) G1
  session_capture on seat 3: monitor shows the seat's OOBE (`shots/g1-dda-DISPLAY6.png`) PASS.
  (d) "effective DPI: \\.\DISPLAY1" logged for seats on DISPLAY6/7.
- In-session screenshots: `scripts/uirun2.ps1 -User pstest-sN` (folder C:\ProgramData\pse2e-ui).
- FINDING (e): seats stay `at_logon` and get NO session agent when the user's profile already
  exists (fast logon): WTS Logon arrives before `seats.rs` records `rt.session_id` (bind step
  4), so `on_session_change` finds no seat and `on_seat_logon` never runs (no agent, no G1
  capture offer). Reproduced on seats 2 and 3 after the first logon. Seat 2 reconnect loop
  reproduced a second time (RDP error_info 17 on seat 2's first host, then loop).
- Done: G3 DWMFRAMEINTERVAL journaled absent -> 16 -> 6 (product journal session.jsonl); seat
  sessions report 2560x1440@120Hz. Measured fps not obtained (OOBE covered the test window;
  later the agent race). OOBE policy DisablePrivacyExperience=1 set (journaled).
- Done: G4 seat_one_lock_off: ahmet's Policies\System DisableLockWorkstation=1 while seat 3
  ran, gone after stop; values identical to before (logs/g4-*.txt). PASS.
- Done: G6 refused `other_autologon`, no account/Winlogon write, values identical after revert
  (logs/g6-*). Note: under dedicated_host the host lock guard wrote ahmet's hive (undone).
- Done: G8 split: presenters at -284..996 and 996..2276 on HDMI VG27A (PASS placement).
- Done: workaround for (e): user pstest-s4 (fresh profile) on seat 3 -> signed_in + agent ->
  G1 capture live, G3 DDA 75 updates/s on HDMI during animation (PASS), capture latency logs.
- G7: no `audio buffer` line / no audio_playback in status while a wav played in seat 3 (FAIL).
- G2: winusb_capture swapped Logitech MI_00/MI_01 to WINUSB at seat 2 start (ASUS kbd + ELAN
  untouched). restore_usb_input restored, then re-captured within 3 s (seat 2 still running);
  BUG: usb-capture.json now records MI_00 original = input.inf, but before it was oem128.inf
  (Logitech unifhid 1.10.80.0) -> Restore phase must rebind MI_00 to oem128.inf if needed.
- Done: G2 reboot 17:37 -> back 17:38:53, ahmet auto-logon OK, Logitech nodes on HidUsb again
  but MI_00 on input.inf (was oem128.inf; journaled side effect). Input backend back to
  raw_input_host.
- Done: G5 BLOCKED (DualSense not connected). Kiosk/desktops FAIL (kiosk shell: "stop event
  could not be created", no apps, nothing shown). FUS: start refused console_in_use; leaving FUS
  impossible via apply_config (seat_one_no_display, validate.rs:606 counts non-present
  monitors) -> config.json edited with the service stopped (journaled).
- Done: step 4 BLOCKED: get_host_info coexistence mode passive, reason reference_installed (Duo).
  Moonlight not downloaded.
- Done: step 5 logs in `target/e2e/gaming-pc-2026-11-24/logs/final/`, after-snapshot `after/`
  (Winlogon, TermService, class filters identical to before). Seats stopped, product still
  installed + enabled (termsrv_wrapper, raw_input_host).
- REPORT.md: the subagent harness refused to write report files; the full report went back to
  the caller in the hand-off message (save it as `target/e2e/gaming-pc-2026-11-24/REPORT.md`).
- In progress: nothing. Task C done.
- Next (Restore phase): follow `changes.jsonl`; extra points: Logitech MI_00 back to
  oem128.inf, `scripts/ccd.ps1 -Mode detach -Target 4352` in ahmet's session, delete
  pstest-s2/s3/s4 + profiles, OOBE policy key, C:\ProgramData\pse2e-ui, tmpsshd\pse2e (keep the
  rest of tmpsshd), Sunshine/Parsec Auto + start, Disable-ComputerRestore C:\.
- Next: G-checks, step 4 remote (confirm passive), step 5 report.

## Task D (2026-11-24): restore the gaming PC + fix E2E bugs

- Goal: put the gaming PC back to `target/e2e/gaming-pc-2026-11-24/before/` using `changes.jsonl`,
  diff per item, then fix clear E2E bugs on main (small commits, gates with
  `CARGO_TARGET_DIR=target\wf-e2e`). Another agent may commit on main in parallel: no rebase.
- RESTORE DONE (18:13). Pulsar Seat uninstalled (the physical checklist needs the pstest users
  and an enabled product; the users had to go). ASTER never touched (Start 4 / stopped before and
  after). Per-item table: `target/e2e/gaming-pc-2026-11-24/restore/restore-diff.tsv` (the harness
  refuses .md report files). Equal: Winlogon, TermService/Duo, class filters, watched services,
  input devices (Logitech MI_00 back on oem128.inf; Logitech extension oem185.inf re-applied on
  MI_01, the product's swap-back had dropped it), display topology, audio registry, System
  Restore, named firewall rules, installed products. Windows-only drift: firewall total 1771 ->
  1696 (mpssvc purged per-user AppX rules at boot after the test users were deleted), RasMan WAN
  miniports not enumerated, BITS/IsolationSession start mode, phantom Remote Audio endpoints.
- Fixes DONE, committed on main (not pushed): f910211 (bug 8, validate.rs present monitors only),
  f077268 (bug 2, seats.rs session id recorded before the user read + at-logon snapshot with a
  user = signed_in; test `start_seat_sees_a_logon_that_beat_the_session_record`), 089f28d
  (bug 12, DPI log names the session), 623957a (bug 13, tools/vm/seatctl.ps1 SCM PID before the
  pipe), f1a7d13 (journal). Gates on the combined tree (no other commit landed meanwhile):
  clippy --workspace --all-targets -D warnings clean, cargo test --workspace 2114 passed 0 failed
  (`restore/taskD-clippy.log`, `restore/taskD-cargo-test.log`, CARGO_TARGET_DIR=target\wf-e2e).
- Not fixed (need design or hardware): bug 1 reconnect loop (service + rdp backend rebind),
  3 black rdp_decode presenter (hybrid GPU HYPOTHESIS), 4 audio, 5 winusb_capture original INF
  + NEW: swap-back drops extension INFs (Logitech oem185.inf), 6 kiosk stop event, 7 host
  account 0xc0000142, 9 detached monitor attach, 10 Duo passive remote, 11 refused preset guard.
- In progress: nothing. Task D done (status.md left to the final step).
- Next: a new x64 test build with these fixes, then the physical checklist on the gaming PC.

## Round 2 prep, fix agent b (2026-11-24): bugs 3 and 9

- Branch `e2e2-b` (worktree), not merged, not pushed. Gates: clippy -D warnings for
  seat-display, seat-rdp, seat-host on aarch64 (native) and x86_64-pc-windows-msvc; cargo test
  of those crates green (`CARGO_TARGET_DIR=target\wf-fix-b`).
- Bug 9 DONE (code): `topology::plan_topology` attaches connected, unspecialized plan targets
  outside the desktop (journal `attached`), `plan_paths` prefers a source on the target's own
  adapter; `binding.rs` passes the console targets; tests in topology.rs and binding.rs.
- Bug 3 (diagnostics + most likely fix, root cause NOT proven): host stderr logger
  (`seat-host/src/hostlog.rs`), flip chain on the DXGI owner of the monitor
  (`win::dxgi_output_owner`, `model::pick_flip_adapter`), GDI fallback on a new window
  (`gdi_only`), per-output stats lines in `presenter.rs` (`log_stats`), adapter line per RDP
  connection in seat-rdp.
- Round 2 checks:
  - Bug 9: leave the VG27A DP detached (as restored), start seat 2 without `ccd.ps1`: service.log
    shows `display: attached ... journaled`, seat 2 binds; `journal\display-topology.json` has the
    entry with `"attached": true`; drop seat 2's monitor from the config (or disable): the DP goes
    back out and the entry is gone. Restore: nothing extra if the entry is gone.
  - Bug 3: in `host-02.log` / `host-03.log` read `presenter: output N ... flip chain on adapter`
    (vs the CCD adapter), `first RDP frame ... X of Y samples not black`, the 10 s
    `cpu frames / presents / present errors / occluded / gdi paints` lines and
    `seat-rdp: connection N: graphics adapter`. Not black + monitor black: presentation, rerun
    with `PULSAR_SEAT_PRESENTER=gdi`. Black: RDP composition, rerun with `PULSAR_SEAT_GFX=no-h264`
    then `bitmap`.

## Round 2 prep, integration (2026-11-24): branches e2e2-a..d merged on main

- Merged (no-ff, not pushed): e2e2-a (bugs 1, 7), e2e2-b (bugs 3, 9), e2e2-c (bugs 4, 6),
  e2e2-d (bugs 5, 11). Only conflict: `docs/journal.md` (both dated entries kept).
- Gates on the merged tree (`CARGO_TARGET_DIR=target\wf-integ`, changed .rs touched first):
  clippy --workspace --all-targets -D warnings clean on aarch64 and on
  `--target x86_64-pc-windows-msvc`; cargo test --workspace all ok, 0 failed (aarch64).
  Manager (touched by e2e2-d): check 0 errors; test 376/376 after abce764 (the first
  RemoteTab render passed vitest's 5 s default under the full parallel run; 15 s now).
  Installer not touched. Worktrees and branches e2e2-a..d removed after `git branch --merged`.
- Every fix is HYPOTHESIS on hardware until round 2. Bug 7's commit widens the listener-only
  `WINSTA_EXITWINDOWS` exception to seat hosts: needs the maintainer's OK before a release.
- Host environment (read from code, 2026-11-24): `launch_host` passes no extra env; the host
  gets `CreateEnvironmentBlock(host token, bInherit = FALSE)` plus `PULSAR_SEAT_LOG` and the
  service PID. So `PULSAR_SEAT_PRESENTER`, `PULSAR_SEAT_GFX` and `PULSAR_SEAT_HOST_LOG` set on
  the service (or in a shell) do not reach it. HYPOTHESIS: a machine variable under
  `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` does (journal it, remove
  it after).
- Round 2 checks (run a new x64 test build from main HEAD first):
  - Bug 1 (reconnect loop): leave a pstest-s2 session disconnected (or force an RDP drop), start
    seat 2: one `seat 2: rebound from session X to session Y` or `session Y is the seat's` line,
    then no `session ended` and no relaunch loop. `stop_seat` logs off the real session
    (`qwinsta` shows nothing left). After a host exit, a `session X not logged off ...` line
    shows the WTS view of a disconnected session (settles the client-name HYPOTHESIS).
    `error_info 17` itself is not addressed.
  - Bug 7 (host 0xc0000142): first seat start logs `host token = account PulsarSeatHostNN ...
    mask 0x173` (0x177 with clipboard redirection); no `exit 0xc0000142` / `could not reach its
    desktop`; about 7 s faster start. 0xc0000142 with mask 0x173: cause is elsewhere (profile,
    desktop rights, or `Local\` rights of the logon SID; HYPOTHESIS from fix agent c).
  - Bug 9 (detached monitor): see the fix agent b section above.
  - Bug 3 (black rdp_decode): see the fix agent b section above; for the reruns use the env
    route of the host environment note.
  - Bug 6 (kiosk, desktops backend, `kiosk.accounts = per_seat`, seats 2 and 3): service.log
    `kiosk shell runs as seat account ...` with no `desktop grant` error and no console-user
    fallback; `kiosk-shell-0N.log` has no `stop event could not be created` and shows
    `service connected`, `layout: N monitor(s)` (or `lists no monitor of this seat`),
    `autostart: N app(s)`, `app X started`; launcher + charmap/cmd/notepad visible on the
    VG27As; `stop_seat` without `kiosk shell did not stop in time; killing it`.
  - Bug 4 (audio, wav in a seat with the VG27A endpoint): `host-0N.log` has `audio formats on
    <rdpsnd|AUDIO_PLAYBACK_DVC>: ...`, then `playback started ... on the seat's endpoint`, then
    `audio buffer ...`; status has `audio_playback`; sound on the VG27A. No `audio formats`
    line at all: the server never opened audio; check the session's Remote Audio device.
  - Bug 5 (winusb_capture driver): record MI_00/MI_01 INF, section, version, extension IDs and
    the LOGI_DOWNLOAD_ASSISTANT component first (`g2probe.ps1`, `logiext.ps1`; expect MI_00 =
    oem128.inf [LogiLDA] 1.10.80.0, MI_01 = input.inf + oem185.inf). Capture logs `journaled
    original driver: oem128.inf [LogiLDA] 1.10.80.0 Logitech` (MI_01 with `extensions:
    oem185.inf`). `restore_usb_input` with seat 2 running: answer `{"capture_held":true}`, note
    `usb_capture_held`, `installed oem128.inf [LogiLDA]`, `extension oem185.inf re-applied` (or
    `is back by itself`), `restore check: exact` for both, no `still carries the Pulsar capture`
    sweep line, no re-capture until a seat starts again; SWD component present. Reboot
    mid-capture: one exact restore per node, none at later starts. Report any `NOT exact` text
    and any device other than MI_01 touched by the oem185.inf install.
  - Bug 11 (host lock guard): repeat G6 (dedicated_host + host_autologon with ahmet's
    auto-logon): ahmet's `DisableLockWorkstation` and `ScreenSaveActive` match
    `logs/g4-before.txt`; service.log has `console user X is not the host account ...; nothing
    written there` and no `console lock guard (host account): N written`. Open decision: with
    the preset off, a console user under `dedicated_host` still gets the guard.
  - Not addressed in round 2 prep: bug 10 (remote kind passive beside Duo); the remote seat
    step stays BLOCKED while Duo is installed.

## Round 2 run (2026-11-24): gaming PC, x64 test build of 293b049

- Evidence: `target/e2e/gaming-pc-round2/` (`REPORT2.txt` = matrix, remaining bugs A-N, PC state,
  restore notes, physical checklist; `changes.jsonl` = journal; `before/`, `after/`, `logs/`, `shots/`).
- Fixed on hardware: bug 5 (capture journals the vendor INF + extensions, emergency restore exact and
  held, graceful reboot releases exactly), bug 7 (no 0xc0000142, mask 0x120173), bug 8, bug 11,
  bug 12 (mostly). Bug 9 attach works; detach-back does not (console plan names every monitor).
- Still failing: bug 2 (existing-profile logons often stay at_logon, no agent), bug 3 (RDP image black
  in every graphics mode; the session's Microsoft Remote Display Adapter crashes in WUDFHost at each
  error_info 17), bug 4 (no rdpdr channel; HYPOTHESIS that the server then never starts audio), bug 6 on
  seat 3 (shell restart loop), bug 1 partly (no endless loop; cross-seat logoff attempt, slow relaunch).
- NEW blocker: per-seat host accounts (BATCH logon) cannot query TermService (DACL: SY, BA, IU, SU), so
  `service_pid` says "not running" and every seat fails `rdp_peer_untrusted`. The PC runs with
  `HostTokenCompat=2` (journaled) until this is fixed.
- Restore phase done (journal `changes.jsonl` R1-R5, `restore-diff.tsv`, `after-restore/`): back to
  `before/` except what the maintainer's physical checklist needs, listed with removal steps in
  `LEFT-INSTALLED.txt`: product installed + enabled, seats 2 (pstest-r2a, DP + Logitech) and 3
  (pstest-r2b, HDMI) configured and stopped, `HostTokenCompat=2`, `DWMFRAMEINTERVAL=6`, PulsarSeatHost*
  accounts, DP VG27A attached for seat 2, helper scripts in `C:\ProgramData\tmpsshd\pse2e`.
  Sunshine / Parsec Running + Auto again, OOBE key and pstest-r2c gone, Winlogon / owner hive / Logitech
  drivers equal to before.
- Fixed on main after the run: e8463b9 bug A (TermService PID passed to hosts; HYPOTHESIS until a
  hardware run), ce29f6a bug L, 10b3a49 bug H wording. Gates: clippy clean, cargo test 2146 passed.
- Next: maintainer runs the REPORT2.txt physical checklist; then a round 3 test build with bug A fixed
  (try without `HostTokenCompat`), and fixes for bug B (log every SessionChange with the slot's session
  id), bug I (join rdpdr with audio), bug H (detach when no seat names the target), bug C (claim
  connecting sessions), bug F (do not resend an unchanged layout). Final removal: LEFT-INSTALLED.txt.

## Round 3 prep, fix agent a (2026-11-24): bugs B, C, D, E, J, K (branch e2e3-a)

- Done (unit-tested, gates clean): C (no match / log-off of a session named for or bound to another
  seat; host-reported failure ends the start generation), D (bound session after a host exit: any live
  state + bound id + account), E (signed-in seat keeps its session after a host exit, follows the
  reconnect; recovery off the watchdog thread; manual start clears the crash window), B (session poll
  catches up a missed logon; every SessionChange logged), K (no restart while `SM_SHUTTINGDOWN`; stop
  logged), J (re-choosing `winusb_capture` resumes the capture). Journal line of 2026-11-24 has the
  evidence (LocalSessionManager events read on the PC, read-only).
- Round 3 checks: see the journal line and the e2e3-a hand-off; log lines to look for:
  `session change Logon for session N, user named|no user named (seat sessions: ...)`,
  `has signed-in user ... but no logon was seen` (catch-up fired), `session N kept after the host exit`,
  `log-off of session N ... took N ms`, `restart held, Windows is shutting down`,
  `host exited (pid N) while Windows shuts down`, `service stop requested`, `USB capture resumed`.

## Round 3 prep, fix agent b (branch e2e3-b, 2026-11-24): bugs F and H

- Bug F (bug 3, black RDP picture / RdpIdd crash): the engine sends a layout only when it differs
  from the one in effect and at most every 2 s; the host keeps presenter + layout on a `Reconfigure`
  with unchanged outputs; the presenter reports a layout only on a move / loss / DPI change.
  Retest: host log has `seat host: reconfigure: outputs unchanged` at other seats' logons, at most a
  few `connection N: layout sent (...)` per connection and many `layout not sent (...): unchanged`;
  `graphics reset` count per connection small (1-2); System log without DriverFrameworks-UserMode
  10111 for the Remote Display Adapter; no `error_info 17`; `first RDP frame ... N of 14400 samples
  not black` with N > 0 in `rdp_decode` (avc444, then no-h264, then bitmap). If the adapter still
  crashes with no layout churn: the duplicate-layout HYPOTHESIS is wrong; next suspect Duo TermWrap
  1.6 with termsrv 10.0.26100.9818 on build 26300 (test with one seat only, then a plain RDP client).
- Bug H (bug 9 detach-back): only `TopologyPlan::attach` (seat 1's assigned monitors, wizard extend)
  and seat targets are attached. Retest with the DP VG27A detached by the user first: product enable
  with no seat using it logs no `display: attached`; assigning it to seat 2 logs `attached ... (a seat
  presents it through the desktop); journaled`; dropping it from seat 2 and, separately, `set_enabled
  false` each log `target 4352 of adapter ... detached again (no seat uses it)`, the journal
  `display-topology.json` is empty and `ccd-list` shows only t256 and t4354 active.

## Round 3 fix e2e3-c (2026-11-24): bugs I (audio) and G (kiosk shell loop)

- Branch `e2e3-c`. Done: I = device-less `rdpdr` joined with playback (`seat_rdp::channels::audio_gate`,
  MS-RDPEA appendix A<1>), log `static channels offered: ...` and `audio notes [...] (render open X,
  capture open Y)`. G = root cause proven by the gaming PC event log (21x Application 1000:
  pulsar-seat-host.exe, `twinapi.appcore.dll_unloaded`, 0xc0000005, 20:17-20:19 local) after the
  failed start of the Notepad stub (IFEO `notepad.exe` UseFilter=1, filter entries with
  AppExecutionAliasRedirect=1); `seat_session::alias`, shell refusal + crash guard
  (`kiosk/launch_guard.rs`), service error `kiosk_app_execution_alias`, manager copy.
- Round 3 retest: seat with audio: host log has `static channels offered: rdpsnd (playback), rdpdr
  (audio gate, no devices), drdynvc`, then `audio formats on ...`, `playback started`, and sound from the
  seat's endpoint. Kiosk seat 3 with `C:\Windows\System32\notepad.exe`: apply refused with
  `kiosk_app_execution_alias`; with a config written before the fix the shell logs "is an app execution
  alias" and keeps running (no "session ended" loop, no Application 1000 events).
