# Pulsar Seat — start here

Free, open-source (GPL-3.0) **multi-seat** for Windows: one PC, several people, each in
their own Windows session at the same time. Every seat has a kind:

- `local`: monitors, keyboards, mice, audio devices and USB ports plugged into the PC.
- `remote`: a virtual display and virtual pads, streamed with Pulsar or Moonlight. This kind
  comes from Pulsar Split, which is being merged into Pulsar Seat
  ([docs/agents/merge-plan.md](docs/agents/merge-plan.md)).

Clean-room implementation, part of the Pulsar umbrella (`../`). Runs natively on **x64 and
ARM64** (the remote kind's per-process shim is x64 / x86 only). No payment, no activation,
no telemetry.

Until the merge is done, remote-kind code keeps its own docs under `docs/split/`. Its
`split-*` crates are merged into the `seat-*` crates (steps 4-8, done) and its manager into
`apps/manager` (step 9, done); installer pieces follow at step 10.

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
| Merge with Pulsar Split: joint plan (seat kinds local / remote, shared crates, steps) | [docs/agents/merge-plan.md](docs/agents/merge-plan.md) |
| Release checklist (maintainer steps, first commit, keys, signing) | [docs/release/checklist.md](docs/release/checklist.md) |
| Current status / resume point | [docs/agents/status.md](docs/agents/status.md) |
| Dated log of sessions and findings | [docs/journal.md](docs/journal.md) |
| Per-task progress notes | `docs/progress/` |
| User troubleshooting and offline recovery (Turkish) | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Credential provider + filter (fast user switch auto-logon) | [credprov/README.md](credprov/README.md) |
| Behaviour spec (analyst-written): `spec/local/` (local kind), `spec/remote/` (remote kind) | [spec/README.md](spec/README.md) |
| Test machine access, local paths (local only) | `docs/local/README.md` |

### Remote kind (from Pulsar Split, until the merge is done)

| Topic | File |
| --- | --- |
| Split rules (HDR, stream servers, third-party pins) | [docs/split/agents/rules.md](docs/split/agents/rules.md) |
| Split phases, scope and decisions | [docs/split/agents/plan.md](docs/split/agents/plan.md) |
| Split design: crates, IPC v2, ACL, ports, registry, seat lifecycle | [docs/split/agents/architecture.md](docs/split/agents/architecture.md) |
| Split package contracts: crate APIs, IPC additions, integration | [docs/split/agents/contracts.md](docs/split/agents/contracts.md) |
| Third-party drivers, stream server, RDP libs: verified table + defaults | [docs/split/agents/third-party.md](docs/split/agents/third-party.md) |
| Pulsar stream backend (`pulsar-host`): launch, isolation, control pipe, pads | [docs/split/agents/pulsar-host-backend.md](docs/split/agents/pulsar-host-backend.md) |
| Merge inventory, Split side (packages, IPC, config, overlap) | [docs/split/agents/merge-plan-split.md](docs/split/agents/merge-plan-split.md) |
| Test coverage: commands, ARM64 workaround, baseline | [docs/split/agents/coverage.md](docs/split/agents/coverage.md) |
| Open spec / design questions | [docs/split/agents/open-questions.md](docs/split/agents/open-questions.md) |
| Split status and journal until the import (history, not updated) | [docs/split/agents/status.md](docs/split/agents/status.md), [docs/split/journal.md](docs/split/journal.md) |
| Remote seat pages of the one manager (from Split's manager, step 9) | `apps/manager/src/lib/remote/` ([apps/manager/README.md](apps/manager/README.md)) |
| Remote installer pieces (temporary, merged at step 10) | [installer/split/README.md](installer/split/README.md) |
| Per-process HID shim (separate x64 / x86 workspace) | [shim/README.md](shim/README.md) |
| Package READMEs (build, test, untested parts, notices) | `crates/*/README.md`, `drivers/*/README.md` |

