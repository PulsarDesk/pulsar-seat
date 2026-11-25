# Pulsar Seat credential provider + filter (`credprov/`)

In-process COM DLL `pulsar-seat-credprov.dll` that LogonUI loads. It gives
`fast_user_switch` seats (spec 35 §5.4 / §5.5, U1 "real sessions, one at a time") an
automatic logon with the seat's stored password, and a "switch seat" link on the logon and
lock screens. Clean room: written from Microsoft Learn (`ICredentialProvider`,
`ICredentialProviderCredential2`, `ICredentialProviderFilter`,
`KERB_INTERACTIVE_UNLOCK_LOGON`, `CredProtectW`, named pipes); no sample code copied.

**Status: built and self-tested only. Untested on hardware** (never registered, never
loaded by LogonUI on any machine). Unsigned.

## Files

| File | What |
| --- | --- |
| `credprov.vcxproj` | the DLL, ARM64 (built as ARM64X: `BuildAsX`, with an ARM64EC configuration as its other half) + x64, Release + Debug, v145 toolset, SDK 10.0.28000, static CRT, CFG, Spectre, `/W4 /WX` |
| `credprov-selftest.vcxproj` | console self-test of the same sources (never shipped); ARM64, ARM64EC, x64 |
| `src/dll.cpp` | `DllGetClassObject`, `DllCanUnloadNow`, class factory, `InLogonUI()` (no `DllRegisterServer`) |
| `src/provider.*` | provider, the two tile kinds, the filter, `ApplyFilter`, `SeatTileWillExist` / `FilterHides`, `Notifier` (poll + `CredentialsChanged`), link text in the UI language |
| `src/copy.h` | tile copy: English default, Turkish locale (`GetUserDefaultUILanguage` of LogonUI) |
| `src/pipe.*` | service pipe client (`\\.\pipe\PulsarSeat.Control`), one connection per call |
| `src/json.*` | small strict JSON reader / writer for the seat-proto frames |
| `src/pack.*` | Negotiate logon blob (`KERB_INTERACTIVE_UNLOCK_LOGON`, packed offsets) |
| `tests/selftest.cpp` | 66 checks: JSON shapes (incl. secret buffers, `pending`), packing layout, filter decision, the shared seat-tile / filter predicate, link text, inert outside LogonUI |

CLSIDs (also in `crates/seat-session/src/credprov.rs`): provider
`{C086BD57-C066-4FBA-ACED-7679C157E3DC}`, filter `{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}`.

## Behaviour

- Does nothing outside `%SystemRoot%\System32\LogonUI.exe`, and nothing for any scenario but
  `CPUS_LOGON` / `CPUS_UNLOCK_WORKSTATION` (`E_NOTIMPL`: CredUI and change-password never
  see it).
- Asks the service `credprov_query` (2 s limit). No answer, or "not a seat session" = no
  tiles, nothing hidden.
- **Seat tile** (logon screen of a seat session that has an account): label "Seat N" (Turkish locale "Koltuk N"),
  the account, a password box. If the service armed a one-time logon for this session it is
  the default tile with auto-logon: at `GetSerialization` it takes the credential once
  (`credprov_take_credentials`), packs it for Negotiate and submits. The outcome goes back
  with `credprov_report`; a failure marks the stored password failing in the service, and
  the tile stays as a normal password tile (the stored password is never retried).
  Tiles are built at `GetCredentialCount` (after `SetUserArray`). The tile names the seat
  account's SID (`GetUserSid`, from the service) only when LogonUI's user array lists that
  SID; otherwise it is an "other user" tile.
- **Late grant** (`pending`): under `fast_user_switch` the console is disconnected and
  LogonUI of the fresh session enumerates at once, usually before the service has armed the
  grant. While the service answers `pending` (a hand-over to this console session is under
  way), the provider takes part with no tiles, keeps the `ICredentialProviderEvents` from
  `Advise` and polls every 250 ms for at most 30 s on a worker thread; a changed answer
  goes into the cache and `CredentialsChanged` makes LogonUI call `GetCredentialCount`
  again, where the tiles are rebuilt (default + auto-logon once the grant is there).
  **Untested on hardware**: `CredentialsChanged` from a worker thread; whether LogonUI
  re-runs the filter then (if not, the other providers stay visible during a late
  auto-logon — harmless, the default tile auto-submits).
- **Switch links** (logon and lock screen of a seat session, when other seats exist): one
  command link per seat ("Switch to seat 3", Turkish "Koltuk 3'e geç") → `credprov_switch`. They sit in the seat tile
  when it exists; otherwise in a "Switch seat" ("Koltuk değiştir") tile tied to the seat user's SID when
  LogonUI lists it (else an "other user" tile). Windows 11 shows only the tiles of the
  locked / selected user, so a user-less switch tile was never reachable there (VM test
  2026-11-15, Win11 26300 ARM64). Seat 1 is offered while the backend reports its own
  session waiting off the console (`console_seat_away`).
  A seat without an account (seat 1) answers with its session user's account and SID, so
  that user's lock screen gets a seat tile with the links (the switch tile alone was built
  but never shown there, credprov.log). After a password-less switch the target seat's tile
  is the default one on the logon screen it opened (`preselect`, no auto-logon). Tiles carry
  an image drawn at run time (seat number). Diagnostics: `%ProgramData%\Pulsar Seat\logs\credprov.log`.
- **Filter**: in the logon scenario only, and only when the service says `hide_others`
  (an armed one-time logon in this session), the answer gives a seat tile (account known:
  `SeatTileWillExist`, the same predicate the provider uses) and our provider is in the
  list, every other provider is hidden. The service also never says `hide_others` without
  an account. Provider and filter share one cached answer per LogonUI process.
- **After a failed auto-logon** the provider asks the service once more (the grant is gone,
  so the answer no longer hides) and calls `CredentialsChanged`; the seat tile and its
  error text stay. Limitation: LogonUI decides the filter at enumeration; if it does not
  re-filter on `CredentialsChanged` (HYPOTHESIS, untested), PIN / Windows Hello / "other
  user" tiles stay hidden in that LogonUI instance: type the seat account's password, use
  the switch link, or press Ctrl+Alt+Del / lock again for a fresh logon screen
  ([docs/troubleshooting.md](../docs/troubleshooting.md) §7).
- The lock screen never auto-submits.

## Security

- Server check: the pipe's server PID must be the running `PulsarSeat` service (SCM).
  Client side of the check in the service: SYSTEM token, image `LogonUI.exe` in System32,
  process session = token session (`crates/seat-service/src/credprov.rs`).
- The grant is one-time, bound to one session id, expires after 120 s, and goes away on
  logon / logoff / seat stop.
- Passwords live in wiped buffers (`SecretW`, `Wipe`); the password is protected with
  `CredProtectW` for the logon scenario. The JSON reader decodes every string into one
  buffer reserved up front on the heap (never reallocated, never in the small-string
  buffer, so moves hand over the pointer); half-parsed values, the frame text and the parse
  tree are wiped on every error path (short read, timeout, bad JSON, wrong id); the `Ok`
  subtree is moved, not copied; the password is converted to UTF-16 in a wiped vector.
  Remaining: the kernel / pipe buffers and `CredProtectW`'s own copies are outside our
  control.
- **Unsigned today.** LogonUI loads the DLL as SYSTEM: a release must ship it
  Authenticode-signed. `check-payload.ts --release` and `sign-release.ps1` refuse an
  unsigned one; `build-installer.ps1 -SignedCredProv <dll>` bundles a signed build
  (docs/release/checklist.md §3).

## Registration

Never by the installer and never by the DLL. The service registers it
(`seat_session::credprov::sync`) only while the product is enabled **and** the active
session backend is `fast_user_switch` **and** the DLL next to the service is native for
this PC; it removes the four keys on disable, on another backend and on uninstall. Keys:

- `HKLM\SOFTWARE\Classes\CLSID\{…}` + `InprocServer32` (default = DLL path,
  `ThreadingModel=Apartment`), for both CLSIDs
- `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{C086BD57-…}`
- `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters\{FDC3F428-…}`

Recovery: `CredProvDisabled=1` (DWORD) under
`HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters`, safe mode, or deleting the
keys offline: [docs/troubleshooting.md](../docs/troubleshooting.md) §7.

## Architectures

One DLL per installer architecture: x64, and **ARM64X** in the ARM64 package (parity-arm64,
2026-11-17). The ARM64 half serves the native LogonUI; the ARM64EC half lets x64 processes
under emulation (CredUI hosts, any x64 app that enumerates credential providers: it sees the
same 64-bit registry keys) load the DLL instead of failing on an ARM64-only image. Inside
such a process the provider stays inert as everywhere outside LogonUI (`InLogonUI()` false,
`CPUS_CREDUI` refused with `E_NOTIMPL`), so CredUI shows the system tiles only.

- Built by `BuildAsX` in the ARM64 configuration (MSBuild builds the ARM64EC configuration
  as the other half and links `/MACHINE:ARM64X`). Needs the ARM64EC MSVC tools and Spectre
  libraries (VS 18 component ids in `ci.yml`).
- `build-credprov.ps1` checks machine 0xAA64 **and** CHPE (hybrid) metadata in the load
  config, and runs the self-test as ARM64 and as ARM64EC (both natively on an ARM64 host).
  `-NoArm64X` builds a plain ARM64 DLL (toolchain without ARM64EC; test builds only).
- The service still checks only the PE machine (`check_dll`: an ARM64X image carries
  0xAA64).
- Verified 2026-11-17 on the ARM64 dev box: Release + Debug ARM64X link, `dumpbin
  /headers` = "AA64 machine (ARM64) (ARM64X)", exports `DllCanUnloadNow` /
  `DllGetClassObject`, self-tests ARM64 / ARM64EC / x64 "0 failed". HYPOTHESIS: an emulated
  x64 CredUI host loads the EC half and calls nothing but `DllGetClassObject` /
  `DllCanUnloadNow` (never loaded by any host here).

## Build

```
powershell -ExecutionPolicy Bypass -File installer\scripts\build-credprov.ps1
powershell -ExecutionPolicy Bypass -File installer\scripts\build-credprov.ps1 -Platforms ARM64 -OutDir target\payload\arm64
```

Builds both projects, checks the PE machine (0xAA64 / 0x8664) and, for ARM64, the ARM64X
hybrid metadata, runs the self-test (ARM64, ARM64EC and x64 under emulation on an ARM64
host). CI: job `credprov` in `.github/workflows/ci.yml` (native
runner per architecture); locally `installer\scripts\ci-local.ps1 -CredProv` (or
`-Payload`). `build-installer.ps1` calls it and puts the DLL into the
payload. Outputs go to `credprov\build\` (gitignored). No `/INTEGRITYCHECK`: it would make
the loader demand a signature.

CI takes the SDK 10.0.28000 from Microsoft's NuGet packages (`installer/wdk/packages.config`,
used while `PULSAR_WDK_NUGET` names the restored folder; see `drivers/README.md`, Build),
with the runner image's VS 2026. Locally: `ci-local.ps1 -CredProv -WdkNuGet`.
