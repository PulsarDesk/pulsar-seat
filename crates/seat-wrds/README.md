# seat-wrds: Remote Desktop protocol provider

GPL-3.0-only. `pulsar-seat-wrds.dll`, an in-process COM server that TermService loads for
the WinStation listener `PulsarSeat-Idd`. It lets remote seats run on **our** IddCx
display (`drivers/seat-idd`, remote role, hardware id `PulsarSplit_IndirectDisplay`)
instead of the stock RDP display, so resolution, refresh, super-sampling, desktop scale and
HDR metadata of a remote seat go through our driver's control interface
(spec/remote 40 §2.3, §4.4, §7, §8.4). Added 2026-11-17 (package remote-idd-hdr).

Written from the public Microsoft documentation of the Remote Desktop Protocol Provider API
and the Windows SDK headers, through the `windows` crate bindings. No third-party code, no
code of the reference product (whose provider is shipped unused, spec/remote 30 §11).

## How a seat start uses it

```
service (session 0)                         TermService (svchost, NETWORK SERVICE)
  engine: plan() -> provider path            loads pulsar-seat-wrds.dll for PulsarSeat-Idd
  hub.connect(seat, user, password) --pipe--> provider: ConnectRequest (validated again)
                                             IWRdsProtocolListenerCallback::OnConnected
                                             IWRdsProtocolConnectionCallback::OnReady
                                   <-------- Accepted          (AcceptConnection)
                                             GetClientData: size, client name, user (no password)
                                             GetUserCredentials: the password, once, then wiped
                                             IsUserAllowedToLogon: only the named seat account
                                   <-------- SessionId          (NotifySessionId)
                                             IWRdsWddmIddProps(1)::GetHardwareId -> PulsarSplit_IndirectDisplay
                                   <-------- DriverLoaded       (OnDriverLoad, device instance on IddProps1)
                                   <-------- LoggedOn           (LogonNotify)
  engine: PulsarIdd attach (SET_SEAT_CONFIG v2) on the session's device, wait first frame
  stream server starts in the session; display.mode.request -> REQUEST_MODE (our IDD)
  stop: WTSLogoffSession / WTSDisconnectSession, Disconnect -> BrokenConnection
```

Every failure on that path (provider not registered / not loaded / not listening, connect
refused or timed out, display not ready) ends what was done and the seat starts through the
loopback RDP client with the `rdp_session` display instead; the manager shows a
`display_fallback` notice and the reason (`seat_runtime.display.fallback`).

## Files

| File | What |
| --- | --- |
| `src/state.rs` | pure state: connections the service asked for, credential hand-out once, logon check, outgoing messages (unit-tested on any host) |
| `src/com.rs` | COM objects (`IWRdsProtocolManager`, `IWRdsProtocolListener`, `IWRdsProtocolConnection` + `IWRdsProtocolLicenseConnection` + `IWRdsProtocolLogonErrorRedirector` + `IWRdsWddmIddProps` + `IWRdsWddmIddProps1`), class factory, `DllGetClassObject` / `DllCanUnloadNow` |
| `src/pipe.rs` | pipe client: connects to `\\.\pipe\PulsarSeat.Wrds`, checks that the server process is the `PulsarSeat` service (SCM process id), hello, frames both ways, reconnect with backoff |
| `build.rs` | silences MSVC LNK4104 for the two COM exports |

Contract: `seat_proto::wrds` (pipe, messages, CLSID, listener name, limits). Service side:
`seat-service` `remote::wrds` (plan, hub, port) and `remote::win::wrds` (pipe server,
TermService process check, registration). Registration: `seat_session::wrds`.

## Registration (service, `seat_session::wrds`)

- `HKLM\SOFTWARE\Classes\CLSID\{6D3B1E54-7C2A-4F19-9E8B-2A51C4D07F36}\InprocServer32` =
  `<install>\pulsar-seat-wrds.dll`, `ThreadingModel = Both`.
- `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Idd`:
  `LoadableProtocol_Object` = the CLSID, `Security` = logon / connect for the seat accounts
  only (same descriptor builder as the loopback listener), `fInheritAutoLogon = 1`,
  `fPromptForPassword = 0`, `fInheritInitialProgram = 0`, `Shadow = 0`,
  `MaxInstanceCount = 64`, `fEnableWinStation` written 0 first and 1 last. No port, no LAN
  adapter, no stored credentials. `RDP-Tcp` is never touched.
- Written while the remote display setting is `pulsar_idd` (or automatic with a signed own
  driver), removed otherwise and by `pulsar-seat-service uninstall`. Recovery: DWORD
  `WrdsDisabled = 1` under `HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters`.
- The DLL must be native (PE machine 0xAA64 on ARM64, 0x8664 on x64); a release ships it
  Authenticode-signed (`check-payload.ts` `MUST_BE_SIGNED`).

## Build and test

```
cargo test -p seat-wrds
cargo clippy -p seat-wrds --all-targets -- -D warnings
cargo build --release -p seat-wrds          # target\<triple>\release\pulsar_seat_wrds.dll
```

`build-installer.ps1` builds it with the service and ships it as `pulsar-seat-wrds.dll`.

## HYPOTHESIS (nothing ran: needs Windows Server with the RD Session Host role)

- TermService loads a listener's provider only when it starts, so a new registration needs
  a TermService restart (status `wrds_restart_needed` until the provider says hello).
- Call order of TermService into the provider as drawn above; `OnReady` right after
  `OnConnected` is accepted.
- Null keyboard / mouse / beep handles (`GetInputHandles`) and a null video handle
  (`GetVideoHandle`) are accepted for a protocol that names a WDDM IDD; input reaches the
  seat through the stream server's `SendInput` inside the session, as on the loopback path.
- `WTS_CLIENT_DATA::ColorDepth` takes 32 (bits per pixel); `ProtocolType` RDP is accepted
  for a non-RDP provider; `WTS_QUERY_LOGON_SCREEN_SIZE` takes two ULONG entries.
- `SessionArbitrationEnumeration` / `AuthenticateClientToSession` /
  `RequestClientLicense` answering `E_NOTIMPL` makes TermService use its default
  arbitration and licensing (RDS CALs, spec/remote 30 §13).
- The credentials from `GetUserCredentials` with `fInheritAutoLogon` log the seat user on
  without a prompt; `IsUserAllowedToLogon` refusing any other account is honoured.
- `BrokenConnection(0, 0)` makes TermService disconnect and close the connection.
- TermService (NETWORK SERVICE + its service SID) can open the pipe with
  `D:P(A;;GA;;;SY)(A;;0x0012019b;;;<TermService SID>)` and query the `PulsarSeat`
  service's process id.
- The seat-idd remote role starts on the device TermService creates for our hardware id
  (the 2026-11-16 VM run showed IddCx refusing a root-enumerated remote-role device, which
  is exactly why this provider exists).
- Sound (review 2026-11-17): `QueryProperty(WTS_QUERY_AUDIOENUM_DLL)` names the Windows RDP
  audio endpoint enumerator `rdpendp.dll` when the seat streams sound
  (`ConnectRequest.audio`), so the session gets the same "Remote Audio" endpoint an RDP
  session has, and the stream server captures it (loopback). Unverified: that the
  enumerator creates the endpoint without an `rdpsnd` virtual channel, and that TermService
  frees the returned string with `LocalFree` (it is `LocalAlloc`ed). Provider sessions still
  have no virtual channels of their own, so no microphone hold (the reference has no
  microphone path either, spec/remote 70 §1), no clipboard and no drive redirection (the
  reference session asks for none, spec/remote 30 §5.1).
- `Uninitialize` followed by `Initialize` in the same TermService process (the DLL never
  unloads) starts the pipe thread again (`state::PipeGate`, review 2026-11-17).
- Windows Server needs a WHQL-signed seat-idd driver (spec/remote 40 §2.3); the provider
  itself is user mode and needs only Authenticode for a release.
