# 50 — Per-process shim and seat isolation

Behaviour of the reference product's per-process isolation layer: how its module gets
into processes, what it changes in them, and what each seat sees as a result. Evidence
markers as in `00-architecture.md` (**[S]** static, **[L]** live, **[I]** inferred,
**[?]** unknown). No code, no pseudo-code, no decompiled material. Identifiers are
quoted only where interop or collision avoidance needs them.

Every mechanism ends with a **Pulsar Split approach** subsection. Those are design
proposals for the implementer, not descriptions of the reference.

---

## 1. Scope and summary

The isolation layer has one job: processes in seat session N must behave as if seat N
were the only interactive session on the PC. Processes in the console session must
not see seat-owned input either. The reference does this with one user-mode DLL that
AppHelp (the Windows application-compatibility shim engine) loads into a fixed set of
system and user processes. Child processes inherit it through a compatibility layer.

| Area | Reference behaviour (short) | Section |
| --- | --- | --- |
| Injection | custom AppHelp database, one shim, module in `System32`/`SysWOW64`, machine-wide `__COMPAT_LAYER` | §2 |
| Seat lookup | session id of the process compared with each seat's `SessionId` in HKLM | §3 |
| HID isolation | opens of HID device paths are allowed or refused by device origin vs. caller session | §4 |
| XInput focus | controller-focus WNF publications are filtered per seat | §5 |
| Steam isolation | per-seat Steam IPC name, registry re-routing, a "Steam not running" answer, per-seat web-helper data | §6 |
| Hidden pad id | SDL/Steam told to ignore one Microsoft VID/PID | §7 |
| Remote-session masking | the active-console query and display-device enumeration are changed | §8 |
| Environment | per-seat variables set in-process and re-broadcast from the shell | §9 |
| DXVK redirect | legacy verifier path only, not in the 1.6 shim | §10 |
| Legacy verifier | cleanup only in 1.6 | §11 |
| Failure, logging, bitness | §12–§14 | |

## 2. Injection through AppHelp

### 2.1 Files and registration [S][L]

| Item | Value in the reference | Notes |
| --- | --- | --- |
| 64-bit module | shipped as `DuoAcRes64.dll`, copied to `C:\Windows\System32\AcRes.dll` | x64 PE, exports the two shim-engine entry points `GetHookAPIs` and `NotifyShims` [S] |
| 32-bit module | shipped as `DuoAcRes32.dll`, copied to `C:\Windows\SysWOW64\AcRes.dll` | x86 PE, same exports plus their stdcall-decorated forms [S] |
| Signing | Authenticode, vendor self-signed root that the installer imports machine-wide; status "valid" on the live box [S][L] | see `00-architecture.md` §8 item 4 |
| Database file | `C:\Windows\AppPatch\CustomSDB\{558f7556-3078-a346-a1fa-1b3b45a8b1ff}.sdb`, 2188 bytes on the live box [L] | installed by `sdbinst -q <temp file>`, removed by `sdbinst -u -g {GUID}` [S] |
| Registration | `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\InstalledSDB\{GUID}` (description = database name) and one `AppCompatFlags\Custom\<exe>` value per target [L] | written by `sdbinst`, not by the product itself |
| Machine variable | `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` value `__COMPAT_LAYER` = `LoadAppHelp` [S][L] | the manager treats the shim as installed only if the space-separated list contains `LoadAppHelp` [S] |
| Service re-check | at every service start the service copies both modules over the system copies again [S] | keeps the system copies at the shipped version |

"Installed" as shown by the manager = both module copies exist, the `.sdb` file exists,
and the machine variable contains the layer token [S].

### 2.2 Database layout [S][L]

The manager builds the database with the documented-in-practice `Sdb*` writer API of
`apphelp.dll`. Parsed from the live copy:

```
DATABASE
  OS_PLATFORM  = 5                       (DWORD)
  NAME         = <product> Application Compatibility Layer
  DATABASE_ID  = {558f7556-...}           (16 bytes)
  LIBRARY
    SHIM  NAME = Patches   DLLFILE = AcRes.dll
          INEXCLUDE { INCLUDE, MODULE = * }     -> hooks apply to all modules
  LAYER  NAME = Duo   SHIM_REF Patches          -> usable via __COMPAT_LAYER
  EXE  (one per target, 11 in total)
    NAME = <exe file name>   WILDCARD_NAME = <same>   APP_NAME = Targets
    EXE_ID = <new random GUID per build>   flag tag 0x1006 [?]
    MATCHING_FILE  NAME = *   [PRODUCT_NAME]   [FILE_DESCRIPTION]
    SHIM_REF Patches
```

### 2.3 Target list and matching [S][L]

| Target | Match attributes |
| --- | --- |
| `userinit.exe`, `explorer.exe`, `rundll32.exe`, `sihost.exe`, `cmd.exe`, `powershell.exe` (Windows PowerShell 5.1), `dwm.exe`, `GameInputSvc.exe`, `svchost.exe` | exe name, plus product name and file description **read from the real system file's version resource when the database is built** (for example "Desktop Window Manager") [S][L] |
| the product's own `sunshine.exe` | exe name only (the file has no version strings) [L] |
| `steam.exe` | exe name, product `Steam`, description `Steam` (fixed values) [S][L] |

- Matching is by file name and those version-resource attributes. The path is not
  part of the match [S].
- The database is rebuilt only when the operator toggles process patching on.
  On the live box the file date equals the last toggle [L]. Windows updates that
  change a system file's description could therefore break matching until the next
  toggle [I].
- The hidden *Application List* (`HKLM\SOFTWARE\Duo\ProcessPatchingTargets\<key>`,
  value `ImagePath`) uses as key the lowercase hex MD5 of the UTF-8 **file name**
  (not the path) [S]. In 1.6 the database builder does not read this list. The
  built-in list is fixed. So user-added entries have no effect [S] (bounded: the
  only database-build call site uses the fixed list).
- **Inheritance:** the database also defines a *layer* named `Duo` that contains the
  same shim [S]. The module rewrites `__COMPAT_LAYER` in the processes it is in, and it
  watches process creation (`NtCreateUserProcess`) [S]. The likely effect is that
  every descendant of a patched process (games started from Explorer, Steam, the
  stream server or a shell) also gets the shim through the layer [I]. The exact
  variable value the children receive is [?].
- **Exclusions:** the module refuses to install its hooks in the product's own service,
  headless RDP client and manager executables (matched by full path under the install
  folder) [S].

### 2.4 Lifecycle [S]

| Event | Effect |
| --- | --- |
| toggle on (manager) | copy modules → set machine variable → build temp database → `sdbinst -q` → delete temp file → `EnableProcessPatching`=1 → offer reboot |
| toggle off (manager) | delete both module copies → delete machine variable → `sdbinst -u -g` → `EnableProcessPatching`=0 → offer reboot |
| service start | re-copy modules (§2.1) |
| reboot | needed so long-running targets (dwm, svchost, explorer, userinit) start with the shim [S][I] |

Deleting a module copy that running processes still map fails at the file-system level.
The manager does not rename it or schedule a delete at reboot. The outcome of a failed
delete (error shown, or a partial state) is [?].

### 2.5 Pulsar Split approach

- Use its own database GUID, database name, shim name, layer name and module file
  name. The module name must not imitate a Windows file. A Windows-like name such as
  `AcRes.dll` hides the module from operators and AV triage.
- AppHelp loads a custom shim `DLLFILE` from the system directory only. Plain names
  resolve there. Placing the module in `System32` is therefore unavoidable for this
  mechanism. Document this and remove the module on uninstall.
- **Narrow the target set.** Do not target `svchost.exe` system-wide. The reference
  matches `svchost.exe` by name, so the module lands in every service host, including
  ones unrelated to input. Prefer, in order:
  1. Processes in seat sessions only. The module must quit early (no hooks) when the
     process is in session 0 or in the console session and no console-side rule
     applies. See §4.4 for the console-side rule.
  2. For the controller-focus service (§5), target only the specific service host,
     if one is required at all.
- Match targets by name **and** product name, as the reference does, but rebuild the
  database whenever a target file's version changes. Check at service start and
  compare against the stored attributes.
- Implement the user-added target list for real: key = SHA-256 of the lowercase file
  name, value = path, merged into the database at build time.
- Uninstall: rename loaded module copies to a unique name, then schedule them for
  delete at the next reboot (`MoveFileEx` with delay-until-reboot). Remove the database
  and the variable at once.
- **Signing:** an AppHelp shim module is an ordinary user-mode DLL. Memory Integrity
  (HVCI) enforces kernel-mode code integrity only, so it should not block the module
  [I]. **This is not yet verified live:** the reference box has HVCI off [L]. Smart App
  Control (when on) and WDAC user-mode policies *do* apply to DLL loads. An unsigned or
  self-signed module is then refused in every target, and the AV may flag custom shims.
  The reference box carries Defender exclusions for the module files [L], which
  suggests that it was flagged. Pulsar Split must not add AV exclusions (00 §8 item 8).
  It must sign the module with a publicly trusted code-signing certificate and must
  degrade cleanly when the module is blocked (§12). Protected (PPL) service hosts
  never load a non-Microsoft module [I]. That is one more reason not to target
  `svchost.exe`.
- Consider an **alternative to AppHelp** for seat processes the product launches
  itself (stream server, Steam, autostart apps): start them suspended and load the
  module with a documented mechanism, for example a launcher that sets up the process
  and then resumes it. AppHelp is then needed only for processes that the shell
  starts.

## 3. How the module learns its seat

### 3.1 Inputs [S]

| Input | Source |
| --- | --- |
| own session id | `ProcessIdToSessionId` for the current process |
| console session id | `RtlGetActiveConsoleId` (the real value, read before the module's own hook applies) [I] |
| seat list | `HKLM\SOFTWARE\Duo\Instances\*`, value `SessionId` (DWORD) of each seat key |
| feature switches | `HKLM\SOFTWARE\Duo` values `HostHidIsolation`, `SteamIsolation`, `Verbosity` |
| process identity | own image path (`GetModuleFileNameW`), token user SID, the session's interactive user SID |

### 3.2 Decision

| Process's session | Role it gets |
| --- | --- |
| equals the active console session | CONSOLE |
| equals the `SessionId` of a seat key | SEAT (that seat's name; the first matching key wins) |
| anything else (session 0, unknown sessions) | NONE [I] |

- The seat `Status` value (for example `Booting`) is not among the module's strings.
  The module does not check whether a seat is running. It trusts `SessionId` [S].
- `SessionId` values stay behind after a seat stops. The live box shows two stopped
  seats that both still hold `SessionId`=2 [L]. A later session that reuses id 2 would
  match the first seat key in enumeration order [I]. This is a real mis-attribution
  risk.
- The behaviour of role NONE is [?]. Session-0 service hosts (for example the game-input
  service) are in this role.
- Defaults when a switch value is absent: the live box lacks `HostHidIsolation` and
  `Verbosity`, and seats still worked there [L]. Whether absent means on or off for HID
  isolation is [?]. For `Verbosity`, absent behaves as "nothing logged": no module
  events exist in the log [L].

### 3.3 Pulsar Split approach

- The service publishes a small, **authoritative** session-to-seat map: one
  ACL-protected registry key (read: Everyone; write: SYSTEM). Each running seat has an
  entry with session id, seat id, seat user SID and a generation counter. The service
  deletes the entry when the seat stops, so no stale ids remain. The module reads only
  this map.
- Define every default explicitly. Absent switch = isolation **on**. Absent verbosity
  = errors only.
- Role NONE: install no hooks at all.

## 4. HID / controller isolation

### 4.1 What is intercepted [S]

| Path | Mechanism | Filtered? |
| --- | --- | --- |
| file-system opens of HID interface paths (`\??\hid#…`, `\\?\hid#…`) | `NtCreateFile` hook | yes; a refused open fails as access-denied [I] |
| brokered device access (WinRT / Windows.Gaming.Input style opens through `deviceaccess.dll`) | hook named for "open device from interface path" | yes [S name][I purpose] |
| dynamic look-ups of hooked functions | `LdrGetProcedureAddressForCaller` hook, so `GetProcAddress` callers also receive the filtered versions [I] | — |
| device enumeration (`SetupDi*`, `CM_Get_Device_Interface_List`) | not referenced | **no** — devices stay listed, only opening them fails [S bounded][I] |
| Raw Input device list / registration | not referenced | **no** [S bounded] |
| Windows.Gaming.Input / GameInput inside the game | through the brokered path above and through the game-input service target | partly [I] |

### 4.2 Device classification [S][L]

The module takes the device instance of the path being opened and walks up its parent
chain (`CM_Locate_DevNodeW`, `CM_Get_Parent`, `CM_Get_Device_IDW`) until an instance id
contains one of three tags:

| Tag in an ancestor's id | Typical device | Origin label |
| --- | --- | --- |
| `TERMINPUT_BUS` | Remote Desktop keyboard/mouse/HID devices that RDS creates per session; live instance ids embed the session, as in `TERMINPUT_BUS\UMB\…&SESSION<N>KEYBOARD0` / `…MOUSE0` [L] | remote |
| `DUOCONTROLLER` | the product's own virtual pads; live hardware id `HID\VID_054C&PID_0CE6&DUOCONTROLLER&<32-hex pad id>` [L] | remote (seat-owned) |
| `XBOXGIPSYNTHETIC` | HID devices that Windows synthesises for Xbox (GIP) controllers [I] | see below [?] |
| none of them | physical USB/Bluetooth devices on the PC | console |

The module has three labels for the device side: console, remote and unknown. It logs
each decision as *granted* or *denied* [S].

### 4.3 Access rule (observable behaviour)

| Caller role | console-origin device (physical pad) | seat-owned device (virtual pad, RDS input) |
| --- | --- | --- |
| CONSOLE | granted | denied [I] |
| SEAT | denied [I] | granted [I] |

- The rule depends only on console vs. remote. Nothing in the strings maps a specific
  virtual pad to a specific seat [S bounded]. Per-seat separation of virtual pads is
  most likely done elsewhere: the pad driver or the stream server binds each pad to its
  seat (the driver reads a `DuoControllerSessionId` value [S]; see the controller spec,
  row 60). A seat process could therefore open another seat's virtual pad through HID
  [I], but its XInput/focus traffic would not route there (§5).
- The `XBOXGIPSYNTHETIC` decision (console or remote) is [?]. No Xbox controller was
  present on the live box [L].
- Gate: `HostHidIsolation` [S]. The absent-value default is [?] (§3.2).

### 4.4 Pulsar Split approach

- **Primary mechanism: device security, not hooks.** For Pulsar Split's own virtual pads
  (row 60), the service sets each pad device's security descriptor when it creates the
  pad. Use the documented device-property route
  (`SetupDiSetDeviceRegistryProperty` with `SPDRP_SECURITY_SDS`, or the INF `Security`
  directive). The descriptor grants access to SYSTEM, the owning seat's user SID and
  Administrators only. This works for every API (HID, Raw Input, GameInput,
  Windows.Gaming.Input), needs no injection, and needs distinct Windows users per seat,
  which seats already have.
- Physical pads on the console: the same device-security route can scope them to the
  console user. Do this only as an opt-in, because it changes devices the product does
  not own. Restore the original descriptor when the service stops.
- RDS-created input devices already carry their session in the instance id. Classify
  them by that id rather than by walking parents.
- Keep a hook-based HID filter only as a fallback for the case where two seats share a
  Windows account. In that case, filter enumeration too (`SetupDi*` / `CM_*` interface
  lists and Raw Input), so that games do not see devices they cannot open. Many games
  treat an access-denied pad as broken.
- Note: an administrator can change device security. That is acceptable, because seat
  users should not be administrators (the reference makes them administrators,
  `00-architecture.md` §4; Pulsar Split should not).

## 5. XInput / controller focus

### 5.1 Reference behaviour [S]

- Windows tracks which process currently receives game-controller input with
  machine-wide WNF state that the shell publishes. That state covers a listener
  registration, a focus owner PID and an exclusive-focus flag. Because WNF is
  machine-wide, a focus change in one seat moves focus away from every other seat [I].
- The module hooks `RtlPublishWnfStateData`. It recognises three well-known state names
  with the `SHEL` owner tag (raw 64-bit values `0x0D83063EA3BC7875`,
  `0x0D83063EA3BC8075`, `0x0D83063EA3BC8875`; sequence numbers 15, 16, 17 after
  removing the well-known XOR mask) [S]. Its log messages cover three cases: a process
  registers a PID as a controller listener, sets the focus to a PID, or toggles
  exclusive focus [S]. Which constant belongs to which case is [?]. The public names
  of these states are [?]. They are probably the shell's game-controller
  listener/focus states [I].
- Observable effect intended: a pad that belongs to seat N drives only the foreground
  application of seat N, and the console keeps its own focus [I]. The decision rule is
  [?]: either publications from a seat are dropped, or they are rewritten so the
  focus never points at a PID in another session [I]. No live test was possible,
  because the service was stopped [L].
- The hooks matter in `explorer.exe`, `sihost.exe`, `dwm.exe` and the game-input
  service, which are all targets [S][I].

### 5.2 Pulsar Split approach

- First measure whether per-seat focus breaks at all once pads are device-scoped (§4.4)
  and XInput inside a seat sees only that seat's pads. If every seat's processes can
  open only their own pads, a machine-wide focus owner matters less [I].
- If filtering is still needed, filter in the seat sessions' shell processes only:
  suppress a focus publication whose target PID is in a different session. Never
  rewrite state on behalf of the console.
- Record the WNF names as identifiers only. Do not reuse the reference's raw-value
  list blindly: verify the values against the target Windows build at runtime and turn
  the feature off if they are unknown.

## 6. Steam isolation

Gate: `SteamIsolation` = 1 (live value 1) [S][L]. Applies to `steam.exe` and
`steamwebhelper.exe` processes in a seat session [S].

### 6.1 Reference behaviour

| Aspect | Behaviour |
| --- | --- |
| IPC name | Steam's master IPC name is made per seat. The module adds `-master_ipc_name_override <seat name>` to the command line that `GetCommandLineA/W` returns, and sets the variable `steam_master_ipc_name_override=<seat name>` [S][I format]. The seat app-list template also starts Steam with this override and the user chooser set to the seat name (`20-service.md` §3) [L]. Effect: a second Steam in a seat does not hand its arguments to the console's Steam. Each seat runs its own Steam client |
| Web helper | the web helper's `htmlcache` and `cef_log` locations are treated per seat, so two web helpers do not lock the same cache [S names][I] |
| Registry re-routing | value-level calls (`RegQueryValueEx`, `RegSetValueEx`, `RegDeleteValue`, A and W) on `Software\Valve\Steam` are sent to "the user's Steam key". The module compares the process token SID (special case `S-1-5-18`, SYSTEM) with the session's interactive user SID, and builds paths under that user's hive (`HKEY_USERS\<SID>`, `<SID>_Classes`, `Volatile Environment`) [S]. Effect: a Steam started in a seat with a SYSTEM token (for example by the stream server) still reads and writes the seat user's Steam settings, not `.DEFAULT`'s [I] |
| Named values | `TempAppPath`, `TempAppCmdLine` (Steam's hand-off of a pending launch) get special handling [S][?]. A query for `SteamPID` always returns 0 [S] |
| "Steam not running" | the 0 answer for `SteamPID` makes the Steam bootstrapper in a seat believe that no Steam is running, so it starts its own client and does not try to attach to another one [I] |
| URL handler | the module and the service both read `HKCR\steam\Shell\Open\Command` [S]. Purpose [?]: probably to recognise Steam launches through `steam://` URLs. On the live box the handler is the stock value [L] |
| Legacy add-on | at service start a `version.dll` in the Steam folder (an older "multibox" proxy) is deleted, and the service logs it [S]. Detection = the file is present. No content check was seen [S bounded]. Live box: no such file [L] |
| Helper executables | the service also knows Steam's helper executables (overlay UI, error reporters, system-info tool, in-home-streaming client, minidump writer) [S]. Their use is [?]: possibly they are closed when a seat stops |

### 6.2 Pulsar Split approach

- Prefer documented Steam command-line options over hooks. Start each seat's Steam
  from the service with the IPC-name override and a per-seat user chooser, as the
  reference app-list template already does. Start it **as the seat user** (not
  SYSTEM), so HKCU is already the seat user's hive and no registry re-routing is
  needed.
- Do not fake `SteamPID`. With distinct users per seat, every seat has its own HKCU
  and its own `SteamPID`. Faking is only a symptom of running Steam under the wrong
  token.
- Do not delete files in the Steam folder without asking. If a legacy proxy DLL is
  found, report it in the manager and let the operator remove it.
- Legal note: Steam's subscriber agreement and per-account limits apply to each seat.
  Running several clients on one PC is the operator's responsibility. Document this.

## 7. Hidden Microsoft device id for SDL / Steam

### 7.1 Reference behaviour [S]

The module sets two process environment variables:

| Variable | Value | Consumer |
| --- | --- | --- |
| `SDL_GAMECONTROLLER_IGNORE_DEVICES` | `0x045E/0x0B09` | SDL-based games (a standard SDL hint) |
| `STREAM_IGNORE_DEVICE_GUIDS` | `030092625e040000090b000000007701` | Steam's input stack [I] (identifier as shipped) |

The GUID decodes, in SDL's joystick-GUID layout, as bus USB, vendor 0x045E (Microsoft),
product 0x0B09, driver signature `w` (SDL's Windows.Gaming.Input backend) [S][I]. The
intent is to hide one Microsoft device as it appears through Windows.Gaming.Input,
which is most likely a duplicate or synthetic view of a pad that would otherwise show
up twice [I]. The exact device that carries 045E:0B09 is [?]. The live box has none [L].

### 7.2 Pulsar Split approach

Only add such hints after reproducing the duplicate-pad problem with Pulsar Split's own
pad stack. If the problem is needed, set the hints in the environment of seat
processes that the service launches. Do not set them through hooks. Keep the VID/PID
list in configuration rather than in built-in constants.

## 8. Remote-session masking

### 8.1 Reference behaviour

| Detection surface | Masked? |
| --- | --- |
| `RtlGetActiveConsoleId` (and `WTSGetActiveConsoleSessionId`, which reads the same value [I]) | **yes**: hooked [S]. Likely returns the caller's own session id, so "am I on the console?" checks pass in a seat [I] |
| display-device enumeration (`EnumDisplayDevicesW`) | **yes**: hooked [S]. Likely hides or renames the remote-display adapter strings that some games reject [I]. Exact change [?] |
| `GetSystemMetrics(SM_REMOTESESSION)` | **no**: not referenced [S bounded] |
| `WTSQuerySessionInformation` (client protocol, session name `RDP-Tcp#N`) | **no**: not referenced [S bounded] |
| `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server` values | **no** [S bounded] |

Targets: every process that carries the shim (§2.3), including descendants through the
layer [I]. The manager's install dialog advertises this as a fix for applications that
refuse to run in a remote session [S].

### 8.2 Pulsar Split approach

- The seat display is Pulsar Split's own IddCx monitor (row 40), not the RDP adapter.
  That alone removes the main display-name trigger. Measure first.
- Masking the console id inside system processes such as `dwm` and the game-input
  service changes OS decisions and is high risk. If masking is kept, limit it to
  user applications that run in seat sessions. Make it a per-application opt-in in the
  manager, off by default.
- Anti-cheat software may treat API masking as tampering. Document this for operators.

## 9. Environment variables and re-broadcast

### 9.1 Reference behaviour [S][I]

- The module sets per-seat variables in-process (`RtlSetEnvironmentVariable`): the Steam
  IPC override (§6), the SDL/Steam ignore lists (§7) and `__COMPAT_LAYER` (§2.3).
- When it runs inside `C:\Windows\explorer.exe`, it sends `WM_SETTINGCHANGE` with the
  parameter string `Environment` through `SendMessageTimeoutW`. Other top-level windows
  in that session then reload their environment, and processes started later from the
  shell inherit the variables [S][I].
- The service has a matching sub-command (`20-service.md` §1). It runs inside a seat
  session, sends the same broadcast, then waits with retries and a timeout until that
  session's taskbar process has seen it, and checks that the taskbar process belongs to
  the same session [S]. Trigger: seat start, after the session's profile and
  environment are prepared [I].
- The module also reads the session user's `Volatile Environment` key [S]. Purpose [?]:
  probably to build the user's real environment for processes that run with a
  different token.

### 9.2 Pulsar Split approach

Set per-seat variables in the seat user's environment before the seat's shell starts,
for example in `HKEY_USERS\<seat SID>\Environment` written by the service. Then send
one `WM_SETTINGCHANGE("Environment")` from a helper in the seat session. No
in-process variable injection is needed.

## 10. DXVK redirect (legacy path only)

### 10.1 Reference behaviour [S]

- The 1.6 shim module contains **no** DXVK, `d3d8` or `d3d9` references [S bounded].
  In 1.6 the redirect exists only in the legacy verifier modules (§11), which 1.6
  does not install. In practice the 1.6 product does not redirect Direct3D [I].
- In the verifier modules: file opens (`NtCreateFile` / `NtOpenFile`) of
  `C:\WINDOWS\SYSTEM32\d3d8.dll` and `…\d3d9.dll` are sent to
  `C:\Program Files\Duo\assets\dxvk\x64\` (64-bit) or `…\x32\` (32-bit). The 32-bit
  module also matches the `System32` spelling, because WOW64 file-system redirection
  applies later [S][I]. `DXVK_LOG_PATH` is set [S]. The value is [?].
- Only Direct3D 8 and 9 are redirected. The shipped folders also contain `d3d10core`,
  `d3d11` and `dxgi`, which are never redirected [S].
- No setting enables or disables this. It applies to every process the verifier loads
  into [S]. No per-application opt-in was seen.

### 10.2 Pulsar Split approach

Do not redirect system DLL loads. If DXVK is offered, use DXVK's documented deployment:
copy `d3d8.dll`/`d3d9.dll` next to the game executable, as a per-game opt-in in the
manager with an undo option. DXVK is zlib-licensed, so keep its notice. With Pulsar
Split's own IddCx display rendering on a real GPU, D3D9 in a seat may work without
DXVK. Test before adding the feature.

## 11. Legacy verifier path

### 11.1 Reference behaviour [S][L]

- Older versions injected `DuoVerifier64/32.dll` through the Application Verifier
  mechanism. They used per-executable IFEO values (`GlobalFlag`, `VerifierFlags`,
  `VerifierDlls`, `VerifierActivationFilter`), driven by
  `HKLM\SOFTWARE\Duo\VerifierApplications` and `VerifierUsesBlacklist`. A built-in
  exclusion list covered the product's own executables and several browsers,
  messengers, the phone-link host, the error reporter and a CPU-tuning tool. Some
  versions **replaced `System32\verifier.dll` and `SysWOW64\verifier.dll`** and kept
  `.bak` copies. These modules are unsigned and also handle packaged (AppX) apps [S].
- 1.6 does cleanup only. At service start it restores the `.bak` copies, if present,
  and removes the IFEO verifier entries [S]. Live box: genuine Microsoft
  `verifier.dll` in both folders, no `.bak`, no verifier IFEO entries [L].

### 11.2 Pulsar Split approach

**Not implemented.** Never write protected system files and never use verifier IFEO
injection. Optional: when a reference-product install is detected, the manager reports
leftover state (verifier IFEO entries, `.bak` files) and links to repair steps
(`sfc /scannow`). It does not repair them itself.

## 12. Failure and safety behaviour

### 12.1 Reference behaviour

| Situation | Behaviour |
| --- | --- |
| module missing or blocked at load | AppHelp skips the shim and the process runs without isolation [I]. No health signal reaches the manager [S bounded] |
| crash inside the module | crashes the host process. In `dwm.exe` that restarts the desktop compositor; in a critical service host it can stop the OS. The manager's confirmation dialog warns about this [S][I] |
| kill switch | none seen besides toggling process patching off, which needs the manager and a reboot [S bounded] |
| safe mode | no check seen [S bounded]. Whether AppHelp custom databases apply in safe mode is [?] |
| uninstall while loaded | see §2.4: no rename, no delete at reboot [S] |
| own executables | the product's own processes are excluded from hooks (§2.3) |

### 12.2 Pulsar Split approach

- **Kill switch:** a registry value (for example `ShimDisabled`=1) that the module reads
  first and that makes it install no hooks. It can be set offline through the Windows
  Recovery Environment (`reg load`). Document the recovery steps.
- **Safe mode:** in safe boot the module installs no hooks.
- **Crash guard:** the module counts its own faults per target. After repeated faults
  in a target, the service turns that target off and the manager shows why.
- **Health:** each loaded module reports "loaded, version, role, hooks on" over the
  service named pipe (no HTTP, see `plan.md`), so the manager can show which seats are
  actually isolated.
- Never target system-critical processes without a fallback. Prefer the
  device-security and launcher approaches (§2.5, §4.4, §6.2), which need no injection.

## 13. Logging

### 13.1 Reference behaviour [S][L]

- Event source `DuoAcRes` in the Application log (message file = .NET's generic one).
  The module registers it itself if it is missing [S]. It also writes `DbgPrint` output
  [S].
- Level from `HKLM\SOFTWARE\Duo\Verbosity` (0–4 = nothing, error, warning,
  information, debug) [S]. HID decisions, focus publications, Steam re-routing and
  command-line rewrites are logged at verbose levels [S][I].
- Live box: `Verbosity` absent and no `DuoAcRes` events [L].
- A version value `DuoAcResVersion` (inner version 1.5.7.0, file version 1.6.0.0) is
  written somewhere [S]. The location is [?].

### 13.2 Pulsar Split approach

Use an ETW provider (TraceLogging) for the module rather than the event log, because
per-device decisions are high-volume. Write only errors to the event log, under
Pulsar Split's own source name. Log no SIDs or user names above debug level.

## 14. Bitness and architecture

| Host / process | Reference | Pulsar Split |
| --- | --- | --- |
| x64 host, 64-bit process | `System32` module (x64) [S] | same |
| x64 host, 32-bit process | `SysWOW64` module (x86) [S] | same |
| ARM64 host | no ARM64 or ARM64EC module shipped [S]. An x64 module cannot load into native ARM64 processes (shell, dwm, service hosts) [I] | out of scope for Phase 4 (the multi-seat host is x64). If added later: an ARM64X module (native ARM64 + ARM64EC) in `System32` and the x86 module in `SysWOW64` |

## 15. Coexistence

If the reference product's database GUID `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}` is
installed, or a module of the reference type sits in `System32\AcRes.dll`, Pulsar Split
must not install its own shim for the same targets. Two isolation layers would make
conflicting decisions. The manager tells the operator to turn off the other product's
process patching first (`00-architecture.md` §10).

## 16. Open questions

| # | Question | How to resolve |
| --- | --- | --- |
| 1 | Behaviour for role NONE (session 0 service hosts, including the game-input service) [?] | runtime trace with the service running |
| 2 | Default when `HostHidIsolation` is absent [?] | live HID open test from a seat with the value absent |
| 3 | Access decision for `XBOXGIPSYNTHETIC` devices [?] | live test with an Xbox pad |
| 4 | Mapping of the three WNF constants to listener / focus / exclusive, their public names, and the filter rule (drop vs. rewrite) [?] | WNF tracing in a seat |
| 5 | Exact `__COMPAT_LAYER` value given to children; whether every descendant gets the layer [?] | read a child process's environment in a running seat |
| 6 | Exact command-line format of the IPC override and the per-seat `htmlcache` / `cef_log` paths [?] | read the command lines of the seat's Steam processes |
| 7 | Handling of `TempAppPath` / `TempAppCmdLine`; purpose of the `steam` URL-handler read [?] | runtime trace |
| 8 | What the display-enumeration hook changes [?] | compare enumeration output inside and outside the shim in a seat |
| 9 | Which device has 045E:0B09 [?] | public USB id lists / a live device with an Xbox pad |
| 10 | Whether the shim loads with Memory Integrity **on**, with Smart App Control on, and under Defender ASR rules without exclusions [?] — the live box has HVCI off and SAC off [L] | test on a box with HVCI on; needs maintainer approval (system change) |
| 11 | Whether AppHelp custom databases apply in safe mode [?] | test on a VM |
| 12 | Outcome when toggle-off cannot delete a loaded module [?] | test |
| 13 | Location of the `DuoAcResVersion` value; `DXVK_LOG_PATH` value [?] | static follow-up |
| 14 | Purpose of the Steam helper-executable list and the `XInput` string in the service [?] | static follow-up / runtime |
| 15 | Behaviour of `Moonlight.exe` (named in the module) [?] | static follow-up |
