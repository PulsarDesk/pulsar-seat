# Pulsar Seat

Free, open-source multi-seat for Windows (x64 and ARM64): several people use one PC at the
same time, each in their own Windows session. Every seat has a kind:

- **Local seat:** a monitor, keyboard, mouse, audio device and USB ports plugged into the
  PC.
- **Remote seat:** a virtual display and virtual gamepads, streamed to a Moonlight or
  Pulsar client on another device. Remote seats need concurrent Windows sessions (Windows
  Server with Remote Desktop Session Host, or the experimental wrapper backend, off by
  default).

Both kinds run side by side on one PC. One service, one manager (English UI with a Turkish
locale) and one MSI per architecture. The remote kind was Pulsar Split; it is merged into
Pulsar Seat (`docs/agents/merge-plan.md`; Split's history is under `docs/split/`).

No payment, no activation, no telemetry. Nothing has been tested on real hardware yet:
behaviour that only hardware can confirm is marked HYPOTHESIS in the docs.

## Install

Each release has, per architecture (ARM64 and x64, both native):

- `PulsarSeat_<version>_<arch>.msi`: the installer (Windows Installer; also for
  deployment with `msiexec /i ... /qn`).
- `PulsarSeat_<version>_<arch>_setup.exe`: a small native setup program with the MSI
  inside. It checks that it fits the PC, then starts the MSI. `/?` lists its options
  (`/quiet`, `/passive`, `/norestart`, `/log <file>`, `NAME=value`, `/extract <folder>`).
- `SHA256SUMS.txt` and its release-key signature `SHA256SUMS.txt.minisig`.

Requirements, options and recovery: [installer/README.md](installer/README.md),
[docs/troubleshooting.md](docs/troubleshooting.md).

## Build

Windows 11 (ARM64 or x64), Rust stable via rustup, [bun](https://bun.sh) (never npm),
Visual Studio 2026 Build Tools with the Windows SDK / WDK 10.0.28000 for the drivers and the
credential provider. The gates that CI runs, locally and in the same order:

```powershell
powershell -ExecutionPolicy Bypass -File installer\scripts\ci-local.ps1
```

Per-package build notes are in each package's README (`crates/*/README.md`,
`drivers/*/README.md`, [apps/manager/README.md](apps/manager/README.md),
[shim/README.md](shim/README.md), [credprov/README.md](credprov/README.md)).

## Contributing, security, licence

- How to contribute (clean-room rules, tests, commit style): [CONTRIBUTING.md](CONTRIBUTING.md).
- Reporting a vulnerability: [SECURITY.md](SECURITY.md).
- Licence: GPL-3.0-only ([LICENSE](LICENSE)). Third-party notices:
  [installer/licenses/NOTICES.md](installer/licenses/NOTICES.md); written source offer
  for the binaries: [installer/licenses/SOURCE-OFFER.txt](installer/licenses/SOURCE-OFFER.txt).

Contributors and agents: start at [AGENTS.md](AGENTS.md).
