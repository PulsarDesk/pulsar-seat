# 50 — Input devices per seat

Status: draft v1 (2026-11-12, critiqued, cross-checked). Analyst-written clean-room behaviour spec. No code.
Items marked **HYPOTHESIS** come from names, imports, strings or indirect evidence and have
not been confirmed by watching the reference product run (its service is disabled on the
test PC).

Covers keyboards, mice, touchpads, touch screens, pens, gamepads and other HID devices:
how a device is identified, how it is assigned to a seat, how its input reaches only that
seat's session, hot-plug, hotkeys, system buttons and Ctrl+Alt+Del.

Related specs: `10-manager.md` (assignment screens, seat count), `20-service.md` (service,
config, recovery), `30-sessions.md` (one session per seat, session model),
`40-display.md` (monitors), `70-usb-devices.md` (USB hubs, storage),
`80-apps-compat.md` (process-level tricks, app seat API).

## 1. Terms

- **Seat**: one workplace (monitor + keyboard + mouse + audio) used by one person. Seat 1 is
  the console seat. It is always on. The maximum seat count is owned by `10-manager.md`.
- **Seat mask**: a bit set with one bit per seat. Bit 0 is seat 1. In the exported
  configuration the mask is written as a string of binary digits, with seat 1 as the
  rightmost digit. Example: `1` = seat 1, `10` = seat 2, `0` = no seat.
- **Input unit**: the smallest thing that can be assigned to a seat (see 2.1).
- **Pool**: devices that belong to no seat, or to every seat (shared). The manager shows them
  in a separate area for system, spare and shared devices.

## 2. Observed behaviour of the reference product

### 2.1 What can be assigned

The product does not assign whole USB devices. It assigns input units. An input unit is
one device interface:

| Unit | Device interface class | What it covers |
| --- | --- | --- |
| Keyboard | keyboard interface `{884B96C3-56EF-11D1-BC8C-00A0C91405DD}` | one keyboard devnode (HID keyboard collection, PS/2 keyboard) |
| Mouse | mouse interface `{378DE44C-56EF-11D1-BC8C-00A0C91405DD}` | one mouse devnode (HID mouse collection, the mouse part of a touchpad, PS/2 mouse) |
| HID | HID interface `{4D1E55B2-F16F-11CF-88CB-001111000030}` | one HID top-level collection that is not a keyboard or a mouse |

HID units seen in the live device list, with their HID usage page / usage:

- touch screen: digitizer page 0x0D, usage 0x04;
- pen / tablet: 0x0D / 0x02;
- precision touchpad: 0x0D / 0x05. The touchpad's mouse collection is a separate Mouse unit;
- consumer control (media keys on keyboards, webcams, etc.): 0x0C / 0x01;
- game controller, for example a DualSense pad. It is assigned as a plain HID unit.

Other observations:

- One physical keyboard or receiver produces several units, for example one keyboard, one
  mouse, one consumer-control and one system-control collection. Each unit can go to a
  different seat. The product does not group them automatically. It only uses the USB
  hub and ContainerID as hints in its first-run wizard (see 2.3).
- One I2C touch controller on a laptop produces several HID collections from one parent
  devnode: on the test PC a touch screen, a pen and several vendor-defined collections, all
  children of the same ACPI I2C HID device.
- Vendor-defined collections (usage pages 0xFF00–0xFFFF) are listed in the diagnostic
  device tree, but they are not assignable units in the device list.
- The diagnostic device tree also tags collections by role: system keyboard (0x01/0x06),
  system mouse (0x01/0x02), system consumer (0x0C/0x01), system control (0x01/0x80, the
  power, sleep and wake buttons).
- Devices enumerated by the Remote Desktop input bus (TERMINPUT_BUS) and root-enumerated
  devices are excluded from filtering (**HYPOTHESIS**, from exclusion names in the driver).
  No root-enumerated input unit appears in the live exported configuration.
- A consumer-control collection was seen with mask 0 (free, no seat).

### 2.2 How devices are identified and stored

Every unit is saved in the exported configuration with these fields (key names are
interop identifiers):

- `CanonicalName`: a human-readable locator.
  - For USB devices: the USB port chain from the root hub, as dash-separated port numbers
    (for example `0-1-1-2-2`).
  - For non-USB devices: a short index, or the device's friendly name.
- `CanonicalName2`: a location string. Its parts:
  - a prefix letter: `A` = based on the Windows location path; `B` = root-enumerated
    device, where the device instance path stands in for the location path (no location
    path exists for such devices);
  - the location path (or instance path), with segments separated by `|`. USB example:
    PCIROOT/PCI/USBROOT/USB(n)…; I2C example: ACPI(_SB_)/ACPI(bus)/ACPI(device);
  - `&` followed by the first 8 hex digits of the interface class GUID;
  - `USBMI(n)` when the unit belongs to a USB composite interface. ACPI/I2C entries and
    non-composite USB devices carry no `USBMI` part;
  - for other device kinds an index suffix follows: `SRn` on display entries and `FRn`
    plus a filter GUID and pin name on audio entries (owned by `40-display.md` and
    `60-audio.md`). No input unit in the live configuration carries such a suffix.
- `DeviceTypeGUID`: the interface class GUID from 2.1.
- `InitialName`: the friendly name when the unit was first seen. It is for display only.
- `Type`: how the device is connected:
  - `USB_ConnectedDevice`;
  - `PS2_ConnectedMouse`: non-USB mice. This includes I2C/ACPI touchpads;
  - `Assorted`: non-USB HID units such as I2C touch screens, pens and touchpads.
- `StationMask {Initialized, Mask}`: the seat mask (see section 1).

Consequences:

- An assignment follows the **port**, not the device serial. Moving a keyboard to another
  USB port makes it a new unit. Plugging a different keyboard into the same port gives it
  the old seat.
- **Identity collision.** `CanonicalName2` does not always identify one unit:
  - The touch screen and the pen of one I2C controller had identical `CanonicalName2`
    values in the live configuration (same ACPI path, same HID interface class, no
    `USBMI`).
  - A USB DualSense pad was listed twice with identical `CanonicalName2` values and
    different seats (seat 1 and seat 2).
  - Cause, verified live: a HID top-level collection devnode has no location path of its
    own. Only its parent (the USB interface or the I2C HID device) has one. So the
    reference key cannot tell sibling collections of one parent apart. How the product
    resolves the two entries (list order, first match, or both) is unknown.
- Bluetooth HID devices (classic and LE) have no location path at all (seen live for a
  DualSense on Bluetooth and a Bluetooth LE mouse). How the reference product keys them is
  not verified. **HYPOTHESIS**: it falls back to the instance path or friendly name.

At run time, the diagnostic report shows these fields for each unit:

- the configured mask, and an **effective** mask. The effective mask may differ when the
  seat is inherited from a USB hub (**HYPOTHESIS**);
- the parent hub and the port number on it;
- a last-activity counter. The manager uses this counter to flash the device it shows.

Persistence on the devnodes:

- The kernel component stores a per-devnode seat value as a DWORD in the devnode's driver
  (software) key under `HKLM\SYSTEM\CurrentControlSet\Control\Class\{class}\NNNN`.
- Seen values: 0x1 on keyboards and mice, including absent ones, and 0xFFFFFFFF on a
  consumer-control collection.
- These values did not match the seat masks in an earlier report.
- **HYPOTHESIS**: while the product is enabled, the driver also writes run-time devnode
  properties that hold a unit's seat mask and its node's mask. Evidence: two property-name
  strings in the driver and its import of the device-property write routine. Nothing was
  observed live, because the product is disabled. If true, the authoritative per-unit
  assignment is the exported configuration plus these run-time properties, and the
  driver-key value only marks the unit as known, or is reset on disable.

### 2.3 How the user tells devices apart (identification flows)

1. **Activity indicator.** In the manager, every device picture flashes while the device is
   used, for example while a mouse moves. A device plugged in a moment ago flashes in a
   different colour for a few seconds. The per-user setting
   `secondsToFlashInsertedDevice` has a default of 4. An unplugged device is drawn as
   disconnected. A device shared with every seat has its own marker.
2. **Replug.** The user unplugs a device and plugs it in again. The device then appears at
   the end of the list for its type.
3. **Drag and drop or a dialog.** The user drags a device onto a seat. Alternatively, a
   device-to-seat dialog lists the seats:
   - Shareable device types get one check box per seat, plus choices for all seats and for
     no seat.
   - Non-shareable types get radio buttons, so only one seat can be chosen.
   - **HYPOTHESIS**: keyboards and mice are not shareable, while some HID units and other
     device types are.
4. **First-run console wizard (older tool).**
   - The user presses any key on the main keyboard and moves the main mouse. The tool
     detects them through raw input.
   - The tool saves three items to a small text file: the main display, the mouse
     interface path and the keyboard interface path.
   - Those devices go to seat 1. The tool extends the desktop to every monitor and creates
     one seat per monitor.
   - The remaining HID devices are spread over the seats by USB hub and ContainerID. The
     goal is to keep devices that share a hub or a monitor on the same seat.
5. **On-screen configuration wizard (current).** It runs on every seat's monitor at the
   same time.
   - The user picks the monitor for seat 1 first.
   - On seat *n*, the user presses function key F*n* a set number of times (twice) on the
     keyboard meant for that seat. Supported keys are F1–F12.
   - The user then clicks a few randomly placed targets with the mouse meant for that
     seat.
   - Enter saves the current seat and Backspace resets it. With Shift held, Enter and
     Backspace act on all seats at once.
   - Shift+Esc or Alt+F4 leaves the wizard.
   - Before the wizard starts, the user chooses one of three variants:
     - run the wizard for seats 1..N;
     - keep the devices shown for seat 1 and run the wizard for seats 2..N;
     - set up seats 1 and 2 automatically.
6. **Configuration compare.** The manager compares the live device set with the last
   saved one. It reports three cases:
   - devices added since the save;
   - devices whose seat changed (with the old seat shown);
   - devices that are now absent.

   The manager offers to restore the saved assignment. Some steps require a fixed set of
   devices. If the hardware changes in the middle of such a step, the manager reports it
   and asks to run the step again.

### 2.4 Seat readiness rules

The seat status colours and the readiness rules are owned by `10-manager.md`. Input
devices affect them as follows:

- A seat **must** have a connected display, or it is not operational (red).
- A seat **should** have a keyboard and either a mouse or a HID pointing unit, for example a
  touch screen. Without them the seat still starts but cannot be used (yellow), and the
  manager shows a status for each missing type.

### 2.5 How input reaches only its seat (reference mechanism)

The reference product does this in the kernel. The summary below is for understanding
only. Pulsar Seat must **not** copy it (see section 4).

- **Filter position.** One kernel driver is added to the class `UpperFilters` of four
  classes: Keyboard `{4D36E96B-…}`, Mouse `{4D36E96F-…}`, HIDClass `{745A17A0-…}` and Display
  `{4D36E968-…}`. Display is covered in `40-display.md`.
  - The entry is added when the product is enabled and removed when it is disabled. On the
    test PC with the product disabled, only the Windows defaults (kbdclass, mouclass) and
    other software's filters were present.
  - Where it sits in the list relative to kbdclass and mouclass is not verified.
    **HYPOTHESIS**: it is listed after the class driver, which places it above
    kbdclass/mouclass. From there it sees the open and read requests that each session's
    window manager sends to the keyboard and mouse class devices.
  - The driver also reads the class keys of USB, Ports, MEDIA, Image, Camera,
    XnaComposite (Xbox 360 pads), XboxComposite (Xbox One pads) and Miracast.
  - **HYPOTHESIS**: it keeps a saved copy of the original filter lists, so that disabling
    restores them. Evidence: one string naming such a saved list. Not observed.
- **Install style.** The driver is a legacy kernel service (service type 1, kernel driver)
  plus one root-enumerated control devnode whose class is the LegacyDriver class. No INF
  for it exists in the system INF folder or the driver store, and setupapi logs show no
  device install for it.
- **Routing.**
  - The driver tags each unit's devnode with a seat mask (see 2.2, partly HYPOTHESIS).
  - It works out which session is making a request (it imports the session-id and
    requestor-process routines).
  - **HYPOTHESIS**: it fails or holds open and read requests that come from sessions that
    do not own the unit, so each session's window manager gets input only from its own
    units.
  - **HYPOTHESIS**: the driver also changes kernel-wide console state for each session and
    maps memory into user processes. Evidence: a routine name for setting the active
    console id that the driver resolves at run time, and imports for allocating virtual
    memory in another process, attaching to a process's address space, and image-load and
    process-create notifications. The behaviour itself was not observed. Either way, both
    are out of bounds for Pulsar Seat.
- **Propagation** (`propFilters`, DWORD under the driver's service key, 1 on the test PC).
  **HYPOTHESIS**: when set, the filter, and with it the seat, carries down to child
  devnodes. For example, every device plugged into a USB hub assigned to a seat goes to
  that seat.
- **HID usage filter.**
  - HID collections are matched by their hardware ID of the form
    `HID_DEVICE_UP:pppp_U:uuuu`.
  - The multi-string value `UnfilteredIDs` lists usage IDs that are never filtered and stay
    visible to every seat. Its built-in default is the Lighting and Illumination page
    (0x59, usage 0x01 = lamp array). Two vendor-defined usages (page 0xFFA0) are
    special-cased as well.
  - The unfiltered list was added in a later release.
- **System and consumer buttons.**
  - Since an older release, the media keys and the system-control collections of
    keyboards and mice on extra seats no longer act on seat 1 (changelog).
  - **HYPOTHESIS**: power, sleep and wake buttons on keyboards (system control 0x01/0x80)
    are blocked by default. Evidence: a shipped settings patch sets
    `DisableSysButton = 0` under the driver's service key to allow them. The default
    behaviour without the value was not observed.
  - **HYPOTHESIS**: the service reads those button usages through raw input and HID usage
    lists, and handles them for each session.
- **Xbox controllers.**
  - The driver knows the Xbox GIP device and has an option `killXboxGIP`. **HYPOTHESIS**:
    the option blocks the system's shared Xbox One protocol device, so that pads are only
    reachable through their per-seat HID collections.
  - Xbox 360 and Xbox One pad classes are referenced. Xbox pad support was added over time.
- **Other input paths (known, not specified).** The driver carries settings and names
  that point to paths this spec does not cover:
  - `PORTSfilters` and the Ports class key: a filter list for the Ports class, which
    covers serial and PS/2 / i8042 controller paths (**HYPOTHESIS**);
  - `oldFashionLLP`: **HYPOTHESIS**, a legacy mode built on low-level input hooks;
  - `usdRefreshMsec`: an interval in milliseconds; purpose unknown.

  Pulsar Seat deliberately does not reproduce these. PS/2 / i8042 keyboards and mice are
  covered by Pulsar's own device filter (4.1.2), because it sits on the same stack
  position for i8042prt as for kbdhid/mouhid.
- **App helper DLL (32- and 64-bit).** It does **not** hook or inject anything. It exports
  three query functions:
  - the maximum number of seats;
  - the seat of a process id;
  - the seat of a session id.

  It answers them through the driver's control device. Apps can use it to become
  seat-aware (see `80-apps-compat.md`).
- **Control interface.**
  - Two control devices. **HYPOTHESIS**: the second one is for recovery or a safe mode;
    only its name suggests this.
  - A separate small recovery driver and service exist in the product package. They
    belong to `20-service.md`.
  - All control codes use the generic device type with buffered I/O and any-access.
  - The control library uses about ten function codes and the service uses two. Their
    meaning was not resolved. **HYPOTHESIS**: set a unit's seat mask, read unit activity,
    read the kernel log, map a process or session to its seat, set the hotkey.
  - Pulsar Seat defines its own interface, so the codes are not needed.
- **Coexistence seen on the test PC.** Other software's filters were present on the same
  stacks while the product was disabled: a HID-hiding filter on the HIDClass, XnaComposite
  and XboxComposite class `UpperFilters`, and a third-party keyboard/mouse device upper
  filter on the kbdhid/mouhid devnodes (with its own child devnodes). How the reference
  product behaves with them while enabled is not verified.

### 2.6 Hot-plug

- The driver registers for device-interface arrival and removal, and for container
  (session) change notifications. The manager registers for device-change window
  messages.
- A newly plugged unit is shown at once as "just connected".
  - If its port or location matches a saved entry, it gets that entry's seat (2.2).
  - **HYPOTHESIS**: if it matches nothing, it goes to seat 1 / the pool until the user
    assigns it. If its hub is assigned to a seat and propagation is on, it goes to the
    hub's seat.
- Settings changed in the manager no longer need a restart of the manager itself
  (changelog). **HYPOTHESIS**: a changed seat assignment applies after Apply without a
  reboot. Not verified, because the product is disabled on the test PC. Enabling or
  disabling the whole product needs a reboot.

### 2.7 Hotkey: move keyboard and mouse to another seat

- One system-wide hotkey of the form **Ctrl + one key**. It moves the keyboard and mouse
  from one seat to another.
  - **HYPOTHESIS**: it moves the keyboard that pressed it, and the mouse paired with it,
    to the next seat in turn. It is a rescue tool when a seat has no working input.
- **Setting it.**
  - The user opens the dialog from the system block's context menu and presses a key while
    the edit field is active.
  - Reset disables the hotkey.
  - The dialog warns that the hotkey is system-wide.
  - A failed write shows an error with a numeric code.
- **Storage.** DWORD `InputDirectionSwitchKey` under the driver's service key. The test PC
  holds 0x58. **HYPOTHESIS**: 0x58 is the keyboard scan code of F12 (set 1), so the
  hotkey is Ctrl+F12. The kernel filter sees scan codes, not virtual keys. If it were a
  virtual key, 0x58 would be "X".
- **Where it runs.** The value is read by the kernel filter. **HYPOTHESIS**: so the hotkey
  works on every seat, also at the logon screen. Not observed.
- **Other software keystrokes.** The service can send the Win+Ctrl+Shift+B shortcut
  (graphics driver reset) into the input desktop of a session. It does this with the
  standard input-injection API after opening the input desktop and switching its thread to
  it.

### 2.8 Ctrl+Alt+Del and the logon screen per seat

- Each seat has its own session, its own logon screen and its own credential provider (see
  `30-sessions.md`). The extra seats' sessions are recorded by the session manager as
  local, not remote (`30-sessions.md`).
- No code or string in the input path refers to the secure attention sequence.
  **HYPOTHESIS**: Ctrl+Alt+Del pressed on a seat's keyboard is seen by the window manager
  of the session that owns the keyboard (because of the per-session routing in 2.5), and
  only that seat's logon or security screen reacts.
- On the test PC, the Winlogon value `DisableCad = 1` and the policy
  `SoftwareSASGeneration = 1` are set. It is not proven that the reference product set
  them. They may come from the user or from other software on the PC.

### 2.9 Special settings that affect input

These settings are applied from the manager's special-settings list. Each one needs a
reboot.

| Setting | Where | Effect |
| --- | --- | --- |
| `AAPThreshold = 0` | `HKLM` and `HKCU` `…\Windows\CurrentVersion\PrecisionTouchPad` | Turns off the precision touchpad's typing palm rejection. Without this, typing on *any* keyboard (also another seat's) briefly disables the touchpad of a laptop seat. Present on the test PC. |
| `DisableSysButton = 0` | driver service key | Lets keyboard power, sleep and wake buttons work. **HYPOTHESIS**: without it they are blocked (2.5). |
| `propFilters = 1` | driver service key | Seat propagation to child devices (**HYPOTHESIS**, see 2.5). |
| `UnfilteredIDs` | driver service key | HID usages that are never isolated. |
| `killXboxGIP` | driver service key | Xbox GIP handling (**HYPOTHESIS**). |
| `InputDirectionSwitchKey` | driver service key | The Ctrl+key seat-switch hotkey. |

### 2.10 Error cases seen

- A seat has no keyboard, mouse or HID unit: yellow status, and a status line for each
  missing type.
- Saving an assignment that conflicts (for example the same exclusive unit on two seats):
  the manager lists the conflicting devices and asks for confirmation. If the user
  cancels, nothing is saved.
- An assigned device is not found in the system: an error in the device dialog.
- Writing the hotkey fails: an error dialog with the code.
- A saved device is absent: shown as absent in the configuration compare.
- Not observed: what happens to input of extra-seat units while the service is stopped or
  crashed. **HYPOTHESIS**: with the product disabled the filter is not loaded, so all
  units act on the console.

## 3. ARM64 notes

- The reference input path is an **x64 kernel driver**. It cannot load on ARM64 Windows at
  all, because kernel code is never emulated. Nothing of the reference input isolation
  works on ARM64.
- The user-mode parts (manager, control library, service, helper DLL) are x64 or x86 and
  would only run under emulation. A service and a credential provider loaded by system
  processes must be native ARM64. An x64 credential provider DLL cannot load into the
  ARM64 LogonUI process.
- All the Windows mechanisms that Pulsar Seat needs exist on ARM64:
  - KMDF and UMDF;
  - device filters through INF, including extension INFs;
  - raw input;
  - the HID APIs;
  - device notifications (`CM_Register_Notification`);
  - the remote-protocol stack and its input channels (keyboard, mouse, multitouch and pen);
  - SendInput and synthetic pointer injection (only for the conditional model in 4.1.4).
- Typical ARM64 laptop hardware:
  - Keyboard, touchpad, touch screen and pen on I2C/SPI HID, enumerated through ACPI.
    These show up as non-USB units (`PS2_ConnectedMouse` / `Assorted`), so a USB port
    chain does not exist.
  - Touch and pen usually come from **one** I2C device as sibling collections. This is
    exactly the case where a location-path-only key collides (2.2). Pulsar's key must
    include a collection discriminator (4.1.1).
  - Precision touchpads and pens are common, so the palm-rejection setting in 2.9 matters
    on ARM64 laptops.
  - Some platforms expose keyboard and touchpad through a vendor HID-over-SPI or
    HID-over-I2C transport with a UMDF lower driver. A filter must not assume a USB
    parent.
- Every Pulsar driver binary must be built for ARM64 (machine 0xAA64) and x64, with the
  same INF. The installer must check the PE machine type.

## 4. Pulsar Seat approach

Constraints (`docs/agents/rules.md`):

- no vulnerable drivers, no kernel patching, no injection into user processes;
- must work with Memory Integrity (HVCI) and Secure Boot on;
- prefer user mode and UMDF;
- native ARM64.

Session model: `30-sessions.md` recommends that every extra seat is a remote-protocol
session (option A), shown full-screen on the seat's monitors by a Pulsar seat host. On
client SKUs Windows offers no other documented kind of extra interactive session, so this
spec designs for that model. 4.1.4 notes what would change under another model.

### 4.1 Building blocks

1. **Seat model and assignment unit.** The same unit as the reference: one device
   interface, which is a keyboard devnode, a mouse devnode or a HID top-level collection.
   - The service groups a physical device's units in the UI (by ContainerID / parent),
     but each unit can still be assigned on its own.
   - **Unit location key.** Each unit is identified by a key with these parts:
     - the location path of the nearest ancestor that has one (a collection devnode has
       none of its own);
     - the interface class;
     - the USB interface number, when there is one;
     - a **collection discriminator**: the collection number from the collection's
       hardware ID, plus its HID usage page and usage. Keyboard and mouse units that are
       HID collections get it too.
   - For ACPI/I2C/SPI units, the location path is the ACPI path.
   - For Bluetooth units (no location path), the key uses the device's Bluetooth address
     instead of the location path, plus the discriminator.
   - For root-enumerated units, the key uses the device instance path. Such units stay
     on seat 1 by default (they are virtual devices, for example from other software).
   - Two live units must never share a key. If they still do (unusual firmware), the
     service assigns both the same seat, keeps one configuration entry, and the manager
     shows a warning that they cannot be split.
   - The USB port chain is only a readable label.
   - Assignments live in Pulsar's own configuration (owned by `20-service.md`). They are
     mirrored as a private device property (Pulsar's own DEVPROPKEY) on the devnode, so
     the filter can read the seat at device start without asking the service.
2. **Input isolation for keyboards and mice: a KMDF device filter in the documented
   position.**
   - **Position.** A KMDF upper filter **below** kbdclass / mouclass: above kbdhid/mouhid
     or i8042prt. This is the standard position of the WDK keyboard and mouse filter
     samples. Device upper filters load below class upper filters, so a device filter is
     always below kbdclass/mouclass.
   - **Data path.** The filter hooks the documented class-service callback (the internal
     connect request) and sees every input packet of its device.
   - **Seat 1 (console) units.** Packets pass through unchanged.
   - **Units of other seats.** The filter takes packets out of the console's stream and
     hands them to the Pulsar service through an inverted-call request on the filter's own
     control device. The service sends them to the seat host of that seat (4.1.4).
   - **Installation.** The filter is installed through the driver store, never as a legacy
     service:
     - preferred: an extension INF that adds the filter to keyboard and mouse devnodes
       with the documented filter-add directive (position upper, with a filter level), so
       Windows orders it deterministically;
     - otherwise: a device `UpperFilters` entry appended by INF.
     - A class-wide entry is not used for keyboards and mice. kbdclass/mouclass and other
       entries in the class list stay untouched.
   - **Signing.** The filter needs Microsoft signing to load with Secure Boot on (a known
     project risk, see `docs/agents/plan.md`). The reference product's later package ships
     a Microsoft-signed build of its own class filter for Secure-Boot-on PCs, so such a
     signature is obtainable for this kind of driver. A test-signed build is for
     development only.
   - **No kernel patching.** The filter does not touch win32k, the console id, or other
     sessions' memory.
3. **HID collections (touch, pen, gamepads, consumer and system control): visibility
   filter.**
   - A KMDF upper filter on HID collection devnodes, matched by an extension INF on HID
     hardware IDs (it may also be a class filter appended after existing HIDClass
     entries).
   - It decides on **create** only, using the requestor's session id (a documented request
     property). It never modifies data and never holds reads. Rules, in order:
     1. Unit is on seat 1, shared, or in the unfiltered usage list (default: lamp arrays,
        page 0x59): allow every open.
     2. Requestor is in session 0 (services, including LocalSystem services such as the
        HID service, the GameInput service, audio and Bluetooth services, and the Pulsar
        service): allow. Session 0 has no seat; these services are shared by design.
     3. Requestor is the session of the seat that owns the unit: allow.
     4. Otherwise deny with access-denied.
   - Consequence for touch and pen of an extra seat: the console session is denied, so the
     console window manager does not consume them. The Pulsar service opens the collection
     from session 0 (rule 2), reads the reports, and sends them to the seat host (4.1.4).
   - Consequence for gamepads: an app in the seat's remote-protocol session can open the
     pad (rule 3). The pad is invisible to the other seats' sessions.
   - An open that was allowed stays valid when the assignment changes. On reassignment the
     service asks the filter to close handles opened by sessions that lost the unit, or
     tells the user to replug. **Open question** which.
   - **Open question**: can a UMDF 2 filter do this for HID collections whose stack is
     kernel mode? Probably not. Keep this filter KMDF, small and rule-based.
4. **Delivering input to a non-console seat's session.**
   - **Remote-protocol seats (the chosen model).** The seat host for seat *n* runs in the
     console session and owns the remote-protocol connection to seat *n*'s session. It
     receives seat *n*'s packets from the service over an ACL'd named pipe and feeds them
     into the connection's input channels:
     - keyboard as scan codes, mouse as relative or absolute pointer events;
     - touch and pen through the protocol's multitouch / pen input extension, mapped to
       the seat's monitor rectangle (see `40-display.md`);
     - consumer keys (volume, media) as keyboard input in the session.
     The session's window manager treats them as normal input. The seat host must use an
     RDP client implementation that exposes an input API (for example an open-source
     client library under a GPL-3-compatible licence).
   - The seat host window must never take input from the console's own stream. Its input
     comes only from the pipe. Keyboard focus on the console desktop is irrelevant.
   - **Conditional: another session model.** If `30-sessions.md` ever settles on a
     session that has no remote input channel, input must be injected inside that session.
     A UIAccess agent is not enough there: UIAccess cannot reach the Winlogon secure
     desktop. The injector must be a small SYSTEM-token helper started by the service in
     that session. It opens the current input desktop, switches its thread to it (also
     Winlogon's desktop) and uses SendInput (scan codes) and synthetic pointer injection.
     This is the same documented pattern the reference service uses for its shortcut
     (2.7). Without a decision in `30-sessions.md`, Pulsar does not build this path.
   - **Gamepads.** No delivery is needed. The visibility filter makes each pad visible
     only to its seat's session.
     - XInput pads (Xbox 360/One through xusb/xinputhid) and the system GameInput service
       open devices from a shared service process in session 0. Per-seat isolation of
       those is an **open question**.
     - Remote-protocol sessions may not see local XInput pads at all. **HYPOTHESIS**, to
       check in Phase 4.
     - Plan: v1 isolates HID pads (DualSense, generic HID). XInput / GIP pads are marked
       "shared" or "experimental".
5. **Service not running or crashed.** The filters must behave as follows:
   - **Before the service has connected once since boot:** every unit acts on the console
     (pass-through, no HID denial). The PC stays usable at seat 1 even if the service
     never starts.
   - **After the service has connected, then disconnects** (crash, stop):
     - seat 1 units: unchanged;
     - keyboard/mouse units of other seats: the filter keeps a small bounded buffer per
       unit for a short gap (for example 1 s or 256 packets, tunable) to cover a service
       restart. When the buffer is full, or no inverted-call request is pending past the
       gap, packets are **dropped**. They never fall back to the console, because another
       person's typing must not reach seat 1's session;
     - HID units of other seats: the create rules stay in force;
     - the service's restart policy (`20-service.md`) restores delivery. On reconnect the
       buffered packets are delivered only if they are younger than the gap; key-up
       events are synthesised for keys that were down, so no key stays stuck.
   - **Recovery:** booting into Safe Mode, or a Pulsar recovery value set by the
     uninstaller or by a recovery step in `20-service.md`, makes the filters pass
     everything to the console.
   - The manager shows a clear status when the service is down and extra seats have no
     input.
6. **Coexistence with other filters.**
   - Other upper and lower filters may be on the same devnodes and classes (seen: a
     HID-hiding class filter, a third-party keyboard/mouse device filter).
   - The installer never removes, reorders or rewrites other entries in any
     `UpperFilters` / `LowerFilters` list, and the uninstaller removes only Pulsar's own
     entry.
   - Pulsar's keyboard/mouse filter must work at any place among other device upper
     filters below kbdclass/mouclass. A filter above it may still see seat packets only if
     it sits between Pulsar and the class driver; Pulsar removes packets before they move
     up, so those filters simply see fewer packets.
   - Error case: if another driver hides a HID collection from the Pulsar service (its
     open fails from session 0), the unit is reported as hidden by other software, and the
     manager shows it as not routable.
   - Error case: if another keyboard/mouse filter below Pulsar consumes or rewrites
     packets, Pulsar cannot see them. The service detects foreign filters in a unit's stack
     and the manager shows a warning on that unit.
7. **Hot-plug.**
   - The service listens with `CM_Register_Notification` for keyboard, mouse and HID
     interface arrival and removal, and for USB hub changes.
   - When a unit arrives, the service matches its location key to the configuration:
     - a known key gets its saved seat;
     - an unknown unit inherits the seat of the nearest assigned USB hub above it (the
       equivalent of `propFilters`);
     - anything else goes to seat 1 (console), so a new keyboard always works somewhere.
   - The service writes the seat property before the filter starts the device, or it
     updates a filter that is already running through its control device.
   - Every arrival is reported to the manager, which highlights the device for a few
     seconds.
   - A changed assignment applies at once, without a reboot or a replug, except for open
     HID handles (4.1.3).
8. **Identification flows.** The same flows as 2.3, built on Pulsar's own pieces:
   - Activity is reported per unit from the filter path (packet counters) or from raw
     input in the console session.
   - The "press F*n* on your seat's keyboard" wizard works on all seats at once, because
     the service sees the units of every seat before routing.
   - While the wizard runs, unassigned units are delivered to the service only, and not to
     any session.
9. **Hotkeys.**
   - A Ctrl+key (configurable, default Ctrl+F12, stored as a scan code) is detected in the
     service from packets of **any** extra seat's keyboard. For seat 1 units the filter
     reports the combination to the service as well.
   - The hotkey moves the keyboard that pressed it, together with the mouse units of the
     same physical device or seat, to the next seat. The change applies at once and is
     saved only after the user confirms it in the manager.
   - The hotkey can be turned off.
   - A second, fixed emergency combination, for example pressed three times in a row,
     returns all units to seat 1. **Open question**: which combination. It needs the
     service; with the service down, rule 4.1.5 applies.
10. **System and consumer buttons.**
    - Power, sleep and wake (0x01/0x80) from units not on seat 1 are dropped by default.
      There is an option to allow them, but they always act on the whole PC.
    - Consumer keys (volume, media) are delivered to the owning seat's session like
      keyboard input. They must never change the console's volume.
11. **Ctrl+Alt+Del per seat.**
    - Seat 1 uses the normal Windows path.
    - Remote-protocol seats: the seat host translates Ctrl+Alt+Del from the seat's
      keyboard into the protocol's own secure-attention request, so that session's
      Winlogon handles it. No system policy change is needed.
    - Only under the conditional model in 4.1.4: the service would call the documented
      SendSAS function. That needs the `SoftwareSASGeneration` policy (under
      `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`) set to allow
      services, which is value 1 (2 allows Ease of Access apps, 3 allows both). Pulsar
      would set exactly 1, only if the previous value is lower, tell the user, and restore
      the previous value (or delete it if it was absent) on uninstall. Whether SendSAS from
      session 0 reaches a chosen non-console session is an **open question**.
12. **Touchpad palm rejection.** The manager offers the `AAPThreshold` tweak (2.9) as a
    clearly labelled option for laptop seats. It is never applied silently.
13. **App seat API (optional).** Pulsar may offer a small documented query interface, a
    pipe or a DLL, for "seat of this process / session". It is not part of v1 and it
    never hooks other apps.

### 4.2 Things Pulsar Seat will not do

- Kernel-mode injection into user processes, changing the active console id, or patching
  win32k or dxgkrnl behaviour.
- A legacy-service driver without an INF, or test-signing on users' PCs.
- Global low-level keyboard or mouse hooks to steal input from the console session.
  They are unreliable and cannot reach the secure desktop.
- Ports-class filters or legacy low-level-hook modes (2.5).
- **If the kernel filters cannot be signed** (open question 9): the fallback is to drop
  local input isolation, never test-signing and never Memory Integrity off. Extra seats
  then exist only as remote-protocol sessions with no local-input isolation (for example
  driven by remote clients, as in Pulsar Split), and the manager says that local
  keyboards and mice cannot be split between seats.

### 4.3 Configuration Pulsar Seat needs (names are ours; details in `20-service.md`)

| Item | Type | Meaning |
| --- | --- | --- |
| unit location key | string | stable identity: ancestor location path (or Bluetooth address / instance path) + interface class + USB interface number + collection number and usage page/usage (4.1.1) |
| unit seat | seat mask | owning seat(s); 0 = pool |
| unit shareable | bool | HID units only; keyboards and mice are always exclusive |
| hub seat | seat mask per USB hub location | inheritance for new devices |
| switch hotkey | scan code + modifiers, or off | 4.1.9 |
| allow system buttons | bool, default off | 4.1.10 |
| unfiltered HID usages | list of page/usage | default: lamp arrays |
| service-gap buffer | duration + packet count | 4.1.5 |
| inserted-device highlight seconds | int, default 4 | manager |

## 5. Open questions

1. Where does the reference filter sit relative to kbdclass and mouclass, and does it fail,
   hold or drop requests from non-owning sessions? (Needs a kernel debugger or static
   analysis of the driver. Not needed for Pulsar Seat's own design.)
2. What does the reference's per-devnode seat value in the driver key mean (marker or
   mask), does the product reset it when disabled, and do the run-time seat properties
   (2.2) exist?
3. Is `InputDirectionSwitchKey = 0x58` a scan code (Ctrl+F12) or a virtual key (Ctrl+X)?
   Which keyboard and mouse does the hotkey move, and to which seat? Does it work at the
   logon screen? (Check with the product enabled. That is a system change on the test PC,
   so the maintainer's go is needed.)
4. Which device types are exclusive and which are shareable in the reference product?
5. What does the reference product do with an unknown new unit, and what exactly does
   `propFilters` do? Does a seat assignment apply without a reboot?
6. Does the reference product handle Ctrl+Alt+Del itself, or does it rely on `DisableCad`?
   Were `DisableCad` and `SoftwareSASGeneration` set by it or by the user?
7. How does the reference product resolve two entries with the same `CanonicalName2`
   (touch + pen, or two pad collections), and how does it key Bluetooth HID devices?
8. For Pulsar Seat: per-seat isolation of XInput / Xbox GIP / GameInput pads, which are
   opened by shared system processes. Do remote-protocol sessions see local XInput pads?
9. For Pulsar Seat: will Microsoft signing (attestation or HLK) of a keyboard/mouse device
   filter and a HID collection filter be granted? If not, the fallback in 4.2 applies.
10. Touch and pen on remote-protocol seats: does the protocol's multitouch / pen extension
    give full pen fidelity (pressure, tilt, eraser, hover) and enough touch contacts?
11. On reassignment of a HID unit: can the filter force-close handles of sessions that
    lost it, or is a replug needed?
12. Only for the conditional model (4.1.4): can SendSAS from session 0 target a chosen
    non-console session?
