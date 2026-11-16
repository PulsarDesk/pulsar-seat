<!-- SPDX-License-Identifier: GPL-3.0-only -->
# parity-display-gpu (2026-11-17)

Goal: display / GPU parity for local seats. (a) one IddCx adapter per render GPU
(seat-idd + seat-display + service + manager); (b) per-seat GPU preference
(`HKCU\Software\Microsoft\DirectX\UserGpuPreferences` of the seat user); (c) HDR (IddCx
1.10 FP16) and VRR for seats 2..N through the presenter, HDCP documented as not
available; (d) EGFX AVC444 on the GPU path, vendor async hardware MFTs with fallback,
latency hooks.

## Done
- seat-idd minor 3: adapter per render GPU (GET_ADAPTER_INFO, bound LUID, STATUS_DEVICE_BUSY,
  PulsarSeatIdd_Adapter hwid in INF + roles), device registry instead of g_Device, IddCx 1.10
  callbacks for the local role (target info, parse2, target modes2, commit2, default HDR
  metadata), FP16 / 10-bit frames + metadata + QPC stamps (swapchain.cpp). build-drivers clean.
- seat-display: idd_proto minor 3 mirror + tests, idd_adapters.rs (pick_adapter), win/swdevice.rs,
  WinIdd multi-adapter routing, reader formats + HdrFrameRef + latency in pump.
- seat-core: backend/hdr.rs (conversions, HdrFrameRef), FrameSink::present_hdr default,
  latency.rs, AssignmentOptions hdr/vrr, OutputPlacement hdr/vrr, SeatDisplayPrefs hdr/vrr.

## In progress
- dcore.rs HDR10 scan-out + VRR; flip.rs VRR / HDR

## Next
- GPU preference (b); seat-rdp AVC444 GPU + async MFT + latency (d); manager UI; docs

## Verify
- cargo test --workspace; clippy aarch64 + x86_64; manager test/check/build;
  build-drivers.ps1 -Projects seat-idd (ARM64 + x64, OutRoot target\wf\drivers-display)
