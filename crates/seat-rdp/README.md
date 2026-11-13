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
  Overrides: `PULSAR_SEAT_GFX=bitmap|no-h264`, `PULSAR_SEAT_H264_DECODER=software`.
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
  goes out as `TS_FP_RELPOINTER_EVENT`. Touch and pen go out as MS-RDPEI frames.
- `audio`: PCM only. rdpsnd goes to a `PlaybackSink`. `AUDIO_INPUT` comes from a
  `CaptureSource`. The host connects both to `seat_media::bridge::AudioBridge`, by
  endpoint id.
- `session`: one engine thread owns the socket. A `Handle` (also the seat's
  `RdpInputChannel`) sends commands over a channel. Layout changes go through display
  control. Deactivation-reactivation is handled. The engine reconnects in the host only
  for transient network failures, at most 4 times (1, 2, 4, 8 s). Every attempt runs
  the listener check, the peer check and the pin again.

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
  earlier connection are dropped. Drives (`rdpdr`): only FAT / FAT32 / exFAT volumes the
  seat owns alone, by drive letter pinned to its volume GUID path (re-checked right before
  each open: `drive_letter_moved`), never the system drive, read-only when the policy says
  so. NTFS / ReFS are refused (`drive_acl_volume_refused`): drive IRPs run under the seat
  host's token, not the seat user's, so ACLs could not be honoured. Drives the service
  unassigns while connected leave the session (`drive_unassigned`). A seat volume whose
  letter moved into the seat's own logon session (`session_drive_letters` gate) cannot be
  opened by the host and is skipped (`drive_without_global_letter`). Smart cards: only the
  seat's readers are listed (per requested group); readers are matched by device instance
  id (`SCardGetReaderDeviceInstanceIdW`), by friendly name only when it is unique on the
  PC, else refused (`reader_name_ambiguous`); calls naming another reader get
  `SCARD_E_UNKNOWN_READER`, reader administration `SCARD_E_NO_ACCESS`, the shared card
  cache is not used; the PnP notification count is the seat's filtered count. Printers and
  ports are never redirected (the session sees the PC's own queues and ports). Every
  refusal is logged (`seat-rdp: refused <channel>: <reason>`, at most once per 10 s per
  reason) and reported to the service (manager, Durum tab). Asked-for channels without
  resolved data are still logged as `*_redirect_unavailable`. A seat never gets more than
  its policy allows.
- The server's volume PDU is not applied, because the seat session's own endpoint
  already applies volume.
- EGFX / H.264 is **untested against a real RDP server**. Decoding itself is tested: the
  system H.264 encoder feeds our decoder in `h264::mf` tests (hardware and software paths
  both returned every picture on the ARM64 dev laptop, Qualcomm GPU).
- **Deferred:** D3D11 texture output and zero-copy from the decoder to the D3D11 flip
  presenter. The decoded picture is read back to system memory, because the pipeline's
  compositor (surfaces, surface copies, the bitmap cache) works on CPU pixels, and the
  decode device is on the default adapter, not matched to the presenter's by LUID.
  Zero-copy needs a GPU frame variant of `FrameRef`, the decode device on the presenter's
  adapter and a GPU NV12 to BGRA path for surfaces mapped 1:1 that only get AVC updates.
- Frame acknowledgement is not tied to the present: ironrdp-egfx 0.5 sends
  `FrameAcknowledge` (queue depth "unavailable") inside its EndFrame handling, so a slow
  presenter does not throttle the server; overwritten frames show as `coalesced`.
- The vendor's asynchronous hardware MFTs are not used; the Microsoft MFT already decodes
  on the GPU through D3D11 video decoding.

## Third-party notices

- IronRDP (`ironrdp-connector`, `-session`, `-blocking`, `-pdu`, `-core`, `-graphics`,
  `-svc`, `-dvc`, `-rdpsnd`, `-rdpeai`, `-rdpei`, `-displaycontrol`, `-egfx` and their
  sub-crates; `ironrdp-egfx` also credits Teleport):
  Devolutions Inc. and contributors, MIT OR Apache-2.0, https://github.com/Devolutions/IronRDP
- sspi-rs (`sspi`), picky: Devolutions Inc., MIT OR Apache-2.0
- `native-tls`, `schannel`, `sha1`, `x509-cert`, `num-traits`, `zeroize`: MIT / Apache-2.0
- `windows` (Microsoft, MIT OR Apache-2.0): Media Foundation / D3D11 bindings. The
  optional OpenH264 support of `ironrdp-egfx` is **not** enabled (no Cisco binary).

All of them are GPL-3.0-compatible. The release build's `cargo about` step writes their
licence texts into `installer/licenses/THIRD-PARTY-NOTICES-RUST.md`.
