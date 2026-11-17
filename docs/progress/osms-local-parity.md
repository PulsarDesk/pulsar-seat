# OSMS-style local seat parity (2026-11-18)

Goal: bring OpenStreamMS (MIT, clone at target/ref/OpenStreamMultiSeat, commit 3b3392c) local-seat quality to Pulsar Seat local seats. Source: workflow wf_23b355d1-951 (read, synthesize, adversarial verify).

## Summary

Neither OSMS nor Pulsar Seat has native ASTER-style ownership of a monitor by a session. An OSMS local seat is a loopback RDP session to 127.0.0.2. The RDP client (wfreerdp or mstsc) runs hidden in session 0 with `+disable-output` and no `/gfx` or `/rfx` (RdpSessionCreator.cs:510-535, spot-checked). A SeatAgent inside the session captures the desktop with DDA into a ring of 3 shared NT-handle textures with keyed mutexes. The service duplicates those handles into a console-session presenter, which shows them in a topmost flip-model window on the seat monitor. That monitor stays part of the console desktop, which belongs to a dedicated host user.

Our rds_loopback / termsrv_wrapper seat has the same shape, with one difference in the frame path. Our image comes through the RDP codec: the server encodes, IronRDP decodes EGFX, and the result goes to the GpuFrameRef presenter. OSMS skips the codec completely by capturing inside the session.

Multi-session is not a differentiator. OSMS ships no termsrv patch. On client Windows its own code comment says only one RDP session is possible and the console user is disconnected (RdpSessionCreator.cs:1432-1440, spot-checked). So OSMS local seats on Pro also need a termsrv wrapper the user installs, or Server RDSH. That is the same limit as our termsrv_wrapper / rds_loopback.

The real gaps are these:
- In-session DDA capture with a zero-codec frame path.
- Keyboard/mouse isolation through a swap to the inbox WinUSB driver. This needs no kernel driver of ours, and our kmdf_filter is unsigned.
- 60+ Hz session composition (DWMFRAMEINTERVAL). We never set it.
- Pass-through of physical gamepads with a per-session ACL, the Guide-button guard and the Win+L guard for the console user.
- An audio jitter buffer.
- Split screen (halves or quarters per monitor).

None of these needs termsrv patching, code injection or a kernel driver of ours. HidHide is an optional third-party kernel filter and can stay optional. Reader disagreement resolved: a local seat's display is the stock RdpIdd. It is RemoteIdd only when the optional OSMS protocol provider is installed. SeatAgent takes whichever AttachedToDesktop output exists (SeatAgent.cs:71), and RemoteIdd mainly serves Sunshine. Paths in osms_files are relative to `C:\Users\ahmet\Projects\pulsar\pulsar-seat\target\ref\OpenStreamMultiSeat`. Paths in ours_files are relative to `C:\Users\ahmet\Projects\pulsar\pulsar-seat`.

## Multi-session note

Separate-session local seats need several concurrent sessions, and both products need that from outside on Home/Pro. OSMS writes fDenyTSConnections=0, fSingleSessionPerUser=0 and fEnableWinStation=1, and starts TermService. Without a third-party termsrv replacement (RDPWrap/TermWrap, detected through a non-stock TermService\Parameters\ServiceDll), only ONE RDP session works and a console user is disconnected (RdpSessionCreator.cs:1432-1440, spot-checked). OSMS's own design also needs the console host user to stay logged in for the presenter. So on stock Pro, an OSMS local seat cannot coexist with its own console presenter: in practice it needs the wrapper, Server with the RDSH role, or the ServerRdsh edition. Home has no RDP server at all. OSMS installs nothing and only detects (RdpWrapperManager.cs:51-82, 110-210). Its RemoteIdd protocol provider also needs a termsrv patch on client SKUs (IsAllowNonRDPStack).

Ours is the same. rds_loopback needs Server RDSH. termsrv_wrapper is an operator-installed wrapper: experimental, off by default, gated by experimental_ack and the experimental-client-sku feature. There is an unresolved doc conflict about ARM64: architecture.md §8 L1427-1429 says it works with a native ARM64 wrapper, but §10.4 L1766-1770 says enable refuses ARM64. Resolve it before promising ARM64 local seats.

Without either, Home/Pro gets FUS (one live seat) or kiosk seats. None of the gaps above changes this. They make a seat on a multi-session-capable host match OSMS's quality: native-rate frames, isolated input without our kernel filter, 60+ Hz, and pads. Making Pro concurrent without a wrapper is impossible in user mode per spec 35 §5.2, and the ASTER-style kernel route is forbidden by rules.md.

## Already equal or better

- Multi-session on client SKUs: equal, and ours is stricter. OSMS ships no patch. Its own comment (RdpSessionCreator.cs:1432-1440) says only one RDP session works on client Windows and the console user gets disconnected. It only detects a third-party ServiceDll (RdpWrapperManager.cs:51-82). Our termsrv_wrapper does that detection too, plus version/ini, Authenticode and PE-machine checks, journaling and an explicit ack gate.
- Mode and resolution changes: ours is better. IronRDP DisplayControl dynamic resize (seat-rdp/src/session.rs:357, 482-529) avoids OSMS's disconnect-and-reconnect of about 1.2 s and its full seat stop/start Readjust.
- Ctrl+Alt+Del: ours is better. RDP seats send the protocol SAS. OSMS has no CAD or SendSAS path at all (ROADMAP.md:15-17).
- Per-seat microphone: equal or better. IronRDP AUDIO_INPUT (MS-RDPEAI) handles the seat mic natively. OSMS needs a patched wfreerdp (FreeRDP/patches/0001-audin-winmm-choose-microphone.patch) and forces FreeRDP over mstsc.
- Session creation security: ours is better. We use our own IronRDP client with TLS pinned to the RDP-Tcp cert hash, credentials from LSA private data over stdin, a contained Low-IL host account, and an NLA-off listener scoped to loopback by a firewall rule. OSMS passes /p on the wfreerdp command line or uses cmdkey, and sets AuthenticationLevelOverride=0 and AllowUnsignedFiles=1 machine-wide without restoring them.
- Zero-copy GPU frame contract: equal. Our GpuFrameRef keyed-mutex protocol (display.rs:233-258) matches the OSMS ring semantics, so a G1 producer can plug in without presenter changes.
- Monitor topology: equal or better. Both use CCD from a console-session helper because session 0 has no displays. Ours adds journaled rollback (display-topology.json), ClipCursor and a SetWinEventHook guard against stray console windows, and an optional Display Core specialized scan-out path. OSMS only places the monitor at a corner, has no rollback journal, and uses raw byte offsets into DISPLAYCONFIG_MODE_INFO.
- Device and USB gating: ours is broader. It covers storage, cameras, printers and smart cards: seat masks with container/hub inheritance, rdpdr redirection only for owned devices, and a minifilter (unsigned). OSMS covers only an SDS ACL per USB port and documents that storage, camera and audio are not isolated.
- Assignment UX: equal. We have a wizard with monitor number overlays, F<N> keyboard picks and mouse clicks, drag-and-drop in the manager, a USB dock wizard and a test tone. OSMS has a dashboard drag-and-drop, press-a-key identify, monitor logo, chime and mic meter. Only the mic meter may be missing on our side (not checked).
- Fallback model on Home/Pro: ours is better. FUS with a credential provider (VM PASS) and kiosk seats exist. OSMS has nothing for Pro without a third-party termsrv patch.
- Steam multi-session: ours is better for clean room. OSMS needs SteamHook DLL injection (Native/SteamHook, CreateRemoteThread+LoadLibraryW, IAT patch). We do no injection beyond the opt-in shim.

## Confirmed gaps

### G1 [P0/L] In-session capture agent (DDA) to shared keyed-mutex texture ring to presenter, with no RDP codec on the local display path

- OSMS: The wfreerdp client runs hidden in session 0 with `/network:lan +disable-output` and no `/gfx` or `/rfx`. The comment says /gfx makes termsrv H.264-encode and fight for the GPU (RdpSessionCreator.cs:510-535). SeatAgent (Services/Seats/SeatAgent.cs:43-391) runs inside the seat session. It uses EnumAdapters1/EnumOutputs (first AttachedToDesktop output, line 71), D3D11CreateDevice, IDXGIOutput1.DuplicateOutput and a ring of 3 B8G8R8A8 textures (SHARED_NTHANDLE plus SHARED_KEYEDMUTEX, CreateSharedHandle). Each frame is CopyResource under the keyed mutex (producer acquires 0, releases 1). It sends the slot index plus LastPresentTime over the pipe. On DXGI_ERROR_ACCESS_LOST it re-captures after OpenInputDesktop/SetThreadDesktop (463-484). A cursor poller (GetCursorPos every 1 ms, timeBeginPeriod(1)) carries position. DDA supplies only the shape. The agent calls SetGPUThreadPriority(7). The service brokers handles with OpenProcess(PROCESS_DUP_HANDLE) and DuplicateHandle (HandleBroker.cs:35-54). The presenter opens them with OpenSharedResource1 and presents with FLIP_DISCARD, MaximumFrameLatency=1 and a waitable object (SeatHost.cs:265-422). The doc claims about 2 ms from render to screen. If the seat needs a password, the agent runs as SYSTEM in the session: DuplicateTokenEx of the service token plus SetTokenInformation(TokenSessionId) (ProcessInSession.cs:250-268). That lets it capture and inject on the Winlogon desktop.
- OSMS files: Core/Helpers/RdpSessionCreator.cs:463-594, Services/Seats/SeatAgent.cs:43-484, Services/Seats/HandleBroker.cs:35-54, Services/Seats/SeatHost.cs:154-433, Core/Helpers/ProcessInSession.cs:149-268, Services/Seats/SeatLatency.cs
- Ours today: The seat host in the console session is the RDP client: IronRDP receives EGFX/bitmap updates, decodes H.264 through Media Foundation, and hands GpuFrameRef (NT handle plus keyed mutex: producer releases key 1, presenter acquires 1 and releases 0) to the window_presenter FrameSink. This adds a server-side encode, a client decode, lossy H.264 artefacts and RDP flow-control latency. The cursor comes from RDP pointer PDUs. No DDA producer exists anywhere in crates/: a grep for DuplicateOutput found no hits.
- Our files: crates/seat-core/src/backend/display.rs:218-307 (FrameSink, GpuFrameRef), crates/seat-host/src/ironrdp.rs, crates/seat-rdp/src/gfx.rs, crates/seat-display/src/win/presenter.rs, crates/seat-display/src/win/flip.rs
- Proposal: 1. Add a local frame-source choice: `LocalFrameSource { RdpDecode (current), SessionCapture }` in seat-core config (`LocalSeat.frame_source`, defaulting to SessionCapture when available).
2. Add a new seat-host role `pulsar-seat-host --session-capture --seat N --pipe <name>` (crates/seat-host/src/capture/{mod.rs,win.rs}). The service launches it into the seat's RDP session through the existing spawn path under spawn_lock. It uses WTSQueryUserToken, or for lock-screen capture a SYSTEM token with TokenSessionId behind a per-seat setting. It produces textures under the exact GpuFrameRef key protocol, so the presenter is reused unchanged.
3. Add the handle broker in seat-service (`win/handle_broker.rs`, DuplicateHandle producer to host). The host is Low-IL and contained, so it cannot open the producer itself.
4. In seat-rdp, once SessionCapture is live, send TS_SUPPRESS_OUTPUT_PDU (allowDisplayUpdates=0, MS-RDPBCGR 2.2.11.3) and drop the EGFX channel. HYPOTHESIS: termsrv then stops encoding while DDA inside the session still sees RdpIdd frames; prototype this on Server RDSH.
5. Add a fast cursor path: the agent sends GetCursorPos plus the DDA shape to the FrameSink::cursor of the existing CursorState.
6. Fall back to RdpDecode on adapter LUID mismatch (seat monitor GPU differs from the session render GPU), on DDA failure, or on ARM64 drivers without DDA support.
7. Add per-span QPC latency telemetry (read to inject, compose to screen).

x64 and ARM64: windows-sys DXGI/D3D11 only. HYPOTHESIS: DDA works in RDP sessions on Adreno.
- Verifier evidence: REFUTATION FAILED. The gap is real.

OURS: grep for DuplicateOutput, IDXGIOutputDuplication, AcquireNextFrame, SessionCapture and frame_source in crates/, docs/ and spec/ (excluding target/) found no capture producer. The only GpuFrameRef producers are the RDP decode paths: crates/seat-rdp/src/session.rs:725 and crates/seat-host/src/ironrdp.rs:222. The consumers already exist and can be reused: crates/seat-display/src/win/presenter.rs:542 (present_gpu) and crates/seat-display/src/win/flip.rs:17-48 (keyed-mutex open on the chain's device; the cross-device sharing is marked an untested HYPOTHESIS). In-session capture appears only as an untested plan:
- spec/remote/40-display-hdr.md:425-435 (§6.5, marked [?]). It also cites "80 §3.2 states it is unavailable for RDP indirect displays [I]".
- spec/remote/40-display-hdr.md:570 (open question 2).
- crates/seat-display/README.md:159.
- docs/split/agents/pulsar-host-backend.md:258-275 (the old pulsar-capture DXGI path, not ported).
The spec/local/40-display.md:480-500 plan for local seats is a custom IddCx driver plus DisplayManager. It does not use DDA.

OSMS: this is implemented and wired, not just a TODO. The docs/SEAT-AGENT-TODO.md header still says "pendiente", but the code below shows it is done:
- Services/Seats/SeatManager.cs:344-348 and 394-475. OpenPresentation launches `--seat-agent <pipe>` through ProcessInSession.LaunchInSession, asSystem when the seat requires login. It accepts the pipe only from agentPid, receives the texture handles and calls HandleBroker.DuplicateHandleTo(agentPid, h, hostPid) at :446.
- Services/Seats/SeatAgent.cs:60-75 enumerates outputs and takes the first AttachedToDesktop one, optionally matched by --output name. :106 calls DuplicateOutput. :450-460 calls SetGPUThreadPriority(7). :463-484 is Recapture: AttachThreadToInputDesktop, then DuplicateOutput again, retrying every 250 ms.
- Services/Seats/HandleBroker.cs:35-54 uses OpenProcess(PROCESS_DUP_HANDLE) and DuplicateHandle with DUPLICATE_SAME_ACCESS.
- Services/Seats/SeatHost.cs:294 calls OpenSharedResource1.
- Core/Helpers/ProcessInSession.cs:250-268 uses DuplicateTokenEx plus SetTokenInformation(TokenSessionId=12).
- Core/Helpers/RdpSessionCreator.cs:509-536 passes no /gfx or /rfx (the bisect comment names commit 0ad97d6 as the regression), /network:lan and +disable-output.
- SeatManager.cs:707 reuses the same agent with `--output <gdi> --no-audio` on the console session for split screens.
OSMS also has a separate driver path (Native/RemoteIdd/FramePublisher.cpp:128-162, FrameConsumer.cpp:90-114, SharedFrame.h). It is its own IddCx driver used for streaming, and it is not needed for this gap.

OSMS uses no termsrv patch, injection or kernel driver on this path. Everything is documented user-mode DXGI/D3D11, WTS/token and DuplicateHandle calls. Its own docs claim this works in practice: about 2 ms from render to screen, measured in their seat-tests. That evidence contradicts the [I] note in our spec, so our §6.5 test is the deciding experiment.

Ours is clearly weaker. A local seat today pays for server-side encode plus client decode, lossy H.264 and RDP flow control.
- Corrected proposal (wins over Proposal): The direction is right. Correct these points:

(a) Make the default RdpDecode, not SessionCapture. Promote SessionCapture to default only after the spec/remote/40-display-hdr.md §6.5 test passes on x64 and Adreno. Both are HYPOTHESES today, and our own spec cites a claim that DDA is unavailable on RDP indirect displays.

(b) Step 4: copy the OSMS approach first. When SessionCapture is chosen, do not negotiate EGFX (legacy bitmap path only), and make the IronRDP client discard bitmap updates instead of decoding them (the equivalent of +disable-output). Treat TS_SUPPRESS_OUTPUT_PDU as a second, measured experiment. It may stop DWM/RdpIdd presenting, which would starve DDA. Verify that AcquireNextFrame keeps getting frames before relying on it.

(c) Pipe the agent to the service only, never to the Low-IL host. Accept only the launched PID; OSMS does this with AcceptFrom(pid == agentPid). The service duplicates the ring handles into the host process. Reuse our existing GpuFrameRef key order: the producer acquires 0 and releases 1. This matches OSMS, so presenter.rs and flip.rs stay unchanged.

(d) Choose the output by the RdpIdd monitor's DeviceName, or by the adapter LUID that matches the seat monitor's GPU. Do not take the first attached output: with Server RDSH or multi-monitor sessions it can be the wrong one. Fall back to RdpDecode when the LUIDs differ, rather than adding a cross-adapter copy.

(e) Steps 5 and 7 are fine. One addition to 5: when the agent runs as SYSTEM, it must call OpenInputDesktop/SetThreadDesktop before DuplicateOutput is retried, or capture stays dead on Winlogon or UAC.

(f) The same agent with an output override can later replace the split-screen console path, as OSMS does at SeatManager.cs:707. Note this; it is not P0.

(g) Clean room: implement from the Microsoft DXGI Desktop Duplication and DuplicateHandle docs only. OSMS is MIT, so copied code would need attribution, but none is needed.
- Rules / risk: No conflict: documented DXGI, WTS, CreateProcessAsUser and DuplicateHandle; no injection and no driver. Two points to watch. A SYSTEM process in a user session widens attack surface: keep it optional, restricted to the secure-desktop case, with a pipe ACL of SYSTEM plus the service PID check and a no-command surface. Clean room: reimplement from the Microsoft DXGI docs, not OSMS source (MIT, so attribution is required if any code is reused). Still needs a Server RDSH machine, or the operator's wrapper, to test.
- Status: TODO

### G2 [P0/M] Keyboard/mouse isolation by swapping the USB node to inbox winusb.inf, with a user-mode HID reader

- OSMS: UsbCapture.FindTargets matches the device by DEVPKEY_Device_LocationPaths port. For composite devices it takes only the &MI_xx interfaces whose children are Keyboard or Mouse class. Capture works in four steps:
1. Journal the original ClassGUID and InfPath to %ProgramData%\OpenStreamMS\usb-capture.json.
2. Write Enum\<id>\Device Parameters\DeviceInterfaceGUIDs.
3. Set SPDRP_CLASSGUID to USBDevice {88BAE032-...}.
4. SetupDiSetDeviceInstallParamsW (DI_ENUMSINGLEINF, DriverPath=%windir%\INF\winusb.inf), then SetupDiBuildDriverInfoList and newdev!DiInstallDevice.
The console then no longer sees a keyboard or mouse, so no shield is needed. UsbHidReader runs in the service (session 0): CM_Get_Device_Interface_List, CreateFile, WinUsb_Initialize, the first interrupt-IN pipe, and HID report descriptor GET_DESCRIPTOR(0x22) with a boot-protocol fallback. HidReportLayout.Parse and HidTranslator map reports to RI_MOUSE_* and set-1 scancodes. Events go to the agent, which calls SendInput. Release reinstalls the journaled InfPath or input.inf. Recovery: LoadJournal, CM_Register_Notification arrival, RestoreAll at service start, and on uninstall/purge. Known gaps: keyboard LEDs stay dark, no hot-plug into a running seat, USB only (no Bluetooth), hard-coded x64 struct sizes, and a non-atomic journal write.
- OSMS files: Services/Seats/SeatUsbCapture.cs:24-510, Services/Seats/SeatHid.cs:1-277, Services/Seats/SeatManager.cs:583-623, Core/Helpers/DeviceTopology.cs:108-159, docs/puestos-locales.md:149-152
- Ours today: raw_input_host is the default. Under seat_one_console it does not isolate: other seats' keys reach seat 1's foreground window, and the manager shows input_not_isolated. Isolation needs dedicated_host, or our KMDF upper filter drivers/seat-input-filter. That driver is unsigned and passed only test-signed in the VM. spec 50 §4 says that if it cannot be signed, local input isolation is dropped.
- Our files: crates/seat-input/src/win/raw.rs, crates/seat-input/src/raw_router.rs, crates/seat-input/src/kmdf_router.rs, crates/seat-core/src/backend/input.rs, drivers/seat-input-filter, docs/agents/architecture.md:917-963
- Proposal: 1. Add `InputBackendKind::WinusbCapture` in seat-core, behind an InputRouter implementation in seat-input. New modules:
   - `win/usb_capture.rs`: SetupDi plus DiInstallDevice through windows-sys structs. Use no hard-coded sizes, so ARM64 is safe.
   - `hid_layout.rs`: a pure, unit-tested HID report-descriptor parser.
   - `hid_keys.rs`: the HID usage to set-1 table, from Microsoft's public Keyboard Scan Code Specification.
   - `win/winusb_reader.rs`.
2. Select units by our existing device key (location path plus interface number), not by port only.
3. Run readers in the service. Forward events over the existing host pipe to the seat host's RdpInputSink (fast-path scancode/relative input), or to the G1 agent with SendInput if that lands.
4. Journal in %ProgramData%\Pulsar Seat\journal\usb-capture.json with an atomic tmp+rename. Restore at service start, on a CM_Register_Notification arrival, on seat stop, and in the MSI uninstall custom action.
5. Add a safety net OSMS lacks. Validation refuses to capture the last console keyboard/mouse. A boot-time restore runs from the service's early start, plus a scheduled task at startup.
6. Do better than OSMS on LEDs. Map RDP TS_SET_KEYBOARD_INDICATORS_PDU from the seat session to a WinUSB SET_REPORT(Output) on the captured keyboard.
7. Hot-plug: on CM arrival on an assigned port while the seat runs, capture immediately.
8. Keep kmdf_filter for Bluetooth and non-USB devices.
- Verifier evidence: I could not refute the gap. OSMS has this capability and we do not.

OSMS side (checked in the clone, read-only):
- Services/Seats/SeatUsbCapture.cs
  - Journal file: line 27 (`%ProgramData%\OpenStreamMS\usb-capture.json`).
  - Our own interface GUID: line 40. USBDevice class GUID {88BAE032-5A81-49f0-BC3D-A4FF138216D6}: line 44.
  - FindTargets matches by USB port. For composite devices it keeps only the &MI_ nodes that have Keyboard/Mouse class children: lines 51-67.
  - Capture: lines 70-99. It writes `Enum\<id>\Device Parameters\DeviceInterfaceGUIDs`, then calls Setup.Install with winUsb:true.
  - Release reinstalls the journaled INF, falling back to input.inf: lines 105-120.
  - RestoreAll: lines 123-134. WatchArrivals over CM_Register_Notification: lines 141+.
  - The DiInstallDevice P/Invoke from newdev is at line 240. It forces `%windir%\INF\winusb.inf`: lines 277-315.
  - WinUsb_Initialize is called at line 418. The HID report descriptor is read with GET_DESCRIPTOR(0x22): lines 432-471.
- Services/Seats/SeatHid.cs maps HID usages to set-1 scancodes (around lines 244-253).
- RestoreAll is called at service start (SeatManager.cs:38, Program.cs:529) and on data purge (Core/Helpers/DataPurge.cs:40).
- It uses only Microsoft inbox winusb.inf/input.inf, SetupAPI and newdev. It needs no termsrv patch, injection, kernel driver or third-party driver.

Our side (crates/, spec/, docs/agents/, excluding target/):
- A WinUSB / SetupDi / DiInstallDevice / driver-swap path for input exists nowhere. WinUSB appears only in seat-pads usbip, remote wiring, and a passing mention in spec/local/70-usb-devices.md:449.
- docs/agents/architecture.md:917-962 lists only these input backends:
  - `raw_input_host`: isolation complete only under `dedicated_host`. Under seat_one_console, seat-1 keys reach the console foreground window.
  - `kmdf_filter`: not signed.
  - `none`.
- spec/local/50-input.md:443-469 (§4.1.2) designs isolation only as a KMDF upper filter that needs Microsoft signing. Lines 625-626 say that if it cannot be signed, extra seats get no local-input isolation.
- So for USB keyboards and mice, ours is clearly weaker on a Secure Boot / HVCI PC without a signed driver.
- Nothing in docs/agents/rules.md forbids rebinding a device to an inbox INF. Spec 50 §4 constraints (no injection, works with HVCI, prefers user mode) are met.
- Corrected proposal (wins over Proposal): The proposal is mostly right. These corrections fit our architecture better.

1. **Plug into the existing packet path. Do not add a new host-side path.** kmdf_router.rs already does post-processing (hotkey, emergency, SAS, key-up synthesis after GAP) and feeds `PacketSink` (`InputRouter::set_packet_sink`, kmdf_router.rs:655). The seat host already consumes `InputPacket` through seat-host/src/input.rs and seat-input/src/rdp.rs into the RDP session.
   - The WinUSB reader in the service should emit `InputPacket::{Key, Move, Buttons, Wheel, Consumer}` into the same sink.
   - Factor the shared post-processing out of kmdf_router (call it `packet_router`). Then `WinusbCapture` and `KmdfFilter` both reuse it.
   - The hotkey and emergency combination keep working.
   - Do not route through SendInput or the G1 agent. Our seats are RDP sessions, so fast-path injection through the host is the right sink.
2. **Make it a per-unit mechanism inside a hybrid router, not a global replacement.**
   - USB units assigned to seats 2+ get captured.
   - Seat-1 units stay on HID.
   - Bluetooth, I2C/SPI/ACPI and non-USB units fall back to raw_input_host, or kmdf_filter where signed. RouteReport keeps the `input_not_isolated` note per unit.
   - The console listener, wizard capture and activity reporting must also take activity from the WinUSB reader. A captured unit vanishes from raw input, so the capture wizard would no longer see it.
   - The capture wizard and device assignment therefore must run before capture, or must release units during the wizard.
3. **Capture granularity.** Our unit key is a HID collection (spec 50 §4.1.1). Swapping a USB interface node takes every collection under that &MI_xx node. The manager must group those units and force them onto one seat. The 4.1.1 rule for colliding units fits this case.
   - Refuse capture when the interface also carries non-input collections the user assigned elsewhere.
   - Refuse capture of the last console keyboard or mouse.
4. **Recovery.**
   - Journal at `%ProgramData%\Pulsar Seat\journal\usb-capture.json`. Write it with tmp+rename (MoveFileExW with MOVEFILE_REPLACE_EXISTING|WRITE_THROUGH), and write it before the swap.
   - Restore at service start, on CM_Register_Notification arrival, on seat stop and when the product is disabled, and in the MSI uninstall custom action.
   - The boot scheduled task is optional because the service runs early.
   - Also add a manual "restore input devices" command in the manager and in the troubleshooting doc.
5. **ARM64.**
   - Use windows-sys SP_DEVINFO_DATA / SP_DRVINFO_DATA_V2_W with size_of. No hard-coded sizes.
   - newdev.dll exports DiInstallDevice on ARM64. Verify this on the ARM64 box before relying on it.
   - Most ARM64 laptop input is I2C/SPI and cannot be captured this way. Keep the fallback.
6. **LEDs.** Send the TS_SET_KEYBOARD_INDICATORS_PDU from the seat session to the device as a WinUSB control SET_REPORT(Output). Keep this P1, not part of the P0 scope.
7. **Clean room.** Implement from Microsoft's WinUSB, SetupAPI and newdev docs, the USB HID 1.11 spec and HID Usage Tables, and the Keyboard Scan Code Specification. Do not port OSMS code. Record the decision in plan.md and spec 50 §4 as a new building block (user-mode isolation for USB units without a signed driver).
8. **Test only in the VM first.** A failed restore leaves the console without a keyboard. Test with USB passthrough in the VM before any run on the maintainer's machine.

Priority P0 and size M are right. Size grows to M/L once the shared post-processing extraction and the wizard interplay are included.
- Rules / risk: Rules allow this: only Microsoft's inbox winusb.inf and input.inf, no kernel code of ours, no class-wide filter, and documented SetupAPI/newdev/WinUSB. It works with HVCI and Secure Boot. Risks:
- A bricked keyboard if the service never restarts. The safety net and console-device rule above mitigate this.
- PnP restart of about 1 s per capture.
- Clean room: write from the MS WinUSB/HID docs and the USB HID spec, not from OSMS source.
Prefer this over raw_input_host as the default isolation for USB devices.
- Status: TODO

### G3 [P0/S] RDP session composition at 60+ Hz (DWMFRAMEINTERVAL) and per-seat mode/Hz

- OSMS: Before every connect, RdpSessionCreator sets MaxFrameRate and DWMFRAMEINTERVAL. These are Microsoft-documented WinStations values, written under Policies\Terminal Services, Terminal Server\WinStations and WinStations\RDP-Tcp (788-850; compose interval formula, 60 fps gives 4 ms). It also sets fEnableHardwareGraphicsCapture=1 and bEnumerateHWBeforeSW=1 (887-897), DefaultSettings.VRefresh on the RDP adapters (1015+), and 'BetterRDP' values (TermDD FlowControl*, SystemResponsiveness=0). Changing mode or Hz means a reconnect of about 1.2 s (Reconnect 231-287). The values are machine-wide and are never restored.
- OSMS files: Core/Helpers/RdpSessionCreator.cs:788-1015, Core/Helpers/RdpSessionCreator.cs:231-287, Services/Seats/SeatManager.cs:1003-1011
- Ours today: rds.rs writes only bEnumerateHWBeforeSW (and Shadow), with a do-not-fight-GPO rule. A grep finds no DWMFRAMEINTERVAL or MaxFrameRate anywhere in crates/. RDP sessions stay at the stock composition rate of about 30 fps, which makes a 'real monitor' seat feel wrong. Mode changes use the DisplayControl dynamic channel (seat-rdp/src/session.rs:357, 482-529), so no reconnect is needed. That part is better than OSMS.
- Our files: crates/seat-session/src/rds.rs:145-170, 560-600, crates/seat-rdp/src/session.rs:357-529
- Proposal: 1. In seat-session/src/rds.rs, add journaled values: DWMFRAMEINTERVAL under WinStations, and the frame-rate cap policy. Use the existing `r(...)` table with 'only while absent / do not fight GPO' semantics and a restore on uninstall.
2. Derive the interval from the highest Hz among assigned seat monitors. It is global, so warn in the manager that it is per machine.
3. With G1, the presenter is a flip-model window and VRR (FreeSync/G-Sync) on the seat monitor smooths the mismatch.
4. Expose a 'session fps' setting in OutputsDialog.
- Verifier evidence: Gap confirmed. I found no existing implementation to refute it.

OSMS side, checked against source:
- `Core/Helpers/RdpSessionCreator.cs:788-817` `ApplyFrameRatePolicy` writes `MaxFrameRate` to `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services`.
- The same function writes `DWMFRAMEINTERVAL` (DWORD, ms) to four keys: the Policies key above, `SOFTWARE\Microsoft\Windows NT\Terminal Services`, `SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations` and `WinStations\RDP-Tcp`.
- `CompositionIntervalMs` (840-850) picks the interval. In Smooth mode 60 fps gives 4 ms (250 Hz). Efficient mode rounds 1000/fps (60 gives 17 ms).
- Lines 875-896 set `fEnableHardwareGraphicsCapture=1` (RDP-Tcp) and `bEnumerateHWBeforeSW=1` (policy).
- Lines 904-938 write the 'BetterRDP' values: TermDD `FlowControl*` and `SystemProfile SystemResponsiveness=0`.
- Lines 1008-1059 write `DefaultSettings.VRefresh` on the virtual adapters.
- `docs/puestos-locales.md:15` says the RDP refresh rate is global (`DWMFRAMEINTERVAL`), cannot be set per session, and FreeSync adapts each monitor.
- `DiagnosticsBundle.cs:162` reads `DWMFRAMEINTERVAL` back.
- None of these are executables or drivers. Nothing here relies on termsrv patching or code injection.

Our side:
- A grep of the whole pulsar-seat tree (excluding target/) for `DWMFRAMEINTERVAL|MaxFrameRate|VRefresh|fEnableHardwareGraphicsCapture` finds nothing in crates or docs. The only hits are the `hypervRefresh` i18n strings.
- `crates/seat-session/src/rds.rs:157-170` `rds_rows()` has 9 rows. `bEnumerateHWBeforeSW` is scope `Remote` (line 168), so LOCAL seats do not even get the hardware-encoder policy.
- `crates/seat-rdp/src/session.rs` has no frame-rate or Hz handling. It only does Refresh Rect / Suppress Output (lines 436, 594) and DisplayControl resize.
- So local seats compose at the stock RDP cap of about 30 fps. The gap is real.

Two corrections to the claim:
- `MaxFrameRate` is not a Microsoft-documented value; it is folklore. Microsoft documents only `DWMFRAMEINTERVAL` under `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations` (decimal 15 = 60 fps, per the 'Remote Desktop frame rate is limited to 30 FPS' article). The 'all documented' rule_risk is therefore wrong for `MaxFrameRate`.
- OSMS's 4 ms 'Smooth' interval is tuned for Sunshine re-pacing a capture. Our local presenter is the seat-rdp client drawing to a real monitor, which is a different consumer.
- Corrected proposal (wins over Proposal): 1. In `crates/seat-session/src/rds.rs` `rds_rows()`, add one journaled row: `r(TS_WINSTATIONS, "DWMFRAMEINTERVAL", <ms>, "dwm_frame_interval", Common)`. Scope is Common so local seats get it too, and it is restored on uninstall. Use the existing only-while-absent / do-not-fight-GPO semantics and report a `PolicyOverride` warning if a different value is already present. The data is computed, so the row needs a dynamic value instead of the static `r(...)` constant.
2. Do not write `MaxFrameRate`, the Policies or `SOFTWARE\Microsoft` copies, `VRefresh` on RDP adapters, or the TermDD FlowControl / `SystemResponsiveness` values. None of them is Microsoft-documented. If hardware tests show `WinStations` alone does not lift the cap, add `RDP-Tcp\DWMFRAMEINTERVAL` (as OSMS does) behind a HYPOTHESIS note.
3. Compute the interval from the highest refresh rate among assigned local seat monitors. Use ms = max(2, round(1000/Hz)) as the default, for example 60 Hz gives 17 ms (OSMS 'Efficient'). Offer a 'smooth' option at 4 ms if presenter judder shows up. Measure it, because OSMS's 4 ms choice was made for Sunshine, not for a local presenter.
4. The value only takes effect for new sessions, so a change needs a session reconnect, and the manager must say so. Mode changes stay on DisplayControl, which is better than OSMS's 1.2 s reconnect.
5. Also move `bEnumerateHWBeforeSW` (rds.rs:168) from `Remote` to `Common`. Today local seats compose without the hardware path.
6. Manager (OutputsDialog): show one 'session frame rate (machine-wide)' setting with a warning that it applies to every seat. VRR on the seat monitor (G1 flip-model presenter) absorbs the mismatch.
7. Do the hardware tests on both x64 and ARM64 before shipping.
- Rules / risk: None. These are documented RDS registry values, journaled and reversible. Do not copy the undocumented 'BetterRDP' TermDD FlowControl values unless Microsoft documents them.
- Status: TODO

### G4 [P1/S] Lock guard for the console host user (console Win+L or secure desktop blanks every seat)

- OSMS: ConsoleLockGuard.Engage resolves the console user SID and sets HKU\<sid>\Software\Microsoft\Windows\CurrentVersion\Policies\System\DisableLockWorkstation=1 while any seat runs. The code notes the HKLM copy is ignored. The previous value is journaled to console-lock.json and restored on the last seat stop, at the next service start, or on uninstall (ConsoleLockGuard.cs:31-88). RdpSessionCreator.PreventSessionLock also writes HKLM InactivityTimeoutSecs=0 (1425-1430, spot-checked). The docs recommend a host user without UAC on the secure desktop and with display power-off disabled. Exclusive seats get NoClose=1 and the Users shutdown right removed (IsolatedSessionPolicy, ShutdownRight).
- OSMS files: Services/Seats/ConsoleLockGuard.cs:1-89, Core/Helpers/RdpSessionCreator.cs:1414-1430, Core/Helpers/IsolatedSessionPolicy.cs, Services/Seats/SeatManager.cs:45, 545-546
- Ours today: The window_presenter spec admits that seats blank on console lock, UAC or the secure desktop. A grep finds no DisableLockWorkstation write in crates/; seat-host/src/agent.rs uses LockWorkStation only for the FUS switch. Under dedicated_host, the host account's console can still be locked, screen-saved or powered off.
- Our files: crates/seat-display/src/win/presenter.rs, crates/seat-service/src/win/containment.rs, docs/agents/architecture.md:190-198
- Proposal: 1. In seat-service, add a `console_guard.rs` used under dedicated_host. While any presenter-backed seat runs, write HKU\<host SID>\...\Policies\System\DisableLockWorkstation=1, together with the screen-saver and power policies (ScreenSaveActive=0, and the console display timeout through PowerWriteACValueIndex for the active scheme). Journal everything under %ProgramData%\Pulsar Seat\journal and restore on the last stop, at service start and on uninstall.
2. Under seat_one_console, do not force it on the human seat-1 user. Show a notice instead, plus a per-seat opt-in.
3. Exclusive seats: optional NoClose and shutdown-right removal, through the existing compat tweak journal.
- Verifier evidence: The gap is real. We do part of it already (display/sleep power-off), so the proposal needs changes.

OSMS (read only, MIT):
- Services/Seats/ConsoleLockGuard.cs:20-88 resolves the console user via ConsoleSession.TryGetUser, then NTAccount to SID.
- It writes HKU\<sid>\Software\Microsoft\Windows\CurrentVersion\Policies\System\DisableLockWorkstation=1 (DWORD).
- It journals {Sid, User, Previous} to %ProgramData%\OpenStreamMS\console-lock.json before the write. Engage is idempotent: it skips if the journal exists.
- Release restores the previous value, or deletes the value if there was none. If the user's hive is unloaded it keeps the journal and retries at the next start.
- The comment at lines 14-16 says the HKLM copy is ignored by Windows.
- Core/Helpers/RdpSessionCreator.cs:1425-1430 PreventSessionLock writes HKLM ...\Policies\System InactivityTimeoutSecs=0 and DisableLockWorkstation=1.
- No termsrv patch, injection, kernel driver or third-party driver is involved. These are plain documented registry policy values.
- OSMS forces this on whoever is at the console. It does not separate a host account from a human user.

Ours (pulsar-seat):
- A grep of crates/, apps/manager and docs finds no DisableLockWorkstation, InactivityTimeoutSecs, ScreenSaveActive or NoClose write.
- LockWorkStation only appears in crates/seat-host/src/agent.rs:322-413, where it locks for the FUS switch.
- docs/agents/architecture.md:190-197 accepts that under seat_one_console a lock, UAC prompt or sign-out of seat 1 hides every other seat.
- dedicated_host needs host-account auto-logon. spec/local/30-sessions.md:445-452 says that account must have "lock and screen saver disabled", but nothing implements it.

Already present, which refutes part of the proposal:
- crates/seat-compat/src/tweaks.rs:352-365, TweakId::SleepOff, writes active-scheme AC STANDBY_IDLE=0 and VIDEO_IDLE (3c0bc021-...)=0 and turns off USB selective suspend.
- It goes through PowerWriteACValueIndex plus PowerSetActiveScheme (crates/seat-compat/src/win.rs:328-344) and is journaled (journal.rs:742, 975-992).
- It can be set from the installer or CLI with --tweak=sleep_off (setup.rs:541-552, cli.rs:511).
- So the "console display timeout through PowerWriteACValueIndex" part already exists and is machine-wide.
- The compat journal already supports Hive::Users, i.e. HKEY_USERS through with_user_hive (crates/seat-compat/src/ops.rs:17-25), with undo on uninstall/reset.
- So a new %ProgramData% journal is not needed.

Still missing: per-user DisableLockWorkstation, the per-user screen saver setting, and NoClose / shutdown-right removal for exclusive seats. Our seat hosts run with a hardened token without SeShutdownPrivilege (architecture.md:128; host_account.rs:152), but that covers the host processes, not the seat users.
- Corrected proposal (wins over Proposal): 1. Add the lock and screen-saver guard as compat-journal entries, not a separate console_guard.rs with its own journal.
   - Under dedicated_host, at host-account setup, apply these per-user values once through seat-compat with Hive::Users plus with_user_hive (keyed by the host account SID):
     - HKU\<host SID>\Software\Microsoft\Windows\CurrentVersion\Policies\System\DisableLockWorkstation=1
     - HKU\<host SID>\Software\Policies\Microsoft\Windows\Control Panel\Desktop\ScreenSaveActive="0" (REG_SZ)
   - This account exists only to run the hosts, so a permanent setting is fine. It does not need to switch on and off with the seats.
   - Revert happens on host-account removal and uninstall through the existing journal (remove_all path).
2. Display power-off: do not write new power code.
   - Under dedicated_host, require or auto-apply the existing TweakId::SleepOff (VIDEO_IDLE / STANDBY_IDLE = 0, journaled).
   - The manager's preflight should flag it as missing when dedicated_host is on.
3. seat_one_console: never write anything to the seat-1 human user by default.
   - Show a manager notice: "locking seat 1 hides all seats".
   - Add an opt-in "prevent seat-1 lock while seats run". It uses the same Hive::Users journal entry and is applied and released with OSMS-style engage/release semantics on first seat start / last seat stop. Service start reverts a stale entry.
   - Skip HKLM DisableLockWorkstation (OSMS notes Windows ignores it). Treat HKLM InactivityTimeoutSecs=0 as a separate, optional machine policy.
4. Exclusive seats: NoClose and Users shutdown-right removal as optional, journaled compat tweaks (same as the original proposal).
5. Mark as HYPOTHESIS until tested on hardware:
   - whether DisableLockWorkstation also blocks the Win+L path under auto-logon;
   - whether the UAC secure desktop still blanks the presenters. That needs PromptOnSecureDesktop=0, which is a security trade-off and must stay opt-in with the maintainer's OK.
- Rules / risk: None: documented per-user policy values, journaled. UX risk: the console user cannot lock, so make the choice visible in the manager.
- Status: TODO

### G5 [P1/M] Physical gamepad pass-through per seat session (ACL plus node restart; optional HidHide jail) and Guide button guard

- OSMS: GamepadSessionIsolation matches controllers (XUSB {d61ca365}, GIP {05f5cfe2}, HID with HID_DEVICE_SYSTEM_GAME) to a seat by USB port. If HidHide.sys >= 1.4.181 is present, it writes `<instanceId>!<sessionId>` jail entries through IOCTLs on \\.\HidHide. Otherwise it falls back to SeatDeviceAccess: SPDRP_SECURITY_SDS `D:P(A;;GA;;;SY)(A;;GA;;;<seat SID>)` on the node and its children, then DIF_PROPERTYCHANGE restart, journaled. OSMS measured (asignacion-dispositivos.md:11-38) that DEVPKEY_Device_SessionId only hides enumeration (CR 13) yet the device still opens by path, and that Windows clears it on every device start. Only the SDS ACL plus a restart really blocks access (error 5). It sets HKLM ControllerToVKMapping\Enabled=0. GuideButtonGuard sets HKU\<sid>\Software\Microsoft\GameBar\UseNexusForGameBarEnabled=0 for seat and console users, ref-counted, saving the previous value in the user hive.
- OSMS files: Services/Gamepad/GamepadSessionIsolation.cs:15-865, Services/Gamepad/HidHide.cs, Services/Seats/SeatDeviceAccess.cs:1-340, Services/Seats/GuideButtonGuard.cs:1-121, docs/asignacion-dispositivos.md:9-97
- Ours today: Local pads are opt-in (`LocalSeat::virtual_pads`, default off). They read the physical pad in session 0 and recreate a virtual pad through ViGEmBus or usbip-win2 (third-party) or split-pad (ours, unsigned), secured with SPDRP_SECURITY_SDS plus DEVPKEY_Device_SessionId (architecture.md:1879, 1909). pad_device_scope re-ACLs the physical HID nodes, default off. There is no Guide/Game Bar guard and no ControllerToVKMapping handling. The whole path is HYPOTHESIS and has never run on hardware.
- Our files: crates/seat-input/src/local_pads.rs, crates/seat-input/src/pad_scope.rs, crates/seat-pads, crates/seat-service/src/local_pads.rs, docs/agents/architecture.md:1868-1921
- Proposal: 1. Add `PadMode::Passthrough` as the default for local RDP seats. pad_scope.rs applies the seat logon/user SID ACL to the physical pad's HID/XUSB/GIP nodes and descendants, then calls the existing devtree restart (DIF_PROPERTYCHANGE) so the other session's open handles close. Journal it, as pads\scope.json already does.
2. Keep virtual pads only for remapping or DualSense feature cases.
3. Stop relying on DEVPKEY_Device_SessionId for isolation. Keep it as cosmetic only, and re-apply it on each device start. Record OSMS's measurement as a HYPOTHESIS to verify in the VM.
4. Add a `guide_guard` in seat-service: UseNexusForGameBarEnabled=0 per affected user, journaled in %ProgramData% (OSMS has no startup restore; we add one), plus ControllerToVKMapping\Enabled=0, journaled.
5. HidHide is an optional detected backend (`pad_jail_hidhide`), never bundled.
- Verifier evidence: Partial refutation only. Most of the core mechanism already exists in our code, but it is weaker than OSMS and parts are missing.

ALREADY OURS (the proposal understates this):
- crates/seat-input/src/pad_scope.rs:1-30, 126-340 already does SPDRP_SECURITY_SDS plus a DIF_PROPERTYCHANGE restart for physical pads.
  - SDDL = SYSTEM + Administrators + owning user SID (scope_sddl, l.126). It also sets DEVPKEY_Device_SessionId.
  - The journal is <data>\pads\scope.json, written before each change. Restore runs at service start (crash), at stop and when the setting goes off, with retry back-off (l.182-340).
  - Device calls are in seat_pads::win::devtree. Owner planning (plan, l.150) uses the routed local seat, else the console while remote seats run.
- Setting: input.pad_device_scope, default OFF (architecture.md:1891-1900).
- pad_scope.rs:28-29 already records that SessionId alone has no effect (spec/remote 60 §13 #2). Proposal step 3 is mostly already done.
- SDL_GAMECONTROLLER/JOYSTICK_IGNORE_DEVICES hints and the XInput shim seat map exist (architecture.md:1901-1913). OSMS has no equivalent.

REAL GAPS vs OSMS:
1. Node coverage.
   - Ours: only "the HID collection and its parent" (pad_scope.rs:14-16). Pads are found only through HID units.
   - OSMS: walks Cm.Descendants of the pad root (GamepadSessionIsolation.cs:489-494, 557-564, 882-888). It matches the XUSB class {d61ca365-5af4-4486-998b-9db4734c6ca3}, the GIP class {05f5cfe2-4733-4950-a6bb-07aad01a3a84} and HID_DEVICE_SYSTEM_GAME (l.832-856). SeatDeviceAccess.cs:45 reserves a USB port node and all its children.
   - Consequence: wired Xbox One/Series GIP pads (xboxgip, no HID collection) and XUSB-only 360 pads are never scoped by us.
2. Default and mode. Ours is opt-in and off. OSMS isolates by default. There is no plain pass-through mode (ACL only, no virtual pad) for local seats.
3. Guide/Game Bar guard. Grep for GameBar/UseNexusForGameBar in our repo: 0 hits. OSMS GuideButtonGuard.cs:10-121 sets HKU\<sid>\Software\Microsoft\GameBar\UseNexusForGameBarEnabled=0 for the seat user and the console user. It is ref-counted and saves the previous value in a Prev* value in the user hive (l.21, 65-86).
4. ControllerToVKMapping.
   - OSMS: sets HKLM\SOFTWARE\Microsoft\Input\Settings\ControllerProcessor\ControllerToVKMapping\Enabled=0 (GamepadSessionIsolation.cs:127, 209-224).
   - Ours: this value appears only as an ASTER-footprint row in crates/seat-setup/src/coexist.rs:284 (expected Absent, repairable) and spec/remote/90-install.md:626.
   - Conflict: if we set 0 ourselves, coexist would flag or repair it. The coexist row must learn about our own ownership.
5. HidHide. 0 hits in our repo. OSMS HidHide.cs uses IOCTL jail entries "<instanceId>!<session>" when the driver is >= 1.4.181 (GamepadSessionIsolation.cs:198). It is a third-party kernel filter (Nefarius; check its licence and signing).
6. Handle semantics contradict our doc.
   - pad_scope.rs:27 says "open handles from before the change keep working until they are closed".
   - OSMS measured (docs/asignacion-dispositivos.md:25-38) that the restart closes the other session's open handles. It also measured that an ACL change without a restart has no effect, and that the ACL is per user, not per session. Its suggested fix for the same user in two sessions is the logon SID S-1-5-5-x-y.
   - OSMS also states that GameInput/Windows.Gaming.Input are opened by a system service and are not isolated by any ACL.

Status: every piece of ours is HYPOTHESIS, never run on hardware. OSMS has a hardware probe (tests/DeviceSessionProbe/Program.cs:57, 377-435; acl-test.ps1). We must never execute it, but its result table is evidence.
- Corrected proposal (wins over Proposal): Extend the existing pad_scope. Do not add a new subsystem.

1. pad_scope node discovery.
   - Start from the pad's USB or Bluetooth device node and walk all descendants (CM_Get_Child/CM_Get_Sibling).
   - Include XUSB {d61ca365-...}, GIP/XboxComposite {05f5cfe2-...} and HID nodes with compatible id HID_DEVICE_SYSTEM_GAME.
   - Discover pads from those classes too, not only from HID units, so GIP and XUSB pads with no HID reader are covered.
   - Keep the journal-before-change rule per node.
2. Add PadMode::Passthrough as the per-local-seat default, but only for pads routed to a local seat with its own session.
   - Passthrough = pad_scope ACL + restart, no virtual pad.
   - Keep input.pad_device_scope as the global kill switch. Its default can flip to on only after a VM or hardware check.
   - Virtual pads (LocalSeat::virtual_pads) stay for remapping and DualSense feature cases.
3. Owner SID.
   - Use the per-session logon SID (S-1-5-5-x-y) when the same user is signed in to two seats.
   - Otherwise use the user SID.
   - Fix the pad_scope.rs:27 comment. OSMS measured that the restart closes the old handles; record this as a HYPOTHESIS to verify.
4. DEVPKEY_Device_SessionId stays cosmetic only (hides enumeration, CR 13). Re-apply it on each device arrival or start, because Windows clears it on every start.
5. New seat-service guide_guard.
   - Set UseNexusForGameBarEnabled=0 in HKU\<sid>\Software\Microsoft\GameBar for the seat users and the console user, ref-counted.
   - Journal the previous value in %ProgramData% (not only in the user hive). Restore at service start and stop; OSMS has no startup restore.
   - Write to the hive only while it is loaded.
6. ControllerToVKMapping\Enabled=0.
   - Set it while any local seat with passthrough is active, journaled, and restore it after.
   - Update coexist.rs:284 and 90-install.md:626 so the row checks whether Pulsar owns the value via the journal. Then our own 0 is not reported or repaired as ASTER residue.
7. HidHide: detect-only optional backend (pad_jail_hidhide).
   - Use it only if HidHide >= 1.4.181 is already installed and its PE machine type matches the OS (ARM64 build is a HYPOTHESIS).
   - Never bundle or install it.
8. Document the limit. GameInput and Windows.Gaming.Input go through a system broker, so ACLs do not isolate them. Mitigation is limited to the SDL hints and the XInput shim map we already have.

P1, size M (most of the plumbing exists). Everything is HYPOTHESIS until it is run in the VM.
- Rules / risk: The ACL path needs no driver. HidHide is a third-party kernel filter (Nefarius, MIT upstream, separately signed). Rules allow only operator-installed, pinned, signature-checked third-party drivers, so keep it detect-only and optional. HYPOTHESIS: an ARM64 HidHide build exists; verify the PE machine. GameInput and Windows.Gaming.Input are brokered by a system service and are not isolated by any ACL; document this.
- Status: TODO

### G6 [P1/S] Recommended 'host console' preset: technical autologon host user, every human seat a session

- OSMS: OSMS's model is a dedicated host user, logged on through autologon, that owns the console. Every human seat is an RDP session shown on a monitor of that console. A full primary monitor cannot be a seat; it can only be split into areas (SeatManager.Validate 239-261). Seats are refused for sessions that use the console user. The doc recommends no UAC secure desktop and no display power-off for the host.
- OSMS files: docs/puestos-locales.md, Services/Seats/SeatManager.cs:30-77, 239-261
- Ours today: seat_one_console is the default: seat 1 is a human console user, so its lock screen, UAC and sign-out hide the other seats and input leaks to it. dedicated_host exists but is opt-in.
- Our files: docs/agents/architecture.md:190-198, crates/seat-service/src/seats.rs, crates/seat-service/src/wizard.rs
- Proposal: Add a wizard preset 'All seats equal (ASTER-like)'. It selects dedicated_host, sets up PulsarSeatHost autologon through LSA DefaultPassword (journaled), enables G4's guard, enables G2 WinUSB capture for every seat's devices, and gives the console no human seat. Manager copy explains the trade-off.
- Verifier evidence: I could not refute this gap. We have the topology but not the console setup, and the default is the weaker one.

OSMS does this:
- docs/puestos-locales.md:21 recommends a console used only as host: a technical user with autologon.
- docs/puestos-locales.md:167-168: no lock, no UAC on the secure desktop, no display power-off.
- Services/Seats/SeatManager.cs:52-57: autostart skips any seat whose session uses the console user (UsesConsoleUser).
- SeatManager.cs:239-261 (Validate): ConsoleUserAutoStartError is raised for those seats. A Full-area seat on a console monitor is refused (error.primaryMonitor). The doc (lines 35-55) shows that monitor can only be split.
- I did not open the actual autologon writer in OSMS. The doc treats it as an operator recommendation, not a wizard step.

What we have today:
- The topology exists. HostTopology::DedicatedHost is used in crates/seat-service/src/service.rs:299-309, seats.rs:233,969,988 and seat-session backends/rdp.rs:76. The test service.rs tests.rs:621 covers dedicated_host_starts_seat_one_as_a_session.
- The PulsarSeatHost account exists (crates/seat-session/src/host_account.rs:74). Containment logs it on (win/containment.rs:890, password from LSA L$PulsarSeat.HostAccount).
- The default is still seat_one_console:
  - apps/manager/src/lib/demo.ts:171
  - docs/agents/architecture.md:190-198, which lists the costs: seat 1's lock, UAC and sign-out hide the other seats, and input is not isolated.
- dedicated_host can only be chosen from a dropdown in the Advanced tab (apps/manager/src/lib/components/AdvancedTab.svelte:282-284).
- The wizard has no topology preset. crates/seat-service/src/wizard.rs has the variants QuickTwoSeats and KeepSeatOne, with nothing about topology or the host.
- Nothing logs the host account on to the console automatically:
  - The only Winlogon writer is win/vm_guest.rs:225-236 (AutoAdminLogon, DefaultUserName, LSA DefaultPassword through win::lsa::set_named). It is for a VM guest only.
  - seat-session/src/autologon.rs:6 says Pulsar Seat never writes AutoAdminLogon or DefaultPassword for user accounts.
  - architecture.md:196-197 says dedicated_host needs the documented auto-logon of that host account (maintainer decision, spec 30 §6.2.1). It is not implemented.

So with dedicated_host turned on today, someone still has to sign PulsarSeatHost in at the console by hand.

Neither project uses termsrv patching or a kernel driver for this part. Autologon uses only documented Winlogon values and an LSA secret.
- Corrected proposal (wins over Proposal): Add a wizard variant called HostConsole ("All seats equal"). It is opt-in and shows a visible consent step that explains an LSA secret will be stored for autologon.

1. Set settings.backends.host_topology to dedicated_host. Seat 1 becomes an RDP session like the other seats (this already works). Unlike OSMS, keep allowing a seat to use a full primary monitor. Under dedicated_host our console presents for seat 1 the same way it does for seats 2..N, so OSMS's primary-monitor refusal does not apply to us.

2. Console autologon for PulsarSeatHost only:
   - Do not write a new Winlogon writer. Make the one in win/vm_guest.rs:225-236 shared code: HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon with AutoAdminLogon=1, DefaultUserName=PulsarSeatHost, DefaultDomainName=., and DefaultPassword written only through LsaStorePrivateData (win::lsa::set_named), never into the registry.
   - Record the previous values in a journal and restore them when the preset is turned off or the product is uninstalled.
   - Generate the password and rotate it together with L$PulsarSeat.HostAccount, so the two never differ.
   - Update the rule in seat-session autologon.rs:6 to read: "never for user accounts; only the service-owned host account under dedicated_host". Record the maintainer's OK for spec 30 §6.2.1 before shipping.

3. Make the host session a kiosk:
   - Settings for the PulsarSeatHost account only: no screen saver, no lock on idle, display timeout 0 for the active power plan while the preset is on (journaled), and DisableLockWorkstation for that user.
   - Keep PulsarSeatHost a standard user that runs only the seat hosts, so the host never needs UAC.
   - Do not turn PromptOnSecureDesktop off machine-wide as OSMS suggests. That setting would also weaken the human seats' sessions.

4. Treat G4 (guard) and G2 (WinUSB capture) as separate items the preset turns on when they are available. Do not bundle them in the preset's work estimate: they have their own size and risk. Under dedicated_host, raw_input_host is already fully isolated in user mode (architecture.md:194-196). G2 is therefore extra hardening, not a requirement.

5. In the manager, show the trade-off on the wizard card and on the Advanced topology field:
   - Seat 1 loses native console GPU speed and goes through capture and present.
   - A Winlogon autologon secret is stored in LSA.
   - Ctrl+Alt+Del and the secure desktop at the physical console belong to the host, not to a person.

Size: M, not S, because of the journaled Winlogon and power changes and the restore path. Priority P1.
- Rules / risk: Autologon stores a secret in LSA. That is documented (Winlogon DefaultPassword through LsaStorePrivateData) but needs a visible opt-in.
- Status: TODO

### G7 [P2/S] Audio jitter buffer and drift handling for the seat output

- OSMS: Agent: WASAPI loopback on 'Remote Audio' with a 10 ms buffer. Host: shared-mode WasapiOut (event sync, 10 ms) fed by SeatJitterBuffer (SeatAudio.cs:130-209). It plays silence until 30 ms is buffered, and above 100 ms it drops back to 30 ms in one jump (counted). An underrun gets a 5 ms fade-out and a fade-in on refill. Stats are logged every 10 s. Measured total is about 70 ms. There is no resampler.
- OSMS files: Services/Seats/SeatAudio.cs:130-209, Services/Seats/SeatHost.cs:494-587, Services/Seats/SeatAgent.cs:486-542
- Ours today: rdpsnd PCM through IronRDP AudioBridge goes to WASAPI shared mode on the seat endpoint (seat-media/src/bridge.rs, win/wasapi.rs). A grep for jitter, underrun or drift across crates/*.rs finds no hits. Untested on hardware: the VM has no audio endpoint.
- Our files: crates/seat-media/src/bridge.rs, crates/seat-media/src/win/wasapi.rs, crates/seat-audio
- Proposal: Add a pure `seat_media::jitter::JitterBuffer` with configurable target/max, a drop-to-target policy, short fades on underrun, and counters. Unit-test it. Ideally add slow drift correction by occasional sample drop or duplicate instead of jumps. Feed it between the rdpsnd receive and the WASAPI render loop, and report the counters in host_audio_report.
- Verifier evidence: This is a partial refutation. The claim "no jitter/drift handling at all" is too strong, but a real gap remains.

What we already have:
- crates/seat-media/src/bridge.rs:46-101 `PcmRing` is a bounded FIFO between the RDP channel thread and the render thread. It has a latency cap (`max_buffer_ms`, default 200, bridge.rs:152,167). On overflow it drops the oldest audio in whole frames and counts it in `dropped_bytes()` (bridge.rs:66-76,98).
- bridge.rs:346 `queued_bytes()` and seat-host/src/ironrdp.rs:397-450 `queued_latency()` give a latency figure.
- Test `ring_caps_latency_by_whole_frames` is at bridge.rs:418.
- The grep for jitter/underrun/drift missed all of this because none of those words are used.

What is missing compared with OSMS (Services/Seats/SeatAudio.cs:130-209 `SeatJitterBuffer`):
1. No start threshold or prebuffer. render_loop (win/wasapi.rs:227-249) sends whatever is in the ring as soon as it arrives. OSMS waits for 30 ms first.
2. Overflow trims only down to the cap, not back to a lower target. The ring sits at about 200 ms after a burst, while OSMS jumps back to 30 ms once it passes 100 ms. On top of the ring there is the 100 ms shared-mode WASAPI buffer (wasapi.rs:115 `1_000_000` hns), which render_loop fills eagerly. Worst-case latency is therefore about 300 ms, against about 70 ms in OSMS.
3. No underrun detection, underrun counter or fade-out/fade-in. When the ring runs dry, the device plays its own silence and you hear a click.
4. Polling uses a 10 ms sleep. There is no event-driven render (no `AUDCLNT_STREAMFLAGS_EVENTCALLBACK`).
5. Nothing reports `dropped_bytes`. `host_audio_report` (seat-service/src/requests.rs:1030, seat-core/src/state.rs:144) carries only render_open, capture_open and notes, with no counters.

Neither side has a resampler or real drift correction. OSMS handles clock drift only through drop-to-target. All of this is untested on hardware.
- Corrected proposal (wins over Proposal): Extend the existing `seat_media::bridge::PcmRing` instead of adding a separate `jitter::JitterBuffer`. Keep it pure and unit-tested with no Windows dependencies.

1. Add `target_ms` (default 30) next to the existing `max_ms`, with the default max lowered from 200 to about 100.
2. Overflow handling: drop back to `target_ms` in one jump, not just down to the cap, and count it in a `drops` counter (keep the existing `dropped_bytes`).
3. Add a `playing` state. `pop_into` returns silence until `target_ms` is buffered. On a short read it sets `playing=false`, counts an underrun, applies a 5 ms fade-out to the tail and clears the rest. On restart it applies a 5 ms fade-in.
4. The fades must handle 16-bit PCM, because rdpsnd usually negotiates it. The OSMS fade is a no-op except for 32-bit float.
5. In win/wasapi.rs, shrink the shared-mode buffer from 100 ms to about 20 ms. Use `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` with `SetEventHandle` instead of the 10 ms sleep. Always write the full free space, filling with silence from the ring when it is empty, so the ring alone owns the fade.
6. Optional: correct slow drift by dropping or duplicating one frame each N ms when ring depth stays above or below target. This is better than OSMS, which has none.
7. Report the counters (underruns, drops, dropped_bytes, queued_ms) in an additive optional field of `host_audio_report` (seat-proto, additive per architecture.md:2033), not as notes, because notes are limited to snake_case codes (requests.rs:1137).

Keep the fake-backend tests in bridge.rs and add ring tests for prebuffer, underrun-then-refill and drop-to-target. Mark the latency figures as HYPOTHESIS until they are measured on hardware with an audio endpoint.
- Rules / risk: None.
- Status: TODO

### G8 [P2/M] Split screen: up to 4 seats per physical monitor (halves/quarters), plus a console share on the primary monitor

- OSMS: SeatScreenArea (Full, Left, Right, Top, Bottom, four quadrants) is a 2x2 cell bitmask (Seat.cs:19-92). The session size equals the area of the current mode (ResolveMode). The presenter window covers only its area. The monitor is deactivated only when its last seat stops. On the primary monitor, the console keeps the largest free part. With SudoVDA (third-party IddCx), OSMS creates a virtual display, makes it primary, moves console windows there, captures it with a SYSTEM agent and paints it into the free part, adding black blank windows. Without SudoVDA it uses an AppBar (SHAppBarMessage) plus ClipCursor. Cost: two windows on one monitor lose independent flip, about +1 frame.
- OSMS files: Services/Seats/Seat.cs:19-92, Services/Seats/SeatManager.cs:625-741, 1003-1019, Services/Seats/SeatHost.cs:621-663, Services/Seats/VirtualDisplay.cs, Services/Seats/ConsoleShare.cs:32-117, Services/Seats/ConsoleWindows.cs:36-116
- Ours today: A grep found no area or sub-monitor concept in seat-core. A local seat owns whole monitors (CCD target ids).
- Our files: crates/seat-core/src/config.rs:260, crates/seat-core/src/backend/display.rs:182-216, crates/seat-display/src/win/presenter.rs, apps/manager/src/lib/components/MonitorFace.svelte
- Proposal: 1. Add `DeviceOptions.area: ScreenArea` (2x2 cell mask) to the monitor assignment in seat-core, with overlap validation in assign.rs.
2. SeatOutputs carries the sub-rect. The presenter places its window there. The RDP DisplayControl layout, or G1's session mode, uses the area size.
3. Console share on the primary monitor: first the AppBar plus ClipCursor fallback (no driver). Later use our pulsar_idd as the virtual console display once it is signed.
4. Manager: a 2x2 grid in MonitorFace.
- Verifier evidence: Our side, the gap is confirmed. A grep of pulsar-seat outside target/ for ScreenArea, quadrant, split screen, sub-rect, AppBar, SHAppBarMessage and console share found no seat-area concept. The only hits were ClipCursor and the unrelated `console_shared` audio backend.
- The display contract is whole-monitor only. `DisplayBinding::bind_seat(seat, targets: &[DeviceKey]) -> SeatOutputs` reserves whole CCD targets and takes them out of the console desktop. `release_seat` gives whole targets back. No rect or area parameter exists (crates/seat-core/src/backend/display.rs:181-216).
- `LocalSeat` has no area field (crates/seat-core/src/config.rs:257+).
- The kiosk shell takes "monitors whose effective mask is exactly its seat" (docs/agents/architecture.md:457).
- The spec says "one seat is created per monitor" (spec/local/40-display.md:328).
- The presenter's ClipCursor only keeps the console cursor off whole seat monitors (crates/seat-display/src/win/presenter.rs:27, 1144).

OSMS side, the claim is confirmed.
- `enum SeatScreenArea { Full, Left, Right, Top, Bottom, TopLeft, TopRight, BottomLeft, BottomRight }` with a 2x2 cell bitmask (Services/Seats/Seat.cs:19-44).
- Services/Seats/ConsoleShare.cs:16-95 keeps the console in its half. It uses `SHAppBarMessage` with ABM_NEW, ABM_QUERYPOS, ABM_SETPOS and ABM_REMOVE (shell32), plus `GetClipCursor`/`ClipCursor`, re-applied because Windows clears the clip on activation changes.
- SeatHost.cs:705 says the fallback without SudoVDA works only with halves, because an AppBar docks to an edge.
- SeatManager.cs:629-635: with SudoVDA, it creates a virtual display for the console part and makes it primary.
- VirtualDisplay.cs, ConsoleWindows.cs and ConsoleDisplay.cs also exist.
- SudoVDA (third-party IddCx) is mentioned only in README.md, docs/puestos-locales.md and Native/RemoteIdd/README.md. No licence notice for it was found.
- I found no termsrv patch or code injection in this feature path. The only driver is the optional SudoVDA.
- Corrected proposal (wins over Proposal): The proposal is mostly right. These corrections fit our architecture:
1. Put the area in seat-core as a `ScreenArea` 2x2 cell mask on the per-monitor assignment, next to the existing per-monitor `AssignmentOptions { hdr, vrr }`. Validate in assign.rs that cell masks on the same monitor do not overlap. HDR, VRR and mode stay per monitor: seats that share a monitor must agree or inherit.
2. `bind_seat` today reserves whole targets and removes them from the console desktop. Change it to reference-count shared targets. Bind on the first seat. Release only when the last seat on that target stops, as OSMS does with "deactivate when last seat stops". `SeatOutputs` gets a sub-rect per output.
3. The presenter (seat-display presenter.rs) places its window in the sub-rect. The presenter ClipCursor and the kiosk `place` logic must clip to the sub-rect, not the monitor. Accept the loss of independent flip (about +1 frame) and show it in the UI.
4. The RDP DisplayControl layout uses the area size. For the `desktops` backend, the pulsar_idd monitor per output uses the area size.
5. Console share on the primary monitor: first an AppBar plus ClipCursor fallback, halves only (an AppBar docks to an edge). Use our own pulsar_idd as the virtual console display only after it is signed. Never use SudoVDA.
6. Manager: a 2x2 picker in MonitorFace, and a spec/local/40-display.md update. The spec says one seat per monitor today, and the analyst should amend it under the clean-room rules.
Priority P2, size M-L. The refcounted bind/release and the console-desktop topology change are larger than they look.
- Rules / risk: Do not adopt SudoVDA, a third-party driver whose licence is not in OSMS notices. pulsar_idd is ours but unsigned, so ship the AppBar fallback first.
- Status: TODO

## Refuted

- G9 Seat liveness watchdogs (disconnect from inside the seat, presentation lost): OSMS claim checked and correct. In Services/Seats/SeatManager.cs, IsSessionConnected (803-808) calls WTSQuerySessionInformationW with class 8 (WTSConnectState) and expects WTSActive=0. WatchSeatSessions (814-833) stops the seat after a 3000 ms grace. TrackSeatWindowsSession (839-853) bumps _generation when the session id changes. Readjust (859-872) is a full Stop+Start, triggered by the presenter's "monitor_mode" JSON (910-915). Bridge pump end calls OnPresentationLost (893-897, 921-927), which is de-duplicated through _lost (939-963). The seat then stays off until the user starts it again. None of this uses termsrv patching, injection or drivers; it is plain wtsapi32 P/Invoke.

Each OSMS watchdog already has an equivalent in Pulsar Seat, or does not apply to our design:
1) Disconnect from inside the seat. Our RDP client runs in the seat host, so the session disconnecting ends the connection itself. crates/seat-host/src/flow.rs:88-101 maps the event to Disconnected or LoggedOff with a reason, and the host exits. crates/seat-session/src/autologon.rs:163-186 (after_session_end) then reconnects with back-off and stops after MAX_RECONNECT_ATTEMPTS. Replaced or Denied stops the seat. A WTSConnectState poll would add nothing, because a session that is disconnected while our client is still connected cannot exist. Our policy reconnects; OSMS stops the seat. That is a design choice, not a missing capability.
2) Session change and logoff. crates/seat-service/src/seats.rs:923-1025 (on_session_change) handles the SCM WTS notifications: Logon, Logoff, Lock, Unlock, Terminate (Terminate calls on_session_gone at 1356). A restart writes the new session_id at seats.rs:678. A 1 s poll is not needed.
3) Host or presentation process death. seats.rs:1400-1465 (watchdog) checks process_alive(host.pid) and restarts with backoff_ms, giving up after CRASH_LIMIT with Error HostExited. This is stronger than OSMS's stop-once.
4) Monitor mode change. PresenterEvent::Layout (crates/seat-display/src/win/presenter.rs:97-104) goes to on_layout in crates/seat-host/src/display.rs:77-85. That is a live relayout (ironrdp End::Relayout reports "reconnecting", ironrdp.rs:540), not a full stop/start like OSMS Readjust. Ours is better.
5) Capture-agent liveness depends on G1, which has not landed; no doc outside docs/journal.md mentions G1 or a capture agent. There is nothing to watch yet.

One small gap of our own, not an OSMS capability: crates/seat-host/src/display.rs:88 drops PresenterEvent::Output { present: false }, and the comment claims the event is forwarded. HybridPresenter (crates/seat-display/src/win/dcore.rs:605-660) retries the output on its own, but the seat state and the manager never show "waiting for display". A monitor unplug in seat-service only sets devices_dirty (service.rs:540-543). OSMS does worse here: it falls back to (0,0) and covers the console primary.
