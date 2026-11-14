# Hard rules

> **Superseded (merge step 10, 2026-11-14).** Pulsar Split is merged into Pulsar Seat. This file is kept as history and is not updated: names such as `pulsar-split-service`, `PulsarSplit.Control`, IPC v2, `installer/split/` and `split-*` crates are gone. Current facts: [AGENTS.md](../../../AGENTS.md), `docs/agents/`, `installer/README.md` and the package READMEs.

Index: [../../AGENTS.md](../../../AGENTS.md).

## Pulsar-wide (from `../docs/agents/rules.md`)

- License **GPL-3.0**, never MIT.
- **bun**, never npm. Rust via rustup stable.
- **UI copy is Turkish.** Commit messages in English.
- **Never push a behaviour change the maintainer hasn't tested.** Docs and tests may be
  pushed. Push only on the maintainer's go.
- Tests stay green: `bun run test` (UI), `cargo test` (Rust).

## Security

- **No vulnerable drivers, no kernel patching.** No BYOVD, no writing kernel memory, no
  disabling or working around DSE / HVCI (Memory Integrity). A feature that needs this is
  done another way or not at all.
- Everything must keep working with Memory Integrity **on**.
- Never read, print, copy or commit secrets: pairing keys, certificates, seat-user
  passwords, tokens.

## Local services

- No local web server or HTTP API of ours; manager ↔ service is the named pipe
  `\.\pipe\PulsarSplit.Control` only. One **accepted** exception (maintainer,
  2026-11-13, OQ 28): for Sunshine seats the service, and only the service, uses the
  upstream stream server's own web-UI listener for `/api/pin` and the client list. Keep it
  strict: connect to `127.0.0.1:<base+1>` only, `origin_web_ui_allowed = pc`, never a
  firewall rule for base+1, credentials random per server start (memory only, zeroised,
  written to `credentials_file` as upstream's salted hash, never argv / env / log /
  manager), the seat server's own certificate pinned, no system proxy, no browser.
- Every child process started inside the service takes `split_proto::spawn` (contracts §0).

## HDR

- Use the supported indirect-display path: IddCx HDR / wide colour gamut on the Windows
  versions that have it. If the OS lacks it, seats run SDR and the UI says why.

## Clean room

- See [../../CLEANROOM.md](../../../CLEANROOM.md). Implementers read `spec/remote/` and public docs
  only; never third-party binaries, disassembly or analyst raw notes.
- No third-party names, logos, icons, images or UI copy. Layout and flow may follow the
  spec for now; the maintainer will redesign the UI later.

## Dev machine

- Windows 11 **ARM64** laptop. Install only native ARM64 builds where they exist; check the
  PE machine type before and after. Admin work: one elevated
  `Start-Process powershell -Verb RunAs -Wait` per batch, ask before system changes.
- The multi-seat host runs on an x64 test PC (see `docs/local/`). Read-only there unless
  the maintainer approves a change.
