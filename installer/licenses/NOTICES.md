<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Third-party notices

Pulsar Seat ships only its own binaries (GPL-3.0-only, see `SOURCE-OFFER.txt`). They are
built with third-party code whose licences require their texts to travel with the
binaries. The release build (CI `installers` job, step "Third-party notices") writes them
next to this file **before** `tauri build`, and the job fails if either is missing:

| File | Content | Made by |
| --- | --- | --- |
| `THIRD-PARTY-NOTICES-RUST.md` | every Rust crate in the service, host, agent, manager (Tauri runtime included) and app API helper DLL, with licence texts | `cargo about` (pinned version, `about.toml` + `about.hbs`) |
| `THIRD-PARTY-NOTICES-JS.md` | the JavaScript packages bundled into the manager frontend (`@tauri-apps/api`, svelte, SvelteKit client runtime and their dependencies) | `bun installer/scripts/js-notices.ts` |

Both files are generated, not tracked. A local build that skips the step ships without
them: do not distribute such a build.

The seat host's RDP client is built on **IronRDP** (Devolutions Inc. and contributors,
MIT OR Apache-2.0, https://github.com/Devolutions/IronRDP; including its graphics
pipeline (`ironrdp-egfx`, which also credits Teleport), clipboard and device-redirection
crates and their Windows back-ends) and sspi-rs (MIT OR Apache-2.0). They are linked
statically into `pulsar-seat-host.exe`, and their licence texts are part of
`THIRD-PARTY-NOTICES-RUST.md`. The decision is in `crates/seat-rdp/README.md`.

H.264 is decoded by Windows Media Foundation (part of Windows); no OpenH264 / Cisco
binary is shipped, and the optional OpenH264 support of `ironrdp-egfx` is not enabled.

The WebView2 runtime is not shipped: the installer downloads Microsoft's bootstrapper on
demand.

No third-party driver is shipped today. A driver package added later is listed here with
its licence text, source URL and pinned hash, and only if it is signed and redistributable.
