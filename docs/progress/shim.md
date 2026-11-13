# shim
Goal: package "shim" per docs/agents/contracts.md §4: `crates/split-shim-core` (pure decisions +
small Windows parts, root member) and `shim/` (separate workspace: module cdylib, launcher,
shimctl). Spec 50.
Ownership note: the workflow task text said "shim/ only"; contracts.md §0.1 gives the shim
package `crates/split-shim-core` too, and §4.2/§10 require it there. Followed the contract.
Done:
- Read AGENTS, CLEANROOM, rules, plan, contracts §0/§4/§5.3/§9/§10, third-party, spec 50,
  spec 60 §3.6/§8, spec 90 §8.1.
- Fresh GUIDs: SHIM_DB_GUID d9a4fcd5-83a4-45c2-8caa-b3a4f973ad03,
  ETW provider 81916b1b-e779-43f0-9ba0-6ecf949e9db9.
- crates/split-shim-core written: lib, error, regval, seatmap, decide, steam, guard, health,
  journal, launch, sdb, install (+SystemRestoreApplier), dxvk, fake, win. 61+ unit tests green
  in a scratch copy (root workspace was temporarily unloadable: crates/split-pads had no
  src/lib.rs while the pads agent was writing it). clippy clean, x64 check ok.
- Contract deviations (additive): Role::Seat has `session_id`; SeatFlags has `shared_account`;
  extra fns classify_nodes/hook_plan/wnf_known_with/ShimConfig; DXVK default release 3.1.1.
- shim/ workspace written: module (rt/: mod, hook, device, hid, focus, mask, steam, reg,
  health, log, fault; filter.rs pure), launcher (main + win), shimctl (cmd + win).
  retour 0.3.1 fails on i686 -> pinned =0.4.0-alpha.4.
- Green: core 63 tests, clippy 0; shim check x64+i686, clippy x64/i686/aarch64 0 warnings,
  cargo test host + x64 + i686 (emulated). Release DLLs link; exports undecorated.
- Smoke (no system change): launcher x64/x86 injected into own shimctl, PulsarSplitInit ran.
- READMEs: shim/, shim/{module,launcher,shimctl}/, crates/split-shim-core/.
- open-questions shim-1..7 and journal line appended.
In progress: none (final verification).
Next: integrator wires SeatMapWriter/health pipe/installer (contracts §4.4).
Verify:
- cargo test -p split-shim-core ; cargo clippy -p split-shim-core --all-targets
- cargo check -p split-shim-core --target x86_64-pc-windows-msvc
- cd shim; cargo check --target x86_64-pc-windows-msvc ; cargo check --target i686-pc-windows-msvc
- cd shim; cargo test (pure parts of launcher / module)

## Fix round 1 (review findings, 2026-11-12)
Scope: shim/ + crates/split-shim-core (package per contracts §0.1). Nothing committed.
Status: all items below done; verification green (see end).
- [x] core: KEY_WOW64_64KEY in every HKLM open/create/delete (win.rs hklm_open /
      hklm_create / hklm_delete_tree; test every_hklm_access_uses_the_helpers)
- [x] core: winfs.rs (ensure_private_dir, check_file, check_tree, write_replace);
      WinShimSystem::write_sdb via temp + MoveFileEx, run_sdbinst checks the .sdb;
      win::open_journal / ensure_dxvk_root; install::restore_file_allowed;
      dxvk::ReleasePin + pin_release/store_pin/load_pin/verify_pinned, FsDxvkDeployer.pin
- [x] core: decide::is_filtered_device_path GLOBALROOT order fix
- [x] shimctl: secure journal (read-only commands skip it), pinned DXVK download/deploy
- [x] module: hook.rs rewrite (install_first, load on launcher path, deferred via
      LdrRegisterDllNotification), threads.rs (freeze others, HeapLock while suspending)
- [x] module: hid.rs NtCreateFile/NtOpenFile + kernelbase CreateFileW/A/2 fallback,
      filter::may_be_device_path prefilter (tested against the core rule)
- [x] module: steam/reg/mask kernelbase-first; mask console id from KUSER_SHARED_DATA
- [x] module: mod.rs live() refresh outside locks (REFRESHING / REFRESH_BUSY), InitPath,
      `failed` covers all groups, log::set_verbosity
- [x] module: fault.rs prepared frames + NtCreateFile/NtWriteFile; health.rs access mode +
      SQOS; log.rs verbosity gating + etw_error
- [x] launcher: Remote/Injected enums, timeout -> terminate + restart uninjected (buffer
      leaked), System32 copy when layered, szExePath full-path match, success by base
- [x] docs: shim/README.md, open-questions shim-8..12, journal line
Skipped: PEB command-line patch for Steam (kernelbase caches the command line during
process init, which our own remote thread triggers; documented HYPOTHESIS in steam.rs).
Verify (2026-11-12, all green): core test 69 ok, clippy 0, x64 check ok; shim clippy
x64/i686/aarch64 0 warnings; shim test host/x64/i686 ok; release x64+x86 build; launcher
smoke into shimctl status (x64, x86) exit 0.
