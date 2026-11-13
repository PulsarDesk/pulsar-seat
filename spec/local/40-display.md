# 40 — Display: monitors and GPU outputs per seat

Status: draft v1 (2026-11-12, critiqued, cross-checked). Analyst. Reference product: version 2.70.3
installed on the x64 test PC (product disabled while observed), 2.70.6 package read
statically. Items marked **HYPOTHESIS** are inferred and not yet confirmed by observing the
running product. Because the product was never observed enabled, every statement about
what a seat session does at run time is a HYPOTHESIS unless it says "observed".

## 1. Scope

How each seat gets its own monitors. This spec covers:

- the display data model (adapters, sources, targets, monitors) and how seats own them;
- the device-class filter position of the reference driver on display adapters;
- binding monitors to outputs, cloning and the cursor type;
- what the session of a seat can see, and DWM per session;
- multi-GPU and GPU isolation;
- resolution, refresh rate and orientation;
- fullscreen games;
- monitor hot-plug and monitor power;
- indirect and display-only devices;
- wallpaper, lock-screen image and screensaver handling;
- the display parts of the first-run wizard and of the diagnostic report.

Related specs: input routing is in `50-input.md`. How each seat gets its own session is in
`30-sessions.md`. Install-time changes and driver signing are in `90-install.md`. The
manager screens are in `10-manager.md`. Out of scope: the reference product's licence
component (it exists; it is not described).

## 2. Terms

| Term | Meaning in this spec |
| --- | --- |
| Adapter | A WDDM display adapter (GPU, USB display adapter, indirect display adapter). It is identified by its PnP instance and by an adapter LUID that changes on every boot. |
| Source | A VidPN source of an adapter: one desktop surface, seen by user mode as `\\.\DISPLAYn`. In the reference UI this is the unit a seat is given (one tile per source). |
| Target | A VidPN target: one physical output connector (HDMI, DP, eDP, ...) or one virtual output. It has a target id that stays stable for the same connector. |
| Monitor | The device on a target (a `DISPLAY\...` PnP device with an EDID). |
| Path | A source-to-target link, as reported by CCD (`QueryDisplayConfig`). |
| Seat | One workplace: monitor(s), keyboard, mouse, audio, and one interactive session. Seat 1 is the console session. |

## 3. Reference behaviour

### 3.1 Seat-to-display model

- **A seat owns sources, not monitors** (observed in the saved configuration). The unit of
  assignment is an adapter source. A monitor belongs to a seat because its target is
  linked to a source that the seat owns.
- **Display identity in the UI.** Each source is shown as `A.S`:
  - `A` is the adapter number. Adapters are numbered in the order they were initialised
    (changelog 2.23). A legacy numbering scheme can be selected with the per-machine switch
    `oldDisplayNums` (read by the control library; location and values HYPOTHESIS).
  - `S` is the source number on that adapter, starting at 1.
  - The tile name combines the adapter name, the output type and the monitor name.
  - A source with no output linked is shown as inactive. A view option hides such sources.
- **Identity in the saved configuration.** The configuration export (`DeviceAsignment[]`,
  see `10-manager.md`) has one record per source with `Type` = `Display`. The record holds:
  - the short `A.S` name;
  - a long name made of the adapter's PnP location path, the display-adapter interface
    class prefix and the 0-based source index (`SR0`, `SR1`, ...);
  - a root-enumerated adapter (a virtual display) is keyed by its instance path instead;
  - the seat mask as a string of binary digits.
  Monitors are not stored as separate records.
- **Seat mask and seat index.** In the diagnostic report every source and monitor has a
  `StationsMask` and an `EffectiveStationsMask` (bit n = seat n+1). Observed: the two
  sources of the integrated GPU had mask 0x2 (seat 2). The discrete GPU's first source
  was saved for seat 1.
- **Hard rule: seat 1 must have a display.** The manager refuses a configuration where the
  first seat has no display. A seat without a display is shown as not operational. Such a
  seat starts only once a monitor is attached.
- **One monitor per seat is the normal case.** A seat with several monitors is supported
  with one restriction: none of its GPUs may be shared with another seat (see 3.5).

### 3.2 Device-class filter position

- Static (never observed in place, the PC was only seen disabled): the reference kernel driver
  is installed as a **PnP class upper filter** on the Display
  adapter class `{4d36e968-e325-11ce-bfc1-08002be10318}`: its service name is added to the
  class `UpperFilters` value under `HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-...}`.
  The seat service and the control library write this value; the driver also reads the
  class key. The same driver is a class upper filter on the HID, keyboard and mouse classes
  too (see `50-input.md`).
- Observed while the product is disabled: the Display class key and every display adapter
  instance have no `UpperFilters` value, so the filter is added on enable and removed on
  disable (or on the disable command of the service).
- The driver keeps a marker value `mtxk_upperfilters` and a marker `mtxk_denydeviceclasses`
  (HYPOTHESIS: they record what it added, so it can restore the original state).
- **HYPOTHESIS:** being a filter on every display adapter lets the driver see each adapter
  start and stop and match it with its `Control\Video` settings (3.3).

### 3.3 Binding monitors to outputs (forced video outputs)

- An advanced dialog lists every source of every adapter, with the adapter's outputs
  beside each source. For each source the user can do three things:
  - pick which output(s) to link to it (one or more);
  - turn **cloning** on, so one source is shown on several monitors;
  - choose a **hardware or software mouse cursor** for that source.
- The dialog warns that Windows may not honour the choice and that a reboot is needed to
  check the result.
- In the list, each monitor shows whether it is forced to a given `A.S` or is only
  currently in use with a given `A.S`. Possible errors:
  - the output cannot be reassigned;
  - the display flags cannot be written;
  - the device is no longer present;
  - devices conflict.
- A view option makes the product re-apply the saved output-to-source bindings every time
  the seats start (added 2.60.5, extended 2.70.1).
- Since 2.70.6 the product also handles several monitors on one port (DP MST) and marks a
  monitor configuration that conflicts.
- **Storage** (observed live). The data sits under
  `HKLM\SYSTEM\CurrentControlSet\Control\Video\{VideoID}\000N`, where `{VideoID}` is the
  adapter's `VideoID` device parameter. The same set of values is repeated in every
  `000N` subkey of that adapter:

  | Value name pattern | Type | Meaning |
  | --- | --- | --- |
  | `SSSS_mtxk_terminal` | DWORD | `SSSS` = 4-digit decimal 0-based source index. Value = 0-based seat index that owns the source; `0xFFFFFFFF` = unassigned. |
  | `TTTTTTTT_mtxk_source` | DWORD | `TTTTTTTT` = 8-digit hex target id. Value = the 0-based source the target is forced to. Two targets forced to one source = clone. |
  | `SSSS_mtxk_flags` | DWORD | Per-source flags. Bit 0 was set on one source. **HYPOTHESIS:** bit 0 is the clone or the hardware-cursor choice. |

  Observation: after a GPU driver update, the discrete GPU got a new `VideoID` and lost all
  of these values. The mapping is therefore fragile across driver reinstalls.
- `HARDWARE\DEVICEMAP\VIDEO` is used to map `\Device\VideoN` to an adapter and source. The
  report prints a `VideoN = A.S` table.
- The driver also reads `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity`,
  the Windows CCD persistence database (HYPOTHESIS: to learn the saved topology per
  monitor set and keep seat paths in it stable).

### 3.4 How a session is bound to its monitors (all HYPOTHESIS)

Nothing in this section was observed at run time.

- **HYPOTHESIS:** the reference product makes each seat's session the owner of that seat's
  sources. The session then renders its desktop directly on its own physical monitors, as
  if it were a console session on those outputs.
- Static evidence for this:
  - the kernel driver locates internals of the graphics kernel (`dxgkrnl.sys`) and of the
    win32k base module, opens the graphics kernel device and uses the watchdog GDI callout;
  - it checks whether the current process is the desktop window manager (DWM);
  - it imports memory-mapping primitives that allow writes to protected kernel memory.
    **HYPOTHESIS (strong):** it modifies the graphics stack in memory so that a
    non-console session can own physical VidPN sources. Pulsar Seat must not do this
    (section 6).
- **Secure Boot / HVCI context.** 2.70.6 adds an error that tells the user the driver
  cannot work together with the Windows 11 code-integrity protection. 2.70.6 also ships two
  builds of the kernel driver: one signed only by the vendor, and one with a Microsoft
  attestation signature. The installer picks one by the Secure Boot state. **HYPOTHESIS:**
  the attestation-signed build lets the driver load with Secure Boot on, but its in-memory
  modification of the graphics kernel still fails when HVCI is on, which produces the
  error. The diagnostic part of the control library reads the HVCI scenario key
  `HKLM\SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity`
  (and the Host-Guardian scenario key next to it).
- **DWM per session.** Every interactive Windows session has its own DWM instance.
  **HYPOTHESIS:** the driver's DWM check is there because the patched behaviour applies only
  when the DWM of a seat session opens or presents to its sources. How DWM of a seat behaves
  when the seat is locked, disconnected or at the sign-in screen is not known.
- The seat service, in user mode, does the following (from imports):
  - opens the adapter that backs a display from an HDC (`D3DKMTOpenAdapterFromHdc`),
    creates a device (`D3DKMTCreateDevice`), calls `D3DKMTSetVidPnSourceOwner`, queries
    the device state (`D3DKMTGetDeviceState`), and later destroys the device and closes
    the adapter (`D3DKMTDestroyDevice`, `D3DKMTCloseAdapter`). **HYPOTHESIS:** it does this
    from inside each seat session to claim the seat's sources, and releases them when the
    seat stops;
  - calls `ChangeDisplaySettingsW` with a reset step (the value name `CDSReset` appears)
    to re-apply the registry mode.
  - looks up `D3DKMTDDisplayEnum` (2.70.3) and also `D3DKMTDDisplayEnum2` (2.70.6) at run
    time from the API set `api-ms-win-dx-d3dkmt-l1-1-4`. **HYPOTHESIS:** an OS capability
    check or enumeration of displays that are not part of the desktop.
- Tuning values under `HKLM\SYSTEM\CurrentControlSet\Services\<driver service>` (DWORD;
  "regpatch" = one of the reference product's preset registry tweaks):

  | Value | Observed | Meaning |
  | --- | --- | --- |
  | `enableVidPN` | 1 (also a regpatch) | Use VidPN-level source/target binding for seats. **HYPOTHESIS:** 0 = older method. |
  | `conMonNtfy` | 1 (regpatches for 1 and 2) | Mode for connected-monitor (hot-plug) notifications. Meaning of 1 vs 2 unknown. |
  | `checkMonPresence` | not set | **HYPOTHESIS:** check that a monitor is present before a source is given to a seat. |
  | `oldPathsMgmt`, `oldGDCBS` | not set | **HYPOTHESIS:** switch back to older path-management and callback schemes. |
  | `ddpcCompBits`, `ddpcCompBitsX` | not set | Unknown (**HYPOTHESIS:** compatibility bits for display paths). |
  | `WDDMGrade` | not set | **HYPOTHESIS:** forces a WDDM version level for compatibility. |
  | `DXTest` | 1 | **HYPOTHESIS:** the value behind an experimental toggle in the manager for extra display modes. |
  | `DODTest` | 1 | Support for display-only devices (USB display adapters, indirect displays). Exposed in the experimental settings. |
  | `usdRefreshMsec` | not set | **HYPOTHESIS:** a refresh or poll interval in milliseconds (name suggests a shared-data update period). |
  | `vmwHpcMask` | not set | Unknown (**HYPOTHESIS:** a VM-related mask). |

### 3.5 What a seat's session can see

- **HYPOTHESIS:** while seats run, each session's desktop covers only that seat's sources.
  Window placement, `EnumDisplayMonitors` and the display settings page in that session
  show only the seat's monitors. Seat 1 sees only its own monitors too.
- The manager (running in seat 1) sees every adapter, source, target and monitor. It gets
  them from CCD, from the device tree and from the driver.
- With the product disabled, all monitors form one extended desktop of the console
  session (observed).
- The public helper library lets an application find its seat number (see `20-service.md`).

### 3.6 Multi-GPU and GPU isolation

- GPUs are assigned to seats through their sources. Two cases (from the manager's
  validation and help text):
  - several seats with one monitor each may share one GPU, each on a different source;
  - a seat with several monitors may not share any of its GPUs with another seat. The
    manager rejects such a layout. It suggests a dedicated GPU, or merging the monitors
    into one surface with the GPU vendor's multi-monitor spanning feature.
    **HYPOTHESIS:** source ownership per session works at adapter granularity once one
    session holds several sources.
- **GPU isolation** (changelog: optional since 2.23, on by default). All of this is from
  the changelog and value names, not observed:
  - **HYPOTHESIS:** processes in a seat see and use only the GPU(s) that drive that seat's
    monitors. The changelog ties the feature to Vulkan applications.
  - `AllGPUAccess` = 1 (DWORD, driver service key, regpatch) turns isolation off, and every
    seat may use every GPU.
  - **HYPOTHESIS:** isolation is done in the kernel by filtering adapter enumeration per
    session.
- Microsoft Basic Display / Basic Render are handled for compatibility (2.28). The driver
  reads their service keys.
- `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\HwSchMode`: 1 = hardware GPU
  scheduling off, 2 = on. Two regpatches set it. The 2.70.6 report prints it. It is a
  support workaround, not a product feature. Observed: not set on the test PC.
- Other `GraphicsDrivers` values observed on the test PC: `UnsupportedMonitorModesAllowed`
  = 1 and `PlatformSupportMiracast` = 1 (and `IddCxDebugCtrl` from other indirect-display
  software). Their origin is unknown; neither name was found in the driver, control
  library or service. See open question 9.
- While seats are active the product denies new display-class driver installs (changelog
  2.17). It does this through the Windows device-installation policy
  `HKLM\SOFTWARE\Policies\Microsoft\Windows\DeviceInstall\Restrictions\DenyDeviceClasses`
  (observed: key present and empty while disabled). A GPU driver update would break the
  seat mapping, which is the likely reason it is blocked.

### 3.7 Resolution, refresh rate, orientation

- Old rule (still in the UI help): while the product is enabled the user cannot change
  resolution or connect/disconnect monitors. The user must disable the product, extend the
  desktop to all monitors, set the modes, then enable it again. In practice the modes in
  effect at enable time are kept.
- Later versions added monitor hot-plug (2.27, build 15063+) and synchronisation of video
  modes (2.29). **HYPOTHESIS:** mode changes made inside a seat are applied to that seat's
  sources only and are kept in sync with the saved CCD database.
- The wizard and the report record, per path: resolution, desktop position, refresh rate
  (numerator/denominator), rotation and output technology (internal, DP, HDMI, Miracast,
  indirect wired/virtual, ...). The report prints both the CCD database configuration and
  the active paths.
- Orientation is a normal per-source mode attribute. No product-specific rotation feature
  was found.

### 3.8 Fullscreen games

- Fullscreen game support came in 2.15. Frame-rate improvements for the non-console seats
  came in 2.11-2.14 (changelog).
- **HYPOTHESIS:** each seat's session owns real VidPN sources, so exclusive fullscreen and
  flip presentation work per seat on the physical output.
- Known side effects (changelog and help):
  - a mode switch on a GPU shared with another seat can disturb that seat (hence the
    shared-GPU rules and the mode-sync work);
  - Steam needs a separate per-seat fix (see `80-apps-compat.md`).

### 3.9 Hot-plug and monitor loss

- Monitor hot-plug works from 2.27. The notification mode is set by `conMonNtfy`.
- Fixed in 2.31: switching one monitor off used to blank the other seats. Background:
  Windows re-applies the whole CCD topology on any connect or disconnect. The product has
  to keep the other seats' paths stable when that happens.
- A monitor that appears on an unassigned output is shown in the manager as a new device,
  with a temporary highlight frame (manager setting, 2 to 20 s, default 4).

### 3.10 Indirect, display-only and wireless displays

- USB display adapters, USB A/V adapters (a USB device that carries a display output plus
  audio and input ports) and other display-only devices are supported. They are gated by
  `DODTest`, which has been on by default since 2.17.
- A dedicated wizard handles newly attached USB A/V adapters in four steps:
  1. wait for all adapters to enumerate;
  2. ask the user to extend the desktop onto them and check their resolution in the system
     display settings;
  3. pick a seat for each adapter;
  4. restart the manager.
- Miracast monitors (2.27) and the internal laptop panel output (2.29.1) are supported.
  Touchpads built into USB monitors are supported too (2.24, see `50-input.md`).
- Virtual display adapters from other software, and remote-desktop indirect adapters, show
  up as ordinary adapters (observed in the device inventory). HYPOTHESIS: they can be
  assigned like any other adapter.

### 3.11 Monitor power and the idle screensaver

- The driver has per-seat monitor power values: `EnableMonitorPower`,
  `giPowerOffTimeOutMs`, `EnableAutoSleep`, `PersistPM`, `enablePMSync` (none set on the
  test PC). **HYPOTHESIS:** each seat's monitors power down on that seat's idle timeout,
  independently of the other seats, and the PC is not put to sleep while any seat is active.
- A separate per-seat screensaver (a `.scr` installed in System32) does this:
  - it turns the seat's monitors off through DDC/CI, using the VCP power-mode feature
    through the dxva2 physical-monitor API;
  - if a monitor does not support this, the screen is just black and the user is asked to
    switch the monitor off with its own power button.
  The manager has a per-seat option for it, and a shortcut to the system screensaver
  settings.
- The seat service also uses DDC/CI `SetVCPFeature` (**HYPOTHESIS:** to power monitors on
  when a seat starts or stops).

### 3.12 Wallpaper, lock screen and logo

- Seat options in the manager (from the UI; behaviour not observed):
  - use the product's wallpaper as the desktop background in the seat sessions;
  - use the product's image on the lock and sign-in screen while the product is enabled;
    the UI states that the previous image comes back afterwards (unverified);
  - open the system lock-screen settings.
- The lock-screen image is set machine-wide through
  `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PersonalizationCSP\LockScreenImagePath`
  (REG_SZ). Observed: absent on the test PC with the product disabled.
- Two presets touch this value:
  - the set preset is malformed as shipped: its key path starts with a stray backslash and
    the image path points into the vendor's own build tree. It would not work on a customer
    machine;
  - the remove preset writes an empty string to the value; it does not delete the value.
  **HYPOTHESIS:** the manager writes `LockScreenImagePath` itself at run time, with the path
  of an image in its install folder, and the presets are support leftovers.
- Purpose: a user can see at a glance that a seat was started by the multi-seat software.
- The install folder holds a set of wallpaper images (vendor art, not to be reused).

### 3.13 Identification and first-run wizard (display part)

- Identification: the manager can draw a large seat or display number on every monitor. It
  uses shaped, top-most overlay windows (window regions).
- First-run wizard, display outcome:
  - the current CCD paths are recorded;
  - the desktop is extended to every connected monitor (`SetDisplayConfig`, including
    displays not yet attached to the desktop), and each path's adapter, source, target,
    rotation and refresh rate is logged;
  - the user chooses the monitor of seat 1; the primary display is offered as the default.
    Without any display the wizard cannot continue;
  - one seat is created per monitor;
  - input devices on the same USB hub, or in the same device container, as a monitor go
    into that monitor's seat (for monitors with built-in hubs);
  - the main display and main keyboard/mouse interface paths are saved to a small text file
    in the wizard's data folder.
- While the wizard runs, the other seats show a placeholder screen that says the wizard is
  in progress.

### 3.14 Diagnostic report (display sections)

For every display, the report lists:

- per monitor: adapter number, current source, forced source, target ordinal and output
  type;
- per source: source number, flags and `\\.\DISPLAYn` name;
- the CCD database and active paths (`A.S`, resolution, position, refresh, output type,
  target device path);
- the `\Device\VideoN` to `A.S` table;
- the driver service tuning values;
- in 2.70.6, also `HwSchMode` and the HVCI state.

### 3.15 Error and validation cases

- No display on seat 1: the configuration is rejected.
- A seat with several monitors shares a GPU with another seat: the layout is rejected and
  the user is told why.
- A forced output cannot be re-assigned, a flag write fails, or the device is missing:
  error dialog.
- An experimental display flag cannot be written: error dialog with a code.
- A monitor conflict (2.70.6): it is marked in the device view.
- A seat has no display: the seat is shown red and does not start.
- 2.70.6: the driver cannot work with the active code-integrity protection: error with a
  code (3.4).

### 3.16 Windows mechanisms used by the reference (display)

- CCD: `QueryDisplayConfig`, `GetDisplayConfigBufferSizes`, `DisplayConfigGetDeviceInfo`,
  `SetDisplayConfig`; the CCD persistence key `GraphicsDrivers\Connectivity`.
- GDI: `EnumDisplayDevicesW`, `EnumDisplaySettingsW`, `ChangeDisplaySettingsW`,
  `EnumDisplayMonitors`, `MonitorFromPoint`.
- D3DKMT: `D3DKMTOpenAdapterFromHdc`, `D3DKMTCreateDevice`, `D3DKMTSetVidPnSourceOwner`,
  `D3DKMTGetDeviceState`, `D3DKMTDestroyDevice`, `D3DKMTCloseAdapter`; run-time lookup of
  `D3DKMTDDisplayEnum` / `D3DKMTDDisplayEnum2` from `api-ms-win-dx-d3dkmt-l1-1-4`.
- DDC/CI (dxva2): `GetPhysicalMonitorsFromHMONITOR`, `SetVCPFeature`.
- PnP: Display class `UpperFilters`, device interface paths of the display adapter and
  monitor classes, the `Control\Video` and `DEVICEMAP\VIDEO` registry keys, the
  `DenyDeviceClasses` device-installation policy.
- Kernel: undocumented internals of the graphics kernel and win32k, with in-memory
  modification (HYPOTHESIS).

## 4. Configuration summary (reference)

| Where | Name | Purpose |
| --- | --- | --- |
| `HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}` | `UpperFilters` (REG_MULTI_SZ) | driver added as Display class upper filter while enabled |
| `HKLM\SYSTEM\CurrentControlSet\Control\Video\{VideoID}\000N` | `SSSS_mtxk_terminal`, `TTTTTTTT_mtxk_source`, `SSSS_mtxk_flags` | source to seat, target to source, source flags |
| driver service key | `enableVidPN`, `conMonNtfy`, `checkMonPresence`, `DODTest`, `DXTest`, `AllGPUAccess`, `oldPathsMgmt`, `oldGDCBS`, `ddpcCompBits(X)`, `WDDMGrade`, `usdRefreshMsec`, `EnableMonitorPower`, `giPowerOffTimeOutMs`, `EnableAutoSleep`, `PersistPM`, `enablePMSync`, `vmwHpcMask` | display tuning (3.4, 3.6, 3.11) |
| driver-kept markers (location HYPOTHESIS) | `mtxk_upperfilters`, `mtxk_denydeviceclasses` | record of filters and install blocks it added |
| control library setting (location HYPOTHESIS) | `oldDisplayNums` | legacy adapter numbering |
| `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers` | `HwSchMode` (1/2) | Windows hardware GPU scheduling (support tweak) |
| `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity` | (read) | Windows CCD persistence database |
| `HKLM\SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity` | (read) | HVCI state for the diagnostic report |
| `HKLM\SOFTWARE\Policies\Microsoft\Windows\DeviceInstall\Restrictions\DenyDeviceClasses` | list | blocks display-driver installs while active |
| `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PersonalizationCSP` | `LockScreenImagePath` | lock-screen image |
| configuration export (JSON) | `DeviceAsignment[]` with `Type` = `Display` | source to seat mask |
| manager per-user settings | `displayViewMask`, window `screenName` / `knownScreens` | which display tiles to show; the monitor the manager window sits on |

## 5. ARM64 notes

- The reference has no ARM64 build. Its display isolation lives in an x64 kernel driver
  that modifies graphics-kernel internals. It cannot run on ARM64 at all, and the method is
  not portable even in principle (different kernel binaries, HVCI on by default).
- ARM64 Windows PCs usually have **one integrated GPU**. There are few outputs: the internal
  panel plus one to three USB-C DisplayPort Alt-Mode ports, sometimes DP MST. USB display
  adapters with ARM64 drivers exist. Real multi-GPU is practically absent. Consequences:
  - every seat shares one GPU. The reference's rule that a multi-monitor seat needs its own
    GPU would make multi-monitor seats impossible on ARM64, so Pulsar Seat must not depend
    on per-adapter ownership;
  - MST hubs and USB display adapters are the main ways to get more monitors.
    Display-only and indirect devices must be first class, not an experimental flag.
- Available natively on ARM64:
  - CCD;
  - the D3DKMT user-mode thunks;
  - DXGI and D3D11/12;
  - the IddCx UMDF framework;
  - the WinRT `Windows.Devices.Display.Core` API;
  - dxva2 DDC/CI.
  **HYPOTHESIS:** support for specialized (non-desktop) displays, hardware cursor planes and
  DDC/CI over USB-C depends on the GPU driver. It must be tested on the ARM64 GPU drivers.
- Games in seat sessions may be x64 running under emulation. That does not change the
  display path, but the render GPU is always the integrated one.
- Every Pulsar display component (IddCx driver, presenter, service code) must build for
  ARM64 and x64. Check the PE machine type of each artifact.

## 6. Pulsar Seat approach

Constraints (from `docs/agents/rules.md`): no kernel patching, no vulnerable drivers, works
with HVCI and Secure Boot on, prefer user mode and UMDF, ARM64 first class. Pulsar Seat
installs **no** class filter on the Display class and never modifies graphics-kernel
memory.

### 6.1 Model

- Keep the user-facing model of the reference: a seat owns displays, seat 1 must have one,
  and there is an assign-outputs view.
- The stored identity of a display is **the monitor on a target**, not a source index or a
  `VideoID`. Key by adapter kind:

  | Adapter kind | Adapter part of the key | Target part | Fallback |
  | --- | --- | --- | --- |
  | PCI GPU (integrated or discrete) | PnP location path (stable across driver updates and reboots) | CCD target id | monitor EDID identity |
  | USB display / USB A/V adapter | PnP instance path, including the device serial if it has one (the location path changes with the USB port) | CCD target id | monitor EDID identity; if the instance path has no serial, the adapter's container id plus its hardware id |
  | Root-enumerated or indirect adapter (virtual displays, other indirect drivers) | PnP instance path (as the reference does) | CCD target id | none |

  The monitor EDID identity is manufacturer, product code and serial from the CCD target
  name query. Adapter LUIDs and `VideoID` GUIDs are runtime data only. The reference lost
  its mapping after a GPU driver update; Pulsar must re-resolve instead.
- Do not block display-driver installs. After an adapter reset or a driver update,
  re-resolve the keys above and re-apply the seats.

### 6.2 Seat 1 (console session)

Seat 1 uses its physical monitors through the normal Windows desktop. Pulsar changes only
which monitors stay in the console desktop (CCD `SetDisplayConfig`). Native performance,
exclusive fullscreen, VRR and HDR keep working as on any PC.

### 6.3 Other seats: virtual monitor in the session plus a presenter on the physical output

Scope (`00-architecture.md` §5.2, tension T1): this design applies to backends where the seat
has no display of its own (backend D, and any future local-session backend). Under backend A
(remote-protocol seat sessions) the seat session renders into its remote-protocol display, the
seat host presents those frames on the seat's physical targets, and the IddCx driver is not
used for that seat. Which case ships is decided with the backend.

1. **Virtual monitor per seat.** A Pulsar IddCx driver (UMDF 2, user mode) adds one
   virtual monitor for each additional seat. That monitor belongs to that seat's session
   only. The seat's session renders its desktop onto it, and this is all the session can
   see.
   - **HYPOTHESIS:** IddCx supports monitors that belong to a given session, the way the
     remote-desktop indirect display does. How a local, non-console session gets its own
     indirect monitor is shared with the session work in `30-sessions.md` and with Pulsar
     Split. Decide there and reuse it.
2. **DWM of the seat session.** The seat session's own DWM composes onto the virtual
   monitor. The virtual monitor must stay plugged in for the whole life of the seat session:
   while the seat is signed in, locked, at the sign-in screen, or disconnected. Then DWM and
   the secure desktop of that session always have a target. **HYPOTHESIS:** whether DWM and
   the sign-in screen of a local non-console session actually render on an IddCx monitor
   without a connected client is decided in `30-sessions.md` and must be tested. If a
   session goes away (logoff), the seat shows Pulsar's own idle frame until a new session
   gets the monitor.
3. **Physical monitors leave the console desktop.**
   - The seat's physical monitor is removed from the console desktop.
   - It is acquired by a Pulsar **presenter** through the documented
     `Windows.Devices.Display.Core` API (`DisplayManager`, acquire target, create a scanout
     source and primaries, present per vblank).
   - The presenter copies each frame of the seat's virtual monitor to the physical target.
   - **HYPOTHESIS:** the target must be in specialized display (non-desktop) mode. Windows
     11 has a per-monitor setting that removes a display from the desktop. Whether Pulsar
     can set it programmatically, or must guide the user, is an open question.
4. **Same GPU where possible.** The IddCx adapter's render adapter is set to the GPU that
   drives the seat's physical monitor (`IddCxAdapterSetRenderAdapter`, documented). If
   seats sit on different GPUs, use one IddCx adapter per render GPU.
   **HYPOTHESIS (frame transport):** the IddCx swapchain is processed inside the UMDF host
   process (`WUDFHost`, session 0, service identity), while the presenter runs elsewhere
   (see open question 2). Frames should move between them as cross-process shared GPU
   resources (an NT shared handle of a texture plus a keyed mutex or fence), never through
   system memory. That such handles can be duplicated from the UMDF host to the presenter
   process is not documented and must be proven by a prototype. Fallback: a CPU copy
   through shared memory, at a measured cost.
5. **Cursor.** The IddCx driver sets up the hardware cursor for its monitors
   (`IddCxMonitorSetupHardwareCursor`) and reads the cursor shape and position from IddCx
   (`IddCxMonitorQueryHardwareCursor`) on each change. The presenter composites the cursor
   into the frame it scans out, or uses a cursor plane on the physical target if the API
   and the driver allow it (**HYPOTHESIS**). If hardware cursor setup is refused, IddCx
   falls back to a cursor drawn into the frames by DWM; that is the software cursor mode,
   and it is also what the per-seat cursor option selects. Seat 1 uses the normal Windows
   cursor.
6. **Clone and several monitors on one port.** One seat's virtual monitor may be presented
   on several physical targets: the presenter acquires each target and scans out the same
   frame on each (clone, the analogue of the reference's cloning). Each acquired target
   uses its own mode; the presenter scales if modes differ. A seat that wants an extended
   layout over several physical monitors gets one virtual monitor per physical monitor.
   Monitors behind a DP MST hub are separate targets, so they can be acquired and assigned
   one by one, like any other target (**HYPOTHESIS:** works with specialized-display mode
   on MST; test on docks).
7. **Fallback (documented API only, weaker isolation).** If specialized-display ownership is
   not available on a GPU or driver:
   - the seat's monitor stays in the console desktop;
   - the presenter covers it with a borderless, top-most, fullscreen window;
   - the console user's cursor is kept out of it (`ClipCursor`);
   - Win+arrow and window moves onto it are blocked.
   Mark it as degraded in the UI. Known limitations of the fallback, to state in the UI and
   the docs:
   - the console's lock screen, the UAC secure desktop, the Ctrl+Alt+Del screen and fast
     user switching on the console replace the console desktop, so they cover or hide the
     presenter window and **all fallback seats go blank** while they are shown;
   - the presenter window lives in the console session, so it stops when the console user
     signs out;
   - the console user, and any program in the console session, can capture the seat's
     screen (it is a window on their desktop). The fallback gives no privacy between seat 1
     and the fallback seats.

### 6.4 GPU choice and multi-GPU

- Pulsar provides **no GPU enumeration isolation**. Applications in every seat session see
  every GPU, as on a normal PC. There is no kernel filtering and no hiding of adapters.
- What Pulsar does provide is a **preferred render GPU per seat**: `IddCxAdapterSetRenderAdapter`
  makes DWM and the IddCx swapchain of that seat use the GPU of the seat's physical monitor.
  This choice applies to composition only. Which GPU an application renders on stays the
  application's own choice and Windows' per-app graphics preference.
- Optionally, the manager can write the per-user graphics preference that Windows Settings
  writes (`HKCU\Software\Microsoft\DirectX\UserGpuPreferences`) for the seat user, so that
  chosen applications prefer the seat's GPU. This is a preference, not isolation.
- Several monitors per seat and GPU sharing between seats are both allowed. The
  reference's restriction does not apply, because seats never own VidPN sources.
- `HwSchMode`: do not change it. Report it in diagnostics only. Pulsar does not write
  `UnsupportedMonitorModesAllowed` or `PlatformSupportMiracast`.

### 6.5 Modes, orientation, scaling

- The virtual monitor of a seat advertises the physical monitor's EDID and mode list, so
  the seat's session sees the right name, resolution and refresh rates.
- When the seat changes mode on its virtual monitor, the presenter sets the same mode on
  the physical target if it can. Otherwise it scales, keeping the aspect ratio.
- Rotation chosen in the seat is applied by the presenter when it scans out
  (**HYPOTHESIS:** IddCx passes the rotation to the driver; otherwise the presenter rotates).
- Per-monitor DPI scaling is a normal per-session setting on the virtual monitor.

### 6.6 Fullscreen games (seats 2..N)

- Games present to the virtual monitor. Exclusive fullscreen becomes composed or flip mode
  on an indirect display.
- Expected cost, all **HYPOTHESIS** to measure:
  - about one extra frame of latency (frame copy, then presenter scan-out);
  - no VRR / adaptive sync;
  - HDR only if IddCx and the driver support it.
- HDCP-protected video is not shown on indirect displays. Document this as a known
  limitation.
- The presenter paces to the physical target's vblank. The virtual monitor reports the same
  refresh rate.

### 6.7 Hot-plug

- The presenter listens for display-target changes. When a seat's monitor goes away:
  - the seat's virtual monitor stays, so the session's windows do not move;
  - frames are dropped;
  - the seat is shown as having no display.
- When the monitor comes back (same key, see 6.1), it is acquired again.
- A new, unknown monitor joins the console desktop and appears in the manager as
  unassigned, with a highlight.
- Hot-plug on one seat must never change another seat's paths. Specialized targets are not
  part of the console topology, which is what makes this possible (HYPOTHESIS until tested).

### 6.8 Monitor power and idle

- Each seat's session keeps its own idle and power policy for its virtual monitor. When the
  seat goes idle, the presenter blanks the target and, where supported, powers the monitor
  down with DDC/CI (documented dxva2 VCP power-mode feature). It wakes the monitor again on
  that seat's input.
- No separate screensaver binary is needed. Monitors without DDC/CI get a black frame.
- The PC must not sleep while any seat is active. Use the documented power-request APIs in
  the service.

### 6.9 Wallpaper, lock screen, identification

- Optional per-seat wallpaper uses Pulsar's own images, set inside the seat session through
  the documented user wallpaper API.
- An optional machine-wide lock-screen image uses the documented `PersonalizationCSP` value.
  Save the previous value (including "absent") and restore it exactly on disable: delete
  the value if it was absent, do not leave an empty string. **HYPOTHESIS:** this value is
  honoured only on some editions; check on Pro.
- Identify: in the console desktop, an overlay window per monitor. On specialized targets,
  the presenter draws the number itself. All text is Turkish and all art is Pulsar's own.

### 6.10 Diagnostics

The diagnostic report lists, per display endpoint:

- the stable key, the current adapter LUID, the target id, the output technology, the mode,
  the rotation and the owning seat;
- whether it is presented natively (seat 1), through the presenter (specialized target) or
  in fallback mode; the cursor mode (hardware or software);
- the GPU driver version, `HwSchMode`, the HVCI and Secure Boot state, and the IddCx
  version.

### 6.11 Signing

The IddCx driver is a UMDF driver package. It needs a signature Windows accepts with Secure
Boot on (attestation signing). Test-signing is for development only. The reference ships an
attestation-signed driver build in 2.70.6, which shows the path is open for a seat product;
Pulsar's driver, unlike the reference's, also has to pass HVCI because it runs in user mode.
See `90-install.md`.

## 7. Open questions

1. Can a local non-console session get its own IddCx monitor through a documented path, the
   way the remote-desktop indirect display does? Should this be shared with Pulsar Split?
   (Decide in `30-sessions.md`.) Do DWM and the sign-in / lock screen of such a session
   render on it while no client is connected?
2. Where does the presenter live? Options: a process in session 0 under the service, a
   process in the console session, or a process in each seat session. Does
   `Windows.Devices.Display.Core` target acquisition work from session 0 at all? What
   happens to an acquired target, and to the presenter, when the console user signs out
   or switches user? Can a shared GPU texture handle be passed from the UMDF host process
   to that presenter?
3. Can Pulsar mark a physical monitor as a specialized (non-desktop) display without user
   interaction?
4. Do the ARM64 GPU drivers support specialized-display scan-out, cursor planes and DDC/CI
   over USB-C? Do they support DP MST on common docks, and does specialized mode work for
   MST targets?
5. What is the measured latency and frame pacing of the virtual monitor to presenter path
   at 60/120/144 Hz? Is VRR possible?
6. Reference: what do `conMonNtfy` = 2, `checkMonPresence`, `ddpcCompBits(X)`, `WDDMGrade`,
   `usdRefreshMsec` and `vmwHpcMask` do exactly? What do the bits of the per-source flags
   value mean (clone or hardware cursor)? Where is `oldDisplayNums` stored? Needs a run with
   the product enabled; that is a system change on the test PC and needs the maintainer's
   approval.
7. Reference, while running: does a seat session really see only its own monitors and only
   its own GPUs? Can a seat change its resolution while enabled (the 2.29 mode sync)? Does
   the lock-screen option restore the previous image? Observe with the product enabled.
8. Does `PersonalizationCSP\LockScreenImagePath` take effect on Windows 11 Pro, or only on
   Enterprise/Education?
9. Does the reference write `GraphicsDrivers\UnsupportedMonitorModesAllowed` and
   `PlatformSupportMiracast` (both 1 on the test PC), or did other software set them?
   (The manager executable is packed and was not searched.)
10. Should Pulsar offer a native mode for seats 2..N on x64 multi-GPU PCs (a seat owns a
    whole GPU) through any documented mechanism, or is the virtual-monitor path the only
    one? No documented API gives a non-console session physical VidPN sources today
    (HYPOTHESIS).
