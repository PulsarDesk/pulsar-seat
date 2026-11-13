# 30 — Sessions and Remote Desktop Services

This spec covers four things in the reference product:

- how it gets several interactive Windows sessions running at the same time,
- how it creates a seat's session with nobody at a keyboard,
- how it finds that session again,
- how the session ends.

It also covers the protocol-provider module, which is shipped but not used. Each
mechanism ends with a "Pulsar Split approach" section.

Evidence markers are the same as in `00-architecture.md`: **[S]** static, **[L]** live,
**[I]** inferred, **[?]** unknown. This spec has no code. Registry value names, object
names and Windows API names appear only where an implementer needs them for interop or
to understand the mechanism.

Related specs: `00-architecture.md` §6.2, §7.1–7.2 and §8 item 1; `20-service.md` §5–§7;
`40-display-hdr.md` for the per-seat display.

> **Decision gate.** Multi-session on Windows *client* editions is an open maintainer
> decision (`docs/agents/plan.md`, "Known risks"). §2.4 describes what the reference does,
> at the behaviour level only. It does **not** ask anyone to build a `termsrv.dll`
> patcher. Until the maintainer decides, implementers build the **Windows Server path**
> (§2.5 path A) and the edition/coexistence checks only.

---

## 1. Overview

```
 seat service (session 0, LocalSystem)
   |
   | 1. system prep: RDS registry values; on client SKUs the reference also
   |    redirects TermService to its wrapper and restarts TermService (§2.4)
   | 2. wait for RDS readiness (Global\TermSrvReadyEvent)                    (§3)
   | 3. per seat: check the seat user, ensure Remote Desktop Users membership  (§4)
   | 4. per seat: start headless RDP client --TCP 127.0.0.1:<RDP-Tcp PortNumber>--+
   |                                                                              v
   |                                         Remote Desktop Services (TermService)
   |                                           TLS + NLA (CredSSP/NTLM)
   |                                           creates session N for the seat user
   | 5. enumerate WTS sessions, match client name == seat name  <------------------+
   | 6. store SessionId, wait for display ready, start stream server in session N
   | 7. stop: end stream server, end client (session disconnects), log off session N
```

Five files in the package belong to this area [S]:

| File | Role in this spec |
| --- | --- |
| `Duo.exe` (service) | registry preparation, TermService wrapping, client launch, session discovery, log-off, sweeps |
| `TermWrap.dll` | ServiceDll wrapper. It loads the real `termsrv.dll` and patches it in memory so client SKUs allow concurrent sessions. It is version-gated. |
| `DuoRdp.exe` | headless loopback RDP client: Python 3.13 with the MIT `aardwolf` library, packed as a PyInstaller one-file bundle |
| `DuoRds.dll` | WRDS protocol provider (COM). It is shipped but **not registered and not used** in 1.6 [S][L] (§11). |
| `rfxvmt.dll` | Microsoft "RemoteFX VM Transport" binary, shipped in the program folder [S]. No product binary references it (§12). |

The seat start/stop *trigger* is outside this spec. In Pulsar Split it arrives over the
manager ↔ service **named pipe** (plan.md: no local web server). The reference's HTTP
routes are not part of the Pulsar Split design.

## 2. Multi-session enablement

### 2.1 What each Windows edition does by default (Windows facts)

| Edition | Remote Desktop host | Concurrent interactive sessions |
| --- | --- | --- |
| Home (EditionID `Core`, `CoreSingleLanguage`, `CoreCountrySpecific`, `CoreN`) | none. TermService exists, but incoming RDP is not licensed or enabled. | 1 (console) |
| Pro / Pro for Workstations / Education / Enterprise (not multi-session) | yes | **1**. A remote logon takes over or disconnects the console session. A second remote user replaces the first. termsrv enforces this in its licensing and session-arbitration logic. Registry values alone do not lift it. |
| Enterprise multi-session | yes | many. Licensed only on Azure Virtual Desktop. |
| Windows Server without the RD Session Host role | yes | 2 remote sessions for administration, plus the console |
| Windows Server with the RD Session Host role | yes | many. Each user or device needs an RDS CAL (§13). |

The reference does not check the edition. Only the wrapper's version gate rejects a host
[S][I]. On Home there is no RDP listener to connect to, so seats fail when the session is
created [I].

### 2.2 Edition, build and coexistence checks

Live box [L]: EditionID `Professional`, InstallationType `Client`, ProductType `WinNT`,
CurrentBuild 26300, UBR 9974, DisplayVersion 26H2. The ProductName string still says
"Windows 10 Pro", so do not use ProductName. `termsrv.dll` fixed file version is
10.0.26100.9818. The file's *version string* resource said .8875, which was stale, so
always read the binary fixed version (`VS_FIXEDFILEINFO`), never the string [L].

**Pulsar Split approach**

- Detect the edition with `GetProductInfo` / `RtlGetVersion` plus
  `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\EditionID` and `InstallationType`,
  and `HKLM\SYSTEM\CurrentControlSet\Control\ProductOptions\ProductType`
  (`WinNT` = client, `ServerNT` / `LanmanNT` = server).
- Home edition: refuse multi-seat. Tell the operator that the edition has no Remote
  Desktop host. Pulsar Split never adds the missing components.
- Server: check that the RD Session Host role is installed (the `RDS-RD-Server` feature)
  and that a licensing mode is configured. If either is missing, warn, but let one test
  seat run inside the grace period (§13).
- Client Pro/Enterprise: multi-seat depends on §2.5 path B. Until the maintainer
  decides, report "multi-session not available on this edition" and allow at most one
  seat, which replaces the console session.
- Coexistence (`00-architecture.md` §10): if `TermService\Parameters\ServiceDll` is not
  `%SystemRoot%\System32\termsrv.dll`, another product has wrapped TermService. Pulsar
  Split then changes no RDS settings. It reports which DLL is configured and stops.

### 2.3 Registry values the reference writes at service start [S][L]

The reference writes these values on every service start [S]. The data column holds the
values read on the live box with the service stopped [L]. The values stay in place
after the service stops, and the reference does not restore them [L]. All keys are under
HKLM. "TS" means `SYSTEM\CurrentControlSet\Control\Terminal Server`, and "Pol" means
`SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services`.

| Key | Value | Data | Windows meaning | Pulsar Split |
| --- | --- | --- | --- | --- |
| `SYSTEM\CurrentControlSet\Control\Hvsi` | whole key | deleted [S]; absent [L] | Application Guard (Hyper-V isolated) container settings. No side effect is visible on the live box. | **skip** |
| TS | `fDenyTSConnections` | 0 | allow incoming RDP | write 0 (needed for loopback) |
| TS | `fSingleSessionPerUser` | 0 | one account may hold several sessions | **leave at default (1)**. Pulsar Split forbids sharing an account (§10). [?] confirm reconnect behaviour |
| TS | `ContainerMode` | deleted [S]; absent [L] | container-hosted RDS mode. Undocumented. | **skip** |
| `TS\Licensing Core` | `EnableConcurrentSessions` | 1 | concurrent-session switch. termsrv honours it on client SKUs only when patched [I]. | path B only |
| `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon` | `AllowMultipleTSSessions` | 1 | allow several sessions at the Winlogon level (fast user switching) | path B only |
| `TS\WinStations` | `SetGpuRealtimePriority` | 1 | real-time GPU scheduling priority for remote sessions | optional, default on |
| `TS\WinStations\RDP-Tcp` | `Shadow` | 0 | session shadowing disabled | write 0 (privacy) |
| same | `UserAuthentication` | 1 | Network Level Authentication required | write 1 |
| same | `SecurityLayer` | 2 | TLS security layer | write 2 |
| same | `PortNumber` | 3389 | listener port. The reference only **reads** it (loopback target) and never writes it [S]. | read only |
| same | `MinEncryptionLevel` | 2 | not written by the reference. Windows default. | leave |
| `TS\AddIns` | key | created (empty) | container for virtual-channel add-ins | verify only |
| `TS\AddIns\Clip Redirector` | `Name` / `Type` | `RDPClip` / 3 | clipboard redirector add-in. The data equal the Windows defaults, so the reference repairs a deleted key [I]. | verify only, do not rewrite |
| `TS\AddIns\DND Redirector` | `Name` / `Type` | `RDPDND` / 3 | drag-and-drop redirector add-in (Windows default [I]) | verify only |
| `TS\AddIns\Dynamic VC` | `Type` | 0xFFFFFFFF | dynamic virtual channel host (Windows default [I]) | verify only |
| `SYSTEM\CurrentControlSet\Control` | `SvcHostSplitThresholdInKB` | 3670016 | the Windows default (3.5 GB). Above it, each svchost service gets its own process. On the live box (32 GB) TermService runs alone in its svchost [L], so a TermService restart affects nothing else. | skip (default) |
| `SYSTEM\CurrentControlSet\Control\Lsa` | `LimitBlankPasswordUse` | 0 | lets network/RDP logons use blank passwords | **never**. Seat users must have passwords. |
| `SYSTEM\CurrentControlSet\Control\GraphicsDrivers` | `TerminateIndirectOnStall` | 0 | do not kill an indirect display driver that stalls | decided in `40-display-hdr.md` |
| same | `IddCxDebugCtrl` | 128 | IddCx debug control flag (undocumented) | **skip** |
| `SYSTEM\CurrentControlSet\Control\Keyboard Layout` | `IgnoreRemoteKeyboardLayout` | 1 | the session keeps its own layout and ignores the client's | write 1 (the layout comes from the seat user's profile) |
| Pol | `fDenyTSConnections` | 0 | policy: allow RDP | only if a policy currently denies RDP. Then warn and ask. |
| Pol | `bEnumerateHWBeforeSW` | 1 | policy: use the hardware GPU for RDP sessions | write 1 (seat apps must render on the GPU) |
| Pol | `fEnableWddmDriver` | 1 | policy: use the WDDM indirect display driver for RDP sessions | see `40-display-hdr.md` |
| Pol | `AVCHardwareEncodePreferred`, `AVC444ModePreferred` | deleted | RDP H.264 encoder policies | skip (frames do not travel over RDP) |
| Pol | `MaxCompressionLevel` 1, `ImageQuality` 2, `GraphicsProfile` 1, `VisualExperiencePolicy` 1, `VGOptimization_CaptureFrameRate` 1, `VGOptimization_CompressionRatio` 1 | as listed | RDP graphics-pipeline tuning (bandwidth vs quality) | skip. The loopback RDP stream carries no frames in our design. |
| Pol | `fEnableVirtualizedGraphics` | 1 | RemoteFX vGPU policy (feature removed from current Windows) | skip |
| Pol | `Shadow` | 0 | policy: no shadowing | write 0 |
| `Pol\Client` | `EnableHardwareMode` 1, `fUsbRedirectionEnableMode` 2 | as listed | *client-side* (mstsc) settings. They do not affect the headless client [I]. | skip |
| `SOFTWARE\Microsoft\PolicyManager\default\Connectivity\DisableCrossDeviceResume` | `value` | 1 | turns off cross-device resume | out of scope here (`50-shim-isolation.md`) |

`00-architecture.md` §6.2 lists other boot changes that belong to other specs:
Hiberboot off, controller-to-VK mapping, FeatureManagement overrides, and the firewall.

**Pulsar Split approach**

- Write only the "write" rows. Before the first change, save each original value (or
  "absent") under the Pulsar Split service key. Restore them on uninstall and when the
  operator switches multi-seat off.
- Avoid the `Policies` key except for the rows above. Those values override Group Policy.
  If a domain policy also sets them, the domain value wins on the next refresh. Detect
  this and report it.
- **Firewall:** the loopback connection needs no inbound rule. Do *not* enable the
  built-in "Remote Desktop" rules, which the reference does (`20-service.md` §5).
  `fDenyTSConnections` = 0 opens the listener on every interface. Pulsar Split adds an
  inbound **block** rule for TCP/UDP `PortNumber` on non-loopback addresses unless the
  operator asks for remote RDP.

### 2.4 Reference: TermService wrapping on client SKUs (behaviour only) [S][L]

- The service sets `HKLM\SYSTEM\CurrentControlSet\Services\TermService\Parameters\ServiceDll`
  to `%ProgramFiles%\Duo\TermWrap.dll`. The original value is
  `%SystemRoot%\System32\termsrv.dll`. `ServiceDllUnloadOnStop` = 1 on the live box [L].
- It then restarts TermService. It stops the dependent services first: on the live box
  the only dependent is `UmRdpService` [L], and the strings also name `SessionEnv` and
  `CertPropSvc` [S]. If TermService does not stop, the service ends the hosting svchost
  by process id [S]. Then it starts the service again. If the stop or the start fails, or
  the wrapper refuses the installed `termsrv.dll` version, boot fails and no seat starts
  [S]. The SCM wait timeout is [?].
- TermService on the live box: `svchost.exe -k NetworkService`, start type Manual. It
  depends on `RPCSS` and `StateRepository`. Recovery is "restart after 60 s" twice, with
  the counter reset after 1 day [L].
- Inside TermService, the wrapper patches `termsrv.dll` in memory so that more than one
  interactive session is allowed. It is version-gated: an unrecognised build is
  refused, and the service logs that the version is unsupported [S]. On the live box,
  build 10.0.26100.9818 was accepted [L]. This spec describes no patch method, no patch
  targets and no version table.
- The redirection **stays in place after the reference service stops** [L], so the
  wrapper keeps running in TermService whenever TermService runs.
- Memory Integrity: in-memory patching of a user-mode service is not a kernel change.
  HVCI does not block it. The wrapper is signed with the vendor's self-signed
  certificate, and svchost loads it on the live box, so no Microsoft-signed-only DLL
  mitigation is in force for that svchost group [L][I].

### 2.5 Pulsar Split approach: two paths

**Path A — Windows Server + RD Session Host (first-class, build now).**

- Requirements: Windows Server 2022/2025 (Desktop Experience) with the RD Session Host
  role, an RD Licensing server or a per-user/per-device mode, and one RDS CAL per seat
  user or device (§13). TermService is not modified.
- Pulsar Split writes only the §2.3 "write" rows. No ServiceDll change, no TermService
  restart.
- Server enforces session limits through policy (`MaxInstanceCount` on the listener,
  "Limit number of connections"). Pulsar Split reads them and refuses to start more
  seats than they allow.

**Path B — client SKU (Pro/Enterprise): maintainer decision pending.**

Two options exist. Until the maintainer decides, neither is implemented.

| Option | What Pulsar Split does | Legal / technical facts |
| --- | --- | --- |
| B1: operator-provided | Pulsar Split ships nothing that changes termsrv. If the operator has already arranged concurrent sessions, Pulsar Split detects that a second session works and uses it. It records the configured ServiceDll in diagnostics. | The licence question stays with the operator. Pulsar Split still must not fight another product's redirection (§2.2). |
| B2: own wrapper | a separate, optional, clearly labelled component. It is off by default and needs explicit opt-in that warns about licence terms. It fails closed on any build it does not recognise and restores the original ServiceDll on uninstall or opt-out. | Windows client licence terms do not allow several concurrent remote users (§13). Patching a Microsoft binary in memory may also break the licence's reverse-engineering and modification terms. AV products often flag such wrappers as hack tools or PUA [I]. Each `termsrv.dll` servicing update can break it. The DLL needs a code-signing certificate that chains to a public root. Pulsar Split never installs a private root (`00-architecture.md` §8 item 4). |

## 3. Ordering and RDS readiness [S][I]

- Every WTS call is preceded by a wait on the Windows-owned event
  `Global\TermSrvReadyEvent`. If the event does not exist yet, the service creates it
  itself, so a later wait succeeds once TermService signals it [S].
- Boot order: RDS registry values, then TermService wrapping and restart (client SKUs),
  then readiness, then the stale-session sweep (§9.2), then the seats [S].
- Seats start one after another. Each seat waits for its own session before the next
  seat starts [S][I].

**Pulsar Split approach:** open `Global\TermSrvReadyEvent` with SYNCHRONIZE only and
never create it. If it does not exist, poll for TermService to be RUNNING and for
`WTSEnumerateSessions` to succeed, for up to 120 s. After that, report "Remote Desktop
Services not ready". Path A never restarts TermService. Seats may start in parallel,
because the client-name match (§6) tells their sessions apart.

## 4. Seat user prerequisites [S][L]

| Check | Reference behaviour | On failure |
| --- | --- | --- |
| account details readable | user info is queried at level 4 | seat does not start (error logged) |
| password expired | expiry flag is checked | seat does not start (error logged) |
| member of Remote Desktop Users (`S-1-5-32-555`) | the group name is resolved from the well-known SID (works on localised Windows) and the user is added with `NetLocalGroupAddMembers` [S]. A second routine writes the local SAM alias key directly; it holds the security database under `SAM\SAM\Domains\Builtin\Aliases` [S]. | seat does not start (error logged) |
| profile exists | at first start the service logs the user on, builds the user's environment block and runs a helper copy of itself as that user, which creates the home directory. A timeout ends the helper [S]. | [?] |
| keyboard layout | read from the user's hive unless the seat overrides it (`20-service.md` §6.2) | — |

Live box: the RDU group has 3 members [L].

**Pulsar Split approach**

- Use only `NetLocalGroupAddMembers` / `NetLocalGroupGetMembers` with the well-known
  SID. **Never write the SAM hive.** If the API call fails, the seat does not start, and
  the manager shows the reason (the account cannot be added to Remote Desktop Users).
- Require a non-blank, non-expired password. Do not change `LimitBlankPasswordUse`.
- Create the profile with `LoadUserProfile` (after `LogonUser`) inside the service.
  There is no self-spawned helper.
- Seat users need not be administrators (`20-service.md` §10).

## 5. Session creation: headless loopback RDP client

### 5.1 Reference client contract [S]

| Input | Source |
| --- | --- |
| target address | always `127.0.0.1` |
| port | `RDP-Tcp` `PortNumber` (3389 on the live box [L]) |
| client name | seat name |
| user name | seat user (local account; no domain given) |
| password | seat user's password. The reference passes it **on the command line**. |
| keyboard layout | seat override or the user's preload layout |
| width, height | seat's last known resolution |

- The client process is started by the service in session 0 as SYSTEM. Its TEMP/TMP
  point to a private `DuoRdp` folder under the system temp directory, which the
  service creates [S]. The service deletes that folder on stop [S].
- Connection: TLS security layer, then CredSSP (NLA) with an **NTLM** credential [S].
  The server's self-signed RDP certificate is accepted without validation. That is
  the library's behaviour, and the target is loopback [I]. The live box has one
  certificate in the "Remote Desktop" machine store [L].
- Session parameters requested [S]: 32 bpp, no virtual channels (no clipboard, drive,
  audio or printer redirection from the client), font smoothing and similar desktop
  performance flags. The audio endpoint is the session's own "Remote Audio" device
  (`70-audio.md`).
- Lifetime: the client stays connected and reads server traffic until the server ends
  the connection or the process is killed. When it exits, the session moves to
  *disconnected* [S][L]. The client does not reconnect by itself [S][I].
- A PyInstaller one-file bundle unpacks into TEMP on every start. That is why each run
  gets its own TEMP folder [I].

### 5.2 Pulsar Split approach

**Decision needed (recommendation given).**

| Candidate | Licence | For | Against |
| --- | --- | --- | --- |
| FreeRDP (`libfreerdp`, headless use) | Apache-2.0 (GPL-3 compatible) | mature; NLA/Kerberos/NTLM; maintained; native ARM64/x64 | large C dependency; must be built without channels we do not use |
| aardwolf | MIT | small; proven by the reference | Python runtime and PyInstaller bundle; AV false positives; slow start |
| own Rust client (e.g. on IronRDP, MIT/Apache-2.0) | compatible | one language; in-process option; small | needs CredSSP/NTLM in Rust (IronRDP has it); less field-tested |

Recommendation: **IronRDP-based helper in Rust**, with FreeRDP as the fallback if
IronRDP's CredSSP fails on a target build. Either runs as a child process of the service,
so a client crash cannot take the service down.

Requirements whatever client is chosen:

- **The password never appears in argv, environment, files or logs.** The service passes
  it to the child through an inherited anonymous pipe or handle, and the child reads it
  once at start-up. Only seat name, port, layout and size go in argv. The password buffer
  is wiped after the handshake.
- Keep NLA on. Pin the server certificate: before connecting, read the thumbprint of the
  local RDP listener certificate (`Win32_TSGeneralSetting.SSLCertificateSHA1Hash`) and
  reject any other certificate. Loopback does not by itself guarantee the far end is
  TermService.
- Connect to `127.0.0.1` (or `::1` if the listener is IPv6-only) on the configured
  `PortNumber`. Never hard-code 3389.
- The child reports its state to the service on stdout or the pipe: connecting,
  authenticated, connected, disconnected(reason), exited. The service treats a child
  exit as "seat session disconnected" (§9).
- Run the child with the lowest identity that works: a restricted SYSTEM token or a
  per-service virtual account. It needs no special privilege for an outbound loopback
  TCP connection.

## 6. Session discovery by client name [S][L]

- The service enumerates sessions with `WTSEnumerateSessions` (level 1). It skips the
  `Services` and `Console` WinStations and the `RDP-Tcp` listener. For each remaining
  session it queries `WTSClientName` and compares it with the seat name. It also logs
  the full `WTSClientInfo` record for debugging [S]. The first match gives the seat's
  session id, which is written to `Instances\<seat>\SessionId` [S].
- Live box: seat logon events show source address 127.0.0.1. Session ids are reused
  (2–5) [L]. `SessionId` is **left behind** after a stop (two seats both still held 2)
  [L]. Nothing should trust a stored id without checking it again.
- Client-name limits come from the RDP protocol: the client name field holds **at most
  15 UTF-16 characters**, and longer names are cut off by the client. Whether the
  reference compares case-sensitively is [?]. A seat name longer than 15 characters
  would never match, so that seat fails with "session not found" [I]. Live seat names
  are 4–7 ASCII characters [L], so this was not observed.

**Pulsar Split approach**

- Do not use the human seat name as the RDP client name. The service gives each seat a
  generated client name, for example a fixed 3-letter prefix and an 8-hex-digit id
  (11 ASCII characters), and stores it with the seat. That avoids truncation, case and
  non-ASCII problems and collisions with real RDP clients.
- Match on three things: client name, user name (`WTSUserName`) **and** client address
  = loopback. Prefer the session id the client reports, if the client library exposes
  it.
- Track sessions with `WTSRegisterSessionNotification`-style service notifications
  (`SERVICE_CONTROL_SESSIONCHANGE`) instead of polling, and keep polling as a fallback.

## 7. Timeouts and readiness (requirements)

| Phase | Reference [S][L] | Pulsar Split requirement |
| --- | --- | --- |
| session appears after client start | about **5 min** | default 60 s and configurable. Fail early if the client child reports an auth error. |
| display ready (`Instances\<seat>\Status`: 0 pending, 1 ok, other = failed) | about **90 s** | 30 s default. Readiness is reported over the internal display IPC, not a registry poll (`40-display-hdr.md`). |
| what the user sees on failure | seat stays offline in the manager. One error event in the Application log names the seat and the phase (session, display, token, stream server). | the seat enters *Failed* with a reason code (`client-auth`, `session-timeout`, `display-timeout`, `token`, `stream-server`) sent over the named pipe. The manager shows Turkish text per code. A failed seat logs off its session (§9.1). |

## 8. Launching processes into the seat session [S]

- **Stream server:** the service duplicates its own (SYSTEM) primary token and sets
  `TokenSessionId` to the seat's session. It then creates the stream server on
  `winsta0\default` in that session, so the stream server runs **as SYSTEM in session
  N** [S]. The service needs `SeTcbPrivilege` to set the session id.
- **Autostart apps:** either a duplicated SYSTEM token in session N or the user's token
  from `WTSQueryUserToken`, depending on the per-entry flag. User-token queries are
  retried while the session is still logging on [S].

**Pulsar Split approach:** the stream server runs as SYSTEM in session N (it needs
input-injection and capture rights across UAC desktops). Apps launched from the stream
server or as autostart entries run with the **seat user's** token from
`WTSQueryUserToken`, a user environment block (`CreateEnvironmentBlock`) and the
profile loaded. Running apps as SYSTEM is not offered. Set a job object per seat so the
service can end every child in one step.

## 9. Stop semantics and sweeps

### 9.1 Seat stop [S][L]

Observable behaviour: the stream server and autostart processes are ended, then the
RDP client is ended. The session then disconnects, with LSM events 40 and 24 (reason
`ERROR_NETNAME_DELETED`). After that the service **logs the session off** (LSM event
23), 0–10 s after the disconnect on the live box [L]. Only remote sessions are logged
off: a session is checked with `WTSIsRemoteSession` before `WTSLogoffSession` [S]. The
console is never logged off.

So **stop = log off**. This answers `20-service.md` §6.4 / §11.

**Pulsar Split approach:** stop = log off by default. Make "leave disconnected" a seat
option for fast resume. When logging off, wait for the session to disappear (up to 30 s),
then report *Stopped*. If the client child exits on its own while the seat is *Running*,
start the client again (the session reconnects). After 3 failures in 5 min, mark the
seat *Failed*.

### 9.2 Service start/stop sweeps [S]

- **Stale sessions:** on service start and stop, the service enumerates sessions and
  logs off every *remote* session that is **not active**: disconnected or idle seat
  leftovers. It skips `Services`, `Console`, listener ids (≥ 65536) and active
  sessions [S].
- **Stale processes:** on service start and stop, the service ends leftover processes
  of its own image paths: the service binary (other instances), the RDP client and the
  stream server [S].
- **Temp folder:** on service stop, the client's temp folder is deleted [S].

**Pulsar Split approach:** do the same, but narrower. Log off only sessions whose
client name has the Pulsar Split prefix *and* whose user is a configured seat user. A
manual disconnected RDP session of an operator must survive. End only processes
recorded in the seat state file (PID plus creation time) or found in the per-seat job
object, never by image path alone.

## 10. Special cases

| Case | Reference | Pulsar Split |
| --- | --- | --- |
| console user = seat user | allowed by `fSingleSessionPerUser` = 0: the account gets a second session [I]. Not checked [?]. | refuse: a seat user may not be the console user or another seat's user (case-insensitive SID compare). |
| reboot / shutdown | sessions end with LSM event 40 and **no** event 23 (no log-off by the service) [L]. The next boot sweep cleans up. | same; nothing extra is needed. Fast startup (Hiberboot) is turned off by the reference (`00-architecture.md` §6.2) so that a "shutdown" is a real boot. Pulsar Split recommends that too, as an operator choice. |
| suspend / resume | seats are stopped on suspend and started on resume (`20-service.md` §7) | same |
| seat user logs on at the console during a running seat | [?] | refuse console logon for seat users through the "Deny log on locally" right, which the manager sets as an option |

## 11. WRDS protocol provider (unused) [S][L]

- `DuoRds.dll` is a COM in-process server, CLSID `{c8769a24-bb6c-4e13-9491-1bbaa5ab27ae}`
  [S]. It is not registered in HKCR on the live box [L].
- It implements the Windows Remote Desktop protocol-provider interfaces
  `IWRdsProtocolManager`, `IWRdsProtocolListener`, `IWRdsProtocolConnection` and
  `IWRdsProtocolLicenseConnection` [S]. Through them it would act as its own listener
  in place of the RDP stack. It would accept "connections" that the service asks for via
  shared memory and events (`Global\Duo_RDS_*`, `00-architecture.md` §5.3), and report a
  client name, resolution, keyboard layout and colour depth.
- A WinStation would point at it through `LoadableProtocol_Object`. On the live box
  `RDP-Tcp` has the Microsoft default `{5828227c-20cf-4408-b73f-73ab70b8849f}` and
  `Console` has none [L]. No other binary in the package uses its objects [S].
- Why unused [I]: a custom protocol must supply input and video handles and licensing
  answers, which is a large surface. The loopback RDP client gets the same result with
  the stock stack.

**Pulsar Split approach:** not implemented in Phases 1–4. If loopback RDP ever turns out
unsuitable, the WRDS provider API is public (Windows SDK `wrdsprotocol` headers) and
could be built clean-room from Microsoft documentation. It changes the WinStation
registration, so it needs its own spec and a maintainer decision.

## 12. `rfxvmt.dll`

- It is a Microsoft binary, "RemoteFX VM Transport", version 10.0.15063.0 [S]. No
  binary in the package imports it or names it [S].
- On the live box, `System32\rfxvmt.dll` is byte-identical to the shipped copy (same
  old 15063 version on a 26300 build) [L]. That suggests someone copied it into System32
  (the product or the operator) [I]. Community multi-session guides use this file to
  make RDP work on editions that lack it [I].
- Shipping or copying Windows system files is not allowed by the Windows licence
  (`00-architecture.md` §8 item 2).

**Pulsar Split approach:** never ship or copy `rfxvmt.dll` or any other Microsoft DLL
(status.md: no shipped Microsoft DLLs). Nothing in the Pulsar Split design depends on
it. The display comes from Pulsar Split's own IDD (`40-display-hdr.md`). If the stock
RDP stack on an edition needs it, that edition is unsupported.

## 13. Licensing (facts for the maintainer's decision)

- **Windows client (Pro/Enterprise):** the licence terms allow remote access to the
  device by one user at a time. Several concurrent interactive users on one client
  licence is not a licensed use. Microsoft offers this only through Windows Server +
  RDS CALs or Azure Virtual Desktop (Enterprise multi-session).
- **Windows Server + RD Session Host:** each concurrent user (per-user CAL) or device
  (per-device CAL) needs an RDS CAL in addition to the Server licence. Server allows a
  grace period (120 days) before an RD Licensing server must issue CALs. After that,
  logons are refused. Pulsar Split shows the licensing mode and grace state and does not
  try to bypass it.
- **termsrv wrappers** (reference `TermWrap`, RDP Wrapper and similar): grey to
  non-compliant, as described in §2.5 path B. Pulsar Split must state this to the
  operator in the manager before any client-SKU multi-seat option is offered.
- Pulsar Split itself has no licence or activation features (plan.md).

## 14. Error cases

| Condition | Reference [S][L] | Pulsar Split |
| --- | --- | --- |
| Home edition | fails late (no listener) [I] | refuse at install/boot with a reason (§2.2) |
| `termsrv.dll` build not recognised by the wrapper | boot fails; no seats | path A: n/a. Path B2: fail closed and keep the original ServiceDll |
| TermService cannot be stopped/started | boot fails | path A never restarts it |
| another product has redirected TermService | the reference overwrites it | refuse to change RDS settings; report it (§2.2) |
| user cannot be added to Remote Desktop Users | seat not started | same, reason code `rdu-membership` |
| password expired / account unreadable | seat not started | same, reason codes `password-expired` / `account` |
| client cannot authenticate (wrong password, NLA failure) | seat times out after about 5 min | child reports `client-auth` at once; seat *Failed* |
| session not found in time | seat not started; error logged | `session-timeout`; client ended; any matching session logged off |
| display not ready in time | seat not started | `display-timeout` |
| token duplication / session-id change fails | seat not started | `token` |
| client exits while seat runs | [?] (watchdog covers only the stream server) | restart client; *Failed* after 3 tries in 5 min |
| listener port changed while running | not handled [?] | re-read `PortNumber` at each client start |

## 15. Open questions

- [?] SCM stop/start timeouts the reference uses when it restarts TermService. Path A
  does not need them.
- [?] Whether the reference compares client names case-sensitively. Pulsar Split avoids
  the issue (§6).
- [?] Reconnect behaviour with `fSingleSessionPerUser` = 1 when the client restarts
  while its session is disconnected. Expected [I]: Windows reconnects the user to the
  disconnected session. Confirm on a test box before relying on it.
- [?] Whether the profile-creation helper has a fixed timeout and what happens to the
  seat if it fails.
- [?] Effect of the undocumented `ContainerMode` value and the `Hvsi` key deletion.
  Pulsar Split skips both, so this is informational only.
- [?] Who copied `rfxvmt.dll` into System32 on the live box (installer, service or
  operator). No package binary references it, and the uninstall log lists only the
  program-folder copy.
- [?] Real-world IronRDP CredSSP/NTLM compatibility against TermService on 24H2/26H2.
  Prototype before committing to it (§5.2).
