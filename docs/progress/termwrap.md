# termwrap
Goal: crates/split-termwrap per docs/agents/contracts.md §2 (EXPERIMENTAL, off by default,
path B1 = integrate an operator-installed open-source wrapper; no own termsrv patcher).
Done (2026-11-12):
- Crate complete: version, support (rdpwrap.ini parser + gate), pe, paths, journal (JSONL,
  guarded reverse replay, b64), state (HKLM\SOFTWARE\PulsarSplit\Termwrap), system traits,
  engine (WindowsTermwrap: status/enable/disable/boot_check/restore_all), fake (FakeWorld),
  own_wrapper (feature, always `unsupported`), win/{registry,scm,files,host}.
- 57 tests green, clippy -D warnings clean (aarch64 + x86_64), x64 check ok.
- README.md, open questions termwrap-1..4, journal line.
- Public facts used (stascorp/rdpwrap, Apache-2.0, read on GitHub, nothing copied):
  %ProgramFiles%\RDP Wrapper\rdpwrap.dll + rdpwrap.ini next to DLL; sections [<ver>] and
  [<ver>-SLInit]; unknown version = runs unpatched; x86/x64 only.
In progress: none.
Next: integrator wiring (IPC get_termwrap_status / set_termwrap, boot_check before
  prepare_host, setup RestoreHook -> restore_all). Real-box test of enable/disable.
Verify: the root workspace failed to load on 2026-11-12 because crates/split-pads had no
  targets (another package mid-write). Builds were run in a scratch workspace copy
  (split-proto + split-termwrap, same workspace.package values, same Cargo.lock):
  cargo test -p split-termwrap [--all-features]
  cargo clippy -p split-termwrap --all-targets [--all-features] -- -D warnings
  cargo clippy -p split-termwrap --all-targets --all-features --target x86_64-pc-windows-msvc -- -D warnings
  Re-run these from the repo root once the workspace loads.

## Fix round 1 (2026-11-12) — review findings: DONE
- journal.rs: ACL-checked JsonlRestoreLog (protected dir via win::files::create_dir_with_sddl,
  dir + open-handle owner/DACL check, fail closed); `without_acl_check` for tests.
- engine.rs: owned() from HKLM state only; replay allow-list (3 targets), ServiceDll
  previous re-validated (stock or passing wrapper, else stock); unusable journal -> stock
  fallback; switch_off stores Enabled=0 + RestorePending first, clears it after restart;
  restore_all no-op on foreign ServiceDll; boot_check finishes RestorePending.
- state.rs: RestorePending DWORD. acl.rs (new, pure): wrapper allow-list + journal rules.
- win/files.rs: owner+DACL reader, ancestors walk with roles, local_fixed_path
  (GetFinalPathNameByHandleW + DRIVE_FIXED), revocation WHOLECHAIN cache-only.
- win/scm.rs: pending-state aware stop/start, stop-phase unwind, last_error captured once.
- Tests: 73 default / 74 all-features green; clippy -D warnings clean aarch64 + x86_64;
  x64 check ok (scratch workspace copy; root workspace still fails on split-pads).
- Open question termwrap-5 (owner rule vs admin-user-owned installs).
Next: unchanged (integrator wiring, real-box test).
