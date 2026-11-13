# Hard rules

Index: [../../AGENTS.md](../../AGENTS.md).

## Pulsar-wide (from `../docs/agents/rules.md`)

- License **GPL-3.0**, never MIT.
- **bun**, never npm. Rust via rustup stable.
- **UI copy is Turkish.** Commit messages in English.
- **Never push a behaviour change the maintainer hasn't tested.** Docs and tests may be
  pushed. Push only on the maintainer's go.
- All work lands on `main`; short branches only, merged the same day.

## Security

- **No vulnerable drivers, no kernel patching.** No BYOVD, no writing kernel memory, no
  disabling or working around DSE / HVCI (Memory Integrity). A feature that needs this is
  done another way or not at all.
- Everything must keep working with Memory Integrity **on** and Secure Boot **on**.
- Never read, print, copy or commit secrets: licence keys, seat-user passwords, tokens.

## ARM64 is first class

- Every component (service, manager, drivers, installer) builds and runs natively on
  **ARM64 and x64**. No x64-only dependency without the maintainer's OK.
- Drivers use only frameworks available on ARM64 Windows (KMDF/UMDF, IddCx). Prefer UMDF
  and user-mode solutions where they are enough.
- Check the PE machine type of every build artifact (0xAA64 = ARM64, 0x8664 = x64).

## Clean room

- See [../../CLEANROOM.md](../../CLEANROOM.md). Implementers read `spec/` and public docs
  only.
- No third-party names, logos, icons, images or UI copy. Layout and flow may follow the
  spec for now; the maintainer redesigns the UI later (with Claude Design).

## Dev machine

- Windows 11 **ARM64** laptop. Install only native ARM64 builds where they exist; check the
  PE machine type before and after. Admin work: one elevated
  `Start-Process powershell -Verb RunAs -Wait` per batch, ask before system changes.
- The reference product runs on an x64 test PC (see `docs/local/`). Read-only there unless
  the maintainer approves a change.
