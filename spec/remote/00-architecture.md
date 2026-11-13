# 00 — Architecture of the reference product

Behaviour spec of the multi-seat host that Pulsar Split must be compatible with
("the reference product", version 1.6.0.0). Written by the analyst from static
inspection of the shipped files and read-only observation of one installed system.
No code, no decompiled material. See [../CLEANROOM.md](../../CLEANROOM.md).

Evidence markers used below:

- **[S]** observed statically in the shipped files (names, strings, imports, resources).
- **[L]** observed on the live reference system (registry, services, devices, event log).
- **[I]** analyst inference from S/L evidence; treat as likely, not certain.
- **[?]** unknown; needs runtime capture or a design decision of our own.

Pulsar Split does **not** need to be binary-compatible with the reference product's
internal interfaces (shared-memory layouts, private registry values, internal HTTP
API). The compatibility target is the *external* behaviour: what a Moonlight/Pulsar
client sees, what the operator can configure, and what happens to the Windows system.
Internal names are recorded so the implementer understands the moving parts and can
avoid collisions when both products are installed (see "Coexistence" at the end).

---

## 1. Big picture

One Windows PC hosts several **seats** (the reference product calls them
*instances*). Each seat is:

1. a local Windows user account (one per seat, chosen by the operator),
2. a separate interactive Windows session for that user, created *headlessly* by
   making a loopback Remote Desktop connection to the PC itself,
3. a virtual monitor inside that session (the RDP indirect display, augmented so it
   can take arbitrary resolution / refresh rate / HDR and render on a chosen GPU),
4. one dedicated game-stream server process (a Sunshine fork) running inside that
   session on its own port block, which Moonlight/Pulsar clients pair with,
5. per-seat isolation of controllers / HID devices / Steam so that seats do not see
   each other's input, plus per-seat virtual gamepads.

The physical console session (session 1) stays usable by whoever sits at the PC.

```
            Moonlight/Pulsar client A        client B            client C
                     |                          |                   |
          ports P_A-5..P_A+21           P_B-5..P_B+21         P_C-5..P_C+21
                     |                          |                   |
+--------------------|--------------------------|-------------------|---------+
| Windows host       v                          v                   v         |
|  session 2 (user a)         session 3 (user b)        session 4 (user c)    |
|  +----------------+         +----------------+        +----------------+    |
|  | stream server  |         | stream server  |        | stream server  |    |
|  | (Sunshine fork)|         | (Sunshine fork)|        | (Sunshine fork)|    |
|  +-------^--------+         +-------^--------+        +-------^--------+    |
|          | frames (shared section)  |                          |             |
|  +-------+--------+         +-------+--------+        +-------+--------+    |
|  | virtual monitor|         | virtual monitor|        | virtual monitor|    |
|  | (RDP IDD +hook)|         | (RDP IDD +hook)|        | (RDP IDD +hook)|    |
|  +----------------+         +----------------+        +----------------+    |
|          ^ session created by loopback RDP (headless client, per seat)      |
|          |                                                                   |
|  session 0: seat service (LocalSystem) -- HTTP control API on 127.0.0.1    |
|            Remote Desktop Services (termsrv, wrapped/patched)               |
|  session 1: physical console user + manager GUI                              |
+-----------------------------------------------------------------------------+
```

## 2. Component inventory

| # | Shipped file | Kind | Runs as / where | Started by | Role |
| --- | --- | --- | --- | --- | --- |
| 1 | `DuoManager.exe` | .NET Framework 4 WPF desktop app (ModernWpf UI, Costura-embedded deps, WebView2 only for an OAuth dialog) [S] | interactive user, console session; manifest says "as invoker" but it writes HKLM and controls a service, so in practice it must run elevated [S][I] | user (Start menu) | configuration GUI; see `10-manager.md` |
| 2 | `Duo.exe` | native x64 Win32 service + command-line helper [S] | service `DuoService`, display name `Duo`, own process, LocalSystem, session 0 [L] | SCM (manual or automatic start) | seat lifecycle, system preparation, HTTP control API, watchdog; see `20-service.md` |
| 3 | `sunshine.exe` | fork of the GPL-3 Sunshine game-stream host, built by the vendor [S] | one process per running seat, inside the seat's session [S][I] | the service | streams the seat's virtual monitor, audio and input to Moonlight; adds a private frame source (capture method id `ddx`) and a private virtual-gamepad backend [S] |
| 4 | `DuoRdp.exe` | headless RDP client: PyInstaller one-file bundle of Python 3.13 + the MIT-licensed `aardwolf` RDP library [S] | one per seat while the seat session is being created/held [I] | the service | connects to the local RDP listener (127.0.0.1, port read from the RDP-Tcp WinStation) with the seat user's credentials and a client name equal to the seat name, so Windows creates a real interactive session; video output is discarded [S][I] |
| 5 | `TermWrap.dll` | ServiceDll wrapper for Remote Desktop Services [S] | inside the TermService svchost | Windows (registry redirect) | loads the real `termsrv.dll` and patches it in memory to allow multiple concurrent sessions on a client SKU, several sessions per user, non-RDP stacks, multi-monitor and app-server mode (same technique family as the open-source "RDP Wrapper") [S][L] |
| 6 | `DuoRds.dll` | COM in-proc server implementing the Windows Remote Desktop Services *protocol provider* interfaces (manager / listener / connection / license connection) [S] | would run inside TermService | — | alternative session-creation path with its own shared-memory IPC; **not registered** on the observed system, so 1.6 appears to use the loopback-RDP path instead [L][I] |
| 7 | `DuoIdd.dll` | UMDF module that substitutes for Microsoft's RDP indirect display driver (`RdpIdd`) [S] | inside the WUDF host that serves each RDP session's display adapter | Windows, after the service swaps the RdpIdd service image path [S][I] | loads the genuine `RdpIdd.dll`, hooks the IddCx callbacks and the RdpIdd swap-chain processing, and (a) forces the mode requested for the seat (width, height, refresh, scale, HDR luminance, SDR white level), (b) pins the render GPU, (c) copies every frame into a per-session shared section for the stream server [S] |
| 8 | `RdpIdd.dll` | Microsoft binary, "Rdp Indirect Display" 10.0.22621.3672 [S] | — | DuoIdd | fallback copy of the genuine driver when the DriverStore copy is unusable [S][I] |
| 9 | `rfxvmt.dll` | Microsoft binary, "RemoteFX VM Transport" 10.0.15063.0 [S]; also present in System32 on the live box [L] | — | — | transport for the (hidden) sandbox/VM seat mode [I] |
| 10 | `DuoAcRes64.dll`, `DuoAcRes32.dll` | AppHelp shim module (exports the shim-engine entry points) [S] | injected into every process listed in a custom shim database, in every session | Windows loader via AppHelp | per-process isolation hooks: HID device filtering, XInput focus, Steam isolation and registry redirection, command-line and environment rewriting, remote-session masking [S]; see shim spec (pending) |
| 11 | `DuoVerifier64.dll`, `DuoVerifier32.dll` | Application Verifier provider DLLs [S] | injected through Image-File-Execution-Options verifier settings | Windows loader | older/alternative injection path with similar Steam-isolation and DXVK-redirect logic; no verifier IFEO entries present on the live box [L][I] |
| 12 | `DuoBluetooth.dll` | ServiceDll wrapper for the per-user `BluetoothUserService` [S][L] | inside each user's Bluetooth user-service host | Windows (registry redirect) | lets the real Bluetooth user service run only in the active console session and terminates it elsewhere, so remote seats do not grab Bluetooth controllers [S][I] |
| 13 | `DuoController\DuoController.dll` + INF/CAT | UMDF 2.15 HID minidriver, root-enumerated, own device class [S][L] | WUDF host, one device node per virtual pad | the stream server through exported functions | virtual DualSense / DualSense Edge / DualShock 4 / Xbox One Elite pads with full HID reports (triggers, haptics, LEDs); per-pad shared-memory IPC [S][L]; see controller spec (pending) |
| 14 | `assets\drivers\ViGEmBus\*`, `assets\drivers\nefconw.exe` | third-party Nefarius ViGEmBus kernel driver (WHQL-signed) and its CLI installer [S][L] | kernel | installer | standard virtual Xbox 360 / DS4 pads (fallback) |
| 15 | `assets\dxvk\x32|x64\*.dll` | DXVK (zlib licence) Direct3D 8/9/10/11 → Vulkan translation DLLs [S] | in game processes | the injected shim/verifier, by path redirection of `d3d8.dll` / `d3d9.dll` | lets old D3D8/9 games render in a remote session on the seat GPU [S][I] |
| 16 | `assets\web\duo\*` | static files (HTML helper JS, CSS, icons) for the service's own status page [S] | served by the service | — | browser dashboard of the HTTP control API |
| 17 | `assets\web\*` (rest), `assets\shaders\*`, `assets\apps.json`, PNGs | Sunshine's normal web UI and shaders, plus default app list and icons [S] | — | sunshine | standard Sunshine assets |
| 18 | embedded in `Duo.exe` | HDR enabler (kernel-level; method deliberately not specified) [S][L] | kernel | the service at start | see §8 item 3 — **out of scope, not reproduced** |
| 19 | `unins000.exe/.dat` | Inno Setup installer/uninstaller [S] | — | — | install actions, §6 |
| 20 | `msvcp140/vcruntime140/ucrtbase/zlib1.dll`, `runtimes\win-x64\native\WebView2Loader.dll` | redistributable runtimes [S] | — | — | — |

## 3. Who launches whom

```
SCM ──> Duo.exe (service, session 0, LocalSystem)
          ├─ (start) prepares the system: privileges, RDS registry, TermService wrapper,
          │          Bluetooth wrapper, IDD swap, shim files, firewall rule, Defender
          │          exclusion, legacy cleanup, [HDR enabler, out of scope]         (§6, 20-service)
          ├─ HTTP control server on 127.0.0.1:<global Port>                (§5.1)
          └─ for every seat marked "start with service":
               ├─ Duo.exe --CreateHomeDirectory ...   (first logon prep of the user profile) [S][I]
               ├─ DuoRdp.exe <ip> <port> <client name=seat> <user> <password> <layout> <w> <h>
               │     └─ Windows RDS creates session N for the seat user (ClientName = seat)
               ├─ wait for session N to appear; record SessionId in the seat's registry key
               ├─ sunshine.exe config\<seat>.conf   (in session N, token duplicated) [S]
               │     └─ loads DuoController.dll on demand for virtual pads
               ├─ per-seat autostart applications (as SYSTEM or as the seat user) in session N
               └─ watchdog: if the stream server exits, start it again (observed ~every 2 s
                  when it kept crashing) [L]
Manager (console session, elevated)
          ├─ ServiceController: start / stop / restart DuoService, read status
          ├─ HTTP GET to 127.0.0.1:<global Port>/...          (seat start/stop/status)
          ├─ HTTP GET http://<ip>:<seat port>/serverinfo       (is the seat's stream server up)
          └─ opens https://<ip>:<seat port+1>/ (Sunshine web UI) and /pin in the browser [I]
Windows loader ── AcRes.dll shim (custom SDB) into the listed processes, every session
TermService svchost ── TermWrap.dll ── termsrv.dll (patched in memory)
WUDF host per RDP session display ── DuoIdd.dll ── RdpIdd.dll (genuine)
```

Exact port arithmetic for the stream server follows upstream Sunshine: the configured
`port` is the HTTP pairing port; HTTPS is `port-5`, the web UI `port+1`, RTSP `port+21`,
video/control/audio UDP `port+9..+11` (upstream rule; the fork is assumed unchanged) [I].
This is why the manager only accepts seat ports 1029–65514 (port-5 ≥ 1024 and
port+21 ≤ 65535) [S].

## 4. Sessions and privileges

| Actor | Session | Account / token | Why |
| --- | --- | --- | --- |
| seat service | 0 | LocalSystem; enables SeTcb, SeDebug, SeShutdown, SeRestore, SeBackup, SeTakeOwnership [S] | create processes in other sessions, load user hives, edit SAM-protected group membership, restart services, patch registry ACLs |
| headless RDP client | 0 [I] | service | only a network client of the local RDP listener |
| seat session | N ≥ 2 | the seat's local user; session created through RDP, so Windows treats it as a *remote* session [L] | |
| stream server | N | duplicated service (SYSTEM) token with session id set to N [S][I] — same model as upstream Sunshine's service mode | needs desktop duplication / input injection rights in the seat session |
| autostart apps | N | per app: SYSTEM token in N, or the seat user's token [S] | |
| manager | 1 (console) | interactive admin user | writes HKLM, installs shim database, controls service |
| shim / verifier DLLs | every session | host process | per-process hooks |

Observed on the live system [L]: seat users are local accounts that are members of
both *Administrators* and *Remote Desktop Users*; `qwinsta` with the service stopped
shows only services (0), console (1) and the RDP listener. The service adds the seat
user to *Remote Desktop Users* itself if missing (error message exists for failure) [S].

## 5. Communication channels

### 5.1 Service HTTP control API (plain HTTP, no authentication) [S]

> Pulsar Split: reference behaviour only. Per `docs/agents/plan.md`, Pulsar Split has no
> HTTP API; the same operations go over the ACL'd named pipe.

Listens on the port stored in `HKLM\SOFTWARE\Duo` value `Port` (DWORD; default 38299 when absent [S], observed 8088
[L]); the manager talks to it via `127.0.0.1`. Responses are plain text. All verbs are
HTTP GET.

| Path | Response | Effect |
| --- | --- | --- |
| `/` | HTML dashboard: product/version header, host name, one row per seat with its name (linking to the seat's stream-server web UI), Start and Stop buttons, a "restart service" button, a message box | none |
| `/static/<file>` | files from `assets\web\duo` (`script.js`, `style.css`, icons) | none |
| `/instances/<name>` | `true` if the seat is running, `false` otherwise | none |
| `/instances/<name>/start` | human-readable message ("… has been started", or "… doesn't exist", or failure) | starts the seat |
| `/instances/<name>/stop` | message | stops the seat |
| `/restart` | message ("restarting the service") | the service restarts itself |
| unknown action | message saying the action is unknown | none |

The dashboard script polls `/instances/<name>` for each row every 5 s, enables Start or
Stop accordingly, and pauses polling for 5 s after a button press while showing the
returned message. Whether the server binds to loopback only or to all interfaces is
[?]; the installer adds an inbound firewall allow rule for `Duo.exe` on all profiles
[L], so LAN exposure is likely. **Pulsar Split must bind to loopback only and must
authenticate local callers** (named pipe with an ACL is preferred over HTTP).

### 5.2 Registry as shared state

`HKLM\SOFTWARE\Duo` is the single configuration store, written by the manager and read
by the service, the shim DLLs, DuoIdd and the controller driver. Full key list in
`20-service.md` §2. Runtime values written by the service (or the stream server) under
`Instances\<seat>`: `SessionId`, `Width`, `Height`, `RefreshRate`, `Status` (e.g.
`Booting`) [S][L]; read by DuoIdd to configure the virtual monitor and by the shim to
map "which seat am I in" (by comparing the current session id) [S].
Additional global values read by DuoIdd: `MinLuminance`, `MaxLuminance`,
`MaxFullFrameLuminance` (HDR metadata) [S].

### 5.3 Named kernel objects

| Name (verbatim, needed only to avoid collisions) | Type | Creator → consumer | Purpose |
| --- | --- | --- | --- |
| `Global\DuoManager` | mutex | manager | single-instance guard; second launch shows a message and exits [S] |
| `Global\DuoIdd<sessionId>` | file mapping | DuoIdd → stream server | latest frame of the seat's virtual monitor [S] |
| `Global\DuoIdd<sessionId>FrameReady` | event | DuoIdd → stream server | a new frame is in the section [S] |
| `Global\DuoIdd<sessionId>FrameEncoded` | event | stream server → DuoIdd | frame consumed, buffer may be reused [S] |
| `Global\Duo_<padId>_input`, `…_input_event` | mapping + event | stream server → DuoController | HID input report for one virtual pad [S] |
| `Global\Duo_<padId>_output`, `…_output_event` | mapping + event | DuoController → stream server | HID output report (rumble, adaptive triggers, LEDs, audio-haptics) [S] |
| `Global\Duo_RDS_Config`, `Global\Duo_RDS_Output` | mappings | service ↔ DuoRds | connection request (client name, resolution, keyboard layout, colour depth) and result for the unused protocol-provider path [S] |
| `Global\Duo_RDS_RequestEvent`, `…ResponseEvent`, `…StopEvent` | events | service ↔ DuoRds | handshake for the above [S] |
| `Global\TermSrvReadyEvent` | event (Windows-owned) | TermService → service | wait until RDS is ready before creating sessions [S] |
| WNF state names | WNF | shim | XInput focus/listener state is published through WNF; the shim intercepts publication to keep focus per seat [S][I] |

Pad ids are 32-hex-digit strings [S]. The controller sections use the security
descriptor "authenticated users read/write, administrators full" [S]. Byte layouts of
all sections are [?] (not recoverable from strings; Pulsar Split defines its own).

### 5.4 Stream-server conventions (per seat)

Each seat's stream server is upstream-Sunshine-compatible towards clients: HTTP
`/serverinfo` on the seat port (the manager probes it every few seconds to show the
seat as online), HTTPS pairing with PIN entry through the stream server's own web UI
(`/pin` page). The fork reads a few extra config keys (see `20-service.md` §4).

## 6. System changes made by install and by service start

### 6.1 Installer (Inno Setup script functions visible in the uninstall log) [S]

- copies the program into `C:\Program Files\Duo` (hard-coded path in several binaries),
- checks for / downloads and installs the WebView2 runtime,
- adds Microsoft Defender exclusions,
- imports a code-signing certificate into machine certificate stores (needed for the
  self-signed driver and DLLs, §8),
- runs DISM to add a package / enable optional features and "container support"
  (Hyper-V / VirtualMachinePlatform for the sandbox mode), gated on Windows build,
- registers the service, creates inbound firewall rules named `Duo` (for `Duo.exe`) and
  `Duo Sunshine` (for the bundled `sunshine.exe`), all protocols, all profiles [L],
- installs ViGEmBus (shown in Programs as a separate MSI product) and the
  DuoController driver package (class `DuoController`, signer = vendor's own
  certificate) [L].

### 6.2 Service start ("boot" phase) — system-wide changes [S][L]

| Area | Change |
| --- | --- |
| privileges | enable the six privileges listed in §4 |
| event log | register sources `Duo`, `DuoIdd`, `DuoAcRes` under the Application log (message file = .NET's generic one) [L] |
| Defender | add exclusion for the program folder via PowerShell; live box also has exclusions for `AcRes.dll`, `verifier.dll`, `DuoIdd.dll` and a kernel driver file [L] |
| firewall | enable the built-in "Remote Desktop" rules for TermService on 3389 [S][L] |
| Remote Desktop Services | registry: allow connections, no single-session-per-user, concurrent sessions enabled, multiple TS sessions allowed, blank-password restriction off, RemoteFX/WDDM IDD policies on, hardware encode preferences, compression/quality policy, GPU real-time priority, clipboard and drag-drop redirector add-ins changed, svchost split threshold raised, IDD stall termination off, remote keyboard layout ignored, Hiberboot (fast startup) off, controller-to-VK mapping off, a set of FeatureManagement overrides, cross-device resume disabled [S][L] |
| TermService | point its `ServiceDll` to `TermWrap.dll`; restart TermService (and its dependents); refuse if the local `termsrv.dll` version is unsupported [S][L] |
| BluetoothUserService | point its `ServiceDll` to `DuoBluetooth.dll` [S][L] |
| display | copy `DuoIdd.dll` to `System32\drivers\UMDF`, then point the RdpIdd UMDF service image path at it when a seat starts ("toggle on") and back afterwards [S][L] — on the live box with the service stopped the path pointed back at the genuine DriverStore copy [L] |
| shim | ensure `System32\AcRes.dll` / `SysWOW64\AcRes.dll` are the vendor's shim builds; clean up legacy `verifier.dll` replacement (restores `.bak` copies) and legacy verifier IFEO entries [S][L] |
| Steam | uninstall a legacy "multibox" add-on (a `version.dll` proxy in the Steam folder) [S] |
| HDR | HDR enabler (§8 item 3; out of scope) when Memory Integrity is off [S][L] |

On service stop it removes virtual controller device nodes and their INF [S], and the
per-seat state (sessions, stream servers) is torn down.

### 6.3 Manager-driven system changes

- "Enable process patching" toggle installs/removes the shim database (§7.3) and
  sets/clears the system environment variable `__COMPAT_LAYER=LoadAppHelp`; asks for a
  reboot [S].
- "Start with Windows" toggles the service start type between automatic and manual by
  writing the service's `Start` value [S].

## 7. Windows mechanisms relied on

### 7.1 Multi-session on a client SKU

- Windows 10/11 *Pro* allows one interactive session at a time. The reference product
  lifts this by (a) RDS registry settings and (b) in-memory patching of `termsrv.dll`
  from a ServiceDll wrapper (TermWrap), version-gated so unknown `termsrv.dll` builds
  are refused [S]. The patch method is out of scope; see `30-session-rds.md` for the
  decision gate (Pulsar Split builds the Windows Server + RDSH path first).
- Observed SKU: Windows 11 Pro (registry product name still says "Windows 10 Pro"),
  build 26300, edition `Professional` [L].

### 7.2 Session creation

- Loopback RDP connection per seat with NTLM credentials of the seat user; client name
  = seat name; requested resolution = seat resolution; keyboard layout passed through
  [S]. The service then finds the session by enumerating WTS sessions and matching the
  client name to the seat name [S].
- An alternative WRDS protocol-provider implementation exists but is unused (§2 #6).

### 7.3 Per-process injection (shim)

- A custom AppHelp database (`.sdb`) with GUID `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}`
  defines one shim named
  `Patches` whose module is `AcRes.dll`, applied to: `userinit.exe`, `explorer.exe`,
  `rundll32.exe`, `sihost.exe`, `cmd.exe`, `powershell.exe`, `dwm.exe`,
  `GameInputSvc.exe`, `svchost.exe`, the bundled `sunshine.exe`, and `steam.exe` [S][L].
  It is installed with `sdbinst` (quiet) and shows up in "Programs" as an uninstallable
  entry [L]. The manager also has a (hidden) list of extra target executables
  (`ProcessPatchingTargets`) [S].
- Because the shim DLL is loaded into `svchost.exe` and `dwm.exe` system-wide, a bug in
  it affects the whole OS; the manager warns about this [S].
- Shim behaviours [S]: per-session HID access decisions for virtual-pad device classes
  (`XBOXGIPSYNTHETIC`, `DUOCONTROLLER`, `TERMINPUT_BUS`) by console vs. remote session;
  XInput focus/listener control; Steam isolation (per-seat IPC name override, user-hive
  redirection of Steam's HKCU keys, faking the running-Steam PID); hiding a specific
  Microsoft device id from SDL/Steam; masking "remote session" detection; re-broadcasting
  environment changes. Details belong to the shim spec.

### 7.4 Virtual display

- Uses Windows' own RDP Indirect Display Driver (one display adapter per RDP session,
  device ids `SWD\REMOTEDISPLAYENUM\RDPIDD_INDIRECTDISPLAY&SESSIONID_000N` [L]) and
  hooks it from a substituted UMDF module; frames are handed to the stream server
  through shared memory instead of being RDP-encoded [S].
- Relies on the IddCx 1.9+/1.10 callbacks for HDR (FP16) on Windows 11 23H2+ [S][I].

### 7.5 Input / controllers

- Own UMDF HID minidriver for PlayStation/Xbox-Elite pads (root-enumerated devices
  `ROOT\VID_054C&PID_0CE6` etc., hardware ids tagged `DUOCONTROLLER` plus a per-pad id)
  [S][L]; ViGEmBus as an additional bus [L].
- Keyboard/mouse/touch/pen come from upstream Sunshine (SendInput, synthetic pointer
  devices) running inside the seat session [S].

### 7.6 Audio

- Upstream Sunshine audio with the virtual sink forced to the RDP session's audio
  endpoint (`virtual_sink = Remote Audio`) and Steam audio-driver installation disabled
  [S][L].

## 8. Legal, licensing and safety concerns (factual)

1. **Multi-session on Windows client editions.** Running several concurrent interactive
   sessions on Windows 10/11 Pro is not permitted by the Windows client licence terms;
   Microsoft licenses this through Windows Server + RDS CALs or Azure Virtual Desktop.
   Patching `termsrv.dll` in memory (TermWrap) is the same approach as RDP Wrapper.
   Pulsar Split must document this risk to users and should keep the termsrv-patching
   piece isolated, optional and clearly labelled; a Windows Server host path (no
   patching needed) should be supported first-class.
2. **Redistributed Microsoft binaries.** The package ships `RdpIdd.dll` and `rfxvmt.dll`
   copied from Windows builds. Redistribution of Windows system files is not allowed by
   the Windows licence. Pulsar Split must use the copies already on the system and never
   ship them.
3. **HDR on seat displays (user-visible behaviour only).** In the reference product a
   seat's virtual monitor can expose HDR to the stream (10-bit, PQ / Rec. 2020) on
   Windows 11 23H2 and later; it is a supporter-only feature there, can be suppressed
   per seat with *Force SDR*, and SDR content brightness is set with *SDR white level*.
   The reference reaches HDR availability on RDP-session displays through a kernel-level
   modification performed by the service at start, and only when Memory Integrity is
   off [S][L]. That mechanism is **out of scope** for Pulsar Split (maintainer decision:
   no vulnerable drivers, no kernel patching, no DSE/HVCI changes) and is deliberately
   not specified here. Pulsar Split delivers HDR only through the supported IddCx HDR /
   wide-colour-gamut path of its own indirect display driver, on the Windows versions
   that provide it, with Memory Integrity left on; where that path is unavailable the
   seat is SDR-only. See the display/HDR spec row in `README.md`.
4. **Self-signed code.** `TermWrap.dll`, `DuoBluetooth.dll`, the controller driver and
   other binaries are signed with a certificate whose subject and issuer are the vendor
   itself (no public CA), timestamped by DigiCert [S]; the installer imports it into the
   machine trust stores [S]. Trusting a vendor root machine-wide is a significant
   security grant. Pulsar Split should not install a private root CA.
5. **Driver signing requirements for Pulsar Split.**
   - UMDF drivers (virtual HID pad, an own IDD) on x64 must be signed with a certificate
     that chains to a trusted root for PnP installation; for distribution to end users
     without manual trust steps they need Microsoft attestation signing (EV certificate
     + Partner Center) or WHQL. Test-signing mode is acceptable only for development.
   - Kernel drivers (if ever needed, e.g. a virtual audio device or bus) require Microsoft
     attestation/WHQL signing; Secure Boot systems refuse anything else. Prefer reusing
     signed third-party drivers (ViGEmBus — note it is end-of-life upstream; Virtual
     Display Driver / other open IDDs under compatible licences) where licences allow.
6. **GPL-3 obligations for the Sunshine fork.** The bundled stream server is a modified
   GPL-3 program; corresponding source must be offered by its distributor. Pulsar Split
   may use upstream Sunshine (or its forks such as Apollo) under GPL-3 and must ship its
   own modifications as source.
7. **Third-party components** in the package (DXVK — zlib; aardwolf — MIT; ViGEmBus —
   BSD-3; ModernWpf, ControlzEx, Newtonsoft.Json, Costura — MIT) are permissively
   licensed and can be used by Pulsar Split with notices, if needed.
8. **Defender exclusions and blanket firewall rules** weaken host security; Pulsar Split
   should add only narrow, port-scoped firewall rules and no AV exclusions by default.
9. **Unauthenticated control API** (§5.1) lets anyone who can reach the port start/stop
   seats; do not replicate.
10. **Stored seat passwords.** The manager stores each seat user's Windows password in
    the registry, AES-encrypted with an application-wide constant-derived key [S] (so it
    is effectively reversible by anyone with read access to HKLM). Pulsar Split should
    store credentials with DPAPI (machine scope, ACL'd to SYSTEM and Administrators) or
    via LSA private data.

## 9. Features present in the reference product that Pulsar Split will not carry

| Feature | What it is | Pulsar Split |
| --- | --- | --- |
| supporter licensing | Patreon OAuth (browser dialog, PKCE, local redirect on port 38299) or Ko-fi e-mail verification code against the vendor's web endpoint; result stored as an encrypted `License` value bound to the motherboard serial number and the machine SID; free users limited to one seat, no HDR, no >60 Hz-class refresh (up to 500 Hz for supporters), no supersampling, no SDR-white/format controls, no sandbox [S][L] | **not implemented — every feature is free** |
| HDR via kernel modification | §8 item 3 | not implemented; HDR via supported IddCx path instead |
| sandbox / VM seats | seats running inside a Hyper-V VM ("zygote" base disk, per-sandbox disk, `install.wim`, language CAB, shared folders, port forwarding, VM memory/storage sliders, GPU restart on VM shutdown) gated on Windows 11 23H2+ and the VirtualMachinePlatform feature; UI pages exist in code but are not reachable from the 1.6 UI [S] | out of scope for Phase 1–4; maybe later |

## 10. Coexistence with an installed reference product

Pulsar keeps support for hosts that run the reference product. Pulsar Split must not
fight it. Therefore Pulsar Split must use its own: service name, registry root,
kernel-object names, shim database GUID, event-log sources, install folder, firewall
rule names and device class GUIDs. On start it should detect the reference product
(service `DuoService` present / running, `TermService` ServiceDll redirected, the shim
database GUID installed) and refuse to modify shared Windows settings while the other
product is active, telling the operator why.
