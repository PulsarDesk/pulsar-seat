# Pulsar Split — start here

Free, open-source (GPL-3.0) multi-seat host for Windows: several people use one PC at the
same time, each in their own session with their own display, audio and controllers,
streamed with Pulsar or Moonlight. Clean-room implementation, part of the Pulsar umbrella
(`../`). No payment, no activation, no telemetry.

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
| Hard rules: license, security, HDR, tooling | [docs/agents/rules.md](docs/agents/rules.md) |
| Phases, scope and decisions taken | [docs/agents/plan.md](docs/agents/plan.md) |
| Our design: crates, IPC, ACL, ports, registry | [docs/agents/architecture.md](docs/agents/architecture.md) |
| Third-party drivers, stream server, RDP libs: verified table + defaults | [docs/agents/third-party.md](docs/agents/third-party.md) |
| Package contracts: crate APIs, IPC additions, integration | [docs/agents/contracts.md](docs/agents/contracts.md) |
| Service integration: seat start order, engine, wiring, IPC v2 | [docs/agents/architecture.md](docs/agents/architecture.md) (Processes, IPC, Seat lifecycle) |
| Package READMEs (build, test, untested parts, notices) | `crates/*/README.md`, `shim/README.md`, `installer/README.md`, `drivers/*/README.md` |
| Pulsar stream backend (`pulsar-host`): launch, isolation, control pipe, pads | [docs/agents/pulsar-host-backend.md](docs/agents/pulsar-host-backend.md) |
| Test coverage: commands, ARM64 workaround, baseline | [docs/agents/coverage.md](docs/agents/coverage.md) |
| Open spec / design questions | [docs/agents/open-questions.md](docs/agents/open-questions.md) |
| Current status / resume point | [docs/agents/status.md](docs/agents/status.md) |
| Dated log of sessions and findings | [docs/journal.md](docs/journal.md) |
| Per-task progress notes | `docs/progress/` |
| Behaviour spec (analyst-written) | [spec/README.md](spec/README.md) |
| Test machine access, local paths (local only) | `docs/local/README.md` |
