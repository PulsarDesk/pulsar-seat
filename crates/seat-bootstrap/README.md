# seat-bootstrap

`pulsar-seat-setup.exe`: the native setup program of Pulsar Seat, one build per
architecture (ARM64 `0xAA64`, x64 `0x8664`). It replaces Tauri's NSIS `-setup.exe`, whose
stub is 32-bit x86 (emulated on ARM64, not allowed by `docs/agents/rules.md`). Release
name: `PulsarSeat_<version>_<arch>[_<culture>]_setup.exe`.

## What it does

1. Native machine of the PC (`IsWow64Process2`; also correct when an x64 build runs under
   emulation on ARM64).
2. Picks the MSI: the one embedded at build time when it is for that machine, else
   `PulsarSeat_<version>_<arch>[_tr-TR].msi` next to the program (the Turkish MSI first
   when the user's UI language is Turkish). An x64 setup on an ARM64 PC with no ARM64 MSI
   beside it stops and names the ARM64 setup to download (exit 1633).
3. Checks the MSI: SHA-256 of the embedded bytes (compiled in) after unpacking into a new
   folder under `%TEMP%`; for a sibling MSI, `SHA256SUMS.txt` beside it must list the same
   hash when that file is present. Release builds (`PULSAR_SEAT_RELEASE=1` at compile time)
   also require a valid Authenticode signature (`WinVerifyTrust`, whole-chain revocation).
4. Runs `%SystemRoot%\System32\msiexec.exe /i "<msi>"` with the validated options and
   returns its exit code (3010 = reboot needed, 1602 = cancelled ...). The program is
   `asInvoker` (embedded manifest); msiexec asks for elevation itself.

```text
pulsar-seat-setup [/quiet | /passive] [/norestart] [/log <file>] [NAME=value ...]
pulsar-seat-setup /extract <folder>      write the embedded MSI there (deployment)
pulsar-seat-setup /payload-info          name, SHA-256, arch of the embedded MSI
pulsar-seat-setup /?
```

Switches take `/` or `-`. `NAME=value` passes a public MSI property (upper-case name,
value without quotes or control characters, at most 1024 characters); anything else is
refused (exit 1639), so nothing unchecked reaches the msiexec command line. Own errors use
Windows Installer codes: 1603 failure, 1619 no package, 1620 damaged / unsigned package,
1633 wrong architecture, 1639 bad command line. Messages go to the parent console when
started from a terminal, else to a message box (none with `/quiet`); English and Turkish
(`src/text.rs`, `seat_core::i18n`).

## Build

`installer/scripts/build-setup.ps1 -Arch arm64|x64 [-Msi <msi>] [-OutDir <dir>] [-Release]`:
static CRT, own target sub-folder (`<target>\setup`), MSI embedded through
`PULSAR_SEAT_SETUP_MSI` (`build.rs` refuses an MSI of another version / architecture or a
file that is not a compound file), PE machine and manifest checked, `/payload-info`
compared with the MSI's hash when the host can run the build, `<exe>.build.txt` record.
`build-installer.ps1` runs it after each MSI (test build; `-NoSetup` skips it). A release
setup is built from the **signed** MSI with `-Release`, then signed itself
(docs/release/checklist.md §4); `sign-release.ps1` refuses a setup whose record names
another MSI hash.

Without `PULSAR_SEAT_SETUP_MSI` (workspace builds, tests, clippy) nothing is embedded and
the program looks for the MSI beside it.

## Tests

`cargo test -p seat-bootstrap`: argument parsing and refusals, msiexec command-line
quoting, package choice (embedded / sibling / wrong arch / Turkish), MSI names, checksum
lookup, hashing, the en / tr text table.

Checked on the ARM64 dev laptop 2026-11-17 (no install was run): both builds 0xAA64 /
0x8664 with the MSIs of 8097f7f embedded, `/payload-info` hashes equal the MSIs', `/extract`
writes the identical MSI, the x64 setup on ARM64 exits 1633 with the message, an unknown
switch exits 1639.

HYPOTHESIS (never run, needs a test VM): the actual install through msiexec (UI, UAC
prompt, exit codes 0 / 3010 / 1602), the Turkish MSI choice on a Turkish Windows, the
release build's `WinVerifyTrust` check against a real signature, SmartScreen behaviour of
a signed setup.
