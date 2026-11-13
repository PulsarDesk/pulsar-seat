# drivers/seat-input-filter

Pulsar Seat's own KMDF input filter (spec 50 §4.1.2, §4.1.3, §4.1.5; spec 00 §5.6, §5.5.2).
One driver (`seatinput.sys`, service `PulsarSeatInput`) with three roles, picked per devnode
from its setup class at `EvtDriverDeviceAdd`:

| Role | Devnodes | Position | What it does |
| --- | --- | --- | --- |
| keyboard | Keyboard class `{4D36E96B-…}` (HID keyboards, PS/2) | device upper filter **below kbdclass**, above kbdhid / i8042prt | hooks the class service callback (`IOCTL_INTERNAL_KEYBOARD_CONNECT`), routes every packet |
| mouse | Mouse class `{4D36E96F-…}` (HID mice, touchpad mouse collections, PS/2) | below mouclass | same with `IOCTL_INTERNAL_MOUSE_CONNECT` |
| hid | HIDClass `{745A17A0-…}` collections (touch, pen, precision touchpad 0x0D/0x05, pads, consumer, system control) | upper filter on the collection devnode | decides on `IRP_MJ_CREATE` by requestor session; never touches data |

Remote-desktop input bus devices (`TERMINPUT_BUS\…`) are never filtered. Unknown classes
pass through untouched.

**Status: built (WDK 10.0.28000, ARM64 + x64, zero warnings, infverif /w and WDK code
analysis clean), unsigned, never loaded.** Our drivers stay unloaded until Microsoft-signed
(docs/agents/plan.md). Until then the
service reports `driver_not_signed` for `kmdf_filter` and uses `raw_input_host`.
Everything here is **untested on hardware**.

## Behaviour

Keyboard / mouse packets (DISPATCH_LEVEL, `src/kbdmou.c`):

| State | Seat-1 or shared unit | Unit of exactly one seat N > 1 | Spare unit (mask 0), or several seats without seat 1 |
| --- | --- | --- | --- |
| before the first service attach since boot, Safe Mode, `Parameters\PassThrough` ≠ 0 | pass | pass (fail-open) | pass |
| service attached | pass; Ctrl+hotkey swallowed and reported (`HOTKEY` packet, `A` bit 0 = Shift held → emergency press); wizard / activity copies (`CONSOLE_COPY`; outside the wizard a keyboard copy carries `A` = `B` = 0: only the fact and time of activity, never the key) | queued for the service only | dropped; queued with `WIZARD` while the wizard runs |
| service gone after an attach | pass, **including** Ctrl+hotkey (nothing would act on it; no stale hotkey is queued) | queued in a bounded buffer **per unit** (`GapPackets` per unit, aged after `GapMs`; a unit over its budget drops its newer packets), **never** to the console | dropped |

Keyboards and mice are exclusive: a mask with several seats and no seat 1 is treated as
spare, the same rule as the service's route model (`UnitRoute::Shared`, delivered to
nobody), so both backends behave alike. A multi-seat mask that includes seat 1 passes to
the console.

`HOTKEY` packets age out after `GapMs` like every other packet (a hotkey read minutes later
must not move a unit).

On reconnect, packets older than `GapMs` are dropped and the next packet of an affected unit
carries `GAP`, so the service releases every key that unit held (no stuck keys).

HID create rules (`src/hid.c`), in order: fail-open / pass-through → allow; unit on seat 1,
shared, or usage in the unfiltered list (default lamp arrays 0x59/0x01; usage 0 in the list
means the whole page; the service puts 0x01/0x80 first when system buttons are allowed),
or a system-control collection (0x01/0x80) while `SEATINPUT_CF_ALLOW_SYSTEM_BUTTONS` is
set → allow; System-process thread → allow; requestor session 0 **and** the create's token
user is a service account (LocalSystem, LocalService, NetworkService, `NT SERVICE\*` =
`S-1-5-80-*`, same rule as the storage gate) → allow; requestor session = session of a seat
in the mask → allow; else `STATUS_ACCESS_DENIED`. "Session 0" alone is not enough: a user's
own code runs there (S4U / stored-password scheduled tasks, BITS, impersonating services)
and is judged like an unmapped session. Gap: a session-0 service configured to run as a
real user account is denied extra-seat HID collections too.

The session rule applies to **kernel-mode requestors too**: win32k's raw input thread opens
touch, pen, consumer and system-control collections from kernel mode in each session's own
context, so a "kernel mode = allow" shortcut would let the console consume an extra seat's
touch screen and media / power keys. Threads of the System process (no session) are
allowed. Only a failed session lookup fails open (an unclassifiable requestor is a system
component; breaking it could make the PC unusable, letting it through only weakens isolation
for that one open). Opens allowed before a reassignment, and every open the console made
during fail-open before the first attach, stay valid: the service reports `needs_replug`.

The service reads extra-seat touch, pen, touchpad, consumer and system-control collections
itself from session 0 (`kmdf_router` HID reader) and forwards them to the seat host. When
another program holds a collection exclusively the service reports
`hidden_by_other_software`.

Control device lifetime: created with the first unit, deleted with the last one, except
while the service still holds its handle; then deletion is deferred and the next unit reuses
the device (its name would otherwise stay taken and the service could never reattach).
Creation is retried on every device add. The deferred case only keeps the driver image
loaded; the installer stops the service before a driver update.

The seat mask of a devnode is read at device start from the private DEVPROPKEY
`{0527596e-0b5b-4dcf-88b3-118399aa27b1}` pid 2 (UINT32, written by the service; absent →
seat 1) and updated at run time with `IOCTL_SEATINPUT_SET_UNIT`.

## Control interface (interface major 1)

Header: [`include/seatinput_ioctl.h`](include/seatinput_ioctl.h). Rust mirror and layout
tests: `crates/seat-input/src/filter_proto.rs`. Client: `crates/seat-input/src/kmdf_router.rs`
(+ `win::WinFilter`).

- Device `\Device\PulsarSeatInput`, link `\DosDevices\PulsarSeatInput` (`\\.\PulsarSeatInput`),
  `SDDL_DEVOBJ_SYS_ALL` (SYSTEM only), exclusive open, buffered I/O, `FILE_DEVICE_UNKNOWN`.
- The service opens it overlapped (the pended read must not block other IOCTLs).
- The contract uses this named device, not a device interface: a non-PnP control device
  cannot register one. The service name is `seat_core::consts::INPUT_FILTER_SERVICE`. A new
  interface major gets a new device name.

| IOCTL | Code | Access | In | Out | Notes |
| --- | --- | --- | --- | --- | --- |
| `GET_VERSION` | `0x222000` | any | – | `SEATINPUT_VERSION` (16) | always first; service refuses major ≠ 1 and never attaches |
| `ATTACH` | `0x22E004` | RW | `SEATINPUT_ATTACH` (8) | – | wrong major → `STATUS_REVISION_MISMATCH`; ends fail-open until reboot |
| `SET_CONFIG` | `0x22E008` | RW | `SEATINPUT_CONFIG` (88) | – | wizard / system buttons / hotkey flags, hotkey scan, gap ms + packets, unfiltered usages (≤ 16) |
| `SET_UNIT` | `0x22E00C` | RW | `SEATINPUT_UNIT_MASK` (416) | – | by device instance id (case-insensitive); `STATUS_NOT_FOUND` if no such unit |
| `SET_SESSIONS` | `0x22E010` | RW | `SEATINPUT_SESSIONS` (72) | – | session id per seat, `0xFFFFFFFF` = none |
| `READ_PACKETS` | `0x22E014` | RW | – | `SEATINPUT_PACKET[]` (32 each) | inverted call, pended until packets exist; cancelled on handle close / `CancelIoEx` |
| `LIST_UNITS` | `0x22E018` | RW | – | `SEATINPUT_UNIT_LIST` (16) + `SEATINPUT_UNIT_INFO[]` (432 each) | unit id ↔ instance id, role, mask, packet counter, last activity |

Packet: `UnitId`, `Kind` (1 keyboard, 2 mouse, 3 hotkey), `Seat` (1-based owner), `Flags`
(`GAP` 1, `CONSOLE_COPY` 2, `WIZARD` 4), `Time` (interrupt time), keyboard `A`=MakeCode
`B`=Flags, mouse `A`=Flags `B`=ButtonFlags `C`=ButtonData `LastX` `LastY`, hotkey `A`=
`SEATINPUT_HK_*` (bit 0 Shift).

## Build

```
powershell -ExecutionPolicy Bypass -File installer\scripts\build-drivers.ps1 -Projects seat-input-filter
# package: target\drivers\<Platform>\<Configuration>\seat-input-filter\seat-input-filter\
#          seatinput.sys + seat-input-filter.inf (UTF-16 LE, stamped)
# then attestation signing through Partner Center (EV certificate) -- never test-signing
```

Toolchain and the script: [../README.md](../README.md) (Build). The script checks the PE
machine of `seatinput.sys` (0xAA64 ARM64, 0x8664 x64) and runs `infverif /w`; `-Analyze`
runs the WDK code analysis (DriverRecommendedRules, clean). Still to add before signing:
Static Driver Verifier, Driver Verifier with the code-integrity rules on a test VM.
HVCI-clean: NX pool only (`POOL_NX_OPTIN`), no RWX sections, `/INTEGRITYCHECK`, CFG.

- `IoGetRequestorProcess` / `IoGetRequestorSessionId` are declared in `ntifs.h`, so the
  driver includes `ntifs.h` (which includes `ntddk.h`).
- `DEVPKEY_Device_InstanceId` is defined locally in `driver.c` (devpkey.h only declares
  it without `initguid.h`).
- The control device has `FILE_DEVICE_SECURE_OPEN`, so its SYSTEM-only DACL also covers
  opens of names below it.
- The INF uses `AddFilter` in an extension INF (Windows 10 1903+, decorated
  `NTamd64.10.0...18362` / `NTarm64.10.0...18362`) and runs from the driver store (`%13%`).
## Install / remove (only after signing, maintainer's go)

```
pnputil /add-driver seat-input-filter.inf /install
pnputil /delete-driver oem<N>.inf /uninstall          # removes only our filter entry
reg add HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatInput\Parameters /v PassThrough /t REG_DWORD /d 1
                                                       # recovery: pass everything after next start
dism /image:<offline> /remove-driver /driver:oem<N>.inf  # offline recovery (WinRE)
```

## Test plan (needs a signed or test-signed build on a test VM, never on a user's PC)

1. Boot without the service: every keyboard / mouse / pad works on seat 1 (fail-open).
2. Start the service, assign a USB keyboard to seat 2: its keys no longer reach the console;
   `READ_PACKETS` returns them; seat-1 keyboard unaffected.
3. Kill the service while typing on seat 2: nothing reaches seat 1; restart within
   `GapMs`: buffered keys arrive, no key stuck (GAP → key-ups).
4. Ctrl+F12 on a seat-1 keyboard: no F12 in the console, one `HOTKEY` packet (`A`=0).
   Ctrl+Shift+F12: `HOTKEY` with `A`=1, the unit does not move; three times within 3 s:
   every unit returns to seat 1. With the service stopped: Ctrl+F12 reaches the console
   and nothing moves when the service comes back.
5. Touch screen assigned to seat 2: console window manager cannot open it
   (`ACCESS_DENIED`), the service (session 0) can, an app in seat 2's session can.
   5b. Assign a touch screen, a pen, a precision touchpad and a keyboard with media keys
   to seat 2, then replug them (or reboot so no fail-open handle survives): touching the
   screen, the pen or the touchpad must not move the console pointer, and the media keys
   must not change the console volume; seat 2 receives them.
   5c. Unplug every keyboard and mouse while the service runs, plug one back: the service
   can still attach (control device reused).
6. Safe Mode and `PassThrough=1`: everything passes.
7. Driver Verifier (standard + IRQL checking + code integrity) through steps 1–6, on ARM64
   and x64, with Memory Integrity and Secure Boot on.

## Clean room

Written fresh from the public WDK documentation on Microsoft Learn (KMDF filter drivers,
`CONNECT_DATA` / class service callback, control device objects, inverted call, extension
INFs). The MS-PL WDK samples (e.g. kbfiltr, moufiltr) are not GPL-compatible and were not
copied.
