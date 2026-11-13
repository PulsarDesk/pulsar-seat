# 35 — Local seat sessions with user-mode code only (feasibility)

Status: draft v2 (2026-11-13), revised after the spec-critic review of v1. Analyst-written.
Behaviour and feasibility only, no code; see [../CLEANROOM.md](../../CLEANROOM.md). Extends
[30-sessions.md](30-sessions.md) §3, §3.11 and §6.1. Related:
[00-architecture.md](00-architecture.md) §5.2, [40-display.md](40-display.md) §6.3,
[50-input.md](50-input.md), [60-audio.md](60-audio.md) §5,
[../docs/agents/plan.md](../../docs/agents/plan.md).

Confidence markers as in spec 30: no marker = observed (in the reference product's files, in
Windows binaries on the dev laptop or the test PC, or in Microsoft documentation);
**HYPOTHESIS** = inferred, not confirmed at run time. Nothing in this file was run as a
session-changing test; every runtime statement about session behaviour is either taken from
the reference product's event-log timeline (spec 30 §3.2) or marked HYPOTHESIS.

Machines: dev laptop = Windows 11 Pro ARM64, build 28000. Test PC = Windows 11 Pro x64,
build 26100 / 26300 line, reference installed but disabled (read-only checks only).

---

## 1. Question

Can Pulsar Seat give each seat its own **concurrent, local** interactive Windows session —
own user, own logon screen, own desktop on the seat's monitors, own input — on **client**
Windows 11 (Home and Pro, x64 and ARM64), using only user-mode code and no kernel component
that patches or injects?

Sub-questions:

1. What does the reference product achieve with its kernel-side step "change the kernel's
   notion of the active console session" (spec 30 §3.11)?
2. Is there a user-mode way to reach the same end result, or one close enough for seats?

Short answer: **no** for concurrent local seats; **yes** for time-shared local seats (one
live at a time) and for a reduced single-session kiosk mode. Details in §5.

## 2. What the reference does, step by step

### 2.1 Per step: user-mode or kernel-side

| Step | Where the reference does it | User-mode equivalent on client Windows |
| --- | --- | --- |
| Create a fresh local session for an extra seat | Seat service (session 0) disconnects the console session. **HYPOTHESIS** (strong; only the event-log timeline was observed): Windows then creates a new console session with its own winlogon and LogonUI, as in normal fast user switching. | Same: documented `WTSDisconnectSession` on the console session. |
| Log the seat's account on in that session | Its own credential provider, loaded by that session's LogonUI. | Same: a documented credential provider with automatic submit (§5.4). Must be a native DLL for each architecture. |
| Hand the console to the new session and back | A client call into LSM's local RPC endpoint. The interfaces it calls are the **documented** Terminal Services protocol interfaces (session, notification and enumeration interfaces of [MS-TSTS]); the hand-over itself is the "connect session" operation, the same operation that `WTSConnectSessionW` performs. | Same: `WTSConnectSessionW` / `WTSDisconnectSession` from a SYSTEM service. |
| Keep the extra session alive and usable **after** the console has gone back to seat 1 | In-process modification of LSM (excluded by spec 30 §6), plus the kernel driver work below. | **None.** |
| Make the seat session count as "the console" for kernel and service consumers | Kernel driver: sets the system-wide active console session id (§2.2). | **None.** No user-mode setter exists. |
| Give the seat session its physical monitors | Kernel driver works on the graphics kernel so that a non-console session can own physical display sources (spec 40 §3.4, **HYPOTHESIS** strong). | **None** for direct scan-out (§3.3). |
| Give the seat session its physical keyboard, mouse, gamepads | Kernel driver (input routing, spec 50) plus the console-id override. | None for keyboard and mouse into a second session. |
| Per-seat view of logon-related registry values | Kernel registry filter. | Not needed if Pulsar uses its own credential provider per session (spec 30 §6.2.11). |

Correction to spec 30: §3.8 (last row of the runtime-objects table) and §3.11 (third
bullet) call the reference's LSM RPC use "undocumented interfaces for session creation and
console hand-over". The interfaces it calls are documented in [MS-TSTS] and are wrapped by
documented WTS functions. The reference binaries also carry the identifiers of private LSM
interfaces, but no client calls to them were found (**HYPOTHESIS:** the in-LSM part uses
them; this would explain why the logon begin/end event pair is mostly missing for extra
seats, spec 30 §3.2). What is undocumented is only the part that keeps a disconnected
session on its own hardware.

### 2.2 What the kernel-side console step achieves

- Windows keeps **one** system-wide value, the active console session id, in the page that
  the kernel shares read-only with every process. `WTSGetActiveConsoleSessionId` and the
  native getter read it.
- The setter is a kernel routine. It is exported only by the kernel image; its only
  **static importers** in the box are the two win32k kernel modules (an import scan cannot
  see callers that look it up at run time). The user-mode system library exports only the
  getter. Same on x64 and ARM64.
- Who changes it in stock Windows is **HYPOTHESIS**, with two candidates: (a) win32k, when a
  session's win32k attaches to the physical console during a console connect; (b) LSM,
  through a system-information call into the kernel. Either way, no user-mode component
  outside the session manager changes it.
- The reference's driver sets the value.
- **End result (HYPOTHESIS, strong):** a seat session that LSM records as *disconnected*
  is treated as "the console" by every component that asks "is this the console?". That
  keeps the physical outputs, HID devices and console-bound audio usable from that session.
  Because the value is one global, the reference also needs its in-process changes to serve
  several seats at once; the console-id change alone gives one seat, not N.
- Who reads the value: static import scan of either getter in System32 and System32\drivers
  (top level) on the ARM64 laptop. The scan finds **127 distinct binaries**; a narrower
  earlier scan found 119 (a subset). Groups:
  - kernel: graphics kernel, HID class driver, monitor class driver, watchdog, the Xbox
    gamepad drivers, win32k;
  - user mode: the audio services, the sound-playing and multimedia libraries, winlogon,
    wininit, userinit, user32, the profile service, power policy and shutdown UI, sticky
    keys, smart-card service and credential providers, biometrics and Windows Hello
    providers and services, location and capability-consent services, HID and input
    services, the game input service, WLAN service, the display broker, LSM, termsrv.
  - These are the components that would treat any non-console seat session as "not the
    console", and route their behaviour (UI, devices, prompts) to seat 1.

## 3. How client Windows enforces one interactive session

Two separate limits apply. They are often mixed up.

### 3.1 Licence policy (user mode: termsrv and LSM)

- termsrv and LSM read product-policy values from the software-licensing store. On both Pro
  machines: multiple sessions allowed = 1 (sessions may *exist*: fast user switching),
  app-server mode = 0, max user sessions = 0, Enterprise multi-session flag = 0, and several
  per-class session maxima of 1. Which licensing class means which limit is a
  **HYPOTHESIS**.
- termsrv's session-count policy counts sessions in use against the allowed maximum and runs
  session arbitration (disconnect the other session, single session per user, request
  dialog to the console user). This logic sits in termsrv core, above any protocol stack, so
  it applies to RDP and to any third-party protocol provider alike (**HYPOTHESIS**, strong).
- The console counts toward the limit: the in-box resources tell the console user they will
  be disconnected when a remote user connects. The RDP client has an error message for a
  loopback connection while a console session is in progress (**HYPOTHESIS** on the exact
  trigger; the evidence is the message resource only).
- These are licensing values, not settings. No documented switch changes them on client
  SKUs. The user-facing switches that matter for fast user switching are the Winlogon value
  `AllowMultipleTSSessions` (absent or 1 = on) and the policy `HideFastUserSwitching`
  (absent or 0 = on); domain-joined PCs may turn it off by default.
- Other client editions do not change this. Windows 11 Enterprise, Enterprise LTSC and IoT
  Enterprise (including IoT Enterprise LTSC) are single-interactive-session client SKUs;
  their kiosk features (assigned access, shell launcher, keyboard filter) work inside one
  session. Enterprise multi-session exists only as a cloud-hosted SKU. The licence values
  were not read on those editions (**HYPOTHESIS**, strong, that they match Pro). The
  question "is there a client edition with local multi-session" is closed: no.

### 3.2 Architecture (kernel: one physical console)

- Only one session's win32k is attached to the physical console terminal at a time. LSM
  owns the local message port that each session's subsystem extension connects to; through
  it, LSM tells a session's win32k to attach to or detach from the physical display and
  input. The attached session is the one the active console id names.
- A disconnected local session keeps its processes, but it has no display, no physical
  keyboard or mouse, and (**HYPOTHESIS**) no audio output.
- This limit is not a licence value. It is the same on Server. On Server, extra sessions are
  remote-type sessions, not extra console sessions.

### 3.3 Display ownership

- **HYPOTHESIS** (strong; the evidence is strings in the graphics kernel and the IddCx
  documentation): the graphics kernel ties physical (render-capable) adapters and their
  outputs to the console session. A non-console session can own only a *sessionized*
  adapter, which is a remote indirect-display adapter.
- A remote indirect-display (IddCx remote-session) driver starts only when the OS remote
  desktop stack (termsrv) created the device for a remote session; adapter start fails
  otherwise (Microsoft Learn, IddCx 1.4). One device node per remote session, removed on
  disconnect.
- So a second session cannot scan out directly to a physical monitor in user mode. At
  best it renders to a remote virtual monitor, and a viewer in the console session copies
  those frames onto the seat's monitors.

### 3.4 Input ownership

- Physical keyboards and mice feed win32k of the console-attached session only.
- The HID class driver refuses HID reads from a session that is not the active console,
  unless the per-device value `SessionSecurityEnabled` = 0 is set under the device's
  `Device Parameters` key. The value name is in the HID class driver on both architectures
  and in the reference driver's per-seat table, but it is set on no device on either
  machine. Only one public statement from a Microsoft engineer describes it; it is not on
  Microsoft Learn (**HYPOTHESIS** that it still works on current builds). It would cover
  generic HID readers (some gamepads, custom devices), not keyboards and mice, and not the
  Xbox gamepad drivers, which check the console id themselves.

### 3.5 What exists on client Windows besides the console

| Kind | Public API | Concurrent with console? | Why it is not a seat |
| --- | --- | --- | --- |
| Fast-user-switching sessions | Yes (WTS) | Exist concurrently, but only one is connected | No display or input while disconnected |
| Child session | Yes (documented, Win8+) | Yes, one per system | Same user as the parent, no logon screen, no lock, ends with the parent, Pro/Enterprise only |
| Headless / agent / utility sessions (new in 26100+ / 28000) | No (experimental exports, private interfaces) | Yes, capped | Parent-bound, credential logon without LogonUI, no frame consumer, no input, display-capable kind refused by LSM, feature- and caller-gated |
| Remote (RDP) sessions | Yes | No on client: the first one disconnects the console | Licence policy (§3.1) |

## 4. Candidate user-mode approaches

All candidates were checked adversarially (raw notes outside the repo). "Refuted" means the
evidence contradicts the candidate's claim as a seat mechanism.

| # | Approach | How | Client SKU | ARM64 | Gaps | Effort | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| U1 | Fast user switching, orchestrated (spec 30 option C) | SYSTEM service creates each seat's session by disconnecting the console, logs the seat account on with a per-session auto-submit credential provider, and moves the console between seat sessions with `WTSConnectSessionW`. | Pro: yes. Home: **HYPOTHESIS** (strong; switch user and disconnect exist on Home; test path P1-H). Fails when fast user switching is turned off. | Yes (all pieces native; provider must be an ARM64 DLL). | Not concurrent. The live seat takes every monitor and every input device. Others are frozen: no display, input or audio (HYPOTHESIS for audio); GPU apps and games may lose their device. Blank gap at each switch (about 1–3 s, HYPOTHESIS; P1 measures it). Each seat needs its own account (single session per user is the default). Switch trigger limits (§5.5). | Low for sessions; medium with switch trigger and per-seat display topology. | **Holds, time-shared only.** |
| U2 | Single session, one desktop object per seat (option D as written) | `CreateDesktop` per seat, `SwitchDesktop` between them. | Runs, but one desktop at a time. | Same. | Only the input desktop is visible and takes input, on all monitors. Secure desktops (UAC, Ctrl+Alt+Del) hide all seats. | Low | **Refuted** as concurrent seats; usable only as "one kiosk at a time". |
| U3 | Single session, one desktop, one full-screen window per seat (D′, kiosk / app seats) | Pulsar seat shell owns one borderless window per seat on that seat's monitors; reads each seat's devices with Raw Input in background mode by device handle; draws its own cursors; picks the seat's audio endpoint for its own content. | Yes (Home and Pro). | Yes. | Same user, profile, clipboard, shell for all seats. One foreground window, one focus, one system cursor. Ordinary apps and most games get input only when focused, so they cannot serve two seats at once. Without an input filter, extra seats' keyboards also reach seat 1's focused window, so seat 1 cannot be a normal desktop at the same time. Seat 1 lock, UAC prompt or sign-out hits every seat. | Medium | **Holds, reduced** (seat-aware content only). |
| U3+U1 | Hybrid: seat 1 is a real U1 session used as a normal desktop, extra seats are U3 kiosk windows in that same session | As U3, inside the live U1 session. | As U3. | Yes. | The extra seats' keyboards and mice also drive seat 1's focused window and system cursor, because there is no user-mode way to keep a device's input out of the session's normal input path. Usable only if the seat 1 user accepts that, or if the input filter (spec 50) exists. | Medium | **Holds only with the input filter**; without it, seat 1 is not a normal desktop. |
| U4 | Child session as one extra seat | Enable child sessions, show the child full-screen on seat 2's monitors through the RDP client control. | Pro/Enterprise only; Home not supported. | Binaries present and native; untested. | Same user, no logon screen, no lock, one per system (max 2 seats), ends with the parent; input only while the control has focus in seat 1's desktop; seat 1 lock or UAC hides it (HYPOTHESIS). | Medium | **Refuted** as a seat. At most an optional same-user "second desktop". |
| U5 | Inbox headless / agent sessions | Undocumented experimental session-creation exports and the agent-isolation platform. | Present but feature-gated, caller-gated (no session-0 caller), capped. | Present; same limits. | No frames delivered, no input, parent-bound, credential logon without LogonUI, display-capable kind not wired up in LSM, undocumented. Its visible variant is loopback RDP into a normal remote session, i.e. U7. | n/a (blocked) | **Refuted.** Watch item only. |
| U6 | Per-subsystem console-id workarounds | Run seats as non-console sessions and fix each console-id reader (HID per-device value, audio forwarding, virtual display plus viewer). | Needs a concurrent non-console host session; on client the only one is the same-user child session (U4). | Same. | Gamepad drivers and 100+ other readers have no user-mode fix; display is a mirror, not ownership. | High | **Refuted** as an architecture. Pieces reusable: audio forwarding, remote display plus viewer, HID value test. |
| U7 | Remote-type seat sessions + presenter in the console (loopback RDP or own protocol provider) | Seat 1 or a host session is the console; each seat is a termsrv session with a remote virtual monitor, shown full-screen by a viewer. | No: the first seat connection disconnects the console that runs the viewer. A custom protocol provider loads on Pro/Enterprise (not Home) but is undocumented on client and hits the same limit (**HYPOTHESIS**, strong). | No working variant. | Licence policy (§3.1). | High | **Refuted** on client. Valid only on Server (U8). |
| U8 | Windows Server + RDSH loopback (spec 30 option A) | As U7, with every seat (seat 1 included) a remote session and a dedicated host console session. | No (Server only). | No: no generally available Windows Server for ARM64. | Server licence plus RDS CALs; input still needs per-device routing (and a filter to keep it out of the focused window); RDP audio, USB and GPU limits; N+1 sessions. | Medium–high | **Holds**, Server x64 only. Not an answer for client SKUs. |
| U8b | Windows Server + Pulsar's own protocol provider (WRDS) instead of the RDP stack | Each seat a remote session served by a Pulsar protocol provider registered with termsrv (documented `IWRdsProtocolManager` / `IWRdsProtocolConnection` family on Server); Pulsar feeds each session its own seat's input and takes its frames through the remote indirect display path. | No (Server only). | No (no Server ARM64). | Larger effort than U8; same licences; removes the RDP client from the loop, so input routing and frame path are under Pulsar's control (**HYPOTHESIS** that latency and device handling improve). | High | **Possible later** variant of U8, not Phase-2 work. |
| U9 | termsrv/LSM wrapper or patch, then loopback RDP seats | Community wrappers lift the client session limit. | x64 only while per-build offsets exist (already missing for the test PC's build). | No: wrapper DLLs are x64; no maintained ARM64 data. | Excluded by spec 30 §6; remote sessions, so it keeps all of U8's presenter and input gaps. | High | **Refuted** (rules and ARM64). Open question 10. |
| U10 | One VM per seat (Hyper-V) | Each seat a guest OS in a full-screen viewer. | Pro/Enterprise host only; no Hyper-V on Home. | Host role works. | Viewer input follows host focus (same input problem as U3); one Windows licence per seat; not sessions of one install; enhanced session needs Pro guests. (Device assignment and GPU partitioning are not supported on client hosts; GPU partitioning is reported to work unofficially on Pro x64, but this does not fix the input or licence gaps.) | Medium, still incomplete | **Refuted** as a seat. Useful as a test harness (P1-H). |

Precedent for U8: Microsoft's own local multi-seat product line (Windows MultiPoint Server
2010–2012, then the MultiPoint Services role of Windows Server 2016, no longer developed
after that) was built on Remote Desktop Session Host and existed only on Server, x64. Microsoft
never shipped local multi-seat on a client SKU. This supports "Server-only, x64" for U8.

Dropped claims (shown false or overstated by the checks):

- "Client multi-session is a setting": it is licensing policy (§3.1).
- "Separate desktops give concurrent seats": only one desktop per window station is visible
  (U2).
- "A custom protocol provider gives extra sessions on client": it inherits termsrv's limit;
  it is also not loaded on Home (U7).
- "Headless sessions give a second desktop": the display-capable kind is refused and no
  frames leave (U5).
- "Option A works on ARM64 technically": no Windows Server ARM64 is available (U8).
- "IoT Enterprise or LTSC allow local multi-session": they are single-session client SKUs
  (§3.1).

## 5. Recommendation

### 5.1 What is possible in user mode

1. **Real local sessions, time-shared (U1).** Own user, own logon screen, own desktop, own
   profile per seat, on Home (HYPOTHESIS) and Pro, x64 and ARM64. One seat live at a time.
   This is the only user-mode path that reproduces the reference's *session* half (create,
   log on, hand over the console). It cannot reproduce the *concurrency* half.
2. **Concurrent kiosk / app seats in one session (U3).** Several people at once, x64 and
   ARM64, Home and Pro, but only for content that Pulsar renders or that reads per-device
   input in the background. No per-seat user. Combined with a U1 seat 1 used as a normal
   desktop (U3+U1) only when the input filter exists.
3. **Concurrent full sessions on Server x64 (U8).** Unchanged from spec 30 option A; U8b is a
   later variant.

Best user-mode path for "true local seats" on client x64 and ARM64: **U1 as the
session-lifecycle backend**, with U3 as the separate concurrent mode. Neither is true
multi-seat; the manager must say so.

### 5.2 What stays impossible without kernel code

- Two or more sessions attached to physical display and input at the same time.
- Changing the active console id, or attaching a disconnected session's win32k to physical
  hardware.
- Direct scan-out of a non-console session to a physical monitor (only remote virtual
  monitors are sessionized, and only termsrv creates them).
- Physical keyboard and mouse input into a non-console session's win32k.
- Making the console-id readers (gamepad drivers, audio services, power UI, Windows Hello,
  smart card, consent prompts and the rest of §2.2) serve a non-console seat.
- On client SKUs, any concurrent second session with its own user and logon screen; the
  only documented concurrent kind (child session) is same-user and logon-less.

### 5.3 U1 behaviour: audio

- Pulsar does not set the system default audio endpoint on a switch. Windows has no
  documented API for it, and spec 60 §5 rules out any default-endpoint setter.
- Per-seat audio in U1 comes only from (a) apps that let the user choose an output endpoint,
  and (b) the per-user default endpoint that each seat's user picks once in Windows sound
  settings. **HYPOTHESIS** (P3 checks): that per-user choice persists in the user's profile
  and applies again when the seat's session becomes the console.

### 5.4 U1 behaviour: logon of a seat session (credential provider)

- The Pulsar seat service (SYSTEM) keeps the table seat → account → current session id. It
  records the new session id when the console is disconnected and the fresh session
  appears (WTS session-change notification).
- The Pulsar credential provider runs inside each session's LogonUI. It finds its own
  session id from its own process and asks the seat service over local IPC, keyed by that
  session id, which seat account belongs to the session and for the credential to submit.
- If the session belongs to a seat that should log on automatically, the provider reports
  one tile as the default with auto-logon, so LogonUI submits it without user action
  (documented `ICredentialProvider` contract).
- Provider filtering: a Pulsar credential-provider filter (documented
  `ICredentialProviderFilter`) hides the other providers only in sessions that the service
  marks as auto-logon seat sessions, and only for the logon scenario. In every other
  session, and whenever the service does not answer, the filter hides nothing, so the
  normal password, PIN and other tiles stay available.
- The lock screen of a seat session is a normal unlock; the provider does not auto-submit
  there.

### 5.5 U1 behaviour: switch trigger

- A session-0 service cannot see keyboard or mouse input. A Pulsar helper in the live seat's
  session runs only on that session's default desktop; it does not run on the logon, lock or
  secure (UAC, Ctrl+Alt+Del) desktops.
- Rule: the user starts a switch from (a) the Pulsar tray or manager UI in the live seat's
  desktop, which may also register a hotkey that works on that desktop only, or (b) a
  "switch to seat N" link that the Pulsar credential provider shows on the logon and lock
  screens of every seat session. The helper or provider sends the request to the seat
  service, which calls `WTSConnectSessionW`.
- No switch while a secure desktop (UAC, Ctrl+Alt+Del) is up. No switch on "last device
  used": the first input would land in the wrong session.
- **HYPOTHESIS** (P7 checks): a credential provider command link on the lock screen can
  reach the service and the switch completes without first unlocking.

### 5.6 U1 behaviour: per-seat display topology

- In U1 the live session owns every physical monitor. "Seat topology" means which monitors
  are active and how they are arranged for the live seat (for example, only seat 2's
  monitor on, the others off).
- Owner: the Pulsar helper in the seat's own session applies the seat's topology with the
  documented display-configuration API right after that session becomes the console and the
  user's desktop is up.
- While the session is still at LogonUI, no Pulsar helper runs there; the logon screen shows
  whatever topology Windows restores for the connected monitor set. **HYPOTHESIS** (P3
  checks): the credential provider, running in that session's LogonUI, can apply the topology
  before logon; if not, the logon screen appears on all monitors.
- The seat service in session 0 does not apply topology (**HYPOTHESIS** that the call does
  not reach the console from session 0).

### 5.7 Prototype plan

Goal: prove or kill U1 as a usable product mode, and settle the U3 unknowns, with the
smallest experiments. All experiments except P5 change session state; the maintainer must
approve each one before it runs.

Measurement of the blank gap (P1, P3): from the seat service's timestamp of the switch
request to the first frame presented in the target session, taken by the Pulsar helper in
that session (first successful desktop capture after the connect). Cross-check with the
timestamps of the LSM operational event log (disconnect and reconnect events). Ten switches
per direction; report median and maximum. Session-at-LogonUI targets are measured with the
event log only.

| # | Experiment | Machine | Thresholds / proves / kills | Maintainer must approve |
| --- | --- | --- | --- | --- |
| P1 | From a SYSTEM service: disconnect the console, check that a fresh LogonUI session appears; sign seat account B in by hand; reconnect seat 1 with `WTSConnectSessionW` (with and without B's password); then switch to B and back. | ARM64 laptop first, then x64 test PC (reference stays disabled). | Pass: session created and handed over from session 0 on both architectures; blank gap median ≤ 3 s and maximum ≤ 5 s. Kill U1 if a session-0 caller cannot create or hand over sessions, or if the maximum gap is above 5 s. Settles the password question for SYSTEM callers. | Two local test accounts on each machine; live session switching (open work in seat 1 is at risk); x64 test PC use. |
| P1-H | P1 and P2 inside a Windows 11 **Home** guest VM (Hyper-V basic session, which is a console-type session inside the guest) on a Pro host. Unactivated install is enough. | x64 test PC host; ARM64 laptop host if a Home ARM64 install is possible (**HYPOTHESIS**). | Same thresholds as P1, gap measured inside the guest. Settles open question 4. | Enabling Hyper-V and creating one VM. |
| P2 | Leave a session at the logon screen and hand the console away: does Windows end it, and after how long? | Both. | Pass: the session survives at least 10 minutes. Decides whether seats can be prepared in parallel or only one after another. | Same as P1. |
| P3 | Run a game or a GPU-heavy app and an audio player in seat B, switch away and back: what survives? Also: per-user default audio endpoint after the switch (§5.3); topology applied by the helper, and by the credential provider at LogonUI (§5.6). | x64 test PC (GPU), then ARM64. | Records frozen vs. lost state for the limits text. Topology pass: seat topology active within the P1 gap budget after the switch. | Same as P1, plus installing one test app. |
| P4 | Minimal per-session auto-submit credential provider plus filter (§5.4), native ARM64 and x64, for one seat account; IPC to the service keyed by session id. | Both. | Pass: seat session logs on without user action, other sessions still show the normal tiles, and logon still works with the service stopped. Proves auto-logon per seat without touching the global Winlogon auto-logon values. | Registering a credential provider and filter (logon-screen change) on both machines; a recovery path if logon breaks. |
| P5 | U3 core: one process, two borderless windows on two monitors, two mice and two keyboards read in the background by device handle, own cursors. Then check whether the second keyboard also types into the focused window, and what happens when the foreground app registers raw input for itself in exclusive mode (no legacy messages) or is a full-screen game. | ARM64 laptop (needs a second monitor and USB keyboard/mouse), then x64. | Pass: device-event-to-cursor-update latency inside the process median ≤ 4 ms and 99th percentile ≤ 16 ms; own cursors redraw at the monitor refresh rate with no visible stutter during 2 minutes of movement on both seats; background reading of both seats' devices continues while another app is foreground in exclusive raw-input mode. | None for the code; only the extra devices. No system change. |
| P6 (optional) | Per-device `SessionSecurityEnabled` = 0 on one gamepad, read from a non-console session. | x64 test PC with a child session enabled (only available concurrent non-console session). | Confirms or drops the HID value as a U6 piece. | Enabling child sessions and writing one device value on the test PC; revert afterwards. |
| P7 | Switch trigger (§5.5): tray/manager request from the live seat; credential-provider link on the logon screen and on the lock screen of a seat session. | Both, after P4. | Pass: all three paths switch within the P1 gap budget; no switch is possible while a secure desktop is up. | Same as P4. |

Stop rules:

- If P1 fails on either architecture, U1 is dropped and Pulsar Seat on client SKUs offers U3
  only.
- If P1-H fails, U1 is offered on Pro (and higher) only.
- If P5 fails because background reading stops while another app is foreground, U3 is
  limited to "Pulsar seat shell is always the foreground app". If P5 fails on latency or
  cursor smoothness on either architecture, U3 is dropped on that architecture until a
  different input path exists. If both U1 and U3 fail, client SKUs get no seat backend in
  Phase 2.

## 6. Effect on spec 30 and on the current implementation

### 6.1 Spec 30

- §3.8 (LSM RPC row) and §3.11 (third bullet): replace "undocumented LSM RPC interfaces for
  session creation and console hand-over" with: the calls are documented [MS-TSTS]
  operations wrapped by WTS functions; only keeping a disconnected session on its hardware
  is undocumented and done in-process and in the kernel.
- §3.11 (second bullet): state what the console-id step achieves (§2.2 above).
- §4 technical side: add the two-limit split of §3.1 / §3.2, and that the console counts
  toward the limit.
- §5: Windows Server for ARM64 is no longer a HYPOTHESIS: only a time-limited Insider preview
  existed and no general release is announced. Add that Windows 11 Enterprise multi-session
  is cloud-only (not a local client escape), and that LTSC and IoT Enterprise are
  single-session (§3.1).
- §6.1 option table:
  - A: add the MultiPoint precedent (§4) and the U8b variant as a later option.
  - B: keep excluded; add "no user-mode equivalent exists" (§5.2).
  - C: keep; note it is the only user-mode path to real per-seat sessions on client SKUs,
    plus the gaps of U1 (fast user switching may be off, one account per seat, serial
    setup, switch trigger rule of §5.5, frozen inactive seats, no system default audio
    change).
  - D: split into D (separate desktops: one at a time only) and D′ (one desktop, per-seat
    windows, seat-aware content only; seat 1 not a normal desktop without the filter).
  - Add rejected rows with one-line reasons: child session (U4), headless/agent sessions
    (U5), per-subsystem console-id fixes (U6), remote seats plus presenter on client (U7),
    own protocol provider on client (U7), termsrv wrapper (U9), VM per seat (U10).
- §6.1 "Rule conflict": unchanged in substance; the client and ARM64 answer is C and D′.
- §7: open question 1 gets the answer "no documented user-mode backend for concurrent
  sessions on client SKUs"; question 14 (kiosk mode) becomes the main client concurrent
  mode question.

Spec 40 §6.3 items 1–2 (virtual monitor in a local non-console session): per §3.3, an own
indirect monitor cannot belong to a local non-console session; only termsrv-created remote
sessions get a sessionized monitor (**HYPOTHESIS**, strong). The virtual-monitor-plus-
presenter design therefore applies to backend D′ (virtual monitor in the console session
itself) and not to a "future local-session backend".

### 6.2 Current implementation (crates/seat-session backends)

| Backend | Change |
| --- | --- |
| `rds_loopback` (default) | No change in design. Its "default" status only makes sense on Server x64; on client SKUs and ARM64 the probe should report it unavailable and the manager should explain why (licence policy, no Server ARM64). |
| `termsrv_wrapper` (experimental) | Conflicts with spec 30 §6 (wrapping termsrv excluded on every SKU), while plan.md (Phase 2 sessions decisions) allows it as an off-by-default experimental module; it also has no ARM64 path. This spec does not support it. Decision tracked as open question 10. |
| `fast_user_switch` | Becomes the client-SKU and ARM64 "real session" backend. Add: refuse when fast user switching is off (both the Winlogon value and the policy, plus domain defaults); require a distinct account per seat; set up seats one after another unless P2 passes; switch only by the trigger rule of §5.5; after a switch, the helper in the new live session applies the seat's display topology (§5.6); do not change the system default audio endpoint (§5.3); tell the user that inactive seats are frozen. Optional: the per-session auto-submit credential provider and filter of §5.4 (P4) instead of asking for the password; built native per architecture. The open item on SYSTEM callers and passwords is settled by P1. |
| `desktops` | `PerSeat` mode: label as "one kiosk at a time" (already noted in code). `Shared` mode becomes D′: needs per-device background Raw Input routing for Pulsar-rendered seats, own cursors, and a clear limit that ordinary focus-driven apps serve one seat at a time. Seat 1 cannot be an ordinary desktop at the same time unless the input filter (spec 50) exists (U3+U1). |
| Backend list / manager | Show per host what each mode really is: "concurrent, full sessions" (Server x64 only), "real sessions, one at a time" (client, all architectures), "concurrent kiosk seats" (client, all architectures). |

## 7. Open questions

1. Can a SYSTEM caller in session 0 hand the console to another user's session with
   `WTSConnectSessionW` without that user's password (P1)? Microsoft Learn does not document
   an exemption for SYSTEM callers.
2. Does a session left at the logon screen survive when the console leaves it (P2)?
3. What survives in a disconnected seat session: GPU devices, full-screen state, audio
   streams, per-user default endpoint (P3)?
4. Does fast user switching behave the same on Home (P1-H)?
5. Is `SessionSecurityEnabled` still honoured on 26100 / 28000 (P6)?
6. The analyses disagree on the exact set of [MS-TSTS] operation numbers the reference
   calls (whether disconnect and logoff go over RPC or not). This does not change the
   conclusion; settle it only if spec 30 needs the detail.
7. Are the private LSM interfaces really used by the reference's in-LSM code (§2.1)? Only
   matters for explaining the missing logon events; Pulsar uses none of it.
8. Will Microsoft document the headless / agent-session family with display and input
   surfaces? Re-check each feature update; until then it is a watch item.
9. Maintainer decision: is "real sessions, one at a time" (U1) plus "concurrent kiosk seats"
   (U3) an acceptable client-SKU product, or does Phase 2 wait for a documented path?
10. Maintainer decision: spec 30 §6 excludes wrapping termsrv on every SKU; plan.md allows an
    experimental, off-by-default `termsrv_wrapper` module. Remove the module, or confirm the
    plan.md exception explicitly (it would stay x64-only)?
11. Can the credential provider apply display topology at LogonUI, and can its lock-screen
    link trigger a switch (P3, P7)?
