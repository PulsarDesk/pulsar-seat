# Behaviour spec

Written by the analyst only (see [../../CLEANROOM.md](../../CLEANROOM.md)). Behaviour and
interfaces, no code. Each spec ends with a "Pulsar Split approach" and "Open questions".

| File | Covers | Status |
| --- | --- | --- |
| `00-architecture.md` | components, processes, services, drivers, how they talk, install/boot system changes, Windows mechanisms, legal/signing concerns | draft v1 (2026-11-12) |
| `10-manager.md` | manager screens, fields, defaults, validation, flows, what each action changes | draft v1 |
| `20-service.md` | seat service: registry model, per-seat config files and keys, ports, boot sequence, seat lifecycle, watchdog, control API (pipe in Pulsar Split), errors | draft v1 |
| `30-session-rds.md` | multi-session enablement, headless loopback-RDP session creation, session discovery, logoff/disconnect, WRDS provider; **decision gate** on termsrv wrapping | v1 reviewed (critic + revise) |
| `40-display-hdr.md` | per-seat virtual monitor (own IddCx), modes, GPU pinning, frame hand-off; HDR via IddCx only | v1 reviewed |
| `50-shim-isolation.md` | per-process injection, HID visibility per session, XInput focus, Steam isolation, remote-session masking, DXVK redirect | v1 reviewed |
| `60-controllers.md` | virtual pads, driver package, devnode naming, report/feedback channels, ownership, Bluetooth | v1 reviewed |
| `70-audio.md` | per-seat audio from the RDP endpoint, microphone, stream-server audio | v1 reviewed |
| `80-stream-server.md` | per-seat stream-server integration, config, pairing/PIN via manager, ports, firewall | v1 reviewed |
| `90-install.md` | installer/uninstaller, drivers + signing, firewall, coexistence | v1 reviewed |
