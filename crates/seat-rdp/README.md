<!-- SPDX-License-Identifier: GPL-3.0-only -->
# seat-rdp

The seat host's loopback RDP client engine (GPL-3.0-only). It is the default RDP backend of
`pulsar-seat-host.exe`. The Remote Desktop ActiveX control is kept as a fallback
(`--rdp-backend activex` or `PULSAR_SEAT_RDP_BACKEND=activex`). The host never falls back
on its own, because the control cannot pin the listener certificate.

## Decision (2026-11-13): IronRDP, not FreeRDP

| | IronRDP (chosen) | FreeRDP |
| --- | --- | --- |
| Licence | MIT OR Apache-2.0 | Apache-2.0 |
| Language / build | Rust crates, cargo only; TLS via SChannel (`native-tls`) | C, CMake, OpenSSL or SChannel; a separate ARM64 toolchain build |
| aarch64-pc-windows-msvc | builds natively, no C toolchain | builds, but needs its own CMake/vcpkg pipeline |
| NLA / CredSSP | `sspi` crate (NTLM) | yes |
| Certificate pinning | ours: we own the TLS step | callback |
| Audio out / mic | `ironrdp-rdpsnd` (static channel), `ironrdp-rdpeai` (`AUDIO_INPUT`) with pluggable back-ends | yes |
| Touch / pen | `ironrdp-rdpei` (MS-RDPEI v3) | yes |
| Relative mouse | fast-path `TS_FP_RELPOINTER_EVENT` (RDP 10.12) | yes |
| Multi-monitor / resize | GCC monitor blocks, `ironrdp-displaycontrol` | yes |
| Clipboard | `ironrdp-cliprdr` + `ironrdp-cliprdr-native` (Windows back-end: delayed rendering, OLE file paste) | yes |
| Drives / smart cards | `ironrdp-rdpdr` + `ironrdp-rdpdr-native` (Windows drive back-end with read-only switch, WinSCard back-end) | yes |

IronRDP covers every channel the seat needs, and it fits the existing Rust workspace and
the Pulsar Split helper (same library). FreeRDP stays the fallback if IronRDP's CredSSP
does not work against a real Server build (untested, see below).

## What it does

- `pin`: the listener's certificate thumbprints come from RDP-Tcp `SSLCertificateSHA1Hash`,
  or else from the local machine's `Remote Desktop` store. The service sends them in
  `HostAssignment::listener_cert_sha1`; the host can also read them itself. If no pin
  is known, the host does not connect (fail closed). The pin is checked before CredSSP.
- `config`: with a password, the host uses CredSSP / NLA only. Without a password (the
  seat shows its logon screen), the host uses TLS only and sends no secret. Client name
  `PSEAT-NN`. Multi-monitor GCC blocks with scale and orientation. Keyboard layout.
- `output`: the BGRA framebuffer (with a software pointer) is cut into one frame per seat
  output for the seat host's `FrameSink`. Only dirty areas are copied. One output is
  presented without a copy.
- `gfx` / `h264`: the graphics pipeline (MS-RDPEGFX) through `ironrdp-egfx` (capability
  negotiation V8 .. V10.7, surfaces, bitmap cache, solid fill, surface copies, ClearCodec,
  planar, RemoteFX, RemoteFX Progressive, AVC420, AVC444, frame acknowledgements).
  H.264 is decoded by Media Foundation's H.264 decoder MFT: GPU first (a D3D11 device with
  video support handed to the MFT), CPU otherwise; low-latency mode, NV12 read back into
  YUV 4:2:0 planes, converted and (for AVC444) recombined by the pipeline. IronRDP
  composites the frames into the same framebuffer as bitmap updates, so the splitter and
  the presenter are unchanged. A server that does not open EGFX sends bitmap updates.
  Metrics (`SessionEvent::Graphics`): version, codecs, decoder path, frames, decode time,
  errors, pictures the decoder dropped, updates the pipeline dropped, resyncs, hardware to
  software fallbacks, presents, coalesced frames, longest wait of a change for its present.
  Overrides: `PULSAR_SEAT_GFX=bitmap|no-h264`, `PULSAR_SEAT_H264_DECODER=software`,
  `PULSAR_SEAT_GFX_ZERO_COPY=0` (no GPU path).
  - Decoder adapter: the H.264 decoder's D3D11 device is created on the adapter that
    presents the seat's first output (`FrameSink::gpu_adapter`, by LUID), not the default
    adapter.
  - Zero-copy path (`gpu`, round 4): the EGFX channel is our `gpu::channel::GfxChannel`,
    which wraps IronRDP's `GraphicsPipelineClient` (registered in its place, as
    `ironrdp_session::composite_graphics_pipeline` documents) and decompresses / splits
    the PDUs itself. A surface mapped 1:1 over seat outputs that all take GPU frames on
    the decoder's adapter, with no other surface over them, moves to the GPU at an AVC420
    IDR (`gpu::route`): it is seeded once from the session image between two frames, then
    its AVC420 updates are decoded by a per-surface Media Foundation MFT in D3D11 mode and
    blitted NV12 to BGRA by the D3D11 video processor into a surface texture
    (`gpu::d3d`), never read back. Paints of the other codecs still run in IronRDP's
    compositor and are copied into the texture at each `EndFrame` (minus later AVC paints
    of the same frame). The texture is copied into one of three shared keyed-mutex
    textures and handed to the presenter (`FrameSink::present_gpu`) when its output took
    the last one; otherwise the frame waits (newest wins). The splitter skips GPU outputs.
    AVC444, progressive tiles, scaled / window mappings, copies or cache stores that read
    AVC areas, a GPU error or a presenter refusal send the surface (or the whole path)
    back to the CPU path; when its CPU pixels are stale the session asks for a graphics
    protocol reset (fresh IDR).
  - Back-pressure: `FrameSink::backlog` counts outputs whose last frame is not on screen
    yet. While it is non-zero, CPU presents wait (changes coalesce, at most 50 ms) and
    `FrameAcknowledge` PDUs are held (`gpu::ack`: at most 3 frames / 100 ms, queue depth
    = bytes of the held frames, released in order when the presenter catches up), so the
    server slows down instead of queueing pictures we would drop.
  - `ResetGraphics` of a new size (the server's answer to a display-control resize while
    EGFX is open, no reactivation): IronRDP resizes the framebuffer, the session cuts it
    with the seat layout again and reports `DesktopSize` / `Outputs`. The PDU's monitor
    list does not reach us (ironrdp-egfx 0.5 handler gives only the size); the layout as
    sent is used.
  - Resync: a failed decode, a dropped update or a dropped picture asks for a full redraw
    (at most every 2 s); damage that keeps coming after it asks for an MS-RDPEGFX graphics
    protocol reset (V10.3+), which restarts the H.264 stream with an IDR.
  - Pictures are paired with their access unit by sample time; a late one is counted as
    dropped, never painted for the wrong unit (AVC444 views stay paired).
  - Hardware decode that fails 5 times in a row, or loses its D3D11 device, switches to
    the software MFT at run time.
  - Pacing: with EGFX the session presents at each completed frame (EndFrame); bitmap
    updates at most every 8 ms and when the socket goes quiet.
- `input`: keys, pointer, wheel and X buttons go out as fast-path events. Relative motion
  goes out as `TS_FP_RELPOINTER_EVENT`. Touch and pen go out as MS-RDPEI frames. Pen contacts carry the optional `RDPINPUT_PEN_CONTACT` fields when the pen reports them (2026-11-17): `penFlags` (barrel, eraser, inverted), `rotation` (0..359), `tiltX` / `tiltY` (-90..90, each on its own), `pressure` (0..1024). HYPOTHESIS: never sent to a real RDP server.
- `audio`: PCM only. rdpsnd goes to a `PlaybackSink`. `AUDIO_INPUT` comes from a
  `CaptureSource`. The host connects both to `seat_media::bridge::AudioBridge`, by
  endpoint id.
- `session`: one engine thread owns the socket. A `Handle` (also the seat's
  `RdpInputChannel`) sends commands over a channel. Layout changes go through display
  control. Deactivation-reactivation is handled. The engine reconnects in the host only
  for transient network failures, at most 4 times (1, 2, 4, 8 s). Every attempt runs
  the listener check, the peer check and the pin again.

## Headless hold mode (`headless`, merge step 7)

Replaces Pulsar Split's `pulsar-split-rdp-helper.exe`. The service starts
`pulsar-seat-host.exe --headless-hold <args>` per running **remote** seat; the seat host
dispatches the sub-command before anything else (`crates/seat-host/src/headless.rs`).

- `headless::proto` (always built, also without `client`; the service uses it):
  `HelperArgs` argv (`--protocol 1 --port <u16> --address 127.0.0.1|::1 --client-name <PSS…>
  --user <sam> --width --height --layout <KLID hex> --cert-sha1 <40 hex> [--audio-playback]
  [--audio-capture] [--dry-run]`; `--password` / `-p` / `/p` are refused), one stdin
  password frame (`u32` LE length + UTF-8, at most 512 bytes, then EOF), JSON Lines
  `HelperEvent`s on stdout (`connecting`, `authenticated`, `connected {session_id}`,
  `disconnected {reason}`, `failed {code, detail}`, `exited {code}`), exit 0 / 1 / 2. This
  is Split's helper contract, unchanged; Seat's `host_report` pipe is not used (a hold
  has no seat pipe, token or monitors).
- `headless::client` (`client`): X.224 with NLA only → TLS (SChannel) → the server
  certificate's SHA-1 must equal `--cert-sha1` → CredSSP / NTLM → active stage that only
  answers the server. No frame sink: frames are decoded into a scratch image and dropped.
  `rdpsnd` is joined (plain PCM offered, every wave PDU acknowledged, samples dropped) so
  the session gets its Remote Audio endpoint. The password copy is dropped after CredSSP
  and the reactivation factory is rebuilt without it (same IronRDP plain-`String` limit as
  below). The session id is not reported (`connected{session_id: null}`); the service
  finds the session by client name + user + loopback.
- `headless::mic` + `--audio-capture` (2026-11-14, spec/remote 70 §5.1): the password
  frame is not followed by EOF; stdin then carries microphone frames
  (`seat_core::remote::mic`: u32 LE length + start / pcm / stop, 48 kHz s16le). The client
  sets `INFO_AUDIOCAPTURE`, joins DRDYNVC with MS-RDPEAI `AUDIO_INPUT` offering only 48 kHz
  stereo / mono (`HOLD_CAPTURE_SPECS`; mono / stereo converted, never resampled), and the
  active loop runs on a non-blocking socket so mic packets go out between server PDUs
  (same `Patient` writes as the session engine). Without the flag nothing changes.
  **Untested against a real server** (HYPOTHESIS: the session gets a capture endpoint).
- Tests: proto units (argv round trip, sub-command, password frame, events), client units
  (config, pin, audio formats, error mapping) and `crates/seat-host/tests/headless.rs`
  (runs the built seat host with `--dry-run` over the real argv / stdin / stdout).
  **A real connection is untested** (needs a Server + RDSH box).

## Known limits

- **Untested on hardware.** No RDP target exists on the dev box. The first real run needs
  Windows Server + RDSH.
- IronRDP keeps the password as a plain `String` in its config and CredSSP state. Only
  our `Zeroizing` copy is wiped (same limit as Pulsar Split's helper). The seat host is a
  SYSTEM-only process.
- Redirection channels (`channels`, `policy`; **untested on hardware**): clipboard
  (`cliprdr`) per the seat's direction (`to_seat` / `from_seat` / both) and format classes
  (text, html, images, files; every other format refused), bridged to the **host
  session's** clipboard, so it is shared with seat 1's user and every other seat that has
  it on (off by default). An empty format list never wipes the seat's own clipboard;
  console copies made in another seat are refused unless `cross_seat`; late answers of an
  earlier connection are dropped. Drives (`rdpdr`): only volumes the seat owns alone, by
  drive letter pinned to its volume GUID path (re-checked right before each open:
  `drive_letter_moved`), never the system drive, read-only when the policy says so.
  FAT / FAT32 / exFAT volumes are served by the host; NTFS / ReFS (any volume with ACLs)
  by the **drive broker** (`channels::broker`, 2026-11-17): the seat session's agent opens
  the files as the seat user, the host forwards the IRPs over two service-made anonymous
  pipes, and the broker serves only the service's allowlist. Live changes (2026-11-17):
  drives and printers the service assigns or unassigns while connected join or leave the
  open channel (`SeatRdpdr::live_changes`; `drive_unassigned`, `printer_unassigned`), ACL'd
  drives once the broker is linked (`drive_broker_unavailable` until the user signed in). A seat volume whose
  letter moved into the seat's own logon session (`session_drive_letters` gate) cannot be
  opened by the host and is skipped (`drive_without_global_letter`). Smart cards: only the
  seat's readers are listed (per requested group); readers are matched by device instance
  id (`SCardGetReaderDeviceInstanceIdW`), by friendly name only when it is unique on the
  PC, else refused (`reader_name_ambiguous`); calls naming another reader get
  `SCARD_E_UNKNOWN_READER`, reader administration `SCARD_E_NO_ACCESS`, the shared card
  cache is not used; the PnP notification count is the seat's filtered count. Printers: opt-in
  (see `policy`), added and removed live too (Pulsar's patch of `ironrdp-rdpdr`, folder
  `third_party/ironrdp-rdpdr`). Ports are never redirected. `ListReaderGroups` lists only
  groups that hold one of the seat's readers. Every
  refusal is logged (`seat-rdp: refused <channel>: <reason>`, at most once per 10 s per
  reason) and reported to the service (manager, Durum tab). Asked-for channels without
  resolved data are still logged as `*_redirect_unavailable`. A seat never gets more than
  its policy allows.
- The server's volume PDU is not applied, because the seat session's own endpoint
  already applies volume.
- EGFX / H.264 is **untested against a real RDP server**. Decoding itself is tested: the
  system H.264 encoder feeds our decoder in `h264::mf` tests (hardware and software paths
  both returned every picture on the ARM64 dev laptop, Qualcomm GPU).
- The zero-copy path and the acknowledgement gate are **untested on hardware and against
  a server** (built 2026-11-14; the router, the gate and the channel are tested with
  hand-built PDUs and a fake GPU compositor). HYPOTHESIS, to verify on the first runs:
  hardware MFT pictures (NV12 texture-array slices) are accepted as video processor input
  views; the output target rectangle limits each blit to its region; keyed-mutex sharing
  between the decoder's device and the presenter's device on one adapter works on x64
  GPUs and on Adreno (ARM64); the video processor's full-range BT.709 conversion matches
  the CPU path; Windows servers in AVC420 mode keep AVC and other-codec regions mostly
  apart and send an IDR when a surface starts (else surfaces rarely promote, or demote
  often: `promotions` / `demotions` in the log line); Windows servers throttle on held
  acknowledgements and read `queueDepth` as documented.
- AVC444 on the GPU (2026-11-17, `gpu::avc444`, `gpu::avc444_model`): compute shaders merge
  each view into an `R32_UINT` 4:4:4 picture, a pixel shader converts the painted
  rectangles; promoted only at an IDR with both views over the whole surface; mixing
  AVC420 and AVC444 on one GPU surface demotes it. The rules are tested against IronRDP's
  CPU combination, the HLSL on WARP (dev box, `shaders_match_the_model_on_warp`).
  HYPOTHESIS: hardware decoders' NV12 slices copy into a shader-readable NV12 texture on
  the target drivers; the HLSL needs feature level 11.0 (below it AVC444 stays on the CPU).
- Zero-copy limits: outputs that are clone sources
  or Display Core targets get CPU frames; one GPU compositor per connection (the adapter
  of the seat's first output); the cursor over GPU frames is drawn through a small staging
  copy per present.
- H.264 decoder ladder (2026-11-17): the GPU vendor's asynchronous hardware MFT first
  (`MFTEnumEx` with `MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT |
  MFT_ENUM_FLAG_SORTANDFILTER`, unlocked with `MF_TRANSFORM_ASYNC_UNLOCK`, driven by
  `METransformNeedInput` / `METransformHaveOutput`, at most 200 ms wait for input and
  50 ms for the unit's picture), then Microsoft's MFT on the GPU, then on the CPU; a
  decoder that keeps failing steps down one level. `PULSAR_SEAT_H264_DECODER=microsoft`
  skips the vendor MFT. The ARM64 dev box (Adreno) lists **no** vendor H.264 MFT, so the
  async path ran only as far as its fallback; HYPOTHESIS on x64 GPUs (NVIDIA / AMD /
  Intel vendor MFTs).
- Latency hooks: `GfxStats::decode_latency` / `frame_latency` (p50 / p95 / p99 / max of
  the last 512 samples) in the `gfx ...` log line.

## Third-party notices

- IronRDP (`ironrdp-connector`, `-session`, `-blocking`, `-pdu`, `-core`, `-graphics`,
  `-svc`, `-dvc`, `-rdpsnd`, `-rdpeai`, `-rdpei`, `-displaycontrol`, `-egfx` and their
  sub-crates; `ironrdp-egfx` also credits Teleport; `ironrdp-rdpdr` 0.9.0 is vendored with
  a small patch in `third_party/ironrdp-rdpdr`, licence files kept, see its `PATCHES.md`):
  Devolutions Inc. and contributors, MIT OR Apache-2.0, https://github.com/Devolutions/IronRDP
- sspi-rs (`sspi`), picky: Devolutions Inc., MIT OR Apache-2.0
- `native-tls`, `schannel`, `sha1`, `x509-cert`, `num-traits`, `zeroize`: MIT / Apache-2.0
- `windows` (Microsoft, MIT OR Apache-2.0): Media Foundation / D3D11 bindings. The
  optional OpenH264 support of `ironrdp-egfx` is **not** enabled (no Cisco binary).

All of them are GPL-3.0-compatible. The release build's `cargo about` step writes their
licence texts into `installer/licenses/THIRD-PARTY-NOTICES-RUST.md`.
