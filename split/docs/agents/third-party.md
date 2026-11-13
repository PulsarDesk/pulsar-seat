# Third-party components

Index: [../../AGENTS.md](../../AGENTS.md). Decision context: [plan.md](plan.md) (Drivers,
2026-11-12). Install-side facts: `spec/90-install.md` §3.

Research date **2026-11-12** (web only: upstream READMEs, GitHub REST API, upstream docs,
crates.io). No binary was downloaded or opened. "Signing" cites what upstream *says*;
before any package is bundled, check it on the clean ARM64 test laptop (Memory Integrity
on) with `Get-AuthenticodeSignature` / `signtool verify /pa /v` on the `.cat` and the
signer chain. Mark that check in this file. Unverified claims are marked `HYPOTHESIS`.

Selection rules (from [rules.md](rules.md) / [plan.md](plan.md)): open source under a
GPL-3-compatible licence, redistributable, signed so it loads with Secure Boot and Memory
Integrity **on**, no test-signing, no root certificate installed by us or by the package,
no licence/activation, no HTTP API of ours, x64 + ARM64 wanted.

**Excluded outright:** proprietary drivers from other products (ASTER-like multi-seat
drivers, Parsec's closed driver binary), commercial/activation-gated frameworks
(Nefarius VirtualPad, LizardByte libvirtualhid Windows driver), anything that needs
test-signing, a vulnerable driver or a kernel patch.

## Recommended defaults

| Role | Default | Fallback / later | Why |
| --- | --- | --- | --- |
| Virtual display (HDR via IddCx) | **Virtual-Display-Driver** (VirtualDrivers, MIT) | own IddCx driver in `drivers/` once signed | Only candidate that is open source, permissively licensed, signed by a public CA (SignPath Foundation), ships x64 **and** ARM64, does HDR10/12 through IddCx, and is still active. |
| Xbox 360 / DS4 pads | **ViGEmBus 1.22.0** (BSD-3), *use if installed*, do not bundle by default (spec 90 §3.2) | own pad driver; usbip path below | Microsoft-signed (WHCP), x64+x86+ARM64, the API the stream server already calls. Retired upstream: no fixes ever. |
| DualSense / DS4 with haptics, triggers, touchpad | **usbip-win2** (BSD-2) + our own user-mode USB/IP device emulator | — | Active (0.9.8.1, 2026-09-27), WHLK/attestation signed per README with a Microsoft Hardware Certification Report, x64 + ARM64 installers. Real USB descriptors, so games see a real DualSense. |
| Stream server | **upstream Sunshine** (LizardByte, GPL-3), pinned tag (spec 80) | ideas (not binaries) from Apollo, under GPL-3 | Active (release 2026-11-12), Windows x64 **and** ARM64 builds, one process per seat via config-file argument + `port`, documented `/api/pin`. Apollo is x64-only, slower, and depends on SudoVDA (rejected). |
| Headless RDP client (create/keep seat sessions on RDSH) | **IronRDP** (MIT OR Apache-2.0, Rust crates) | FreeRDP 3 (Apache-2.0, C) via FFI | Rust, sans-I/O, in-process → password never leaves our process; CredSSP/NLA (NTLM, Kerberos). FreeRDP has broader feature coverage if IronRDP lacks something. |

## Virtual display

| Field | Virtual-Display-Driver | SudoVDA (Apollo) | Parsec VDD | LuminalVGD |
| --- | --- | --- | --- | --- |
| Upstream | https://github.com/VirtualDrivers/Virtual-Display-Driver | https://github.com/SudoMaker/SudoVDA (bundled in https://github.com/ClassicOldSong/Apollo) | driver by Parsec; open wrapper https://github.com/nomi-san/parsec-vdd | https://github.com/NortheBridge/LuminalVGD |
| Licence | MIT (GitHub SPDX) | GitHub SPDX **none**; README: "MIT and CC0 or Public Domain" for author changes, Microsoft parts under Microsoft's licence | wrapper MIT; **driver binary proprietary, redistribution terms not stated** | AGPL-3.0 (SPDX) |
| Latest version | release 25.7.23 (2025-07-23); repo pushed 2026-09-19 | no releases in repo; last push 2025-04-14; shipped inside Apollo 0.4.6 | driver 0.45 (wrapper v0.45.1, 2024-04-30) | not checked (repo pushed 2026-09-14) |
| Architectures | x64, ARM64 (release assets `VirtualDisplayDriver-ARM64…`, x86). README footnote: ARM64 on Win11 24H2+ "may require test signing" | x64 (Apollo ships x64 only); ARM64 not mentioned | x64; ARM64 not mentioned | Windows 11 IddCx; arch not checked |
| Signing (evidence) | Authenticode via **SignPath Foundation** certificate (README, wiki); release 25.7.23 notes "signed" driver + control app. Microsoft attestation not mentioned. `HYPOTHESIS`: publicly trusted CA is enough for a UMDF/IddCx package on x64; ARM64 24H2 may need a Microsoft signature (footnote) | **self-signed**: Apollo `src_assets/windows/drivers/sudovda/install.bat` imports `sudovda.cer` into LocalMachine **Root** and **TrustedPublisher** via certutil; Apollo 0.4.0 notes "changed code signature to last 5 years", old one expired 2025-08-19 | wrapper README marks driver "signed"; signer not stated | Authenticode by "NortheBridge Foundation", RFC 3161 timestamp (third-party release notes; unverified) |
| Redistributable | yes (MIT; keep notice) | unclear (no licence file metadata) | no clear grant → **no** | yes under AGPL-3 (ship source) |
| Control interface | `C:\VirtualDisplayDriver\vdd_settings.xml` + named pipe `\\.\pipe\MTTVirtualDisplayPipe` (source `MttVDD/Driver.cpp`): `PING`, `RELOAD_DRIVER`, `SETDISPLAYCOUNT n`, `GETSETTINGS`, `SETGPU name`, `GETALLGPUS`, `GETASSIGNEDGPU`, `IDDCXVERSION`, `D3DDEVICEGPU`, `HDRPLUS`, `SDR10`, `CUSTOMEDID`, `PREVENTSPOOF`, `CEAOVERRIDE`, `HARDWARECURSOR`, `LOGGING`, `LOG_DEBUG`. Pipe DACL `D:(A;;GA;;;WD)` = **Everyone full access**. No per-monitor add/remove: only a global count; one shared EDID for all monitors; per-monitor `ContainerId` GUID | registry `HKLM\SOFTWARE\SudoMaker\SudoVDA` (reload/reboot to apply); Apollo adds/removes one display per client | IOCTLs on the device: add (returns index), remove, periodic ping (displays unplug after ~1 s without ping); max 16 per adapter; modes via Win32 display API | not checked |
| HDR | SDR 8/10-bit, HDR 10/12-bit, Win11 23H2+ | full on Win11 24H2, partial 23H2, none Win10; `hdrBits` 10/12 | **no** (EDID hack only) | not checked |
| Maintenance | active (205 open issues; former lead left) | **unmaintained** (no push since 2025-04; downstream LuminalShine calls it unreliable) | driver frozen since 0.45 | active, young (0.1.0-alpha) |
| Verdict | **default** | **rejected**: installs a root certificate (rule), no licence metadata, unmaintained | **rejected**: proprietary binary, no HDR | watch only |

Integration notes for VDD:
- The Everyone-writable pipe lets any seat user change display count, GPU or reload the
  driver for all seats. Mitigation: our service is the only owner of the settings file and
  restores state on change; we cannot change the driver's DACL without forking. See
  open question 19.
- `SETDISPLAYCOUNT` is global; per-seat display identity has to come from our mapping
  (monitor `ContainerId` / target id → seat). Our own IddCx driver in `drivers/` is the
  long-term path for per-seat add/remove and per-seat EDID.
- `HYPOTHESIS`: IddCx monitors attach to the console session's display stack; an RDSH
  session gets the Remote Display Adapter instead. How a seat session uses an IddCx
  monitor is a Phase 3 design point (spec 40). Open question 20.

## Pads

| Field | ViGEmBus | usbip-win2 | libvirtualhid (LizardByte) | Nefarius VirtualPad |
| --- | --- | --- | --- | --- |
| Upstream | https://github.com/nefarius/ViGEmBus | https://github.com/vadimgrn/usbip-win2 | https://github.com/LizardByte/libvirtualhid | https://docs.nefarius.at/projects/VirtualPad/ |
| Licence | BSD-3-Clause | BSD-2-Clause | library MIT; **Windows UMDF driver + broker LB-SAL 1.0 (source-available)** | commercial, business partners only |
| Latest version | v1.22.0 (2023-11-02), final | v.0.9.8.1 (2026-09-27) | not checked | n/a |
| Architectures | x86, x64, ARM64 (single installer `ViGEmBus_1.22.0_x64_x86_arm64.exe`; README: 1.17+ builds ARM64) | x64, ARM64 (`USBip-0.9.8.1-arm64.exe`, `-x64.exe`) | Windows **AMD64 only** (ARM64 needs dashboard signing they lack) | not stated |
| Signing (evidence) | README: pre-built Win10/11 binaries "production-signed"; spec 90 §3.2 observed signer *Microsoft Windows Hardware Compatibility Publisher* (package 1.21.442) | README: "WHLK certified (or attestation signed)" via OSSign; release 0.9.8.1 links a Microsoft Hardware Certification Report; local builds use test cert `usbip.pfx` (never ship) | release: Azure Trusted Signing; PR builds self-signed test cert imported by installer | n/a |
| Redistributable | yes (BSD-3; keep notice) | yes (BSD-2; keep notice) | **no** for our use: driver needs a paid key, 5 activations, daily online validation | no |
| Control interface | ViGEm client API (IOCTLs on bus device `Nefarius\ViGEmBus\Gen1`); Xbox 360 + DS4 targets only, no DualSense | `usbip.exe` CLI (`attach -r host -b busid`, `detach`, `list -r`) to a USB/IP server over TCP (default 3240) | C++ runtime API (`Runtime::create`, `create_gamepad(profiles::dualsense())`) + broker service | n/a |
| Maintenance | **retired**: archived 2023-11-02 after "ViGEm" trademark conflict; EOL statement: no updates, existing installs keep working, old updater contacts the lost domain | active (1,985 commits; 0.9.8.x 2026; note: 0.9.8.1 is a large AI-assisted refactor, author warns of regressions) | active | n/a |
| Verdict | **default for X360/DS4** if present; do not bundle by default | **default for DualSense** | **excluded** (licence/activation) | **excluded** |

Integration notes:
- Sunshine itself (config `gamepad_driver`) offers `vigembus`, `virtualhid` (= the paid
  libvirtualhid driver, no fallback) or `all`. Seats must set `gamepad_driver = vigembus`
  so nothing ever asks for a licence.
- The usbip-win2 path needs a USB/IP **server** that presents a virtual DualSense. We
  write it ourselves (user mode, Rust); it is a raw TCP protocol, not HTTP. Bind to
  loopback only, reject remote peers. Open question 21.
- Pin `usbip-win2` to a tested version; 0.9.8.1 has a known serial-number regression.

## Stream server

| Field | upstream Sunshine | Apollo |
| --- | --- | --- |
| Upstream | https://github.com/LizardByte/Sunshine | https://github.com/ClassicOldSong/Apollo |
| Licence | GPL-3.0 | GPL-3.0 (Sunshine fork) |
| Latest version | v2026.1112.40957 (2026-11-12); spec 80 pins `v2026.516.143833` | v0.4.6 (2025-07-13), pre-release v0.4.7-alpha.1 (2025-08-12); repo pushed 2026-05-21 |
| Architectures | Windows AMD64 **and ARM64** (MSI + lite zip) | Windows x64 `.exe` only |
| Signing | not checked for the Windows MSI (`HYPOTHESIS`: SignPath project "sunshinefoundation" exists). We build from source anyway (GPL corresponding source) | not checked |
| Config | `sunshine.exe <full path to .conf>`; `port` (default 47989) offsets all other ports; per-file `file_apps`, `credentials_file`, `file_state`, `log_path`; `origin_web_ui_allowed = pc`; `output_name` = display `device_id` GUID; `dd_*` display-device options | same keys; wiki "multiple instances": one process per `.conf`, distinct `port`, `file_state`, `log_path`, `sunshine_name`, run as SYSTEM |
| Pairing PIN API | HTTPS web-UI port (`port+1`), Basic auth, `POST /api/pin` body `{"pairing_id", "pin", "name"}` (blocks until `ping_timeout`); `/api/clients/list`, `/api/clients/unpair`, `/api/clients/update`; CSRF token needed only from browsers | first client gets full permissions, later clients limited until granted in its web UI; client-driven virtual display via Artemis (Android). API body not checked |
| Per-session use | one instance per seat (our design: seat port block, `architecture.md`). Session placement (seat session vs session 0) is Phase 2 | same model; README warns it "will no longer be compatible with OG Sunshine and OG Moonlight eventually" |
| Virtual display | none built in; works with any IddCx display (`output_name`) | built-in SudoVDA (rejected above) |
| Maintenance | active, frequent tagged releases | single maintainer, 346 open issues, no stable release since 2025-07 |
| Verdict | **default** | rejected as base; per-client display identity and permission ideas may be reimplemented (GPL-3, keep notices if code is reused) |

Integration notes:
- The PIN must reach `/api/pin` on the seat's `port+1` from our service, with web-UI
  credentials our service generates and keeps (DPAPI, never argv/env/log). This is the
  stream server's own listener, loopback only, firewall closed; we add no HTTP server.
  See open questions 4, 5 and 22 (`pairing_id`).
- Rebase to a newer upstream tag only deliberately (spec 80).

## Headless RDP client

| Field | IronRDP | FreeRDP |
| --- | --- | --- |
| Upstream | https://github.com/Devolutions/IronRDP | https://github.com/FreeRDP/FreeRDP |
| Licence | MIT OR Apache-2.0 | Apache-2.0 |
| Latest version | crate `ironrdp` 0.17.0 (2026-07-10) | 3.33.1 (2026-11-10) |
| Architectures | anything Rust targets (x64, ARM64 Windows) | C; Windows builds via MSVC/MinGW; ARM64 Windows `HYPOTHESIS` (not stated) |
| Signing | n/a (we compile it into our signed binary) | n/a (built from source) |
| Redistributable | yes | yes (Apache-2.0 is GPL-3 compatible) |
| Control interface | Rust library, sans-I/O connector (X.224, MCS, capabilities, licensing, reactivation), CredSSP NTLM/Kerberos, EGFX PDUs, RDPSND, RDPDR; `ironrdp-agent` CLI exists but takes `--password` (forbidden, use the library) | C library `libfreerdp` + clients (`/p:` on argv is forbidden; use the library or `/from-stdin`) |
| HDR | n/a | n/a |
| Maintenance | active (2,200+ commits, fuzzed) | very active (monthly 3.x releases, many CVE fixes in 2026) |
| Verdict | **default** | fallback |

## Licence notices

Every bundled component keeps its licence text in the install folder and in the manager's
About screen (spec 90 §7): VDD (MIT), ViGEmBus (BSD-3, if ever bundled), usbip-win2
(BSD-2), Sunshine (GPL-3, ship corresponding source), IronRDP (MIT/Apache-2.0).
