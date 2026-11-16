# drivers/seat-wfp

Pulsar Seat's own source-IP callout (spec 80 §4.2, §11.3; spec 00 §5.5.2): a KMDF
**non-PnP** Windows Filtering Platform callout driver (`seatwfp.sys`, service
`PulsarSeatWfp`, INF class `WFPCALLOUTS`) that gives the TCP / UDP sockets of a seat's
processes the seat's IPv4 address. Each seat can then bind the same port on its own address
(LAN games, per-seat network identity) without the program knowing about seats.

**Status: built (WDK 10.0.28000, ARM64 + x64, Release + Debug, zero warnings, infverif /w,
ApiValidator Universal and the WDK code analysis clean, 2026-11-17), unsigned, never
loaded.** Our drivers stay unloaded until Microsoft-signed (docs/agents/plan.md). Until
then the service reports `source_ip_callout: not_installed` and seats use the user-mode
fallback (`PULSAR_SEAT_SOURCE_IP` in the environment of the programs the service starts,
`seat_compat::srcip`). Everything here is **untested on hardware**.

## What it does

| Layer | When | Effect |
| --- | --- | --- |
| `FWPM_LAYER_ALE_BIND_REDIRECT_V4` | a TCP / UDP socket of a mapped process binds to `0.0.0.0` (explicit `bind`, or the implicit bind of the first `connect` / `sendto`) | the bind becomes `<seat address>:<same port>` |
| `FWPM_LAYER_ALE_CONNECT_REDIRECT_V4` | an outbound TCP connect / UDP flow of a mapped process to a non-loopback destination, local address not yet the seat's | local address set to the seat address (backstop for sockets bound before the map arrived) |

Left alone: explicit binds to a given address (the program chose it), loopback traffic
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

## Driver Verifier and test plan (test VM, test-signed, `tools/vm/testsign.ps1`)

Nothing below has run. On a Hyper-V VM (ARM64 and x64), Memory Integrity and Secure Boot
on, test-signed package (VM only):

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

Open questions (HYPOTHESIS until the VM run): PASSIVE_LEVEL classify for both layers; local
address rewrite honoured at the connect-redirect layer for TCP and UDP; app id format for
programs on network shares; dual-stack sockets classified at the V4 layers; the SID-string
SDDL accepted by the control device.

## Clean room

Written fresh from the public WDK documentation on Microsoft Learn (WFP callout drivers,
"Using bind or connect redirection", `fwpsk.h` / `fwpmk.h` reference, KMDF control device
objects, non-PnP KMDF drivers). The WDK samples (MS-PL) were not used. No third-party
binary was looked at; spec 80 §4.2 says only that the reference product binds a seat's apps
to a source address.
