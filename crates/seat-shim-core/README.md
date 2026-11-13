# seat-shim-core

Shared part of the per-process isolation layer (spec `spec/remote/50-shim-isolation.md`, contract
`docs/split/agents/contracts.md` §4.2). Root workspace member and also a path dependency of the
separate `shim/` workspace (hence explicit package fields, and the `proto` feature that
`shim/` turns off). Design overview, mechanism choice and test status: `shim/README.md`.

| Module | What |
| --- | --- |
| `seatmap` | `SeatMapEntry` <-> registry values under `HKLM\SOFTWARE\PulsarSplit\Runtime\Seats\<session>`; writer/reader traits |
| `decide` | `role`, `classify` / `classify_nodes`, `hid_access`, `filter_focus`, `wnf_known(_with)`, `mask_remote`, `hkcu_redirect`, `hook_plan`, device-path helpers |
| `steam` | `steam_launch_args`, `steam_env`, `steam_cmdline_patch` |
| `guard` | `FaultCounter` (3 faults in 10 min disable a target) |
| `health` | `ShimHealth`, `encode_health` / `decode_health` (u32 LE + JSON, ≤ 4 KiB) |
| `install` | `ShimInstaller`, `AppHelpInstaller` over `ShimSystem`, foreign-shim signals (spec 90 §8.1, 6–10), `user_target_key`, `SystemRestoreApplier` |
| `sdb` | database tree (written by apphelp's writer API), tag ids, GUID layout |
| `launch` | launcher argv, PE bitness, launch plan, `CommandLineToArgvW`-compatible quoting |
| `dxvk` | release download + SHA-256, extraction, per-game deploy / undo with backups, `DXVK_NOTICE` |
| `journal` | restore records (JSON Lines), `restore_plan`, `restore_all`, `rfc3339_utc` |
| `win` | real impls: `RegSeatMap` (volatile keys, SDDL `D:P(A;OICI;KA;;;SY)(A;OICI;KR;;;WD)(A;OICI;KR;;;AC)`), `WinShimSystem`, `AppHelpSdbWriter`, `WinHttpFetcher`, process facts |
| `fake` | in-memory fakes (`fake` feature): registry/file system, seat map, fetcher, journal, clock |

Errors: `ShimError` with stable codes (`shim_blocked`, `shim_foreign`, `shim_kill_switch`,
`shim_target_disabled`, `dxvk_hash_mismatch`, `dxvk_not_downloaded`, …) and
`From<ShimError> for split_proto::ProtoError` (`proto` feature).

Additions to the contract (see `docs/split/agents/open-questions.md` shim-6): `Role::Seat` also
carries `session_id`; `SeatFlags::shared_account`; `classify_nodes`; `hook_plan` +
`ShimConfig`; `wnf_known_with`; `DxvkRelease::default_release()`.

## Build / test

```
cargo test -p seat-shim-core
cargo test -p seat-shim-core --no-default-features
cargo clippy -p seat-shim-core --all-targets
cargo check -p seat-shim-core --target x86_64-pc-windows-msvc
```

Tests use fakes only: no HKLM, services, files outside the temp folder, or network.

## Untested

Everything in `win.rs` that changes the system (seat-map keys with their DACL, `sdbinst`,
module copy / delete-on-reboot, apphelp's writer, WinHTTP download) was compiled but not
run: the dev box rule forbids system changes. Read-only parts (`process_facts`,
`read_shim_config`, `AppHelpInstaller::status`) ran once through `shimctl status`.
