# 10 — Manager app and setup wizard

Status: draft v1 (2026-11-12, critiqued, cross-checked). Reference: the reference product, versions 2.70.3
(installed on the test PC) and 2.70.6 (installer, read without running it).
Scope: the desktop manager (the GUI the administrator uses) and the first-run setup wizard.
It covers every screen, its fields and defaults, the validation rules, the flows, and what
each action changes. The service, the sessions, display, input, audio and USB internals are
covered in `20-` to `80-`. This file only says which of their settings the manager edits.

Terms used in this file:

- **Seat**: one workplace (monitor(s) plus keyboard, mouse and other devices) used by one
  person. Seat 1 is the console seat and always exists.
- **Seat mask**: a bitmask with one bit per seat. Bit 0 is seat 1. A device whose mask has
  every bit set is *shared* by all seats. A mask of 0 means *spare* (no seat uses it).
- **Surface**: a display source of a graphics adapter (one desktop rectangle, `\\.\DISPLAYn`).
  **Output**: a physical connector/target with a monitor behind it. A surface is labelled
  `A.S` (adapter number, source number), for example `1.2`.
- **Enabled / disabled**: the product's services and driver are set to start at boot
  (enabled) or are turned off (disabled). Enabling or disabling needs a reboot.
- **Started**: the extra seats are running (their sessions exist). This can only happen
  while the product is enabled.
- **Seat limit**: the highest seat number the product supports: 12 in 2.70.3; 16 in 2.70.6
  is a **HYPOTHESIS** (`30-sessions.md` §2).

Names of tabs, menus, buttons and dialogs in this file are **our working names**. They
describe the function. They are not the reference product's labels, and the Turkish UI
copy is written separately.

Items marked **HYPOTHESIS** come from string or naming evidence only. They are not
confirmed by watching the running product.

---

## 1. Components and processes

| Component | Kind | Role |
| --- | --- | --- |
| Manager | GUI app (Qt 6 widgets, x64, packed binary) | Main window, tray icon, every settings dialog, the full-screen setup wizard UI |
| Control library | DLL loaded by the manager and the console wizard | Device enumeration, device identification, apply settings, service and driver install/config, diagnostic report. It talks to the kernel driver through control devices (section 11). |
| Console wizard | Console x64 exe | First-run "gatherer" and an automatic basic layout (section 9.1) |
| Branding DLL | Resource DLL | Product name, publisher, website, a few device images |
| Licence component | separate exe/DLLs | It exists. It is out of scope and not described. |

Launch and lifetime:

- The installer adds a machine-wide Run entry (value named after the manager executable, verified live) that
  starts the manager with the `-autostart` argument at every logon. It also adds a
  Start-menu folder (the manager, a link to the online user guide, the EULA, the licence
  tool) and a public desktop shortcut to the manager, with no arguments.
- Only one instance can run per session. A second launch shows a message that points the
  user to the running instance in the taskbar or tray, then exits.
- On start the manager refuses to run when:
  - a 32-bit build is started on a 64-bit OS;
  - the user forced compatibility mode on the exe;
  - the OS build is not in the product's supported range. The manager then offers to search
    online for a build that supports this OS.
  - the admin-only policy is on and the user is not an administrator (section 6.1).
- Closing the main window hides it to the notification area (since 2.70.4) and shows a
  notice that the program keeps running there. HYPOTHESIS: the form of the notice (dialog
  or tray balloon) and whether it appears only once are not known. Only the tray menu's
  quit item really ends the process.
- The manager runs in **every** session, on every seat. The title bar shows the version,
  the seat the manager runs on and the product state.
- Actions that need administrator rights (from the product's messages): saving some
  settings, running the setup wizard, the diagnostic report level that adds minidumps,
  one-time upgrade work after a version update, enable/disable, reset. When the user is
  not elevated, the manager asks for elevation (UAC) for that operation, or tells the user
  to restart it as administrator.
- No evidence was found of actions that are refused only because the manager is not on
  seat 1. The live logs show the manager running on seat 2 in both launch kinds seen there
  (see below). HYPOTHESIS: there are no seat-1-only actions; open question 12.
- Logs: one log file per manager run goes to `%ProgramData%\<product data dir>\logs\`. The
  file name has the date and time, a running number, `wp` plus the seat number (2 digits)
  and a one-letter launch kind. Three letters were seen live: `s`, `u` and `a`, on seat 1
  and seat 2. A `u` log can come from an administrator (its first entry records admin
  state = admin), so `u` does not mean "non-admin". The `a` logs hold only a control-library
  start-up failure. HYPOTHESIS: `a` is a short-lived auxiliary launch (for example an
  elevated helper copy). The meaning of each letter is not known. Each entry has a running
  index, a timestamp, a level (error, info, debug, trace) and a message. A dialog sets the
  verbosity (section 7.10).

## 2. Main window

### 2.1 Layout

- **Title bar**: program name, version, the seat the manager runs on, product state
  (enabled or disabled; started or not started).
- **Header link buttons**: check for updates, online licence check (out of scope),
  contact the developer, user manual (opens a web page), product website. Each button has a
  tooltip. The update and website buttons stay disabled until a network reachability check
  succeeds.
- **Tabs** (working names, by function):
  1. **Seats**: the device view (section 3). This is the default tab.
  2. **Basic settings**: the seat table, start mode, appearance and behaviour (section 4).
  3. **Advanced settings** (section 5). It carries a warning banner that the tab is for
     experienced users only.
  4. **Per-seat network addresses** (section 7.4): a table with one row per seat. Clicking
     a row opens the per-seat IP binding dialog.
  5. Licence management (out of scope).
- **Bottom button bar** (by function):
  - **Save**: enabled only while some changes are not yet saved. Its tooltip says that
    unsaved changes are lost when the manager restarts and that some settings need admin
    rights.
  - **Enable or disable, then restart**: one toggle button. Its label and its action
    follow the current service state. It changes the product's boot state and then asks
    for a reboot (section 8.3).
  - **Start seats**: starts the extra seats now (section 8.4). It is disabled while the
    product is disabled.
  - **Reboot**: enabled when a saved change needs a reboot to take effect.
  - **Support**: builds a diagnostic report (section 7.12).
  - **Minimise to taskbar** (2.70.4) and **close**. Close hides the window to the tray.
- A keyboard shortcut (Ctrl+Shift+S) exists. HYPOTHESIS: it saves the configuration.

### 2.2 Tray icon (since 2.70.4)

- The tray menu has: show the window, start seats, enable/disable services, quit.
- Double-clicking the tray icon shows the window. HYPOTHESIS: standard behaviour, not seen.
- A setting hides the tray icon (section 4.4). The process then has no visible UI while the
  window is hidden. The manager can still be reopened from its shortcut: the second-instance
  rule brings the running instance up.
- Quitting asks for confirmation that the process should really end and not only go to the
  tray.

### 2.3 Global messages shown by the main window

These are the conditions the manager detects and reports. The wording is ours to write.

| Condition | Behaviour |
| --- | --- |
| Settings changed and not saved, then window closed or tab left | Ask: save, discard or cancel |
| Enable/disable state changed | Explain that a reboot is needed; offer reboot now or later. If the automatic reboot fails, tell the user to reboot by hand. |
| Product configured but disabled | Offer to enable it and start seats now |
| No display assigned to seat 1 | Saving is refused. Seat 1 must have a display. |
| User tried to change resolution or add monitors while enabled | Explain the steps: disable the product, extend the desktop to all monitors, set the resolutions, then enable again |
| Devices sit on seats above the seat limit | They go to the spare pool and the user is told to save |
| Seat 1 is not using monitor `1.1` (2.70.6) | Recommend giving the first monitor of the first adapter to seat 1, for stability |
| Run entry missing but the start mode needs it (2.70.6) | Warn that the chosen automatic start mode will not work |
| Program files missing or damaged (2.70.6) | Tell the user to reinstall |
| Windows security policy conflict, error `2(31)` (2.70.6) | The product says that an incompatible Windows *security* policy is active and tells the user to turn it off, with a link to instructions. HYPOTHESIS: the policy is Memory Integrity (HVCI), the vulnerable-driver blocklist or a similar code-integrity policy. Evidence: the control library reads the HypervisorEnforcedCodeIntegrity and Host-Guardian DeviceGuard scenario keys, and the diagnostic report records the HVCI and Secure Boot state. See `20-service.md` and `90-install.md`. **Pulsar Seat never shows this kind of guidance** (see Pulsar Seat approach). |
| Windows build changed while enabled (feature update) | Tell the user to disable, reboot, check the seats, then enable again |
| Windows itself booted in Safe Mode | The product does not run. Tell the user to reboot normally and try again, and to contact support if that does not help. |
| The product's own safe fallback after an unclean shutdown | The previous Windows session ended abnormally (crash, power loss). The product starts in its own safe mode (no extra seats) to avoid a crash loop. Tell the user to fix the cause and reboot; the product then tries a normal start. A tweak (section 7.8) turns this check off; see `20-service.md` (`ignoreLastShutdown`, `ignoreLastBoot`). |
| The service cannot start (often antivirus) | Show an error naming the seat service |
| Start or relogin of a seat failed | Show an error dialog with an error class and code, and say that the services must be enabled |
| Network unreachable | HYPOTHESIS: the network-dependent buttons (update, website) stay disabled, and the user is told to restart the manager once the network works. A message of this kind and a "network check in progress" message exist in 2.70.6, but which features use them is not known. |

Error codes are shown as `class(code)`, for example `2(-5)` or `2(31)`. The class names the
subsystem. HYPOTHESIS: 1 = driver load, 2 = core/driver state, 4 = timing.

## 3. Seats tab (device view)

This is the main configuration surface. It is a canvas of **device containers**:

- **System container**: drawn as a PC case. It holds the *spare and shared* devices. Its
  context menu holds the global commands (section 3.7).
- **One container per seat**: seat number and name. It shows the devices assigned to that
  seat. Visible seats are filtered by a bitmask UI preference (section 4.5). The containers
  are laid out in rows. An option forces even rows. Each seat container has its own menu
  (section 3.6).

### 3.1 Device categories and pictures

Every device is drawn as a picture of its category. Categories and the device-interface
class each one is enumerated by:

| Category | Interface class GUID (interop) | Shareable |
| --- | --- | --- |
| Display (adapter surface) | `{5b45201d-f2f2-4f3b-85bb-30ff1f953599}` (display adapter) | No: exclusive to one seat |
| Monitor | `{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}` | Follows its surface |
| Keyboard | `{884b96c3-56ef-11d1-bc8c-00a0c91405dd}` | No (HYPOTHESIS) |
| Mouse | `{378de44c-56ef-11d1-bc8c-00a0c91405dd}` | No (HYPOTHESIS) |
| Other HID (touchpad, touchscreen, pen, gamepad, consumer keys) | `{4d1e55b2-f16f-11cf-88cb-001111000030}` | HYPOTHESIS: some usages yes |
| Speakers (audio render endpoint), drawn as a left/right pair | `{b5b6dcec-79e1-483e-a46d-1e1f85eedf2f}` (product-own type id, see `60-audio.md`) | Yes |
| Microphone (audio capture endpoint) | `{f3f459b9-597a-4f0a-9e46-71642eff8048}` (product-own type id) | Yes |
| Webcam | `{eaca086a-320a-4b0e-850f-6ed61be1b612}` (HYPOTHESIS: camera category) | Yes |
| Smart-card reader | `{50dd5230-ba8a-11d1-bf5d-0000f805f530}` | Yes (HYPOTHESIS) |
| USB hub | `{f18a0e88-c30c-11d0-8815-00a0c906bed8}` | Yes. Assigning a hub gives the seat everything plugged into its ports. |
| Network interface | (adapter list) | Shown for IP binding only |

Picture rules:

- A built-in default picture exists for each category. Users can add their own pictures
  as files in category folders under the install directory: monitor, keyboard, mouse, HID
  (optionally in sub-folders by HID usage page, for example `05` for digitizers), audio
  render, audio capture, video capture.
- Monitors and audio-render devices have an "in use" picture and a "not in use" picture
  (unplugged or switched off). They are paired by a file-name suffix. When the "not in use"
  file is missing, the default is used.
- Images are scaled to the cell size without keeping the aspect ratio.
- The reference product ships sample images: keyboards in several styles; mice wired and
  wireless in many colours; a touchpad and a trackpoint for HID; monitors including one
  with speakers, a curved one and a TV. Pulsar Seat draws its own (CLEANROOM).
- Per device, the user can choose a custom picture from the available set (section 7.9).
  A global command clears all custom pictures.

Overlay signs drawn on a device picture:

- **Activity**: a flashing dot and frame while the device sends input (mouse moved, key
  pressed). This is the main way to find a device: touch it and watch which picture
  flashes. The source is a per-device last-activity counter (`50-input.md`).
- **Just connected**: a coloured frame around a device that was just plugged in. It stays
  for N seconds (setting, 2 to 20 s in steps of 2, default 4). A re-plugged device also
  goes to the end of its category list, so unplug and re-plug is a second way to find it.
- **Unplugged**: a crossed-out connector. The device is in the configuration but absent.
- **Shared with all seats**: a small triangle of dots.
- **Attention**: a warning sign for a device with a problem (conflict or invalid
  placement).
- **USB hubs**: grey dots show the hub depth (0 = root hub, 1 = hub on a root hub, ...).
- **Surfaces** not connected to any output are drawn colourless.

Devices are listed in column-major order (top to bottom, then the next column).

### 3.2 Tooltips (what the user can learn about a device)

- Every device: category, friendly name, the seats it is assigned to (or that it is spare,
  or that all seats share it).
- USB device: port path (`USB-n`), manufacturer, product, serial number when known.
- USB hub: number of ports and what is connected to each port.
- Display (surface): its label `A.S`, the adapter name, the output type (internal, HDMI,
  DisplayPort, Miracast, indirect, ...) and the monitor name.
- Seat container: seat number, name, state, logged-on user, assigned IP address. A marker
  shows when an auto-logon password is stored.
- System container: an explanation of the spare/shared pool.
- Some adapter families get their own tooltip text: USB display adapters, a
  vendor-specific USB display/audio adapter, and USB video devices.

### 3.3 Seat status

Each seat container shows a colour status computed from its devices:

| Colour | Meaning |
| --- | --- |
| Grey | The seat will not start (disabled in the start list, section 4.1) |
| Red | Not operational: no display |
| Yellow | Will start but cannot be used: no keyboard or no mouse |
| Green | Operational: display, keyboard and mouse present |

There are also short status texts for the same states: no display, no mouse, no HID
device, all good, disconnected. When a seat is set to start but has no display, the start
is held back until a display is attached.

### 3.4 Assigning devices

Ways to assign:

1. **Drag and drop** a device picture from one container to another. Dropping on the
   System container makes the device spare.
2. The device menu item that opens the assignment dialog (section 7.1).
3. The device menu item that sends the device to the spare pool.
4. The setup wizard (section 9).

Rules and validation:

- **Exclusive** types (displays, keyboards, mice) belong to at most one seat. **Shareable**
  types (audio, webcam, card reader, USB hub) can have any seat mask. A one-click choice in
  the dialog sets the shared mask (all seats).
- A display (surface) moves together with its monitor(s). A seat may hold several
  surfaces (multi-monitor seat).
- **GPU rule**: a seat with several monitors must not share any graphics adapter with
  another seat. 2.70.6 states the rule per adapter. When an adapter's monitors are split
  across seats, each seat may hold only one monitor of that adapter. Otherwise all of
  that adapter's monitors must go to one seat. Breaking the rule marks the affected
  monitors as unavailable and blocks saving. The message suggests two fixes: dedicate a
  GPU to the multi-monitor seat, or merge its monitors into one large display with the GPU
  driver's display-spanning feature, so that the OS sees one display.
- Seat 1 must have a display (save is refused otherwise).
- On save, the control library checks for **conflicts**: devices whose assignment cannot
  be applied, for example a child of a hub given to another seat. The user sees the list
  of conflicting devices and may save anyway or cancel. A separate notice reports when a
  conflict was resolved.
- When the control library cannot move a device to the target seat, the manager shows a
  device-assignment error that names the device and the seat.
- Some assignments take effect only after a reboot (display assignments while started)
  (HYPOTHESIS: most input/audio changes apply live once saved; changelog 2.60.5 says that
  settings apply without restarting the GUI).

### 3.5 Device menu (per device)

Each item is described by what it does:

- Open the assignment dialog for this device (section 7.1).
- Send the device to the spare pool (seat mask 0).
- Start or stop **indication** of the device (section 3.5.1).
- Choose or change the device's custom picture (section 7.9).
- Show or hide device identifiers (hardware IDs) in the device label.
- Show the connection path: the port chain from the root hub and the parent hub.
- Show the sharing state: which seats use the device.
- Displays only:
  - open the advanced output-assignment dialog (section 7.5);
  - connect this surface to an output of its adapter;
  - switch between hardware and software mouse pointer for this surface;
  - pin a monitor to a specific output, so that it is re-bound to that output at start
    (the per-device form of the force-assign setting in section 7.5).

#### 3.5.1 Indicate device

Indication makes one device announce itself, so the user can tell which physical device a
picture stands for. It stays on until the user stops it. What each category does:

| Category | Indication | Evidence |
| --- | --- | --- |
| Monitor / surface | A large number drawn on that monitor in a shaped, top-most window. | Changelog fix for the position of monitor indications; the control library creates shaped windows. HYPOTHESIS for the exact content. |
| Speakers | A short built-in test sound played on that audio endpoint. | HYPOTHESIS: the control library opens a legacy wave-out device, matched to the endpoint, and plays a sound embedded in its resources (`60-audio.md`). |
| Webcam | A live preview window of that camera (since 2.51). Seat-level options show every camera's picture at once, or the video of the seat's own camera. | Changelog and menu entries. |
| Keyboard, mouse, other HID | HYPOTHESIS: no separate indication. The activity flash (3.1) does the job: the user touches the device. No keyboard-LED control was found in any binary. | absence of evidence |
| Microphone, card reader, USB hub | Unknown. | open question 13 |

A global command, HYPOTHESIS, indicates all connected devices at once (an entry with that
meaning exists).

### 3.6 Seat container menu (per seat)

- **Assign an IP address**: opens the per-seat IP binding dialog (section 7.4).
- **Rename the seat**: opens the seat name dialog (section 7.2).
- **Set the seat's sign-in account**: opens the auto-logon account dialog (section 7.11).
- **Force a new sign-in**: signs the seat's session out and starts a new logon on it
  (section 8.5).
- **Start this seat**: starts only this seat (section 8.4). It needs the product enabled.

### 3.7 System container menu (global commands)

Grouped by function (working names):

- CPU core assignment per seat (section 7.3).
- Compare the live devices with the saved configuration (section 3.8).
- Check for updates.
- Clear all custom device pictures.
- Shortcuts to Windows system panels: Device Manager, display settings, network
  connections, user accounts.
- Experimental driver options (section 7.7).
- The keyboard/mouse switch hotkey (section 7.6).
- Reset every product setting to defaults (section 8.6).
- Log verbosity (section 7.10).
- The support-only tweak list (section 7.8).
- Seats-tab view options (section 4.5).
- Licence items (out of scope).

### 3.8 Saved configuration and comparison (since 2.60.5)

- On save, the manager writes a **device-assignment snapshot** file into the product's
  ProgramData folder. The file and field names below are interop identifiers, given so that
  an import of an existing configuration is possible:
  - file `default_configuration.json`; top-level array `DeviceAsignment` (spelled this way);
  - per record:
    - `CanonicalName`: a short, stable position. It is the USB port chain such as
      `0-1-1-2-2` for USB devices, `A.S` for surfaces, or an ordinal for ACPI/PS/2 devices;
    - `CanonicalName2`: the full location path. A one-letter prefix says whether the device
      sits on a physical bus (`A`) or is root-enumerated, virtual (`B`). The path ends with
      the first 8 hex digits of the interface class GUID and the USB interface number
      (`USBMI(n)`), or a source index for surfaces;
    - `DeviceTypeGUID`: the category GUID from 3.1;
    - `InitialName`: the friendly name at save time;
    - `Type`: the connection type: USB-connected device, PS/2 mouse, display, or other
      (values `USB_ConnectedDevice`, `PS2_ConnectedMouse`, `Display`, `Assorted`);
    - `StationMask`: an object with `Initialized` (flag) and `Mask` (the seat mask as a
      binary string, seat 1 is the right-most digit; all ones = shared, `0` = spare).
- The snapshot keys devices by **physical position**, not by instance ID. A device
  replaced by an identical one in the same port keeps its seat.
- The comparison checks the live devices against the snapshot. The result is one of two
  states: everything matches, or the configuration changed since the last save. In the
  second case each device is marked: added after the save, assignment changed (with the old
  seat shown), or missing (unplugged since the save). The user can restore the saved
  assignment.
- 2.70.6: newly found monitors can be sent to the spare pool with one button.
- The same check runs at manager start when the hardware changed. The wizard refuses to run
  unless all devices are connected.

## 4. Basic settings tab

### 4.1 Seat table

One row per seat, up to the seat limit. Columns:

| Column | Content | Edit | Stored |
| --- | --- | --- | --- |
| Seat | number and name | name dialog (section 7.2) | per-seat machine key (HYPOTHESIS; default name = the seat number) |
| Account | auto-logon account, or the normal logon screen (default) | account dialog (section 7.11) | per-seat machine key (user, domain) plus an LSA secret for the password |
| State | checkbox: start this seat automatically | checkbox. Seat 1 is always on and cannot be cleared. | machine-wide start bitmask |

The product's help warns against auto-logon on seat 1: keep the normal logon screen there.

### 4.2 Start mode

A combo box with three values:

1. **Manual**: seats start only from the start button (or the tray item). This is the
   safest mode and is recommended at first.
2. **At system start**: seats start at boot. Every seat's monitor must be powered on at
   boot.
3. **When someone signs in on seat 1**: a delayed variant for PCs whose monitors are not
   ready at boot.

The value is a machine-wide DWORD (`AutoStartMode`; observed 1 = at system start). Modes 2
and 3 need the manager's Run entry (2.70.6 warns when it is missing). HYPOTHESIS: mode 3 is
triggered by the manager's `-autostart` launch in seat 1.

A UI preference controls whether a manual start asks for confirmation first, with an option
to stop asking.

### 4.3 Appearance

- **Language**: English, Spanish, French, Hindi, Indonesian, Korean, Polish, Portuguese,
  Russian, Simplified Chinese (one translation file each).
- **Widget style**: the Qt styles available (for example Fusion, Windows), or the
  product's own theme.
- **Colour scheme**: system default, light, dark, mixed, or a user scheme file.
- **Skins**: a combo that selects a custom skin. HYPOTHESIS: a skin is a style-sheet or
  image set that changes the look of the window. No skin files were found on the test PC
  or in the installer file lists; the storage key is not known.
- A flag that shows the product logo at start (UI preference).

### 4.4 Behaviour

- **Only administrators may run the manager**: a machine-wide flag. Both the machine key
  and the user key store a copy. When set, a non-admin user gets a message that the
  administrator restricted the program, and the manager exits (2.70.2).
- **Hide tray icon**.
- **Wallpaper**: give the extra seats the product's wallpaper as desktop background. One
  of about ten bundled images is used. The point is to show at a glance that a session runs
  on an extra seat.
- **Logon/lock-screen image**: replace the lock-screen image while the product is enabled.
  It is written through the Windows PersonalizationCSP lock-screen image path value. A
  failure shows an error (code 109 was seen in the logs).
- **Idle power saver**: install the product's screensaver for the seats. It turns the
  monitors off (or blanks them) when a seat is idle. Buttons open the system screen-saver
  and lock-screen settings pages. The help says the screensaver is enabled through the
  setup wizard.

### 4.5 Seats-tab view settings (dialog from the System menu)

All are per-user UI preferences, stored in the per-user settings key (value names are the
reference product's, for interop only):

| Setting | Value name | Default | Effect |
| --- | --- | --- | --- |
| Scale device icons to screen resolution | `adjustTermIconsSizes` | on | Smaller icons on small screens |
| Even rows of seat containers | `DevContainersLayout_EvenLayout` | off | Layout |
| Duration of "just connected" frame | `secondsToFlashInsertedDevice` | 4 s (2–20, step 2) | Section 3.1 |
| Show outputs that have no monitor / surfaces not connected to any output | `displayViewMask` (HYPOTHESIS: one bit each; live 0) | off | Display view |
| Show all USB hubs | `showAllUsbHubs` | on | When off, empty hubs and hubs that only hold another hub are hidden |
| Show shared devices inside seat containers | `sharedShownInWorkplaces` | off | Shared devices are drawn under the seat's own devices, with the shared sign |
| Per-container shared-devices toggle | `sharedHideoutShown_<n>` (`-1` = System) | System: on, seats: off | Remembered per container |

The same dialog also holds the **force-assign monitors to outputs at start** option
(2.70.1). It is **not** a per-user preference: the service re-binds every monitor to its
recorded output at seat start, so the value is machine-wide. Its storage is not known
(open question 3). It is described with the output assignment in section 7.5.

Other per-user state:

| Value name | Meaning |
| --- | --- |
| `windowGeometry`, `window\geometry`, `window\screenName`, `window\knownScreens` | window position, the screen it was on, the known screens |
| `visibleTermMask` | visible-seats bitmask (open question 8) |
| `ctrlTab` | HYPOTHESIS: index of the last selected tab (live 0; 1 in an older version's key) |
| `termMasterDetail` | unknown. HYPOTHESIS: an alternative master/detail layout of the Seats tab. Live: off. |
| `showInventory` | unknown. HYPOTHESIS: shows a device inventory pane. Live: off. |
| `noShowGoodApply` | do not show the success message after save |
| `askOnManualStart` | confirm before a manual start |
| `hideTrayIcon`, `locale`, `uiStyle`, `uiColorScheme`, `ShowBRANDLogoOnStartup` | see 4.3 and 4.4 |

## 5. Advanced settings tab

This tab is for experienced users only. Items:

- Per-seat IP binding table (section 7.4), if it is not on its own tab.
- A Hyper-V helper (2.x). It shows whether Hyper-V is installed and can install or remove
  it (removal also removes the virtual switches and adapters). It lists virtual switches
  (add/remove, by type) and virtual adapters (add one per seat, show its IP, make the IP
  static). The purpose is to give each seat its own IP address on a virtual adapter.
- Links to Device Manager and Network Connections: there the user can create virtual network
  adapters or add IP addresses that can then be given to seats.

## 6. Policies the manager enforces

### 6.1 Admin-only

See 4.4. The check uses the user's group membership (Administrators). The manager logs the
policy value and the admin state at start (verified live).

### 6.2 Same account on several seats

- The warning comes from the manager, so it only appears when the manager runs in that
  seat's session. That happens through the manager's `-autostart` Run entry at logon
  (section 1). When the Run entry is missing, the warning never appears. When the tray
  icon is hidden, the warning HYPOTHESIS still shows (it is a dialog, not a tray message),
  but this is not verified.
- Content: the account is already signed in on another seat, and this setup is not
  recommended. The box has an option to stop showing it and, from 2.70.6, a button that
  signs the user out.
- The service decides how strictly this is enforced (allow / warn / deny; machine value
  `SameUserMultiLogon`, see `30-sessions.md`). The manager reads the other seats' logged-on
  users from the product's volatile runtime key.

## 7. Dialogs

### 7.1 Assign device to seats

- It shows one control per seat. Shareable types get checkboxes plus "all seats" and "no
  seat" buttons. Exclusive types get radio buttons plus a "no seat" choice.
- Save writes the device's new seat mask. Cancel closes the dialog without saving.
- "No seat" makes the device spare.

### 7.2 Seat name

- One text field. The default name is the seat number.
- HYPOTHESIS: there is no validation beyond a non-empty name, and the length limit is
  unknown.

### 7.3 CPU cores per seat

- A grid: one row per seat, one checkbox per logical processor.
- Save stores a per-seat affinity mask (machine-wide, in the per-seat key; see
  `80-apps-compat.md`). The default is all cores for every seat.
- HYPOTHESIS: at least one core per seat is required.

### 7.4 IP address per seat and applications

- Purpose: some network programs and games need each seat to use its own local IP address
  (for example LAN games between seats, or servers that tell the seats apart).
- Fields:
  - **IP address**: a combo that lists only *static* IPv4 addresses on this PC, or "not
    assigned".
  - **Rule** (radio): bind the IP to all programs started on this seat; or only to the
    programs in the list; or to all programs except those in the list.
  - **Program list**, common to all seats: rows of title + path, each with a checkbox to
    switch it off for a while. Add and remove buttons. Adding opens a picker tree with these
    roots: Computer (file system), common and personal desktop, and common and personal
    Programs menus. One action adds the selected exe or the target of the selected
    shortcut. Another adds every program in a folder; it works only under the Computer
    root.
  - A warning appears when the list is too long and would be truncated on save.
- Stored as follows: the per-seat IPv4 DWORD (`SpecificIP`) in the per-seat machine key;
  the rule (`SpecificIPMode`); the program list under a machine-wide `SpecificIPApps` key.
  The enforcement is in the kernel and is described in `80-apps-compat.md`.

### 7.5 Video outputs assignment (advanced)

- A list of surfaces (`A.S`). Next to each surface are its adapter's outputs, as checkboxes
  with pictures. The output that drives the surface now is framed and in bold.
- The user picks which output(s) should feed a surface. More than one output means
  **cloning**, which has its own checkbox to allow it.
- A per-surface checkbox chooses a hardware or a software mouse pointer.
- Changes need a reboot. The help says Windows may not honour the request.
- Results are reported per monitor: assigned to output, force-assigned to output, or not
  assigned.
- **Force-assign monitors to outputs at start** (2.70.1, machine-wide, off by default
  HYPOTHESIS): the automatic form of the same binding. At seat start the service re-binds
  every monitor to its recorded output. Storage: open question 3; see `40-display.md`.
- Errors: the output cannot be reassigned, the display flags cannot be set, a conflicting
  device, the device is no longer present.

### 7.6 Keyboard/mouse switch hotkey

- The hotkey is Ctrl plus one key. The modifier is fixed by the dialog: no other modifier
  can be chosen (HYPOTHESIS that the driver also accepts only Ctrl; the stored value is a
  single key code). The user clicks the key field and presses a key. A reset button turns
  the hotkey off.
- Effect: this system-wide hotkey moves the keyboard and mouse that pressed it to the next
  seat (see `50-input.md`).
- Stored as a key code in the driver service key (`InputDirectionSwitchKey`). The test PC
  has 0x58 (the X key). **The default value after install is not known**; 0x58 may be the
  default or a value set earlier on that PC. A write failure shows an error with its code.
- The dialog warns that a system-wide hotkey must be chosen with care.
- Not related: changelog 2.26 mentions a software-simulated Win+Ctrl+Shift+B press. That is
  the Windows graphics-driver restart shortcut, used by the product itself; it is not a
  user hotkey.

### 7.7 Experimental settings

Two checkboxes. Each writes a driver flag:

- special handling of display modes. HYPOTHESIS: this is the `DXTest` flag.
- support of display-only devices (USB and indirect displays): the
  `DODTest` flag. It is on by default since 2.17.

The dialog warns about unpredictable side effects. A write failure shows an error dialog.

### 7.8 Special settings (tweak list)

- A combo of named tweaks that are shipped as registry files next to the program, plus an
  apply button. The user must confirm. On success the dialog says a reboot is needed. On
  failure it shows an error code.
- The dialog warns: use these only on support's advice.
- The catalogue of tweaks and their meaning is in `20-service.md` and `90-install.md`
  (GPU isolation off, compatibility-check level, monitor hot-plug mode, file-system filter
  on/off, power buttons, session transition timeout, VidPN binding, hardware GPU scheduling,
  ignore last unclean shutdown (linked to the safe fallback in 2.3), legacy boot/session
  schemes, filter propagation, same-user policy, SharedPC mode mask, touchpad palm
  rejection off, no automatic restart sign-on, lock screen logo set/clear, icon size).

### 7.9 Device picture

- A grid of the pictures available for this category (built-in plus user folder).
- The chosen picture is stored per device in a JSON file in ProgramData. Interop names:
  file `user_custom_icon_configuration.json` (2.70.x) with a top-level array
  `IconUserSubstitute`. An older file `UserIcons.json` has the same shape. Both are empty
  on the test PC, so the record fields are not known. HYPOTHESIS: each record holds the
  device's canonical name (as in 3.8) and a picture reference.

### 7.10 Log verbosity

- Checkboxes for Debug, Info and Trace. Errors are always logged.

### 7.11 Seat account (auto-logon)

- Default: show the normal logon screen (no auto-logon).
- Account type radio: **Local** or **Domain**.
  - Local: a combo of the local user accounts, plus a button that opens the system User
    Accounts panel.
  - Domain: a domain name field and an account name field.
- Password and password confirmation. The password may be empty. Both fields must match.
- Stored: user name and domain in the per-seat machine key (`DefaultUserName`,
  `DefaultDomainName`); the password in an LSA private secret (never in the registry in
  clear text). The seat table shows a password-set marker. The service also records the
  last signed-in user SID per seat (`LastLoggedOnUserSID`).

### 7.12 Support request and diagnostic report

- The manager builds a zip report at one of three levels: basic system info; detailed
  info; detailed info plus recent Windows minidumps (which needs minidumps enabled; the
  manager offers to enable them). The user can attach files and paste clipboard text,
  HTML or an image, then send the report to the vendor. Pulsar Seat keeps only the local
  "save diagnostic report" part.
- Report contents (also written before each enable, as a pre-enable report):
  - product state, the started-seats map and the auto-start map, the start mode;
  - the Windows version, Secure Boot and HVCI state;
  - per-seat settings: name, IPv4, IPv6, domain, user (masked), password-set flag,
    affinity mask;
  - every device with its parent, port, interface class, path, seat mask and effective
    seat mask, and last activity;
  - monitors (adapter, source, forced source, instance, output type) and surfaces
    (adapter, source, flags, GDI name);
  - service and driver registry values;
  - the CCD (display configuration) database and the active paths;
  - the device tree with seat-owner tags and HID usages;
  - the core error history;
  - file checksums.
- The manager can also save and load a **configuration dump** file (binary, protobuf,
  extension `.asudf`) for support. It holds the device tree and assignments.

## 8. Flows and their effects

### 8.1 First start on an unconfigured PC

- The manager finds that no seat assignment exists (the control library reports whether
  any seat masks are set).
- It asks whether to run the setup wizard. Admin rights are needed.
- Yes: the wizard runs (section 9). No: the user configures the seats by hand on the
  Seats tab.
- At the end the manager reminds the user to pick a start mode and enable the product.

### 8.2 Save

- The control library writes:
  - each device's seat mask, as devnode properties read by the driver; display-source
    ownership in the video keys; audio endpoint per-seat state;
  - per-seat machine keys: name, account, IP, affinity;
  - the start bitmask and the start mode;
  - the assignment snapshot JSON (3.8).
- Driver flags (hotkey, experimental flags) are written directly to the driver service key.
- Elevation is needed. The success message can be turned off.
- Errors from the apply step are shown as an error class and code.

### 8.3 Enable / disable

Described as the **state the system is in** after each action, not as a call sequence.

- **Enabled state** (needs admin to reach; reboot required). Sources: the service host's
  install verb and the live disabled state (`20-service.md` sections 4.2 and 4.3); the manager notes
  only confirm that enable writes a pre-enable report and ends with a reboot prompt.
  - A pre-enable diagnostic report exists in ProgramData (verified file).
  - The driver service and the seat service have boot/automatic start types.
  - The driver is listed in the class upper-filter lists of the Display, HIDClass,
    Keyboard and Mouse classes.
  - The credential provider is registered.
  - HYPOTHESIS: a device-installation deny policy for display adapters is set, so that no
    display driver is installed while enabled (changelog 2.17).
  - 2.70.5+: the driver blocks the Windows Update engine while the product is enabled
    (driver value `BlockWUHandlers`), and the service writes Windows Update deferral
    values. Mechanism and values: `20-service.md` section 7.4 and `80-apps-compat.md`
    section 8.
- **HYPOTHESIS**: the order in which these changes are made, and whether the manager makes
  them itself through the control library or through the service host's command-line
  verbs, is not known. The control library holds both paths (`20-service.md` 4.2).
- **Disabled state** (verified live on the test PC): both services disabled, no class-filter
  entries, credential provider not registered. HYPOTHESIS: the device-install policy and
  the Windows Update block are also removed; not verified.
- Both actions end with a reboot prompt. Details are in `90-install.md`.

### 8.4 Start seats

- Allowed only while enabled. The manager asks the service to start every seat in the
  start bitmask, or one seat from its container menu (3.6).
- HYPOTHESIS: the transport of this request is not known. The control library has both
  named-pipe server and client code, with a pipe name built at run time, and the service
  has no pipe code (`20-service.md` section 10). So the pipe is likely between manager
  instances (for example an unelevated manager and an elevated copy). The start request
  itself may use service control or a value the service watches.
- Each seat gets a session and either the logon screen or an auto-logon (see
  `30-sessions.md`).
- A seat with no display waits until one is attached.

### 8.5 Force relogin (per seat)

- The seat's session is signed out and a new logon (or auto-logon) is started on it.
- Errors are reported as relogon errors with a code.

### 8.6 Reset settings

- After a double confirmation the manager runs the seat service executable with its reset
  verb. Every product setting returns to its default, then the manager restarts. Failure to
  launch the reset, or a non-zero exit code, is shown.

### 8.7 Hardware change while enabled

- Monitors cannot be added and resolutions cannot be changed while enabled. The supported
  path is: disable, reconfigure Windows displays, enable.
- New devices appear in the spare pool with the "just connected" frame, unless a USB hub
  assignment already covers their port. HYPOTHESIS: hubs assigned to a seat pass new
  devices straight to that seat.

### 8.8 Vendor USB display/audio adapter wizard

The reference product has a separate wizard for a vendor-specific USB display/audio
adapter. What it achieves, in effect:

- It counts the adapters it finds and waits until the count is stable.
- It requires the product to be disabled while the new displays are set up (disable and
  reboot if needed, then the wizard continues).
- The user extends the desktop onto the new displays and checks their resolutions (it
  opens the Display settings).
- Each adapter (by serial number) gets a seat, pre-selected automatically, and an optional
  name. Seats that already have a display are marked.
- At the end it enables the configured seats and the product and reboots. Each of these
  final steps can be skipped.

Pulsar Seat does not need a vendor-specific wizard. The general wizard should handle USB
and indirect displays the same way.

## 9. Setup wizard

There are two cooperating parts.

### 9.1 Console gatherer and automatic basic layout

A console program with these modes:

- **Gather** (no argument or `1`): asks the user to press any key on the *main* keyboard
  and move the *main* mouse. Raw input tells it which device did this. It also detects the
  active main monitor. It writes a small text file into a shared ProgramData folder (ACL
  Everyone) with three lines: the main monitor's GDI name, the main mouse's interface path
  and the main keyboard's interface path. It also writes a log and an error-flag file.
- **Stop gather** (`2`): signals a running gatherer through a named semaphore.
- `-force`: overwrite an existing configuration.
- **Automatic basic layout** (after gathering). Observed effects only, not an order:
  - Every connected monitor becomes part of the extended desktop.
  - There is one seat per active monitor. Seat 1 gets the gathered main monitor.
  - Audio endpoints whose ContainerID matches a monitor's ContainerID (monitor speakers)
    go to that monitor's seat.
  - Keyboards and mice (HID) are grouped by the USB hub they sit in, near each monitor.
    HYPOTHESIS: "near" means sharing a hub with the monitor's USB parts; the rule is not
    known in detail. ContainerID matching is documented for audio only.
  - Seat 1 gets the gathered main keyboard and mouse.
  - The result is applied as a normal save.
  - It changes nothing when the product already has a seat assignment or is enabled,
    unless `-force` is given.

### 9.2 Interactive full-screen wizard (in the manager, since 2.60.5)

Preconditions: all devices connected and the hardware fixed during the run. Admin rights.
The wizard is not offered when the screen resolution is below a minimum. On start the
manager first explains the steps in a dialog.

1. **Variant choice**: the dialog shows the devices it would use for seat 1. The user picks
   one of:
   - use external mice on every seat and run the wizard on seats 1..N;
   - keep the shown devices on seat 1 and run the wizard on seats 2..N;
   - put the shown devices on seat 1 and all other devices on seat 2, with no wizard.
     This is the quick two-seat setup.
2. **Pick seat 1's monitor**: every monitor shows a large number on a shaped, top-most
   overlay window. The user presses the matching number key. Seat 1 is always on.
3. **One monitor per seat**: each remaining connected and powered-on monitor becomes one
   seat, up to the seat limit. Extra monitors beyond the limit get no seat.
4. **Keyboard**: each monitor shows a full-screen page for seat N. The user presses F<N>
   twice on the keyboard meant for that seat: F1 for seat 1, F2–F12 for the others. The
   key and the count are shown on screen. Raw input tells which physical keyboard pressed
   it.
5. **Mouse**: on each seat's screen, the user clicks two targets at random positions with
   that seat's mouse. Raw input tells which mouse clicked.
6. **Keys valid on any keyboard during the wizard**:
   - Enter confirms this seat.
   - Backspace resets this seat.
   - Shift+Enter saves all seats.
   - Shift+Backspace resets all seats.
   - Shift+Esc or Alt+F4 quits without saving.

   A seat that is done shows a waiting page until the other seats are done.
7. **Result**: the wizard writes an output file with one section per seat. The manager reads
   it and applies it through the control library. Errors: wizard program not found, not
   started, crashed, no output file, unreadable file, bad format, unknown section, no
   devices in the output, conflicting devices, apply error with code.

These steps are user-visible screens, so their order is part of the behaviour.

## 10. Data the manager reads and writes (summary)

| Data | Location (reference product) | Written by the manager? |
| --- | --- | --- |
| UI preferences | per-user Qt settings key (`HKCU\SOFTWARE\<vendor>\<manager>\v2.0.0`): value names in 4.5 | yes |
| Product root | `HKLM\SOFTWARE\<vendor>\<product>`: `AutoStartMode`, build string, install path, help URL | `AutoStartMode` |
| Per-seat settings | subkeys `00`..`15` of the product root: `DefaultUserName`, `DefaultDomainName`, `LastLoggedOnUserSID` (service), `SpecificIP`, IP mode, `Affinity`, name (HYPOTHESIS) | yes |
| Start bitmask, admin-only flag | context subkey of the product root: start bitmask (`startstations`), `OnlyAdminCanChangeSettings` | yes |
| Runtime state | volatile subkey of the product root, and the seat service's `link\volatile` key | read only |
| Auto-logon password | LSA private data, one secret per seat | yes |
| Driver flags | driver service key: `InputDirectionSwitchKey`, `DODTest`, `DXTest`, and the tweak values | yes |
| Device seat masks | devnode properties plus video/audio keys (via the control library) | yes |
| Assignment snapshot | `%ProgramData%\<product>\default_configuration.json` (3.8) | yes |
| Custom pictures | `%ProgramData%\<product>\user_custom_icon_configuration.json`; older `UserIcons.json` (7.9) | yes |
| Diagnostic report / config dump | `%ProgramData%\<product>\preEnableReport.txt`, `ConfigurationDump.asudf` | yes |
| Logs | `%ProgramData%\<product>\logs\` | yes |
| Wizard handoff | `%ProgramData%\<utils folder>\` (main-device file, error flag, legacy settings ini, logs) | the console wizard |
| Run entry | `HKLM\...\CurrentVersion\Run`, value named after the manager executable, `-autostart` | installer |
| User pictures | `<install>\Userimages\<category>\` | read only |
| Wallpapers | `<install>\Wallpaper\` | read only |
| Tweak files | `<install>\Regpatches\*.reg` | read only (applied on request) |

## 11. Windows APIs and mechanisms

Evidence source per line: **CL** = imports/strings of the control library; **M** = strings of
the manager exe. The manager exe is packed, so its own import table is not verified; most
mechanisms below are proven for the control library only, which the manager loads.

- Device inventory (CL): SetupDi / CfgMgr32 (device tree, parents, location paths, bus
  numbers, addresses, ContainerID, USB hub port enumeration), HID
  (`HidD_GetPreparsedData`, `HidP_GetCaps`, usage pages), the device-interface class GUIDs
  in 3.1, device-change notifications (`RegisterDeviceNotification`, MMDevice notification
  client).
- Identify by touch (CL): Raw Input (`RegisterRawInputDevices` with input sink,
  `GetRawInputData`, `GetRawInputDeviceInfo` to map a handle to an interface path).
- Identify monitors (CL): shaped top-level windows (`SetWindowRgn`) on each monitor with a
  big number; `EnumDisplayMonitors` / `GetMonitorInfo` / `MonitorFromPoint`.
- Displays (CL): CCD (`GetDisplayConfigBufferSizes`, `QueryDisplayConfig`,
  `SetDisplayConfig`, `DisplayConfigGetDeviceInfo` for names, output technology and target
  device paths), `EnumDisplayDevices`.
- Audio (CL): MMDevice endpoints (registry and API); a test sound through waveOut
  (`waveOutOpen`, `waveOutWrite`, `waveOutMessage`, ...).
- Webcam preview (M): a multimedia capture framework (Qt Multimedia on Media Foundation).
- Sessions (M): `WTSEnumerateSessions` (who is logged on where).
- Networks (M): `GetIpAddrTable` (static IP list). Hyper-V management (PowerShell / WMI):
  HYPOTHESIS.
- Accounts: local account listing for the account combo. HYPOTHESIS: `NetUserEnum` or a
  similar API; no such name was found in any strings dump. Passwords (CL):
  `LsaStorePrivateData`.
- Named pipes (CL): `CreateNamedPipeW`, `ConnectNamedPipe`, `WaitNamedPipeW`,
  `TransactNamedPipe`, `DisconnectNamedPipe`, `SetNamedPipeHandleState`,
  `GetNamedPipeClientProcessId`, `GetNamedPipeServerProcessId`. Both server and client
  side. The pipe name is built at run time and was not recovered. HYPOTHESIS: used between
  manager instances (8.4).
- Driver control (CL): two control devices, opened with `CreateFile` and driven with
  overlapped `DeviceIoControl` with a timeout:
  - the **main control device** (a DOS device name derived from the driver name), used for device ownership while the driver
    runs normally;
  - the **safe control device** (the same name with a safe suffix), a reduced path used when the driver
    runs in its safe fallback (HYPOTHESIS, see `20-service.md`).
- Security state (CL): reads the DeviceGuard scenario keys (HypervisorEnforcedCodeIntegrity,
  Host Guardian) and the Secure Boot state for the report and for the policy-conflict check
  (2.3).
- Services (CL): SCM (`CreateService`, `ChangeServiceConfig`, `ControlService`), class
  UpperFilters registry edits, credential provider registration.
- Power and reboot (CL): `ExitWindowsEx` with the shutdown privilege; elevation through
  `ShellExecuteEx` with the "runas" verb.
- Self-check (CL): `WinVerifyTrust` on its own files.

## 12. Error cases (manager side)

- Control library missing or failing to initialise. The manager cannot work; the console
  wizard logs a load error and falls back to file-only output. The live `a` logs show this
  failure for short-lived launches (section 1).
- Neither control device of the driver is reachable (main or safe, section 11).
  Device-ownership calls fail: the logs showed many timed-out driver I/O calls with Windows
  error 31 while the product was disabled. Expected behaviour: views still work from
  user-mode enumeration.
- ProgramData folder cannot be created, locked or ACL'd. This is reported as a data-store
  error.
- Registry write failure for the start bitmask or the hardware history.
- The wizard errors listed in 9.2.
- Save conflicts (3.4), invalid GPU placement (3.4), missing seat-1 display.
- Not elevated; admin-only policy; second instance; unsupported OS build; OS build changed
  under the product; Windows Safe Mode; the product's safe fallback; security-policy
  conflict (2.3).

---

## ARM64 notes

- Every manager-side binary of the reference product is x64: the Qt 6 manager, its control
  library and the console wizard. On Windows 11 ARM64 they would run only under x64
  emulation. Their kernel driver cannot load at all, so the manager's ownership features
  are dead there. There is no ARM64 build of any part.
- Pulsar Seat's manager must be a native ARM64 build (and x64). Tauri 2 with WebView2 has
  ARM64 support. Every API in section 11 exists on ARM64 Windows: SetupDi/CfgMgr32, HID,
  Raw Input, CCD, MMDevice/WASAPI, Media Foundation, WTS, IP Helper, LSA, SCM, named pipes.
  For the speaker test sound, use WASAPI on the chosen endpoint, not legacy waveOut.
- **CPU cores dialog**: ARM64 PCs can have cores of different classes (performance and
  efficiency), and some have more than 64 logical processors (processor groups). Build the
  grid from `GetSystemCpuSetInformation` / `GetLogicalProcessorInformationEx`. Show the
  core class. Use CPU-set IDs or group-aware masks, not a single 64-bit mask.
- **Displays**: ARM64 PCs often have a single integrated GPU with several outputs. The GPU
  rule in 3.4 decides which seat layouts are possible. The manager must explain it clearly.
  USB/indirect displays (IddCx) are a common way to add monitors on ARM64. HYPOTHESIS:
  vendor ARM64 drivers for USB display adapters exist; check them on the dev laptop.
- **Hyper-V helper**: Hyper-V exists on ARM64 Windows 11 Pro/Enterprise, but the per-seat
  IP binding it supports needs a different mechanism in Pulsar Seat (see
  `80-apps-compat.md`). Do not depend on it for v1.
- **Webcam preview**: use Media Foundation directly or the WebView `getUserMedia`. Both
  work natively on ARM64.
- **Wizard input**: keyboards and mice on ARM64 laptops are often I2C/SPI HID (built-in
  keyboard, precision touchpad), not USB. Grouping by USB hub does not apply to them. Group
  by ContainerID and treat built-in devices as seat 1 by default.
- **Memory Integrity**: ARM64 Windows 11 PCs ship with HVCI on by default. This is one more
  reason the manager must never suggest turning a security policy off.
- **Check the PE machine type** (0xAA64 / 0x8664) of the manager, the wizard and any helper
  exe in CI (rules.md).

## Pulsar Seat approach

Constraints from `docs/agents/rules.md`: no vulnerable drivers, no kernel patching,
everything works with HVCI and Secure Boot on, UMDF/user mode preferred, native ARM64, UI
copy in Turkish, GPL-3, no third-party names or UI text.

- **Security policies are never touched or questioned.** Pulsar Seat never asks the user to
  turn off Memory Integrity, the vulnerable-driver blocklist, Secure Boot, test-signing
  enforcement or any other Windows security policy. The 2.3 row for error `2(31)` has no
  Pulsar equivalent. If a Pulsar component fails to load because of such a policy, that is
  a Pulsar bug: the manager reports the component as failed and points to a fix in Pulsar
  (update), never to a Windows setting.
- **Process split**:
  - The manager is a Tauri 2 + SvelteKit app, unprivileged by default, one instance per
    session.
  - All privileged work goes through the Pulsar Seat service over one named pipe with an
    ACL (plan.md). This covers writing config, enabling/disabling, starting seats, assigning
    devices, storing secrets and reset. The service authorises each request: Administrators
    (elevated token) for changes; any interactive user for read-only status, if the admin
    allows it. The admin-only policy (6.1) is enforced by the service, not only by the
    manager.
  - There is no separate control DLL inside the GUI and no manager-to-manager pipe. The
    service owns the config.
- **Config model**: one versioned config document owned by the service (for example
  a file under `%ProgramData%\Pulsar Seat\`, ACL SYSTEM + Administrators full, Users read;
  format and location are fixed in `20-service.md` §13.2). It holds the
  seats (number, name, start flag, auto-logon user/domain, affinity, IP rule), the start
  mode, the hotkey, flags, and the device assignments keyed like the reference snapshot:
  a stable physical position (port chain / location path + interface class + interface
  number) plus the friendly name, so that a replacement device in the same port keeps its
  seat. Store seat masks as integers. Passwords go into LSA private data or DPAPI (machine
  scope), never into the file. UI preferences live in the app's own per-user store.
  - Optional importer: read an existing `default_configuration.json` (3.8 field names) and
    map it to our config. This is a convenience for users who switch, not a v1 must.
- **Seat limit**: Pulsar Seat sets its own limit (proposal: 16, matching the largest seat
  table seen). No paid tiers (plan.md).
- **Screens to build** (Phase 1): Seats canvas (drag and drop, device and seat menus, status
  colours, activity flash, "just connected" frame, shared/spare pool, indication), basic
  settings (seat table, start mode, language fixed to Turkish at first, theme), assignment
  dialog, seat name, seat account, view settings, configuration comparison, diagnostic
  report, tray menu. Later phases: video-output assignment, CPU cores, hotkey, IP binding,
  experimental flags. The tweak list becomes named settings in our own UI. There are no
  raw `.reg` files.
- **Indication** (3.5.1): monitor number overlay window; WASAPI test tone on the chosen
  render endpoint; Media Foundation / WebView camera preview; activity flash for input
  devices. All user mode.
- **Identify by touch** works without kernel hooking:
  - Before isolation is active, the manager in seat 1 registers Raw Input with input-sink
    for keyboards, mice and HID. Each event is mapped to its device interface path, and the
    picture flashes.
  - While isolation is active, input from other seats may not reach seat 1's session. The
    service then forwards "device activity" events over the pipe. Their source is the
    per-unit packet counters of Pulsar's own signed KMDF/UMDF input filter in the documented
    WDK filter position, or raw input in the console session. That design is in
    `50-input.md` section 4.1, items 2 (input-isolation filter) and 6 (identification flows).
    No global hooks, no kernel patching.
- **Identify monitors**: per-monitor top-most borderless windows (Tauri multi-window or
  plain Win32 layered windows) that show the seat or monitor number. Mapping uses CCD
  target device paths, not GDI names, which change.
- **Wizard**: one native implementation inside the manager. It covers:
  - main-device capture by touch;
  - extending the desktop via CCD (with the user's consent);
  - one seat per monitor, up to our seat limit, with monitor speakers matched by
    ContainerID;
  - F-key / mouse-target capture on full-screen per-monitor windows. The service routes
    input from every unit to the wizard while it runs (`50-input.md` 4.1 item 6), so the
    wizard also works with isolation on;
  - writing the result straight into the service config.

  There is no separate console exe and no file handoff.
- **What each action changes in Pulsar Seat**:
  - Save: the config document, then the service applies device ownership through its
    documented mechanisms (`40-` to `70-`).
  - Enable/disable: the service's own start type and the components that the
    architecture spec (`00-architecture.md`) decides on. Any class or device filter that
    Enable registers is a signed KMDF or UMDF package with an ARM64 and an x64 build that
    loads with HVCI and Secure Boot on. No unsigned or test-signed component.
  - **Policy changes are out of scope for Enable.** Pulsar Seat does not set a
    device-installation restriction policy and does not block or defer Windows Update
    (consistent with `20-service.md` and `80-apps-compat.md`). If a Windows update breaks
    a seat, the manager detects it (OS build change, component health) and tells the user.
    A device-install restriction may come back later only as an explicit, visible opt-in
    setting with its own decision.
  - Start seats / relogin: service requests over the pipe.
  - Reset: the service restores the default config.
- **Not copied**: kernel injection, display-kernel internals, test-signing toggles, the
  per-seat IP kernel hook, security-policy guidance, Windows Update blocking, licence and
  activation screens, vendor support upload, the vendor adapter wizard, online
  update/OS-support lookup (Pulsar has its own update channel; see `90-install.md`).
- **Turkish UI**: all strings are ours. Error codes are our own scheme, but with the same
  idea: subsystem plus code, plus a short fix hint.

## Open questions

1. How exactly does "start when someone signs in on seat 1" trigger? Is it the manager's
   `-autostart` launch, the service watching logons, or the credential provider?
2. Where are the seat name and the IP rule stored? A per-seat value name for the name was
   not seen (the names on the test PC are the defaults). We need a seat with a custom name
   on the test PC, set by the maintainer.
3. Where is the "force-assign monitors to outputs" flag stored (2.70.1)? It may be a driver
   service value, a seat service value, or `_mtxk_flags` in the video key.
4. Which HID usages count as shareable, and which are exclusive like a keyboard or mouse
   (touchpad, touchscreen, consumer-control keys, gamepads)?
5. What does the display "Flags" value (seen as 0x1 on one surface) mean: hardware cursor,
   cloning or forced binding?
6. Which settings apply live after Save, and which need a seat restart or a reboot?
7. What does the wizard output file look like (sections, keys)? No output file was found on
   the test PC; only the legacy settings ini name is known.
8. What does the visible-seats bitmask encode? The live value 12 does not match "seats 1–2
   configured". It may be offset by one, or it may hide seats.
9. Pulsar Seat's own seat limit: 16 is proposed. Does any Windows limit (sessions, display
   paths) make a lower or higher number sensible?
10. Does the reference wizard need isolation off? It runs while disabled and while enabled.
    With isolation on, raw input from other seats' keyboards reaching the wizard implies a
    driver-side path. Check this on the test PC with the maintainer (needs the product
    enabled, which is a system change).
11. Can the "only admins" policy be bypassed by starting the manager from another account?
    In the reference product it is a client-side check. In Pulsar Seat the service enforces
    it.
12. Are any manager actions refused when the manager runs on a seat other than seat 1? No
    evidence was found; check with the maintainer on a started seat.
13. What does indication do for microphones, card readers and USB hubs? Is there a
    keyboard-LED or mouse indication at all?
14. What do the log-name letters `s`, `u` and `a` mean exactly?
15. What does the custom-skin setting load, and where is it stored?
16. What are the fields of an `IconUserSubstitute` record? Needs a device with a custom
    picture on the test PC (a change, ask the maintainer).
17. The default value of `InputDirectionSwitchKey` after a clean install.
18. Which features use the "no network, restart the manager" message?
