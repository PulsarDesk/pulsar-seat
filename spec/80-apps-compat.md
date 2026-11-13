# 80 - App and game compatibility

Status: draft v1 (2026-11-12, critiqued, cross-checked). Sources: the reference product 2.70.3 as installed
on the x64 test PC (service disabled, read-only probes), the 2.70.6 package (static only), its
changelog, its registry tweak files and its translation catalogue. Everything in this file is
written fresh in our own words. Items marked **HYPOTHESIS** were not verified at run time.

Related specs: `20-service.md` (service config, IPC), `30-sessions.md` (same user on several
seats, SharedPC, auto-logon, console hand-over), `40-display.md` (GPU isolation, display
modes, fullscreen), `60-audio.md`. Specs still pending: `50-input.md` (gamepads, power
buttons, touchpad input), `70-usb-devices.md` (file-system filter), `90-install.md` (Windows
Update, build gating, firewall rules, signing). Items those pending specs will own are kept
here in short form and tracked in section 12 so they are not lost.

**Search coverage.** Statements below of the form "no specific handling found" cover only
what could be searched: the kernel driver (2.70.3 and both 2.70.6 variants), the control
library, the service DLL and EXE, the two helper DLLs, the translation catalogue and the
changelog. The manager executable is packed and was **not** string-searched, so it may hold
app-specific logic we have not seen.

## 1. Scope

What one seat's apps see and how the reference product keeps common apps and games working
when several people use one PC at once:

- apps that allow only one running copy per machine (single-instance apps), Steam in
  particular;
- per-seat restrictions applied to apps: CPU cores, source IP address;
- the support-only registry tweak list (one tweak file per item, applied by the manager): the
  items this file owns are touchpad palm check, device icon size, start-up check level,
  session transition timeout, file-system filter on/off, ignore last unclean shutdown, legacy
  boot scheme, logon/lock screen image and wallpaper;
- the public helper API that lets third-party apps ask which seat they run on;
- anti-cheat and Windows Update interaction;
- known issues from the changelog that affect apps and games.

## 2. Terms

- **Seat**: one workplace (monitor(s), keyboard, mouse, audio). The reference product numbers
  seats from 1; registry subkeys use two-digit indexes from `00`.
- **Seat session**: the Windows interactive session shown on a seat (see `30-sessions.md`).
- **Global object**: a named kernel object in the machine-wide namespace (`Global\...`, or
  `\BaseNamedObjects\...` in kernel terms), visible to every session. A `Local\` object is
  per session.
- **Driver key**: the reference kernel driver's service key under
  `HKLM\SYSTEM\CurrentControlSet\Services\`. **Service key**: the reference seat service's
  service key in the same place. **Product key**: the product's key under `HKLM\SOFTWARE`,
  with per-seat subkeys `00`, `01`, ... The literal key names are in the analyst notes;
  Pulsar Seat does not read them (no coexistence or migration with the reference product is
  planned, see open question 14).

## 3. Single-instance apps

### 3.1 General behaviour

Each seat runs its own Windows session, so the session namespace already separates most apps:
objects named `Local\...`, per-session window stations, per-user registry. Apps break only
when they:

1. create a **global** named object (mutex, event, file mapping) to detect a second copy;
2. lock a **per-user** resource (profile folder, database, cache) and the same Windows account
   is signed in on two seats;
3. talk to a machine-wide service that allows only one client.

The reference product handles case 1 for one app only (Steam, section 3.2). For case 2 it
offers a policy for the same account on several seats (value `SameUserMultiLogon` on the
service key, three levels: allow, warn, deny - **HYPOTHESIS** for the exact level order). When
the warn level is active, the manager shows a one-time dialog in the second session: the
account is already active on another seat, this can make apps fail, and one account per seat
is advised. The dialog has an opt-out checkbox so it is not shown again. Case 3 is not
handled. (The full policy is described in `30-sessions.md`.)

The reference manager itself is single-instance per session: starting it a second time in the
same session shows a message that points the user at the running copy (taskbar or tray). It
runs once per seat session (it starts from the machine Run key for every user who signs in;
its logs carry a seat number and a user/admin marker). It has a per-user option to hide its
tray icon, stored next to its view settings.

### 3.2 Steam

Observed:

- The reference driver (2.70.3 and both 2.70.6 driver variants) contains the process name
  `steam.exe`, the global object name `Valve_SteamIPC_Class` under `\BaseNamedObjects`, and a
  switch name `fix4Steam`. The object type (mutex, event or section) is not visible in the
  strings.
- `fix4Steam`: read by the driver, so most likely a DWORD on the driver key
  (**HYPOTHESIS** for type and location). Default when absent: on (the changelog of 2.31 says
  the fix is switchable and enabled by default).
- Changelog 2.31: running Steam on more than one seat became possible without a sandbox
  program. Before that release the advice was to run the extra Steam copies inside a
  sandbox.

**HYPOTHESIS** (mechanism): the Steam client uses a global IPC object to find an already
running client on the machine. A second seat's Steam then finds the first seat's object and
refuses to start or hands over to the wrong client. The reference product works around this
by modifying the Steam process from kernel mode so that each seat gets its own copy of the
object. **This is kernel-mode modification of a user process and must not be copied.**

Other Steam facts relevant to implementers:

- **Steam Client Service** (live): a machine-wide Windows service from the Steam install
  (32-bit, under the shared Common Files folder of Program Files (x86)), start type Manual,
  runs as LocalSystem. It is shared by all sessions and is case 3 of section 3.1; renaming the
  IPC object does not cover it. **HYPOTHESIS**: two clients using it at once may conflict
  (for example during game installs or updates).
- **Shared install** (**HYPOTHESIS**, needs a test): two Steam clients on two seats share one
  install folder, one game library and the same machine-wide config files. Even with separate
  IPC objects they may fight over these (file locks, update writes, library manifests).
- Steam installs virtual audio endpoints (streaming speakers and microphone, root-enumerated
  media devnodes). They show up in the reference device list and must be treated as shared or
  assignable audio endpoints (see `60-audio.md`).
- **HYPOTHESIS**: a game started from a seat's Steam runs in that seat's session and so gets
  that seat's restrictions (CPU cores, source IP), as far as those restrictions apply to child
  processes at all (section 4).

### 3.3 Launchers and other apps

- In the searched modules (see "Search coverage" above), no other launcher or app has a
  specific fix or name. The packed manager was not searched. **HYPOTHESIS**: launchers that
  use global single-instance objects or a machine-wide helper service work on one seat at a
  time only.
- The test PC has its own app-compatibility layers on some games (run as admin, fullscreen
  optimisation off, DPI). These are the owner's settings. No Image File Execution Options or
  AppCompat layer entries attributable to the reference product were observed on the test PC
  (product disabled at the time); whether it writes any while enabled is not known.

## 4. Per-seat app restrictions

### 4.1 CPU cores per seat

- Manager: a dialog for splitting the logical processors among seats, opened from the context
  menu of the system (PC case) block. It is a grid: one row per seat, one checkbox per logical
  processor. Save / Cancel. Added in 2.25b1 / 2.25.
- Storage: a value `Affinity` per seat. The control library builds a per-seat path next to it,
  so most likely under the per-seat subkey of the product key (**HYPOTHESIS**); the driver and
  the seat service also read a value of that name. Live: no `Affinity` value exists anywhere
  under the product, driver or service keys on the test PC, so **absent = no limit**.
- Type and bit order: **unknown**. The diagnostic report prints a per-seat mask as a string of
  binary digits, one per logical processor (live: 16 ones on a 16-thread PC, all ones = no
  limit). Whether the stored value is a DWORD, QWORD or binary blob, and whether the first
  digit of the report is processor 0 or the highest processor, is open (question 4).
- Effect (**HYPOTHESIS**): processes started in the seat's session are limited to the selected
  cores, applied around process creation. Not verified: whether child processes, elevated
  processes and system processes of the session are covered.
- Unknowns: behaviour with more than 64 logical processors (processor groups), what happens
  when a seat has no core selected.

### 4.2 Source IP address per seat

- Purpose (from the manager help, in our words): some network programs and LAN games need each
  seat to have its own IP address, for example to play against each other on one PC or to tell
  the seats apart on the network.
- Manager: a table with one row per seat and its IP. Selecting a row opens a dialog where the
  user:
  1. picks one **static** local IPv4 address of the PC for that seat (or none);
  2. picks a rule: all apps of the seat use it / only the listed apps / all apps except the
     listed ones;
  3. edits a list of apps and folders. A folder means every executable inside it. A browse
     tree shows the computer's drives and the common and personal Desktop and Start-menu
     Programs folders and resolves shortcuts. Entries have a checkbox to switch them off
     temporarily. A very long list is cut to a fixed maximum with a warning.
- Is the list shared or per seat? The manager help says one list for all seats; the control
  library builds the list path from a per-seat subkey plus a list subkey name, which suggests
  one list per seat. **Unresolved** (question 4); treat "shared" as **HYPOTHESIS**.
- Helpers in the manager to get extra IPs: open Network Connections (add addresses to an
  adapter), open Device Manager (add a virtual network adapter), and a Hyper-V page that can
  install or remove Hyper-V, add or remove a virtual switch and add a virtual network adapter
  per seat with a fixed IP (**HYPOTHESIS**: host-side virtual NICs on an internal switch).
- Storage:
  - `SpecificIP`: DWORD per seat. Byte order (live, verified on an older per-seat key of the
    same vendor on the test PC): the four bytes of the DWORD in little-endian memory order are
    the address octets in dotted order, i.e. the address is kept in network byte order as a
    raw 32-bit value. Absent or 0 = no address.
  - `SpecificIPMode`: per seat, holds the rule. Type and value-to-rule mapping unknown
    (question 4).
  - `SpecificIPApps`: subkey holding the list (location see above).
  - The diagnostic report has IPv4 and IPv6 fields per seat (IPv6 always empty in what we
    saw).
- Effect (**HYPOTHESIS**): the driver acts on socket bind for matching processes of the seat,
  through the Winsock kernel driver device, and forces the chosen source address.
- Related: the installer adds an inbound and an outbound firewall allow rule (all profiles)
  for the manager executable. This matters for LAN features of the manager, not for games
  (to be described in `90-install.md`).

## 5. Support-only registry tweaks

The manager has a dialog for applying support tweaks: a drop-down list, a strong warning that
the items are for use only on vendor support's advice, a confirmation step, then either
success plus a reboot request or an error with a numeric code. Each item is one `.reg` file in
the install folder's `Regpatches` subfolder; 2.70.3 and 2.70.6 ship the same 29 files. All
values are DWORDs unless noted.

How a tweak is applied (**HYPOTHESIS**, not observed at run time): the manager imports the
chosen file as a whole. Writing the HKLM parts needs administrator rights, so the manager or
the import must run elevated. HKCU parts land in the hive of the account that runs the import
only, i.e. the user signed in on the seat where the manager was used (or the admin account
used for elevation). Other seats' users are **not** changed. This limits what the HKCU items
can achieve (see `AAPThreshold` below).

### 5.1 Items owned by this spec

| Item | Writes | Meaning |
| --- | --- | --- |
| Touchpad palm check | `AAPThreshold`=0 under both HKCU and HKLM `SOFTWARE\Microsoft\Windows\CurrentVersion\PrecisionTouchPad` | Windows precision-touchpad accidental-activation sensitivity set to the most permissive level, so the touchpad is not blocked while someone types. **HYPOTHESIS**: needed because typing on any seat counts as typing for the whole machine and blocks the touchpad of another seat. Related changelog item: 2.24 fixed a touchpad that did not work on a USB monitor. The HKCU part only reaches the user who applied the tweak; whether Windows honours the HKLM value for other users is unknown (question 11). Live: both values are 0 on the test PC. |
| Adjust icon size | `adjustTermIconsSizes`="true" (REG_SZ) in the manager's per-user settings key | Manager-only view preference: scale device icons to fit the screen. Also a checkbox in the manager's view settings. Per user, like `hideTrayIcon`. |
| Check level | `chkLevel` = 0x000, 0x100, 0x300, 0x700 or 0x800 on the driver key | Selects which start-up compatibility checks the product enforces before it enables seats. Live value 0x100. The effective value is printed in the diagnostic report. Checks seen near it: OS build and version range, server SKU allowed, Secure Boot state, a legacy Windows 7 update, last boot succeeded. Changelog 2.25: one start error (code 1617) is fixed only if the 0x100 level is applied. **HYPOTHESIS** for every bit meaning, for 0x000 meaning "all checks off", and for whether the values are bits at all: the set 0x100/0x300/0x700 looks cumulative, while 0x800 may be a separate mode. |
| Session transition timeout | `TransitionDueTime` = 30 or 40 on the service key | A timeout used by the seat service near its console hand-over and seat start-up logic (the value is read next to the legacy session scheme switch and the seat run-state logic). Changelog 2.28 raised a start-up timeout for extra seats on slow systems. **HYPOTHESIS**: unit seconds; the maximum time for one console hand-over or seat start step. What the service does when it expires is unknown. Default when absent: unknown. Not present live. |
| File-system filter off / on | `disableFSFilter` = 1 / 0 on the driver key | Turns the reference minifilter off or on. That filter controls per-seat access to and hiding of USB disks, optical drives and physical drives. Used when backup, disk, encryption or antivirus tools conflict with it. Detailed behaviour belongs to `70-usb-devices.md` (pending). |
| Ignore last shutdown | `ignoreLastShutdown`=1 on the service key | Skips the unclean-shutdown guard. Observed: the seat service reads the Windows value `LastBootShutdown` under `HKLM\SYSTEM\CurrentControlSet\Control`; the driver reads `LastBootSucceeded` from the same place and has its own switch `ignoreLastBoot`; the driver also has a separate recovery control device (a second device link next to its main control device) that the control library knows. The manager has a message that after an abnormal reboot the product runs in its own safe mode and the user should remove the cause and reboot. **HYPOTHESIS**: in that safe mode extra seats are not started and only seat 1 works, while the recovery device lets the manager talk to the driver. Live: both Windows values are 1. |
| Windows Safe Mode | none (not a tweak) | Separate from the above: the driver reads the Windows Safe Mode boot flag (`InitSafeBootMode`), and the manager has a message for the case where Windows itself was started in Safe Mode. **HYPOTHESIS**: the product does not enable seats in Windows Safe Mode. |
| Legacy boot scheme | `oldFashionBoot`=1 on the driver key and `Start`=4 on **both** the driver key and the service key | Switches back to an older driver load / enable scheme (**HYPOTHESIS**; the changelog reworked both in 2.26 and 2.29.1). Because it also disables both services, the product is off after the reboot until the user enables it again. |
| Logon screen logo | `LockScreenImagePath` (REG_SZ) under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PersonalizationCSP` | Sets the machine lock / logon image to the product logo. The shipped file points at a path on the vendor's build machine and has a malformed key line, so it most likely does nothing on a customer PC (**HYPOTHESIS**). It also writes only the path: **HYPOTHESIS** (public Windows behaviour, to verify) that this key normally also needs `LockScreenImageStatus`=1 and `LockScreenImageUrl` to take effect. |
| Remove wallpaper | same value set to an empty string | Clears the lock-screen image again. Live: the key is absent on the test PC. |

Related manager options (not tweak files): use a product wallpaper as each seat's desktop
background, use it as the seat lock screen, replace the logon/lock image only while the product
is enabled, open the Windows lock-screen settings, open screensaver settings, and an idle-seat
power-saver screensaver that turns off only that seat's monitors (installed into System32;
a monitor may show black instead of switching off). The install folder holds ten numbered
wallpaper images (**HYPOTHESIS**: one per seat number, to tell seats apart).

### 5.2 Items owned by other specs (short form)

| Item(s) | Short meaning | Spec |
| --- | --- | --- |
| `AllGPUAccess`, `conMonNtfy` 1/2, `enableVidPN`, GraphicsDrivers `HwSchMode` 1/2 | cross-seat GPU access, monitor-change notification, display-path handling, hardware GPU scheduling on/off | `40-display.md` |
| `DisableAutomaticRestartSignOn`, `SameUserMultiLogon` 0/1/2, `SharedPCMode`, `oldFashionSTSM` | restart auto sign-on, same account on several seats (three levels), shared-PC mode, legacy session switch scheme | `30-sessions.md` |
| `DisableSysButton`, `propFilters` | block system power/sleep buttons per seat, input filter placement | `50-input.md` (pending; tracked in section 12) |

## 6. Public helper API for apps

- Two tiny DLLs (32-bit and 64-bit) export three functions by name: `GetMaxTerminalsCount`,
  `ProcessIdToTerminalId`, `SessionIdToTerminalId`. They let an app (for example game-club or
  kiosk software) ask how many seats exist and which seat a process or session belongs to.
  They talk to the driver's control device.
- Parameter and return types: not determined (open question). Error behaviour when the product
  is disabled: not determined.

## 7. Anti-cheat

- In the searched modules (driver, control library, service, helper DLLs, translations,
  changelog) no anti-cheat product is named and no specific handling was found. The packed
  manager was not searched.
- **HYPOTHESIS** (not tested): kernel anti-cheats can refuse to run or flag the PC because the
  reference product (a) needed test-signing mode on its legacy path, (b) modifies user
  processes from kernel mode (section 3.2) and (c) patches graphics-kernel internals. Some
  anti-cheats also refuse to run in a session they see as remote, or when Memory Integrity is
  off.
- **HYPOTHESIS**: games that only accept the "console" session may fail on extra seats. The
  reference driver references a routine that changes the active console session, which
  suggests it moves the console identity between seats (see `30-sessions.md`).

## 8. Windows Update interaction

- The seat service DLL contains the names of Windows Update UX deferral values under
  `HKLM\SOFTWARE\Microsoft\WindowsUpdate\UX\Settings` (`DeferUpgrade`,
  `DeferFeatureUpdatesPeriodInDays`, `DeferQualityUpdatesPeriodInDays`, `BranchReadinessLevel`,
  `ExcludeWUDriversInQualityUpdate`) and reads an override value `ovrUpdatesPeriod` from its
  own service key. Live values on the test PC: feature 30 days, quality 15 days, defer on,
  drivers excluded. **HYPOTHESIS**: the service writes these values (with `ovrUpdatesPeriod`
  changing the period); whether it writes or only reads them, and whether the live values came
  from the product or the owner, is not verified.
- 2.70.5+ (driver switch `BlockWUHandlers`, present in both 2.70.6 driver variants, absent in
  2.70.3): while the product's services are enabled, the Windows Update engine is blocked from
  loading (**HYPOTHESIS**: by watching image loads).
- When an update changes the OS build while the product is enabled, the manager tells the user
  to disable the product, reboot, check the seat configuration and enable it again. A build
  newer than the product supports is refused; the manager offers to look for a newer product
  version and says preview (Insider) builds are not supported.
- 2.70.6 adds error 2(31): a conflict with a Windows 11 security policy; the installer checks
  one code-integrity policy by GUID and the manager links to instructions to disable it
  (to be described in `90-install.md`, pending).

## 9. Known issues from the changelog (app and game relevant)

| Area | Issue (in our words) | Releases |
| --- | --- | --- |
| Fullscreen games | Limits on fullscreen modes; better fullscreen support | 2.13, 2.15 |
| Performance | Lower FPS on extra seats | 2.14 |
| Gamepads | Xbox-type controllers per seat | 2.15 |
| Graphics APIs | 2.23: an optional ban on using GPUs of other seats was added, enforced only for Vulkan apps. 2.26b2: GPU isolation changed to fix a Vulkan problem. DirectX errors after an OS update also fixed around 2.26b2 | 2.23, 2.26b2 |
| Touchpad | Touchpad not working on a USB monitor fixed | 2.24 |
| Start-up checks | Start error 1617 fixed when check level 0x100 is applied | 2.25 |
| CPU cores | Assigning CPU cores to seats added | 2.25b1, 2.25 |
| Audio | Extra seats without sound devices; Store apps without audio (fix for Windows 10 build 15063 and later only) | 2.14, 2.25b1, 2.27.2, 2.29.1 |
| Seat start-up | Longer start-up timeout for extra seats on slow systems | 2.28 |
| Steam | Steam on several seats without a sandbox, switchable, on by default | 2.31 |
| System services | Windows Search service problem; RDP connections broken on one build | 2.26.2, 2.26.1 |
| Security software | Antivirus incompatibility and false positives; antivirus can disable the seat service | 2.12, 2.52.2 |
| Logon | Network-connection error on manual logon | 2.60.4 |
| Display recovery | Software key combination to restart graphics added | 2.26b2 |
| Display apps | Compatibility with a third-party virtual-display app, basic display adapter | 2.20, 2.28 |
| Screen blanking | All seats blank when one user logs off (fix later withdrawn); other seats blank when one monitor is switched off | 2.27.2, 2.28, 2.31 |
| OS updates | Almost every release adds support for new Windows builds; unsupported builds stop the product | all |
| Secure Boot | Had to be off before 2.25, and still on server SKUs / WHQL-only systems; full support in 2.70 | 2.25, 2.70 |
| Windows Update | Blocking of destabilising updates; policy conflict error 2(31) | 2.70.5, 2.70.6 |

Most of these come from the reference design itself (patching graphics and session internals
that change with each Windows build). Pulsar Seat avoids that class of problem by design.

## 10. ARM64 notes

- The whole reference product is x64 only and its kernel driver cannot load on ARM64. None of
  sections 3.2, 4 or 8 works there.
- Most games and launchers on Windows on ARM are x64 or x86 programs run under emulation. The
  Steam client is a 32-bit (x86) program today (installed under Program Files (x86)), and its
  client service is 32-bit too. Anything Pulsar Seat puts into or next to an app process (for
  example a shim DLL) must match the target process architecture: x86 for 32-bit apps, x64
  (or ARM64EC) for emulated x64 apps, ARM64 for native apps. Do not assume the target is
  ARM64.
- CPU cores per seat: many ARM64 SoCs mix core types. The core picker must show each core's
  efficiency class (CPU-set information) and suggest balanced splits, not just numbers.
  Emulated processes honour CPU sets and affinity like native ones (**HYPOTHESIS**, verify).
- Touchpad palm check: same Windows value on ARM64 laptops; most ARM64 PCs are laptops with a
  precision touchpad, so this item matters more there.
- WFP (section 11.3) and the documented process/CPU-set APIs are available on ARM64 Windows.
- Hyper-V (for extra IPs) exists on Windows 11 Pro ARM64, but needs virtualization support from
  the firmware; treat it as optional.
- **HYPOTHESIS**: kernel anti-cheat support on Windows on ARM varies by vendor and title; some
  have added ARM64 support, others have not. Check per title; outside our control.
- The helper API DLL must ship as x86, x64, ARM64 and ARM64EC builds.

## 11. Pulsar Seat approach

Rules (`docs/agents/rules.md`): no vulnerable drivers, no kernel patching, no code injection
from kernel mode, works with Memory Integrity and Secure Boot on, user mode first, every
component native on ARM64 and x64.

### 11.1 Single-instance apps and Steam

- Default: rely on the per-session namespace; most apps just work.
- Same account on several seats: offer the same three-level policy (allow / warn / deny), owned
  by `30-sessions.md`. Default **warn**. Our own Turkish copy explains that apps locking the
  user profile (browsers, mail clients, cloud sync) may fail.
- Global single-instance objects (Steam and similar): do **not** rename objects from kernel
  mode. Options, to be decided after a test on real hardware:
  1. **Default for the first release**: document the limitation and list known affected apps.
     First check whether current Steam versions still block a second session at all and which
     global object(s) they use (behaviour may have changed since 2022), and test the shared
     install folder, library and the machine-wide client service (section 3.2).
  2. **HYPOTHESIS**, limited value: a separate Windows account per seat does **not** give an
     app a private `Global\` namespace; global objects are machine-wide for every account.
     A different account only changes the access-check result, so a second copy may fail to
     open the first one's object (and may then fail to start) instead of attaching to it.
     AppContainer / private namespaces do not apply to classic desktop apps like Steam. So
     there is no documented user-mode way to give an unmodified app a private global
     namespace; this option is dropped unless a test shows a useful effect.
  3. Last resort, **opt-in only, maintainer approval required before any implementation**: a
     user-mode shim loaded only into that one app by our launcher that adds a seat suffix to
     the one known global object name. Risks: the Steam client hosts anti-cheat / anti-tamper
     components, so injecting into the Steam client itself can be flagged and may get
     accounts or games blocked; the shim must be an **x86** build for today's Steam client
     (plus x64/ARM64 variants for other targets, section 10); it breaks when the app changes.
     Never for games, never for processes under anti-cheat, never system-wide.
- Our manager is single-instance per session (named object in `Local\`), and talks to the
  service over the named pipe (`20-service.md`).

### 11.2 CPU cores per seat

- User mode only, and **scoped to processes our seat launcher starts** (the seat shell and the
  apps the user starts from our launcher). We do **not** open arbitrary processes of the seat
  session, so games under anti-cheat and protected (PPL) processes are never opened by us
  (consistent with 11.6).
- Mechanism: the launcher creates the process suspended inside a job object. Hard limit: a
  job-level affinity (documented job group-affinity information). Soft limit: the process's
  default CPU sets, set on the handle the launcher already owns before the process resumes.
  Child processes stay in the job, so a job-level limit also reaches games started from a
  launcher inside the job (unless they break away from the job; document that).
  **HYPOTHESIS** to verify: whether default CPU sets are inherited by child processes, and
  whether a documented job-level CPU-set limit exists on the supported Windows builds; if not,
  the soft limit only covers the first process.
- Elevated / UAC-launched processes: an app elevated through UAC is created by the system
  outside our job, so it is **not** covered. State this in the help; optionally a seat-wide
  soft default (CPU sets) applied by the service through a documented per-process API to
  newly seen processes, **opt-in**, with the known limit that such a watcher is asynchronous
  (the process runs briefly before the limit applies) and that opening some processes fails;
  failures are logged and skipped, never forced.
- More than 64 logical processors: CPU sets cover all processor groups; hard affinity through
  a job or affinity mask covers one processor group per mask, so a hard limit on a
  multi-group PC must be expressed per group or the UI must restrict hard limits to one group.
- Store per seat in Pulsar Seat's config as a list of CPU-set IDs, not a raw bit string.
- Validation (Pulsar design, not observed in the reference): every seat keeps at least one
  core; on hybrid CPUs warn when a seat gets only efficiency cores.
- Show core type (efficiency class) and processor group in the manager.

### 11.3 Source IP per seat

- Phase 5 or later; not required for the first release.
- **Default (first release with this feature): no driver.** Help the user create additional
  local addresses on an existing adapter or on a virtual adapter (for example a Hyper-V
  internal switch NIC where available), and let apps that have a bind-to-address setting use
  them. Loopback addresses (127.0.0.0/8) already work locally but cannot be reached from the
  LAN, so they do not replace real adapter addresses.
- **Optional, later, maintainer decision**: a WFP callout driver (KMDF) at the documented ALE
  bind-redirect layers (`FWPM_LAYER_ALE_BIND_REDIRECT_V4` / `_V6`) that maps a seat's
  processes to a chosen local address. It is a supported mechanism, builds for ARM64 and works
  with Memory Integrity, but it is an extra kernel driver and needs attestation signing (see
  `docs/agents/plan.md`, signing risk); rules.md prefers user mode. No interception of the
  Winsock kernel driver.
- Keep the same model: per-seat address + rule (all / only listed / all except listed) + an
  app/folder list (shared or per seat to be decided; per seat is simpler to reason about).

### 11.4 Support tweaks

- No hidden support-only drop-down of raw registry files. Each still-useful item becomes a
  named, documented option with its own Turkish help text and undo:
  - touchpad palm check: an option in the input settings, applied per user inside each seat
    session by our per-session agent (not only for the user who runs the manager), and
    machine-wide only if the user asks;
  - icon size: not a setting; the manager layout is responsive;
  - check level: replaced by an explicit pre-flight check list in the manager (OS build,
    Secure Boot, Memory Integrity, driver signing, session support), each check reported as
    pass / warn / fail with a reason. No bitmask;
  - session transition timeout: a numeric option (seconds) with a sane default; the service
    also extends it automatically on slow start (logged);
  - file-system filter on/off: belongs to `70-usb-devices.md`; must have an off switch;
  - unclean-shutdown guard: keep the idea (do not start extra seats after repeated crashes)
    as a crash-loop counter: after N failed boots in a row start seat 1 only and show why;
    an option disables the guard. In Windows Safe Mode, do not start extra seats;
  - legacy boot scheme: none (we have one scheme);
  - logon/lock image and wallpaper: our own generated images (seat number and colour), no
    third-party assets. Desktop wallpaper is set per user inside the seat session with the
    documented user API. The lock/logon image is machine-wide in Windows; we only offer it as a
    machine-wide option, set all values that key needs, and restore the previous values on
    disable or uninstall;
  - idle-seat power saving: belongs to `40-display.md` (monitor power per seat).
- Every HKLM change needs elevation through the service, never through the manager importing
  files.

### 11.5 Helper API

- Ship a small "which seat am I on" API: a named-pipe query to the service, plus a DLL with
  functions of the same three names for drop-in compatibility (if the signatures can be
  confirmed from public sources or by black-box testing) and Pulsar-native names. Builds for
  x86, x64, ARM64, ARM64EC.

### 11.6 Anti-cheat

- Our design (no kernel patching, no test-signing, Memory Integrity and Secure Boot on) is the
  one kernel anti-cheats expect. Never open, inject into or modify game processes; per-seat
  limits reach games only through job-object inheritance from our launcher (11.2). Keep a
  public compatibility list filled from real tests.
- Find out early whether extra seat sessions look "remote" or "not the console" to games
  (depends on the session approach chosen in `30-sessions.md`).

### 11.7 Windows Update

- Do not block the Windows Update engine and do not write deferral values silently. Pulsar
  Seat avoids OS internals, so OS builds should not break it.
- Optional, visible setting: exclude driver updates from quality updates (protects a working
  GPU driver setup), with a clear explanation. Off by default.

## 12. Open questions and tracked items

1. `chkLevel`: what does each of 0x000, 0x100, 0x300, 0x700, 0x800 enable? Are they bits
   (0x100/0x200/0x400 cumulative) or modes (0x800 separate)? Does 0x000 mean all checks off?
   Default when absent?
2. `TransitionDueTime`: default, unit, which step(s) it times, and what happens on expiry.
3. Does current Steam still refuse a second copy in another session? Which global object(s)
   and of which type? Do two clients conflict over the shared install, library or client
   service? Test only after the maintainer agrees (needs the product enabled or two live
   sessions, which is a system change).
4. `Affinity`: location, type (DWORD / QWORD / binary) and bit order versus the report string.
   `SpecificIPMode`: type and value-to-rule mapping. `SpecificIPApps`: one list per seat or one
   shared list? IPv6 support.
5. Does the reference apply the CPU limit to all processes of the seat session, child
   processes, elevated processes, or only to user apps?
6. Signatures of the three helper API functions; does any known third-party software use them?
7. Which Windows Update values does the product write itself, what does `ovrUpdatesPeriod`
   do, and are the values removed on disable?
8. Which code-integrity policy is behind error 2(31), and does it affect Pulsar Seat drivers?
9. Do the numbered wallpapers map one-to-one to seat numbers, and does the product restore the
   user's own wallpaper on disable?
10. Lock-screen image: which of `LockScreenImagePath`, `LockScreenImageStatus`,
    `LockScreenImageUrl` are required, and is the key honoured on Pro SKUs or only on
    Enterprise/Education?
11. Does Windows honour the HKLM `AAPThreshold` for users who have no HKCU value, and is the
    palm check really machine-wide across sessions?
12. What exactly does the product's own safe mode do (which seats start, what the recovery
    control device allows)?
13. Does the packed manager contain app-specific logic (other launchers, anti-cheat names)?
    Needs runtime inspection, which is out of reach while the test PC stays read-only.
14. Should Pulsar Seat detect an installed reference product (to warn about two multi-seat
    stacks)? If yes, the reference service key names become interop identifiers and must be
    added here.
15. Tracked for pending specs: `DisableSysButton` and `propFilters` (`50-input.md`); the
    file-system filter and `disableFSFilter` (`70-usb-devices.md`); firewall rules, build
    gating, error 2(31), `BlockWUHandlers` and the two 2.70.6 driver variants
    (`90-install.md`). Until those specs exist, the short descriptions in this file are the
    reference.
