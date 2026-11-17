# Authenticode signing dry run (TEST certificate)

The release signing order of [checklist.md](checklist.md) §3 / §4 exercised end to end
with the **test certificate** only, on copies in a throwaway folder. It proves that the
signing hooks work on every file type we sign and that every release gate refuses a
test signature. Nothing here can produce a shippable file.

```powershell
powershell -ExecutionPolicy Bypass -File installer\scripts\sign-dryrun.ps1          # uses target\installer\dist
powershell -ExecutionPolicy Bypass -File installer\scripts\sign-dryrun.ps1 -Keep    # keep the folder for a look
```

## What it uses

- The test certificate `CN=Pulsar Seat TEST ONLY driver signing` in the current user's
  certificate store, created once by `tools\vm\testsign.ps1 -Action Cert` (VM test
  signing). The dry run uses it through the store by thumbprint; its exported key file is
  never opened. The script never creates a certificate, imports anything into a trusted
  store, or changes a system setting.
- `installer\scripts\sign-authenticode.ps1` (the real signing hook: `signtool sign /fd
  SHA256`, `/tr` + `/td SHA256` timestamp, certificate by thumbprint or a cloud signing
  `/dlib` plug-in). With a test certificate it signs only below `target\sign-dryrun\`.
- No timestamp (offline); a release signature always has one.

## Steps and expected results

| # | Step | Expected |
| --- | --- | --- |
| 1 | copy `PulsarSeat_<v>_<arch>.msi` + build records from dist into `target\sign-dryrun\<stamp>\dist` | copies only |
| 2 | `msiexec /a` each copy, sign the payload's own exe / dll (`remote\` third-party files excluded, files with a signature kept) | every first-party binary carries the test signature, the credential provider DLL included |
| 3 | `check-payload.ts --release` on the signed payload | **refused**: `test_signed` |
| 4 | sign each MSI copy | signer = test certificate, status not `Valid` (untrusted root) |
| 5 | `build-setup.ps1` from each signed MSI copy, sign the setup copy | setup builds (52 / 64 MiB), signed |
| 6 | `write-sums.ps1` on the dry-run dist | warns: not validly signed |
| 7 | `sign-release.ps1 -CheckOnly` on the dry-run dist | **refused** at the first MSI: Authenticode not Valid; no key opened |
| 8 | the real dist folder | byte-for-byte unchanged, no test signature in it |

`DRYRUN-REPORT.txt` in the folder lists the results; without `-Keep` the folder is deleted.

## Result 2026-11-17 (ARM64 dev laptop, MSIs of 8097f7f)

All 21 checks PASS: both MSIs extracted; 17 (ARM64) and 13 (x64) first-party binaries
signed; credential provider DLL test-signed in both; `check-payload --release` exit 1
(`test_signed`) for both payloads; MSI copies signed, status `UnknownError` (untrusted
root, as expected); setup programs built from the signed copies (ARM64 0xAA64, x64
0x8664) and signed; `write-sums.ps1` warned; `sign-release.ps1 -CheckOnly` refused
`PulsarSeat_0.1.0_arm64.msi: Authenticode UnknownError`; the five files of the real dist
were unchanged.

## What the dry run cannot show (HYPOTHESIS until a real certificate exists)

- A real certificate's chain and revocation check (`Valid`), the RFC 3161 timestamp
  servers, and SmartScreen reputation.
- The cloud `/dlib` route (Azure Artifact Signing or another service): parameters per
  the service's documentation; never run here.
- `sign-release.ps1` past the Authenticode gate: the build-record, tag and setup-record
  checks and the minisign signature run only on validly signed files from a tagged,
  clean `-Release` build.
