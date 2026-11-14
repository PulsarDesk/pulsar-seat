# Merge plan: Pulsar Seat + Pulsar Split (JOINT v1)

Index: [../../AGENTS.md](../../AGENTS.md). Status: **joint plan, 2026-11-14, agreed by the
Seat and Split sessions, approved by the maintainer (§5). Steps 0-10 done (2026-11-14); step
11 (hardware tests) is the maintainer's.** Results per step: [status.md](status.md).

Inputs:
- Seat side: this file's earlier draft (2026-11-13).
- Split side: [../split/agents/merge-plan-split.md](../split/agents/merge-plan-split.md) (Split commit `efb328a`, imported under `split/` in e26aa4b and moved to `docs/split/` at step 2-3).
  That file has the full Split package, pipe, config and installer inventory. This file
  does not repeat it.

Decision source: plan.md "Merge with Pulsar Split into ONE product".

## 1. Goal

One product (name: maintainer picks; placeholder `<prod>`), one installer, one service, one
manager. Every seat has a kind:

| Kind | What it is | Comes from |
| --- | --- | --- |
| `local` | physical monitors, keyboards, mice, audio, USB ports of this PC | pulsar-seat |
| `remote` | virtual display + virtual pads, streamed to Moonlight / Pulsar | pulsar-split |

Both kinds run side by side on one PC.

## 2. Agreed by both sessions

- **Base repo: pulsar-seat.** Split is folded in with full history. Reasons:
  - Seat's service already serves many client kinds through an access table.
  - Seat has the release key, the update path, the WDK driver build and the credential
    provider.
  - Seat's config is one versioned JSON document.
  - Split's tested packages (stream, pads, shim, display, setup) move in mostly unchanged.
- **Crate prefix `<prod>-`**, renamed once at step 3. Do not use `ms-` (reads as
  Microsoft). Kind-only crates keep a kind word (`<prod>-stream`, `<prod>-host`).
- **Config:** Seat's JSON document. `SeatConfig.kind = local | remote` with exactly one of
  `local: LocalSeat` / `remote: RemoteSeat` set. Old Seat documents load as `kind = local`.
- **IPC:** one control pipe, `PROTOCOL_VERSION = 3` (above Seat v1 and Split v2), `hello`
  first, Seat's `Access` table. Split requests are added as `Access::Admin`. A seat-scoped
  request on the wrong kind fails with `wrong_seat_kind`. Split's shim health pipe stays.
  Seat's `subscribe` topics plus a new `stream` topic.
- **Service:** Seat's skeleton (boot sequence, safe mode, crash loop). Split's power-event
  and session-change handling is ported in. Split's `engine/` becomes the remote seat
  worker.
- **Secrets:** a `SecretStore` trait with LSA private data as the target, after checking
  LSA limits for per-seat Pulsar secrets (HYPOTHESIS: size / count limits).
- **Sessions:** one `rds_loopback` backend behind Seat's `SessionBackend`. One IronRDP
  crate (`seat-rdp` base). Split's headless helper becomes a headless mode of it (no frame
  sink; password still over stdin, never argv). One client-SKU wrapper integration as a
  backend kind. During migration, the startup sweep recognises both prefixes (`PSEAT-NN`,
  `PSS…`).
- **U1 / U3** are local-only. A remote seat needs concurrent sessions (Server + RDSH, or
  the wrapper kind). Without them, the service refuses remote seats with
  `session_unavailable`.
- **Pads + per-process HID shim:** shared crates from Split. `seat-input` stays for local
  keyboard, mouse and HID routing and can feed a seat-owned virtual pad. This closes Seat's
  "gamepads per seat" gap.
- **Display:** both repos' backends behind one trait for now. Later, one own IddCx driver
  so that it is signed once.
- **Audio:** merge endpoint discovery. Routing stays per kind.
- **Journals:** one record format. Uninstall replays both old formats.
- **Manager:** Seat's shell plus Split's pages (remote seat editor, host, pairing, logs,
  DXVK). One `i18n.ts`.
- **Installer:** one MSI. It keeps Seat's signing and update gates and adds Split's pinned
  dependency fetch and payload rules. It also removes a leftover `PulsarSplit` /
  `PulsarSeat` service.
- **Spec:** `spec/local/` and `spec/remote/`, analyst material kept apart. One
  CLEANROOM.md.

## 3. Migration steps

Every step ends with all gates green:
- `cargo test --workspace`
- clippy with 0 warnings on aarch64 and x86_64
- `bun run test` / `check` / `build`
- the `shim/` workspace tests
- `bun test installer/scripts`
- PE machine checks of the artefacts

Test counts may only go up, except for tests a step deletes on purpose (listed in the step).

| # | Step |
| --- | --- |
| 0 | Maintainer makes the first commit of pulsar-seat. Freeze both repos and record test counts. |
| 1 | Joint contract delta (docs only): seat model with `kind`, protocol v3 list, `SecretStore`, `SessionBackend` for remote seats. |
| 2 | Import Split into the Seat repo with full history (`git mv` on a Split branch, then `merge --allow-unrelated-histories`). Old names, two services, two managers. |
| 3 | Rename crates, pipes, service, data dir to `<prod>` (mechanical, behind constants). |
| 4 | Merge proto + core: `kind`, sub-structs, schema migration, protocol v3. |
| 5 | Secrets: `SecretStore`, LSA impl (DPAPI reader only if migration is wanted). |
| 6 | One service: Split engine as the remote seat worker; one pipe. |
| 7 | Session layer: one `rds_loopback`, one IronRDP crate with a headless mode, one wrapper kind. |
| 8 | Audio, then pads + shim (local pads behind a feature flag), then display traits. |
| 9 | One manager. |
| 10 | One MSI (x64 + ARM64). |
| 11 | Hardware tests per kind, then mixed (one local + one remote seat). The maintainer runs them. |

## 4. Risks

- Neither stack has run on hardware, so merging can hide where a failure comes from. If
  possible, run a hardware test per side before step 6.
- Both RDS loopback implementations write the same Terminal Server values. Seat also adds
  a second listener with NLA off. These writes need one journal and a fixed order.
- The sweep could log off the other kind's sessions while the prefixes still differ.
- Split's `shim/` is x64 / x86 only, so remote per-process isolation stays off on ARM64.
- Renames change ACL-checked paths. A half-migrated tree must be refused.
- `pulsar-host` pipe name: change it before `pulsar-host` is built.
- Mixed line endings in Split. Add `.gitattributes` at the rename step.

## 5. Maintainer decisions (2026-11-14: all taken)

The maintainer approved the plan and accepted every recommendation below. The questions
stay listed for the record.

- **Name: Pulsar Seat.** The repo stays `pulsar-seat` and crates keep the `seat-` prefix
  (`<prod>` = `seat`). At step 3, Split crates are renamed `split-*` to `seat-*`; where a
  name clashes (proto, core, service, session, display), the step 4–8 merge resolves it.
  Service, pipe and data dir keep the Seat names.
- Base repo: pulsar-seat. Secrets: LSA for everything. One IronRDP client binary. One own
  IddCx driver. No migration of existing installs, so no HKLM / DPAPI importer. Remote seat
  status is admin only. `pulsar-host` and per-seat pipes stay separate. Local pads come at
  step 8 from the shared pads crate.
- Code merge starts now, after the first commit (step 0).
- From the Split session: pulsar-desktop has `--split-seat` (local commits ddc0403 /
  15a685e, not pushed), the remote-seat stream host. Keep its pipe contract
  ([../split/agents/pulsar-host-backend.md](../split/agents/pulsar-host-backend.md)) unchanged when the service moves.
  Only the pipe-name prefix may change, and only together with pulsar-desktop.

Original questions:

1. Product name, repo name, service name, install and data paths.
2. Base repo: pulsar-seat (both sessions recommend it) or a fresh repo.
3. Secrets: LSA for everything, including the Pulsar identity and relay key?
4. One IronRDP client binary for both kinds (recommended) or a separate headless helper?
5. One own IddCx driver for both kinds (recommended: signed once) or two?
6. Migrate existing installs? Neither repo is published. Recommendation: no, which skips
   the HKLM / DPAPI importer.
7. May standard users read the status of remote seats (Seat's IU access), or admins only?
8. Must remote seats coexist with U1 / U3 on client SKUs? Today that is impossible without
   the wrapper.
9. Pads for local seats in the shared pads crate at step 8 (Seat option B)? Recommended:
   yes.
10. One pipe for manager and stream control, or keep the `pulsar-host` / seat pipes
    separate? Split recommends separate (secrets stay on a per-launch pipe).
