# Plan and decisions

Index: [../../AGENTS.md](../../AGENTS.md).

## Phases

| # | Scope | Output |
| --- | --- | --- |
| 0 | Behaviour spec of the reference local multi-seat product | `spec/` |
| 1 | Manager app + seat service: seat model, device inventory, assign devices to seats, config, start/stop, status | Tauri app + Rust service |
| 2 | Concurrent local interactive sessions, one per seat, with auto-logon per seat | |
| 3 | Display: monitors / GPU outputs per seat, each session shown only on its monitors | |
| 4 | Input isolation: keyboards, mice, gamepads, touch routed to their seat only | |
| 5 | Audio and USB: per-seat audio endpoints, USB storage / printers / other devices per seat | |

Each phase works on its own before the next starts.

## Decisions (2026-11-12, maintainer)

- Name **Pulsar Seat**; lives under the Pulsar umbrella next to Pulsar Split.
- **Monorepo**: one repo `pulsar-seat` holds everything. Root Cargo workspace + root bun
  workspace. Planned layout: `crates/` (Rust libs + service), `apps/` (manager),
  `drivers/` (filter drivers etc.), `installer/`, `spec/`, `docs/`. One CI, one version,
  one release.
- Manager: **Tauri 2 + SvelteKit + Svelte 5** (latest), `adapter-static`; service and
  drivers in **Rust** where possible. No local web server; manager and service talk over a
  named pipe with an ACL.
- Clean room with **two separate agents** (see [../../CLEANROOM.md](../../CLEANROOM.md)).
- UI layout may match the reference for now; the maintainer redesigns it later.
- Fully free: no licence keys, activation or paid tiers.
- Native **ARM64** support from day one (the reference product has none).

## Decisions for phases 1–5 (2026-11-12, maintainer: "do all phases in one run, no live test")

Same calls as Pulsar Split's Phase 2 decisions, applied here until the maintainer says
otherwise:

- **Sessions:** a swappable session backend.
  - Default: Windows Server + RDSH, using RDP loopback sessions (spec 30 option A). This is
    licence-clean.
  - Experimental client-SKU module (termsrv wrapper): off by default, with a clear warning
    in the manager. It is built only from public open-source knowledge under compatible
    licences, never from the reference product.
  - Fallbacks: fast user switching (option C) and desktops in one session (option D,
    kiosk / guest seat).
  - Never modify LSM, never patch the kernel.
- **Drivers:** no money for signing now.
  - Default backends are user-mode or existing **signed, open-source, redistributable**
    drivers. Verify signing and licence before use; keep their notices.
  - Our own drivers (input device filter, IddCx display, storage minifilter) are written
    under `drivers/` behind the same backend traits, but **not loaded or tested until
    signed**. No test-signing on the test PC.
  - Drivers build on the dev laptop with VS 2026 Build Tools + WDK 10.0.28000 (2026-11-13,
    `installer/scripts/build-drivers.ps1`); the builds stay unsigned and are never loaded.
- **Code from Pulsar Split** (our own GPL-3 code) may be copied and adapted. It must not be
  added as a path or git dependency.
- Nothing from the reference product is used or shipped.

## Decisions (2026-11-13, fix-rdp-client)

- Logon-screen seats (no password) on an NLA-only RDP-Tcp listener: design (a), a second
  loopback-only WinStation listener with NLA off for passwordless seats (details and the
  rejected option in architecture.md §4.1). Built (`seat_session::loopback_listener`,
  setting `loopback_listener`); with the setting off such seats still fail with
  `nla_required`.
- The IronRDP seat host reconnects by itself only for passwordless seats; with a stored
  password it exits as `disconnected: transient` and the service decides (no password is
  replayed by the host after a failure).

## Decisions (2026-11-13, maintainer)

- **Client SKUs:** U1 (real sessions taking turns, credential provider) plus U3 (concurrent
  kiosk / app seats), per spec 35. Server x64 keeps concurrent `rds_loopback` seats.
- **termsrv wrapper:** keep it behind `experimental-client-sku`, off by default, with the
  warning. It is x64 only.
- **Credential provider:** build it. **Second loopback listener with NLA off** for
  passwordless seats: build it. Release blockers: clear them.
- **Driver signing:** test-signing in a Hyper-V VM now; Microsoft attestation before the
  release. Enabling Hyper-V on the dev laptop is a system change and needs a reboot, so
  agree the timing with the maintainer first.
- **USB storage:** "same as the reference". The reference uses a file-system minifilter
  (spec 70), so the minifilter stays as an approved exception. It falls back to
  device-security gating when not loaded. The maintainer sends the altitude request
  (`docs/release/minifilter-altitude-request.md`).
- **Wizard keys:** "same as the reference". The listener reports only the keys the wizard
  uses: F1–F12, digit keys, Enter, Backspace, Esc, and Shift/Alt with F4. It does not
  report every key code. Done 2026-11-13 (final-fix): `console_report::is_wizard_key`; `wizard_all_keys`
  is obsolete (always false, ignored).
- Hyper-V was enabled on the dev laptop on 2026-11-13 (maintainer OK). Windows reported no
  restart needed.
- **Release signing key:** generate it now. The private key goes to
  `%USERPROFILE%\.pulsar-seat\release-key`, never into the repo; the public key goes into
  `installer/release-signers.txt`. The maintainer keeps a backup. Run it with the round-3
  keygen script once that exists. Done 2026-11-13 (key id 0E7EC5FC77170613); backups open.
- **Merge with Pulsar Split into ONE product:** each seat is either "local" (physical
  devices, this repo) or "remote" (virtual display streamed with Moonlight / Pulsar,
  Pulsar Split). Both kinds run side by side on
  one PC. Order:
  1. Finish round 3.
  2. Write a joint monorepo merge plan together with the Split session.
  3. Merge the code.
  The maintainer picks the product name.
- **Publishing:** stays local. No remote and no push. Public repo plus GitHub Releases come
  only after the hardware tests pass.
- **First live test:** on the ARM64 dev laptop, with U1 (test accounts) and U3. No drivers
  loaded. The maintainer starts it.

## Decisions (2026-11-14, maintainer)

- **Merge plan approved** ([merge-plan.md](merge-plan.md) JOINT v1, all §5 recommendations).
  The merged product keeps the name **Pulsar Seat**; repo pulsar-seat is the base, and
  Split is folded in with history.
- **Gamepads per seat:** step 8 of the merge, via the shared pads crate (option B).
- **First commit** on main now (local only, no push); then start the code merge.

## Open questions

- **Session backend (blocks Phase 2, from spec 30 §6.1).** The reference product creates its
  extra local sessions by working inside Windows' session manager (LSM) and the console. The
  analyst excluded that path. The options left are:
  - A: RDP loopback sessions on Windows Server + RDS. Documented, but x64 / Server only and
    needs CALs.
  - C: fast user switching, one seat at a time.
  - D: separate desktops inside one session. Works on client SKUs and ARM64, but has no
    per-user isolation.
  Maintainer to choose, or to allow a user-mode session-manager approach (no kernel patching)
  as an explicit exception.
- Minifilter for USB storage gating (spec 70, 90): allowed as an exception to the driver
  list?
- Driver signing route: attestation signing (Partner Center + EV cert) vs. a test-signing VM.
- Live run of the reference on the test PC (needs enabling its service and probably the
  2.70.6 upgrade, a reboot and a restore point) to confirm the enabled-state HYPOTHESIS items.
- Gamepads per seat (open gap): Pulsar Split handles this with usbip-win2 for DualSense,
  ViGEm only if already installed, and a per-session HID shim (`crates/seat-pads` and the
  shim crates in pulsar-split). Copy and adapt that, or decide a shared crate (2026-11-13,
  agreed with the Split session: no cross-repo dependency for now).
- Share code with Pulsar Split (session creation, service skeleton, manager shell)? Both
  need several sessions on one PC. Decide once both specs exist; until then no cross-repo
  dependency.

## Known risks (to resolve before a public release)

- **Driver signing:** any kernel driver needs signing to load with Secure Boot on. Free
  option: test-signing (bad UX). Proper: attestation signing (paid EV cert). Prefer
  designs that need few or no kernel drivers.
- **Multiple interactive sessions on client Windows SKUs** are restricted by Microsoft's
  licence terms. Decide the approach in Phase 2 (same question as Pulsar Split).

## Decisions (2026-11-15, maintainer)

- **Printer redirection** (`SeatRedirect.printers`, round 4): accepted as a Pulsar Seat
  addition beyond the reference behaviour (spec/local/70 §3.8: the reference does nothing
  with printers). Default stays `off`, so out of the box every seat sees every printer,
  as with the reference.
- **Remote seat microphone** (pulsar-host contract §3.8, `mic` capability): accepted. The
  matching change in pulsar-desktop is still to be made; until then Pulsar seats report
  `mic = unsupported`.
- **Seat 1 kiosk shell lifecycle** (VM test 2026-11-15, open issue 2): option (a). When the
  product is enabled and `settings.kiosk.seat1_desktop = false` and seat 1 has a
  `kiosk_shell`, the service itself starts seat 1's kiosk shell under the same rules as the
  other kiosk seats: start on enable and at boot, stop on disable, restart after a crash,
  clean up on stop. Seat 1 stays the console seat in every other respect.
- **Kiosk hotkey guard** (VM fix round, open issue): debug the kiosk shell's own
  `WH_KEYBOARD_LL` hook first (a standalone hook blocks the Windows key in the same VM).
  An Explorer-less kiosk console is the next option only after asking; it is not built
  without a new decision.
- **Console listener account rights** (VM fix round, open issue): the contained console
  listener's account gets `WINSTA_EXITWINDOWS` on the console window station, as its own
  ACE only, removed when the listener stops and journaled for uninstall. Accepted risk:
  that account could log the console user off. The `ERROR_PIPE_BUSY` behind it is to be
  found and fixed. **Decided 2026-11-24 (maintainer; gaming PC E2E bug 7, commit a1f9ce5
  kept):** seat hosts get the same right on their own ACE; same scope and accepted risk.
  Reason: without it USER32 init fails (0xc0000142), proven on the gaming PC in round 3.
  Risk note (HYPOTHESIS): the host can log off only its own logon session.
- **Kiosk console apps in classic conhost** (VM round 3): console apps of kiosk seats open
  in conhost, not Windows Terminal, through the per-user default-terminal setting
  (`HKCU\Console\%%Startup` `DelegationConsole` / `DelegationTerminal`), only for kiosk
  seat users (never seat 1's desktop user, never the machine default), applied when the
  kiosk shell starts and restored on stop / uninstall, journaled.
- **`wizard_start` in the kiosk configuration**: find and fix the hang (VM round 3).
- **Hotkey guard warm-up** (2026-11-15, before deciding on an Explorer-less kiosk console):
  after the kiosk shell installs or re-installs its `WH_KEYBOARD_LL` hook, and when a new
  window first takes the foreground, it injects one harmless key (`VK_F24` down + up,
  tagged in `dwExtraInfo`) with `SendInput`; the hook swallows it so no app sees it and the
  shell logs the hook's latency for it. If a console window in the foreground still makes
  the first Windows-key press miss, stop and report; the Explorer-less kiosk console is
  still not built without asking.
- **Hotkey guard in a helper process** (2026-11-15, after the bisect showed that an LL hook
  in the process that owns the foreground window and a raw-input registration is not
  called): the kiosk shell starts `pulsar-seat-host --hotkey-guard --seat N` as the kiosk
  console user (same session, desktop and integrity, no extra rights), which installs the
  `WH_KEYBOARD_LL` guard with `seat_input::hotkey_guard` and registers no raw input. The
  shell watches it, restarts it with back-off and stops it on exit; a minimal channel from
  the shell (an anonymous pipe, no key text) carries re-install and warm-up requests. No
  re-install loop; the hook thread does no blocking I/O.
- **Driver round in the test VM** (2026-11-15; the user's answer, relayed by the
  coordinator, to the 2026-11-13 decision "test-signing in a Hyper-V VM now"): only in the
  VM PulsarSeatTest-arm64: Secure Boot off and `bcdedit /set testsigning on`; a
  self-signed test code-signing certificate kept under `%USERPROFILE%\.pulsar-seat\testsign\`
  on the dev machine (never in the repo, the MSI or the release signing path) and trusted
  in the VM (Root + TrustedPublisher); our drivers (input filter, IddCx, storage minifilter
  with a TEST ONLY altitude, split-pad) installed there with `pnputil` and run under Driver
  Verifier once. Memory Integrity, HVCI and VBS stay on / untouched everywhere; an HVCI
  block is a driver bug. Nothing changes on the host's boot or security configuration.
- **Remote IDD role and HVCI in the test VM** (2026-11-16; the user's answer relayed by the
  coordinator: "1 2 onay veriyorum" to (1) "VM'de remote rol testi: sürücü seviyesinde,
  remote rolde adaptör + monitör oluşturmak" and (2) "VM'de HVCI testi: VM'de Memory
  Integrity'yi açıp sürücülerimizi denemek. Açmak güvenliği artırır, azaltmaz."): only in
  the VM PulsarSeatTest-arm64, nothing on the host. (1) remote-role adapter and monitor
  through the driver's control interface, no stream server. (2) VBS + HVCI (Memory
  Integrity) on in the VM if this Hyper-V ARM64 guest can run them under test-signing
  (nested virtualization exposure allowed on the VM's configuration); nothing weakened to
  force it; checkpoint `testsigning-hvci` if it works.

## Decisions (2026-11-24, maintainer, after gaming PC E2E round 3)

- **Bug 7, seat hosts get `WINSTA_EXITWINDOWS`:** commit a1f9ce5 is kept. Reason: without
  it USER32 init of the seat host fails (0xc0000142), proven on the gaming PC in round 3.
  Risk note (HYPOTHESIS): the host can log off only its own logon session. Architecture §3
  (host account).
- **N, seat-one console guard under `dedicated_host`:** kept. With the host console preset
  (auto-logon) off, a human console user under `dedicated_host` still gets the guard (lock
  and screen saver off), as `console_guard::console_owner` does today. Architecture
  "Console lock guard and seat power policies".
- **Bug 10 / M, remote seats beside the remote reference product: always allow.** Its
  presence (installed or running) no longer makes the remote kind passive: drivers install
  and remote seats start normally. A running reference service gives the non-blocking note
  `duo_service_running`. ASTER rules unchanged; nothing is written into the reference
  product's keys or files. Architecture "Remote kind beside the reference remote product".
  HYPOTHESIS until a hardware run.
