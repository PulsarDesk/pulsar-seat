# Contributing to Pulsar Seat

Thank you for helping. Pulsar Seat is GPL-3.0-only; by sending a change you agree that it
is published under that licence.

## Clean room first

Pulsar Seat is a clean-room implementation ([CLEANROOM.md](CLEANROOM.md)). Before you
write code:

- Never look at, disassemble or decompile the binaries of other multi-seat products, and
  never paste anything from them (code, strings, images, UI copy).
- Work from `spec/` (behaviour descriptions written by the project's analysts) and public
  documentation (Microsoft Learn, WDK samples, open-source projects under a
  GPL-3-compatible licence, keeping their notices).
- If you have seen the internals of such a product, say so in your pull request; you may
  then work on parts that do not touch that product's area only.

## Rules that every change keeps

The full list is [docs/agents/rules.md](docs/agents/rules.md). The short form:

- Every source file starts with `SPDX-License-Identifier: GPL-3.0-only`.
- English in code, comments, logs, docs and commit messages. User-facing strings exist in
  English (default) and Turkish with the same keys (`apps/manager/src/lib/locales/`,
  `seat_core::i18n`, `installer/wix/locales/`).
- bun, never npm / npx. Rust stable via rustup.
- ARM64 and x64 are both first class: everything builds natively for both.
- No vulnerable drivers, no kernel patching, nothing that needs Memory Integrity, Secure
  Boot or driver signature enforcement turned off.
- Never commit secrets, keys, certificates, local paths or machine facts
  (`bun installer/scripts/repo-scan.ts` checks the tree).

## Before you open a pull request

Run the gates; all must be green and test counts only go up:

```powershell
powershell -ExecutionPolicy Bypass -File installer\scripts\ci-local.ps1
# drivers / credential provider / installers when you touched them:
powershell -ExecutionPolicy Bypass -File installer\scripts\ci-local.ps1 -Drivers -CredProv -Installers
bun installer/scripts/repo-scan.ts
```

- Add tests with the change (a bug fix gets a test that fails without it).
- Mark behaviour you could not verify on real hardware as `HYPOTHESIS` in the package
  README and say so in the pull request.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`fix(seat-input): ...`, `feat(installer): ...`, `docs: ...`).
- Keep changes focused; update the matching README / doc in the same change.

## Drivers

Driver changes are built unsigned (`installer/scripts/build-drivers.ps1`), pass infverif,
ApiValidator and the WDK code analysis, and are never loaded on a contributor's own PC.
Test-signing is for disposable VMs only (`tools/vm/README.md`). Release signing goes
through Microsoft (docs/release/driver-submission.md).

## Reporting bugs

Open an issue with the Windows build, architecture, the Pulsar Seat version (manager →
About) and the service log (`%ProgramData%\Pulsar Seat\logs`). Remove user names, IP
addresses and passwords before you attach a log. Security problems go to
[SECURITY.md](SECURITY.md), not to a public issue.
