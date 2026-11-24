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
- Status: B1 build starting (HEAD 076451f).
