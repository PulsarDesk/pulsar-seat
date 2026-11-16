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

No own driver package is shipped today (none is signed yet). A driver package added later
is listed here with its licence text, source URL and pinned hash, and only if it is signed
and redistributable.

## Remote seat kind (third-party payload, from Pulsar Split)

Installed below `<install>\remote\` (merge step 10). Every artefact is pinned by SHA-256 in
`installer/remote/drivers.json` / `stream-server.json` and checked by
`installer/scripts/fetch-deps.ts`. Licence texts are copied unchanged from the upstream
repository at the pinned tag.

| Component | Version | Licence | Shipped how | File | Upstream |
| --- | --- | --- | --- | --- | --- |
| Virtual-Display-Driver (VirtualDrivers) | 25.7.23 | MIT | INF package in `remote\drivers\vdd`, installed only when `PULSARDRIVERS` names it | `VirtualDisplayDriver-MIT.txt` | https://github.com/VirtualDrivers/Virtual-Display-Driver |
| usbip-win2 | 0.9.8.1 | BSD-2-Clause | upstream signed installer in `remote\drivers\usbip-win2`, run only when named in `PULSARDRIVERS` and `PULSARCONSENT` | `usbip-win2-BSD-2-Clause.txt` | https://github.com/vadimgrn/usbip-win2 |
| ViGEmBus (Nefarius) | 1.22.0 | BSD-3-Clause | **not shipped** (detected only; the stream server's bundled ViGEmBus installer is left out); notice kept because the stream server talks to it when present | `ViGEmBus-BSD-3-Clause.txt` | https://github.com/nefarius/ViGEmBus |
| Sunshine (SunshineStream, formerly LizardByte) | v2026.516.143833 | GPL-3.0 | stream server in `remote\stream-server\` (that release's own portable zip per arch), without its browser web UI, its `scripts\` folder and `tools\sunshinesvc.exe` | `Sunshine-GPL-3.0.txt`, `SOURCE-OFFER.txt` | https://github.com/SunshineStream/Sunshine/tree/v2026.516.143833 |
| IronRDP (Devolutions) | connector 0.12, session 0.13 | MIT OR Apache-2.0 | compiled into `pulsar-seat-host.exe` (its `--headless-hold` mode replaced `pulsar-split-rdp-helper.exe` at merge step 7) | `IronRDP-MIT.txt`, `IronRDP-Apache-2.0.txt` | https://github.com/Devolutions/IronRDP |
| DXVK | per manifest | zlib | **not shipped**; downloaded on operator request per game (remote spec 50 §10.2) | `DXVK-zlib.txt` | https://github.com/doitsujin/dxvk |

The per-process shim (`shim\x64\`, `shim\x86\`, and `shim\arm64\` in the ARM64 package) is
our own GPL-3.0-only code. Its ARM64 build links **Microsoft Detours** (commit
`1dbd4ea281d73d478aa8d09a8b83fbd265f715f7`, <https://github.com/microsoft/Detours>) under
the MIT licence:

> Copyright (c) Microsoft Corporation.
>
> MIT License
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this
> software and associated documentation files (the "Software"), to deal in the Software
> without restriction, including without limitation the rights to use, copy, modify,
> merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
> persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED *AS IS*, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
> INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
> PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
> FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
> OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
> DEALINGS IN THE SOFTWARE.
