# installer

Goal: package "installer" per docs/agents/contracts.md §8: `crates/split-setup` (lib) +
`installer/` (WiX fragment, NSIS hooks, Tauri bundle snippet, manifests, bun scripts,
licence notices). Nothing executed, nothing committed.

Done (2026-11-12):
- `crates/split-setup`: error, journal, coexist (signals 1-18), drivers, layout, system
  traits, applier, cmds (install / reconcile / uninstall / check-coexistence), cli, fake,
  win/* (probe, registry, fs, firewall COM, devices SetupAPI, runner, WinHTTP fetch,
  accounts, scm). 52 tests.
- `installer/`: drivers.json, stream-server.json, wix/PulsarSplit.wxs, nsis/hooks.nsh,
  tauri-bundle.json, scripts/{fetch-deps,check-payload}.ts + tests (14), licenses/.
- READMEs (installer/, crates/split-setup/), open questions setup-1..9, journal line.

Contract deviations (for the integrator):
- `SystemProbe` += `reg_value_names`; `evaluate_with_own` added.
- `RestoreApplier` += default `live()` / `reboot_pending()`; skips via
  `SetupError::Unsupported/Skipped`. `RestoreHook` += default `needs_termservice_restart`.
- `SetupCtx` is a struct of boxed traits (`win::windows_ctx` builds it);
  `InstallOptions { drivers, consent, register_service, unregister_service, allow_downgrade }`.
- `DriverInstall` += `DetectOnly`; `InfPackage` += `root_hardware_id`, `class_guid`;
  `DriverPackage` += `min_build`, `default_selected`, `archive`, `notes`.
- Journal: `ValueState::{Key, Qword}`, `RecordKind::Feature` (OQ setup-4).

Next: integrator wires `split_setup::cli::parse/run` into `split-service` main, merges
`installer/tauri-bundle.json`, registers RestoreHooks per package.

Verify (all green at repo root 2026-11-12):
- cargo test -p split-setup
- cargo clippy -p split-setup --all-targets (and --features fake)
- cargo check -p split-setup --target x86_64-pc-windows-msvc --all-targets
- bun test installer/scripts

## Fix round 1 (2026-11-12, review findings)

Done (all in crates/split-setup + installer/):
- win/fs.rs: data folder via CreateDirectoryW + SD with owner (DATA_DIR_SDDL now `O:SY...`),
  owner/DACL set by SetSecurityInfo on no-follow handles; untrusted existing folder moved
  aside; reparse points / hard links / foreign-owned children deleted (links, not
  targets); `SetupFs::check_trusted` (new); remove_dir_all fallback never follows links.
- cmds.rs: uninstall replays journals only when `check_trusted(data_root)` passes (else no
  replay, no log, no folder delete); install_official hashes + keeps the exe open with
  share-read only (`drivers::open_verified`) until run; corrupted cached download is
  re-fetched once; driver choice saved to `Setup\Drivers` (REG_MULTI_SZ) and reused when
  `--drivers` is absent; install_inf journals published `oemNN.inf` (done record via
  `journal::journaled_outcome`), previous = MultiSz of pre-existing same-named packages,
  upgrades only packages our journal added, foreign ones untouched.
- applier.rs: `inf:` restore deletes only the recorded published name (original name
  cross-checked), never names in `previous`; accounts without remove-user-data =
  `SetupError::Deferred`.
- journal.rs: k-way merge (seq strictly per package, parsed RFC 3339 across packages),
  done record content wins, `ReplayReport.deferred`, `retire_keeping`.
- firewall: `remove_rule(name, group)`; Windows impl renames our rule then removes it.
- accounts: DeleteProfileW before NetUserDel (feature Win32_UI_Shell).
- installer: stream-server.json / NOTICES / SOURCE-OFFER -> SunshineStream/Sunshine (moved
  repo, evidence in $comment), fetch-deps `validateStream`; NSIS waits for STOPPED (60 s,
  Turkish abort); WiX/NSIS comments on saved driver choice. OQ setup-10, setup-11.

Verify: same four commands as above (64 cargo tests incl. 3 win::fs temp-folder tests
with hard link + junction; 15 bun tests). All green 2026-11-12.
