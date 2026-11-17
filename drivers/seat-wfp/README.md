# drivers/seat-wfp

Pulsar Seat's own source-IP callout (spec 80 §4.2, §11.3; spec 00 §5.5.2): a KMDF
**non-PnP** Windows Filtering Platform callout driver (`seatwfp.sys`, service
`PulsarSeatWfp`, INF class `WFPCALLOUTS`) that gives the TCP / UDP sockets of a seat's
processes the seat's IPv4 address. Each seat can then bind the same port on its own address
(LAN games, per-seat network identity) without the program knowing about seats.

**Status: built (WDK 10.0.28000, ARM64 + x64, Release + Debug, zero warnings, infverif /w,
ApiValidator Universal and the WDK code analysis clean, 2026-11-17), unsigned; loaded only
test-signed in the Hyper-V test VM (ARM64, HVCI on, Driver Verifier; results in "VM round
2026-11-17" below).** Our drivers stay unloaded on real PCs until Microsoft-signed
(docs/agents/plan.md). Until then the service reports `source_ip_callout: not_installed`
and seats use the user-mode fallback (`PULSAR_SEAT_SOURCE_IP` in the environment of the
programs the service starts, `seat_compat::srcip`). Untested on physical hardware and x64.

## What it does

| Layer | When | Effect |
| --- | --- | --- |
| `FWPM_LAYER_ALE_BIND_REDIRECT_V4` | a TCP socket of a mapped process binds to `0.0.0.0` with a port of its own (listeners); a UDP socket binds to `0.0.0.0` (explicit `bind`, or the implicit bind of the first `sendto`) | the bind becomes `<seat address>:<same port>` |
| `FWPM_LAYER_ALE_CONNECT_REDIRECT_V4` | an outbound TCP connect / UDP flow of a mapped process to a non-loopback destination, local address not yet the seat's | local address set to the seat address: every TCP client socket (implicit bind, or a wildcard bind to port 0, both skipped by the bind layer since VM round 2026-11-17), and sockets bound before the map arrived |

**TCP clients vs. UDP (VM round 2026-11-17).** The bind layer cannot see the destination.
A seat address given to the implicit bind of a TCP client made every connect to `127/8`
fail (`WSAEADDRNOTAVAIL`), so TCP client sockets (`FWP_CONDITION_FLAG_IS_IMPLICIT_BIND`,
or `0.0.0.0:0`; the flag alone did not cover .NET clients) are left to the connect layer:
the SYN to a LAN / internet address left with the seat address (pktmon) and loopback
connects work. For UDP the connect layer's local address was **not** honoured for
unconnected sends (the datagram left with the PC's default address), so UDP keeps the bind
redirect. **Limit:** a seat program that sends UDP to `127/8` from a socket it never bound
fails with `WSAEADDRNOTAVAIL`; put such a program on the seat's program list as an
exception (`all_except_listed`).

Left alone: explicit binds to a given address (the program chose it; the bind classify
records the socket's transport endpoint handle and address in a 1024-slot table, and the
connect classify skips a socket found there with the same local address, parity review
2026-11-17; HYPOTHESIS: the endpoint handle metadata is present at both layers; a slot
collision evicts the older socket, which then gets the connect backstop), loopback traffic
(127.0.0.0/8 local or remote), IPv6 sockets (the seat address is IPv4; HYPOTHESIS: a
dual-stack socket talking to an IPv4 peer is classified at the V4 layers), connections
another callout already redirected (`FwpsQueryConnectionRedirectState0` != not
redirected), every process whose session is not in the map.

**Which process is whose:** the map is keyed by **session id**. The classify function
reads the process id from the metadata, then the session from the process's primary token
(`PsLookupProcessByProcessId`, `PsReferencePrimaryToken`, `SeQuerySessionIdToken`). That
needs PASSIVE_LEVEL; a classify at a higher IRQL is counted in `Skipped` and left alone.
HYPOTHESIS: both ALE redirect layers classify socket calls of user-mode processes at
PASSIVE_LEVEL in the caller's context (the `Skipped` counter shows it on hardware).

**Rule and program list** (same model as the manager dialog): `ALL` = every process of
the session; `ONLY_LISTED` / `ALL_EXCEPT_LISTED` match the process image path, WFP's
`ALE_APP_ID` (an NT path such as `\device\harddiskvolume3\games\lan\game.exe`), against the
list: equal, or below a listed folder. ASCII case-insensitive. The service converts the DOS
paths of the list (`C:\Games\Lan`) with `QueryDosDeviceW`; UNC paths become
`\device\mup\server\share\…` (HYPOTHESIS for mapped network drives). An unknown image counts
as "not listed".

## Control device (`include/seatwfp_ioctl.h`, interface major 1)

`\Device\PulsarSeatWfp` (`\\.\PulsarSeatWfp`), exclusive, DACL
`D:P(A;;GA;;;S-1-5-80-2060529063-4084430102-647711624-1206425353-118540754)`: the service
SID of `PulsarSeat` only (the service runs with `SERVICE_SID_TYPE_UNRESTRICTED`,
`seat-service` `win/scm.rs`). Not SYSTEM in general, not administrators. The SID is
pinned by a Rust test that derives it from the service name (SHA-1 of the upper-case name,
the documented service-SID rule). HYPOTHESIS: `WdfControlDeviceInitAllocate` accepts a SID
string (not only an SDDL alias) in the device SDDL subset; if not, the driver fails to load
(visible as `installed_not_loaded`), never with a wider DACL.

| IOCTL | Code | Buffers |
| --- | --- | --- |
| `GET_VERSION` | `0x222000` | out `SEATWFP_VERSION` (16 B): major, minor, build, flags (`PASS_THROUGH`, `CALLOUTS`, `FILTERS`, `MAP`) |
| `SET_MAP` | `0x22E004` | in: header (24 B) + seats (32 B each) + path table (8 B each) + UTF-16 path characters; replaces the whole map atomically |
| `CLEAR` | `0x22E008` | none |
| `GET_STATS` | `0x22E00C` | out `SEATWFP_STATS` (40 B): bind / connect rewrites, skipped, map generation, seats, flags |

Limits: 64 seats, 4096 paths, 1024 characters per path, 1 MiB per map. A seat address must
be a unicast IPv4 (not 0.0.0.0, 127/8, multicast, broadcast, class E); session ids are
unique. The Rust mirror is `crates/seat-compat/src/wfp.rs` (`encode_map` / `decode_map`
apply the driver's checks; its tests pin every size, code and constant against this
header).

## Fail open, recovery

- No map before the service's first `SET_MAP`, and the map is dropped when the service's
  handle closes (stop or crash): session ids are reused by Windows, so a stale map must not
  outlive the service. Then no socket is touched.
- Safe Mode or `HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatWfp\Parameters\PassThrough`
  ≠ 0: no callout, no filter; the control device answers `GET_VERSION` with `PASS_THROUGH`
  and refuses `SET_MAP` (the service then reports `driver_pass_through`).
- WFP objects live in a **dynamic** filter-engine session: sublayer (weight 0x100, below
  firewall / VPN sublayers), two callouts, two filters with no conditions. They disappear
  when the engine handle closes (unload, BFE stop). Nothing is persistent. BFE may start
  after the driver at boot: the driver subscribes to BFE state changes and adds its objects
  when BFE runs (`FwpmBfeStateSubscribeChanges0`).
- Unload order: BFE unsubscribe, engine close (filters gone), callouts unregistered,
  redirect handle destroyed, map freed, control device deleted.

## Install (only once Microsoft-signed)

Primitive driver package (no device): `[DefaultInstall.NTamd64]` / `[DefaultInstall.NTarm64]`
with `AddService` (kernel driver, `StartType` 2 = auto start, so the callouts exist before
seat sessions open their first sockets). `seat_compat::drivers` stages it like the other own
packages (signed by "Microsoft Windows Hardware Compatibility Publisher", journaled, removed
on uninstall); `parse_inf` reads the DefaultInstall decorations as the package's
architectures and a newly installed primitive package asks for a reboot (HYPOTHESIS:
`pnputil /add-driver … /install` creates the service but does not start it).

```
pnputil /add-driver seat-wfp.inf /install
sc stop PulsarSeatWfp & pnputil /delete-driver oem<N>.inf /uninstall
```

## Build

`installer\scripts\build-drivers.ps1 -Projects seat-wfp` (ARM64 + x64, Release + Debug;
`-Analyze` for the WDK code analysis). Links `fwpkclnt.lib`; NDIS 6.30 headers
(`NDIS630`), callout API version 2 (`FwpsCalloutRegister2`, Windows 8+). The WFP layer
GUIDs and our own keys are defined in `src/guids.c` (`initguid.h` first; the kernel
libraries carry no uuid.lib for them). The engine lock is a synchronization event, not a
fast mutex: `FwpmXxx` calls must run at PASSIVE_LEVEL with APCs enabled.

| File | What |
| --- | --- |
| `include/seatwfp_ioctl.h` | control contract (shared with the Rust mirror) |
| `src/driver.c` | DriverEntry (non-PnP), unload, control device, IOCTLs, map swap |
| `src/map.c` | map validation and lookup (pure, no WFP / WDF) |
| `src/callout.c` | callout registration, BFE subscription, dynamic session objects, classify |
| `src/guids.c` | GUID definitions |

## VM round 2026-11-17 (ARM64 VM, test-signed, HVCI on, Secure Boot off)

`verifier /standard /driver seatwfp.sys` + WDF verifier (`Parameters\Wdf\VerifierOn`):

- **PASS:** loads at boot under HVCI and Verifier; `netsh wfp show filters` lists "Pulsar
  Seat bind redirect" / "connect redirect" (`FWPM_LAYER_ALE_*_REDIRECT_V4`); the service
  opens the control device (the SID-string SDDL is accepted) and reports `loaded` 1.0
  (`source_ip.enforcement = driver`); `pnputil /add-driver /install` creates the service
  but does not start it (HYPOTHESIS confirmed); four unloads (`sc stop`) and reloads under
  Verifier without a violation, leak report, bugcheck or CodeIntegrity block.
- **PASS, two seats (fast user switching, addresses .231 / .232 on one adapter, rule
  `all_programs`):** a listener on `0.0.0.0:5000` in each seat binds to its seat address
  (both at once, no conflict); UDP server `0.0.0.0:5001` gets the seat address; the TCP SYN
  and the UDP datagram to a remote address leave with the seat address on the wire
  (pktmon); explicit binds to `127.0.0.1` stay; TCP loopback works (after the fix above).
- **Limit:** UDP to `127/8` from an unbound socket (see above).
- Not run here: rule `only_listed` / `all_except_listed`, network-share app ids, dual-stack
  sockets, `PassThrough`, BFE restart, x64.

## Driver Verifier and test plan (test VM, test-signed, `tools/vm/testsign.ps1`)

Plan for the remaining steps (2026-11-17 ran 1, 2, 3, 5, 6 and 8 on ARM64, see above). On a
Hyper-V VM (ARM64 and x64), Memory Integrity on, test-signed package (VM only):

1. `verifier /standard /driver seatwfp.sys` plus `/irql` checking (IRQL rules matter
   here: classify runs up to DISPATCH_LEVEL, the map lock is an `EX_SPIN_LOCK`), reboot.
   Expect no bugcheck through steps 2-7. Also `verifier /flags 0x02000000` (code integrity
   checks) once.
2. Boot with the driver installed and the service stopped: `GET_VERSION` flags show
   `CALLOUTS | FILTERS` (`netsh wfp show filters` lists two "Pulsar Seat" filters in the
   sublayer); networking of every session is unchanged (no map).
3. Two seats with addresses 192.168.x.52 / .53 (extra static addresses on one adapter),
   rule `ALL`: in each seat `Test-NetConnection` / a UDP sender to a LAN peer leaves with the
   seat's address (capture on the peer); a TCP listener on port 5000 in both seats binds
   twice without a conflict; `GET_STATS` counts bind / connect rewrites.
4. Rule `ONLY_LISTED` with a folder: a listed program gets the address, `curl.exe` from
   System32 does not; `ALL_EXCEPT_LISTED` the opposite.
5. Loopback: a program in a seat connecting to `127.0.0.1` keeps working; an explicit bind
   to another local address stays as chosen.
6. Stop the service (and kill it once): the map is gone (`GET_STATS` seat count 0), traffic
   leaves with the default address again; restart re-sends the map.
7. `Parameters\PassThrough = 1` + reboot: no filters, service reports
   `driver_pass_through`. BFE restart (`net stop bfe` is refused while dependants run; use a
   reboot with BFE delayed) re-adds the objects.
8. Unload with traffic running (`sc stop PulsarSeatWfp`) under Verifier: no leak report
   (pool tag `PsWf`), no bugcheck.

Answered in the VM (2026-11-17): the SID-string SDDL is accepted; the connect-redirect
local-address rewrite is honoured for TCP, not for unconnected UDP sends; both layers
classified the seat processes' sockets (the rewrites happened, so the session lookup ran
at PASSIVE_LEVEL; `GET_STATS` itself was not read: the control device admits only the
service). Still HYPOTHESIS: app id format for programs on network shares; dual-stack
sockets classified at the V4 layers.

## Clean room

Written fresh from the public WDK documentation on Microsoft Learn (WFP callout drivers,
"Using bind or connect redirection", `fwpsk.h` / `fwpmk.h` reference, KMDF control device
objects, non-PnP KMDF drivers). The WDK samples (MS-PL) were not used. No third-party
binary was looked at; spec 80 §4.2 says only that the reference product binds a seat's apps
to a source address.
