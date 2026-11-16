# Public release checklist

Everything that must be true before a Pulsar Seat build is distributed to anyone, in the
order it has to happen. Owner of every unchecked item: the **maintainer** (accounts,
money, keys, approvals). Tooling: `installer/README.md`. Status of the code:
`docs/agents/status.md`.

Legend: **[blocker]** = no public MSI without it; **[drivers]** = only for a release that
ships a driver package (none does today); **[decision]** = a choice only the maintainer
can make.

---

## 0. Gates already in the tree (no action, know they exist)

| Gate | Where | Fails when |
| --- | --- | --- |
| Version consistency | `build-installer.ps1`, CI `installers` job | tag ≠ Cargo workspace ≠ `tauri.conf.json` ≠ `apps/manager/package.json` |
| Source-offer tag | `build-installer.ps1 -Release` (CI tags pass it) | `git ls-remote https://github.com/PulsarDesk/pulsar-seat refs/tags/v<x.y.z>` finds nothing |
| Release key present | `PULSAR_SEAT_RELEASE` builds (compile-time assert in `crates/seat-service/src/update.rs`), `pulsar-seat-release check-signers --require` | `installer/release-signers.txt` lists no key |
| ARM64EC helper | `build-installer.ps1` | arm64ec rustup target missing; `-Release -NoArm64ec` |
| Payload / PE machine | `installer/scripts/check-payload.ts` | wrong machine type, VC runtime DLL, key / certificate file, unsigned driver package, blocklisted driver |
| Licence texts in the MSI | `check-payload.ts --notices` (`build-installer.ps1 -Release`, `sign-release.ps1`) | `licenses/` inside the MSI lacks `LICENSE.txt`, `NOTICES.md`, `SOURCE-OFFER.txt` or the generated `THIRD-PARTY-NOTICES-RUST.md` / `-JS.md` (< 1 KiB) |
| Release key file ACL | `pulsar-seat-release keygen`, `release-keygen.ps1` | the key folder is on a volume without ACLs (FAT / exFAT), or the protected DACL (you + SYSTEM) cannot be applied / read back |
| SDV clean | `run-sdv.ps1` (CI `sdv` job) | a defect, no rule passed, a timeout / spaceout, or a count missing from the log |
| Workflow lint | `installer/scripts/lint-ci.ts` (CI `bun` job, `ci-local.ps1`) | malformed `ci.yml`, unchecked exit codes in pwsh steps |

Run everything locally first:
`powershell -ExecutionPolicy Bypass -File installer\scripts\ci-local.ps1 -Payload -Release`
(and `-Drivers` / `-Installers` when touching those).

## 0b. First commit and release tag — [blocker]

The repository has **no commit yet** (every file untracked; agents never commit).
`build-installer.ps1` writes a `<msi>.build.txt` record (commit, dirty, release) and
`sign-release.ps1` refuses an MSI built from no commit, a dirty tree, or a commit that is
not tag `v<version>`. So nothing can be signed before:

- [ ] Review the tree (`git status`; no `docs/local/`, no `target/`, no key: see §1), then
      the first commit on `main`.
- [ ] Tag `v0.1.0` on that commit (or the release commit later).
- [ ] From the clean tag: `build-installer.ps1 -Release` for **both** arches (ARM64 with the
      ARM64EC helper, x64), then `sign-release.ps1`. The MSIs in `target/installer/dist`
      today are unsigned test builds from an untracked tree: never publish them.

## 1. Public repository and tag (GPL-3 §6 source offer) — [blocker]

`installer/licenses/SOURCE-OFFER.txt` promises the Corresponding Source at
`https://github.com/PulsarDesk/pulsar-seat`, tag `v<x.y.z>`.

- [ ] Create `PulsarDesk/pulsar-seat` (public), push `main` (maintainer's go; nothing is
      pushed by agents).
- [ ] Check what is published: no `docs/local/`, no `spec/` raw analyst notes outside
      `spec/` (CLEANROOM.md), no third-party binaries, no `target/`, no keys
      (`git ls-files | Select-String -Pattern '\.key$|\.pfx$|\.p12$|minisign|release-key'`
      **must print nothing**; `installer/release-signers.txt` does not match it. Also
      `git grep -n -E "RW(QAAEIy|RTY0Iy)[A-Za-z0-9+/]{100,}"` must print nothing: a
      minisign secret key line starts with `RWQAAEIy` (unencrypted, `Ed` 00 00 `B2`, what
      `pulsar-seat-release keygen` writes) or `RWRTY0Iy` (password-protected, `EdScB2`,
      `minisign -G`) followed by its long base64 body. The bare prefixes appear in this
      checklist, status / journal and a unit test on purpose; only a prefix followed by
      ≥ 100 base64 characters is a leak: rotate the key).
- [ ] CI runs green on `main` once (it has never run: written blind against the
      `windows-11-arm` and `windows-2025` images; see the HYPOTHESIS notes in `ci.yml`).
- [ ] Tag `v<x.y.z>` on the release commit and push the tag **before** the release build
      (`-Release` refuses otherwise).
- [ ] Keep the tag forever (the offer points at it). Never re-tag a published version.

## 2. Release signing key (update check) — [blocker]

The service accepts an update only when `SHA256SUMS.txt` carries
`SHA256SUMS.txt.minisig`, an Ed25519 (minisign-format) signature by a key compiled in from
`installer/release-signers.txt`. The key is offline: a hijacked GitHub account cannot ship
an installer the key did not sign, nor move a signed checksum file to another tag (the
signed trusted comment carries `version:X.Y.Z`), nor offer a PC a release older than one it
already accepted (signed `timestamp`, remembered in `updates\state.json`). **What it can
still do:** withhold updates (delete the new release or point `latest` at an old one), and
offer an older signed release that is newer than the installed version to a PC that never
saw a newer signature (e.g. a vulnerable 0.4.0 to a 0.3.0 PC after 0.5.0 exists). So
protect the GitHub account (2FA, no stale tokens) and say in the release notes when a
release fixes a security problem. **The real key exists since 2026-11-13** (maintainer
decision in docs/agents/plan.md; created on the dev laptop with `release-keygen.ps1`, key id
`0E7EC5FC77170613`, secret key in `%USERPROFILE%\.pulsar-seat\release-key\`, public line in
`installer/release-signers.txt`). Still open for the maintainer: the two offline backups.

- [x] Create it once, on a machine you trust, outside every repository:
      `powershell -ExecutionPolicy Bypass -File installer\scripts\release-keygen.ps1`
      (default `%USERPROFILE%\.pulsar-seat\release-key\`; `-OutDir <dir>` for e.g. an
      encrypted removable drive, **NTFS**; FAT / exFAT is refused because it has no ACLs).
      The secret key is never printed and is created with a protected DACL (your account
      + SYSTEM only) from the first byte; the script reads the ACL back and stops if any
      other account has access. Alternative: `minisign -G` (password-protected key; then
      sign with `minisign -S -s <key> -m SHA256SUMS.txt -t "timestamp:<unix
      s><TAB>file:SHA256SUMS.txt<TAB>version:<x.y.z><TAB>hashed"`: same format; the
      `version:` field is required, a plain `minisign -S` signature is refused as
      `sums_mismatch`).
- [x] Paste the printed `RWQ…` line into `installer/release-signers.txt` (done; commit it
      with the first commit, §0b).
- [ ] Two offline backups of the key folder. Losing the key = the next release must be
      signed with a new key that an *already installed* build does not know: users would
      have to update by hand once.
- [ ] Rotation: release N lists old + new key; release N+1 may be signed with the new key;
      release N+2 drops the old key.
- [ ] Never put the secret key into CI secrets, a cloud-synced folder or the repository
      (`pulsar-seat-release keygen` refuses folders inside a source tree).

## 3. Code signing (Authenticode) of exe / dll / MSI — [blocker] [decision]

The update check also requires a valid Authenticode signature on the MSI (WinVerifyTrust,
trusted root, revocation). Optionally pin the leaf certificate in
`AUTHENTICODE_PINS` (`crates/seat-service/src/update.rs`): SPKI SHA-256 + subject + issuer.

Routes (check prices / eligibility on the day; HYPOTHESIS where marked):

| Route | Cost | Notes |
| --- | --- | --- |
| Azure Artifact Signing (ex Trusted Signing) | ~US$10 / month | Cheapest; cloud HSM, CI-friendly. **Individual** identity validation reported open only in the USA / Canada (2026); organisations in more countries. Not usable for the hardware dashboard (driver attestation). |
| SignPath Foundation (free for OSS) | free | HYPOTHESIS: certificate issued to the foundation, project must meet their OSS policy, builds must come from their CI integration (GitHub Actions). Check terms. |
| Certum Open Source Code Signing | low (≈ tens of € / year) | HYPOTHESIS: individual OSS developers, smart card or cloud key; OV-level (SmartScreen reputation builds slowly). |
| Commercial OV / EV certificate (DigiCert, Sectigo, GlobalSign, SSL.com …) | ≈ US$300–600+ / year | **EV is required anyway for driver attestation** (hardware dashboard account needs ≥ 1 EV certificate), so one EV certificate can cover both. |

- [ ] Decide the route. If drivers will ever ship: EV (one certificate for both).
- [ ] Sign every `.exe` / `.dll` of the payload **before** bundling (re-run
      `build-installer.ps1` on the signed payload with `-SkipCargo`), then the MSI, with an
      RFC 3161 timestamp (`signtool sign /fd sha256 /tr <tsa> /td sha256 …`).
      **`pulsar-seat-credprov.dll` is mandatory** (LogonUI loads it as SYSTEM):
      `build-installer.ps1 -SignedCredProv <signed dll>` bundles it; `sign-release.ps1`
      extracts every MSI and refuses one whose DLL is not validly signed
      (`check-payload.ts --release`, rule `unsigned_system_dll`). CI MSIs carry the
      unsigned DLL and are for testing only.
- [ ] Optional: fill `AUTHENTICODE_PINS` with the new certificate's pin; rebuild.
- [ ] Never install the certificate or its root on a user machine; never import anything
      into Trusted Publishers / Root.

## 4. Build, sign, publish (per release) — [blocker]

1. [ ] `main` green in CI; versions bumped in Cargo.toml, `tauri.conf.json`,
       `apps/manager/package.json` (the same `x.y.z`).
2. [ ] Push tag `v<x.y.z>`. CI's `installers` job builds both MSIs with `-Release` and
       uploads `installers-unsigned-<arch>` (unsigned: testing only).
3. [ ] Sign payload + MSIs (§3) on the signing machine.
4. [ ] `installer\scripts\write-sums.ps1 <dir>` → `SHA256SUMS.txt` of the **signed** MSIs
       (removes a stale `.minisig`).
5. [ ] `installer\scripts\sign-release.ps1 -Dist <dir>` → checks every listed MSI's hash
       and Authenticode signature, writes `SHA256SUMS.txt.minisig`, verifies it against
       `installer/release-signers.txt`.
6. [ ] GitHub release `v<x.y.z>`, not draft, not pre-release, assets exactly:
       `PulsarSeat_<x.y.z>_arm64.msi`, `PulsarSeat_<x.y.z>_x64.msi`, `SHA256SUMS.txt`,
       `SHA256SUMS.txt.minisig`.
7. [ ] On a test machine with the previous version: manager → Güncellemeler → check →
       download → install. Expect `ready` with the publisher name, then a clean upgrade.
       (Never exercised yet: no release exists.)
8. [ ] Install / upgrade / uninstall of the MSI on a test VM (custom actions, rollback,
       restore point, tweaks + journal revert): never run so far (`installer/README.md`
       "Open items").

## 5. Driver packages — [drivers] [decision]

None ships today; the installer stages only Microsoft-signed packages. Per driver
(`seat-input-filter`, `seat-idd`, `seat-storage-filter`, `seat-wfp`):

1. [ ] Maintainer OK for the driver (the storage minifilter is an exception to
       `docs/agents/rules.md`, §6 below).
2. [ ] Static checks green: `installer/scripts/build-drivers.ps1 -Analyze` (WDK code
       analysis), infverif, ApiValidator; **SDV** (§7.1); CodeQL "Static Tools Logo" queries
       (HYPOTHESIS: the current Microsoft replacement for SDV; not set up yet).
3. [ ] Dynamic checks green on test VMs (§7.2).
4. [ ] Hardware dashboard (Partner Center): register the company / organisation, associate
       the **EV certificate**, accept the agreements.
5. [ ] Build the package for **x64 and ARM64** (`target\drivers\<Platform>\Release\<project>\<project>\`),
       make one CAB per driver containing the INF, binaries and PDBs for both
       architectures (`MakeCab` with a `.ddf`; layout in Microsoft Learn "Attestation
       signing a kernel driver for public release"), sign the CAB with the EV certificate.
6. [ ] Submit for attestation (Windows 10 / 11 client, both architectures). Microsoft
       returns the package signed by "Microsoft Windows Hardware Compatibility Publisher".
       Attestation-signed drivers load on client Windows only (not Server; not Windows
       Update retail).
7. [ ] Put the returned packages under `drivers-signed\<arch>\<package>\` (untracked) and
       build with `build-installer.ps1 -SignedDrivers drivers-signed\<arch>` (it checks the
       catalog signer and catalog membership of every binary).
8. [ ] Pin `DRIVER_BLOCKLIST_SHA256` in `ci.yml` (Microsoft recommended driver block rules)
       when the first package ships; the `installers` job refuses drivers without it.
9. [ ] Hardware test plan of each driver README on the ARM64 laptop and the x64 test PC
       (maintainer's approval for each machine).

## 6. Minifilter altitude — [drivers] [decision]

Only if the maintainer approves the storage minifilter exception. The request text is
drafted in [minifilter-altitude-request.md](minifilter-altitude-request.md) (send from a
long-term project address; 30 business days; only for a publicly shipping product, so
the public repository must exist first). Until then the INF keeps `UNALLOCATED`.

## 7. Verification of drivers

### 7.1 Static Driver Verifier (SDV)

SDV is no longer in the WDK (not in builds after 26017; WDK 10.0.28000 says so on
`msbuild /t:sdv`). Microsoft's remaining route is the **Windows 11 22H2 EWDK
(10.0.22621, VS Build Tools 17.1.5)**. Result of the attempt on the ARM64 dev box and the
CI job: see [sdv.md](sdv.md).

- [ ] SDV clean (no defects, at least one rule passed, **no timeouts / spaceouts**; "not
      applicable" rules documented) for `seat-input-filter` and `seat-wfp` (KMDF rules) and
      `seat-storage-filter` (WDM rules) on x64 — `ci.yml` job `sdv` (manual / tag runs).
      `run-sdv.ps1` exits 1 on any of these; `-AllowTimeouts` is for a local look only and
      never ticks this box. The `sdv` job is **advisory for the CI artifacts**: it is not in
      the `installers` job's `needs` (it takes hours and only runs on tags / on request),
      so a tag build produces MSIs even when SDV is red. Read the `sdv` job result and
      `sdv-summary.txt` before signing; a release that ships a driver needs it green.
- [ ] CodeQL driver queries (Static Tools Logo test) — not set up (HYPOTHESIS: needed for
      WHQL only, not for attestation).

### 7.2 Driver Verifier and Memory Integrity (HVCI) on a test VM

Never on the dev laptop or the test PC. The VMs are throw-away; Secure Boot and Memory
Integrity stay **on** (the product must work that way), so the driver under test must be
**attestation-signed** (a pre-release attestation submission is fine) — test-signing
would need Secure Boot off and is not allowed (`docs/agents/rules.md`; an exception for a
disposable VM is a maintainer decision).

1. [ ] VMs: Windows 11 x64 (Hyper-V Gen 2) and Windows 11 ARM64 (Hyper-V on the ARM64
       laptop, or an ARM64 cloud VM), Secure Boot on (template "Microsoft Windows"), vTPM
       on. Snapshot before each step.
2. [ ] Memory Integrity on: Windows Security → Device security → Core isolation → Memory
       integrity, reboot; `msinfo32` shows "Virtualization-based security Services
       Running: Hypervisor enforced Code Integrity".
3. [ ] Install the signed package: `pnputil /add-driver <inf> /install`; reboot. A driver
       that HVCI rejects is logged in Event Viewer → Applications and Services →
       Microsoft → Windows → CodeIntegrity → Operational (event 3087 / 3089) and does not
       load: that is a blocker.
4. [ ] Driver Verifier on the kernel drivers: `verifier /standard /driver seatinput.sys
       pulsarseatstoragegate.sys`, reboot, `verifier /query` shows them. Kernel Driver
       Verifier does not cover the UMDF IddCx driver (`seat-idd`): for it turn on the UMDF
       verifier (WDF Verifier, `WdfVerifier.exe` from the WDK, or the driver's
       `Parameters\Wdf` `VerifierOn` value) and Application Verifier on `WUDFHost.exe`.
5. [ ] Exercise: create / start / stop seats, hot-plug keyboards / mice / USB storage,
       move a device between seats, sleep / resume, service crash (kill the service:
       filters must drop other seats' input, never leak it to seat 1), Safe Mode boot
       (filters pass everything), uninstall.
6. [ ] No bug check, no verifier break, no CodeIntegrity errors. Collect
       `C:\Windows\MEMORY.DMP` on any crash (kernel dump configured) for analysis.
7. [ ] `verifier /reset`, reboot, revert the VM.
8. [ ] Record the results (date, build, Windows build, flags) in the driver's README and
       `docs/journal.md`.

## 8. Docs and housekeeping before the first release

- [ ] `docs/troubleshooting.md` (Turkish) mentions the exact service / INF names of any
      shipped driver (offline recovery).
- [ ] `installer/licenses/THIRD-PARTY-NOTICES-*.md` generated by CI (cargo-about,
      `js-notices.ts`); a build without them is not releasable. Enforced:
      `build-installer.ps1 -Release` refuses to start without them and checks them inside
      the MSI; `sign-release.ps1` refuses an MSI without them (`check-payload.ts
      --notices`). For a local release build generate them first with the two commands of
      the `ci.yml` step "Third-party notices".
- [ ] NSIS stays unshipped unless the maintainer approves the x86-stub exception.
- [ ] Release notes in Turkish on the GitHub release page.
