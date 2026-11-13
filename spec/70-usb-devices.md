# 70 — USB devices, storage and other peripherals per seat

Status: draft v1 (2026-11-12, critiqued, cross-checked). Reference: the reference product, version 2.70.3
installed, 2.70.6 package inspected statically. Behaviour and interfaces only.

Items marked **HYPOTHESIS** are inferred and not confirmed by observation on a running
system. The reference product was disabled on the test PC during analysis. Because of
this, no runtime state (per-device masks, filter attachments) could be observed live.

Related specs: `50-input.md` (keyboards, mice, HID, gamepads, unfiltered HID usages),
`60-audio.md` (audio endpoints, including USB headsets and monitor audio), `40-display.md`
(monitors, USB display adapters), `10-manager.md` (screens), `20-service.md` (config model),
`30-sessions.md` (sessions, same account on several seats).

## 1. Scope

This spec covers:

- How the reference product decides which seat owns a device. It covers USB hubs and
  ports, composite devices and device containers.
- USB mass storage (disks, sticks, card-reader media) and USB optical drives. It covers
  access control and drive-letter visibility per seat or session.
- Webcams, smart-card readers, printers and other devices (serial ports, phones,
  Bluetooth, network adapters, USB A/V adapters).
- Hot-plug, the device-indication feature and the device-related parts of the manager.
- How ownership is enforced, and how Pulsar Seat should do it within `docs/agents/rules.md`.

Input devices and audio are covered elsewhere. They appear here only where they share the
ownership model.

## 2. Terms

These terms are ours. They describe the reference product's concepts; they are not its
labels.

| Term | Meaning |
| --- | --- |
| Seat | One workplace: monitor(s) + input + audio, used by one interactive session. Seats are numbered from 1. Seat 1 is always active. |
| Seat mask | A bit set with one bit per seat (bit 0 = seat 1). The product supports 12 seats in 2.70.3; 16 in 2.70.6 is a **HYPOTHESIS** (`30-sessions.md` §2). |
| Explicit mask | The mask the administrator assigned to a device node. |
| Effective mask | The mask actually enforced for a device node, after inheritance from its parents. |
| Free device | Mask 0: no seat uses the device. |
| Shared device | The mask has more than one bit set. Shared with every seat means every bit is set. |
| Common area | The manager area that holds free devices and devices shared with every seat. |
| Exclusive kind | A device kind that can belong to at most one seat. |
| Shareable kind | A device kind that can belong to several seats at once. |
| Arrival highlight | A temporary frame the manager draws around a device that was just plugged in. |
| Activity mark | A blinking marker on a device that reports use (key press, mouse move, camera use). |
| Absent mark | A marker on a device that is in the saved configuration but not plugged in. |
| Shared mark | A marker on a device shared with every seat. |
| Container | A Windows device container (`DEVPKEY_Device_ContainerId`): all functions of one physical product. |
| Container group | A number the product's diagnostic report gives to all device nodes of one container. It is a grouping index, not a seat. |
| Port chain | A path from a USB root hub through hub port numbers down to the device. |

## 3. Reference product behaviour

### 3.1 Device kinds the product manages

The control library sorts devices into a fixed set of kinds. Only these kinds appear as
separately assignable items in the manager. Everything else is managed indirectly (see 3.3)
or not at all.

Exclusive and shareable: `10-manager.md` records that displays, keyboards and mice are
exclusive (the assignment dialog offers one choice per seat), and that other kinds are
shareable (one check box per seat). The live configuration agrees: every keyboard, mouse and
display has a single-seat or empty mask, and hubs, cameras and most audio endpoints have the
all-seats mask. For the other kinds there is no direct evidence.

| Kind | How it is found | Assignable on its own | Exclusive or shareable | Default for a new device |
| --- | --- | --- | --- | --- |
| USB hub (root and external) | Device interface `GUID_DEVINTERFACE_USB_HUB` {f18a0e88-c30c-11d0-8815-00a0c906bed8}. Port count and parent port appear in the report. | yes | shareable (observed: all-seats masks) | HYPOTHESIS: shared with all (all 9 hubs on the test PC were shared with all) |
| Keyboard | Keyboard interface class | yes | exclusive | HYPOTHESIS: seat 1, or as the wizard sets it |
| Mouse | Mouse interface class | yes | exclusive | HYPOTHESIS: seat 1, or as the wizard sets it |
| Other HID collection (touch, pen, consumer keys, vendor pages) | HID interface class plus usage page/usage | yes | HYPOTHESIS: shareable for some usages (see `50-input.md`) | HYPOTHESIS: as the wizard or container sets it |
| Webcam (video capture) | Video capture / camera interface, plus its own type id in the config | yes | shareable (observed: all-seats masks) | HYPOTHESIS: shared with all (both cameras on the test PC were) |
| Microphone (audio capture), speakers (audio render) | Audio endpoints | yes | shareable | see `60-audio.md` |
| Smart-card reader | Smart-card reader interface; reader names under `HKLM\SOFTWARE\Microsoft\Cryptography\Calais\Readers\<name>` (value `Device` links the name to a device) | yes, and only on its own (it does not follow hub assignment; changelog 2.26) | HYPOTHESIS: shareable | HYPOTHESIS: shared with all |
| Monitor / display surface | See `40-display.md` | yes | exclusive | as the wizard sets it |
| USB A/V adapter (USB display + audio in one box) | Matched by a special wizard | yes, as one unit | HYPOTHESIS: exclusive | needs its own setup wizard; the product must be disabled and the PC rebooted first |

These are **not** listed as kinds. They never appear in the device list as separate items:
disk drives, CD/DVD drives, volumes, printers and print queues, portable devices
(MTP/WPD phones and cameras), Bluetooth radios and devices, network adapters, biometric
readers and serial ports. They appear only in the diagnostic device tree.

### 3.2 Ownership data model

- **Per device node, two masks.** The diagnostic report shows two masks for every managed
  device: an explicit one and an effective one. The driver contains the value names
  `mtxk_termbits`, `mtxk_nodebits` and `mtxk_terminal` (interop identifiers).
  HYPOTHESIS (the whole point): these names hold the per-device masks while the product is
  enabled, termbits being the explicit mask, nodebits the effective mask and terminal a
  single-seat form; they are removed when the product is disabled. Evidence: on the disabled
  test PC no such value exists anywhere under `HKLM\SYSTEM\CurrentControlSet\Enum`. The only
  hint of where they are stored is that the driver also builds `\Device Parameters` and a
  two-digit numbered sub-key path, which suggests the device's hardware key; device properties
  are another candidate.
- **Inheritance.** A device that is not a managed kind gets the effective mask of the nearest
  ancestor that has an explicit mask. In practice this ancestor is a USB hub, for example a USB
  disk behind a hub. A managed kind can have its own explicit mask. That mask overrides what it
  inherits from its hub. HYPOTHESIS: the override rule (whether the child mask may be wider
  than the hub mask) was not observed.
- **Containers.** The product groups all device nodes that share one container id. In the
  diagnostic tree they carry the same container-group number: a webcam's video, HID buttons,
  microphone function and audio endpoint all show one number. The product assigns or reports
  such a group as one physical device. The wizard uses container ids to keep the parts of one
  product together, for example a monitor's audio. A known bug class: two physical devices that
  report the same container id (fixed in 2.23). The container-group number says nothing about
  seats.
- **Identity across reboots.** A device is identified by where it is plugged in, not by a
  serial number. The saved configuration stores, per device:
  - a short port chain, written as root-hub index and port numbers joined by dashes;
  - a long form that is the Windows location path (PCI root, PCI functions, USB root, USB
    ports), plus a short type tag and the USB interface number. Root-enumerated devices use a
    separate prefix and their instance path;
  - the interface class GUID (device type), the friendly name seen at save time, and a
    connection type: USB-connected, PS/2 mouse, display, or a catch-all type for audio and
    non-USB HID;
  - the seat mask, written as a binary string (most significant seat on the left, so `1` =
    seat 1, `10` = seat 2, all ones = every seat, `0` = free) plus an initialized flag.

  So the same device plugged into another port is a new device, and a different device in
  the same port inherits the old assignment. HYPOTHESIS: the product does not fall back to
  hardware id or serial number when the location changes. No fallback was seen.
- **Persistence.** The assignments live in the product's saved-configuration JSON file under
  ProgramData. A binary configuration dump exists there too. The driver gets them through the
  control library when settings are applied. See `20-service.md` for the full config model.
- **Seat count limit.** A licence component exists; it can limit the number of usable seats.
  Nothing further is described here.

### 3.3 USB hubs and ports per seat

- Every USB hub, including root hubs and hubs built into monitors or keyboards, is an
  assignable item. Assigning a hub to a seat gives that seat the storage and optical drives
  plugged into that hub and into hubs below it (changelog 2.22b). HYPOTHESIS: other functions
  that are not a managed kind (serial adapters, phones) follow the hub too. Network adapters
  are covered in 3.9.
- The manager shows hubs as a tree. Each hub shows its depth below the root hub, its port
  count, what is connected to each port and its connection path. There is a view option to
  show all hubs, or to hide hubs that are empty and hubs that contain only other hubs (HKCU
  value `showAllUsbHubs`).
- **Monitor hubs. HYPOTHESIS.** The wizard contains a step that collects USB hubs belonging
  to a monitor (inferred: the same container as the monitor) and gives each such hub to the
  monitor's seat, so devices plugged into a monitor's hub follow that monitor. Evidence is
  only the step names in the wizard binary and its log. On the test PC the wizard log shows
  that step running with zero monitor hubs found, so the behaviour was never seen. What
  happens to a monitor hub when its monitor is unassigned or shared is unknown (open
  question).
- Mechanism (HYPOTHESIS): the kernel driver watches hub interface arrivals, walks the device
  tree and computes effective masks for children. It references the USB, Ports, MEDIA, Image,
  Camera and Xbox classes in addition to the four classes it is a registered upper filter for
  (Display, HIDClass, Keyboard, Mouse). A service value `propFilters`=1 is described as
  propagating filtering to child devices. The likely reading is that the driver attaches
  itself to child device stacks of these extra classes at run time, without class registry
  entries, so it can gate opens per seat.

### 3.4 USB storage and optical drives

- **What is controlled.** Disks and CD/DVD drives that sit below an assigned USB hub
  (changelog 2.22b). HYPOTHESIS: internal SATA/NVMe disks are not controlled. Supporting
  evidence: the changelog speaks only of USB disks on assigned hubs, and the diagnostic
  report's list of masked devices contains no disk at all (internal or USB). The USB disk's
  control is therefore assumed to come from its hub's mask. The report's container-group
  numbers do not show seat control: the USB disk has its own group, different from its hubs,
  and the NVMe disk has none because it is not part of a multi-function container.
- **How access is denied (observable behaviour).** A file or directory open on a USB volume
  whose disk is not owned by the requesting process's seat is refused. HYPOTHESIS: the error
  is an access-denied status, and SYSTEM and services are not restricted. The product uses a
  file-system minifilter at a high altitude for this; it cannot be unloaded while running and
  goes away only when the product is disabled and the PC rebooted.
- **Switch.** DWORD `disableFSFilter` under the driver service key: 1 turns the storage
  filtering off, 0 or absent turns it on. The manager's support-settings list offers it as a
  patch.
- **Raw device access. HYPOTHESIS.** The driver builds the Windows names of raw CD-ROM and
  physical-drive devices (`\\.\CdRomN`, `\\.\PhysicalDriveN`). It may also block raw opens of
  another seat's disk and optical drive. A file-system filter does not see raw device opens,
  so another means would be needed; none was identified.
- **What other seats see.** The drive letter stays visible to everyone. Opening it from a
  non-owning seat fails. The changelog says the drives are hidden only if the user has turned
  on both Explorer options that hide empty drives and hide hidden files, folders and drives.
  HYPOTHESIS: a drive whose root cannot be opened looks empty or hidden to Explorer, and those
  options then hide it. The product does not set these options itself. The test PC had the
  defaults.
- **Explorer volume cache. HYPOTHESIS.** The driver also presents a filtered view of the
  per-user Explorer volume cache (`...\Explorer\MountPoints2\CPC\Volume`). This may stop
  Explorer in one seat from showing or auto-playing volumes that belong to another seat.
- **No policy keys are used.** The product does not use the Windows Removable Storage Access
  policies or the Explorer `NoDrives`/`NoViewOnDrive` policies. Observed: none set, and the
  binaries do not reference them.

### 3.5 Drive-letter visibility per session

- Drive letters stay global: the product does not move them into per-session or per-logon
  namespaces (no evidence of `DefineDosDevice` or mount-point APIs in the binaries).
  Visibility is controlled only by the access denial and Explorer behaviour above.
- Network drives and `subst` letters keep normal Windows per-logon-session behaviour. The
  product does nothing to them.

### 3.6 Webcams

- Webcams are a managed, shareable kind. On the test PC every camera was shared with all
  seats.
- The manager can show a live preview of every camera at once so the admin can tell them
  apart (2.51). Cameras also take part in device indication.
- The device type used in the saved configuration is a product-specific GUID, not the Windows
  camera interface GUID. The device path is the camera's video-capture interface.
- History: cameras became assignable in 2.20. 2.31 reworked how camera ownership is enforced
  on Windows 8/10/11. The reworked method is on for new installs and off when upgrading.
  HYPOTHESIS: it deals with the Windows camera frame-server service, which opens cameras on
  behalf of apps from session 0, so a per-session check on the camera device alone no longer
  works. The method itself was not identified. Candidates seen in the driver: the Camera and
  Image class GUIDs, process/image-load notifications, and writes of the per-device
  session-id property (the latter seen for audio endpoints).
- A webcam's built-in microphone is a separate audio-capture item in the same container
  (`60-audio.md`).

### 3.7 Smart-card readers

- Since 2.26, readers can be assigned to seats. They do not follow hub assignment. Each
  reader must be assigned on its own.
- Mechanism (HYPOTHESIS): the driver presents a per-seat view of
  `HKLM\SOFTWARE\Microsoft\Cryptography\Calais\Readers`, so each seat's session sees only its
  own readers and the smart-card stack and logon screen in that session list only them.
  Whether the smart-card service itself is also restricted was not determined.

### 3.8 Printers

- The product does nothing with printers. There are no spooler, print-queue or printer-port
  references in any binary, no printer kind in the manager, and nothing product-related under
  `Control\Print` on the test PC. All seats see all printers, as on any multi-user Windows PC.
  The default printer is per user, as in standard Windows.
- USB printers behind a hub assigned to a seat are not isolated. HYPOTHESIS: the spooler opens
  the USB print device from session 0, so a per-session check would not apply.

### 3.9 Other devices

- **Serial ports.** The driver references the Ports class and a `PORTSfilters` value.
  HYPOTHESIS: USB serial adapters behind an assigned hub are gated per seat like storage. Not
  confirmed.
- **Network adapters. HYPOTHESIS: not affected by hub gating.** On the test PC USB Ethernet
  adapters sit below two hubs that are managed items. The driver's class list has no entry for
  the Net class. So a USB network adapter on a seat's hub most likely stays usable by every
  seat, as network adapters are system-wide in Windows anyway.
- **Phones / MTP / WPD, Bluetooth, biometric.** These are not managed. They are reachable
  from every seat, apart from any side effect of hub-based gating. Bluetooth HID devices
  (keyboards, mice, pads) are handled through the HID filtering in `50-input.md`. The
  Bluetooth radio is not per seat.
- **Unfiltered HID usages.** The driver service value `UnfilteredIDs` lists HID usages that
  are never isolated. It concerns HID only, not storage or other USB functions; see
  `50-input.md`.
- **Gamepads** (HID, Xbox GIP): see `50-input.md`.
- **USB A/V adapters** (one USB box with video + audio): see 3.1. They are handled by a
  dedicated setup wizard and need the product disabled and a reboot first.
- **USB display adapters** (display-only devices): see `40-display.md`.

### 3.10 Hot-plug and device indication

- The manager listens for device arrival and removal. A newly plugged device gets the
  arrival highlight for a set time. The time is set with a slider from 2 to 20 s in 2 s steps
  and stored as HKCU `secondsToFlashInsertedDevice` (default 4).
- Devices that are in the saved configuration but unplugged stay in the list with the absent
  mark. Devices that report activity show the activity mark. Devices shared with every seat
  carry the shared mark.
- To find which physical device is which, the manager tells the admin either to watch the
  activity mark, or to unplug and replug the device: a replugged device appears at the end of
  its kind's list.
- A device newly plugged into a hub that is already assigned gets the hub's seat at once
  (inheritance). HYPOTHESIS: no re-apply is needed.
- **HYPOTHESIS (not observed):** a new managed-kind device in an unassigned location gets a
  default for its kind. The only evidence is the live configuration, where hubs and cameras
  are shared with all seats and keyboards and mice each sit on one seat.

### 3.11 Manager parts that concern devices

Exact layout is in `10-manager.md`. Device-related parts:

- **Seat panels and the common area.** Every seat has a panel with its devices. The common
  area holds free and shared devices. Devices are moved by drag and drop, or with a context
  menu that has an action to make the device free.
- **Assignment dialog per device.** For shareable kinds it has one check box per seat plus an
  action that selects every seat. For exclusive kinds it has one radio button per seat. A
  separate action makes the device free. The dialog saves or cancels.
- **Device tooltip.** Shows kind, name and current assignment (one seat, every seat or free).
  For USB nodes it also shows manufacturer, product and serial number. For hubs it shows the
  port count, the device on each port and the connection path.
- **View settings.** Show shared devices inside seat panels too (below the seat's own
  devices, with the shared mark). Show/hide the shared-device area. Show all hubs or only
  useful ones. Icon size. Arrival-highlight duration. Show or hide device identifiers.
  Custom icon per device, stored in a small JSON file under ProgramData.
- **USB tree view.** A separate tree of the USB topology.
- **Camera preview.** Shows the picture of all cameras at once.

### 3.12 Configuration compare and restore

On start, and on request, the manager compares the current device set with the saved
configuration and lists:

- devices added since the last save;
- devices whose seat assignment changed since the last save, with the old assignment;
- devices that are absent, i.e. unplugged since the last save.

It then offers to restore the saved assignments (2.60.5).

### 3.13 Registry and files (this area)

`<drv>` is the reference product's kernel driver service key under
`HKLM\SYSTEM\CurrentControlSet\Services`. Its literal name is an interop identifier listed
only in `90-install.md` §11, so Pulsar Seat can detect the reference product and refuse to
run beside it. Pulsar Seat must not use it.

| Location | Name | Type | Meaning |
| --- | --- | --- | --- |
| `<drv>` | `disableFSFilter` | DWORD | 1 = storage filtering off |
| same | `propFilters` | DWORD | 1 = propagate filtering to child devices (live value 1) |
| same | `PORTSfilters` | DWORD | HYPOTHESIS: serial-port filtering on/off |
| same | `UnfilteredIDs` | MULTI_SZ | HID usages never isolated; see `50-input.md` |
| `<drv>\Instances` and its instance sub-key | `DefaultInstance`, `Altitude`, `Flags` | SZ, SZ, DWORD | minifilter instance registration: altitude `429999`, flags 0 |
| device nodes (HYPOTHESIS: hardware key or device property) | `mtxk_termbits`, `mtxk_nodebits`, `mtxk_terminal` | mask | HYPOTHESIS: per-device ownership while enabled |
| `HKLM\SOFTWARE\Microsoft\Cryptography\Calais\Readers` | (filtered view) | — | HYPOTHESIS: per-seat reader list |
| `HKCU\...\Explorer\MountPoints2\CPC\Volume` | (filtered view) | — | HYPOTHESIS: per-seat volume cache |
| HKCU manager settings key | `showAllUsbHubs`, `secondsToFlashInsertedDevice`, `sharedShownInWorkplaces`, `sharedHideoutShown_<n>` | various | view options |
| ProgramData product folder | saved-configuration JSON, configuration dump, icon-override JSON, pre-enable diagnostic report | files | see 3.2 |

### 3.14 Windows mechanisms used

These are the mechanisms the reference product uses. They are not an endorsement for Pulsar
Seat.

- User mode:
  - enumeration with SetupAPI and CfgMgr32 (class devices, interfaces, device properties,
    parent walk, location information, bus number and address);
  - USB hub port count and the port of each child. HYPOTHESIS: read with the standard USB hub
    node IOCTLs. This is inferred from the report fields (port count, parent port); only a
    generic device I/O control import was seen;
  - HID preparsed data for usages;
  - RegisterDeviceNotification for hot-plug;
  - SetupAPI class-installer calls to restart devices after a change.
- Kernel (x64 WDM driver):
  - PnP device-interface notifications and container information;
  - writing per-device properties;
  - attaching to device stacks (HYPOTHESIS, see 3.3);
  - a file-system minifilter at a high altitude that cannot be unloaded while running;
  - a registry filter that returns per-seat views of selected keys (HYPOTHESIS for the exact
    effect);
  - mapping the requesting process's session to a seat.

### 3.15 Error cases and edge cases

- **Device moved to another port.** It is a new device and loses its assignment. The compare
  view reports the old entry as absent and the new one as added.
- **Same container id on different devices.** The wrong functions get grouped together.
  Fixed in 2.23 by better grouping, but the root cause is firmware.
- **Conflicting assignment on save.** For example an exclusive device on two seats. The
  manager lists the conflicting devices and asks whether to save anyway or cancel; it does not
  refuse outright. HYPOTHESIS: the wizard behaves the same. Whether a failed apply is rolled
  back was not determined.
- **Storage filter off** (`disableFSFilter`=1). USB disks are reachable from every seat.
- **Explorer options off** (the default). Other seats still see the drive letters of a
  seat's USB disk. Opening one fails with an access error.
- **Smart-card reader on a seat's hub.** It does not follow the hub and must be assigned by
  hand.
- **Configuration changed while the product is disabled.** On the next start the compare
  view is shown.
- **Same account on several seats.** Since 2.22 there is an option to deny one user logging
  on at two seats (off by default); since 2.26 the product warns when this happens. See
  `30-sessions.md`. It matters here because per-user state (camera consent, default printer,
  per-user Explorer cache) cannot tell two seats of one account apart.

## 4. ARM64 notes

- The reference product cannot do any of this on ARM64. Ownership enforcement, storage
  filtering, the registry filter and the child-device attachment all live in one x64 kernel
  driver, and Windows on ARM64 cannot load x64 kernel code. The user-mode parts would run
  only under emulation and have nothing to talk to.
- Every mechanism Pulsar Seat needs here has a native ARM64 equivalent:
  - SetupAPI, CfgMgr32 and `CM_Register_Notification`;
  - USB hub IOCTLs. The USB stack (USB hub3/xHCI) is the same on ARM64. Qualcomm and other
    ARM64 SoCs expose standard xHCI root hubs, but some laptops route ports through internal
    hubs or USB4/Thunderbolt-like routers. Port chains therefore differ from x64 boards and
    must not be hard-coded;
  - device security descriptors through SetupAPI;
  - per-logon-session DOS device namespaces and the mount manager;
  - the print spooler API;
  - Filter Manager, which is present on ARM64. A minifilter, if one is ever approved (5.3),
    must be compiled for ARM64 and x64 and signed for both.
- ARM64 devices are often laptops with few USB ports, so a typical seat setup uses one USB
  hub or dock per seat. Hub-based assignment is therefore the primary flow on ARM64.
- Location paths on ARM64 often start from an ACPI node instead of a PCI root. The identity
  format must accept any location-path shape.
- Built-in cameras on ARM64 laptops are usually MIPI/ISP cameras, not on a USB hub; some
  ARM64 devices do use USB-attached internal cameras. A camera that is not on a hub can only
  be assigned on its own.
- Camera isolation is not an ARM64-only problem. On Windows 10/11 the frame server mediates
  Media Foundation camera access for UVC cameras as well as MIPI cameras, on every
  architecture. The limits in 5.2(7) apply to x64 and ARM64 alike.

## 5. Pulsar Seat approach

Constraints (from `docs/agents/rules.md`):

- no vulnerable drivers and no kernel patching;
- must work with HVCI and Secure Boot on, without test-signing;
- native ARM64 and x64;
- drivers only on KMDF/UMDF (and IddCx); prefer user mode and UMDF.

The rule is to enforce with documented Windows security objects first. Any kernel component
needs the maintainer's explicit OK (5.3).

### 5.1 Ownership model (same idea, our own format)

- Keep the model: a seat mask per device node, with an explicit and an effective mask.
  Inheritance runs from the nearest assigned ancestor, normally a hub port or hub. Container
  grouping is applied, and each kind has a default policy (exclusive or shareable).
- Identify devices by location path plus port chain. Store hardware id, container id and
  serial number when there is one as secondary keys. When a known device turns up on a new
  port, offer to move its assignment instead of silently treating it as new. This is better
  than the reference product (3.15).
- Allow assignment per hub, per hub port (one port of a shared hub), and per container.
- Monitor hubs: our wizard offers to give a monitor's hub (same container) to the monitor's
  seat. If the monitor is later unassigned or shared, the hub keeps its own mask and the
  manager flags the mismatch; nothing moves silently.
- Network adapters are never gated, even below a seat's hub (system-wide resource). The
  manager shows them as not isolated.
- Config lives in the service config (see `20-service.md`). Our format is not tied to the
  reference format. An optional importer for the reference product's saved-configuration JSON
  may be added later. Its shape is described in 3.2.

### 5.2 Enforcement building blocks (user mode, documented)

1. **Per-device security descriptor.**
   - The seat service runs as LocalSystem.
   - It writes a device security descriptor on the devnode of each owned device: SetupAPI
     device registry property for security, and the characteristic that forces secure open
     on the whole device namespace.
   - For storage the descriptor must go on both the disk devnode and the volume devnode
     (`STORAGE\Volume`, Volume class). File opens go through the volume device, not the disk.
     CD/DVD: the CD-ROM devnode, which is also the volume device for CDFS/UDF (HYPOTHESIS).
   - The descriptor allows SYSTEM, Administrators (configurable) and the logon SID of the
     owning seat's interactive session. The logon SID is per logon, so this works even when
     the same account is logged on at two seats.
   - What it adds over Windows defaults (to verify on a prototype):
     - `\\.\PhysicalDriveN`: already admin-only by the default disk ACL, so little is added
       for standard users. HYPOTHESIS for removable disks.
     - `\\.\CdRomN`: standard users can read raw by default unless the AllocateCDRoms policy
       is on. The descriptor closes this per seat. HYPOTHESIS: AllocateCDRoms with several
       interactive sessions has undefined seat semantics; do not rely on it.
     - Volume opens (`\\.\E:`, `\\?\Volume{GUID}`): HYPOTHESIS, the descriptor on the volume
       devnode gates them. File opens: see (4).
     - Serial ports, raw HID (with `50-input.md`) and vendor USB devices (WinUSB/libusb
       style apps): the descriptor is the main gate.
   - **Open handles.** Rewriting a descriptor does not revoke handles already open, for
     example an old session's Explorer or a background process. When ownership moves, or a
     seat logs off and its device returns to free or to another seat, the service must
     invalidate them:
     - storage: lock and dismount the volume (refuse and tell the admin if a lock fails, to
       avoid data loss), or restart the device;
     - other devices: restart the devnode (query-remove first, honour a veto, report it).
     - At logoff the seat's own processes are gone; remaining handles belong to services or
       other sessions, and the same rule applies.
   - The service rewrites the descriptor at each logon and logoff of a seat and at service
     start.
2. **Race-free default (needs maintainer sign-off as a system change).**
   - Set a class-level default security descriptor (class registry property for security)
     on the classes the admin chooses to isolate.
   - New devices then start out closed to normal users until the service opens them to the
     owning seat. This avoids the window between arrival and the service reacting.
   - A class descriptor on DiskDrive, CDROM or Volume also applies to internal and system
     disks, not only removable ones. It is not a rules.md violation, but it changes global
     security state and can break the system (boot, backup, AV, shell). So:
     - off by default; enabled only by an explicit admin choice, after maintainer sign-off;
     - the previous class value is saved and restored on uninstall and on disable;
     - internal devices get an explicit open-to-all device descriptor at the same time.
   - HYPOTHESIS to prototype: which classes need it (DiskDrive, CDROM, Volume, Ports, WPD),
     and whether system components (Explorer, AutoPlay, Defender scans) cope with it.
3. **Drive letters per seat.**
   - For a USB volume owned by exactly one seat, its global drive letter is removed and a
     small agent in the owning seat's session defines the letter in that logon session's own
     DOS-device namespace (DefineDosDevice from a non-system caller is per logon session).
   - Result: only that seat sees the letter in Explorer, file dialogs and `GetLogicalDrives`.
     This is real per-session visibility, which the reference product does not have.
   - For shared volumes, keep the global letter.
   - Gaps to solve:
     - **Arrival race.** The mount manager assigns the global letter at arrival, before the
       service can remove it, and volume-arrival notifications reach every session (Explorer,
       AutoPlay). Prefer stopping the automatic letter for that volume only, not globally
       (do not turn off automount system-wide). HYPOTHESIS: candidates are the per-volume
       no-default-letter attribute and clearing the remembered letter before re-arrival.
     - **Remembered letters.** The mount manager stores remembered letters in
       `HKLM\SYSTEM\MountedDevices` and re-assigns them on the next arrival. The service
       removes the global `\DosDevices\X:` entry for a seat-owned volume and keeps its own
       record of the letter the seat used, so it can redefine the same letter per session.
       The volume GUID entry is left alone.
     - **UAC split tokens.** The filtered and elevated tokens of one user are separate logon
       sessions with different LUIDs. A letter defined in the filtered session is invisible
       to elevated apps of the same seat. This is the same problem as mapped drives and
       `EnableLinkedConnections`. The agent may need to define the letter in the linked
       session too (HYPOTHESIS: whether that is possible without a SYSTEM helper).
     - Explorer refresh and AutoPlay with per-session letters; what happens when the agent is
       not running; interaction with BitLocker To Go (HYPOTHESIS, prototype).
4. **File-level access on the volume.**
   - Hiding the letter is not enough. Another seat can still reach `\\?\Volume{GUID}\`.
   - HYPOTHESIS, must be tested per file system: the descriptor on the volume devnode with
     secure open set applies to file opens under that volume. Test the disk devnode and the
     volume devnode separately.
   - If it does not hold, the only fallback is a file-system minifilter of our own design
     that gates opens by requestor session against the volume's owner mask. This is **not
     allowed by default**: a minifilter is a Filter Manager kernel driver, neither KMDF nor
     UMDF, so it falls outside the rules.md driver list. It needs the maintainer's explicit
     OK (see 5.3 and open questions). If approved, it is optional, installed only when the
     admin enables strict storage isolation, and uses an officially requested altitude.
5. **Printers (optional, HYPOTHESIS).**
   - Possibly better than the reference: per-seat printer visibility through the printer
     security descriptor (spooler API). HYPOTHESIS: EnumPrinters may still list printers the
     caller cannot use, so this may give per-seat use but not per-seat visibility. Prototype.
   - Lifecycle: logon SIDs change at every logon. A printer ACL that holds them must be
     rewritten at each seat logon, cleaned at logoff, and repaired at service start after a
     crash (remove stale SIDs). A stable alternative is user SIDs, which fails for the same
     account on two seats.
   - The seat agent can set a per-seat default printer at logon (per user, same caveat).
   - The spooler still opens the USB device from session 0, which is correct.
6. **Smart-card readers.**
   - v1: no isolation. Readers are shared and documented as such.
   - Later: research per-session reader visibility. The smart-card service is one service in
     session 0, and there is no documented per-session reader filter for local sessions. Do
     not use a registry callback driver for this.
7. **Webcams (all architectures).**
   - v1: cameras are shared by default (same as the reference default). An exclusive camera
     uses (1) for direct-open stacks.
   - Media Foundation access through the frame server runs in session 0, for UVC and MIPI
     cameras alike, so the device descriptor does not tell seats apart there. Therefore:
     - the per-user camera privacy setting (consent store) can be set per seat user as a
       coarse "this seat has no camera" switch. Limits: it is per user, not per seat, so it
       fails when one account is on two seats; and it may not cover every desktop app that
       opens a camera directly (HYPOTHESIS, test DirectShow and direct KS opens);
     - exact per-camera, per-seat isolation through the frame server is an open question.
   - Do not inject into processes or hook the frame server.
8. **Hubs and ports.** These need no enforcement of their own. They are only the place where
   ownership is inherited. The service re-evaluates the subtree on every arrival.

### 5.3 What Pulsar Seat must not copy, and kernel rules

Never:

- kernel attachment to arbitrary device stacks without an INF;
- kernel process or image-load hooks for device isolation;
- registry-callback per-seat views of system keys;
- an unallocated or copied minifilter altitude;
- test-signing.

A kernel component is a last resort and needs the maintainer's explicit OK. Allowed
candidates are an INF-installed KMDF filter (UMDF preferred) or, only with that OK as an
exception to the rules.md driver list, a documented Filter Manager minifilter for storage.
Hard requirements for any kernel component:

- built for ARM64 and x64; PE machine type checked;
- HVCI-compatible build: no executable pool (non-executable pool only), no RWX sections,
  linked with `/INTEGRITYCHECK`, passes the HVCI code-integrity checks;
- attestation-signed (or WHQL); loads with Memory Integrity on and Secure Boot on, without
  test-signing or any DSE change;
- unloadable for uninstall and update without kernel tricks.

### 5.4 Manager behaviour to provide

The manager should match the reference flow (3.10–3.12). UI copy is Turkish and written
fresh:

- the USB hub tree;
- drag and drop;
- the assignment dialog with exclusive and shareable modes;
- the shared and free areas;
- arrival highlight with a configurable duration, plus activity, absent and shared marks;
- camera preview;
- compare with the saved configuration and restore;
- on save, list conflicts and let the admin save anyway or cancel (as the reference does).

Additions:

- a prompt to move an assignment to a device's new port;
- a per-device isolation-status row that shows which enforcement block applies: descriptor,
  per-session letter, minifilter (if approved), or none (shared, not supported, or network);
- a warning when a seat-isolated device is used by an account that is logged on at two
  seats (per-user mechanisms cannot separate them; see `30-sessions.md`).

### 5.5 Hot-plug pipeline (service)

1. Arrival: register for interface notifications (disk, CD-ROM, volume, USB device, hub,
   camera, smart-card reader, COM port, printer).
2. Compute the effective seat from the parent chain and the container.
3. Apply the device descriptor (disk and volume devnodes for storage).
4. For a volume: stop or remove the global letter, update `MountedDevices`, and tell the
   owning seat's agent to define the per-session letter.
5. Notify the manager, which shows the highlight.
6. Removal: clean up the per-session letters and the state.
7. Ownership change or seat logoff: rewrite descriptors and invalidate open handles (5.2(1)).

All steps are idempotent and re-run at service start and at every seat logon.

## 6. Open questions

1. Does a device security descriptor with secure open set on the **volume** devnode (and,
   separately, the disk devnode) control file opens through the mounted volume? Test NTFS,
   exFAT, FAT32 and UDF/CDFS. This decides whether strict storage isolation needs a
   minifilter.
2. If a minifilter is needed: will the maintainer grant the exception to the rules.md driver
   list, pay for attestation signing (plan.md known risk) and request an altitude? Or is
   strict storage isolation dropped in favour of hidden letter + descriptor only?
3. Exact behaviour of the reference product's raw-disk/CD-ROM blocking. The mechanism was not
   identified. Is it needed for parity at all, given the default disk ACL?
4. Inheritance precedence when a managed device below a hub has its own mask: does the child
   mask replace the hub's mask, or is it narrowed by it? Observing this needs the product
   enabled on the test PC (system change; maintainer approval required).
5. The reference product's reworked camera method (2.31): what does it actually restrict, and
   does it work with apps that use the frame server? Observing it needs the product enabled
   and two seats running.
6. Smart-card readers: does the reference product also restrict the smart-card service, or
   only the registry view? Is per-seat reader isolation needed in v1 at all?
7. Serial ports (`PORTSfilters`): is this a real feature of the reference product, and is it
   on by default?
8. Should Pulsar Seat isolate MTP/WPD phones and Bluetooth (the reference product does not)?
   Proposed: no in v1, descriptor-based later.
9. Per-session drive letters: how do Explorer, AutoPlay and shell notifications behave when a
   letter exists only in one logon session's namespace? Does Windows Defender's on-access scan
   still work? Can automatic letter assignment be suppressed per volume for removable media?
   Prototype on x64 and ARM64.
10. UAC split tokens: how does a seat's elevated session see a per-session letter (same issue
    as `EnableLinkedConnections`)? Does the elevated token carry the same logon SID used in
    the device descriptors? Verify both.
11. Is the Administrators group allowed through all seat isolation by default (so an
    administrator on seat 2 can open seat 1's disk)? Proposed: yes, configurable.
12. Monitor hubs in the reference product: confirm the redistribution on a PC with a hub in a
    monitor, and what happens when the monitor is unassigned or shared.
13. Same account on two seats: per-user mechanisms (camera consent, default printer, printer
    ACL by user SID) cannot separate the seats. Should Pulsar Seat refuse seat-isolated
    devices for such a session, or only warn? Cross-check with `30-sessions.md`.
14. Class-level default descriptors (5.2(2)): does the maintainer accept this system change,
    and for which classes?
15. Cameras that are not on a hub (MIPI, or USB internal on some ARM64 devices): is "seat 1
    only or shared" acceptable when the camera cannot be isolated per seat?
