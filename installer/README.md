# installer/

Packaging of Pulsar Seat (spec 90 §11). Owner: the **installer** package (with
`crates/seat-compat`, which holds the install-time logic, and `crates/seat-service`, whose
`preflight` / `install` / `uninstall` sub-commands the MSI runs). `bun test
installer/scripts` covers the scripts.

> **Status (2026-11-13):** the MSI **builds** on the ARM64 dev box with
> `installer/scripts/build-installer.ps1` (Tauri bundler + the WiX 3.14 it downloads into
> `%LOCALAPPDATA%\tauri\`; no WiX / NSIS / dotnet installed system-wide). The built MSI
> was extracted (`msiexec /a`) and PE-checked, **never installed**: every elevated step
> (custom actions, service record, restore point, tweaks, uninstall revert) is untested on
> hardware. Nothing is signed.
>
> **2026-11-14 (merge step 10):** one MSI per arch for both seat kinds. Both MSIs built on
> the ARM64 dev box (unsigned), extracted and PE-checked: ARM64 package all native 0xAA64
> except the app API helpers of other arches and the x86 usbip-win2 installer stub, no shim;
> x64 package all 0x8664 except `shim\x86\` and the same stubs. SHA-256 in
> `docs/agents/status.md`. Never installed.
## Files

| File | What |
| --- | --- |
| `wix/PulsarSeat.wxs` | WiX v3 fragment merged into the Tauri MSI: service exe + `ServiceInstall` (`PulsarSeat`, LocalSystem, own process, automatic, depends on `RpcSs` + `LSM`, SID type unrestricted; **no recovery actions in the MSI**: Windows Installer opens the service without `SERVICE_START`, so failure actions fail with MSI error 1939; the deferred `install --msi` custom action (`scm::configure_existing`) sets restart 5 s / 30 s / 60 s then none, reset 1 day, the required privileges and the service DACL, HYPOTHESIS until the fixed MSI runs in the VM), host + agent exes, the credential provider DLL `pulsar-seat-credprov.dll` (file only: **never registered by the MSI**; the service registers it while enabled under `fast_user_switch`, credprov/README.md), app API helper DLLs (`apihelper\<arch>\`), machine Run value, `HKLM\SOFTWARE\Pulsar Seat` (`InstallPath`, `Version`), empty `drivers\` folder, deferred no-impersonate custom actions `install` / `uninstall` + a **rollback** action, arch / build / admin / coexistence / property-validation conditions, a yes / no dialog before running seats are stopped (upgrade, repair, removal; full UI only) and a non-blocking TermService warning dialog. |
| `wix/locales/en-US.wxl`, `tr-TR.wxl` | Every visible string of the fragment (`!(loc.Id)`): English (release default) and Turkish, same ids (`scripts/wix-locales.test.ts`). `tauri-bundle.json` builds `en-US`; `build-installer.ps1 -Language tr-TR` builds a Turkish MSI named `PulsarSeat_<version>_<arch>_tr-TR.msi` (`bundle-config.ts --language`). Tauri adds its own default strings for ids the file does not set (HYPOTHESIS until the next MSI build). The NSIS hooks carry LangStrings for 1033 / 1055. |
| `nsis/hooks.nsh` | Tauri NSIS `installerHooks` for a `-setup.exe`. **Not shipped** (the exe installer is the native `setup.exe`, `scripts/build-setup.ps1`): Tauri's NSIS stub is 32-bit x86 (emulated on ARM64), which `docs/agents/rules.md` does not allow; kept correct for a maintainer-approved exception. Arch guard, build ≥ 19045, refuse beside the reference product, TermService warning (full expanded path), **full pre-flight gate from `$PLUGINSDIR` before any file is laid down**, stop seats on upgrade (asks first), Turkish yes / no questions, validated + quoted arguments, cleanup on a late refusal. |
| `tauri-bundle.json` | Snippet merged into `apps/manager/src-tauri/tauri.conf.json` (`productName` "Pulsar Seat", `bundle.active`, **MSI only**, fixed `upgradeCode`, `tr-TR`, fragment + component refs, licences, WebView2 bootstrapper). CI passes it with `tauri build --config`. |
| `scripts/check-payload.ts` | Release gate: PE machine of every `.exe/.dll/.sys` is the target arch (0xAA64 / 0x8664, read from the PE header) except `apihelper/<x86\|x64\|arm64\|arm64ec>/`, which must match its folder; no Microsoft runtime DLLs (static CRT); no certificates / keys; drivers only with `--allow-drivers` and then only as INF + CAT package; vulnerable drivers by a name pre-check and, with `--blocklist <SiPolicy.xml>`, by Authenticode hash / file name against Microsoft's recommended driver block rules. `--machine-only` = PE machine check only (CI drivers job). `--release`: `pulsar-seat-credprov.dll` (LogonUI, SYSTEM) must carry an Authenticode certificate table (`unsigned_system_dll`). Remote kind (union with Split's check, merge step 10): web-UI assets refused, more Microsoft files (`rfxvmt`, `AcRes`, `d3dcompiler_NN`), the pinned packages of `remote/drivers/<id>/` allowed without `--allow-drivers` (still INF + CAT; `*.sys` warn for review; an upstream installer `.exe` there may be x86), `shim/x64/` = x64, `shim/x86/` = x86 (both packages), `shim/arm64/` = ARM64 (ARM64 package only). ARM64 package: `pulsar-seat-credprov.dll` must be ARM64X (`not_arm64x`; error with `--release`, warning otherwise). |
| `scripts/js-notices.ts` | Licence notices of the JS packages bundled into the manager frontend. |
| `scripts/bundle-config.ts` | Writes the snippet without `$comment` keys for `tauri build --config`, plus the generated fragment `PulsarSeatDrivers.wxs` (`ComponentGroup PulsarSeatDriverPackages`: one `drivers\<package>\` folder per staged signed package, empty otherwise), the remote payload fragment `PulsarSeatRemote.wxs` (`--remote <dir>`; empty group without it) and an optional `--version`. |
| `scripts/build-installer.ps1` | The whole MSI build for one arch (local and CI): release binaries (static CRT), app API helpers, optional `-SignedDrivers <dir>` (staged only when every catalog is Authenticode-valid and signed by "Microsoft Windows Hardware Compatibility Publisher", and every `.sys` / `.dll` either a member of that catalog (`signtool verify /pa /c`) or embedded-signed by the same publisher; an embedded signature from anyone else refuses the package), **version gate** (Cargo workspace = `tauri.conf.json` = `package.json` = `-Version`), payload gate, bundle config, `tauri build --bundles msi`, `msiexec /a` extraction + payload gate on every file inside, `target\installer\dist\PulsarSeat_<v>_<arch>.msi` + `SHA256SUMS.txt`. Remote kind: builds the shim (x64 package: `shim\x64`, `shim\x86`) and runs `fetch-deps.ts` into `<payload>\remote` (`-NoRemoteDeps` leaves it out: test builds only, refused with `-Release`). `-SkipCargo` uses an existing payload (CI; the x64 shim comes from the `shim-x64` artifact), `-NoArm64ec` leaves the ARM64EC helper out (test builds on a toolchain without that target only; refused with `-Release`; without it a missing `arm64ec-pc-windows-msvc` target fails the build), `-Release` sets `PULSAR_SEAT_RELEASE` (the service then refuses to compile while `release-signers.txt` lists no key), checks the key list (`pulsar-seat-release check-signers --require`), the source-offer tag and the generated third-party notices (also inside the MSI: `check-payload.ts --notices`). Every build writes `<msi>.build.txt` (version, arch, commit, dirty, release, service SHA-256) next to the MSI. `-SignedCredProv <dll>` bundles a validly signed credential provider DLL; with `-Release` an unsigned one only warns (CI has no signing; `sign-release.ps1` refuses it). |
| `scripts/write-sums.ps1` | `SHA256SUMS.txt` for the **signed** MSIs and setup programs of a release (warns about any file without a valid signature; removes a stale `SHA256SUMS.txt.minisig`). |
| `release-signers.txt` | Release signing **public** keys (minisign Ed25519, one `RWQ…` line each) compiled into the service's update check (`include_str!`, `update::ACCEPTED_SIGNERS`). Holds the real key since 2026-11-13 (docs/release/checklist.md §2). |
| `scripts/release-keygen.ps1` | Once, maintainer only: `pulsar-seat-release keygen` → secret key **outside the repo** (default `%USERPROFILE%\.pulsar-seat\release-key\minisign.key`, never printed, ACL current user + SYSTEM; refuses a folder inside any source tree and never overwrites), prints the public key line for `release-signers.txt`. |
| `scripts/sign-release.ps1` | Per release: checks that every MSI in `SHA256SUMS.txt` is present, has that hash and a valid Authenticode signature, then writes `SHA256SUMS.txt.minisig` with the offline key and verifies it against `release-signers.txt`. It also extracts each MSI (`msiexec /a`) and requires a Valid signature on the `pulsar-seat-credprov.dll` inside plus `check-payload.ts --release --notices`. Refuses any MSI without its `.build.txt`, built from no / another / a dirty commit or not from tag `v<version>`, not a `-Release` build, or whose service binary differs from the record (stale MSI), and any MSI in the folder that `SHA256SUMS.txt` does not list. The signed trusted comment carries `version:<x.y.z>` (from the MSI names). Listed setup programs must be validly signed `-Release` builds of the same commit whose record names exactly the listed MSI's hash. `-CheckOnly` runs every check without opening the key. |
| `scripts/lint-ci.ts` | Structural lint of `.github/workflows/ci.yml` (Bun YAML parser: keys, `needs`, matrix / env references, `uses` pins, pwsh steps that would ignore a failing native command). `--local ci-local.ps1`: every workflow job has a `# ---- <job> job` section there and every installer script a step runs is run there too or listed in a `# ci-local skips:` line with its reason. |
| `scripts/ci-local.ps1` | The CI gates on this machine in CI order (lint, build / test / clippy per target, manager, API helper, bun tests, shim x64 test + clippy and x86 check; `-Payload`, `-Drivers`, `-Installers`, `-Release` for the rest). Installs nothing. The shim and API helper workspaces build into the shared `CARGO_TARGET_DIR` here (debug only); `build-installer.ps1` gives their **release** builds own sub-folders (`<target>\shim`, `<target>\apihelper`) because Tauri's MSI bundler picks up every DLL next to the manager exe in `<target>\<triple>\release`. |
| `scripts/build-credprov.ps1` | Credential provider: msbuild `credprov.vcxproj` + `credprov-selftest.vcxproj` for ARM64 / x64 (v145, SDK 10.0.28000), PE machine check, self-test run (x64 emulated on an ARM64 host), `-OutDir` copies the DLL into a payload. Called by `build-installer.ps1` (not with `-SkipCargo`: then the DLL must already be in the payload). Registers nothing. |
| `scripts/build-drivers.ps1` | Drivers package: msbuild ARM64 + x64 with WDK 10.0.28000 (seat-input-filter, seat-idd with both IddCx roles, seat-storage-filter, seat-wfp source-IP WFP callout) plus cargo + stampinf + inf2cat for the Rust `split-pad`; zero warnings, infverif, ApiValidator, PE machine (CI drivers job). Never signs or loads. |
| `scripts/build-setup.ps1` | Native setup program `PulsarSeat_<v>_<arch>[_<culture>]_setup.exe` (`crates/seat-bootstrap`) with the MSI of the same arch embedded (`PULSAR_SEAT_SETUP_MSI`), static CRT, own target sub-folder; checks PE machine, the asInvoker manifest and `/payload-info` (when the host runs the arch); writes `<exe>.build.txt` (MSI name + SHA-256). `-Release` refuses an MSI without a valid signature. `build-installer.ps1` step 9 runs it (`-NoSetup` skips). |
| `scripts/sign-authenticode.ps1` | Authenticode hook for the signing machine: `signtool sign /fd SHA256` + RFC 3161 timestamp, certificate by thumbprint (store, token / smart card) or a cloud `/dlib` plug-in; folders are expanded to `.exe .dll .sys .msi .cat .cab`, `remote\` (pinned third-party payload) and already-signed files are left alone; a real certificate must verify `Valid` afterwards. A TEST certificate signs only below `target\sign-dryrun\`. |
| `scripts/sign-dryrun.ps1` | End-to-end signing dry run with the TEST certificate on copies in `target\sign-dryrun\<stamp>` (docs/release/signing-dry-run.md); proves every gate refuses test signatures and the real dist stays unchanged. |
| `scripts/driver-cab.ts` | Partner Center submission CABs of our drivers (INF + binaries + PDBs, `<driver>\<Platform>\` folders, makecab DDF, CAB file table read back, `manifest.json`); refuses the storage filter's `UNALLOCATED` altitude unless `--allow-unallocated-altitude`. Unsigned (docs/release/driver-submission.md). |
| `scripts/repo-scan.ts` | Public-repository scan of everything git would publish: local user paths, keys, tokens, binaries, required licence / contributing files, SPDX headers (docs/release/public-repo.md); allow list `repo-scan-allow.txt`. |
| `scripts/*.test.ts` | `bun test` (synthetic PE headers, temp folders only). |
| `licenses/` | `SOURCE-OFFER.txt` (GPL-3 §6), `NOTICES.md`, `about.toml` + `about.hbs` (cargo-about); the generated `THIRD-PARTY-NOTICES-*.md` (not tracked). |
| `remote/drivers.json` | Remote kind: third-party driver manifest (`DriverPackage[]`): URL, pinned SHA-256, licence, install kind, detection, minimum build. Defaults from `docs/split/agents/third-party.md`. |
| `remote/stream-server.json` | Remote kind: stream-server manifest (upstream Sunshine, pinned tag of remote spec 80), one portable zip per arch with its SHA-256, the web-UI folders and `drop_paths` (upstream's `scripts\`: service install, firewall allow rules, bundled ViGEmBus installer; and `tools\sunshinesvc.exe`) left out. |
| `scripts/fetch-deps.ts` | Remote kind (from Pulsar Split): downloads the pinned artefacts over HTTPS, verifies SHA-256, unpacks with the system `tar.exe`, checks each INF (arch section + root hardware id), drops the stream server's web UI and `drop_paths`, writes `target/installer-deps/<arch>/remote/` (resolved `drivers.json`, `drivers\<id>\`, runtime `stream-server.json`, `stream-server\`), then runs `check-payload.ts` on it. Cache: `target/installer-deps/cache/<sha256>-<file>`. `--stream-local target/wf/sunshine-out/<arch>` keeps the upstream payload (assets, shaders) but takes the exe and runtime manifest of the Pulsar build (`stream-server/build-sunshine.ps1`; flavour `pulsar_fork`, version `<pinned tag>-pulsar.N`, exe hash, GPL licence checked). |

## Target

- One MSI per architecture (ARM64, x64); msiexec runs natively on both. Each refuses the
  other machine type by the native machine (`PROCESSOR_ARCHITECTURE` of the 64-bit
  machine environment in the MSI, `IsNativeARM64` in NSIS, `IsWow64Process2` /
  `GetMachineTypeAttributes` in the pre-flight, which reports "unknown" rather than
  assuming a match), never the WOW64 variable.
- Per-machine install under `%ProgramFiles%\Pulsar Seat\`; data under
  `%ProgramData%\Pulsar Seat\` (`seat-core::consts::DATA_DIR_SDDL`; `run\` and `journal\`
  `PRIVATE_DIR_SDDL`). The journal sets owner Administrators + protected DACL itself
  (`SetNamedSecurityInfoW`, re-applied to existing children) and refuses a folder or
  file it cannot trust (see `crates/seat-compat/src/journal.rs`).
- MSI quiet install: `msiexec /i PulsarSeat_<v>_<arch>.msi /qn [PSRESTOREPOINT=1]
  [PSTWEAKS=fast_startup_off,sleep_off,small_crash_dumps]`. `PSTWEAKS` may hold only
  lowercase ids and commas; `PSRESTOREPOINT` / `REMOVEUSERDATA` only 0 or 1 (launch
  conditions). Reboot needed: logged in `setup.log` (the custom action maps 3010 to 0);
  a build that stages signed driver packages also has `ScheduleReboot` (`PSHASDRIVERS`
  from the generated driver fragment), so `msiexec /qn /norestart` returns **3010**. Without
  `/norestart` such a `/qn` run restarts the PC at the end: always pass `/norestart` in
  deployments. `/qn` and `/qb` upgrades stop running seats **without asking**; full UI
  asks first.
- MSI full UI: an options page after the install folder (restore point, fast startup off
  — offered only while fast startup is on —, sleep off, small crash dumps; all off by
  default) sets `PSRESTOREPOINT` / `PSTWEAKS`; removal from the MSI's own maintenance UI
  shows a "delete settings and logs too" page (`REMOVEUSERDATA`). Removal from Settings
  runs with reduced UI and keeps `%ProgramData%\Pulsar Seat`; quiet removal with data:
  `msiexec /x PulsarSeat_<v>_<arch>.msi /qn REMOVEUSERDATA=1`. The update download cache
  (`%ProgramData%\Pulsar Seat\updates\`) is deleted on every uninstall.
  Upgrades keep the config and the LSA secrets. Every uninstall deletes the seat passwords
  and the seat-host accounts and Pulsar's kiosk accounts (`PulsarKiosk<NN>`; their
  profiles only with `REMOVEUSERDATA=1` since the parity review, 2026-11-17) with their LSA
  secrets, also with `REMOVEUSERDATA=0` (no stored password outlives the product). While
  installed, an unused kiosk account is only disabled at service start (its profile stays)
  and deleted on the manager's explicit "Delete the seat's kiosk user…": a reinstall finds the kept configuration (seats,
  backends, revision) but the seat passwords must be set again (VM test 2026-11-16).

## Build

```powershell
# ARM64 (x64: -Arch x64). Needs rustup targets aarch64/x86_64/i686(-pc-windows-msvc)
# (+ arm64ec for a release ARM64 package), bun, and network access for Tauri's first
# WiX download (%LOCALAPPDATA%\tauri\WixTools314, user-local).
powershell -ExecutionPolicy Bypass -File installer\scripts\build-installer.ps1 -Arch arm64
# Signed driver packages (from the hardware dashboard), one folder per package:
powershell -ExecutionPolicy Bypass -File installer\scripts\build-installer.ps1 -Arch x64 -SignedDrivers C:\signed\x64
```

Licence texts: CI generates `installer/licenses/THIRD-PARTY-NOTICES-{RUST,JS}.md`
(`cargo about`, `js-notices.ts`) before the bundle; a local test build without them still
bundles `installer/licenses/` (GPL text, `NOTICES.md`, `SOURCE-OFFER.txt`, the remote kind's upstream licence texts) but is **not**
releasable.

The x64 MSI can be built on the ARM64 dev box (cross-compiled binaries, same WiX); its
`candle -arch x64` output is the same as on an x64 host.

## Signing and release

Full ordered list, including accounts and approvals: [docs/release/checklist.md](../docs/release/checklist.md).

1. CI (tag `v*` or manual) uploads `installers-unsigned-<arch>` (MSI + partial
   `SHA256SUMS.txt`). Unsigned artefacts are for testing only.
2. The maintainer signs every exe / dll **before** bundling (or re-bundles with signed
   binaries) and the MSI itself, with a publicly trusted code-signing certificate and an
   RFC 3161 timestamp (`installer\scripts\sign-authenticode.ps1`). No certificate is ever
   installed on a user machine. Rehearsal with the test certificate:
   `installer\scripts\sign-dryrun.ps1` (docs/release/signing-dry-run.md).
3. `installer\scripts\build-setup.ps1 -Arch <arch> -Msi <signed msi> -Release` per arch,
   then sign the two `PulsarSeat_<x.y.z>_<arch>_setup.exe`.
4. `installer\scripts\write-sums.ps1 <dir>` → `SHA256SUMS.txt` of the signed MSIs and setups.
5. `installer\scripts\sign-release.ps1 -Dist <dir>` → `SHA256SUMS.txt.minisig` (offline
   Ed25519 release key, minisign format; the `minisign` tool works too).
6. Publish the GitHub release `v<x.y.z>` (not a pre-release, not a draft) with the assets
   `PulsarSeat_<x.y.z>_arm64.msi`, `PulsarSeat_<x.y.z>_x64.msi`, the two `_setup.exe`,
   `SHA256SUMS.txt`, `SHA256SUMS.txt.minisig`.
   Before the first release: create the release key once
   (`installer\scripts\release-keygen.ps1`) and add its public key to
   `release-signers.txt`. Tag builds set `PULSAR_SEAT_RELEASE` and fail to compile while
   that list is empty. Rotation: ship a release that lists both keys first. Optional:
   pin the Authenticode leaf in `AUTHENTICODE_PINS` (`crates/seat-service/src/update.rs`;
   SPKI SHA-256 + subject + issuer).
6. The tag `v<x.y.z>`, the Cargo workspace version, `tauri.conf.json` and
   `apps/manager/package.json` must match (CI "Version consistency" step and
   build-installer.ps1 refuse a mismatch).
7. **Source offer (GPL-3 §6).** `licenses/SOURCE-OFFER.txt` points at
   `https://github.com/PulsarDesk/pulsar-seat` and the tag of the version. Today the
   repository is local only (no remote, no commits): **no MSI may be distributed before
   that public repository and the tag `v<x.y.z>` exist.** `build-installer.ps1 -Release`
   checks it (`git ls-remote --tags`) and refuses the build otherwise.
8. Build the service and the host in separate cargo invocations (build-installer.ps1 and
   ci.yml do): a joint build unifies features and links IronRDP into the service.

## Updates (spec 90 §11 item 10)

- Opt-in: manager → **Güncellemeler** → "Yeni sürümleri denetle (GitHub)"
  (`settings.updates.check`, default off; optional weekly check `settings.updates.weekly`,
  which only reports). While off the service opens no connection.
- The **service** (not the manager) asks
  `https://api.github.com/repos/PulsarDesk/pulsar-seat/releases/latest` over WinHTTP
  (system TLS 1.2+ and proxy, generic user agent, nothing about the PC sent), follows
  redirects by hand only to `github.com`, `objects.githubusercontent.com`,
  `release-assets.githubusercontent.com`, skips drafts / pre-releases / tags that are not
  `vX.Y.Z`, and offers only a newer version that has `PulsarSeat_<v>_<native arch>.msi`.
- The release's `SHA256SUMS.txt` is used only with a valid `SHA256SUMS.txt.minisig` from a
  key in `release-signers.txt` (`ACCEPTED_SIGNERS`, compiled in; pre-hashed minisign
  signatures only). Missing / malformed signature: `sums_unsigned`; unknown key or bad
  signature: `sums_untrusted`; signed trusted comment without `version:<tag version>`:
  `sums_mismatch`; signed `timestamp` older than one this PC accepted before:
  `sums_rollback`; empty key list: `no_release_key` (nothing is ever offered). Limit: the
  release page owner can still withhold updates, or offer an older signed release that is
  newer than the installed one to a PC that never saw a newer signature
  (crates/seat-service/src/update.rs module doc).
- Download (admin request) into `%ProgramData%\Pulsar Seat\updates\` (SYSTEM +
  Administrators only), at most 512 MiB; then SHA-256 against that signed checksum file
  **and** `WinVerifyTrust` (whole-chain revocation); when `AUTHENTICODE_PINS` is not empty
  the leaf must also match a pin (key SHA-256 + subject + issuer). Any failure deletes the
  file. Unsigned installers are never offered.
- Install: only after the administrator confirms in the manager (seats stop); hash and
  signature are checked again, then `msiexec /i <msi> /qn /norestart /l*v
  %ProgramData%\Pulsar Seat\logs\update-msi.log` starts detached. The MSI's own upgrade
  path stops the service and seats, keeps the configuration and LSA secrets; a needed
  reboot is recorded in `setup.log` and shown by the manager. The service answers at
  once (`installing`); a worker re-verifies without holding the updater lock and waits
  for msiexec. If msiexec ends while the service still runs (e.g. 1618 another install,
  a launch condition), the state becomes `failed` / `install_failed` (exit code + log
  path; `install_timeout` after one hour) and the verified file stays for a retry.
- Never automatic, never a silent driver swap. Untested against a real release (none
  published yet).

## Install sequence

1. Conditions (MSI) / PREINSTALL (NSIS): admin, native arch, build ≥ 19045, **refuse**
   while the reference product's service keys exist (`MUTESV_SERVICE`, `MUTENX_SERVICE`
   under `HKLM\SYSTEM\CurrentControlSet\Services`; interop identifiers used only here and
   in `seat-compat::coexist`), warn about a non-default `TermService` `ServiceDll`
   (RDS-loopback seats are then refused at run time). NSIS runs the full gate
   (`pulsar-seat-service preflight`, extracted to `$PLUGINSDIR`) here, before any file.
2. Upgrade / repair: the service is stopped (it stops every seat first). Both installers
   ask before (full UI).
3. Files, service record (MSI `ServiceInstall`; NSIS `install --register-service`).
4. `pulsar-seat-service install --msi "--restore-point=<0|1>" "--tweak=<ids>"
   "--drivers=<dir>."` (deferred, LocalSystem; the MSI passes `[PulsarSeatDriversDir].` so the
   quoted argument never ends in `\"`). Service part: description, required
   privileges, explicit service SDDL, data dir + ACL, Event Log source `PulsarSeat`.
   Compat part (`seat_compat::setup::install`, `InstallOptions::parse`):
   - full pre-flight (`seat_compat::preflight::run`) — any `fail` refuses (exit 4); the `hyperv_vm`
     line only reports whether Hyper-V is present (VM seats); nothing is enabled at install;
     `warn` results go to `%ProgramData%\Pulsar Seat\logs\setup.log`
     (`setup::write_setup_log`) for the installer and the manager's first start;
     Secure Boot / Memory Integrity / Smart App Control are reported only;
   - optional System Restore point before any system setting changes;
   - optional tweaks, each journaled with its previous value: `fast_startup_off`,
     `sleep_off` (the active plan), `small_crash_dumps`;
   - driver packages under `<install>\drivers\<name>\` (must be inside the install
     folder): only INF + CAT packages with a models section for the native arch whose
     **INF is a member of the catalog** and whose catalog verifies and is signed by
     "Microsoft Windows Hardware Compatibility Publisher". Published with
     `SetupCopyOEMInfW`, journaled with the published INF's hash, installed with `pnputil
     /add-driver … /install`. A store package not in our journal is foreign unless its
     original name and hash equal our payload (crash between publish and journal).
     **None ships today** (nothing is signed).
   - exit codes: 0 ok, 3010 reboot, 2 partial, 4 refused, 1 failed; with `--msi` 2 /
     3010 map to 0 (`setup::msi_exit_code`) and are logged.
   - MSI rollback: if anything after this step fails on a fresh install,
     `uninstall --msi --remove-user-data=0` reverts the journal before the files go.
5. Run entry `PulsarSeatManager` = `"…\pulsar-seat-manager.exe" --tray`; service start.

## Uninstall sequence

Service stopped (seats stopped) → `pulsar-seat-service uninstall --msi
--remove-user-data=<0|1>` (`--unregister-service` for NSIS; `UninstallOptions::parse`):
service part removes the credential provider + filter registration (our four keys,
`seat_session::credprov::unregister`), the LSA secrets, the Event Log source and (NSIS) the
service record;
compat part (`seat_compat::setup::uninstall`) reverts **every** journaled change newest
first — tweaks, config-driven tweaks, the per-user palm check of every seat user
(`HKEY_USERS\<sid>`, hive loaded from `NTUSER.DAT` when the user is not logged on),
per-seat IP addresses, driver packages we published (`pnputil /delete-driver oemNN.inf
/uninstall`, no `/force`, only while the store still holds our package under that name).
A value changed by someone else after us is left alone and reported. Then files, Run
entry, `HKLM\SOFTWARE\Pulsar Seat`; `%ProgramData%\Pulsar Seat` only with
`REMOVEUSERDATA=1` / `/REMOVEUSERDATA` / "yes" in the NSIS question.

## Remote seat kind (merge step 10)

The one MSI carries the remote kind too (from Pulsar Split's `installer/split/`, removed).
Install logic: `crates/seat-setup` through `pulsar-seat-service remote-setup install |
reconcile | uninstall | check-coexistence`; remote spec 90 (`spec/remote/`).

- **Payload.** `<install>\remote\` = `fetch-deps.ts` output, generated into the WiX
  fragment `PulsarSeatRemote.wxs` (`ComponentGroup PulsarSeatRemotePayload`, one component
  per file) by `bundle-config.ts --remote`. Sunshine per arch (x64 / ARM64 portable zip),
  the VDD INF package, the usbip-win2 upstream installer. Kept apart from `drivers\`
  (the signed packages `install` stages, which must be WHCP-signed). The service reads the
  stream server from `remote\stream-server.json` (`seat_setup::layout::REMOTE_PAYLOAD_DIR`).
- **Shim** (`ComponentGroup PulsarSeatShim`). x64 package: `shim\x64\{PulsarSplitShim.dll,
  pulsar-split-launch.exe, pulsar-split-shimctl.exe}`, `shim\x86\{PulsarSplitShim.dll,
  pulsar-split-launch32.exe}`. ARM64 package (parity-arm64, 2026-11-17):
  `shim\arm64\{PulsarSplitShim.dll, pulsar-split-launch.exe, pulsar-split-shimctl.exe}`
  (native, Microsoft Detours backend; the service calls this launcher),
  `shim\x64\{PulsarSplitShim.dll, pulsar-split-launch.exe}` (x64 apps under emulation) and
  the same `shim\x86\`. Copied only; the service installs it when the operator turns
  isolation on. The ARM64 package's `pulsar-seat-credprov.dll` is ARM64X
  (`build-credprov.ps1`; `check-payload.ts` rule `not_arm64x`).
- **Custom actions.** After `install`: `remote-setup install --msi "--drivers=[PULSARDRIVERS]"
  "--consent=[PULSARCONSENT]"` (fresh install only) and `remote-setup reconcile …` (every
  install, upgrade, repair). They make the remote journal / log folders (owner SYSTEM,
  the Seat DACLs), **stop and delete a leftover `PulsarSplit` service** (pre-merge Split
  installs), install third-party drivers from `remote\drivers\` and run the reconcile hooks.
  Log: `%ProgramData%\Pulsar Seat\logs\remote-setup.log` (Seat's `setup.log` is separate).
- **Drivers default: none.** A fresh install without `PULSARDRIVERS` sets it to `none`, so a
  local-only PC gets no virtual display or USB/IP driver. Opt in with
  `msiexec /i … PULSARDRIVERS=vdd` (and `PULSARDRIVERS=vdd,usbip-win2 PULSARCONSENT=usbip-win2`
  for the upstream usbip-win2 installer). Repairs and upgrades keep the choice saved at the
  first install (`HKLM\SOFTWARE\PulsarSplit\Setup\Drivers`). Both properties refuse
  whitespace and `/ \ = [ ]` (launch conditions).
- **Uninstall replays both journal formats.** The one `uninstall` action first runs the
  remote uninstall in process (package restore hooks: wrapper module with the TermService
  restart last, RDS rows, shim, stream firewall group `PulsarSplit`, pads, VDD settings;
  then the generic replay of `journal\<package>.jsonl`; our devnodes, our driver packages,
  `HKLM\SOFTWARE\PulsarSplit\Secrets`, with `REMOVEUSERDATA=1` also the seat accounts it
  created and `HKLM\SOFTWARE\PulsarSplit`), then the local kind (device gates, session host,
  seat-compat `journal\changes.jsonl`). Each side skips the other's format; the data folder
  is deleted once, last, by the local part.
- Experimental multi-session (wrapper backend kind): off by default; there is no MSI
  feature for it any more (Split's `Termwrap\Offered` marker had no reader). The manager
  turns it on after its warning.
- Not taken over from Split: the NSIS hooks (Seat ships MSI only), the Tauri
  `resources` / `externalBin` overrides, the "reference seen at install" marker (no reader;
  the service evaluates coexistence at each start).
- **Untested:** no install ran. The remote custom actions, the driver opt-in, the
  leftover-service removal and the chained uninstall never ran elevated.

## Service CLI (implemented in `crates/seat-service/src/win/setup.rs`, untested elevated)

| Command | Behaviour |
| --- | --- |
| `preflight` | `seat_compat::preflight::run`; print `InstallReport::warnings()` lines on stdout; exit `setup::gate_exit_code` (0 / 4). No system change. |
| `install [--msi] [--register-service] [--restore-point=…] [--tweak=…] [--drivers=…]` | service part; `--register-service` = create the service record (NSIS only; the MSI's `ServiceInstall` already made it, creating it again would collide); then `InstallOptions::parse` + `setup::install(&Journal::in_data_dir(..)?, &WinOps, &opts)`, `setup::write_setup_log`; exit `report.exit_code()`, through `setup::msi_exit_code` with `--msi`. Parse errors: exit 1. |
| `remote-setup install \| reconcile [--msi] [--drivers=<ids\|none>] [--consent=<ids>]`, `remote-setup uninstall [--remove-user-data]`, `remote-setup check-coexistence` | Remote kind (`seat_setup::cli`, `crates/seat-service/src/remote/win/setup_cmd.rs`); see "Remote seat kind". |
| `uninstall [--msi] [--unregister-service] [--remove-user-data=<0\|1>]` | the remote uninstall first (both journal formats, see "Remote seat kind"); `UninstallOptions::parse`; `setup::uninstall`; service part; exit `setup::uninstall_exit_code` (0 with `--msi`). |

Also for the service: journal the seat user's SID with
`seat_compat::tweaks::record_user_palm_check` before sending the agent's palm-check task;
open the pipe client in `cli.rs` the way `seat_compat::seatapi::connect` does
(identification level, server PID checked).

## Never

Firewall **allow** rules (the manager talks to the service over a local pipe; spec 90 §11
item 8), Defender exclusions, test-signing, certificate imports, DeviceGuard / CI policy values,
Windows Update blocking or deferral values, `DenyDeviceClasses`, class-wide filter edits,
files in `System32`, legacy driver services.

The one firewall change is made by the **service**, not the installer: on Windows Server
with RD Session Host (`rds_loopback`), while a seat without a stored password exists, it
adds the inbound **block** rule `Pulsar Seat - loopback RDP (block remote)` for the port of
the passwordless-seat listener (`WinStations\PulsarSeat-Loopback`, default 33389), so that
listener answers loopback only (architecture §4.1 design (a)). Disable and `uninstall`
remove the listener key and the rule (both names are reserved, so this works without a
journal too). If an RDP session still holds TermService, the rule stays until TermService
restarts; `uninstall` then prints the `netsh advfirewall firewall delete rule` command and
exits with "partial" (2). **Untested on hardware.**

## Open items (need the maintainer or hardware)

- Install / upgrade / uninstall of the built MSI on a test machine (custom actions,
  rollback, restore point, tweaks + journal revert, service SDDL): never run.
- Driver signing: attestation first (docs/release/driver-submission.md; the unsigned
  submission CABs are built by `scripts/driver-cab.ts`, locally and in CI); the dashboard
  account, EV certificate and upload are the maintainer's. Until a signed package returns
  none is staged.
- Pin `DRIVER_BLOCKLIST_SHA256` in `ci.yml` when the first driver package ships.
- Code-signing certificate (route: docs/release/checklist.md §3); the real release key
  in `release-signers.txt` (§2); first signed release to exercise the update check end to
  end.
- NSIS stays unshipped: the native per-arch `setup.exe` (`crates/seat-bootstrap`,
  `scripts/build-setup.ps1`) is the exe installer since 2026-11-17. Its real install path
  (msiexec UI, UAC, exit codes) is HYPOTHESIS until a VM run.
- CI never ran (no remote). HYPOTHESIS: winget ids for SDK / WDK 10.0.28000 on the
  runner; `windows-11-arm` runs Tauri's x86 WiX tools under emulation (build-time only).
- HYPOTHESIS: the published `oemNN.inf` is a byte copy of our INF (we hash the published
  copy at stage time, so uninstall does not depend on it; adoption after a crash does).
- HYPOTHESIS: an MSI `Type="raw"` read of a `REG_EXPAND_SZ` value starts with `#`
  (TermService warning dialog condition).
