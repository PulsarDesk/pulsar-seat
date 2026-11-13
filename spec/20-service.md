# 20 - Seat service

Status: draft v1 (2026-11-12, critiqued, cross-checked). Analyst-written. Sources: the reference product
2.70.3 as installed on the x64 test PC (service stopped and disabled; read-only probes only),
the 2.70.6 package (static only), the product's changelog, its translation catalogue and its
diagnostic report and log files. Everything here is written fresh. Items marked
**HYPOTHESIS** were not verified at run time. The service was never running during the
analysis, so all run-time behaviour is inferred from the binaries' imports, referenced
identifiers and call structure, from the configuration the product left behind and from the
manager's messages.

Related specs: `00-architecture.md` (component map), `10-manager.md` (screens that drive the
service), `30-sessions.md` (how a seat session is created and logged on), `40-display.md`,
`50-input.md`, `60-audio.md`, `70-usb-devices.md`, `80-apps-compat.md` (tweak values),
`90-install.md` (installer, driver packages, signing, install-time side effects).

## 1. Scope

The seat service is the always-on, machine-wide part of the reference product. This file
covers:

- the service's binaries, registration, start mode, account and SCM control interface;
- its command-line maintenance verbs (install, disable, reset, recovery);
- the configuration model: registry keys and values, ProgramData files, run-time state;
- the boot sequence from power-on to "all seats started";
- the seat life cycle (start, logon, relogon, stop) as the service sees it;
- sleep, resume, fast startup and shutdown;
- safety and watchdog behaviour (unclean shutdown, OS build change, time-outs);
- every inter-process channel between manager, service, driver and the per-session parts;
- logs, the diagnostic report and error reporting;
- the purpose of the relay launcher batch file.

Not covered here: how a session is technically created and bound to a seat (`30-sessions.md`),
display/input/audio/USB routing details (40-70). A licence component exists; it is out of
scope.

## 2. Terms

- **Seat**: one workplace (monitors, keyboard, mouse, audio). Seats are numbered 1..N in the
  UI and `00`..`NN-1` in registry subkeys. N = 12 in 2.70.3; 16 in 2.70.6 is a **HYPOTHESIS**
  (`30-sessions.md` §2). Seat 1 is the
  ordinary console session.
- **Seat mask**: a bitmask with one bit per seat (bit 0 = seat 1). Shown in reports as a binary
  string, seat 1 rightmost (for example `000000000011` = seats 1 and 2).
- **Service key**: the seat service's key under `HKLM\SYSTEM\CurrentControlSet\Services`.
  Its literal name is an interop identifier listed only in `90-install.md` §11 (coexistence
  detection).
- **Driver key**: the kernel driver's key under the same path (name: `90-install.md` §11 only).
- **Config root**: `HKLM\SOFTWARE\<vendor>\<product>`; the product's machine settings. An older
  rebrand of the same product left a second, parallel config root with the same layout.
- **Enabled / disabled**: the product-wide switch. Enabled means both the driver and the seat
  service are set to start at boot and the class filters are in place (filter part
  **HYPOTHESIS**, never observed). Changing it always needs
  a reboot.
- **Started**: at run time, the extra seats' sessions exist. The product can be enabled but not
  started (manual start mode, or a start failure).

## 3. Components and processes

| Part | What it is | Runs where |
| --- | --- | --- |
| Service host (small exe, about 40 KB) | Thin Win32 service executable. Also a command-line tool with maintenance verbs (section 4.3). Checks code signatures (Authenticode trust check) and loads the logic library. Has a minimal service handler of its own, used when the library cannot be loaded (**HYPOTHESIS**: reports a stopped state with an error and exits). | Own process, LocalSystem, session 0. |
| Service logic library (DLL, about 100 KB) | Contains the service main routine, the control handler, session orchestration, auto-logon, display/input helpers, Windows Update deferral, and also a COM credential provider. | Inside the service process; also loaded by the logon UI as a credential provider; **HYPOTHESIS (strong)**: also mapped into some Windows system service processes (section 3.1). |
| Per-session helper | The library's entry decides by the calling process's session id: in session 0 it runs as the service, in any other session it runs as a helper. The service starts the host in a seat session with a copy of a token that has the seat's session id set, with a start argument. The helper code opens the interactive window station and input desktop and resets display modes. | Each seat session. **HYPOTHESIS** for the full role. |
| Credential provider | COM in-process server registered under the Windows credential-provider list with apartment threading. Absent while the product is disabled (verified live); **HYPOTHESIS**: registered on enable. | LogonUI of each seat session. |
| Manager (GUI exe) | GUI that edits the configuration, enables/disables the product, starts seats manually, runs maintenance verbs and shows status. Starts at logon of every user from the machine Run key with an `-autostart` argument and stays in the tray. | Every interactive session. |
| Control library (DLL) | Used by the manager and the setup wizard. Writes the service and driver configuration, notifies the running service, talks to the driver, builds the diagnostic report. | Manager process. |
| Kernel driver | Device filters, registry views, file-system minifilter, per-session work (described in 40-70 and `00-architecture.md`). The service and the manager talk to it through a control device. | Kernel. |
| Licence component | Exists; started by the service. Out of scope. | - |

### 3.1 In-process work inside Windows services (do not copy)

**HYPOTHESIS (strong)**, from the library's imports and referenced module names and the
driver's ability to map sections into user processes: the library is injected into the Local
Session Manager, audio service and display broker processes and intercepts their session and
RPC paths, so that they accept several local console-like sessions and keep per-seat audio
state. A shared memory section connects these copies with the service process.

This is code injection into protected Windows services. It is forbidden by Pulsar's rules and
does not survive modern protections. It is noted only so that the implementer knows which
behaviours have to be reached another way.

## 4. Registration, start mode, account and control interface

### 4.1 Service configuration (verified live on 2.70.3 unless marked)

| Item | Value |
| --- | --- |
| Name / display name | vendor-named key (`90-install.md` §11); display name equals the key name; no description |
| Type | own process (0x10) |
| Start | 4 (disabled) when the product is disabled (live). **HYPOTHESIS**: 2 (automatic) while enabled. |
| Error control | 1 (normal) |
| Image path | quoted full path of the service host in the install folder, no arguments |
| Load order group | `COM Infrastructure` (an early group in the service group order) |
| Dependencies | `RPCSS`, `LSM` |
| Account | LocalSystem |
| Extra value | `KeyImage` = the manager's executable file name. **HYPOTHESIS (strong)**, from the call structure: the service loads that file as a resource-only module and reads from its resources the brand-specific name of the config root, which it then uses to build the registry link (section 5.4). |
| Failure actions | none (reset period 0, no restart actions) |
| Triggers | none |
| Service SID type | none; no required-privileges list |
| Security descriptor | the Windows default for services: SYSTEM and Administrators full control; interactive users and service logons may query status and config, interrogate and send user-defined control codes, but not start or stop |

Last exit code seen live: "service has not been started since boot". No event-log source is
registered for the product and neither the System nor the Application log contains any entry
that mentions it.

The control library edits the `EarlyStartServices` list under
`HKLM\SYSTEM\CurrentControlSet\Control` (it reads the list, compares entries with the service
name and writes it back). On the test PC (product disabled) the list is stock.
**HYPOTHESIS**: the service is added to that list while enabled and removed on disable.

### 4.2 Who registers it

- The installer copies files, then runs a post-install step (a service-host action, elevated)
  that creates the driver service, the driver's root device node, the seat service, the class
  filter entries and the credential provider registration (**HYPOTHESIS** that one verb does
  all of these; the action's code is in the service host, which also holds every key path these
  steps touch).
- The control library in the manager can also create and change both services itself (create
  service, change config, control service) and edit the class filter lists. The manager uses
  this path for "enable" and "disable".
- The 2.70.6 installer refuses to install while the driver service is running or its start type
  is 2 or lower (boot, system or automatic): the user must disable the product first. It also
  has stop, delete, install, start and resume steps for the services and ends with a reboot
  prompt.
- On uninstall the installer runs the service host elevated with its disable action.
- Install-time side effects next to registration (details in `90-install.md`): an inbound and
  an outbound firewall allow rule for the manager executable (verified live); a Defender
  exclusion for the install folder (installer script, **HYPOTHESIS** for the exact paths); the
  machine Run entry for the manager (verified live); an `Instances` subkey under the driver key
  with a default instance and an altitude (verified live), which registers the driver also as
  a file-system minifilter (`70-usb-devices.md`).

### 4.3 Maintenance verbs of the service host

The service host checks its command line for one action. Actions are listed by function, not
by their literal names (no interop need; same list as `90-install.md` §4). Every verb that changes the system ends
with a message box that says a reboot is needed and offers to reboot now (it can enable the
shutdown privilege and restart Windows). The message text is localised from the host's own
resources using the language id the manager stored.

Only the disable action is verified (by the live state after a disable). The other effects are
inferred from which keys, files and APIs each verb's code path references.

| Verb | Effect |
| --- | --- |
| post-install | **HYPOTHESIS**: register the driver service (kernel driver, image under `System32\drivers`), create the root device node for the driver's control device (a root-enumerated device), add the driver to the upper-filter lists of the Display, HIDClass, Keyboard and Mouse device classes, register the credential provider, set the device-installation deny policy for display adapters (possibly only while enabled), set start types. |
| disable | Set both services to disabled, remove the class-filter entries and unregister the credential provider. Live state after disable matches this. |
| delete | **HYPOTHESIS**: remove the services and the root device node (uninstall). |
| reset | **HYPOTHESIS**: reset all product settings by removing the per-device seat markers (seat number, seat bits, node bits under the device class keys), the per-display markers under `Control\Video\{adapter}\NNNN`, the per-seat audio endpoint state values under the MMDevices Render and Capture keys, values under the smart-card reader list (`Calais\Readers`), the per-seat subkeys, the per-seat IP app list and mode, and the auto-start mode, and by recreating a scratch key. It also scans the install folder. The manager runs this action as a child process and reports the exit code if it fails. Whether it also removes entries for devices that are no longer present is not established. |
| arm-recovery-driver | **HYPOTHESIS**: recovery. Create and start a temporary service for the small recovery driver (2.70.6 ships it) whose only job is to clear the product driver's configuration values at boot, then delete that service and file. A related path reads the `Phantom` flag of the product's root device node (whether Windows sees it as non-present) to decide whether recovery is needed. When support would tell a user to run it is unknown. |
| test-signing-off | Legacy: turn Windows test-signing mode off with the boot configuration tool and remove the product's test certificate from the trusted-publisher store. Not used with the Microsoft-signed driver build. |
| per-session start | Used by the service itself when it starts a copy of the host in a seat session (section 3). |

### 4.4 SCM control interface

From the library's service main and control handler (static analysis; **HYPOTHESIS** for
behaviour at run time, nothing was sent to a live service):

- While starting, the service reports start-pending with a wait hint of 30 seconds and accepts
  no controls. A start worker then checks Windows safe mode, opens the driver's control device
  and queries one value from it; if the driver is missing or answers with an unexpected value
  the service reports stopped with a non-zero exit code.
- While running, 2.70.3 accepts **only power events**. 2.70.6 accepts power events and
  **pre-shutdown**. Neither version advertises stop, shutdown, session-change, parameter-change
  or hardware-profile controls, so an administrator cannot stop the service through the SCM
  (**HYPOTHESIS**: `sc stop` fails with "the requested control is not valid for this service").
  The handler has an internal stop path (stop-pending, cleanup, stopped) that runs on stop
  requests and on pre-shutdown.
- Power event: on the automatic-resume power event (`PBT_APMRESUMEAUTOMATIC`) the service signals a global resume event (section 7.5).
- Pre-shutdown (2.70.6): the service signals a global pre-shutdown event, then runs its stop
  path before Windows continues the shutdown.
- User-defined control code `200`: the manager's control library sends it to the running
  service after it changes the configuration (it waits while the service is in a pending state,
  then sends the code if the service is running). The service then runs an internal routine.
  **HYPOTHESIS**: re-read and apply the configuration without a restart (the changelog for
  2.60.5 mentions settings that apply without restarting the GUI). The default security
  descriptor lets interactive users send user-defined codes, so any user could trigger it.
- What happens to live seat sessions when the service process stops or crashes is not
  established. The service has no SCM recovery actions, so after a crash it stays stopped until
  the next boot.

## 5. Configuration model

### 5.1 Overview

| Store | Owner | Holds |
| --- | --- | --- |
| Config root `HKLM\SOFTWARE\<vendor>\<product>` | manager writes, service reads | product-wide settings, one subkey per seat, a context subkey, a run-time subkey |
| Service key | manager writes tunables, service reads | session-orchestration tunables |
| Driver key | manager writes, driver reads | driver tunables (owned by 40-70 and `80-apps-compat.md`) |
| Device properties and `Control\Video` values | manager writes through the control library | per-device and per-display-source seat ownership (`70-usb-devices.md`, `40-display.md`) |
| `link` subkey of the service key (volatile) | service creates at run time | a registry symbolic link to the config root (section 5.4) |
| Winlogon key | service reads and adjusts | auto-logon values `AutoAdminLogon`, `DefaultUserName`, `DefaultDomainName`, `DefaultPassword`, `ForceAutoLogon`, `ForcedLogoffMode` (details `30-sessions.md`) |
| LSA private data and protected credentials | control library stores, service reads | per-seat auto-logon passwords, stored with LSA private data; the service also uses the Windows credential-protection functions before handing data to the credential provider (`30-sessions.md`) |
| `C:\ProgramData\<product> Control\` | manager | device assignment export, diagnostic report, logs, icon overrides |
| `HKCU\SOFTWARE\<vendor>\<manager>\v2.0.0` | manager | per-user UI settings (`10-manager.md`) |

### 5.2 Config root values

"Live" means seen on the test PC (product disabled).

| Value / subkey | Type | Status | Meaning |
| --- | --- | --- | --- |
| `AutoStartMode` | DWORD | live = 1 | How seats start: 1 = when Windows starts (the report names that variant). **HYPOTHESIS**: 0 = manual only, 2 = when a user logs on at seat 1. |
| `BuildString` | string | live | installed version |
| `InstallPath` | string | live | install folder (the service and driver find the other files from it) |
| help link | string | live | URL of the online manual |
| `NN\` (two-digit seat index) | key | live | per-seat account and options. Live: `DefaultUserName`, `DefaultDomainName`, `LastLoggedOnUserSID` (last user of this seat, shown at its logon screen). The password is not stored here. Subkeys can exist and be empty (live: seat `00` exists with no values). |
| `NN\SpecificIP` | DWORD (IPv4) | **HYPOTHESIS** for the current root | per-seat source IP (`80-apps-compat.md`). Live it appears only under the old parallel root's seat subkeys, not under the current root. |
| `NN\Affinity` | DWORD mask | **HYPOTHESIS** | CPU cores a seat may use (`80-apps-compat.md`). Not seen live. The location under the seat subkey is a guess; the service and driver also read a value of that name, and the service reads its tunables from the service key. The report prints a mask per seat. |
| context subkey: `startstations` | DWORD seat mask | live = 7 | seats that start automatically (seats 1-3; the report shows the "auto start" map) |
| context subkey: `OnlyAdminCanChangeSettings` | binary | live | restricts settings changes to administrators (`10-manager.md`) |
| context subkey, licence subkey | - | live | a licence component exists; out of scope |
| `volatile\` | key, run time | **HYPOTHESIS**; absent live | per-seat signed-in account information that the manager reads to warn about the same account on several seats. The manager logs that it cannot open it while the service is not running. |
| `SpecificIPApps`, `SpecificIPMode` | key / DWORD | **HYPOTHESIS**; absent live | apps bound to a seat IP and the mode (`80-apps-compat.md`); the reset verb deletes them |

### 5.3 Service key tunables

All are DWORDs read by the service unless noted; absent means built-in default (defaults
unknown). None of them was present live except the standard service values.

| Value | Meaning |
| --- | --- |
| `AutoStartDelay` | delay before the automatic start of seats after the service is ready (**HYPOTHESIS**: seconds) |
| `AutoStartSync` | whether automatic start waits for a synchronisation point before starting seats (**HYPOTHESIS**) |
| `nextTerminalDelay` | pause between starting one seat and the next |
| `TransitionDueTime` | time limit for one seat session state change (start, logon, relogon), run on a waitable timer; tweak files set 30 or 40 (seconds) for slow PCs |
| `ignoreLastShutdown` | 1 = do not enter safe mode after an unclean shutdown |
| `oldFashionSTSM` | 1 = use the legacy session state machine (`30-sessions.md`) |
| `SameUserMultiLogon` | policy for one Windows account on several seats: allow / warn / deny (`30-sessions.md`) |
| `ovrAutoLogon` | **HYPOTHESIS**: override of the Winlogon auto-logon handling |
| `ovrUpdatesPeriod` | override of the Windows Update deferral period the service writes (section 7.4) |
| `canRunBits` | **HYPOTHESIS**: seat mask of seats allowed to run (result of checks), also reported |
| `Affinity` | **HYPOTHESIS**: read by the service next to the tunables; may be a default or per-seat CPU mask (see 5.2) |
| `Start` | standard service start type; the manager and report read it as "service enabled" |

Run-time status values the service publishes (names seen in the logic library; location the
volatile run-time key, **HYPOTHESIS**): `canRunStatus`, `auxInitStatus`, `svcAutoStarted`,
`svcAutoStrtErr`, `svcAutoSession`, `audProgress`, per-seat user entries, and in 2.70.6
`svcExeError`. The manager's report shows a service-library status, the effective start types
of service and driver and the effective check level.

### 5.4 Registry link

At start the service deletes and recreates a volatile `volatile` subkey and creates the
volatile subkey `link` under the service key as a registry symbolic link that points to the
config root. The kernel driver opens the config through this link. **HYPOTHESIS (strong)**:
this lets the driver find the configuration without knowing the brand-specific config root
path (the service gets that name from the manager executable, see `KeyImage` in 4.1). Both keys
are missing while the service is not running (verified live).

### 5.5 ProgramData

`C:\ProgramData\<product> Control\` (live; the folder ACL gives Everyone full control and is
protected from inheritance):

- `default_configuration.json`: device-to-seat assignment export (format in `70-usb-devices.md`
  and `10-manager.md`);
- `ConfigurationDump.asudf`: binary (protocol-buffer) dump of the full configuration;
- `preEnableReport.txt`: diagnostic report written before enabling (section 9.2);
- icon override JSON files (manager only);
- `logs\` (section 9.1);
- a licence data file (out of scope).

A second ProgramData folder belongs to the setup wizard (`10-manager.md`).

### 5.6 Other values the binaries reference

Listed so that nothing is silently dropped. All meanings are **HYPOTHESIS** unless noted.

| Value / key | Referenced by | Meaning |
| --- | --- | --- |
| `Control\CI\Policy` value `BootUpgradedSystem` | service host (next to the driver start type and the install/delete verbs), control library (report) | a Windows code-integrity value that marks a system upgraded in place; likely used to choose the driver start type or to warn |
| Two scratch keys under `HKLM\SYSTEM` named after the driver | service host (reset path recreates one) | temporary or backup data for reset/recovery; both absent live |
| `enableUIStatus` (driver key) | control library | flag that lets the manager show driver status |
| `lastEnabled` (driver key) | control library, report | time of the last enable (live: a timestamp) |
| `vmwHpcMask` (driver key) | service host recovery path, driver | unknown; possibly a mask for a virtual-machine host device class |
| `Phantom` (root device node) | service host | Windows flag for a non-present device; used to detect that the product's control device node is gone |
| a mutex | service main | serialises the start, stop and apply workers (**HYPOTHESIS** that it is not a cross-process single-instance guard) |
| waitable timers | session state machine | implement `TransitionDueTime` |

## 6. Boot sequence

Order reconstructed from dependencies, waits, values and call structure (**HYPOTHESIS** for the
exact order inside the service):

1. **Kernel driver loads** early in boot (driver key; boot or system start while enabled,
   **HYPOTHESIS**; 4 when disabled, live). It attaches as a class upper filter. The driver
   references its own boot-success flag (`LastBootSucceeded`, `ignoreLastBoot`), the recorded
   OS build (`OSBuild`) and the check level (`chkLevel`); the manager's control library
   references the same values. The service library does not. **HYPOTHESIS**: the driver
   decides at boot whether to activate, and the manager reads the result and explains it.
2. **Seat service starts** in the `COM Infrastructure` group after RPCSS and LSM. The host
   verifies signatures and loads the logic library. The service reports start-pending.
3. **Service-side checks** (section 8): Windows safe mode (checked by the service), the driver
   control device answering as expected (else the service stops with an error). A licence
   component exists and is started during service start-up.
4. **Creates the registry link**, the run-time key, the shared section and its global events
   (section 10). Reports running.
5. **Waits for Windows readiness**: the session-manager "terminal services ready" global event,
   and the services Broker Infrastructure, Plug and Play, RPC Endpoint Mapper, RPC, Windows
   Audio and Windows Audio Endpoint Builder (status polling). The changelog mentions fixing a
   delay of the audio service, which fits this wait.
6. **Writes Windows Update deferral values** (section 7.4) if not overridden.
7. **Automatic seat start decision**: reads `AutoStartMode` and the previous shutdown state
   (`Control\LastBootShutdown`, unless `ignoreLastShutdown`); an unclean previous shutdown
   means no automatic start (product safe mode). Otherwise:
   - at Windows start: after `AutoStartDelay`, start each seat in `startstations` (except seat
     1, which is the console) one after another, with `nextTerminalDelay` between seats. The
     manager warns that this mode needs all seat monitors powered on at boot.
   - when someone logs on at seat 1: wait for the first interactive logon on the console
     session, then start the other seats the same way (the "delayed" variant).
   - manual: start nothing; the manager offers a start button.
8. Each started seat gets its own session, its display sources, input devices and audio
   endpoints, its logon screen and optional auto-logon (`30-sessions.md`).
9. The manager starts in every session from the Run key and shows the state.

## 7. Seat life cycle

### 7.1 States

- Product (from the manager's status texts and the report's state line): disabled /
  enabled-not-started / enabled-and-started / safe mode / error.
- Per seat: the report prints a "started" seat mask and an "auto start" seat mask, and the
  manager's per-seat actions and error messages imply the states not configured, at logon
  screen, logged on (with user), relogon in progress and failed. **HYPOTHESIS**: there is no
  documented per-seat state enumeration; these states are inferred from UI texts.

### 7.2 Operations

| Operation | Trigger | Service behaviour |
| --- | --- | --- |
| Start all seats | boot (auto mode), seat-1 logon (delayed mode), manager button | start each seat of the auto-start mask in order with a pause between them |
| Start one seat | manager per-seat button | start that seat's session only; error if the product is not enabled |
| Force relogon | manager per-seat button | end the seat's current logon and show its logon screen again (or auto-logon again); the manager reports an error on failure |
| Stop one seat | - | the reference has no such operation in its UI or verb list (not found); Pulsar adds it |
| Logon on a seat | user or auto-logon | credential provider in that seat's LogonUI either shows normal tiles or submits the seat's stored account (`30-sessions.md`); the service records the last user SID for the seat |
| Same account on two seats | logon | per `SameUserMultiLogon`: allow, show a warning in the second session, or refuse |
| Logoff on a seat | user | the seat returns to its logon screen; the seat session is kept (**HYPOTHESIS**) |
| Apply changed settings | manager after a change | user-defined control code 200 to the running service (section 4.4) |
| Enable / disable product | manager | change service and driver start types and filters, ask for reboot |
| Shutdown / restart | Windows | 2.70.6: pre-shutdown handling (section 7.5) |
| Monitor power | idle-seat screensaver or power actions | the service can switch a seat's monitors off/on through DDC/CI (`40-display.md`) |

Each state change has a time limit (`TransitionDueTime`). On time-out the seat is marked failed
and the next seat continues (**HYPOTHESIS**).

### 7.3 Per-session work done by the service

From imports: taking ownership of a display source for a session (video present network
source owner), resetting display modes (`CDSReset` flag, change display settings), injecting
input into the input desktop, raw-input listening for system buttons and hotkeys, opening the
interactive window station. Details belong to 40 and 50.

### 7.4 Windows Update deferral

The service writes the Windows Update user-experience settings key: defer feature updates,
defer quality updates (days), defer upgrade, branch readiness level and "exclude drivers from
quality updates". Live values on the test PC: feature 30 days, quality 15 days, defer upgrade
on, exclude drivers on (**HYPOTHESIS**: written by the product). `ovrUpdatesPeriod` overrides
the period. The 2.70.6 driver also blocks the Windows Update engine module while the product is
enabled (`80-apps-compat.md`).

### 7.5 Sleep, resume, fast startup and shutdown

- **Resume**: the service registers for power events; on the automatic-resume power event (`PBT_APMRESUMEAUTOMATIC`) it signals a
  global resume event. A worker waiting on that event switches to the input desktop and redoes
  display modes (static analysis). **HYPOTHESIS**: seat sessions survive sleep as they are and
  only their display modes are restored; seats are not restarted.
- **Hibernate**: no separate handling found beyond the same resume path (**HYPOTHESIS**).
- **Fast startup**: the driver's registry view covers the Windows fast-startup switch
  (`HiberbootEnabled` under `Session Manager\Power`). **HYPOTHESIS**: the product hides or
  forces off fast startup while enabled, so that every boot is a full boot and the boot
  sequence of section 6 runs. Live, fast startup is off on the test PC (origin unknown).
- **Shutdown**: 2.70.3 does not accept shutdown or pre-shutdown controls. 2.70.6 accepts
  pre-shutdown, signals a global pre-shutdown event and runs its stop path. **HYPOTHESIS**:
  the event tells the seat-side parts that Windows is shutting down so that they end cleanly.

## 8. Safety and watchdog

There is no service-level watchdog in the usual sense: the service has no SCM recovery
actions and no restart triggers. Safety comes from start-time checks and boot flags.

| Mechanism | Who checks | Behaviour |
| --- | --- | --- |
| Unclean previous shutdown | service (reads `Control\LastBootShutdown`) | If Windows recorded that the last shutdown was not clean (crash, power loss, forced off), the product starts in its own safe mode: extra seats are not started and the manager tells the user to fix the cause and reboot. `ignoreLastShutdown`=1 disables this. |
| Driver boot flag | **HYPOTHESIS**: driver, reported by the manager | The driver keeps its own "last boot succeeded" state with an ignore switch (`ignoreLastBoot`). The report prints both Windows' and the driver's flags. |
| Windows safe mode | service (safe-boot system metric) and driver | The product does not start in Windows safe mode and tells the user. |
| OS build change | **HYPOTHESIS**: driver decides, manager explains | The driver key stores the OS build seen at enable time (`OSBuild`). If a Windows update changed the build while enabled, the product does not start and the manager asks the user to disable, reboot, re-check the configuration and enable again. |
| Supported OS range | **HYPOTHESIS**: driver and manager | Built-in maximum OS build/version and a check-level bitmask (`chkLevel`, owned by `80-apps-compat.md`) decide which compatibility checks block start. |
| Driver not answering | service | The service stops with an error exit code if the driver's control device is missing or answers unexpectedly. |
| Transition time-outs | service | `TransitionDueTime` per seat state change. |
| Manager-side check | manager | If the service is not running when it should be, the manager shows a service start error and says an antivirus may have disabled the service. |
| Recovery driver | service host action | 2.70.6 ships a tiny driver that only clears the product driver's configuration values (recovery after a boot failure). |
| Reset | service host action | The reset action (section 4.3) removes all markers so that devices fall back to seat 1. |

## 9. Logs and diagnostics

### 9.1 Log files

- Folder: `C:\ProgramData\<product> Control\logs\`.
- File name: date and time of start, a counter, then `wp` + two-digit seat number + one letter
  for the kind of instance: `s` system-context instance (a 72-byte file per seat start on the
  test PC), `u` normal user instance, `a` elevated (administrator) instance. **HYPOTHESIS** for
  the letter meanings.
- Format: plain text; each entry is a sequence number, a timestamp with microseconds and a
  level (info or error) on one line, then the message on the next line.
- These files are written by manager instances. **HYPOTHESIS** that the service writes no log
  file: neither the service host nor the logic library imports a file-write or event-log
  function; the library opens files only for the driver's control device and uses file mapping
  only for its shared section. The service and driver report through run-time registry values,
  the driver's in-memory log (read by the manager through the control device) and the core
  error history.
- Typical content on the test PC: many "overlapped driver control call failed with error 31"
  entries while the driver is disabled, the product-start variant, the per-seat account map,
  and messages from the manager's online update check.

### 9.2 Diagnostic report

Written by the control library before enabling and on support requests (UTF-16 text). Sections:
product state, current seat, started and auto-start seat masks, start variant; Windows version;
a licence section (exists; out of scope); per-seat settings (name, IPv4/IPv6, domain, masked
user, whether a password is set, CPU affinity mask); all managed devices with seat masks;
service info (last enabled time, administrator flag, install path, every value of the driver key
and the service key, crash dump setting, last boot shutdown/succeeded flags, speculation
control, console session id; Memory Integrity, Secure Boot, code-integrity policy and
guarded-host state); display configuration database and active paths; video device to
adapter/source map; device tree with owner seats and HID usages; driver log ring; core error
history; proxy settings; file checksums of every product binary. 2.70.6 adds code-integrity
options, GPU scheduling mode, running processes and per-seat details.

### 9.3 Error reporting

Errors are shown as "class(code)" pairs, for example 2(-5), 2(31), 1(<OS update number>),
plus plain Windows error numbers (109, 1617). Class 2 errors come from the driver start
(**HYPOTHESIS**); 2(31) on Windows 11 is a conflict with a Windows security policy that the
2.70.6 installer probes with the code-integrity policy tool. Error 1617: the 2.25 changelog says
it is fixed by applying the check-level tweak (`chkLevel` 0x100, `80-apps-compat.md`), so it is
tied to the OS compatibility check; its exact meaning is unknown (**HYPOTHESIS**). The core
error history keeps the UTC time of each failure.

## 10. Inter-process channels

| Channel | Between | Purpose |
| --- | --- | --- |
| Service Control Manager | manager (elevated) / installer and both services | create, configure, enable/disable (start type), query state. Users without admin rights can only query. |
| User-defined control code 200 | control library to running service | "configuration changed" notification (section 4.4) |
| Power-event and pre-shutdown controls | SCM to service | resume and shutdown handling (section 7.5) |
| Driver control device (overlapped device I/O with a time-out wrapper) | manager, service, public helper DLL and driver | read/write seat ownership, read driver state and log, apply settings. **HYPOTHESIS**: codes use the unknown device type, buffered method and small function numbers; exact codes are not needed by Pulsar. |
| A second, reduced driver control device | manager and driver | **HYPOTHESIS**: control path used when the driver runs in its safe mode |
| Registry: config root, service key, link and run-time keys | manager, service, driver | configuration in, status out (sections 5.2-5.4) |
| Windows "terminal services ready" global event | Windows to service | Windows session infrastructure is ready |
| Product global resume event | service handler to its workers (and possibly other copies) | resume after sleep (section 7.5) |
| Product per-session context event (one per session, in the session namespace) | service and the parts in a seat session (credential provider, helper) | **HYPOTHESIS**: tells the seat session that its seat context (seat number, auto-logon data) is ready |
| Product global pre-shutdown event (2.70.6) | service to other parts | pre-shutdown phase started |
| Shared memory section | service and its in-process copies in Windows services | shared state (seat table, session map) |
| LSM RPC (local RPC endpoint of the Local Session Manager) | service and Local Session Manager | session queries and control (`30-sessions.md`) |
| Session-manager port | service and Windows session manager | session creation (`30-sessions.md`) |
| Child processes | service to per-session helper and licence component; manager to service host verbs | exit codes as results |
| Named pipes (server and client in the control library) | between manager instances | **HYPOTHESIS**: an unelevated manager talks to an elevated copy, or seat managers talk to each other. The pipe name is built at run time and was not recovered. The service has no pipe code. |
| Public helper API (32- and 64-bit DLL) | third-party apps and driver | seat count, process-to-seat and session-to-seat lookup (`80-apps-compat.md`) |

Logon and logoff messages to Winlogon are not a direct channel of the service; they belong to
the in-process work of section 3.1 (**HYPOTHESIS**).

## 11. The relay launcher batch file

A two-line batch file in the install folder: it sets one environment variable to the URL of a
vendor relay script and then starts the manager. The manager uses that URL for its online
requests (for example the update check) instead of the main vendor site; the same relay URL
shows up in the manager log as the chosen connection method. Purpose: a fallback for networks
where the vendor's main site is blocked. It does not affect the service. Pulsar needs nothing
like it.

## 12. ARM64 notes

- Every part of the reference product is x64 (some x86). There is no ARM64 build, and the
  installer detects ARM64. The x64 kernel driver cannot load on ARM64 Windows at all, so on
  ARM64 the reference service has nothing to talk to.
- An x64 service could run under emulation on ARM64, but the in-process approach of section 3.1
  cannot: native ARM64 processes (LSM, audio service, LogonUI) load only ARM64 or ARM64X
  binaries. A pure ARM64EC or x64 DLL cannot load into them. The same applies to the credential
  provider: LogonUI on ARM64 is native and needs a native ARM64 (or ARM64X) COM server.
- All documented APIs the service uses exist on ARM64: SCM (including power-event and
  pre-shutdown controls), registry, WTS session APIs, `CreateProcessAsUser`, LSA private data,
  credential-provider interfaces, raw input, HID parsing, CCD display configuration, D3DKMT
  thunks, DDC/CI monitor control (DDC/CI depends on the GPU driver; support on ARM64 GPU drivers
  is an open question).
- Modern standby: many ARM64 laptops use modern standby (S0 low-power idle) instead of S3. The
  automatic-resume power event and display-mode restore must be tested there; seats may need
  to handle console display on/off notifications instead.
- Paths: the service must not assume `Program Files` vs `Program Files (x86)` layout for
  emulated parts; on ARM64 everything should be native and in `Program Files`.
- Build: the service crate builds for `aarch64-pc-windows-msvc` and `x86_64-pc-windows-msvc`;
  check the PE machine type of each artifact (0xAA64 / 0x8664).

## 13. Pulsar Seat approach

Within `docs/agents/rules.md`: no vulnerable drivers, no kernel patching, no code injection into
Windows processes, everything works with Memory Integrity and Secure Boot on, native ARM64,
user-mode or UMDF first.

### 13.1 Service shape

- One Rust Windows service with its own (Pulsar) name, own process, LocalSystem (needed for
  session tokens, LSA secrets, Winlogon values and process creation in other sessions). Native
  ARM64 and x64 builds.
- Start type automatic. Dependencies on RPC and the Local Session Manager like the reference;
  wait at start for Plug and Play, audio endpoint builder and audio service to be running, using
  documented service status queries and the SCM notification API, not fixed sleeps.
- Accept stop, pre-shutdown, power events and session-change notifications. Stop and
  pre-shutdown end seat sessions in a defined order (stop one seat at a time with a time-out),
  then report stopped. Unlike the reference, an administrator can stop the service.
- Set real SCM recovery actions (restart the service on first failures, with a reset period).
  On restart after a crash, the service re-discovers existing seat sessions through WTS
  enumeration and adopts them instead of starting duplicates.
- Add a service description and an Event Log source (Turkish UI text, English event ids are
  fine) so that failures are visible in Event Viewer.
- Service SID type unrestricted (not restricted: a restricted SID would make writes to
  Winlogon, LSA, other sessions' tokens and HKLM configuration fail unless every ACL were
  changed), plus a required-privileges list that names only what the service needs (for
  example assign-primary-token, increase-quota, TCB, shutdown), so the other LocalSystem
  privileges are removed from its token.
- Single binary with sub-commands that replace the reference verbs: install, uninstall,
  enable, disable, reset, status. No test-signing verb ever.

### 13.2 Configuration

- One configuration file (JSON or TOML) under `C:\ProgramData\Pulsar Seat\` with an ACL of
  SYSTEM and Administrators full control, Users read. Do not copy the reference's "Everyone full
  control" folder ACL. A schema version field in the file; migrations on read.
- Content: seat list (number, name, auto-start flag, CPU affinity, optional source IP),
  device-to-seat assignments keyed by stable identifiers (container id, location path, device
  interface class), display-to-seat bindings, start mode (manual / at boot / after seat-1 logon),
  timings (start delay, delay between seats, transition time-out), safety switches
  (ignore unclean shutdown), same-account policy.
- Secrets (seat auto-logon passwords) never go into the file: store them as LSA private data or
  with machine-scope DPAPI, write-only from the manager's point of view, never logged, never in
  reports (rules.md).
- Configuration changes reach the running service as a pipe request ("reload"), not as an
  unauthenticated user-defined SCM control code.
- Run-time status lives in the service's memory and is served over the pipe; nothing outside
  needs a volatile registry key or a registry symbolic link. If a Pulsar driver needs settings,
  it reads them from its own documented parameters key or gets them from the service over its
  control interface.
- Windows Update deferral: do not write Windows Update policy behind the user's back. If needed,
  offer it as an explicit manager option that shows exactly what it sets and can undo it.
- Fast startup: do not change it silently. If a full boot is needed, the manager explains why
  and offers the switch.

### 13.3 Boot sequence

1. Service starts; checks Windows safe mode (do nothing), previous unclean shutdown (enter
   Pulsar safe mode unless the user disabled the check), and a crash counter of its own (if the
   last N starts failed, stay in safe mode). OS build changes are logged and shown, and the user
   gets a "re-check configuration" prompt, but they do not block start by default. One
   component (the service) makes all of these decisions and records the reason.
2. Waits for session infrastructure readiness through documented means (service states, WTS
   session notifications).
3. Applies device and display ownership through user-mode mechanisms (CCD for displays; no
   display-class filter) and, only where unavoidable, the signed device-level input filters
   after their interface version check (`00-architecture.md` §5.6, §5.7; see 40-70).
4. Starts seats per start mode with the configured delays and per-seat time-outs.
5. Each failure is a structured event (seat, step, Windows error code) in the log, the Event Log
   and the status the manager reads.

### 13.4 Seat life cycle

Operations: start all, start one, force relogon, stop one (a Pulsar addition; the reference has
none), same-account policy, ordered stop on service stop and pre-shutdown, display restore after
resume. How a seat session is created and logged on is decided in Phase 2 (`30-sessions.md`);
the service API must not depend on the reference's in-process work. If the chosen backend's
auto-logon is not enough, use a documented credential provider (optional component P11 in
`00-architecture.md`; native ARM64 and x64 COM DLL) for per-seat auto-logon and the seat logon
screen; register it only while Pulsar is enabled.

### 13.5 IPC

- Manager to service: one named pipe with an explicit security descriptor (SYSTEM and
  Administrators read/write; interactive users read-only status plus "start my seat" style
  requests if allowed by policy). Message format: length-prefixed JSON or a compact binary
  format, versioned. Verify the client with the pipe's client process id and token, not with a
  file-name check.
- Service to per-session agents (credential provider, optional per-seat helper): the same pipe
  server (one server, a separate request set per client kind, `00-architecture.md` §5.5), plus
  per-session named events when needed, created in the session namespace with an ACL; no global writable
  section.
- Service to Pulsar drivers (if any): device interface with a documented IOCTL set, buffered
  I/O, access checked against SYSTEM only.
- The manager never talks to drivers directly; everything goes through the service.

### 13.6 Logs and diagnostics

- Rolling text or JSON-lines log under `C:\ProgramData\Pulsar Seat\logs\` (size-capped),
  plus Event Log entries for errors. Optional ETW provider for traces.
- A "diagnostic report" command in the service that gathers OS version, Secure Boot / Memory
  Integrity state, seat table, device and display assignments, service state and recent errors.
  It must never include passwords or other secrets, and it masks user names by default.
- Error codes: Pulsar's own stable error ids with the Windows error code next to them; no
  reuse of the reference's numbering.

### 13.7 Things not to copy

Code injection into Windows service processes, kernel mapping of user modules, test-signing
toggles, importing a test certificate, the "Everyone full control" data folder, silent Windows
Update or fast-startup changes, a service that cannot be stopped, an unauthenticated
"apply settings" control code, the alternate relay launcher.

## 14. Open questions

1. Start type and early-start registration of the service while enabled: is it automatic and
   in `EarlyStartServices`? (Needs an enabled snapshot of the test PC; ask the maintainer before
   enabling the product.)
2. Exact numeric meaning of `AutoStartMode` 0 and 2, and of `AutoStartSync`.
3. Units and defaults of `AutoStartDelay`, `nextTerminalDelay` and `TransitionDueTime`.
4. What the per-session helper (per-session start action) does in full.
5. Role of the per-session context event.
6. The named-pipe name and protocol in the control library and who connects to it.
7. Exact layout and contents of the run-time `volatile` keys (per-seat accounts, status values).
8. SCM controls: confirm live that stop is refused and that control 200 re-applies settings.
9. What happens to running seat sessions when the service crashes or is killed (do they stay as
   orphaned sessions, get logged off, or does seat 1 lose devices?).
10. Resume after sleep and hibernate: do seats keep their sessions, devices and audio, or does
    anything restart? Behaviour on modern-standby machines.
11. Which component blocks start on an OS build change or a failed driver boot flag (driver,
    service or manager), and what the user sees in each case.
12. Which Windows Update values the product sets, and whether it restores them when disabled;
    whether it changes `HiberbootEnabled` or only virtualises it.
13. Meaning of `ovrAutoLogon`, `vmwHpcMask`, `BootUpgradedSystem` handling and the exact
    location of the per-seat `Affinity` and `SpecificIP` values in the current config root.
14. Exact meaning of error 1617.
15. Whether DDC/CI monitor power control works with ARM64 GPU drivers (Qualcomm), needed for
    an idle-seat screensaver.
16. Whether Pulsar can reach "several local interactive sessions" without the in-process work
    at all (Phase 2 decision, shared with Pulsar Split).
