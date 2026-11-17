# Pulsar Seat — start here

Free, open-source (GPL-3.0) **multi-seat** for Windows, x64 and ARM64: one PC, several
people, each in their own Windows session. Seat kinds: `local` (monitors, keyboards, mice,
audio, USB ports of the PC) and `remote` (virtual display + pads, streamed with Pulsar or
Moonlight; formerly Pulsar Split, merged in). One service, one manager, one MSI. Part of
the Pulsar umbrella (`../`). Clean-room implementation.

This file is an **index only**. Facts live in the files below. Keep it short; add a row
when you add a doc.

## Before anything

1. Read [CLEANROOM.md](CLEANROOM.md). Know your role: **implementer** (default) or
   **analyst** (only when explicitly started as one).
2. Read [docs/agents/rules.md](docs/agents/rules.md).
3. Resume from [docs/agents/status.md](docs/agents/status.md) and `docs/progress/*.md`.

## Always

- **Crash-safe progress notes:** `docs/progress/<task-label>.md` (goal, done, in progress
  with exact file/function, next, how to verify). Update after each meaningful step;
  delete it when the task is merged into status.
- **Write down what you learn** in the matching doc plus a dated line in
  [docs/journal.md](docs/journal.md). Mark unverified ideas `HYPOTHESIS`.
- Local-only facts (machines, IPs, paths outside the repo) go in `docs/local/`
  (gitignored), never in tracked files.

## Map

| Topic | File |
| --- | --- |
| Clean-room rules | [CLEANROOM.md](CLEANROOM.md) |
| Hard rules (both kinds) | [docs/agents/rules.md](docs/agents/rules.md) |
| Phases, scope, layout, decisions | [docs/agents/plan.md](docs/agents/plan.md) |
| Architecture and contracts: crates, processes, backends, IPC, config | [docs/agents/architecture.md](docs/agents/architecture.md) |
| Merge with Pulsar Split (done through step 10; step 11 = hardware tests) | [docs/agents/merge-plan.md](docs/agents/merge-plan.md) |
| Current status / resume point | [docs/agents/status.md](docs/agents/status.md) |
| Dated log | [docs/journal.md](docs/journal.md) |
| Installer (one MSI per arch), signing, updates, remote payload | [installer/README.md](installer/README.md) |
| Release checklist (maintainer) | [docs/release/checklist.md](docs/release/checklist.md) |
| User troubleshooting (English; Turkish copy `docs/troubleshooting.tr.md`) | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Behaviour spec (analyst-written): `spec/local/`, `spec/remote/` | [spec/README.md](spec/README.md) |
| `pulsar-host` pipe contract (remote seats on the Pulsar backend; still binding) | [docs/split/agents/pulsar-host-backend.md](docs/split/agents/pulsar-host-backend.md) |
| Third-party drivers / stream server / RDP libs (remote kind, verified table) | [docs/split/agents/third-party.md](docs/split/agents/third-party.md) |
| Pulsar Split history before the merge (superseded, not updated) | [docs/split/README.md](docs/split/README.md) |
| Manager (both kinds; remote pages in `src/lib/remote/`) | [apps/manager/README.md](apps/manager/README.md) |
| Credential provider + filter | [credprov/README.md](credprov/README.md) |
| Per-process HID shim (own x64 / x86 / ARM64 workspace, Detours on ARM64) | [shim/README.md](shim/README.md) |
| Package READMEs (build, test, untested parts, notices) | `crates/*/README.md`, `drivers/*/README.md` |
| VM test harness (pipe client, Hyper-V helpers) | [tools/vm/README.md](tools/vm/README.md) |
| Test machine access (local only) | `docs/local/README.md` |
