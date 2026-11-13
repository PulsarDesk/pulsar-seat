# Third-party notices

Installed to `%ProgramFiles%\Pulsar Split\licenses\` and shown in the manager's About
screen (spec 90 §7). Pulsar Split itself is GPL-3.0-only (`LICENSE` in the repository).
Licence texts are copied unchanged from the upstream repository at the pinned tag.

| Component | Version | Licence | Shipped how | File | Upstream |
| --- | --- | --- | --- | --- | --- |
| Virtual-Display-Driver (VirtualDrivers) | 25.7.23 | MIT | INF package in `drivers\vdd` (x64 default; ARM64 opt-in) | `VirtualDisplayDriver-MIT.txt` | https://github.com/VirtualDrivers/Virtual-Display-Driver |
| usbip-win2 | 0.9.8.1 | BSD-2-Clause | upstream signed installer in `drivers\usbip-win2`, run only with consent | `usbip-win2-BSD-2-Clause.txt` | https://github.com/vadimgrn/usbip-win2 |
| ViGEmBus (Nefarius) | 1.22.0 | BSD-3-Clause | **not shipped** (detected only); notice kept because the stream server talks to it when present | `ViGEmBus-BSD-3-Clause.txt` | https://github.com/nefarius/ViGEmBus |
| Sunshine (SunshineStream, formerly LizardByte) | v2026.516.143833 | GPL-3.0 | stream server in `stream-server\` (that release's own portable zip), browser web UI removed | `Sunshine-GPL-3.0.txt`, `SOURCE-OFFER.txt` | https://github.com/SunshineStream/Sunshine/tree/v2026.516.143833 |
| IronRDP (Devolutions) | connector 0.12, session 0.13 | MIT OR Apache-2.0 | compiled into `pulsar-seat-host.exe` (its `--headless-hold` mode replaced `pulsar-split-rdp-helper.exe` at merge step 7) | `IronRDP-MIT.txt`, `IronRDP-Apache-2.0.txt` | https://github.com/Devolutions/IronRDP |
| DXVK | per manifest | zlib | **not shipped**; downloaded on operator request per game (spec 50 §10.2) | `DXVK-zlib.txt` | https://github.com/doitsujin/dxvk |

Microsoft components: none, except the WebView2 bootstrapper the Tauri bundler downloads
or embeds under Microsoft's WebView2 distribution terms (spec 90 §2.3).
