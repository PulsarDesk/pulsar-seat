# seat-setup

Library behind the service sub-commands of spec 90 §9.1 (the integrator wires them into
`pulsar-seat-service remote-setup` (merge step 6; was `pulsar-split-service`); `seat_setup::cli::parse` + `cli::run` take the raw argv):

```text
pulsar-seat-service remote-setup install   [--drivers a,b|none] [--consent a,b] [--register-service] [--allow-downgrade] [--msi]
pulsar-seat-service remote-setup reconcile [same options]
pulsar-seat-service remote-setup uninstall [--remove-user-data | REMOVEUSERDATA=1] [--unregister-service] [--msi]
pulsar-seat-service remote-setup check-coexistence --json
```

Exit codes 0 / 1 failure / 2 partial / 3010 reboot required; `--msi` maps everything but
failure to 0 (Windows Installer EXE custom actions). Each run appends to
`%ProgramData%\Pulsar Seat\logs\remote-setup.log`.

Since merge step 10 (one MSI, `installer/README.md` "Remote seat kind"): the data root is
the one product's `%ProgramData%\Pulsar Seat` (journal / logs with the Seat DACLs, owner
SYSTEM), the third-party payload is `<install>\remote\` (`layout::REMOTE_PAYLOAD_DIR`),
`install` / `reconcile` stop and delete a leftover `PulsarSplit` service
(`layout::LEGACY_SERVICE_NAME`), the generic replay skips seat-compat's `changes.jsonl`
(`layout::FOREIGN_JOURNALS`), and with `InstallOptions::shared_data_root` (set by the
service) `uninstall` leaves the data folder itself to the Seat `uninstall`, which runs this
one first.

## Modules

| Module | Contents |
| --- | --- |
| `coexist` | Spec 90 §8.1 signals 1–18 behind `SystemProbe`, `evaluate` / `evaluate_with_own` (our own journal targets are not residue), modes `Normal` / `Alongside{reference_running}` / `ResidueOnly` (since 2026-11-24, bug 10: the reference product being installed blocks nothing; a running reference service adds the note `duo_service_running`; no repair while it is installed), opt-in `RepairItem`s from the §8.3 Windows-defaults table (TermService items last, "report only" rows never repaired), upstream Sunshine port block. |
| `drivers` | `DriverPackage` manifest (+ validation), `verify_sha256`, `fetch_verified`, `pnputil_add/delete/enum` argv, `parse_enum_drivers` (English labels, shape fallback for localized Windows), `detect` (UpgradeCode, hardware id, published INF). |
| `journal` | Record shape of contract §0.3 (`ValueState` adds `key` and `qword`), `FileJournal` (append + `sync_data` per record), `journaled(intent → change → done)`, `replay_all` / `replay_dir` (newest first; intent without done compares live state), `compact`, `retire`. |
| `applier` | Generic `RestoreApplier` over the system traits: registry (values and keys we created), files (delete / backup copy / small content), firewall rules, our drivers (`inf:<name>`, `device-hwid:<id>`), accounts (only with remove-user-data). Other kinds are left to package hooks. |
| `cmds` | `SetupCtx`, `cmd_install`, `cmd_reconcile`, `cmd_uninstall`, `cmd_check_coexistence`, `RestoreHook` / `ReconcileHook`, `RunReport`. |
| `system`, `layout` | Write-side traits (`SetupRegistry`, `SetupFs`, `FirewallGroup`, `DeviceManager`, `UserAccounts`, `ServiceRegistrar`, `Clock`), paths, SDDL, registry names, own device markers. |
| `win` | Real impls: winreg (64-bit view), SCM (windows-sys), cert store lookup, firewall COM `INetFwPolicy2` (windows 0.62), SetupAPI / CfgMgr / newdev devnodes, `std::process` runner (argv only), WinHTTP fetcher (HTTPS only, no HTTPS→HTTP redirect), `DeleteProfileW` + `NetUserDel`, `MoveFileEx` delay-delete, data folder via `CreateDirectoryW` with owner + DACL and `SetSecurityInfo` on no-follow handles (untrusted owners, reparse points and hard links in the data folder are moved aside / deleted, never followed; uninstall replays journals only from a trusted data folder). `windows_ctx(...)` builds a `SetupCtx`. |
| `fake` | In-memory fakes of every trait (feature `fake`). |

Record conventions used by the generic applier are documented at the top of
`src/journal.rs`. Packages that need other undo logic register a `RestoreHook` named
like their journal `package`; a hook that succeeds makes the generic replay skip that
file.

## Build / test

```powershell
cargo test   -p seat-setup
cargo clippy -p seat-setup --all-targets
cargo clippy -p seat-setup --all-targets --features fake
cargo check  -p seat-setup --target x86_64-pc-windows-msvc --all-targets
```

Tests never change the machine. The `win` tests are read-only (registry, SCM, cert store,
firewall and PnP enumeration) or work in a temp folder.

## Untested (needs an elevated, disposable test machine)

- Every write in `win`: HKLM writes / deletes, firewall rule removal, devnode creation
  (`create_root_device`) and removal (`DiUninstallDevice`), `pnputil` add/delete, service
  create/delete, `DeleteProfileW` / `NetUserDel`, delete-on-reboot, data-folder owner +
  DACL with the production SDDL (`O:SY`, needs SYSTEM), firewall rename-then-remove of
  our own rules (`INetFwRule::SetName`).
- HYPOTHESIS: `CreateProcess` accepts an image held open by setup with share mode
  read-only (`drivers::open_verified`, kept open until the upstream installer started);
  the loader asks only for read/execute sharing.
- `WinHttpFetcher` against a real server (only URL parsing is tested).
- `pnputil` output on a non-English Windows (the parser falls back to value shapes).
- Upstream installer (usbip-win2) silent switches and exit codes.

## Licence

GPL-3.0-only. Dependencies: serde, serde_json, thiserror, log, sha2 (MIT/Apache-2.0),
winreg (MIT), windows / windows-sys (MIT/Apache-2.0); tempfile (dev, MIT/Apache-2.0).
