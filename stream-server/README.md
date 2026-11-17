# Pulsar build of the stream server (Sunshine patch set)

Upstream Sunshine (GPL-3.0, LizardByte / SunshineStream) at the commit pinned in
[`sunshine.json`](sunshine.json), plus the patches in [`sunshine-patches/`](sunshine-patches/),
built with upstream's documented MSYS2 toolchain. Behaviour spec: `spec/remote/80-stream-server.md`
(§0, §3.4–§3.6, §4.2, §8.4) and `spec/remote/40-display-hdr.md` §4.4 / §6.4 (frame channel and
mode path; §6.4 supersedes the object names and ACL of 80 §3.4). The service side is
`crates/seat-stream` (flavour `pulsar_fork`).

No upstream source is vendored here: only the patches. The build fetches the pinned commit by
SHA. Shipping this build means offering its corresponding source (GPL-3 §6): the upstream tag
plus this folder.

## Patches (apply in name order; `git apply --check` against `14ffa6fd`)

| Patch | What |
| --- | --- |
| `0001` seat control pipe and web UI switch | config keys `pulsar_seat_id`, `pulsar_web_ui`, `pulsar_control_pipe`; `src/pulsar/control.{h,cpp}`: `\\.\pipe\PulsarSplit\seat\<seat-id>\server`, created before any network listener, first instance, no remote clients, DACL `D:P(A;;GA;;;SY)`, client must be SYSTEM (impersonation check), u32-LE + JSON frames (1 MiB cap), `hello` first. Requests `status`, `pair.submit` (PIN + client name; answers with the new client's uuid once the client's last pairing step passed, or `wrong_pin` / `no_pending_request`), `pair.cancel`, `clients.list` / `clients.unpair` / `clients.unpairAll`, `apps.reload` (re-reads `apps.json`; a running app keeps running), `session.close`, `server.shutdown`, `log.level`. Events `pair.pending`, `pair.ended`, `stream.started`, `stream.ended`, `capture.error`, `audio.state`. One pending `/pair` request at a time (a new one cancels the old one). `pulsar_web_ui = disabled`: the HTTPS config server on base+1 is never created and no web-UI credentials are read. A seat server shows no tray icon; per-seat certificates get the subject `Pulsar Seat Stream Host`. |
| `0002` pairing timeout | `pulsar_pair_timeout` (10–600 s, default 120): a pending request older than that is failed towards the client (`pair.ended` `timeout`). |
| `0003` `pulsar_idd` capture | `capture = pulsar_idd`: the seat's frame channel of our IddCx driver (`Global\PulsarSplit.Display.<seat>.Header` / `.FrameReady` / `.Tex<gen>.<i>` / `.Fence<gen>`, header `PSDH` v1 as `drivers/seat-idd/public/PulsarIddShared.h`). Textures opened by name on the header's render LUID (no cross-adapter copy), shared fence opened through D3D12 by name and waited on the encoder context, `consumerFrame` / `consumerHeartbeat` written, producer heartbeat > 3 s = gone (stream kept 10 s, then `capture.error producer_lost`), HDR metadata from the header, hardware and software encoder variants, `display_names()` = the one channel. Client launch / resume asks the service for the mode (`display.mode.request` → `display.mode.result`, 5 s), encoder probes never do. |
| `0004` `pulsar_pad` pads | `gamepad = pulsar_ds4 | pulsar_ds5 | pulsar_ds5e | pulsar_xone` (and `auto` without ViGEmBus on a Pulsar seat: DualSense for PlayStation clients, Xbox otherwise). `pad.wanted` / `pad.released` events, `pad.attach` / `pad.denied` / `pad.detach` requests; USB-layout input reports (DS4 / DualSense / Edge 0x01, our Xbox HID 0x01) into `<channel>_In` (seqlock, protocol 1) + `<channel>_InEvt`; output ring `<channel>_Out` back to the client as rumble, trigger rumble, adaptive triggers and lightbar. Touch, motion (accel 8192/g, gyro 16/°/s) and battery. |
| `0005` build option to skip the web UI | `SUNSHINE_BUILD_WEB_UI=OFF` leaves out the npm target (no Node.js / npm in the build). |
| `0006` build with OpenSSL 4 | MSYS2 ships OpenSSL 4.1 (rolling): `ASN1_STRING` accessors and a new `X509_NAME` for the generated certificate (still builds with OpenSSL 3). |

Capabilities in `hello`: `control_pipe`, `web_ui_off`, `apps_reload`, `pair_cancel`,
`client_name`, `audio_state`, `session_close`, `graceful_shutdown`, `log_level`,
`pair_timeout`, `pulsar_idd`, `pulsar_pad`. The service uses a feature only when the server
reports it: a build without `pulsar_idd` / `pulsar_pad` / `pair_timeout` gets a config with the
stock capture / `auto` pads / no timeout key (one restart, `host_warning` `stream_fallback`); a
build without `apps_reload` gets the restart-when-idle path; a Pulsar build that never opens
its seat pipe is driven like upstream (loopback web UI, per-start credentials).

## Build

```
powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Check          # fetch + git apply --check only
powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Arch x64       # UCRT64
powershell -ExecutionPolicy Bypass -File stream-server\build-sunshine.ps1 -Arch arm64     # CLANGARM64
```

Options: `-Msys2Root C:\msys64` (or `PULSAR_MSYS2_ROOT`) uses an existing MSYS2; `-NoBuild`
stops before cmake; `-KeepGoing` runs ninja with `-k 0`; `-Jobs N`.

| Step | Where |
| --- | --- |
| upstream source (git, shallow, by SHA) + submodules listed in the pin file (nested ones except `build-deps`) | `target\wf\sunshine-src` |
| MSYS2 base `msys2-base-x86_64-20260927.tar.xz` (URL + SHA-256 pinned), extracted with Windows' `tar.exe` | `target\wf\msys2\msys64` (download cache `target\wf\msys2-dl`) |
| pacman `-Syuu` (twice: the core update ends the shell) + the toolchain packages of the pin file | inside that MSYS2 only |
| FFmpeg prebuilt of upstream's `build-deps` tag (URL + SHA-256 pinned, pre-seeded so cmake does not download it) | `target\wf\sunshine-src\build-<arch>\_deps` |
| cmake (`BUILD_DOCS=OFF`, `BUILD_TESTS=OFF`, `SUNSHINE_BUILD_WEB_UI=OFF`, publisher "Pulsar Seat") + `ninja sunshine` | `target\wf\sunshine-src\build-<arch>` |
| `sunshine.exe` (PE machine checked), `build-record.json` (commit, patch hashes, MSYS2 package versions), `stream-server.json` (runtime `ServerDist`, flavour `pulsar_fork`), `build.log` | `target\wf\sunshine-out\<arch>` |

Nothing is installed system-wide; no PATH or registry change (the MSYS2 environment variables
are set for the script's process only). MSYS2 packages come from the rolling MSYS2
repositories (pacman checks their signatures); their versions are recorded in the build record,
not pinned. CI: job `stream-server` in `.github/workflows/ci.yml`.

Installer: `bun installer/scripts/fetch-deps.ts --arch x64 --stream-local target/wf/sunshine-out/x64`
lays out the upstream payload (assets, shaders) with this exe and its manifest (checked:
flavour, version, hash, licence). Since the review of 2026-11-17 this is the **default**
payload: `build-installer.ps1` uses `target\wf\sunshine-out\<arch>` (or `-StreamServer
<dir>`), runs this script when the build is missing, and ships upstream Sunshine only with
`-UpstreamStreamServer` (never with `-Release`). CI's installers job downloads the
`stream-server-<arch>` artifact of this job. MSYS2 packages stay rolling (recorded in the
build record, not pinned).

## Results on the ARM64 dev box (2026-11-17)

- `-Check`: all six patches apply to `14ffa6fd`.
- **x64 (UCRT64, x64 tools under emulation): built.** GCC 16.2, OpenSSL 4.1, Boost 1.92 (upstream
  fetched its own Boost 1.89 sources); `sunshine.exe` 63 MB, PE 0x8664, `--help` runs. First
  attempts stopped at upstream code with OpenSSL 4.1 (patch 0006) and at
  `SUNSHINE_ENABLE_TRAY=OFF` (upstream still compiles the tray calls; the script keeps the
  tray built, the seat server turns it off at run time).
- **ARM64 (CLANGARM64, native tools): built.** Upstream's `libdisplaydevice` submodule needs
  `<vector>` / `<type_traits>` / `<iterator>` that the current libc++ no longer includes
  transitively; patches cannot reach submodule files, so the pin file force-includes them
  (`arches.arm64.cxx_flags`). `sunshine.exe` 41 MB, PE 0xAA64, imports
  `minhook-detours.ARM64.Release.dll` (upstream's pinned FetchContent, copied next to the
  exe); `--help` runs natively.
- Same MSYS2 package set on both: CMake 4.5, Boost 1.92 (upstream builds its own 1.89),
  OpenSSL 4.1; exact versions per build in `build-record.json`.

## Not verified (HYPOTHESIS, needs a seat session, our IDD and pad driver loaded)

Everything at run time: the pipe's SYSTEM-client check and framing against the real service,
pairing with Moonlight through `pair.submit` (PIN, name, uuid, wrong PIN, timeout, cancel),
`apps.reload` while an app runs, `server.shutdown`, the web UI really staying off, the
`pulsar_idd` consumer against the driver's ring (named texture / fence opens from session N,
fence wait, generation changes, HDR), the mode request round trip, `pulsar_pad` input and
feedback through the pad driver, and `audio.state`.
