# Hard rules

Index: [../../AGENTS.md](../../AGENTS.md). One rule set for both seat kinds (local and
remote). Since merge step 10 this file holds the union of Seat's rules and Pulsar Split's
`docs/split/agents/rules.md` (kept as history).

## Pulsar-wide (from `../docs/agents/rules.md`)

- License **GPL-3.0**, never MIT.
- **bun**, never npm. Rust via rustup stable.
- **English everywhere, Turkish only as an app i18n locale** (umbrella rule). Commit
  messages, docs, code, logs, wire codes and technical details are English. UI copy is
  English by default with a Turkish locale that has exactly the same keys:
  - Manager: `apps/manager/src/lib/locales/en.ts` (the key set, type `Locale`) and `tr.ts`;
    `i18n.ts` holds the logic. Language = the user's choice (Basic settings) or the
    Windows UI language. `i18n.test.ts` fails on a key in one locale only.
  - Rust UI copy (tray, kiosk shell, seat host notices, Event Log, CLI, account comments,
    firewall descriptions, Tauri shell): `seat_core::i18n::Text { en, tr }`; every table is
    checked with `seat_core::i18n::check_table` in a test. Logs stay English only.
  - Credential provider: `credprov/src/copy.h` (pairs, self-test checks both).
  - Installer: `installer/wix/locales/{en-US,tr-TR}.wxl` (`!(loc.Id)`), NSIS LangStrings
    1033 / 1055; `installer/scripts/wix-locales.test.ts`.
  - The experimental warning: `crates/seat-session/src/termwrap/warning_{en,tr}.txt`.
  - User docs: English; a Turkish copy may sit next to them as `*.tr.md`.
- **Never push a behaviour change the maintainer hasn't tested.** Docs and tests may be
  pushed. Push only on the maintainer's go.
- All work lands on `main`; short branches only, merged the same day.
- Tests stay green: `cargo test --workspace`, clippy with zero warnings (aarch64 and
  x86_64), `bun run test` / `check` / `build` in `apps/manager`, the `shim/` workspace,
  `bun test installer/scripts`. Test counts only go up, except tests deleted on purpose
  (say which and why).

## Security

- **No vulnerable drivers, no kernel patching.** No BYOVD, no writing kernel memory, no
  disabling or working around DSE / HVCI (Memory Integrity). A feature that needs this is
  done another way or not at all.
- Everything must keep working with Memory Integrity **on** and Secure Boot **on**.
- Never read, print, copy or commit secrets: licence keys, seat-user passwords, pairing
  keys, certificates, tokens, the release private key.
- Every third-party artefact we ship is pinned by URL and SHA-256
  (`installer/remote/drivers.json`, `installer/remote/stream-server.json`) and checked at
  build time (`installer/scripts/fetch-deps.ts`, `check-payload.ts`). Nothing unpinned is
  downloaded or bundled. No certificate is ever installed on a user machine.

## Local services

- No local web server or HTTP API of ours. Manager and seat processes talk to the service
  over its named pipes only: the control pipe `\\.\pipe\PulsarSeat.Control` (protocol 3,
  Seat's access table), plus the kept per-purpose pipes (shim health, `pulsar-host`
  per seat; see `docs/split/agents/pulsar-host-backend.md`).
- One **accepted** exception (maintainer, 2026-11-13, Split OQ 28): for Sunshine seats the
  service, and only the service, uses the upstream stream server's own web-UI listener for
  `/api/pin` and the client list. Keep it strict: connect to `127.0.0.1:<base+1>` only,
  `origin_web_ui_allowed = pc`, never a firewall rule for base+1, credentials random per
  server start (memory only, zeroised, written to `credentials_file` as upstream's salted
  hash, never argv / env / log / manager), the seat server's own certificate pinned, no
  system proxy, no browser. The web UI itself never ships (check-payload refuses it).
- Every child process started inside the service holds `seat_core::spawn::spawn_lock`
  around the create call (`spawn_locked` for a `Command`). Passwords reach child
  processes over stdin or a pipe, never argv or the environment.

## ARM64 is first class

- Every component (service, manager, drivers, installer) builds and runs natively on
  **ARM64 and x64**. No x64-only dependency without the maintainer's OK.
- The per-process shim (`shim/`) builds for ARM64 too (Microsoft Detours backend, since
  parity-arm64 2026-11-17): the ARM64 MSI ships `shim\arm64\` plus `shim\x64\` and
  `shim\x86\` for emulated apps (the only non-native binaries besides the app API helpers).
  Third-party upstream installer stubs (for example usbip-win2's) may be x86.
- Drivers use only frameworks available on ARM64 Windows (KMDF/UMDF, IddCx). Prefer UMDF
  and user-mode solutions where they are enough.
- Check the PE machine type of every build artifact (0xAA64 = ARM64, 0x8664 = x64).

## HDR (remote kind)

- Use the supported indirect-display path: IddCx HDR / wide colour gamut on the Windows
  versions that have it. If the OS lacks it, seats run SDR and the UI says why.

## Clean room

- See [../../CLEANROOM.md](../../CLEANROOM.md). Implementers read `spec/` (`spec/local/`,
  `spec/remote/`) and public docs only; never third-party binaries, disassembly or analyst
  raw notes.
- No third-party names, logos, icons, images or UI copy. Layout and flow may follow the
  spec for now; the maintainer redesigns the UI later (with Claude Design).

## Dev machine

- Windows 11 **ARM64** laptop. Install only native ARM64 builds where they exist; check the
  PE machine type before and after. Admin work: one elevated
  `Start-Process powershell -Verb RunAs -Wait` per batch, ask before system changes.
- The reference products run on an x64 test PC (see `docs/local/`). Read-only there unless
  the maintainer approves a change.
