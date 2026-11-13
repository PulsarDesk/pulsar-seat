# 20 — Seat service

Behaviour of the reference product's background service (seat lifecycle, configuration
files, registry, ports, users, sessions, error handling). Evidence markers as in
`00-architecture.md`. No code; prose and tables only.

---

## 1. Identity and process model

| Item | Reference behaviour |
| --- | --- |
| Service name / display name | `DuoService` / `Duo` [L] (Pulsar Split: own names) |
| Binary | `C:\Program Files\Duo\Duo.exe`, own-process Win32 service, LocalSystem [L] |
| Start type | manual by default; automatic when the manager's "Start with Windows" is on [L][S] |
| Dependencies | none declared [L]; at runtime waits for Remote Desktop Services readiness (`Global\TermSrvReadyEvent`) [S] |
| Same binary, other modes | command-line sub-commands parsed by a small argument library [S]: |
| | – *start-service* helper: starts the service through the SCM (used by tooling/installer) |
| | – *create-home-directory*: prepares the seat user's profile/home on first use (spawned by the service) |
| | – *broadcast-environment-change*: run inside a seat session; sends the "environment changed" broadcast and waits (with retries and a timeout) until that session's taskbar process has picked it up; verifies the taskbar belongs to the same session |
| | – *host one instance from a config file* (used inside the sandbox VM, config extension `.vmconf`) [S][I] |
| Logging | Windows Application event log, source `Duo` (plus `DuoIdd`, `DuoAcRes` from the other modules) [L]; level from `Verbosity` [S] |
| Privileges enabled at start | SeTcb, SeDebug, SeShutdown, SeRestore, SeBackup, SeTakeOwnership; failure is fatal with a log entry [S] |

## 2. Registry model (`HKLM\SOFTWARE\Duo`)

### 2.1 Global values

| Value | Type | Written by | Meaning | Default / observed |
| --- | --- | --- | --- | --- |
| `Port` | DWORD | manager | HTTP control API port | 38299 if absent [S]; 8088 [L] |
| `Verbosity` | DWORD 0–4 | manager | log level (nothing, error, warning, information, debug) | [?] |
| `HostHidIsolation` | DWORD 0/1 | manager | shim-level HID isolation between console and seats | [?] |
| `SteamIsolation` | DWORD 0/1 | manager | per-seat Steam isolation | 1 [L] |
| `EnableProcessPatching` | DWORD 0/1 | manager | shim installed | 1 [L] |
| `RestartGPUsOnSandboxShutdown`, `SandboxMemory`, `SandboxStorage`, `SandboxStorageLocation`, `SandboxIp` | various | manager / service | sandbox mode (hidden) | `SandboxMemory`=50 [L] |
| `License`, `Thank You` | string | manager | licence data (encrypted) — not in Pulsar Split | [L] |
| `MinLuminance`, `MaxLuminance`, `MaxFullFrameLuminance` | DWORD | [?] | HDR metadata for the virtual monitor, read by the display module | absent [L] |
| `VerifierApplications` (subkey), `VerifierUsesBlacklist` | — | legacy | verifier-based injection list | absent [L] |
| `DuoControllerSessionId` | DWORD | [?] | read by the virtual-pad driver (pad ↔ session mapping) | absent [L] |
| `ProcessPatchingTargets\<md5>`, `SharedDirectories\<guid>`, `ForwardedPorts\<guid>` | subkeys | manager | see `10-manager.md` §8 | — |

### 2.2 Seat keys `Instances\<seatName>`

`<seatName>` is the stable identifier (ASCII letters/digits, ≤128 chars). It is used as:
RDP client name, config/log/state file prefix, Steam IPC name, the key in HTTP paths.

| Value | Type | Set by | Meaning | Default when absent |
| --- | --- | --- | --- | --- |
| `DisplayName` | string | manager | stream-server host name shown in Moonlight | key name |
| `UserName` | string | manager | local Windows account of the seat | — (seat skipped) |
| `Password` | string | manager | that account's password, encrypted with an app-wide key (base64 AES) [S]; the service and manager share the key | — (seat skipped) |
| `Enabled` | DWORD | manager | start this seat when the service starts | 0 |
| `Port` | DWORD | manager | stream-server base port | random 10000–65534 at creation |
| `Sandboxed`, `HideVMSuffix` | DWORD | manager | VM mode (hidden) | 0 |
| `IsScaleFactorStatic`, `ScaleFactor` | DWORD | manager | DPI scale: dynamic, or static percent | 0, 100 |
| `SuperSamplingFactor` | DWORD % | manager | render scale of the virtual monitor | 100 |
| `ForceSDR` | DWORD | manager | never expose HDR | 0 |
| `SDRWhiteLevel` | DWORD nits | manager | SDR content brightness in HDR mode | 400 (manager default) |
| `RenderAdapter` | string | manager | GPU description to render the seat on | best GPU at creation |
| `KeyboardLayoutOverride` | string/DWORD | [?] (no UI) | force a keyboard layout for the seat session instead of the user's own | absent |
| `SessionId` | DWORD | service (runtime) | Windows session id of the running seat; also used by the shim to know which seat a process belongs to | stale value stays after stop [L] |
| `Width`, `Height`, `RefreshRate` | DWORD | service / display module (runtime) [I] | current (last) virtual-monitor mode; initial desktop size for the next session | e.g. 1920 × 1080 @ 120 [L] |
| `Status` | string | service (runtime) | seat lifecycle state; at least `Booting` is used [S] | — |
| `AutoStartApplications\<guid>` | subkey: `ApplicationPath` (string), `Arguments` (string), `RunAsSystem` (DWORD) | manager | programs to launch in the seat session after it is up | none |

Pulsar Split: keep the same *information model*, but under its own root, store the
password via DPAPI, and add an explicit `Id` (GUID) per seat so renames are possible.

## 3. Files per seat (`<install>\config\`)

Generated by the service from the registry at every seat start [S][L] (file timestamps
on the live box match the seat start times in the event log [L]).

| File | Content | Lifetime |
| --- | --- | --- |
| `<seat>.conf` | stream-server config (key = value lines, §4) | rewritten on each seat start [L] |
| `<seat>_apps.json` | stream-server application list (Moonlight's "apps") | created if missing, then left alone [L][I] |
| `<seat>_state.json` | stream-server state: paired client certificates, unique id, web-UI credentials | owned by the stream server; persists across starts; contains secrets |
| `<seat>.log` | stream-server log | truncated/recreated on start [L][I] |
| `config\apps.json` | operator-editable *template* for new seats' app lists; the literal placeholder `InstanceName` is replaced with the seat name [L][I] | install-time |
| `credentials\…` | stream-server TLS key/cert material | secret |
| `<seat>.vmconf`, `…vmapps…` | sandbox variants | hidden mode |

If the template is absent, a built-in default list is written with two entries [S]:
*Desktop* (icon `desktop.png`) and *Steam Big Picture* (launches the Steam URL to open
Big Picture, auto-detach on, wait-all on, icon `steam.png`). The template observed on
the live box instead launches `steam.exe` through `cmd /c` with an IPC-name override and
user-chooser set to the seat name, Big Picture/gamepad UI, working directory the Steam
folder, auto-detach and wait-all on, exit timeout 5 s, and adds `$(PATH);Steam` and a
`VPROJECT` variable to the environment [L].

The sample `test.conf` (older version) lacks two of the keys below, so the key set grew
over versions [L].

## 4. Stream-server config keys (per seat)

All keys are upstream Sunshine keys; the service only fills a fixed subset.

| Key | Value written | Meaning |
| --- | --- | --- |
| `min_log_level` | from `Verbosity` (observed `none`) [L] | stream-server log level |
| `log_path` | `<seat>.log` | relative to working dir `config\` [I] |
| `file_apps` | `<seat>_apps.json` | app list |
| `file_state` | `<seat>_state.json` | pairing state |
| `credentials_file` | `<seat>_state.json` (same file) | web-UI login |
| `sunshine_name` | seat `DisplayName` | name shown in Moonlight |
| `port` | seat `Port` | base port (HTTP); others derived |
| `install_steam_audio_drivers` | `disabled` | never install Steam's virtual audio driver per seat |
| `virtual_sink` | `Remote Audio` | the RDP session's audio endpoint, so audio is captured per session |
| `minimum_fps_target` | `1` [L] | lets the encoder idle down to ~1 fps on static content |

Not written, so the fork's defaults apply: `capture` (the fork adds method `ddx`, "Duo
Monitor Capture", reading frames from the display module's shared section) [S],
encoder, adapter (the GPU is pinned by the display module instead) [I].

Ports used by one seat (upstream Sunshine rule, base = `port`) [I]:

| Offset | Protocol | Use |
| --- | --- | --- |
| −5 | TCP | HTTPS (pairing, app list, launch) |
| 0 | TCP | HTTP (serverinfo, pairing start) |
| +1 | TCP | web UI (HTTPS) |
| +9 | UDP | video |
| +10 | UDP | control (ENet) |
| +11 | UDP | audio |
| +21 | TCP | RTSP |

Allocation rules in the reference: random base 10000–65534 at seat creation, operator
may edit within 1029–65514; no overlap check [S]. Pulsar Split: allocate deterministically
(e.g. 47989, 48089, 48189 … step ≥ 30), check overlaps across seats and against ports in
use, and create one firewall rule per seat for exactly its port set.

## 5. Service start (boot) sequence

Observed log on start: service started → HDR enabler decision → (per seat) stream-server
started [L]. Full order [S][I]:

1. Report *start pending*; enable privileges (§1). Register event-log sources.
2. Read global settings; set seat `Status` values to `Booting` [S][I].
3. **System preparation** (idempotent; each step logs on failure and some are fatal):
   - Defender exclusion for the install folder (PowerShell) [S].
   - Enable the built-in Remote Desktop firewall rules (TCP/UDP 3389, TermService) via
     the Windows Firewall COM API [S].
   - Remote Desktop Services registry settings (list in `00-architecture.md` §6.2) [S][L].
   - Point TermService's ServiceDll at the wrapper and restart TermService (stop
     dependents first, terminate stragglers by PID if needed); fatal if the service
     cannot be stopped/started or `termsrv.dll`'s version is unsupported [S].
   - Point BluetoothUserService's ServiceDll at its wrapper [S][L].
   - Install/refresh the display module in `System32\drivers\UMDF` [S].
   - Ensure the shim DLLs in System32/SysWOW64 are current when process patching is
     enabled; remove legacy verifier artefacts [S].
   - Remove a legacy Steam "multibox" add-on [S].
   - HDR enabler (see `00-architecture.md` §8 item 3 — out of scope; Pulsar Split gets
     HDR from its own IddCx driver instead).
4. Wait for RDS readiness; read the local RDP listener port (`RDP-Tcp` WinStation
   `PortNumber`, 3389 on the live box) [S][L].
5. Start the HTTP control API on `Port` [S].
6. Report *running*; log an information entry.
7. Enumerate `Instances`; log one entry per parsed seat; for every seat with
   `Enabled` = 1 run **seat start** (§6). A seat lacking user name or password is
   skipped with a log entry [S].

## 6. Seat lifecycle

### 6.1 States

```
          +---------+   start (boot / HTTP / manager)   +----------+
          | Stopped | --------------------------------> | Booting  |
          +---------+                                   +----------+
              ^   ^                                        |  session up, stream
              |   |   stop (HTTP / service stop /          v  server started
              |   |   suspend)                        +----------+
              |   +---------------------------------- | Running  | <--+
              |                                        +----------+    | watchdog:
              |  fatal error during boot                   |          | stream server
              +--------------------------------------------+          | exited → restart
                                                           +----------+
```

`GET /instances/<seat>` returns `true` only in *Running* [S][I].

### 6.2 Start (per seat)

1. Load the seat's settings; refuse with a log entry if the seat does not exist [S].
2. **User checks** [S]:
   - query the account; if its password has expired, refuse with an error entry;
   - if account details cannot be read, refuse;
   - ensure the account is a member of *Remote Desktop Users* (alias RID 555); add it
     if needed (by direct manipulation of the local security database, which is why
     SYSTEM plus backup/restore privileges are used) [S]; refuse if this fails;
   - first-time: create the user's home/profile by spawning the helper mode (§1) [S][I];
   - read the user's keyboard layout preferences from the user's hive (preload list and
     substitutes) unless `KeyboardLayoutOverride` is set [S].
3. **Display module on** for this seat ("toggle the display driver on") — switch the
   RdpIdd UMDF image path to the substitute module [S][L]; refuse on failure.
4. **Create the session**: launch the headless RDP client with: target `127.0.0.1`,
   the RDP listener port, client name = seat name, user name, password, keyboard layout,
   width and height (last known `Width`/`Height`) [S]. The client keeps the RDP
   connection open for the life of the seat [I].
5. **Find the session**: enumerate sessions, skip `Services`, `Console` and the
   `RDP-Tcp` listener, query each session's client info and pick the one whose client
   name equals the seat name; store its id in `SessionId` [S]. Retry until a timeout;
   on failure log an error and stop the seat [S].
6. **Apply the display patch** for that session's display adapter (the substitute
   module hooks the RdpIdd instance of the new session) [S]; failure is logged and
   stops the seat [S][L].
7. Write `<seat>.conf` and, if missing, `<seat>_apps.json` (§3–4).
8. **Start the stream server** inside the session: duplicate the service's own token,
   set its session id to the seat's session, create the process on the session's
   interactive desktop (`winsta0\default`) with working directory = install folder and
   arguments `config\<seat>.conf` [S][I]; log an information entry naming the seat [L].
9. **Autostart applications**: for each entry, create the process in the seat session
   either with a duplicated SYSTEM token (`RunAsSystem`=1) or with the seat user's
   token, passing `Arguments`; log success/failure per entry [S].
10. Mark *Running*.

### 6.3 Watchdog

While a seat is *Running*, the service watches the stream-server process; when it
exits it is started again immediately. On the live box a crashing stream server was
restarted about every 2 s, producing ~20 000 identical log entries in ~16 h — there is
no back-off and no give-up [L]. Pulsar Split must use exponential back-off, a crash
counter, and surface the failure to the manager.

### 6.4 Stop (per seat)

Triggered by HTTP `/instances/<seat>/stop`, service stop, or system suspend [S]:
terminate the stream server and autostart processes (by PID; failure logged), end the
headless RDP client, switch the display module off for the seat, mark *Stopped*, log an
information entry [S][L]. Whether the Windows session is logged off or left
disconnected is [?]; Pulsar Split should make it a seat option (default: log off, so
the next start is clean).

### 6.5 Create / delete

Creating and deleting seats is done by the manager purely in the registry while the
service is stopped (see `10-manager.md`); the service has no create/delete API [S].
Deleting leaves `config\<seat>*` files behind [L][I]. Pulsar Split: the service should
own create/delete (validates name/port/user uniqueness, removes files and firewall
rules, optionally removes pairing state).

## 7. Service-wide events

| Event | Reference behaviour |
| --- | --- |
| Service stop | stop all seats (§6.4); remove virtual-pad device nodes and the pad driver package [S]; log an entry [L]; restore the display-module redirection [S][I] |
| `/restart` (HTTP) | log an entry; stop everything, restart the service process [S] |
| System suspend | log an entry; stop seats [S][I] |
| System resume | log an entry; start enabled seats again [S][I] |
| User logon/logoff at console | no special handling found [?] |
| Seat session disconnect (RDP client lost) | [?] — presumably detected through the watchdog / session enumeration; Pulsar Split: detect via WTS session notifications and recreate the session |
| Shutdown blocking | the headless RDP client registers a shutdown-block reason (keeps its hidden window alive at shutdown) [S][I] |

## 8. HTTP control API

See `00-architecture.md` §5.1 for routes. Behaviour details [S]:

- `start` on a running seat or `stop` on a stopped seat returns a message and does
  nothing else [I].
- Messages are short English sentences: seat started, seat stopped, seat does not
  exist, action unknown, service restarting.
- Static files are served from `assets\web\duo`; the HTML page is generated by the
  service (host name, product version, seat rows).

Pulsar Split replacement (recommended): a named pipe (or loopback-only HTTP with a
per-install token readable only by Administrators) offering: list seats with status,
start/stop seat, create/update/delete seat, service restart, read recent log lines,
and a "pair" call that forwards a PIN to the seat's stream server.

## 9. Error handling summary

| Condition | Effect | Log level |
| --- | --- | --- |
| privileges cannot be acquired | service fails to start | error |
| `Instances` key cannot be opened | no seats started | error |
| one seat key cannot be opened / lacks user or password | that seat skipped | error / info |
| user password expired / account info unreadable / cannot join RDU | seat not started | error |
| TermService cannot be stopped/started, or `termsrv.dll` unsupported | multi-session unavailable; seats fail | error |
| display module cannot be installed / toggled / patched | seat not started (live box: one "module could not be updated" and one "display patch failed" error were logged) [L] | error |
| headless session cannot be created / session id not found | seat not started | error |
| stream server fails to start | logged, retried by watchdog | error |
| stream server exits | restarted (no back-off) | information |
| autostart app fails | logged, seat continues | error |
| firewall / Defender / registry tweak fails | logged, continue | error |

## 10. Users and sessions — operator guidance derived from the reference

- One local account per seat; the account must have a password (RDP refuses blank
  passwords unless the blank-password policy is relaxed, which the reference does
  globally — Pulsar Split should require passwords instead).
- On the live box all seat accounts are also local administrators [L]; that is not
  required by the design (RDU membership suffices) [I]. Pulsar Split should not require
  admin seat accounts.
- The same Windows user must not be used by two seats or by the console user at the
  same time; the reference does not check this [?]. Pulsar Split must.
- Seat sessions are *remote* sessions in Windows' eyes; some software refuses to run
  or behaves differently there (anti-cheat, some launchers, DRM). The reference masks
  this with the shim; Pulsar Split should document limitations per application.

## 11. Open questions for runtime capture (later)

- Exact log-off vs. disconnect behaviour on seat stop.
- When and by whom `Width`/`Height`/`RefreshRate` are written.
- Byte layout and synchronisation protocol of the frame section and pad sections (we
  will define our own; capture only to confirm design assumptions).
- Whether the HTTP API binds to loopback.
- Behaviour when the seat user logs on at the console while the seat runs.
