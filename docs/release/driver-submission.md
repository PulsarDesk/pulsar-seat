# Driver submission (attestation / WHQL)

How our own drivers get a Microsoft signature, from the build to the signed package the
MSI stages. Owner of every account step: the **maintainer** (identity, EV certificate,
agreements). Everything up to the unsigned CAB is scripted and runs in CI. Status of the
drivers themselves: `drivers/README.md`; release order: [checklist.md](checklist.md) §5.

Sources (read 2026-11-17, Microsoft Learn): "Attestation signing a kernel driver for public
release", "Create a new hardware submission", "Driver signing offerings", "Get started
with the hardware program", "Windows Hardware Lab Kit". Steps that only the dashboard can
confirm are marked HYPOTHESIS.

## Which drivers

| Driver | Package | Binary | Kind | Notes |
| --- | --- | --- | --- | --- |
| `seat-input-filter` | `seat-input-filter.inf` | `seatinput.sys` | KMDF filter | keyboard / mouse class upper filter |
| `seat-idd` | `seat-idd.inf` | `seat-idd.dll` | UMDF IddCx | local and remote roles in one package |
| `seat-storage-filter` | `seat-storage-filter.inf` | `pulsarseatstoragegate.sys` | FS minifilter | needs a Microsoft altitude first ([minifilter-altitude-request.md](minifilter-altitude-request.md)); maintainer exception |
| `seat-wfp` | `seat-wfp.inf` | `seatwfp.sys` | KMDF WFP callout | per-seat source IP |
| `split-pad` | `split_pad.inf` | `split_pad.dll` | UMDF (Rust) | remote seats' pads |

Every package is built for **x64 and ARM64** by `installer/scripts/build-drivers.ps1`
(unsigned; never loaded on the dev PC).

## Route: attestation first, WHQL later

| | Attestation | WHQL (HLK) |
| --- | --- | --- |
| Tests | none at Microsoft (our own static + dynamic checks, checklist §7) | the Hardware Lab Kit playlist for the driver's features, run by us on lab machines |
| Signs for | Windows 10 / 11 **client** (x64, ARM64) | client and Server; Windows Update distribution possible |
| Input | one CAB per submission, EV-signed | `.hlkx` package from HLK Studio, EV-signed |
| Cost | dashboard account + EV certificate | the same plus lab machines (an HLK controller on Windows Server and one test client per architecture) |

Pulsar Seat targets client Windows, so **attestation** is the route for the first
release. WHQL is needed only for Windows Server seats (Remote Desktop Session Host) or
distribution through Windows Update; the same driver folders are used (see "WHQL" below).

## 1. Build and package (scripted)

```powershell
# both platforms, Release (+ Debug), zero warnings, infverif, ApiValidator, PE machine
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1
# static analysis (checklist §5.2 / §7.1)
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1 -Configurations Release -Analyze -SkipInfVerif -OutRoot target\drivers-analyze
# one CAB per driver, both platforms inside
bun installer/scripts/driver-cab.ts
```

`driver-cab.ts` writes `target\driver-submission\<driver>.cab` + `.ddf` and
`manifest.json` (SHA-256 of every file and CAB). Layout inside each CAB (no file at the
root; one sub-folder per package, which is what the dashboard expects):

```text
seat-wfp\ARM64\seat-wfp.inf
seat-wfp\ARM64\seatwfp.sys
seat-wfp\ARM64\seatwfp.pdb
seat-wfp\x64\seat-wfp.inf
seat-wfp\x64\seatwfp.sys
seat-wfp\x64\seatwfp.pdb
```

Checks before a CAB is written: one INF per package with `DriverVer` and `CatalogFile`,
every `[SourceDisksFiles]` entry present, PE machine per platform, a PDB for every binary
(Microsoft asks for symbols; they are not shipped to users), unsigned `.cat` left out
(Microsoft generates and signs the catalog), and the storage filter's altitude must not be
the `UNALLOCATED` placeholder (`--allow-unallocated-altitude` builds it anyway, for
inspection only; CI passes it and records it in `manifest.json`). After `makecab` the
CAB's own file table is read back and must match the plan exactly.

`--platforms ARM64` (or `x64`) makes `<driver>-<Platform>.cab`: CI's drivers job builds
each platform in its own job and uploads `driver-submission-<Platform>`. HYPOTHESIS: the
dashboard accepts one multi-architecture CAB per driver as well as one CAB per
architecture; if it rejects the combined CAB, submit the per-platform ones.

Before packaging a release: bump `DriverVer` in the INF (stampinf does it for
`split-pad`), rebuild, and keep the CAB's `manifest.json` with the release notes.

## 2. Sign the CAB (maintainer, signing machine)

The dashboard accepts only CABs signed with the **EV code-signing certificate** registered
to the account:

```powershell
powershell -ExecutionPolicy Bypass -File installer\scripts\sign-authenticode.ps1 `
  -Path target\driver-submission\seat-wfp.cab -Thumbprint <EV certificate SHA-1> `
  -TimestampUrl <the CA's RFC 3161 URL>
```

(`sign-authenticode.ps1` refuses a test certificate outside `target\sign-dryrun\`.)

## 3. Dashboard account (maintainer, once)

1. Partner Center → Hardware program: register (organisation or individual developer as
   the program allows; HYPOTHESIS: the program needs an organisation identity verified
   through the EV certificate's subject).
2. Upload a file signed with the EV certificate to associate it with the account.
3. Accept the Windows Hardware program agreements (attestation and, for WHQL, the
   compatibility program).

## 4. Submit for attestation

Per driver, in Partner Center → Hardware → "Submit new hardware":

- **Product name:** `Pulsar Seat <driver> <version>` (for example `Pulsar Seat seat-wfp 0.1.0`).
- **Package:** the signed CAB.
- **Requested signatures:** Windows 11 client x64, Windows 11 client ARM64 (and the
  Windows 10 client x64 / ARM64 entries the dashboard offers for 19045 support).
- **Test signing:** off. **Driver testing (HLK):** none (attestation).
- **Distribution:** none at first (download the signed package only).

When the submission is finished, download the signed package: the same folders with a
Microsoft-signed catalog ("Microsoft Windows Hardware Compatibility Publisher") and
Microsoft-signed binaries.

## 5. Use the signed package

1. Unpack per architecture into `drivers-signed\<arch>\<package>\` (not tracked).
2. Check: `signtool verify /pa /c <cat> <sys>` for each binary; the MSI build does the same.
3. `build-installer.ps1 -Arch <arch> -SignedDrivers drivers-signed\<arch>`: it stages the
   package only when the catalog verifies, is signed by the Microsoft hardware publisher
   and every `.sys` / `.dll` is a catalog member (or embedded-signed by that publisher).
4. Pin `DRIVER_BLOCKLIST_SHA256` in `ci.yml` (checklist §5 item 8).
5. Dynamic checks on the test VMs with Memory Integrity on (checklist §7.2), then the
   hardware plans in each driver's README.

## WHQL (later, only if needed)

1. Lab: HLK controller (Windows Server, HLK Controller + Studio of the matching Windows
   release) and one test client per architecture (x64 PC, ARM64 PC or VM where the kit
   allows; HYPOTHESIS for ARM64 VMs).
2. Install the driver on each client from the folder of the CAB (`driver-cab.ts` output,
   unpacked) with test signing or a pre-production attestation signature, as the kit's
   instructions say for the target.
3. HLK Studio: create a project, add the clients, select the device / driver, run the
   playlist the kit selects for its features (filter drivers, IddCx display, WFP callout,
   minifilter), fix every failure, then "Package" with the driver folders added (the same
   `<driver>\<Platform>\` folders) and sign the `.hlkx` with the EV certificate.
4. Submit the `.hlkx` in Partner Center instead of the CAB.

None of the HLK steps can run on the dev laptop; they need lab hardware the project does
not have yet.

## What is not automated and why

- The dashboard account, the EV certificate and the upload need the maintainer's identity
  and money (out of scope for agents).
- Nothing is uploaded, signed with a real certificate, installed or loaded by the scripts.
