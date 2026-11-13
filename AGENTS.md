# Pulsar Seat — start here

Free, open-source (GPL-3.0) **local multi-seat** for Windows: one PC, several monitors,
keyboards, mice, audio devices and USB ports, each group forming a *seat* where one person
works or plays in their own Windows session at the same time as the others. Clean-room
implementation, part of the Pulsar umbrella (`../`). Runs natively on **x64 and ARM64**.
No payment, no activation, no telemetry.

Sister project: [Pulsar Split](../pulsar-split/AGENTS.md) (seats streamed over the network).
Seat is about devices plugged into the PC itself.

This file is an **index only**. Facts live in the files below. Keep it short; add a row
here when you add a doc.

## Before anything

1. Read [CLEANROOM.md](CLEANROOM.md). Know your role: **implementer** (default) or
   **analyst** (only when explicitly started as one).
2. Read [docs/agents/rules.md](docs/agents/rules.md).
3. Resume from [docs/agents/status.md](docs/agents/status.md) and `docs/progress/*.md`.

## Always

- **Crash-safe progress notes.** Every agent keeps `docs/progress/<task-label>.md` (goal,
  done, in progress with exact file/function, next, how to verify). Update after each
  meaningful step and before long builds. Delete it when the task is merged into status.
- **Write down what you learn.** Each finding goes into the matching doc below, plus a dated
  line in [docs/journal.md](docs/journal.md). Mark unverified ideas `HYPOTHESIS`. Fix docs
  that turn out wrong.
- Local-only facts (machines, IPs, paths outside the repo) go in `docs/local/` (gitignored),
  never in tracked files.

## Map

| Topic | File |
| --- | --- |
| Clean-room rules: roles, what may cross into the repo | [CLEANROOM.md](CLEANROOM.md) |
| Hard rules: license, security, ARM64, tooling | [docs/agents/rules.md](docs/agents/rules.md) |
| Phases, scope, monorepo layout, decisions | [docs/agents/plan.md](docs/agents/plan.md) |
| Architecture and contracts: crates, processes, backend traits, IPC, config | [docs/agents/architecture.md](docs/agents/architecture.md) |
| Merge with Pulsar Split: draft plan (seat kinds local / remote, shared crates) | [docs/agents/merge-plan.md](docs/agents/merge-plan.md) |
| Release checklist (maintainer steps, first commit, keys, signing) | [docs/release/checklist.md](docs/release/checklist.md) |
| Current status / resume point | [docs/agents/status.md](docs/agents/status.md) |
| Dated log of sessions and findings | [docs/journal.md](docs/journal.md) |
| Per-task progress notes | `docs/progress/` |
| User troubleshooting and offline recovery (Turkish) | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Credential provider + filter (fast user switch auto-logon) | [credprov/README.md](credprov/README.md) |
| Behaviour spec (analyst-written) | [spec/README.md](spec/README.md) |
| Test machine access, local paths (local only) | `docs/local/README.md` |
