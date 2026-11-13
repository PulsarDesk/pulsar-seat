# pads

Goal: package "pads" per docs/agents/contracts.md §5 — crates/split-pads and
drivers/split-pad (UMDF2 HID minidriver source, unbuilt/untested/unsigned).

Done (2026-11-12):
- crates/split-pads complete: types, error, layout, descriptors, features, reports, shm,
  channel, mapping, control, ids, owners, journal, bluetooth, manager (+ manager_tests),
  vigem (proto + backend), usbip (proto, descriptors, device, server, backend), pulsarpad,
  fake, win (channel, devtree, process, services, swdevice, usbip, vigem). README.
- drivers/split-pad: Cargo.toml ([workspace]), build.rs, Makefile.toml, split_pad.inx,
  src/{lib,props,channel,hid}.rs, README, .gitignore. core-only parts type-checked in a
  scratch no_std crate; WDF parts not (no WDK).
- open-questions pads-1..7, journal line.

In progress: none.

Next (integrator / later): wire PadManager into split-service (seat pipe pad.* events,
pump loop, cleanup at start/stop, shim seat map prefixes, get_pad_backends); live tests
listed in crates/split-pads/README.md "Untested".

Verify:
  cargo test   -p split-pads          (95 tests)
  cargo clippy -p split-pads --all-targets
  cargo check  -p split-pads --all-targets --target x86_64-pc-windows-msvc
  cargo check  -p split-pads --features fake

## Fix round (review findings, 2026-11-12) — DONE
All critical/high/medium + low findings applied (see crates/split-pads/README.md
"Review fix" and drivers/split-pad/README.md "Review fix"); open questions pads-8..12.
Not done: docs/journal.md line and contracts.md §5.1 update (not owned by this package;
listed in pads-9 for the integrator).

Verify:
  cargo test   -p split-pads          (110 tests)
  cargo clippy -p split-pads --all-targets [--features fake] [--target x86_64-pc-windows-msvc]
  cargo check  -p split-pads --all-targets --target x86_64-pc-windows-msvc
  driver: channel.rs + SRW lock shape type-checked in a scratch no_std crate; WDF parts not.
