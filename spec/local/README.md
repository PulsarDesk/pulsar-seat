# Behaviour spec

Written by the analyst only (see [../../CLEANROOM.md](../../CLEANROOM.md)). Behaviour and
interfaces, no code. Each file also notes what must change for native ARM64.

| File | Covers | Status |
| --- | --- | --- |
| `00-architecture.md` | components, processes, services, drivers and their device-stack positions, how they talk, install/boot changes, Windows mechanisms used, signing and legal concerns | draft v1 2026-11-12 (critiqued, cross-checked) |
| `10-manager.md` | manager screens, fields, defaults, validation, flows (create seat, assign devices, identify monitor/keyboard), what each action changes | draft v1 2026-11-12 (critiqued, cross-checked) |
| `20-service.md` | seat service: config model (files, registry), boot sequence, seat lifecycle, watchdog, IPC, errors, logs | draft v1 2026-11-12 (critiqued, cross-checked) |
| `30-sessions.md` | several concurrent local interactive sessions: how each seat gets its own session, logon screen per seat, auto-logon, lock / switch user, session-to-seat binding | draft v1 2026-11-12 (critiqued, cross-checked) |
| `35-sessions-usermode.md` | feasibility of concurrent local seat sessions with user-mode code only on client Windows (x64, ARM64): what the reference's console-session step achieves, how client Windows limits sessions, candidate approaches and verdicts, prototype plan, effect on spec 30 and the session backends | draft v1 2026-11-13 |
| `40-display.md` | monitors and GPU outputs per seat, multi-GPU, resolution / refresh / orientation, what a seat's session can see, fullscreen games, display hot-plug | draft v1 2026-11-12 (critiqued, cross-checked) |
| `50-input.md` | keyboards, mice, touch, pen, gamepads per seat: device identification, routing to a session, raw input / HID visibility, hot-plug, hotkeys | draft v1 2026-11-12 (critiqued, cross-checked) |
| `60-audio.md` | per-seat playback and recording endpoints, default device per session | draft v1 2026-11-12 (critiqued, cross-checked) |
| `70-usb-devices.md` | USB ports / hubs per seat, storage, printers, webcams, other devices; drive-letter visibility | draft v1 2026-11-12 (critiqued, cross-checked) |
| `80-apps-compat.md` | app / game compatibility tweaks (single-instance apps, Steam, anti-cheat notes), per-seat app restrictions | draft v1 2026-11-12 (critiqued, cross-checked) |
| `90-install.md` | installer / uninstaller actions, driver packages and signing, services, system settings changed, update channel | draft v1 2026-11-12 (critiqued, cross-checked) |
