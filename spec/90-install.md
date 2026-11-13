# 90 — Install, uninstall, drivers, signing, updates

Status: **draft v1 (2026-11-12, critiqued, cross-checked)**

Behaviour spec of how the reference product gets onto a PC and off it again: installer and uninstaller
actions, the files it lays down, the services and the driver it registers (and how), the registry and
system settings it changes, firewall/antivirus changes, the update channel, and what changed between
the two versions studied (2.70.3 installed on the test PC, 2.70.6 installer examined statically).

Written by the analyst from static inspection of the installer script and binaries, plus read-only
inspection of a PC with the product installed (disabled at the time). No code, no copied UI text.
Items marked **HYPOTHESIS** are inferred, not observed.

Out of scope: a licence component exists. It is not described here.

---

## 1. Installer package

### 1.1 Format

- One signed x64 Windows executable built with a commercial setup authoring tool (script-driven,
  compressed archive appended to the executable). There is **no MSI**, no MSIX, and **no driver
  package (no INF, no CAT)**.
- The payload is a scripted installer: a setup script (dialogs, conditions, actions) plus ~210 file
  records. All binaries are x64 (a few x86). Nothing is ARM64.
- The setup engine copies itself and the script into a per-product uninstall folder under
  `C:\ProgramData\Uninstall\{product-code}\` and uses it later for change / repair / remove. Cached
  copies of the prerequisite programs are kept there too.
- Product code (a GUID) is fixed across versions. An older product generation used a different product
  code and was an MSI. The installer removes the older one through the Windows Installer engine
  **only when both** the old product code is registered **and** the old generation's service
  executable still exists at the install path recorded in the old generation's registry key.
- The setup UI is localised (8+ languages).

### 1.2 Run modes, quiet switches, exit codes

- Modes: install, change, repair, remove (one engine, selected by switches or by the maintenance
  dialog when run again).
- Add/Remove Programs: the normal uninstall string runs the cached setup with `/remove /q0`; the quiet
  uninstall string uses `/remove /q`. (`/q0` = remove with minimal UI, `/q` = fully quiet;
  **HYPOTHESIS** on the exact difference.)
- Reboot handling: the engine keeps a pending-reboot flag and a reboot-mode setting; a child
  process exit code of 3010 or 8192 is treated as "reboot needed".
- Finish: when no reboot is required, setup launches the manager; otherwise it ends with a reboot
  prompt (suppressed in quiet mode, **HYPOTHESIS**).

### 1.3 Pre-flight checks (grouped by topic)

**Privileges and previous installs**

| Check | Result |
| --- | --- |
| Running as administrator | required (elevating stub) |
| Previous generation installed (see 1.1 condition) | uninstall it first (MSI remove) |
| Seat service **or** kernel driver currently enabled (2.70.6: driver running, or its start type is boot/system/automatic; 2.70.3: seat service running) | **refuse**: the user must disable the product in the manager and reboot first. Uninstall refuses in the same state, with a message saying a dependent service could not be stopped. |

**OS and code-integrity environment**

| Check | Result |
| --- | --- |
| OS is Windows 7 without the SHA-2 code-signing update | warn and open the Microsoft download page (legacy) |
| Windows build ≥ 26100 | run the built-in code-integrity policy tool and test whether one specific Microsoft-supplied policy is enforced and authorised; store the result (**HYPOTHESIS**: this is the Windows 11 policy behind a driver-load error caused by a Windows 11 code-integrity policy, for which 2.70.6 added user guidance; what setup does with the result is unknown, see Q2) |
| Secure Boot state (`HKLM\SYSTEM\CurrentControlSet\Control\SecureBoot\State\UEFISecureBootEnabled`) | stored; **HYPOTHESIS**: selects which driver build is laid down (see 3.3) |
| Memory integrity state (`...\Control\DeviceGuard\EnableVirtualizationBasedSecurity` = 1 or `...\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity\Enabled` = 1) | stored; controls the "required settings" component (see 3.3) |
| Compatibility helper (2.70.6: 5th prerequisite program; 2.70.3: 4th) | a small x64 console helper run during setup; it uses `WinVerifyTrust`, `RtlGetVersion` and a registry read. **HYPOTHESIS**: checks OS version and driver/signature compatibility before the driver is laid down. Its result handling is not readable statically. |
| Host CPU is ARM64 (via the WOW64 architecture environment variable) | a symbol for it is defined in the process-check area of the script; **no condition that uses it could be identified** statically. Behaviour on ARM64 (refuse / warn / continue) is unknown (see Q10). |
| Server SKU | writes an extra "allow on server OS" flag (`allowservos` = 1 on the driver service key) |

**Prerequisites and environment**

| Check | Result |
| --- | --- |
| VC++ 2015–2022 x64 runtime older than a minimum | installs/repairs it silently, no restart |
| Microsoft Defender antivirus active | enables the "add exclusion" option |
| Seat capacity | the driver supports up to 12 seats in 2.70.3; up to 16 in 2.70.6 is a **HYPOTHESIS** (`30-sessions.md` §2). The installer's two component sets are the signing variants A and B of §3.3, not capacity variants (**HYPOTHESIS** on how one is chosen) |

### 1.4 Optional choices offered to the user

Each is a checkbox-style option in the setup wizard; they change system-wide settings outside the
product's own keys:

| Option | What it changes |
| --- | --- |
| Disable fast startup (shown only when it is on, Windows 8+) | `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power` `HiberbootEnabled` = 0 |
| Sleep off | activates the built-in High performance power plan; sets monitor timeout (AC) and standby timeout (AC) to never; turns USB selective suspend (AC) off |
| Create restore point | System Restore checkpoint of type "application install" |
| Make crash dumps small | when a full/kernel/automatic dump is configured, switches to small memory dumps |
| Add antivirus exclusion | Defender path exclusion for the install folder |
| Add firewall rules | two allow rules (inbound and outbound) for the manager executable, all profiles, named after the product; any old rules with that name are removed first |

On the test PC: fast startup off, small dumps on, Defender exclusion and firewall rules present,
power plan untouched, no restore point.

### 1.5 Post-install steps

- Run the service helper elevated with the post-install verb (section 4).
- **Fresh install only**: run the first-run wizard (console). The script skips it on an upgrade
  (condition on an "old version present" variable).
- Finish behaviour as in 1.2.

---

## 2. Files laid down

### 2.1 Install folder (`%ProgramFiles%\<product>\`, x64 Program Files)

| Item | Role |
| --- | --- |
| Manager GUI executable (+ branding DLL, control library DLL) | tray/manager app, autostarted |
| Seat service executable + service logic DLL | Win32 service; the DLL is also a credential provider COM server |
| Main driver image as a renamed file (`.sy_` extension) | a **plain, uncompressed** signed kernel driver PE; 2.70.6 ships two builds of it (3.3), both placed here. The extension keeps it from being mistaken for a driver in that folder. |
| Two tiny app-facing helper DLLs (x86 and x64) | public "which seat is this process on" API |
| First-run wizard (console) | run by setup on a fresh install (1.5) |
| Recovery driver | 2.70.3's script lists it in the install folder; 2.70.6 places it in `System32\drivers` (2.2). Not present on the test PC. |
| GUI framework DLLs, platform plugin, multimedia plugin (2.70.6), `qt.conf` | GUI runtime; `qt.conf` is chosen from two variants by OS build |
| `Regpatches\*.reg` (29 files) | the manager's advanced-settings toggles (see `20-service.md` / `10-manager.md`) |
| `Translations\`, `Userimages\` (device icons), `Wallpaper\`, `Styles\`, `EULA\` | UI resources |
| Changelog text file | shown by the manager |
| Small batch file | starts the manager with an alternate web-service endpoint in an environment variable |
| Licence component files | out of scope |

### 2.2 Outside the install folder

| Path | Content |
| --- | --- |
| `%SystemRoot%\System32\drivers\` | the main driver under its real `.sys` name — **copied there by the service helper**, not by the setup engine (on the test PC the `.sys` and the `.sy_` in the install folder have the same hash; the setup script has no file record for it there). 2.70.6: the recovery driver is placed here by the setup engine. |
| `%SystemRoot%\System32\` | idle-seat screensaver (`.scr`) |
| `C:\ProgramData\Uninstall\{product-code}\` | setup engine copy, resolved setup script, cached prerequisites |
| Common Start Menu `\<product> Control\` | manager, online user guide, EULA, and licence component files (out of scope) |
| Public Desktop | manager shortcut |
| `%LOCALAPPDATA%` / `%APPDATA%` `\<vendor>\<product>\` | created empty |
| `C:\ProgramData\<product> Control\`, `C:\ProgramData\<older-brand> Utils\` | created later by the manager / wizard (config JSON, reports, logs) — not by setup |
| `C:\Program Files (x86)\<product>\Regpatches\`, `...\translations\` | empty folders on the test PC, created over a year before the current install. **HYPOTHESIS**: leftovers of an earlier install that put resources in the 32-bit Program Files; not written by the current setup. Implementers need not reproduce them. |

### 2.3 RunOnce

The setup script references the machine `RunOnce` key (**HYPOTHESIS**: used to resume a step after a
required reboot). No value was present on the test PC. The script also writes one oddly named
registry value (named after the Program Files path, value 0) whose purpose is unknown (**HYPOTHESIS**:
setup-engine bookkeeping). Neither matters for Pulsar Seat.

---

## 3. Driver and services

### 3.1 Driver model and how it is installed — no INF

- Main driver model: a **WDM** kernel driver started as a legacy (non-PnP) service. It acts as a PnP
  class upper filter on several device classes, owns a control device, and also registers as a
  file-system minifilter (see `00-inventory`, `50-input.md`, `70-usb-devices.md`).
- The driver is **not** installed through PnP / the driver store. There is no INF, no catalog,
  no `oem*.inf`, nothing in `DriverStore\FileRepository`, and `setupapi.dev.log` never mentions it.
- Instead the service helper (run elevated by setup with the post-install verb):
  1. copies the main driver image from the install folder into `System32\drivers` under its `.sys`
     name (observed result; **HYPOTHESIS** that in 2.70.6 it copies whichever of the two builds setup
     laid down);
  2. creates a **legacy kernel-driver service** with the Service Control Manager: type kernel driver,
     image `system32\DRIVERS\<driver>.sys`, error control *ignore*, no load-order group;
  3. creates a **root-enumerated devnode** `ROOT\<DRIVER>_DEVICE\0000` with the SetupDi APIs, class
     *LegacyDriver* `{8ECC055D-047F-11D1-A537-0000F8753ED1}`, no hardware ID, no function driver —
     a placeholder node the driver can hang per-device state and power/PnP notifications off;
  4. writes a file-system **minifilter instance** under the driver's service key:
     `Instances\<service>` with `Altitude` = `429999`, `Flags` = 0, plus `DefaultInstance`;
  5. writes initial tuning values on the driver service key (keyboard hotkey code, DirectX test flag,
     etc. — see `20-service.md`);
  6. creates the seat service (below);
  7. asks for a reboot.
- **Class upper-filter entries.** Classes referenced: Display `{4D36E968-...}`, HIDClass
  `{745A17A0-...}`, Keyboard `{4D36E96B-...}`, Mouse `{4D36E96F-...}`; the filter name is appended to
  the class key `UpperFilters` multi-string. Observed: no entry while disabled. **HYPOTHESIS**: written
  on enable and removed on disable (only the disabled state was observed; see Q4).
- A driver option (`propFilters`, source: driver strings and the inventory) extends filtering to
  child devices of filtered devices. **HYPOTHESIS** on its exact effect.
- **Credential provider** (`Authentication\Credential Providers\{CLSID}` + `HKCR\CLSID\{CLSID}\InprocServer32`,
  apartment threading): registration code exists in both the service helper's install path and the
  control library. Observed: absent while disabled. **HYPOTHESIS**: registered at enable (or at
  post-install and removed again on disable); timing unresolved (Q4).
- **Early-start list**: **HYPOTHESIS**, the seat service is added to
  `HKLM\SYSTEM\CurrentControlSet\Control\EarlyStartServices` while enabled (the control library
  references that list; no product entry while disabled).
- **Device-install restriction**: the helper references `DeviceInstall\Restrictions\DenyDeviceClasses`;
  the key exists and is empty on the disabled test PC. The changelog says installing display drivers
  is blocked while the product is active. **HYPOTHESIS**: the display class GUID is written there while
  enabled; which class GUIDs are written is not observed.

### 3.2 Service records (observed)

| | Kernel driver | Seat service |
| --- | --- | --- |
| Type | 1 (kernel driver) | 0x10 (own process) |
| Start when enabled | boot/system/auto (setup treats ≤ 2 as "enabled"); exact value **HYPOTHESIS** | auto (**HYPOTHESIS**) |
| Start when disabled | 4 | 4 |
| ErrorControl | 0 | 1 |
| Group | none | `COM Infrastructure` |
| Dependencies | — | RPCSS, LSM |
| Account | — | LocalSystem |
| Extra values | many tuning DWORDs, `OSBuild`, `lastEnabled` timestamp, `Instances\...` | `KeyImage` = manager file name; `link`/`link\volatile` subkeys at runtime |
| Security descriptor | default-like | SYSTEM and Administrators full; **Interactive users and Service users may query and start** it |
| Failure actions / triggers | — | none |

**"Enabled" vs "disabled"** is the start types plus (per 3.1, partly HYPOTHESIS) the filter,
credential-provider and early-start registrations. Disabling sets both start types to 4; files and the
devnode stay. A reboot is always required to switch.

### 3.3 Signing and Secure Boot

| Build | Driver signature | Loads with Secure Boot on? |
| --- | --- | --- |
| 2.70.3 (installed) | vendor EV code-signing cert only (old cross-signed chain to the Microsoft Code Verification Root) | **HYPOTHESIS: on some systems only.** Windows 10 1607+ refuses new cross-signed-only kernel drivers when Secure Boot is on, except on systems upgraded from an older release (the "upgraded system" exemption) and except where policy turns the exemption off. The changelog says that on client Windows Secure Boot no longer has to be disabled, but on Server 2016+ it still must (Server requires a Microsoft signature). The service helper reads the Windows code-integrity "upgraded system" value, which fits this. The test PC has that value set, **and yet** the driver was observed blocked at boot by the in-box Endpoint Security code-integrity policy with Secure Boot and memory integrity off (`00-architecture.md` §3.4). So the exemption does not help on current builds. |
| 2.70.6 "A" | vendor EV only | as above (**HYPOTHESIS**) |
| 2.70.6 "B" | **Microsoft Windows Hardware Compatibility Publisher only** (attestation-signed) | **HYPOTHESIS: yes** with Secure Boot on, if not on a Microsoft block policy. **HVCI compatibility is unverified** (Q3). |
| Recovery driver | dual: vendor + Microsoft attestation | **HYPOTHESIS: yes** |

- The two 2.70.6 builds have the same version number and the same import table; the installer ships
  both and picks one (**HYPOTHESIS**: by Secure Boot state). The user-mode DLLs come in matching sets.
- The service helper still carries a legacy path that turns Windows **test-signing on/off with bcdedit**
  and imports a bundled self-signed test certificate into the machine *Trusted Publishers* store.
  Not observed in use (no cert, test-signing off on the test PC).
- The installer actively **keeps memory integrity off**: a "required settings" component writes
  `DeviceGuard\EnableVirtualizationBasedSecurity` = 0 and
  `DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity\Enabled` = 0 (condition evaluated on the
  current state; **HYPOTHESIS**: written when it is already off so Windows does not switch it on later,
  plus an option to force it off when on). Secure Boot support in the changelog does not mean HVCI
  support.
- The control library reports Secure Boot, memory integrity, code-integrity options and CI policy
  state in its diagnostic report and has per-build gating values (max OS build, "check Secure Boot").

### 3.4 Boot safety

- Driver service values `LastBootSucceeded`, `LastBootShutdown`, `ignoreLastBoot`,
  `ignoreLastShutdown`: if the previous Windows session ended abnormally the product starts in a
  safe mode (seats not started) and the manager tells the user.
- A `chkLevel` bitmask selects which compatibility self-checks run before activation.
- The recovery driver (2.70.6) is a tiny boot driver that deletes the main driver's service values
  and devnode data (**HYPOTHESIS**: used to recover a PC that no longer boots with the filter active).
- The service helper has a reset action that wipes product state (per-adapter seat mapping values under
  `Control\Video`, per-device seat bits, audio endpoint seat state) and **HYPOTHESIS** an action that
  removes phantom devnodes.

---

## 4. Service helper actions (seat service executable, elevated, command-line)

Pulsar Seat needs no interop with these, so they are listed by function, not by name.

| Action | Effect |
| --- | --- |
| post-install | copy main driver; create/refresh driver service, root devnode, minifilter instance, seat service; reboot prompt |
| delete | remove both services, the devnode, filters, credential provider |
| disable | set both start types to 4 and unregister filters (used by setup before upgrade/uninstall) |
| reset | clear product runtime state in Windows keys (see 3.4) |
| arm-recovery-driver | install/arm the recovery driver (**HYPOTHESIS**) |
| test-signing-off | legacy: turn off test mode and remove the test certificate |
| service entry | normal service start |

Each action ends with a reboot prompt (uses the shutdown privilege).

---

## 5. Registry written by setup

| Key | Values |
| --- | --- |
| `HKLM\SOFTWARE\<vendor>\<product>` | `InstallPath`, `BuildString` (version string), `helpFile` (online guide URL); per-seat and runtime subkeys come later |
| driver service key | `InputDirectionSwitchKey` = 0x58, `DXTest` = 1; `allowservos` = 1 on Server SKUs |
| `HKLM\...\CurrentVersion\Run` | manager with an autostart switch (starts in tray) |
| `HKCU\SOFTWARE\<vendor>\<manager>\v2.0.0` | `locale` (UI language from setup language) |
| `DeviceGuard` keys | see 3.3 |
| `Session Manager\Power` | `HiberbootEnabled` = 0 (option) |
| Add/Remove Programs `{product-code}` | display name, version, publisher, install location/source, quiet + normal uninstall strings (1.2), size, TS-aware flag |
| Defender `Exclusions\Paths` | install folder (via the Defender PowerShell module, not raw registry) |
| `RunOnce` | see 2.3 |
| Licence component values | out of scope |

---

## 6. Uninstall

1. Confirm dialog.
2. Refuse if the product is enabled (1.3): the user must disable it in the manager, reboot, then
   uninstall.
3. Run the service helper with the disable / delete actions (services, devnode, filters, credential
   provider gone).
4. Remove files, shortcuts, Run entry, firewall rules, Defender exclusion, product registry.
5. Leaves behind: `C:\ProgramData\<product> Control\` data (configuration, reports, logs), per-user
   settings, `Control\Video` seat mapping values (observed on the test PC after disable), and the
   system tweaks (fast startup, dump type, power plan, DeviceGuard values). **HYPOTHESIS** for the
   exact leftovers after a full uninstall — only the disabled state was observed.
6. Reboot prompt.

---

## 7. Update channel

- No background updater, no scheduled task, no update service.
- The manager has a check-for-updates action that asks the vendor web service (endpoint list kept in
  the manager's per-user settings; an alternate endpoint can be injected by an environment variable
  through the shipped batch file). When a newer build exists it offers a download.
- When the running Windows build is newer than the product supports, the manager blocks activation and
  offers to find and download a compatible version; the browser downloads the setup; the user must
  disable the product, reboot and run the new setup (upgrade-in-place keeps the configuration).
- After an upgrade the manager detects the new version on first run, asks for admin rights to finish
  its post-upgrade migration, and asks for a reboot.
- The package carries a small version tag file (build/channel identifier).

---

## 8. Legacy loader in an old folder

A second, older program folder on the test PC holds an unsigned, packed x86 launcher dated 2019 from
the previous product generation. It is not shipped by the current installer and is ignored by this
spec. Pulsar Seat has no equivalent.

---

## 9. Differences 2.70.3 → 2.70.6

| Area | 2.70.3 | 2.70.6 |
| --- | --- | --- |
| Driver | one build, v76, vendor-signed only | two builds v78: "A" vendor-signed, "B" Microsoft-attestation-signed; one chosen at install |
| Control library / service logic DLL | v37 / v42 | v39 / v44 in both sets |
| App-facing helper DLLs (x86/x64) | as installed | v36 in set "A", **v32** in set "B" (set B ships older helpers) |
| Seat service executable, wizard | — | unchanged (same hashes) |
| Pre-flight | enabled check = seat service running | enabled check = driver running or start ≤ 2; CI-policy check on build ≥ 26100; ARM64 symbol defined |
| Prerequisites | VC++ runtime, wizard, compatibility helper (4th program) | same, compatibility helper is the 5th program |
| Files | — | adds multimedia plugin folder, recovery driver placed in `drivers`, a version tag file, more licence component files (out of scope) |
| Driver features | up to 12 seats | up to 16 seats (**HYPOTHESIS**); blocks the Windows Update engine while enabled; **HYPOTHESIS**: an option to ignore kernel-mode code-integrity status, and supported-build ceiling raised to 26500 (from changelog and strings; behaviour unverified) |
| Seat service | — | adds a pre-shutdown event |
| Diagnostics | — | report adds CI options, GPU scheduling mode, process list, per-seat info |
| Changelog-only | — | tray mode (2.70.4), Windows Update blocking (2.70.5), guidance for a driver-load error caused by a Windows 11 code-integrity policy (2.70.6) |

Unchanged: no INF, legacy service + root devnode install, minifilter altitude, Run-key autostart,
firewall/Defender steps (added in 2.70.1), memory-integrity "required settings", fast-startup option.

---

## 10. ARM64 notes

- Nothing in the reference product runs natively on ARM64. The x64 kernel driver **cannot load at
  all** on ARM64 Windows (there is no kernel-mode emulation). The x64 user-mode parts would only run
  under emulation and are useless without the driver.
- Windows fact: WDM, KMDF, UMDF and IddCx drivers can all be built for ARM64; WDM itself is not the
  blocker. **Project rule** (`docs/agents/rules.md`), not a Windows rule: Pulsar Seat drivers use
  KMDF/UMDF/IddCx only, preferring UMDF.
- An ARM64 installer must carry ARM64 builds of every binary (service, manager, drivers, helper DLLs).
  The app-facing helper DLL additionally needs x64 and x86 builds so emulated and 32-bit apps can
  load it (ARM64X / ARM64EC is worth considering for it).
- Anything loaded in-process by a native ARM64 system process must itself be native ARM64: the
  credential provider DLL (LogonUI is ARM64), any in-proc COM server, shell extension or audio/APO
  component.
- Driver signing: one catalog can cover several architectures (inf2cat with a list of OS/arch
  targets), and an attestation submission can contain a multi-architecture driver package. A
  separate catalog per architecture is allowed but not required.
- Architecture detection must use the native machine query (`IsWow64Process2` /
  `GetMachineTypeAttributes`), not the WOW64 environment variable, because the installer itself may
  run as x64 under emulation.
- The VC++ runtime prerequisite must be the ARM64 redistributable (or link the runtime statically).
- Power-plan GUIDs used by the reference (High performance) may not exist on ARM64 laptops (Modern
  Standby / S0 low-power idle); the sleep-off option must use the settings of the active plan instead.
- Check the PE machine type of every artifact in CI (0xAA64 / 0x8664), per `docs/agents/rules.md`.

---

## 11. Pulsar Seat approach

Target: one installer per architecture (x64, ARM64), or one bundle that picks by native machine type.
Must keep working with **Secure Boot on and memory integrity on**; never turns them off.

1. **Packaging.** MSI (WiX, GPL-compatible toolchain) or an equivalent signed setup; per-machine
   install under `%ProgramFiles%\Pulsar Seat\`; data under `%ProgramData%\Pulsar Seat\`. Uninstall
   entry with a quiet switch; standard 3010 exit code for "reboot needed". First-run wizard only on a
   fresh install; upgrades keep configuration.
2. **Drivers only as PnP driver packages with INF + CAT**, installed through the driver store
   (`DiInstallDriver` / `pnputil /add-driver /install`), never as raw legacy services, never copied
   by hand into `System32\drivers`.
   - Prefer UMDF 2 and user-mode solutions (raw input, HID ownership, CCD, audio policy API,
     IddCx for virtual displays). Any kernel component is KMDF, attestation- or WHQL-signed.
   - **Every kernel component must build natively for ARM64 in the same package and must pass the
     HVCI-compatibility checks** (no executable pool, no writable+executable sections, Driver Verifier
     and the WDK HVCI code-integrity checks) **before it ships**.
   - **Upper filters are device-level only**, declared in an INF (extension INF / per-device filter
     section), never class-wide and never by editing `Control\Class\{GUID}\UpperFilters` from a
     service (`00-architecture.md` §5.6, `50-input.md` §4.1), so a bug cannot take every keyboard
     on the PC down.
   - If a root-enumerated device is needed (e.g. a virtual bus / IddCx adapter), create it the
     documented way (`SwDeviceCreate`, or a root device installed from the INF) with a real hardware
     ID and a proper class — not a LegacyDriver placeholder.
   - A minifilter is **outside the rules.md driver list** (KMDF/UMDF/IddCx): an exception that needs
     the maintainer's explicit OK (`00-architecture.md` T5, `70-usb-devices.md` §5.3). Default is
     device security descriptors only. If approved: same ARM64 + HVCI conditions, a Microsoft-allocated
     altitude, install via INF.
3. **Never**: test-signing, importing certificates into Trusted Publishers/Root, writing DeviceGuard
   or CI policy values, disabling memory integrity, relying on the cross-signing "upgraded system"
   exemption, blocking Windows Update components, or `DenyDeviceClasses` policies that block driver
   updates silently. If a Windows component or policy blocks us, tell the user and stop.
4. **Pre-flight**: admin; supported Windows build range; native architecture (via the native machine
   query); report Secure Boot / HVCI / Smart App Control state for diagnostics only (never change it);
   refuse upgrade while seats are active and offer to stop them instead of making the user hunt for a
   toggle. **Coexistence checks** (`00-architecture.md` §3.3, P19): refuse when the reference product is
   installed, detected by its service keys `MUTESV_SERVICE` (seat service) and `MUTENX_SERVICE` (kernel
   driver) under `HKLM\SYSTEM\CurrentControlSet\Services` (interop identifiers, used only for this
   detection, nowhere else in Pulsar); refuse or warn when `TermService` has a non-default service DLL;
   warn about other multi-seat / input filters in the same stacks and never edit their entries.
5. **Enable/disable** is a service-side operation with a clear state machine, but it must not
   rewrite system-wide filter lists. Prefer enabling per-seat behaviour at runtime without reboot;
   where a reboot is unavoidable (driver install), say so once.
6. **Boot safety and recovery** (no separate "panic" kernel driver):
   - The service tracks unclean shutdowns and starts in a safe single-seat mode.
   - Online (normal boot or Safe Mode): `pnputil /delete-driver oem<N>.inf /uninstall /force`
     removes the package; the uninstaller does the same.
   - Offline from WinRE (pnputil works only on the running OS): either
     `dism /image:<OS drive>:\ /remove-driver /driver:oem<N>.inf`, or load the offline `SYSTEM` hive
     with `reg load`, set our filter/driver services' `Start` = 4 and remove our entry from any
     `UpperFilters` list, then `reg unload`. Document this procedure for users, with our exact
     service names and INF names, in the troubleshooting guide.
   - Our filters must fail open **before the service's first connect since boot**: devices then behave
     as on a PC without Pulsar Seat. After a connect, if the service dies, they drop other seats' input
     rather than leak it to seat 1 (`50-input.md` §4.1, `00-architecture.md` §5.7).
7. **System tweaks** (fast startup off, sleep off, small dumps, restore point) are optional,
   explicit, recorded in our own config, and reverted by uninstall when we changed them.
8. **Firewall/antivirus**: the manager talks to the service over a local named pipe, so **no
   firewall rules** are needed; do not add Defender exclusions (code-sign everything instead).
9. **Credential provider / early start** registration (if `30-sessions.md` needs them) are written by
   the installer, removed by the uninstaller, and documented. The credential provider ships as a
   native ARM64 DLL on ARM64.
10. **Updates**: GitHub Releases feed checked by the manager on demand (and optionally weekly), signed
    installer downloaded and verified (Authenticode + hash) before running; never silent driver
    swaps. Tauri updater is acceptable if it verifies signatures.
11. **Uninstall** removes everything it created, including driver packages from the driver store,
    services, registry, and offers to keep or delete `%ProgramData%\Pulsar Seat` configuration.
12. **Licensing**: none; fully free (GPL-3).

---

## 12. Open questions

1. Which exact condition selects driver build A vs B in 2.70.6 (Secure Boot only, or also memory
   integrity / OS build)? The selection condition is not readable statically.
2. Which policy is the Microsoft CI policy checked on 26100+ (vulnerable-driver blocklist? another
   Windows 11 driver policy?), and what does setup do with the result (block, warn, select B)?
3. Does the 2.70.6 attestation-signed driver actually load with memory integrity on, or does the
   "required settings" component still keep HVCI off? (Not testable without a system change.)
4. While enabled: exact start types (boot vs system for the driver); whether class `UpperFilters`
   entries are written at enable time; when the credential provider is registered (post-install vs
   enable); whether the seat service is added to `EarlyStartServices`; which class GUIDs land in
   `DenyDeviceClasses`. Observable only while enabled.
5. What the installer's own service-table steps (stop/pause/delete/install/start/resume) operate on —
   the recovery driver service, or nothing beyond the helper's registrations?
6. What a full uninstall leaves behind (only the disabled state was observed).
7. Upgrade path: does upgrade keep per-seat users/passwords and the device assignment JSON?
   (**HYPOTHESIS**: yes, they live in ProgramData / LSA.)
8. What the compatibility helper (1.3) checks exactly and how setup reacts to its result.
9. Does the cross-signed-only driver load with Secure Boot on on a clean (non-upgraded) Windows 11
   client install? (Expected no; needs a test machine that is not "upgraded".)
10. What setup does on an ARM64 host (refuse, warn or continue): the detection symbol exists but no
    using condition was found statically.
11. Pulsar Seat decision: MSI (WiX) vs Tauri's NSIS/MSI bundler for a package that also installs
    driver packages on both architectures; and whether one bundle or two per-arch installers.
12. Pulsar Seat decision: which signing route for any kernel/UMDF driver (attestation via an EV
    certificate — paid — vs UMDF-only designs; UMDF drivers still need a signed catalog to install
    without test mode).
