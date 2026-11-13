# Plan and decisions

Index: [../../AGENTS.md](../../AGENTS.md).

## Phases

| # | Scope | Output |
| --- | --- | --- |
| 0 | Behaviour spec of the reference multi-seat product | `spec/` |
| 1 | Manager app + seat service: create/delete seats and users, ports, start/stop, status | Tauri app + Rust service |
| 2 | Multi-session + per-seat streaming host | |
| 3 | Per-seat virtual display (IDD), HDR via IddCx | |
| 4 | Per-seat input / HID isolation, controllers, audio | |

Each phase works on its own before the next starts.

## Decisions (2026-11-12, maintainer)

- Full scope, phased as above.
- Name **Pulsar Split**; lives under the Pulsar umbrella. Pulsar's existing support for an
  installed third-party multi-seat product **stays**.
- Manager: **Tauri 2 + SvelteKit**; service and drivers in **Rust** where possible.
- Clean room with **two separate agents**: analyst writes `spec/` only, implementer reads
  `spec/` only.
- UI layout may match the reference for now; the maintainer redesigns it later. No
  third-party branding at any point.
- Fully free: no licence keys, activation or paid tiers.
- HDR without kernel patching (see [rules.md](rules.md)).
- **No local web server / browser UI** (2026-11-12). Everything is in the desktop app.
  - Manager ↔ service talk over a **named pipe** with an ACL (Administrators + SYSTEM), not HTTP.
  - The stream server's browser config UI is not used and not exposed. Pairing / PIN entry,
    app lists and settings are done in the manager, which talks to the stream server
    through the service.
  - Only the network ports the Moonlight/Pulsar streaming protocol needs stay open.
  - **Accepted exception (OQ 28 / final-7, maintainer, 2026-11-13):** Sunshine seats on
    the default *upstream* flavour keep its HTTPS web-UI/API listener on base+1. Only the
    service uses it, over loopback (`/api/pin`, client list), with credentials hashed per
    server start, a pinned certificate, `origin_web_ui_allowed = pc` and no firewall rule
    (rules.md "Local services"). The fork flavour (seat pipe, web UI off) stays preferred
    when it exists.
- **Monorepo** (2026-11-12): one repo `pulsar-split` holds everything — unlike Pulsar's
  multi-repo split. Root Cargo workspace + root bun workspace. Layout:
  `crates/` (Rust libs + service), `apps/` (manager, later tools), `drivers/` (IDD, pad
  driver — Phase 3/4), `shim/` (per-process isolation — Phase 4), `installer/`, `spec/`,
  `docs/`. One CI, one version, one release.
- **Multi-session (2026-11-12):** both paths, optional. Default: Windows Server + RDSH
  (licence-clean). Separate **experimental** module for client SKUs (termsrv wrapper),
  off by default, with a clear warning in the manager; built only from public
  open-source knowledge under compatible licences (never from the reference product).
- **Drivers (2026-11-12):** no money for signing now.
  - Default backends = existing **signed, open-source, redistributable** third-party
    drivers (display: an IddCx virtual display driver; pads: ViGEmBus, usbip-win2 for
    DualSense). Verify signing + licence before use; keep their notices.
  - Proprietary drivers from other products (e.g. ASTER) are **not** used or shipped.
  - Our own drivers (IDD with IddCx HDR, pad driver) are written under `drivers/` behind
    the same backend traits, but **not loaded/tested until signed** (maybe later).
    No test-signing on the test PC for now.
- **Stack versions:** latest Tauri 2, latest SvelteKit, latest Svelte 5 (runes),
  `adapter-static` SPA inside Tauri (no SSR, no dev server in production).

## Known risks (to resolve before a public release)

- **Driver signing:** an IDD / other drivers need signing to load with Secure Boot on.
  Free options: test-signing mode (bad UX). Proper: attestation signing (paid EV cert).
- **Multi-session on client Windows SKUs** is restricted by Microsoft's licence terms;
  distributing a termsrv wrapper is a grey area. Decide the approach in Phase 2.

- **Pro/Home multi-session (2026-11-12, maintainer):** write our **own** termsrv wrapper DLL (Rust, x64; in-memory patch like RDP Wrapper), derived only from RDP Wrapper's public Apache-2.0 source + a pinned, hash-checked community `rdpwrap.ini` for offsets (attribution kept). Never from the reference product. Fail-closed on unknown `termsrv.dll` versions, journaled + reversible, experimental, off by default, warning in the manager. Replaces 'operator-installed RDP Wrapper only'. Start after workflow `wf_e22fa7f9-77a` (avoids file overlap).
- **Wrapper default ON (2026-11-12, maintainer):** 'installing the app is accepting the risk'. On client SKUs (Pro/Home) the own termsrv wrapper is enabled by default; never used on Server + RDSH. Still fail-closed: unknown `termsrv.dll` version = no patch, manager shows 'unsupported Windows build'. One-time notice at install (Microsoft licence terms, Defender may flag the DLL) instead of a per-toggle risk checkbox; manager keeps an off switch. Supersedes 'experimental, off by default' for this module.
