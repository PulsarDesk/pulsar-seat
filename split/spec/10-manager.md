# 10 — Manager (configuration GUI)

Behaviour of the reference product's manager application, as a basis for the Pulsar
Split manager (Tauri 2 + SvelteKit, Turkish UI copy). Evidence markers as in
`00-architecture.md` ([S] static, [L] live, [I] inference, [?] unknown).

Labels below are the reference product's short English labels, given only so that the
implementer can map controls; Pulsar Split writes its own Turkish copy. Long messages
are paraphrased, never quoted.

---

## 1. General behaviour

- **Single instance.** A machine-wide named mutex guards the app; a second launch shows
  a short "already open" message box and exits [S].
- **Elevation.** Every setting is written directly to `HKLM\SOFTWARE\<product>` and the
  app starts/stops a service, installs a shim database and changes a service start
  type, so it needs administrator rights [S]. (The reference manifest does not request
  elevation; Pulsar Split should request it explicitly, or better: let the manager talk
  to the seat service over an authenticated local IPC and let the service do all
  privileged writes.)
- **TLS.** The app disables certificate validation for its HTTP client because the
  seats' stream servers use self-signed certificates [S]. Pulsar Split should pin the
  seat's own certificate instead.
- **Persistence model.** There is no "Save" button. Every field writes its registry
  value immediately when changed (delete old value, write new one) [S]. Lists (seats,
  autostart apps, hidden lists) are registry subkeys; adding an item creates a subkey,
  removing deletes it [S].
- **Configuration lock.** While the service is running, almost every configuration
  control is disabled and its tooltip explains that changes are blocked by the running
  service [S]. Only the service controls, seat start/stop, open-web-UI and pair actions
  stay usable. Rationale [I]: the service reads configuration once at start.
- **Polling.** A timer refreshes the service state (SCM status), the service's HTTP
  endpoint reachability, and all command enable states every few seconds [S]. While a
  seat page is visible, two more timers poll that seat's running state (service API)
  and its stream-server reachability (`/serverinfo`) [S]; they stop when another page
  is selected [S].
- **Theme.** Windows 11 style (Fluent, light/dark following the system) [S].

## 2. Main window

Default size 800 × 600 [S]. Custom title bar containing the app icon and
"<product> Manager <major.minor.build>" [S]. Double-click on the title bar maximises /
restores; dragging from the maximised state restores and keeps the cursor proportional
on the bar [S].

```
+--------------------------------------------------------------------------+
| [icon] Product Manager 1.6.0                              [_] [□] [X]    |
+-------------------+------------------------------------------------------+
| [ supporter btn ] |                                                      |
| (+) (▶) (■) (🌐)  |                                                      |
|  New Start Stop   |         <content frame: selected page>               |
|        WebUI      |                                                      |
|-------------------|                                                      |
| (◯) seat A        |                                                      |
| (◯) seat B        |                                                      |
| (◯) seat C        |                                                      |
|                   |                                                      |
|                   |                                                      |
| ⚙ Settings        |                                                      |
+-------------------+------------------------------------------------------+
```

### 2.1 Navigation pane

- Left pane, open width 210 px, compact width 60 px, always visible; no back button [S].
  When the pane collapses, the four header buttons are laid out vertically instead of
  horizontally [S].
- **Pane header**
  - Supporter button (label "Unlock Supporter Benefits"), visible only for unlicensed
    users → licensing flow (§9). **Not in Pulsar Split.**
  - Four icon buttons with labels:

| Label | Icon | Enabled when | Action | Tooltip when disabled |
| --- | --- | --- | --- | --- |
| New | + | service stopped **and** (licensed **or** zero seats exist) | New-seat dialog (§5.1) | service running → "config blocked"; free + one seat → "free users limited to one" |
| Start / Restart | ▶ | SCM state is exactly *Stopped* or *Running* (not pending) | if stopped: start the service via SCM and wait (bounded) for *Running*; if running: label reads "Restart" and the app calls the service's `/restart` endpoint, then waits for *Running* | "service is between states" |
| Stop | ■ | SCM state *Running* | stop via SCM and wait for *Stopped* | "service isn't running" |
| WebUI | globe | an HTTP GET to `http://127.0.0.1:<global port>/` succeeds | open that URL in the default browser (the service's status page) | "service isn't running" |

  Each start/stop/restart shows a modal progress dialog with a spinner and a title
  ("Starting …", "Restarting …", "Stopping …") until done [S].
  In Pulsar Split, the licence condition on **New** is removed (unlimited seats).

- **Menu items.** One item per seat (registry subkey of `Instances`), showing the seat's
  key name; icon = the Windows account picture of the seat's user (shell API for the
  user's picture path), clipped to a circle of radius 24; falls back to a generic guest
  picture when no user or no picture [S]. Selecting an item opens the seat page (§4)
  with polling started.
- **Settings item** at the bottom (standard navigation-view settings entry) → Settings
  page (§3). Settings is selected at launch [S]. After a seat is created, the new seat
  is selected automatically (deferred by one UI tick) [S]; after a seat is deleted the
  selection returns to Settings [S].
- **Supporter badge.** A small orange "PRO" badge next to the title when licensed [S] —
  **not in Pulsar Split.**

## 3. Settings page (global)

```
Settings
────────────────────────────────────────────
Windows Settings
  [toggle] Start with Windows

Patch Settings                       (disabled while service runs)
  [toggle] Isolate HID devices
  [toggle] Enable Process Patching   (confirm + reboot flow, §3.2)
  [toggle] Isolate Steam             (enabled only if Process Patching is on)

Web User Interface Settings          (disabled while service runs)
  Web User Interface Port  [ 8088      ]   (port validation)

Log Settings                         (disabled while service runs)
  Verbosity  [ Information ▾ ]  Nothing / Error / Warning / Information / Debug
```

| Control | Stored as | Default | Meaning / effect |
| --- | --- | --- | --- |
| Start with Windows | the service's own SCM `Start` value (`HKLM\SYSTEM\CurrentControlSet\Services\<service>\Start`): on = automatic (2), off = manual (3) [S][I] | off (live: manual [L]) | service starts at boot |
| Isolate HID devices | `HostHidIsolation` DWORD 0/1 [S] | [?] (absent on live box) | the injected shim hides seat-owned virtual pads from the console session and console-owned devices from seats (see shim spec) |
| Enable Process Patching | `EnableProcessPatching` DWORD 0/1 + install/remove of the shim database and system env var (§3.2) [S][L] | off; the toggle's displayed state is re-derived at launch from whether the shim is really installed (files present, database present, env var contains the layer) [S] | turns the per-process shim on/off machine-wide |
| Isolate Steam | `SteamIsolation` DWORD 0/1 [S] | [?] (live: 1) | per-seat Steam instance (IPC name override, per-user Steam registry) — needs process patching |
| Web User Interface Port | `Port` DWORD [S] | 38299 when the value is missing [S]; 8088 observed [L]; validation 1029–65514 | port of the service's HTTP control API / status page |
| Verbosity | `Verbosity` DWORD, index 0–4 = Nothing, Error, Warning, Information, Debug [S] | [?] (absent on live box; seat configs then carry log level "none") [L][I] | log level of service, shim, IDD and the per-seat stream server |

Hidden/unused in 1.6 (code exists, no control on the page) [S]: a value
`RestartGPUsOnSandboxShutdown`, sandbox memory (`SandboxMemory`, % of RAM, live value
50) and storage (`SandboxStorage`) sliders, sandbox storage location, sandbox/zygote
reset buttons, links to "Application List", "Shared Folders" and "Forwarded Ports"
sub-pages. See §8.

### 3.1 Validation rule "port"

Text must parse as a 16-bit unsigned number in the range **1029–65514** inclusive,
otherwise the field shows the error state with a "please enter a valid port" style
message and the value is not stored [S]. (Range keeps a Sunshine port block of
−5…+21 inside 1024–65535.)

### 3.2 Enable Process Patching flow

1. User flips the toggle. The toggle is immediately reverted visually (so it reflects
   reality until the operation finishes) [S].
2. Confirmation dialog, title depends on direction (installing / removing), primary
   button "Install …" / "Remove …", secondary "Cancel" [S]. Body:
   - install: three bullets — improves HID isolation, enables Steam isolation, fixes
     applications that refuse to run in a remote session; question whether to install;
     a bold red warning that compatibility layers sit in Windows' process-loading path
     and can break the whole system; "Caution is advised" in large red text [S].
   - remove: the same three bullets negated (degrades isolation, disables Steam
     isolation, breaks apps that refuse remote sessions) and a question [S].
3. On confirm: progress dialog ("Installing/Removing … layer …") while a background
   task runs, minimum visible ≈ 3 s [S]:
   - **install:** copy the 64-bit shim DLL to `System32\AcRes.dll` and the 32-bit one
     to `SysWOW64\AcRes.dll`; set the machine environment variable `__COMPAT_LAYER` to
     `LoadAppHelp`; generate a shim database in the temp folder (database name, fixed
     GUID, one library with one shim named `Patches` → module `AcRes.dll`, one layer,
     and one EXE entry per target with "match any" file, optional product/description
     match for Steam) and install it with `sdbinst -q`; delete the temp file [S].
     Built-in targets: `userinit.exe`, `explorer.exe`, `rundll32.exe`, `sihost.exe`,
     `cmd.exe`, Windows PowerShell, `dwm.exe`, `GameInputSvc.exe`, `svchost.exe`, the
     product's own `sunshine.exe`, and `steam.exe` (product "Steam") [S][L].
   - **remove:** delete both `AcRes.dll` copies, delete `__COMPAT_LAYER`, uninstall the
     database by GUID with `sdbinst -u -g` [S].
   - store `EnableProcessPatching` accordingly and set the toggle to the new state.
4. "Reboot required" dialog: explanation that a reboot is needed to take full effect;
   buttons "Reboot now" (runs an immediate restart) and "Reboot manually later" [S].

Pulsar Split: same flow (own GUID, own DLL name, own shim name), but never name the DLL
after a Windows system file and never write into `System32` unless unavoidable (AppHelp
shim modules must live in the system directories — document this).

## 4. Seat page

Title = seat key name (fallback "Instance") [S].

```
seatA
────────────────────────────────────────────────────────────────
Actions
 (▶) Start Instance  (■) Stop Instance  (🗑) Delete Instance
 (🌐) Open Sunshine   (👁) Pair New Client

Instance Settings                     (disabled while service runs)
  Display Name  [ seatA                ]
  Port          [ 47989                ]
  [toggle] Start with Service
  [ Autostart Applications ]          (hidden for sandboxed seats)

User Settings                         (disabled while service runs)
  User Name     [ alice            ▾ ]
  Password      [ ••••••••      👁 ]
  [ Test User Credentials ]

Display Settings                      (disabled while service runs)
  Render Adapter  [ NVIDIA GeForce …  ▾ ]
  Desktop Scale   [ Dynamic ▾ ]   Dynamic / Static
                  [====|==========] 100 %   (enabled only when Static)
  Super-Sampling Factor [==|======] 100 %
  SDR White-Level       [====|====] 400 nits
  [toggle] Force SDR in HDR Streams
```

### 4.1 Actions

| Label | Enabled when | Action |
| --- | --- | --- |
| Start Instance | service API reachable **and** seat reported not running | progress dialog "Starting <seat>…"; `GET http://127.0.0.1:<global port>/instances/<seat>/start`; close dialog [S] |
| Stop Instance | service API reachable **and** seat reported running | same with `/stop` [S] |
| Delete Instance | service stopped | confirmation dialog naming the seat, primary button "Delete", body asks for confirmation and warns in bold red that it cannot be undone; on confirm deletes the seat's registry subkey tree and refreshes the list [S] |
| Open Sunshine | the seat's stream server answered `/serverinfo` | open the seat stream server's web UI in the browser: host = `127.0.0.1` (or, for sandboxed seats, the VM address from `SandboxIp`), port derived from the seat port (upstream rule: web UI = port + 1) [S][I] |
| Pair New Client | same as Open Sunshine | pair dialog (§5.3) [S] |

Disabled tooltips: "service isn't running", "instance already running", "instance
isn't running", "config blocked by running service", "stream server isn't running" [S].

The seat's *running* flag comes from `GET /instances/<seat>` answering `true`/`false`
(trimmed text); an HTTP failure marks the service API unreachable [S].

### 4.2 Fields

| Label | Registry value (under `Instances\<seat>`) | Type | Default (missing value) | Validation / notes |
| --- | --- | --- | --- | --- |
| Display Name | `DisplayName` | string | the seat key name (set at creation) | not empty, no leading/trailing whitespace [S]; becomes the stream server's advertised host name (`sunshine_name`) [L] |
| Port | `Port` | DWORD | random in 10000–65534 at creation [S] | port rule 1029–65514 [S]; base port of the seat's stream server |
| Start with Service | `Enabled` | DWORD 0/1 | 0 when missing; set at creation [S] | seat is started automatically when the service starts |
| (Sandboxed) | `Sandboxed` | DWORD 0/1 | 0 | no visible control in 1.6 [S]; hides Autostart button when on |
| (Hide VM suffix) | `HideVMSuffix` | DWORD 0/1 | 0 | no visible control [S] |
| Autostart Applications | subkeys of `AutoStartApplications` | — | none | navigates to §4.3 |
| User Name | `UserName` | string | empty | drop-down of local, **enabled** user accounts (SAM account names) of the machine [S]; changing it refreshes the seat's avatar icon |
| Password | `Password` | string, encrypted (AES, base64; key derived from an application constant) [S] | empty | password box with "peek" reveal; mouse wheel over the box scrolls the page [S] |
| Test User Credentials | — | — | — | enabled when a user name is chosen; flow §4.4 |
| Render Adapter | `RenderAdapter` | string = adapter description text | at creation: the hardware adapter with the most dedicated video memory [S][I] | drop-down of DXGI adapters (software adapters excluded, duplicates by LUID or description removed) [S]; stored by *description*, so two identical GPUs are indistinguishable (Pulsar Split: store the LUID or PnP instance id as well) |
| Desktop Scale | `IsScaleFactorStatic` | DWORD 0/1 | 0 = Dynamic | Dynamic = follow the client; Static = use the slider value |
| scale slider | `ScaleFactor` | DWORD percent | 100 | 100–500 %, shown with a % thumb label; enabled only when Static [S] |
| Super-Sampling Factor | `SuperSamplingFactor` | DWORD percent | 100 | 100–500 %; render the virtual monitor larger than the stream and downscale [S][I]; licence-gated in the reference |
| SDR White-Level | `SDRWhiteLevel` | DWORD nits | 400 when missing [S] (live boxes show 80 and 0 [L]) | 80–480 nits slider; licence-gated |
| Force SDR in HDR Streams | `ForceSDR` | DWORD 0/1 | 0 | stream SDR even when the client asks for HDR; licence-gated ("can't alter the virtual monitor image format") |

Runtime values that also live in the seat key but are not shown: `SessionId`, `Width`,
`Height`, `RefreshRate`, `Status`, `KeyboardLayoutOverride` [S][L].

### 4.3 Autostart Applications sub-page

```
seatA  >  Autostart Applications
────────────────────────────────────────────
[ Add Application ]                 (disabled while service runs)

  C:\Games\Tool\tool.exe
  Arguments [ --minimized          ]   [toggle: Run as user | Run as system]  [Remove]
  ...
```

- Breadcrumb: clicking the seat name navigates back to the seat page [S].
- **Add Application:** file-open dialog (filter: executables, all files), a title naming the seat, starting in a well-known folder; creates subkey
  `Instances\<seat>\AutoStartApplications\<new GUID>` with `ApplicationPath` = chosen
  file, `Arguments` = empty, `RunAsSystem` = 0 [S].
- Per item: read-only path header, editable `Arguments` (string), toggle `RunAsSystem`
  (off = run as the seat user, on = run as SYSTEM inside the seat session), Remove
  (deletes the subkey) [S]. Changes are written immediately.

### 4.4 Test User Credentials flow

1. A modal result dialog opens showing a pending state and a dismiss button [S].
2. Background check [S]:
   - validate the user name/password with an interactive logon attempt against the
     local machine;
   - look the account up; if it is **disabled**, enable it and save; if it is **locked
     out**, unlock it; then validate again.
3. Result: a success title with a short confirmation, or the Windows error text as
   title with a hint to fix the input [S].

Note: enabling/unlocking accounts as a side effect of a "test" button is surprising;
Pulsar Split should ask first.

## 5. Dialogs

### 5.1 New seat

```
+---------------------------------+
| New Instance                    |
| Name [______________________]   |
|                                 |
|            [Create]  [Cancel]   |
+---------------------------------+
```

- Single text field "Name", max 128 chars; Create is enabled only while the text matches
  ASCII letters and digits only (no spaces, no other characters) [S].
- On Create [S]: new subkey `Instances\<name>` with `DisplayName` = name, `Enabled` set,
  `ScaleFactor` = 100, `SuperSamplingFactor` = 100, `Port` = random 10000–65534,
  `RenderAdapter` = best adapter (above). User and password stay empty. The list
  refreshes, the new seat gets selected, and the "New" button's state re-evaluates.
- Collision of names or ports with existing seats is **not checked** [?/I]; Pulsar
  Split must reject duplicate names (case-insensitive) and overlapping port blocks
  (each seat uses base−5 … base+21).

### 5.2 Delete seat

Confirmation, see §4.1.

### 5.3 Pair New Client

```
+--------------------------------------------------------------+
| Pair Instructions for seatA                                  |
| step 1: start the client app                                 |
| step 2: choose "add host"                                    |
| step 3: type this address                                    |
|    Instance URI [ 192.168.1.20:47989 ]   (read-only)         |
| step 4: select the new host to begin pairing                 |
| step 5: press the button, sign in, type the PIN              |
|                      [Enter PIN]  [Close Instructions]       |
+--------------------------------------------------------------+
```

- *Instance URI* = `<first LAN IPv4>:<seat port>` [S]. The LAN address is chosen as the
  first interface that is up, is not a Hyper-V virtual adapter (MAC prefixes
  `00-15-5D`, `00-03-FF`), has a default gateway, and has a non-loopback IPv4 address
  in a private range (10/8, 172/…, 192.168/16); fallback `127.0.0.1` [S].
- "Enter PIN" opens the seat stream server's PIN page (`https://<host>:<web port>/pin`)
  in the browser and keeps the dialog open [S].
- Pulsar Split improvement: show all candidate addresses, a QR code, and accept the PIN
  inside the manager (call the stream server's pin API) so no browser login is needed.

### 5.4 Common dialogs

- **Progress:** title + indeterminate spinner, no buttons [S].
- **Notification:** title + wrapped centred message + one or two buttons [S].
- **Confirmation:** title + custom content + primary button + Cancel [S].

## 6. Error/status messages (meaning only)

| Situation | Meaning shown to the user |
| --- | --- |
| second manager launch | the manager is already open |
| controls disabled, service running | configuration changes are blocked while the service runs |
| service pending state | service is between states; wait |
| service not running | the service (or its HTTP API) is not reachable |
| seat already running / not running | start/stop not applicable |
| stream server not running | the seat's stream server does not answer, so web UI/pairing unavailable |
| no user selected | choose a user before testing credentials |
| invalid display name | must not be empty or have leading/trailing spaces |
| invalid seat name | must be non-empty ASCII letters/digits only |
| invalid port | must be a number in the allowed range |
| invalid mount point (sandbox) | a drive letter other than C:, or a `C:\` path with at least one folder, no trailing backslash |
| credential test failed | Windows' own logon error text |
| sandbox unavailable | needs Windows 11 23H2+, the VirtualMachinePlatform feature, and (reference only) a licence |
| licence-gated control | free users can't use this feature (reference only) |

## 7. What each manager action changes in the system

| Action | Registry | Files | Services / processes |
| --- | --- | --- | --- |
| New seat | create `Instances\<name>` + defaults | — | — |
| Edit any seat field | write that value | — | — |
| Delete seat | delete `Instances\<name>` tree | — (per-seat stream-server config/state files in `config\` are left behind [L][I]) | — |
| Add/remove autostart app | create/delete subkey | — | — |
| Test credentials | — | — | logon attempt; may enable/unlock the Windows account |
| Start with Windows | service `Start` value | — | — |
| Process patching on/off | `EnableProcessPatching`, `__COMPAT_LAYER`, AppHelp database registration | `System32\AcRes.dll`, `SysWOW64\AcRes.dll`, `AppPatch\CustomSDB\{GUID}.sdb` | `sdbinst`; optional reboot |
| Other global toggles | `HostHidIsolation`, `SteamIsolation`, `Port`, `Verbosity` | — | — |
| Start / Stop / Restart | — | — | SCM start/stop; restart via HTTP `/restart` |
| Seat start/stop | — | — | HTTP to service |
| Open web UI / Pair | — | — | default browser |

## 8. Hidden / unreachable features in 1.6 (documented for completeness)

Code and page layouts exist, but no visible navigation leads to them [S]:

- **Application List** (`ProcessPatchingTargets`): extra executables for the shim.
  "Add Applications" multi-select file dialog; each entry stored under a subkey named by
  the lowercase hex MD5 of the file name, value `ImagePath`; list shows icon, file name,
  read-only path, Remove.
- **Sandbox → Shared Folders** (`SharedDirectories\<GUID>`: `HostDirectory`,
  `SandboxDirectory`, `ReadOnly`), mount-point validation, default mount `C:\HostDrive`
  when the host folder is a drive root.
- **Sandbox → Forwarded Ports** (`ForwardedPorts\<GUID>`: `Description`, `Protocol`
  0=TCP/1=UDP, `HostPort`, `SandboxPort`), shown as "host to sandbox" pairs.
- Sandbox storage location (moves `install.wim`, `language.cab`, `zygote.vhdx`,
  `sandbox.vhdx` to a chosen folder, stores `SandboxStorageLocation`; default under the
  program's `config\sandbox`), reset sandbox (delete `sandbox.vhdx`), reset zygote
  (delete both VHDX files).

Pulsar Split: out of scope for Phases 1–4.

## 9. Licence / payment features (reference only — NOT in Pulsar Split)

Described only so the implementer knows what to leave out [S][L]:

- Supporter button → choose platform (Patreon or Ko-fi).
  - Patreon: embedded browser (incognito, cookies cleared, context menu disabled) runs an
    OAuth authorisation-code + PKCE flow with a localhost redirect; the app then reads
    the member's lifetime support amount and compares it with a minimum.
  - Ko-fi: e-mail dialog (format validation) → the vendor's web endpoint sends a 6-char
    lowercase alphanumeric verification code → code dialog → the endpoint verifies and
    returns a licence.
- The licence is stored encrypted in the registry, bound to the motherboard serial
  number and machine SID, and read by the service and the display module too.
- Gated features: more than one seat, supersampling, SDR white level, force-SDR, HDR,
  refresh rates above the free cap (supporters up to 500 Hz), sandbox seats.
- A thank-you / "not enough yet" message dialog lists the benefits.

Pulsar Split: no licensing code, no network calls to any vendor, no gating; all features
available to everyone.

## 10. Pulsar Split manager — recommended screen set (analyst suggestion)

Same information architecture, Turkish copy, plus: seat status column (session id,
stream server up/down, connected client), a log viewer, conflict detection (port,
name, user already used by another seat, reference product installed), and a setup
check page (Windows edition, RDS state, IDD state, signing state) with explanations of
the licensing implications from `00-architecture.md` §8.
