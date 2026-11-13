# display
Goal: package "display" per docs/agents/contracts.md §3: crates/split-display and
drivers/split-idd (IddCx 1.10 driver source, unbuilt/untested/unsigned).

Done (2026-11-12):
- crates/split-display complete: modes, hdr, adapters, seat, frame, ioctl, journal,
  backend/{mod, rdp, vdd, vdd_xml, pulsar_idd}, win/{os, dxgi, devices, deviceio, vdd,
  journal}, fake. README lists every contract addition (nothing renamed or dropped).
- drivers/split-idd complete as source: INF, vcxproj, include/PulsarIddShared.h,
  src/{Driver.h, Driver.cpp, Adapter.cpp, Modes.cpp, Control.cpp, FrameChannel.cpp,
  SwapChain.cpp}, README (UNBUILT · UNTESTED · UNSIGNED), .gitignore.
- Open questions display-1..6 appended; journal line added.
- Green in the repo workspace: `cargo test -p split-display` (51 + 1 ignored),
  `cargo clippy -p split-display --all-targets` (0 warnings, also `--features fake`),
  `cargo check -p split-display --all-targets --target x86_64-pc-windows-msvc`.
  `cargo test -p split-display -- --ignored` (read-only smoke test) passes on ARM64.

In progress: nothing.

Next (integrator / later):
- Wire into split-service (choose_backend, attach + wait_ready after session up), helper
  in the console session for the VDD ModeSetter / monitors (display-1).
- First WDK build of drivers/split-idd (fix `VERIFY` identifiers, display-6).

Verify:
- cargo test -p split-display; cargo clippy -p split-display --all-targets;
  cargo check -p split-display --target x86_64-pc-windows-msvc
- If the repo workspace fails to load because another package is mid-write, copy
  crates/split-display + crates/split-proto into a throw-away workspace (see
  `<scratchpad>/dsp/sync.sh`) and run the same commands there.

## Fix round 1 (2026-11-12, review findings)
Done. Green: cargo test -p split-display (59 + 1 ignored, also --features fake, --ignored smoke ok), clippy --all-targets 0 warnings (also --features fake), check x86_64, fmt. Driver C++ still unbuilt (no WDK). Items:
- [x] win/secure.rs: folder owner/DACL/reparse check + safe temp-file write (VDD settings, journal)
- [x] win/vdd.rs: settings file via secure.rs; pipe SQOS identification, full WUDFHost path,
      session 0 + LocalService, overlapped I/O + CancelIoEx (no abandoned thread)
- [x] win/journal.rs: protected PulsarSplit dir, DACL check, no reparse / link count 1
- [x] journal.rs: `restored` marker; plan_restore only after last marker; restore_all journals
- [x] deviceio.rs + DeviceHandle: in-flight counter, close drains before CloseHandle
- [x] backend/vdd.rs: seat bound to (luid, target); rebind reported on middle detach
- [x] backend/pulsar_idd.rs: no seats lock across IOCTLs
- [x] driver: Control.cpp cancel (MarkCancelableEx), lifecycle mutex, persistent channel,
      WAIT_EVENT lost wakeup, monitor re-create, commit compare hz/colour, squat checks
- [x] OQ display-7..10 appended; journal line; READMEs updated
