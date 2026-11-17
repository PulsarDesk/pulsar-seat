# parity-review-fix (2026-11-17)

Goal: apply the parity review (confirmed findings + completeness gaps), all gates green,
commit "fix: apply parity review" (split into several commits).

## Done (code + tests, not committed yet)

- kiosk pad owner = kiosk user SID (local_pads.rs `KioskSids`, mock `kiosk_users`).
- pad_scope retry with back-off + one warning per error (`sync_at`, `restore_all_at`,
  pad_runtime uses them).
- pad_device_scope default off (config, validate test, manager `?? false`, hint text).
- pad media only with audio (`VirtualPad::has_audio`, usbip `audio`, no mic read).
- hotkey_guard raw-first key behind a pending hotkey.
- shim cursor: DPI conversion, `r != 0`, SetCursorPos / SetPhysicalCursorPos / ClipCursor /
  GetClipCursor, GetAsyncKeyState / GetKeyState / GetKeyboardState (section v2
  `kiosk_cursor`, router `warp` / `set_clip` / `key_state`, shell `on_cursor_requests`).
- per-seat kiosk clipboard (`seat_shim_core::kiosk_clip`, `shim/module/src/rt/clip.rs`,
  shell `ClipSection`).
- mf.rs flush START_OF_STREAM + IMFShutdown / ShutdownObject.
- seat-wfp explicit bind table (callout.c `SeatWfpBound*`); drivers built clean.
- INF [Strings] English + [Strings.041f]; `installer/scripts/driver-inf-locales.test.ts`.
- .gitignore `.claude/`; merged worktrees 1-4 removed (5 = i18n kept: not an ancestor).
- kiosk accounts disabled at service start, deleted at uninstall (profiles with
  REMOVEUSERDATA only) or `remove_kiosk_user` request (manager seat menu).
- `reset_graphics` request + agent task + manager seat menu.
- probe notes `kiosk_users_share_desktop`, `no_per_user_isolation` for console-user seats;
  manager personal-account warning.
- sdb: x64 targets left out on ARM64 (`apphelp_targets`, `pe_machine_of`); isolationNote text.
- test fixes (kiosk masks vs windows-sys, devtree name/doc, kiosk_cursor CAPTUREMOUSE).

## In progress

- shim clippy x64 / i686 / aarch64 after clip.rs.

## Next (gaps)

IddCx pump wiring; remote SDL ignore list; WNF runtime verifier; GameInputSvc opt-in;
CPU watcher + agent launches; pad feedback (XInput, HID PID, Switch SPI); smart card per
seat; webcam per camera; Hyper-V helper; sandbox seats; Sunshine fork patch set; docs.

## Verify

cargo test --workspace; clippy aarch64 + x86_64; manager test/check/build; shim tests;
bun test installer/scripts; build-drivers.ps1 (done once after wfp + INF changes).
