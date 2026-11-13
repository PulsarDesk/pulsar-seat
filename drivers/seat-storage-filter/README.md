# seat-storage-filter (Filter Manager minifilter) — EXCEPTION, built, never installed

Strict per-seat storage isolation for the `storage_minifilter` gate
(`crates/seat-media/src/gate/minifilter.rs`). Spec 70 §5.2(4), §5.3; plan.md open question
"Minifilter for USB storage gating".

**Status: built (WDK 10.0.28000, ARM64 + x64, zero warnings, infverif /w and WDK code analysis clean), unsigned, never installed.** A minifilter is a Filter Manager kernel
driver, outside the rules.md driver list (KMDF / UMDF / IddCx). Before it is ever installed
it needs all of:

1. the maintainer's explicit OK for the exception;
2. an altitude allocated by Microsoft in the *FSFilter Security Enhancer* range
   (400000–409999). The INF carries the placeholder `UNALLOCATED`, which Filter Manager
   cannot use, so an accidental install attaches to nothing. Never copy another product's
   altitude;
3. an attestation-signed (or WHQL) package for **ARM64 and x64**; no test-signing.

The default storage isolation is user mode (`device_security` descriptors on disk + volume
devnodes, `session_drive_letters`). This driver is only for the case where spec 70 §6 q1
shows that the volume descriptor does not gate file opens.

## Files

| File | What |
| --- | --- |
| `inc/seat_storage_gate.h` | Port protocol, shared with `seat_media::gate::minifilter::proto` (keep in step) |
| `src/gate.c` | DriverEntry, registration, instance setup (volume GUID context), pre-create gate, port |
| `seat-storage-filter.inf` | Service + instance registration (one package, `NTamd64` + `NTarm64`) |
| `seat-storage-filter.vcxproj` | WDK project, `Debug`/`Release` × `x64`/`ARM64`, `/INTEGRITYCHECK`, CFG |

Service name `PulsarSeatStorageGate`, binary `pulsarseatstoragegate.sys`, port
`\PulsarSeatStorageGate` (`seat_core::consts::MINIFILTER_PORT_NAME`).

## Behaviour

- **Fail open** until the service commits a plan after boot, after `PASS_ALL`
  (`revert_all`), in Safe Mode (`InitSafeBootMode`), or with `Parameters\FailOpen = 1`
  (recovery value; set offline if needed).
- Armed: `IRP_MJ_CREATE` on a volume whose GUID has a non-shared owner mask is completed
  with `STATUS_ACCESS_DENIED` unless the requestor is kernel mode without
  `SL_FORCE_ACCESS_CHECK` (a kernel create that asks for an access check, e.g. the SMB
  server for a share client, is judged like a user create on the impersonated token and
  the requesting session), its effective token
  (client token when impersonating) has a service account as user (LocalSystem,
  LocalService, NetworkService, `NT SERVICE\*` = `S-1-5-80-*`), its session is one the plan
  maps to an owning seat, or it holds an administrator token when the plan has
  `ADMINS_BYPASS`. "Session 0" alone is **not** an exemption: a user's own code runs there
  in S4U / stored-password scheduled tasks, BITS jobs and impersonating RPC services, and
  is decided like an unmapped session (denied). Gap: a seat user's own session-0 task is
  denied on that seat's own volume too (plan carries session ids, not logon LUIDs).
  Paging-file opens pass. Network and RAW volumes are not attached. The token check needs
  PASSIVE_LEVEL; a gated create at APC_LEVEL is denied (never exempted). The subject
  context is locked while its token is read. `SET_VOLUME` accepts only
  `{8-4-4-4-12}` hex (lower-cased) followed by NUL padding, else
  `STATUS_INVALID_PARAMETER`, so a padded entry can never silently fail to match.
- Shared = mask `0xFFFFFFFF` exactly (`PSSG_SHARED_MASK`, same rule as
  `seat_core::SeatMask::is_shared`; the service does not even send shared volumes).
- Volume GUID: looked up in instance setup; if that fails (first mount before the mount
  manager published the GUID link), the first create queues a generic work item that
  retries at PASSIVE_LEVEL (one at a time per volume, again on later creates while it
  keeps failing). While unresolved and armed, a volume on a removable bus (USB / SD / MMC
  bus type from `IOCTL_STORAGE_QUERY_PROPERTY`, removable media, optical) is **denied** to
  everyone but service accounts and bypassing admins; other volumes pass.
- The plan (session → seat bit, volume GUID → mask, flags) is staged by `BEGIN_PLAN`,
  `SET_SESSION`, `SET_VOLUME` under an exclusive staging push lock (one client can still
  send from several threads) and swapped atomically by `COMMIT_PLAN` under the plan push
  lock (order: staging, then plan).
  A service disconnect keeps the active plan (a crash must not open other seats' volumes).
- Port: default filter port descriptor (SYSTEM + Administrators), one connection. Message
  input is copied under SEH; sizes and magic checked; replies are one `INT32` NTSTATUS
  (`QUERY_VERSION`: `PSSG_VERSION_REPLY`).
- Unloadable (`fltmc unload PulsarSeatStorageGate`).

## Open points (HYPOTHESIS, check on a signed test build)

- `FltGetVolumeGuidName` from the instance-setup callback (documented as allowed for most
  volumes); the lazy work-item retry covers failures, not a deadlock.
- `FltQueueGenericWorkItem` keeps the instance referenced until the routine returns
  (documented behaviour; verify with Driver Verifier during a surprise removal).
- Raw opens (`\.\PhysicalDriveN`, `\.\CdRomN`) never reach a file-system filter; they are
  covered by the device descriptor gate.
- Explorer still shows the drive letter unless `session_drive_letters` is on.
- `seat-storage-filter.inf` is UTF-16 LE with BOM (Turkish strings). Git treats it as
  binary unless `.gitattributes` says `*.inf binary` / `-text` (root file, contracts owner).

## Build

```
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1 -Projects seat-storage-filter
# package: target\drivers\<Platform>\<Configuration>\seat-storage-filter\seat-storage-filter\
#          pulsarseatstoragegate.sys + seat-storage-filter.inf
```

Toolchain: [../README.md](../README.md) (Build). The INF has no `DefaultUninstall`
sections (not allowed in a driver-store package; infverif error 1102): remove with
`fltmc unload PulsarSeatStorageGate`, then `pnputil /delete-driver oem<N>.inf /uninstall`.
Version query: `PSSG_CMD_QUERY_VERSION` -> `PSSG_VERSION_REPLY {1, 0, build}`, used by the
gate on connect and by the service's driver status (`seat_media::probe_minifilter_version`).
Still to do before signing: Driver Verifier + the HVCI readiness checks on a test VM, the
altitude, the maintainer's OK.
