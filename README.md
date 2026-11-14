# Pulsar Seat

Free, open-source multi-seat for Windows (x64 and ARM64): several people use one PC at the
same time, each in their own Windows session. Every seat has a kind:

- **Local seat:** a monitor, keyboard, mouse, audio device and USB ports plugged into the
  PC.
- **Remote seat:** a virtual display and virtual gamepads, streamed to a Moonlight or
  Pulsar client on another device. Remote seats need concurrent Windows sessions (Windows
  Server with Remote Desktop Session Host, or the experimental wrapper backend, off by
  default).

Both kinds run side by side on one PC. One service, one manager (Turkish UI) and one MSI
per architecture. The remote kind was Pulsar Split; it is merged into Pulsar Seat
(`docs/agents/merge-plan.md`; Split's history is under `docs/split/`).

No payment, no activation, no telemetry. Nothing has been tested on real hardware yet.
Licence: GPL-3.0-only (see [LICENSE](LICENSE); third-party notices in
[installer/licenses/NOTICES.md](installer/licenses/NOTICES.md)).

Contributors and agents: start at [AGENTS.md](AGENTS.md).
