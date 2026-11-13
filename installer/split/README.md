# installer/split/

Installer and uninstaller for Pulsar Split (spec `90-install.md`, contract
`docs/split/agents/contracts.md` §8). Everything here is a reviewable script or manifest;
**nothing in this folder has been executed** against a real machine. The logic the
scripts call lives in `crates/seat-setup` (service sub-commands `install`, `reconcile`,
`uninstall`, `check-coexistence`; see its README).

## Files

| File | What it does |
| --- | --- |
| `drivers.json` | Third-party driver manifest (`DriverPackage[]`): URL, pinned SHA-256, licence, install kind, detection, minimum build. Defaults from `docs/split/agents/third-party.md`. |
| `stream-server.json` | Build-time stream-server manifest (upstream Sunshine, pinned tag of spec 80), one portable zip per arch with its SHA-256. |
| `wix/PulsarSplit.wxs` | WiX v3 fragment for the Tauri MSI: service exe + `ServiceInstall` (PulsarSplit, LocalSystem, own process, SID type unrestricted, restart 5 s / 30 s / 120 s, reset 1 day) + `ServiceControl`, RDP helper, shim (x64 MSI only), deferred no-impersonate custom actions (`install`, `reconcile`, `uninstall`), `REMOVEUSERDATA`, feature `ExperimentalMultiSession` (off; only writes `Termwrap\Offered=1`), arch guard, "reference seen" marker. |
| `nsis/hooks.nsh` | Tauri NSIS `installerHooks`: arch guard, Turkish reference-product notice, stop/register/reconcile/start the service, Turkish "remove user data?" question, `/REMOVEUSERDATA`, `/DRIVERS=`, `/CONSENT=`. |
| `apps/split-manager/src-tauri/tauri.conf.json` | Merged bundle config (2026-11-12, final fixer): default target **MSI** (the WiX fragment carries `ServiceInstall` and the deferred custom actions), fixed `upgradeCode`, `tr-TR`, fragment, component/feature refs, NSIS hooks, WebView2 bootstrapper mode. |
| `tauri.release.conf.json` | Release override (`--config`): product name and the third-party payload as `resources`. Not in `tauri.conf.json` because tauri-build copies resources on every cargo build and would fail before `fetch-deps.ts` ran. |
| `tauri.x64.conf.json` | x64 MSI override: adds the shim components to `componentRefs`. |
| `tauri.nsis.conf.json` | NSIS override: target `nsis`, service + RDP helper as `externalBin` (NSIS has no WiX fragment; `hooks.nsh` registers the service). |
| `scripts/fetch-deps.ts` | Build/CI step (bun): downloads the pinned artefacts, verifies SHA-256, unpacks with the system `tar.exe`, checks each INF (arch section + root hardware id), drops the stream server's web UI, writes the resolved `drivers.json` + runtime `stream-server.json` to `target/installer-deps/<arch>` (and `current`), then runs `check-payload`. |
| `scripts/check-payload.ts` | Fails the build on Microsoft DLLs (VC runtime, UCRT, `api-ms-win-*`, `WebView2Loader`, `rdpidd`, `rfxvmt`, `termsrv`, `AcRes`), certificates/keys (`*.pfx`, `*.cer`, `*.pem`, …), web-UI assets, known vulnerable drivers; warns on other `*.sys`. |
| `scripts/*.test.ts` | `bun test` for both scripts (pure helpers, temp folders only, no network). |
| `licenses/` | Upstream licence texts, `NOTICES.md` index, GPL source offer. |

## Build (x64 MSI; ARM64 the same with `aarch64-pc-windows-msvc` / `--arch arm64`)

```powershell
# 1. Our binaries with a static CRT (no vcruntime/ucrt DLLs to ship, spec 90 §2.2).
#    The root .cargo/config.toml sets +crt-static for every Windows target; no RUSTFLAGS needed.
cargo build --release --target x86_64-pc-windows-msvc -p split-service -p split-rdp-helper
# shim (x64 MSI only): see shim/README.md, outputs under shim/target/<triple>/release

# 2. Third-party payload (network, hash-verified) + payload check
bun installer/split/scripts/fetch-deps.ts --arch x64
bun installer/split/scripts/check-payload.ts target/installer-deps/x64

# 3. MSI through the Tauri bundler (config: tauri.conf.json + tauri.release.conf.json
#    + tauri.x64.conf.json)
$env:PULSAR_SPLIT_BIN  = "$PWD\target\x86_64-pc-windows-msvc\release"
$env:PULSAR_SPLIT_SHIM = "$PWD\shim\target"
cd apps/split-manager; bun run installer:msi:x64

# 3b. NSIS instead: copy the sidecars first (Tauri externalBin naming), then
#   copy ..\..\target\x86_64-pc-windows-msvc\release\pulsar-split-service.exe src-tauri\binaries\pulsar-split-service-x86_64-pc-windows-msvc.exe
#   copy ..\..\target\x86_64-pc-windows-msvc\release\pulsar-split-rdp-helper.exe src-tauri\binaries\pulsar-split-rdp-helper-x86_64-pc-windows-msvc.exe
#   bun run installer:nsis -- --target x86_64-pc-windows-msvc
```

Sign every exe/dll and the MSI/NSIS output with a publicly trusted code-signing
certificate and an RFC 3161 timestamp (spec 90 §7). No certificate is ever installed on a
user machine.

## Install behaviour

- Per-machine, elevated, Turkish UI. Install root `%ProgramFiles%\Pulsar Split` (may be
  changed), data root `%ProgramData%\PulsarSplit` (owner SYSTEM, protected DACL SYSTEM +
  Administrators; OQ 25). A data folder that already exists with another owner, or as a
  junction / symlink, is moved aside to `PulsarSplit.untrusted-<ms>` and created fresh;
  links, hard-linked files and foreign-owned entries inside it are deleted.
- MSI sequence: `InstallServices` → `install` (fresh install only) → `reconcile` (every
  install / upgrade / repair) → `StartServices`. Upgrade = `MajorUpgrade` of the Tauri
  template with our fixed `upgradeCode`; a downgrade is refused by the sub-commands unless
  `--allow-downgrade` (spec 90 §5).
- Drivers (`install` / `reconcile`): `PULSARDRIVERS` (MSI) / `/DRIVERS=` (NSIS) when
  given (`none` = no driver); otherwise the choice saved at the first install
  (`HKLM\SOFTWARE\PulsarSplit\Setup\Drivers`, so a repair / upgrade keeps it); the
  manifest defaults for the machine arch only on a first install. An INF package whose
  original name is already published but not by us (no `inf:<name>` record in our
  journal) is foreign: never upgraded over, never deleted. The journal records the
  published `oemNN.inf` we added; uninstall deletes only that one. Official
  installers marked `requires_consent` (usbip-win2) run only when listed in
  `PULSARCONSENT` / `/CONSENT=`. ViGEmBus is detect-only. INF packages go through
  `pnputil /add-driver <inf> /install`, plus a root devnode for the virtual display; every
  step is journaled so uninstall removes only what we added. Passive mode (reference
  product present) skips drivers until a later `reconcile`.
- Coexistence: install is always allowed (spec 90 §8.2). The MSI only records
  `HKLM\SOFTWARE\PulsarSplit\Setup\ReferenceSeenAtInstall`; NSIS shows a Turkish notice.
  The service evaluates all 18 signals at each start and runs passive while the reference
  product is installed; the manager shows the findings and opt-in repair items.
- Firewall: the installer creates no rules (the stream package adds per-seat rules in the
  `PulsarSplit` group). Pipe ACL: created by the service, not the installer.
- Experimental multi-session (client SKUs): `EXPERIMENTALMULTISESSION=1` /
  `ADDLOCAL=ExperimentalMultiSession` only sets `Termwrap\Offered=1`; the manager shows
  the warning and enables it.

## Uninstall behaviour

`uninstall` runs before the service files are removed (MSI: after `StopServices`, not
during a major upgrade). Order: package `RestoreHook`s (TermService-restarting ones last:
RDS / termsrv originals come back), generic journal replay in reverse, firewall group
`PulsarSplit`, our devnodes (`&PULSARPAD&`, `ROOT\PULSARPAD\`, `PulsarSplitPad\` only),
our driver packages (only those in the journal), event source, seat passwords (always).
`REMOVEUSERDATA=1` / `/REMOVEUSERDATA` / "yes" in the NSIS question also removes seat
accounts we created with their profiles, `HKLM\SOFTWARE\PulsarSplit` and the data
folder. When user data is kept, the seat-account records stay in the live journal, so a
later uninstall with remove-user-data still deletes them. Journals are replayed only
when the data folder is trusted (owner SYSTEM / Administrators, no links); otherwise
replay, logging and the folder delete are skipped and reported. Firewall rules are
removed only when their grouping is `PulsarSplit` (same-named rules of other products
stay). Locked files are
scheduled for deletion at reboot. Upstream-installer drivers (usbip-win2) stay; remove
them in Settings → Apps.

## What is untested

- Nothing here was built into an MSI/NSIS or run. WiX was only checked for XML
  well-formedness; the Tauri template ids used (`INSTALLDIR`, `WIX_UPGRADE_DETECTED`),
  Tauri's `MajorUpgrade` settings, the `tr-TR` WiX locale and the NSIS `${ARCH}` define
  are assumptions about the Tauri 2 templates (HYPOTHESIS) and must be checked in the
  first real build.
- `fetch-deps.ts` network path and unzip were not run; artefact hashes are GitHub release
  digests read from the API, not re-hashed locally. Upstream VDD names its amd64
  driver-only zip `x86` (HYPOTHESIS: it is amd64; the INF check fails the build if not).
- usbip-win2 silent switches and detect hardware id are HYPOTHESIS.
- Stream server provenance: GitHub reports `LizardByte/Sunshine` as moved to
  `SunshineStream/Sunshine` (same repository, not a fork; API check 2026-11-12, details
  in `stream-server.json`). `fetch-deps.ts` refuses a build URL whose repo or tag differs
  from `source_url`. third-party.md still names LizardByte (open question setup-10).
- NSIS upgrade waits up to 60 s for `sc query PulsarSplit` to report `STOPPED` (state
  names assumed unlocalised: HYPOTHESIS).
- Driver signing / loading with Memory Integrity on: see third-party.md (OQ 24).
- Whether upstream Sunshine starts without its `assets/web` folder: HYPOTHESIS, check on
  the test PC.
