# 30 — Concurrent local sessions (one per seat)

Status: draft v1 (2026-11-12, critiqued, cross-checked). Analyst-written. Behaviour and interfaces only; see
[../CLEANROOM.md](../../CLEANROOM.md).

Confidence markers:

- No marker: seen directly in the reference product's files, its registry values, its UI, or
  the Windows event logs of a PC that ran it.
- **HYPOTHESIS**: inferred from indirect evidence (imported API names, string tables, timing).
  Not confirmed at runtime.

Related specs: `20-service.md` (service lifecycle, IPC), `40-display.md` (which monitors a
session sees), `50-input.md` (which devices a session gets), `60-audio.md`,
`70-usb-devices.md`, `80-apps-compat.md`, `90-install.md`.

---

## 1. Scope

This file covers:

- How each seat gets its own interactive Windows session on one PC.
- What each seat shows at logon, and how per-seat auto-logon works.
- What lock, sign-out, switch user and re-logon do on a seat.
- How the product binds a session to a seat.
- What happens at boot and at seat start, and when the product refuses to start seats.
- The client-SKU licensing concern.

Display, input and audio routing into the session are covered in 40/50/60. This file names
them only where the session flow depends on them.

## 2. Terms

| Term | Meaning |
| --- | --- |
| Seat | One workplace: its monitor(s), keyboard, mouse, audio and USB. The reference product numbers seats from 1. Version 2.70.3 supports 12 seats (all seat masks are 12 bits wide). For 2.70.6, 16 seats is a **HYPOTHESIS**: the only evidence is that the driver's per-seat audio-state names run up to a 16th seat. The 2.70.6 translations do not mention a seat count above 12. Internally a seat is a "terminal". |
| Seat 1 | The main seat. It always exists and is always enabled. Its session is the normal Windows console session. The product refuses a configuration in which seat 1 has no display. |
| Extra seat | Seat 2 and higher. |
| Seat session | The Windows session (session ID ≥ 1) that runs the user desktop of one seat. |
| Seat bitmask | One bit per seat, with bit 0 = seat 1. It is stored as a DWORD, or in the UI, reports and JSON as a string of 0/1 characters. |

## 3. Reference product behaviour

### 3.1 Components involved

| Component | Role in sessions |
| --- | --- |
| Seat service (Windows service, own process, LocalSystem) | Starts the extra seat sessions, sequences them, handles auto-logon data, checks for the same account on several seats, and runs a helper process inside each seat session. |
| Service DLL (also a COM credential provider) | Holds the service logic. While the product is enabled, it is registered as an extra credential provider that logs on the seat's account in that seat's session. |
| Kernel driver | Gives each seat its own view of some logon-related registry values. It also works around Windows' single-console assumptions (§3.11). |
| Manager (tray GUI) | Holds the per-seat account settings, the start mode, and the per-seat start and re-logon actions. One instance runs in each seat's user session. |
| Small public DLL for apps (x86 and x64) | Maps a session ID or a process ID to a seat number. It also returns the maximum seat count. |
| Licence component | Exists. Out of scope. |

Service registration facts (live):

- The service is a Win32 own-process service that runs as LocalSystem.
- It is in load-order group `COM Infrastructure`, the same group as LSM.
- It depends only on `RpcSs` and `LSM`. It does **not** depend on `TermService`.
- Its security descriptor gives SYSTEM and Administrators control. Interactive and service
  users may only query its status and configuration, interrogate it, and send it
  user-defined control codes. They cannot start or stop it.
- No service failure actions are configured.

### 3.2 Session model

- **Seat 1 = console session.** At boot, Windows starts the console session as usual (normally
  session 1). Seat 1 gets the standard logon screen, or the standard Windows auto-logon if the
  owner turned it on (`Winlogon\AutoAdminLogon`).
- **Extra seats = more local sessions.** The seat service has Windows create one more session
  for each extra seat that is set to start. Each session has its own winlogon, LogonUI and
  user shell, like a fast-user-switching session.
- **Not RDP.** The LSM operational log records the extra seats' sessions with client address
  `LOCAL`, the same as the console. RDP loopback sessions show `127.0.0.1` there. The product
  does not use TermService or the RDP stack. On the test PC, TermService belonged to a
  different product.
- **The console is lent for a moment, then returned.** While an extra seat starts, LSM's
  console connection moves from seat 1's session to the new session. LogonUI, logon and shell
  start run there. Then the console connection moves back to seat 1. After that, LSM regards
  the extra session as *disconnected*, but it stays visible and usable on its own monitors.
  The display and input layers keep it alive (40/50).
- **Logon events.** For extra-seat sessions, LSM usually logs only the session-logon (21) and
  shell-start (22) events. The "session logon begin / end" pair (41/42) is mostly missing: in
  the product's period of use it appeared for 2 of 9 seat-2 logons (41) and 1 of 9 (42). Seat 1
  had the pair for about half of its logons. **HYPOTHESIS**: the extra seats' logon goes
  through a reconnect-like path in LSM, not through the normal new-logon path.
- **Session IDs.** Windows allocates session IDs from its normal pool (session 2 or 3 for
  seat 2, and so on) and reuses them after a session ends. Seat numbers and session IDs are not
  the same thing; a mapping joins them (§3.8).

Observed timeline of one extra-seat start (from the LSM operational log; seat 1 user already
logged on about 10 s earlier):

| t (s) | What Windows logged |
| --- | --- |
| −2 | A new manager log file appears for seat 1's context (**HYPOTHESIS**: the service launches a manager or helper instance for the start). |
| 0 | Session 1 (seat 1) is disconnected from the console by a session-0 caller. |
| +2 | Session 1 is reconnected. A new session object for the extra seat appears. |
| +4 | Session 1 is disconnected again. The extra seat's session gets the console, the seat account logs on, and the shell starts within about 1 s. |
| +16 | The extra seat's session is disconnected (from LSM's view), and session 1 is reconnected. |

The whole start takes about 15–20 s for each seat. While it runs, seat 1's screen briefly
loses its session (**HYPOTHESIS**: seat 1 sees a short black or blank moment). A seat
sign-out or re-logon also takes the console for about 1 s and then gives it back.

### 3.3 Start modes and seat start

Settings (manager, general settings):

| Setting | Values | Stored as |
| --- | --- | --- |
| How to start the seats | Manual; at system startup; when a user logs on at seat 1 | `AutoStartMode` (DWORD) in the product's HKLM software key. 1 = at system startup (seen live together with that UI choice); the 0/2 mapping is a **HYPOTHESIS**. |
| Seats started automatically | One checkbox per seat. Seat 1 is always on. | Seat bitmask `startstations` (DWORD) in a *persistent* context sub-key of the product's HKLM software key (the sub-key's name is brand-derived; Pulsar uses its own). It is **not** in the volatile runtime key. |
| Product enabled / disabled | Enable and disable both need a reboot | Start type of the service and the driver (4 = disabled) |

Manual mode: the manager has a start-all-seats button, and each seat tile has a start-seat
action. Both are unavailable while the product is disabled. The 2.70.4+ tray menu can also
start seats.

Automatic start, described as observable behaviour (tunables are DWORDs under the service's
registry key):

- **Preconditions.** Seat work begins only once Windows' session infrastructure has signalled
  that it is ready (the OS's global "terminal services ready" event). For "at system startup",
  seats start only once these services are running: BrokerInfrastructure, PlugPlay,
  RpcEptMapper, RpcSs, AudioSrv and AudioEndpointBuilder. `AutoStartSync` controls whether the
  product waits for them (**HYPOTHESIS**). `AutoStartDelay` adds an extra delay.
- **Display needed.** "At system startup" expects all monitors assigned to seats to be powered
  on. A seat without an attached display does not start; its tile tells the user that it will
  start once a display is connected.
- **"When a user logs on at seat 1".** Seats start after an interactive logon in seat 1's
  session. The UI offers this as a fall-back for PCs where the boot-time start is unreliable,
  for example because monitors are not ready at boot.
- **Sequencing.** The seats marked in `startstations` start one at a time, never in parallel.
  `nextTerminalDelay` is the pause between two seats. One seat's start (one console hand-over)
  must finish within `TransitionDueTime` seconds; the shipped tweaks raise it to 30 or 40 for
  slow PCs (2.28 changelog: longer start time-out).
- **CPU affinity.** Each seat's session processes get the per-seat `Affinity` mask during
  start-up (details in 80).
- **In-session helper.** For each started seat, the service starts a helper process inside
  that seat's session. It is the service executable, created with a token whose session ID is
  set to the seat's session; the executable switches to helper mode when it finds itself
  outside session 0. Whether a command-line verb is also passed is a **HYPOTHESIS**. The
  helper resets the display mode of that session's input desktop. It does the same after
  resume from sleep or hibernation, when the service sets a global resume event.
- **Results.** The service writes status values into the volatile runtime key (§3.8):
  `svcAutoStarted` (auto-start ran), `svcAutoStrtErr` (auto-start error), `svcAutoSession`
  (which session the auto-start concerned), `canRunStatus` and `canRunBits` (whether, and for
  which seats, start is possible; `canRunBits` is also a tunable), `auxInitStatus` (state of
  the service's auxiliary initialisation), and in 2.70.6 `svcExeError`. The manager reads
  these to show the product and seat state (**HYPOTHESIS** for the exact meaning of each).

### 3.4 When the product does not start seats

| Condition | Observable behaviour |
| --- | --- |
| The previous Windows session ended uncleanly (crash, power loss) | Automatic seat start does not happen. The manager tells the user that the product is in a protective, reduced state because of the unclean end, and that a clean reboot after fixing the cause restores normal start. The service judges this from the standard `Control\LastBootShutdown` value; the service tunable `ignoreLastShutdown` overrides the check. The driver has a matching boot check on `Control\LastBootSucceeded`, overridden by its own `ignoreLastBoot`. |
| Windows booted in Safe Mode | The product does not start. The manager asks for a normal reboot. The driver reads the standard safe-boot indicator (`InitSafeBootMode`). |
| The Windows build changed while the product was enabled (feature or quality update) | The product refuses to run until the user disables it, reboots, checks the seat configuration and enables it again. |
| The Windows build is not supported by this product version | The manager says so and offers to look for a newer compatible version, or to roll Windows back. |
| The product is disabled | Start actions are unavailable; a start attempt shows an error that asks the user to enable the product first. |

Windows Update while the product is enabled:

- The service sets the standard Windows Update deferral policy values (feature- and
  quality-update deferral periods, branch readiness level, the "exclude drivers from quality
  updates" value). A service tunable `ovrUpdatesPeriod` overrides the period (**HYPOTHESIS**
  for the exact set written and when).
- From 2.70.5 the product blocks updates it considers destabilising while it is enabled. In
  2.70.6 the driver has a value `BlockWUHandlers` and a reference to the Windows Update agent
  engine DLL (**HYPOTHESIS**: it stops that engine from loading while the product is active).

### 3.5 Logon screen and auto-logon per seat

Per-seat account setting (the seat's account-edit action in the manager):

- Default: show the normal Windows logon screen on that seat.
- Or a local account (picked from the local users list), or a domain account (domain name plus
  account name).
- Password, entered twice. It may be left empty.
- The UI advises keeping seat 1 on the normal logon screen.

Storage:

| Data | Where |
| --- | --- |
| User name, domain | Per-seat sub-key named with the seat index in two digits (`00` = seat 1, `01` = seat 2, …) under the product's HKLM software key; values `DefaultUserName`, `DefaultDomainName` |
| Password | LSA private data (written with `LsaStorePrivateData`, read back with `LsaRetrievePrivateData`), never in plain registry |
| Last user who logged on at that seat | Same per-seat sub-key, value `LastLoggedOnUserSID` |
| Seat → account index (GUI cache) | The manager's log lists one account index per seat |

How the logon is done:

- While the product is enabled, the service DLL is registered as a credential provider. Its
  CLSID is written under `…\Authentication\Credential Providers` and `HKCR\CLSID`, with an
  in-process server and apartment threading. It is removed when the product is disabled
  (verified: it is absent on the disabled test PC).
- In an extra seat's session, LogonUI loads this provider. When the seat has an account set,
  the provider builds the logon for that seat (user, domain, password from the LSA secret) and
  submits it for the Negotiate package, so no logon dialog appears (**HYPOTHESIS** for the
  exact credential-provider behaviour; the imports and strings match).
- The product also reads the standard Winlogon auto-logon values (`AutoAdminLogon`,
  `DefaultUserName`, `DefaultDomainName`, `DefaultPassword`). The driver gives each seat a
  separate view of `AutoAdminLogon` (§3.11), so only the intended seat auto-logs on.
- Per-seat overrides exist: `ForceAutoLogon`, `ForcedLogoffMode` and an override flag for
  auto-logon (`ovrAutoLogon`) (**HYPOTHESIS**: they force a logon again after sign-out and
  control what happens on a forced logoff).
- Last-user memory: from 2.29.1, each seat's logon screen shows the user who last logged on at
  *that* seat, not the PC-wide last user. The driver keeps per-seat copies of
  `LogonUI\LastLoggedOnUser*`, `LogonUI\SessionData` and `IsFirstLogonAfterSignOut`.
- **Extra seat at its logon screen.** An extra seat without an account set shows LogonUI in
  its own session on its own monitors, and the seat's keyboard and mouse operate it, although
  LSM regards that session as disconnected from the console (§3.2). **HYPOTHESIS**: the
  driver's per-seat display and input routing (40/50) delivers input to that session's
  winlogon desktop independently of LSM's console state.

Branding on the logon and lock screen (optional):

- The product can set the machine-wide lock-screen image (`PersonalizationCSP`
  `LockScreenImagePath`) and a desktop wallpaper for its seats, so users can see that the seats
  are running. Pulsar Seat uses its own images only.

### 3.6 Lock, sign-out, switch user, re-logon

| Action on an extra seat | Behaviour |
| --- | --- |
| Lock (Win+L) | The seat's session shows its own lock screen on its own monitor. Unlock uses the normal Windows credential UI in that session. |
| Sign out | The session ends. If the seat has auto-logon, the product logs it on again (**HYPOTHESIS**: through `ForceAutoLogon`). Otherwise the seat shows its logon screen. The LSM log shows a short console hand-over around the logoff. |
| Shared-PC sign-out button | `SharedPCMode` is a seat bitmask under the driver's service key (the shipped tweak sets 0xFFF, all seats). For the selected seats, the lock screen shows a sign-out button. The driver gives those seats a Shared-PC view of the `SharedPC` key. |
| Switch user / fast user switching | Windows' switch-user would move the console away. The driver gives each seat its own view of the `HideFastUserSwitching` policy (**HYPOTHESIS**: hidden on extra seats, or on all seats). |
| Force re-logon (seat action) | Ends the seat's session and starts it again with a fresh logon. The manager has error texts for a failed start or re-logon. The driver values `relogReqAdmin` and `BSDReqAdmin` exist (**HYPOTHESIS**: whether re-logon, and a second action of unknown meaning, need administrator rights). |
| Start seat (seat action) | Starts a stopped seat's session by hand. |
| Ctrl+Alt+Del | **HYPOTHESIS**. On the test PC, `DisableCAD=1` and the `SoftwareSASGeneration` policy are set; it is not known whether the product set them. A secure attention sequence from an extra seat's keyboard cannot reach that seat's winlogon by the normal path. |
| Sleep / hibernate | After resume, the service signals all seat helpers to reset display modes (§3.3). Keyboard sleep/power buttons on extra seats are blocked unless `DisableSysButton=0` (see 50). |
| Shutdown / restart | 2.70.6 adds a global pre-shutdown event; the service gets ready for shutdown. Automatic Restart Sign-On (ARSO) can be turned off with the shipped tweak `DisableAutomaticRestartSignOn=1` (**HYPOTHESIS**: ARSO confuses the per-seat logon after Windows Update restarts). |
| Fast startup | The driver gives each seat its own view of `HiberbootEnabled` (**HYPOTHESIS**: fast startup is reported as off, so every boot is a full boot with fresh seat sessions). It was 0 on the test PC. |

What seat 1 does to the extra seats (**HYPOTHESIS** for every row: no runtime evidence; the
test PC's Security log has no lock/unlock audit events from the product's period of use):

| Seat 1 state | Expected effect on extra seats |
| --- | --- |
| Seat 1 locks (Win+L) or shows the UAC secure desktop | None. Lock screen and secure desktop belong to seat 1's own session, and each extra seat runs in its own session with its own winlogon desktop. |
| Seat 1 signs out | None while the extra sessions live. A new seat 1 logon later is a new console session. In the "when a user logs on at seat 1" mode, a later logon may start seats that are not yet running. |
| Seat 1 sits at the logon screen at boot | In the "at system startup" mode the extra seats start anyway; no seat 1 logon is needed. The observed timeline (§3.2) only covers the case where seat 1 was already logged on. |

### 3.7 Same account on several seats

- Setting `SameUserMultiLogon` (DWORD under the service key): 0 = allow, 1 = allow but warn,
  2 = deny (**HYPOTHESIS** for the exact 1/2 meaning; the shipped tweaks set 0, 1 and 2). The
  changelog says that deny exists and is off by default.
- The service checks it at logon. It compares the token user SID of the session that is logging
  on with the users of the other seat sessions.
- Warn: the manager shows a dialog in the new seat's session. It tells the user that the
  account is already in use on another seat and advises one account per seat. A
  "do not show again" box stores per-seat flags (`sharedHideoutShown_<n>`) in the user's
  manager settings. The manager reads the per-seat user data for this check from the volatile
  runtime key, through the product's software key path (§3.8).
- Deny: the logon on the second seat is refused (**HYPOTHESIS**: the session is logged off).

### 3.8 Session-to-seat binding and runtime state

- The service holds the seat → session ID mapping at runtime. Consumers:
  - the small public DLL: `SessionIdToTerminalId`, `ProcessIdToTerminalId` and
    `GetMaxTerminalsCount`. The DLL only opens the driver's control device (DeviceIoControl)
    and maps a view of shared memory; it never talks to the service;
  - the manager's status line, which shows which seat is running and its state;
  - per-user values named `User<N>` in the volatile runtime key.
- **Who is authoritative** (**HYPOTHESIS**): the service opens the driver's control device and
  maps a named section that the driver created, and it sends the driver I/O controls. The
  service most likely pushes the seat ↔ session assignment to the driver, and the driver (with
  its section) serves it to the public DLL and to its own per-seat filtering. The driver also
  sees session creation on its own (process-create notifications, per-session kernel objects),
  so it may derive part of the map itself.
- Each seat session has a named "seat context" event in its own `Session\<id>\` namespace. The
  service sets it when that session's seat context is ready (**HYPOTHESIS** for the exact
  meaning).
- Device and display ownership are keyed by seat number, not by session ID (see 40/50). When a
  seat's session changes on re-logon, ownership stays the same.
- Seat 1's identity: the active console session ID is read with `WTSGetActiveConsoleSessionId`.

Runtime objects and IPC (session-related):

| Object | Purpose |
| --- | --- |
| Global event "terminal services ready" | Awaited before any session work. This is an OS object. |
| Global resume event | Set after resume from sleep or hibernation; seat helpers then reset display modes. |
| Global pre-shutdown event (2.70.6) | Shutdown preparation. |
| Per-session context event | See above. |
| Named section shared with the driver | Shared seat/session state between the driver and the service. |
| Per-session kernel objects | The driver refers to `\KernelObjects\Session<N>` objects and a value `lgdMaxCount` (**HYPOTHESIS**: it watches the per-session objects Windows creates, and `lgdMaxCount` bounds a logged-on-session count). |
| `link` sub-key of the service key | A **registry symbolic link** (`SymbolicLinkValue`) that the service creates, pointing at the product's HKLM software root (**HYPOTHESIS**, strong: the link-related strings and the GUI's reads match). So `Svc\link\…` and `Prod\…` are the same keys. |
| `volatile` sub-key (reached as `Svc\link\volatile` by the service, as `Prod\volatile` by the manager) | Runtime state. The service deletes and recreates it, empty, each time it starts. Holds the result values of §3.3, `User<N>` values and audio-progress values (`audProgress`). It is absent while the service is stopped. **HYPOTHESIS**: created as a volatile key, so it also disappears at reboot. |
| LSM RPC (`ncalrpc`, endpoint `LSMApi`) | The service is a client of LSM's local RPC interfaces. It uses them to create sessions and connect them to the console or disconnect them (**HYPOTHESIS**: these are undocumented interfaces). |

### 3.9 Admin-only settings

- `OnlyAdminCanChangeSettings` (stored in the persistent context sub-key of the product key,
  with a copy in the user's manager settings): when set, only members of Administrators may
  change the product's settings; the manager tells other users that the administrator has
  restricted it (see 10).

### 3.10 Settings summary (session-related)

All DWORD unless noted. "Svc" = the seat service's key; "Drv" = the driver's service key;
"Prod" = the product's HKLM software key; "Ctx" = the persistent context sub-key of Prod;
"Vol" = the volatile runtime sub-key of Prod.

| Name | Key | Meaning |
| --- | --- | --- |
| `AutoStartMode` | Prod | Start mode (manual / boot / on seat-1 logon). 1 = at system startup. |
| `startstations` | Ctx | Seats to auto-start (bitmask). |
| `OnlyAdminCanChangeSettings` | Ctx (+ user copy) | Settings changes by administrators only. |
| per-seat `DefaultUserName`, `DefaultDomainName` (REG_SZ) | Prod\NN | Seat account. |
| per-seat `LastLoggedOnUserSID` (REG_SZ) | Prod\NN | Last user at the seat. |
| `svcAutoStarted`, `svcAutoStrtErr`, `svcAutoSession`, `canRunStatus`, `canRunBits`, `auxInitStatus`, `svcExeError` (2.70.6), `User<N>`, `audProgress` | Vol | Runtime status read by the manager (§3.3, §3.8). |
| `AutoStartDelay` | Svc | Extra delay before auto-start. |
| `AutoStartSync` | Svc | Wait for dependent OS services before auto-start (**HYPOTHESIS**). |
| `nextTerminalDelay` | Svc | Pause between seat starts. |
| `TransitionDueTime` | Svc | Max seconds for one seat start / console hand-over (tweaks: 30, 40). |
| `ignoreLastShutdown` | Svc | Start even after an unclean shutdown. |
| `canRunBits` | Svc | Seat mask override (**HYPOTHESIS**). |
| `oldFashionSTSM` | Svc | Use a legacy session-transition scheme (**HYPOTHESIS**: compatibility switch). |
| `SameUserMultiLogon` | Svc | 0 allow / 1 warn / 2 deny. |
| `ovrAutoLogon`, `ovrUpdatesPeriod` | Svc | Overrides for auto-logon and the update-deferral period (**HYPOTHESIS**). |
| `Affinity` | Svc / per seat | CPU mask for the seat's processes. |
| `ForceAutoLogon`, `ForcedLogoffMode` | per session (Winlogon-style) | Auto-logon after sign-out; forced logoff behaviour (**HYPOTHESIS**). |
| `SharedPCMode` | Drv | Seats whose lock screen shows sign-out (bitmask). |
| `ignoreLastBoot` | Drv | Skip the driver's boot-success check. |
| `relogReqAdmin`, `BSDReqAdmin` | Drv | Admin requirement for re-logon and for a second action of unknown meaning (**HYPOTHESIS**). |
| `lgdMaxCount` | Drv | Bound on logged-on sessions (**HYPOTHESIS**). |
| `sessionsecurityenabled` | Drv | Unknown session-security switch (**HYPOTHESIS**). |
| `BlockWUHandlers` (2.70.6) | Drv | Block the Windows Update engine while enabled (**HYPOTHESIS**). |
| `oldFashionBoot` | Drv | Legacy boot scheme. The tweak also disables both services (**HYPOTHESIS**: a recovery switch). |
| `DisableAutomaticRestartSignOn` | `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System` | Optional tweak; standard Windows policy. |

Terminal Services values found on the test PC (not attributed to the reference product; most
likely set by a different RDP-based product installed there): `Control\Terminal Server`
`fSingleSessionPerUser=0`; Winlogon `AllowMultipleTSSessions=1`; Licensing Core
`EnableConcurrentSessions=1`.

### 3.11 Windows-internal mechanisms used by the reference (excluded for Pulsar)

These explain why the reference works on client Windows. Pulsar Seat builds none of them.

- Code injection from the kernel driver into system processes (LSM host, audio service,
  display clients) and in-process interception inside them. Excluded: code injection into
  system processes and runtime patching.
- Changing the kernel's notion of the active console session. Excluded: kernel patching.
- Undocumented LSM RPC interfaces for session creation and console hand-over. Excluded:
  undocumented interfaces.
- Runtime patch of the audio service (see 60). Excluded: runtime patching.
- A registry filter driver that gives each seat its own view of logon-related values
  (Winlogon `AutoAdminLogon`, LogonUI last user and SessionData, `IsFirstLogonAfterSignOut`,
  `SharedPC`, `HideFastUserSwitching`, `HiberbootEnabled`, plus keys covered in 60/70). A
  registry filter itself is a documented, HVCI-compatible mechanism (`CmRegisterCallbackEx`);
  see §6.2.11 for why Pulsar does not need one here.
- A dynamic lookup of the OS routine that reports session-count enforcement (**HYPOTHESIS**:
  to check the client limit).

## 4. Client-SKU licensing concern

- The test PC runs Windows Pro (a client SKU). The reference product gives several people
  their own simultaneous interactive sessions on it.
- Microsoft's licence terms for client Windows generally allow one user at a time to use the
  software on the device. Supported multi-user designs use Windows Server with Remote Desktop
  Services and per-user or per-device RDS CALs. The former MultiPoint Server / MultiPoint role
  existed for exactly this kind of shared local multi-seat; it is discontinued.
- Products that turn on concurrent sessions on client SKUs, whether they patch or wrap
  termsrv or create extra console-type sessions, are generally seen as outside those terms.
  This spec makes no legal judgement. It is a **release blocker to decide** (see
  `docs/agents/plan.md` "Known risks"). The decision is shared with Pulsar Split.
- Technical side: on client SKUs, TermService allows only one interactive session at a time
  through the documented paths (a new RDP logon disconnects the console user). Enforcement is
  internal to termsrv and LSM. There is no documented switch for client SKUs.

## 5. ARM64 notes

- The reference product cannot run on ARM64 at all. Its driver is x64 kernel code, and its
  in-process parts are x64 DLLs. Windows on ARM64 never loads x64 code into native ARM64
  system processes (LSM, AudioSrv, LogonUI); x64 emulation exists only for whole user-mode
  apps. Pulsar Seat must not depend on any of the mechanisms in §3.11, on either architecture.
- LSM, winlogon, LogonUI, TermService, the RDP client stack, WTS APIs, LSA private data and
  credential providers are all native on Windows 11 ARM64.
- **Credential providers** are loaded by LogonUI (native ARM64) and by credential UI that runs
  inside other processes, including x64 apps under emulation (**HYPOTHESIS**: CredUI prompts
  in x64 apps load providers in-process). A provider built only as an ARM64 DLL does not load
  into an emulated x64 process. If Pulsar ships a provider that must also appear there, build
  it as an **ARM64X** binary (ARM64 + ARM64EC code in one DLL, loadable by both native and
  x64-emulated processes) registered once; a plain ARM64EC build alone does not load into
  native LogonUI. If the provider only needs to show in LogonUI, a native ARM64 DLL is enough.
  On x64, ship an x64 DLL.
- Any helper that runs inside a seat session (display reset, tray, notices) must be built
  native for each architecture. Test that on ARM64 it is not started under emulation.
- ARM64 Windows 11 is a client SKU. Windows Server for ARM64 is not generally available to
  buy or install on ordinary hardware (**HYPOTHESIS**: check current availability). So every
  backend that depends on Windows Server RDS (§6.1 option A) does not exist on ARM64 today.
- Any kernel or UMDF component that the session design depends on (for example the input
  filter in 50) must be native ARM64, signed, and HVCI-compatible.
- Boot timing: keep the service's start order configurable. Do not hard-code waits measured on
  x64 hardware.

## 6. Pulsar Seat approach

Constraints from `docs/agents/rules.md`, applied to sessions:

- No kernel patching; no vulnerable drivers; must work with HVCI and Secure Boot on.
- No code injection into system processes, and no user-mode patching or wrapping of system
  services. **Modifying, patching or wrapping termsrv (or LSM) is excluded outright**, on every
  SKU and whatever the licensing decision in §4.
- No undocumented LSM/termsrv interfaces.
- Native ARM64 and x64; no x64-only dependency without the maintainer's OK.

### 6.1 Options for the extra seats' sessions

| Option | How | Fits rules? | Notes |
| --- | --- | --- | --- |
| A. RDP loopback sessions on Windows Server with RDS | Every seat is a TermService session. A Pulsar seat host connects to it over the local RDP stack and shows it full-screen on the seat's monitor(s). | Technically yes (documented TermService, RDP client control, WTS APIs; user-mode only). **But** see the ARM64 conflict below. | Works only where Windows allows several concurrent sessions without changes: Windows Server with the RDS Session Host role and CALs. On client SKUs it would need termsrv modification, which is excluded. |
| B. Extra local console-type sessions, like the reference | Needs injection into LSM, active-console switching and undocumented interfaces (§3.11). | **No** | Excluded. |
| C. Fast user switching only | One active session at a time. | Yes | Not multi-seat. Only a degraded "one seat at a time" fall-back. |
| D. Seats as separate desktops or windows in one session | No separate session per seat. | Yes | No per-user isolation, profiles or logon. Possible later "kiosk / guest seat" mode. Works on client SKUs and ARM64. |

**Rule conflict to resolve (maintainer decision needed):** option A needs Windows Server.
Windows Server is in practice x64-only (§5), so option A is effectively an **x64-only
backend**. That conflicts with "ARM64 is first class" and needs the maintainer's explicit OK.
No documented, rules-compliant backend for several concurrent interactive sessions on client
SKUs is known; every ARM64 PC today runs a client SKU. So on ARM64 (and on any client SKU)
Pulsar Seat currently has only options C and D.

Recommendation:

1. Keep a seat-session abstraction ("create, attach, detach, log off, query state") so that
   the backend can be swapped.
2. Ask the maintainer to choose: (a) accept option A as an x64 / Windows Server-only backend
   for Phase 2, with option D as the client-SKU and ARM64 mode; or (b) make option D the main
   backend everywhere and treat option A as an extra; or (c) hold Phase 2 until a documented
   path exists.
3. Never ship anything that modifies termsrv or LSM (see constraints).

### 6.1.1 Option A: implementation gaps to settle before building it

1. **Where the seat hosts run.** The per-seat RDP clients need an interactive desktop on the
   physical display. Session 0 has none. If they ran in seat 1's user session, a lock (Win+L),
   a UAC secure-desktop prompt, a sign-out or seat 1 sitting at the logon screen would hide or
   end every extra seat, and the "at system startup" mode could not work before seat 1 logs
   on. Viable shape (**HYPOTHESIS**, to prototype): a dedicated **host console session** that
   runs only the seat hosts (a locked-down shell under a dedicated local account with
   automatic logon, lock and screen saver disabled for that account), and **every** seat,
   seat 1 included, is an RDP session. Then lock, UAC prompts and sign-out happen inside each
   seat's own RDP session and do not touch the others. This changes §6.2 point 1 (seat 1 is no
   longer the physical console) and costs seat 1 the RDP performance penalty.
2. **Loopback refusal.** The RDP client refuses a connection to the local machine's own
   console address in some configurations (**HYPOTHESIS**: connecting to 127.0.0.1 is refused
   or redirected to the console; another loopback address such as 127.0.0.2 behaves
   differently). Test on each supported Server version.
3. **Credentials.** Auto-logon passes the stored password to the RDP client control
   programmatically. With Network Level Authentication on, CredSSP delegation policy applies
   (the "allow delegating fresh / saved credentials" policies for `TERMSRV/<host>`; NTLM-only
   variants on a workgroup PC) (**HYPOTHESIS**: which policy is needed for a local-only
   connection). Alternative: an RDP listener bound to loopback only with NLA off; weigh the
   security cost.
4. **RDS licensing.** RDS Session Host needs an RDS licensing server and CALs after the grace
   period (120 days, **HYPOTHESIS** for current Server versions). The installer and the docs
   must say so.
5. **Per-seat devices through RDP.** Audio: the RDP client plays the session's audio on the
   host session's default endpoint, not on the seat's endpoint, unless the seat host can pick
   the endpoint per client (open question, see 60). USB, storage and smart cards: documented
   RDP redirection (drive, smart card, RemoteFX USB) per connection, which must match the
   seat's devices (see 70).
6. **Input.** All seat hosts share one desktop in the host session, and only one window has
   keyboard focus. Per-seat input therefore needs device-level routing (50): read each seat's
   devices (for example with Raw Input in background mode, user-mode) and feed them to that
   seat's RDP client; stopping those devices from also reaching the focused window needs a
   filter. Under rules.md such a filter must be a signed, HVCI-compatible, ARM64-native KMDF
   or UMDF filter (documented filter positions only). This is in line with the rules, but it
   is a driver-signing cost (plan.md "Known risks").
7. **GPU and full-screen.** GPU acceleration inside RDP sessions uses the documented RDP GPU
   policy; game and full-screen-exclusive performance is limited (see 40/80).

### 6.2 Required behaviour (backend-independent)

1. **Seat 1** is the physical console session with the standard Windows logon, unless the
   backend needs a host console session (§6.1.1 point 1). Pulsar Seat never changes the
   Winlogon console auto-logon values for user accounts. If the owner wants seat 1 auto-logon,
   it uses the documented Windows setting. (A dedicated host account for option A would need
   the documented auto-logon for that account; this is a maintainer decision.)
2. **Extra seats** start in seat order. Settings: per-seat "start automatically" bit; a pause
   between seats (default about 2 s); a time-out per seat start (default 60 s, configurable
   30–120). An error in one seat does not block the others.
3. **Start modes**, the same three as the reference: manual; at boot (after the service and
   its dependencies are ready); after the first interactive logon on seat 1. Also a tray or
   manager "start seat" action and a "start all" action.
4. **Readiness wait** before auto-start: LSM (and TermService for option A), audio services
   and PlugPlay running; the seat's monitor present (40). A seat without a display waits in
   the state "waiting for display" and does not count as failed.
5. **Unclean-shutdown guard**: after a crash or power loss (standard `LastBootShutdown`), do
   not auto-start the extra seats. Show a clear notice on seat 1 and let the owner start them
   by hand. A setting turns the guard off.
6. **Per-seat account**: either "show logon screen" or account plus domain plus optional
   password. Store the password as LSA private data or with DPAPI under the service's own
   SYSTEM identity. Never put it in plain registry, files, logs or IPC replies. The manager
   can set or clear it but never read it back.
7. **Per-seat auto-logon** with option A: the seat host gives the stored credentials to the
   RDP connection (§6.1.1 point 3). No credential provider and no change to Winlogon are
   needed. If a credential provider is ever needed (for example "remember last user per seat"
   on the logon tile), it must be a documented ICredentialProvider. It shows only in Pulsar
   seat sessions, is native per architecture (ARM64X where §5 requires it), and is registered
   only while Pulsar Seat is enabled.
8. **Last user per seat**: store the last user SID per seat, and pre-fill it in the seat's
   logon (option A: the RDP client's user-name hint).
9. **Lock / unlock**: Windows' own lock screen in the seat's session; nothing custom. A lock,
   UAC prompt or sign-out on one seat never hides another seat (§6.1.1 point 1).
10. **Sign out**: if the seat has auto-logon, log on again after a short delay (default 3 s).
    Otherwise show the seat's logon prompt. Detect sign-out through session-change
    notifications (WTS session-change events; logoff, disconnect).
11. **Switch user / disconnect on an extra seat**: the seat host notices the disconnect and
    reconnects at once to the seat's own session, or to the logon prompt. A seat's monitor
    never shows another seat's session. Optionally set the documented policy that hides fast
    user switching (machine-wide). Per-seat values of that policy would need a registry filter
    driver; that is allowed by rules.md (documented `CmRegisterCallbackEx`, HVCI-compatible),
    but Pulsar does not build one for this: under option A each seat is its own RDP session,
    so switch user only disconnects that session and the seat host recovers. A filter would add
    a signed kernel driver (cost, risk) for little gain.
12. **Force re-logon**: log off the seat's session (WTS logoff), then start the seat again.
13. **Same account on several seats**: setting allow / warn / deny, default warn.
    - Warn shows a notice in the new seat's session (Turkish UI copy).
    - Deny logs the new session off with a notice.
    - Option A note: the "restrict each user to a single session" setting
      (`fSingleSessionPerUser`, or its documented group policy) must be off, or a second seat
      with the same account would take over the first seat's session. The setting is
      **machine-wide**; there is no per-session scope. Pulsar sets it to 0 for the whole PC
      when option A is in use, tells the owner in the manager, and documents it.
14. **Session-to-seat map**: the service is the only authority for the table seat → session
    ID → user SID → state. It is built from WTS enumeration plus the seat host's own
    connection (option A: a per-seat client name such as `PULSAR-SEAT-NN`, readable through
    the WTS client name). Any driver that needs the map receives it from the service.
    - It is exposed on the service named pipe (ACL: SYSTEM and Administrators full; each
      interactive user sees only their own seat).
    - It survives a service restart: rebuild it from WTS enumeration and client names.
15. **Public seat API** (optional, later): a small native DLL (x64 + ARM64/ARM64X + x86) or a
    pipe call that maps a process or session to a seat number, for apps that need it.
16. **Power**: on resume, re-check each seat's session and reconnect its seat host. On
    shutdown, Windows logs the sessions off; Pulsar does not delay shutdown beyond what the
    service control manager allows.
17. **ARSO and fast startup**: leave the OS settings alone by default. The manager may offer
    the documented policy to turn off Automatic Restart Sign-On, with an explanation. Fast
    startup needs no change for option A, because RDP sessions are not restored from a hybrid
    boot.
18. **Seat states** shown in the manager: off; waiting for display; starting; at logon screen;
    signed in (user); locked; error (with reason). This replaces the reference's colour code,
    which judged completeness from devices.
19. **Admin-only settings**: an option restricts settings changes to Administrators (the
    service enforces it on the pipe, not only the manager).

### 6.3 Errors to handle

| Case | Behaviour |
| --- | --- |
| Session creation refused (no RDS host role, licensing, policy, client SKU) | The seat goes to the error state with a clear reason. The other seats still try. |
| Logon failure (wrong or expired password, account disabled, logon hours) | Show the logon prompt on the seat. Mark the stored password as failing. Do not retry in a loop: lockout risk. |
| Time-out of a seat start | Error state; the manager offers a retry. |
| User signs out during the start | Treat it as a normal sign-out. |
| Service restart while seats run | Seats keep running. The service rebuilds the map (§6.2.14). |
| Windows build change (feature update) | Re-check the prerequisites before auto-start. Do not disable or block Windows Update. |
| Safe Mode boot | Do not start extra seats. |
| Unclean previous shutdown | §6.2.5. |

## 7. Open questions

1. **Licensing and backend decision (§4, §6.1)**: are concurrent sessions on client SKUs in
   scope at all? Is an x64 / Windows Server-only backend (option A) acceptable, given the
   ARM64-first rule? What is the ARM64 answer (option D only?). Shared with Pulsar Split.
2. Is option A's display and GPU performance good enough for the target use, including games?
   This needs a prototype; see 40.
3. Can one RDP loopback client show a session on 2+ monitors of one seat, on both
   architectures, with multimon and the right resolution and refresh rate?
4. Does the host-console-session shape (§6.1.1 point 1) hold up: lock-free host account,
   recovery when the host session itself fails, and behaviour at boot?
5. Which CredSSP / NLA settings does a local-only RDP connection need (§6.1.1 point 3)?
6. Can a seat host send an RDP session's audio to a specific endpoint (see 60)?
7. What exactly do the reference's `ForceAutoLogon` / `ForcedLogoffMode` / `ovrAutoLogon`
   values do? The answer does not block Pulsar's design.
8. Did the reference set `DisableCAD` and `SoftwareSASGeneration` on the test PC, or did the
   owner? A runtime check would need the product enabled, which the maintainer must approve.
9. Exact meaning of `SameUserMultiLogon` values 1 and 2. Needs a runtime check with the
   maintainer's approval.
10. Does the reference show seat 1 a short blank screen at each extra-seat start (§3.2)?
11. How does the reference behave when seat 1 locks, shows a UAC prompt, signs out or stays at
    the logon screen while extra seats run (§3.6)? Needs a runtime check with approval.
12. Meaning of the driver values `relogReqAdmin`, `BSDReqAdmin`, `lgdMaxCount` and
    `sessionsecurityenabled`.
13. Does 2.70.6 really support 16 seats (§2)? Only the driver's audio-state names suggest it.
14. Should Pulsar Seat offer the guest / kiosk seat mode (option D) as the client-SKU and ARM64
    mode?
