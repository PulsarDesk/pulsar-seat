# seat-host

`pulsar-seat-host.exe` (per-seat host: RDP client, presenter, input, audio bridge) and
`pulsar-seat-agent.exe` (per-session agent, user token). The flow of each binary is
documented at the top of `src/main.rs`, `src/ironrdp.rs` and `src/agent.rs`; the contract
is in [docs/agents/architecture.md](../../docs/agents/architecture.md) (§ 3 Processes,
§ 4.8 Seat host RDP client).

## Build and test

```powershell
cargo test -p seat-host
cargo clippy -p seat-host --all-targets -- -D warnings
cargo clippy -p seat-host --all-targets --target aarch64-pc-windows-msvc -- -D warnings
```

## In-session capture (OSMS-parity G1, 2026-11-20)

`local.frame_source = session_capture` (opt-in per local seat, default `rdp_decode`):

- `src/capture/agent_win.rs` (in the agent): DXGI Desktop Duplication of the seat session's
  outputs into rings of shared keyed-mutex BGRA textures, a seqlocked mailbox section and a
  frame event (`seat_core::capture`), offered to the service only.
- `src/capture/host_win.rs` (in the host): fetches the ring the service duplicated into the
  host (`host_capture_ring`), presents its slots through the unchanged `present_gpu` path
  and drops the RDP picture while the capture is live (`src/capture/mod.rs`,
  `CaptureGate`); a capture seat connects without EGFX.

Same code on x64 and ARM64. Clean room from the Microsoft Learn DXGI / D3D11 / Win32 pages;
no third-party code.

## Untested on hardware

- The whole in-session capture path. HYPOTHESES: Desktop Duplication works on RdpIdd
  outputs inside an RDP session (x64 GPUs, Qualcomm Adreno on ARM64); the session renders on
  the seat monitors' adapter (otherwise the seat keeps the RDP picture by design); a
  low-integrity host can open the duplicated shared textures; the session's virtual desktop
  has the client's monitor layout (used to match RDP monitors to captured outputs);
  `GetCursorPos` in the agent reports the seat session's pointer. Rotated outputs, the lock /
  UAC desktop and sessions before sign-in keep the RDP picture.
