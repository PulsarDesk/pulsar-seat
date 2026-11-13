# split-termwrap — EXPERIMENTAL, off by default

Client-SKU (Windows Pro / Enterprise) multi-session for Pulsar Split, through a
TermService wrapper that **the operator installed themselves**. Contract:
[`docs/split/agents/contracts.md`](../../docs/split/agents/contracts.md) §2. Spec: `spec/remote/30-session-rds.md`
§2.2, §2.4, §2.5 path B, §13, §14. Default host path stays Windows Server + RDSH (path A,
`split-session`); this crate only matters when the operator opts in.

## What it does (path B1)

Pulsar Split ships **no wrapper, no `termsrv.dll` patch, no offsets, no patch tables**, and
never modifies Windows files. This crate:

1. **Detects** wrapper DLLs at documented install locations (today: the Apache-2.0
   [RDP Wrapper Library](https://github.com/stascorp/rdpwrap): `%ProgramFiles%\RDP Wrapper\rdpwrap.dll`
   and the `-s` variant `%SystemRoot%\System32\rdpwrap.dll`, config `rdpwrap.ini` next to
   the DLL), plus any path the operator types.
2. **Detects other wrappers already active**: if `TermService\Parameters\ServiceDll` is not
   the stock `%SystemRoot%\System32\termsrv.dll` and we did not write it, the state is
   `ForeignActive` and nothing is touched (enable, disable and boot check all keep their
   hands off; `restore_all` never overwrites a value someone changed after us).
   This includes an RDP Wrapper the operator activated with its own installer.
3. **Strict version gate** (`gate`, fail closed): the binary file version
   (`VS_FIXEDFILEINFO`) of `termsrv.dll` must be listed *exactly* in the wrapper's own
   support file and not be newer than the newest build listed. For RDP Wrapper a build
   counts only if both `[a.b.c.d]` and, for NT 10, `[a.b.c.d-SLInit]` sections exist and
   the file has a `[Main]` section. Only section *names* are read; no patch data is
   interpreted. Unknown wrapper kinds have no table and are always refused.
4. **Further fail-closed checks** before any change: client SKU (Server → `not_client_sku`,
   Home → `edition_unsupported`), reference product not installed (spec 90 §8.1 signals
   1–2), wrapper present, PE machine of the DLL equals the OS native machine (an x64 DLL
   cannot load into an ARM64 svchost; RDP Wrapper publishes no ARM64 build), Authenticode
   (`Trusted`; `Unsigned` only with `TermwrapPolicy::allow_unsigned`; `Untrusted` never;
   whole-chain revocation from cached CRLs only, revoked = `Untrusted`, "unknown offline"
   = accepted with a warning), plain absolute local path (no UNC / device / `..` / env
   vars / ADS; on Windows the resolved final path must be on a `DRIVE_FIXED` drive and not
   `\\?\UNC\`), and an **allow-list ACL check** (`acl.rs`, pure + tested) on the DLL and the
   config, for both the given and the resolved path: the owner of the file, its directory
   and every ancestor up to the volume root must be SYSTEM, Administrators or
   TrustedInstaller; no other SID may hold an allow ACE with write/replace rights (file:
   write/append data, DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_*; wrapper directory: also
   add file / add subdirectory / delete child; ancestors: DELETE and delete child, so no
   rename-and-recreate of the path); allow ACE types other than `ACCESS_ALLOWED_ACE_TYPE`
   count as tamper; deny ACEs are ignored (fail closed). Note: a folder an elevated admin
   creates may be owned by that admin's own user SID, which this check refuses (UAC is not
   a boundary we rely on); fix with `icacls <dir> /setowner *S-1-5-32-544 /T`
   (open question termwrap-5).
5. **Clean enable**: needs `ack_warning_version == WARNING_VERSION`. Stores
   `HKLM\SOFTWARE\PulsarSplit\Termwrap` (`Enabled`, `WrapperPath`, `AckWarningVersion`)
   first, then for `ServiceDll` (REG_EXPAND_SZ) and the two "path B only" rows
   (`Terminal Server\Licensing Core\EnableConcurrentSessions` = 1,
   `Winlogon\AllowMultipleTSSessions` = 1) writes a journal `intent`, the value, then
   `done` to `%ProgramData%\PulsarSplit\journal\termwrap.jsonl` (write-through + fsync;
   the `journal` directory is created with the protected DACL
   `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`, and before every read and append the owner and
   DACL of the directory and of the open file handle must be SYSTEM / Administrators only,
   else the journal is refused),
   then restarts TermService via the SCM (active dependents stopped first and started
   again, timeouts, **never kills svchost by PID**). A failed restart rolls everything back.
6. **Clean disable / uninstall** (`disable`, `restore_all`): stores `Enabled=0` +
   `RestorePending=1` first (so a crash can never lead a boot check to re-enable), then a
   guarded reverse replay of the journal (a value is put back only if it still has the
   value we wrote → idempotent, crash-safe even with `intent` lines lacking `done`). The
   journal is data, not authority: only our three values are replayed, a ServiceDll
   `previous` is restored only if it is stock or a wrapper that passes every check now
   (else stock), and ownership of a ServiceDll comes only from our HKLM state
   (`Enabled`/`RestorePending` + `WrapperPath`), never from the journal. If the journal is
   lost or refused, an owned ServiceDll falls back to stock. Then the journal is cleared,
   TermService restarted if ServiceDll changed (or on a resumed switch-off), and
   `RestorePending` cleared. `restore_all` does nothing at all when ServiceDll is foreign
   (including an RDP Wrapper the operator activated later at the same path).
7. **Boot check**: finishes an interrupted switch-off (`RestorePending`). If we enabled it
   and any gate now fails (Windows update changed
   `termsrv.dll`, wrapper removed, ACL changed, …) it restores stock and reports
   `Blocked { reason }` (persisted in `Termwrap\Blocked` until the operator re-enables). If
   the redirection was lost but the gate passes, it re-applies.
8. **Warning text**: `WARNING_TR` (Turkish, contract §2.4 verbatim) and `WARNING_VERSION`.

### Why not an own wrapper (path B2)

An own in-memory patcher needs a per-build patch table derived from Microsoft binaries
(not something this clean-room project can produce from public, licence-compatible
material), a code-signing certificate chaining to a public root (we never install a
private root), and maintenance on every servicing update. It is the licence-grey core of
the problem and AV-flagged. The cargo feature `own-wrapper` exists only as a reserved slot
(`own_wrapper::OwnWrapper`), and every call returns `unsupported`.

## Additions beyond the contract (for the integrator)

- `BlockReason` extra variants: `EditionUnsupported`, `TermsrvVersionUnreadable`,
  `WrapperConfigInvalid`, `WrapperWrongArchitecture`, `WrapperInsecureAcl`.
- Extra reason codes: `edition_unsupported`, `wrapper_arch`, `wrapper_insecure_acl`,
  `wrapper_path`, `wrapper_config`, `unsupported`, `registry`, `journal`.
  `TermsrvVersionUnreadable` maps to `unknown_termsrv_build`.
- `FileVersion` is defined here (same shape as `split_session::FileVersion`; packages do
  not depend on each other, so the integrator converts).
- `WindowsTermwrap::new(TermwrapDeps, TermwrapPolicy)`; `WindowsTermwrap::system(data_root,
  policy)` on Windows; inherent `restore_all()` for `seat-setup`'s `RestoreHook`.
- Extra traits: `Registry` (HKLM), `FileInspector`, `HostFacts`, `Clock`; fakes under
  `fake` (feature `fake`), including `FakeWorld`.
- `TermwrapPolicy { allow_unsigned, restart_timeout }`. Default refuses unsigned DLLs; the
  upstream RDP Wrapper release is unsigned as far as public pages say (open question termwrap-1).

## Build / test

```
cargo test   -p split-termwrap
cargo clippy -p split-termwrap --all-targets
cargo check  -p split-termwrap --target x86_64-pc-windows-msvc
cargo test   -p split-termwrap --features own-wrapper
```

Tests use fakes only. The `win::*` tests do read-only process/file probes (native
machine, `kernel32.dll` version/PE/ACL walk, a user-owned temp file and journal that must
be refused); none touch the registry, services or drivers.

The repo-root workspace did not load on 2026-11-12 (`crates/seat-pads` had no targets
yet), so results were produced in a scratch copy of the workspace (split-proto +
split-termwrap, same `Cargo.lock` and `workspace.package`). Re-run from the root once it
loads.

## Untested (needs a real client test box, maintainer go)

- Every `win::*` path that changes state: `WinRegistry::set`, `WinTermService::
  {set_service_dll, restart}`, i.e. a real enable/disable/boot check against TermService.
- SCM restart timing and dependents on 24H2/26H2 (`UmRdpService`), and what happens to
  active sessions (expected: all RDP sessions drop; the integrator stops seats first).
- `WinVerifyTrust` result mapping on a real wrapper DLL (incl. revocation codes); ACL
  check against a real `%ProgramFiles%\RDP Wrapper` folder created by the upstream
  installer (owner may be the installing admin's user SID, see termwrap-5).
- The protected journal directory and the handle-based journal check when running as
  SYSTEM (tests run as a standard user and only prove the refusal side).
- SCM pending-state handling and the stop-phase unwind in `WinTermService::restart`.
- Whether RDP Wrapper with a current community `rdpwrap.ini` actually yields concurrent
  sessions on the tested build (not our code; HYPOTHESIS until tried).
- Whether TermService must run in its own svchost for the DLL swap to take effect (the
  upstream installer checks this; spec 30 says it does on the live box). Not checked here.

## Licence notices

GPL-3.0-only. No third-party code is included. The RDP Wrapper Library
(github.com/stascorp/rdpwrap, Apache-2.0) was consulted only for its public file names,
install locations and config section naming, for interoperability; it is not bundled.
