# Pulsar Seat — troubleshooting and recovery

> Turkish copy: [troubleshooting.tr.md](troubleshooting.tr.md). Names in quotes are the
> English labels of the manager; with the Turkish locale the manager shows the Turkish ones.

This page is for users. Pulsar Seat's own drivers (keyboard/mouse filter, virtual display,
USB storage gate) **do not load until they are signed**; with today's version these steps
are normally not needed. Once the drivers are signed and installed, the steps here bring
the PC back to its old state if something goes wrong.

Names (all of them belong to Pulsar Seat):

| Part | Service name | Driver package (INF) | File |
| --- | --- | --- | --- |
| Keyboard / mouse / HID filter | `PulsarSeatInput` | `seat-input-filter.inf` | `seatinput.sys` |
| USB storage gate (minifilter) | `PulsarSeatStorageGate` | `seat-storage-filter.inf` | `pulsarseatstoragegate.sys` |
| Virtual display (IddCx) | device `Root\PulsarSeatIdd` (one `SWD\PulsarSeatIdd\gpuN` per extra graphics card; it goes away when the service stops) | `seat-idd.inf` | `seat-idd.dll` |
| Pulsar Seat service | `PulsarSeat` | — | `pulsar-seat-service.exe` |
| Sign-in component (credential provider + filter) | — | — | `pulsar-seat-credprov.dll` |

Windows names the package in the driver store `oem<N>.inf` (`N` differs on every PC).

Known limits (display):

- Copy-protected (HDCP) content is not shown on seat 2 and later (for example some film
  apps show a black screen). This is not a fault; the picture of these seats comes through
  the virtual display and Pulsar Seat's own presentation.
- HDR and variable refresh (VRR) only work when they are turned on for the monitor in the
  "Display outputs" window and the monitor supports them. If the seat's monitor was not
  taken off the desktop (fallback presentation), there is no HDR and the picture is shown
  as SDR.
- The graphics card preference uses the entry of the Windows graphics settings; it is
  written when the seat user signs in and undone when Pulsar Seat is turned off. If the user
  changes the same program in the Windows settings, their choice is kept.

## 1. Windows starts: turn Pulsar Seat off first

1. In the manager, **turn Pulsar Seat off** (main switch). All devices go back to seat 1,
   the extra seats close, optional system settings are undone.
2. If that does not work, in a command prompt opened as administrator:

   ```bat
   "C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
   sc stop PulsarSeat
   ```

## 2. Windows starts: remove the drivers (pnputil)

In an administrator command prompt:

```bat
pnputil /enum-drivers
```

Note the **Published Name** (`oem<N>.inf`) of the rows whose **Original Name** is
`seat-input-filter.inf`, `seat-storage-filter.inf` or `seat-idd.inf`, and for each one:

```bat
pnputil /delete-driver oem<N>.inf /uninstall
```

`/uninstall` removes the filter entry only from the devices Pulsar Seat added it to; it does
not touch the filters of other software. Then restart the PC.

## 3. The keyboard or mouse does not work but Windows starts

The keyboard/mouse filter has a **pass-through** switch: while it is on, the filter routes
nothing and every key and mouse movement goes its normal way.

```bat
reg add HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatInput\Parameters /v PassThrough /t REG_DWORD /d 1 /f
```

Restart. The on-screen keyboard (Accessibility > On-Screen Keyboard) or a remote desktop can
be used to type this command. In Safe Mode the filter passes everything anyway.

## 4. Windows does not start: Windows Recovery Environment (WinRE)

If startup fails twice, Windows opens the recovery environment by itself. Or start from the
installation USB stick and choose **Repair your computer**. Then:
**Troubleshoot > Advanced options > Command Prompt**.

In the recovery environment the letter of the Windows partition is often not `C:`. Find it
first:

```bat
dir C:\Windows
dir D:\Windows
```

Below, the Windows partition is written as `D:`; replace it with your own letter.

### 4a. Recommended way: put the filter into pass-through

```bat
reg load HKLM\OFFSYS D:\Windows\System32\config\SYSTEM
reg query HKLM\OFFSYS\Select /v Current
```

The `Current` value tells which control set is used (`0x1` → `ControlSet001`).

```bat
reg add HKLM\OFFSYS\ControlSet001\Services\PulsarSeatInput\Parameters /v PassThrough /t REG_DWORD /d 1 /f
reg add HKLM\OFFSYS\ControlSet001\Services\PulsarSeatStorageGate /v Start /t REG_DWORD /d 4 /f
reg unload HKLM\OFFSYS
```

Close the command prompt and start Windows with **Continue**. Then remove the drivers as in
step 2.

**Warning:** do **not** write `Start=4` (disabled) for `PulsarSeatInput`. This filter is in
the keyboard and mouse devices' own `UpperFilters` list; a filter that cannot load stops
those devices completely. The safe way to turn the keyboard filter off offline is
`PassThrough=1` or removing the package as below. The storage gate
(`PulsarSeatStorageGate`) is a minifilter; `Start=4` is safe for it.

### 4b. Remove the packages offline (dism)

```bat
dism /image:D:\ /get-drivers /format:table
```

Find the published name (`oem<N>.inf`) of the rows with `seat-input-filter.inf`,
`seat-storage-filter.inf` or `seat-idd.inf`, and for each one:

```bat
dism /image:D:\ /remove-driver /driver:oem<N>.inf
```

When removing offline, Windows cannot always clean the devices' filter lists. So do the
`PassThrough=1` step of 4a too before removing with `dism`. Once Windows starts, the
`pnputil /delete-driver … /uninstall` command of step 2 cleans up the rest.

### 4c. Last resort: deleting the filter entry by hand

Only if 4a and 4b do not help. In the offline registry (`reg load`, as above) look at the
`UpperFilters` values of the keyboard and mouse devices under
`HKLM\OFFSYS\ControlSet001\Enum\`. Delete **only** the `PulsarSeatInput` line from the value;
do not touch `kbdclass`, `mouclass` or other lines. `regedit` also works in the recovery
environment: **File > Load Hive** opens `D:\Windows\System32\config\SYSTEM`. When done,
close the hive with **Unload Hive**.

## 5. Extra seats do not start

- Create a **Diagnostic report** in the manager. The report is written to
  `%ProgramData%\Pulsar Seat\diag\`; it contains no passwords or user names.
- Service log: `%ProgramData%\Pulsar Seat\logs\service.log`.
- After a Windows update the experimental client session module may turn itself off; the
  seats then fall back to fast user switching.
- With the fast user switching method, seats do not start by themselves when the PC starts
  (every start moves the console to that seat). Start the seat from the manager.
- For safety, the seat host (`pulsar-seat-host.exe`) runs on every seat as a separate local
  account of that seat (**`PulsarSeatHost02`**, **`PulsarSeatHost03`** …) and at low
  integrity level, so one seat's host cannot reach another one's. These accounts are members
  of the local group **`PulsarSeatHosts`**. When a seat is deleted, its account is deleted
  at the next service start; when Pulsar Seat is uninstalled, all of them are. If the seat's
  account cannot be set up, that seat runs as the shared **`PulsarSeatHost`** account (the
  local connection listener uses this shared account too). If printer redirection is on for
  the seat, the account's user profile is loaded while the seat host runs (a folder such as
  `C:\Users\PulsarSeatHost02` appears). The service creates the account itself and renews
  its password at random at every start (the password lives only in Windows' protected LSA
  store; nobody types it). Nobody can sign in to Windows with this account or connect with
  it over the network; it does not show on the sign-in screen either. Do not delete it: if
  it is deleted, the service creates it again at the next start. If the account is added to
  any local group other than `Users` (such as `Administrators`, `Backup Operators`,
  `Remote Desktop Users`; directly or through a domain group), or its groups cannot be read,
  the service does not use it. In that case remove the account from those groups and
  restart the service.
- **Clipboard sharing:** a seat host running as this account reaches the clipboard only
  when clipboard sharing is on in the seat's settings. Restart the seat after changing the
  setting; the change takes effect when the seat host restarts.
- If a seat's own account cannot be set up, or the seat host cannot reach the desktop or the
  service with it, that seat falls back to the shared `PulsarSeatHost` account for this run
  of the service. If the shared account cannot be set up or cannot reach them either, the
  service falls back to the earlier setting (SYSTEM, low integrity level). Both are written
  to `service.log`.
- If a group or user named `PulsarSeatHosts` exists that Pulsar did not create, per-seat
  accounts stay off and all seats use the shared account; rename it and restart the service.
- Seats that open at the sign-in screen (without an account) connect through the port for
  seats without a password. Only accounts given to seats and the **extra accounts** of the
  Advanced tab can use this port. With no account at all these seats cannot start; the
  manager shows this as a warning on the seat. Enter the people who will sign in on these
  seats as extra accounts, or give the seat an account.
- If a seat still never connects on a PC, set one of the values below in an administrator
  command prompt and restart the service:

  ```
  reg add HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters /v HostTokenCompat /t REG_DWORD /d 2 /f
  ```

  - `3`: no per-seat accounts; all seat hosts run as the shared `PulsarSeatHost` account
    (the behaviour of earlier versions).
  - `2`: no separate account; the seat host runs as SYSTEM at low integrity level.
  - `1`: the oldest setting (SYSTEM, unprivileged, system integrity level). Use it only if
    `2` does not help either.

  To undo, delete the same value (`reg delete … /v HostTokenCompat /f`) and report the
  problem.

## 5a. What do the core limit and the source IP address cover?

- **Processor cores given to a seat** apply only to programs Pulsar Seat itself starts on
  that seat: the seat's startup apps and programs opened from the Pulsar Seat launcher (and
  the programs they open).
- **Not limited:** programs opened from the desktop, the Start menu or File Explorer;
  programs run as administrator (with UAC approval).
- When you change the core setting, a running seat takes the new setting at once. Removing
  a hard limit only takes effect once that seat signs out and in again.
- **The seat's source IP address** is enforced when Pulsar Seat's source IP driver is loaded:
  the IPv4 connections of the programs in the seat session leave from the seat's address (the
  Network addresses tab says "Source IP driver loaded"). The driver does not load until it
  has a Microsoft signature; until then only programs that have a "bind to address" option
  in their settings or read the `PULSAR_SEAT_SOURCE_IP` environment variable can use this
  address. The variable is given to programs Pulsar Seat starts; programs those open
  inherit the variable too, whatever the rule.
- Left untouched even with the driver loaded: an address the program picks itself,
  connections inside this PC (127.x), IPv6 connections and kiosk seats that share their
  session with other seats.
- If the driver breaks network connections: set the `PassThrough` value under
  `HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatWfp\Parameters` to 1 and restart the PC
  (in Safe Mode the driver does nothing anyway). To remove it: find the `oem<N>.inf` name of
  the `seat-wfp.inf` package with `pnputil /enum-drivers`, then `sc stop PulsarSeatWfp` and
  `pnputil /delete-driver oem<N>.inf /uninstall`.

## 5b. Separate port for seats without a password (Windows Server)

For Remote Desktop seats that have no saved password (they start at the sign-in screen),
Pulsar Seat sets up a second Remote Desktop listener:

- Registry key:
  `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Loopback`
- Port: **33389** by default (can be changed in the Advanced tab).
- Firewall rule (inbound, block): **`Pulsar Seat - loopback RDP (block remote) <port>`**,
  for example `Pulsar Seat - loopback RDP (block remote) 33389`. Older versions used only the
  name `Pulsar Seat - loopback RDP (block remote)`.

Network level authentication is off on this listener; only this firewall rule keeps it from
being reached from outside the PC. So if Windows Firewall is off in a profile, or a group
policy ignores local rules, Pulsar Seat removes the listener. Remote Desktop Services cannot
restart while Remote Desktop seats are open; a removed listener keeps running until then.
The manager shows this: close the Remote Desktop seats or restart the PC.

If the key or the rule is left after Pulsar Seat was uninstalled, in an administrator
command prompt:

```
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Loopback" /f
netsh advfirewall firewall delete rule name="Pulsar Seat - loopback RDP (block remote) 33389"
netsh advfirewall firewall delete rule name="Pulsar Seat - loopback RDP (block remote)"
```

If you changed the port, write that number instead of `33389` in the rule name. To see the
remaining rules: `netsh advfirewall firewall show rule name=all | findstr /C:"Pulsar Seat"`.
**Delete the key first, then restart the PC, delete the rule last**: if the rule is deleted
while the listener still runs, the port stays open to the network.

If Windows does not start (recovery environment,
`reg load HKLM\OFFSYS D:\Windows\System32\config\SYSTEM`): delete the
`HKLM\OFFSYS\ControlSet001\Control\Terminal Server\WinStations\PulsarSeat-Loopback` key and
close it with `reg unload HKLM\OFFSYS`. The firewall rule is deleted with the `netsh`
command above once Windows starts.

## 6. Removing Pulsar Seat completely

Uninstall it from **Settings > Apps**. The uninstall undoes the service, the driver
packages, the saved seat passwords, the seat host's `PulsarSeatHost` account (with its
sign-in rights and password) and the system settings Pulsar Seat made (lock screen picture,
hiding fast user switching, leaving driver updates out, power settings). Access permissions
given to devices, device classes and printers are restored too; if something cannot be
undone, the uninstall prints it. You can also choose during the uninstall to delete your own
data.

The uninstall stops the service before deleting the account and revokes the desktop
permissions given to the seat hosts in open sessions. If the service cannot be stopped, the
account is not deleted; stop the service and run the uninstall again. A desktop permission
that could not be revoked goes by itself when that session closes.

## 7. No tiles or wrong tiles on the sign-in screen (credential provider)

In the "one seat at a time" (fast user switching) mode, Pulsar Seat adds a small component
to the sign-in screen (`pulsar-seat-credprov.dll`) to sign in by itself with the seat's
saved password. This component is registered **only** while Pulsar Seat is on and this mode
is used; turning Pulsar Seat off or uninstalling it deletes the registration.

The component keeps itself on the safe side: if the Pulsar Seat service does not answer, it
hides no tile and does not show itself either. It hides the other sign-in options
(password, PIN) only while a seat signs in by itself, and only in that seat's session. It
never signs in by itself on the lock screen.

**If the saved password is not accepted** ("The saved password was not accepted. Enter the
password."): Pulsar Seat does not try the password again. PIN, Windows Hello or "Other
user" tiles may stay hidden on the same screen. Type the seat account's password, use the
"Switch seat" link, or press Ctrl+Alt+Del (or lock the screen) to open a new sign-in screen:
on the new screen all tiles show again. Enter the saved password again in the Pulsar Seat
manager.

If there is still no tile at all on the sign-in screen, or the Pulsar Seat tile hangs:

### 7a. Windows starts and you can sign in with another account

In a command prompt opened as administrator:

```bat
"C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
```

If the component should stay off for good (even while Pulsar Seat is on):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v CredProvDisabled /t REG_DWORD /d 1 /f
sc stop PulsarSeat
sc start PulsarSeat
```

To undo, delete the same value
(`reg delete "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v CredProvDisabled /f`).

### 7b. Safe mode

In Safe Mode the Pulsar Seat service does not run; since the component cannot reach the
service, it hides nothing and does not show. The normal Windows sign-in screen comes up.
Run the commands of 7a from there. (To get to Safe Mode: on the sign-in screen hold
**Shift** and choose **Restart**, then **Troubleshoot > Advanced options > Startup
Settings**.)

### 7c. Deleting the registration by hand (running Windows)

In an administrator command prompt delete only these four keys that belong to Pulsar Seat;
do not touch other keys:

```bat
reg delete "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg delete "HKLM\SOFTWARE\Classes\CLSID\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\SOFTWARE\Classes\CLSID\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
```

### 7d. If you cannot get into Windows at all: from the recovery environment (WinRE)

Open the command prompt in the recovery environment as in §4 and find the Windows
partition (`D:` below). Load the SOFTWARE hive of the registry, delete only the Pulsar Seat
keys, then unload the hive:

```bat
reg load HKLM\OFFSOFT D:\Windows\System32\config\SOFTWARE
reg delete "HKLM\OFFSOFT\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\OFFSOFT\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg delete "HKLM\OFFSOFT\Classes\CLSID\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\OFFSOFT\Classes\CLSID\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg unload HKLM\OFFSOFT
```

If a key says "not found", that is fine; that part is not there. Then restart the PC. Once
the registration is deleted, Windows' own sign-in screen comes back complete.

## 8. Broken, frozen or black picture (H.264)

The seat's picture comes from the server through the graphics pipeline (EGFX); mostly it is
encoded as H.264 and decoded on the graphics card. With some drivers this path can give
broken colours, frozen areas or a black screen. The seat host (`pulsar-seat-host.exe`) reads
the environment variables below. Set them as **system environment variables**
(administrator command prompt), then restart the seat from the manager (the value is read
every time the seat host starts). Try the first one first; if that is not enough, add the
second:

```
setx /M PULSAR_SEAT_H264_DECODER software
setx /M PULSAR_SEAT_GFX no-h264
```

- `PULSAR_SEAT_H264_DECODER=software`: H.264 is decoded on the processor instead of the
  graphics card. Try it first; if the picture gets fixed, the problem is in the graphics
  driver.
- `PULSAR_SEAT_GFX=no-h264`: the graphics pipeline stays but H.264 is not requested
  (RemoteFX and similar codecs are used).
- `PULSAR_SEAT_GFX=bitmap` (or `off`): the graphics pipeline does not open at all; the old
  bitmap updates are used. The slowest but most compatible way.
- `PULSAR_SEAT_GFX_ZERO_COPY=0`: the picture decoded on the graphics card is given straight
  to the screen without ever being copied to the processor (the "zero-copy" path). If the
  picture breaks only while this path is on (wrong colours, flickering or stale areas, a
  pointer trail), turn the path off with this variable; H.264 is still decoded on the
  graphics card and the picture comes through the processor as before.
- To undo, delete the variable
  (`reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v PULSAR_SEAT_GFX /f`)
  and restart the seat.

In the seat host log (`%ProgramData%\Pulsar Seat\logs\host-NN.log`, NN = seat number, for
example `host-02.log`) look for this line:

```
seat host: gfx avc444 v0xa0701 decoder=hardware frames=… h264=… errors=… pic_dropped=… dropped=… resyncs=… fallbacks=… …
```

- The first word is the path in use: `avc444` / `avc420` (H.264), `egfx` (graphics pipeline
  without H.264), `bitmap` (no graphics pipeline).
- `decoder=`: `hardware` (graphics card), `software` (processor), `unavailable` (the H.264
  decoder did not start), `none` (no H.264 yet).
- `gpu=`: the zero-copy path. `ready` (on), `off` (not available or off for this output),
  `failed` (did not start, or failed while running; the processor path for this
  connection). `gpu_frames` is the number of frames from the graphics card to the screen;
  if `demotions` keeps growing, the server sends things this path does not support (not a
  problem, the picture comes through the processor path).
- If `present_skipped` and `acks_held` grow, the screen cannot keep up; the seat host then
  merges frames and asks the server to slow down. If `acks_forced` keeps growing, the
  screen hangs for a long time; add the log to the problem report.
- If `errors`, `pic_dropped` or `dropped` grow, part of the picture could not be drawn; the
  seat host then asks the server for the screen again (`resyncs`). If `fallbacks` is above
  0, decoding on the graphics card kept failing and it switched to the processor while
  running.
- If the numbers keep growing and the picture does not get better, try the settings above
  and add the log to the problem report.

## 9. Kiosk seats: the mouse is locked to an area, a window does not come to the front

While kiosk seats run (several seats in the same Windows session) and seat 1 is a kiosk
seat too, Pulsar Seat locks the system pointer to a "focus area" and pins the foreground
window. If the kiosk shell crashes or the service does not answer, the mouse may stay in
this area or Windows windows may not come to the front.

1. Press **Ctrl+Alt+Del** on the console keyboard, then choose **Cancel**. Windows removes
   the pointer lock at that moment.
2. To get back to the home screen on a kiosk seat, use the button on the bottom bar or
   **Ctrl+Alt+Home** (Ctrl+Alt+Del affects all seats on a kiosk seat).
3. If the problem stays, turn the kiosk seats off in an administrator command prompt; when
   the service stops, all kiosk shells and the apps in them close:

```
"C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
sc stop PulsarSeat
```

4. If the lock is still there, sign out and in again; the pointer lock and the foreground
   lock end with the session.

Known limits: games that use raw input, DirectInput or XInput get the seat's own devices
only when per-app isolation is on. For full-screen games choose "Full screen" on the kiosk
app; only one game on the PC can be in true full screen at a time. Store (UWP / MSIX) apps
are added by their app ID (AUMID) and may not open on every Windows version. Without the
input filter, Pulsar tries to tell kiosk keyboards apart by device and keep their system
keys away from seat 1; moving a kiosk mouse can still move seat 1's pointer (the manager's
"Input is not fully isolated" note). Ctrl+Alt+Del and Win+L cannot be blocked by any
user-mode program.

## 9a. A kiosk seat does not open as its own user / old settings are gone

Since 2026-11-17, kiosk seats 2 and later run as their own Windows user: the account saved
for the seat if it has an account and password, otherwise a hidden local account Pulsar
creates (`PulsarKiosk02` …). These accounts do not show on the sign-in screen and cannot
sign in over Remote Desktop or the network.

1. Before the update the kiosk apps' settings and data were in seat 1's user profile; the new
   seat user's profile starts empty. This data is not moved; if needed, use the app's own
   export / import feature.
2. If the manager shows the note "A kiosk seat's own user could not be opened", that seat
   runs as seat 1's user. Look for the `could not be logged on` line in the service log
   (`logs\service.log`): if the saved password is wrong, enter the seat's account and
   password again; if an account named `PulsarKiosk..` exists that Pulsar did not create,
   rename it.
3. To go back to the old behaviour, turn off "Each kiosk seat runs as its own Windows user"
   in the Basic tab. When a seat stops being a kiosk seat, Pulsar's account of that seat
   is only **disabled** at the next service start; its profile (saves, browser profiles)
   stays. To delete it for good use the seat menu "Delete the seat's kiosk user…". At
   uninstall the accounts are removed; their profiles only with "delete settings and logs
   too" (`REMOVEUSERDATA=1`). Turning the option off deletes nothing.
4. Kiosk seats are not a security boundary: every kiosk seat shares one Windows desktop,
   so a program on one seat can read the keyboard, screen and clipboard of the others
   (Pulsar's own per-seat clipboard covers apps that use the normal clipboard calls). Do
   not sign a personal or work account in to a kiosk seat; the manager warns when a
   seat's saved account is used.

## 10. A kiosk seat's sound does not come from its own device

If a kiosk seat was given a sound device assigned only to it (speaker, headphones,
microphone), the kiosk shell gives every app of the seat this device as its "per-app
default device" (Windows 10 1803 and later). In the manager, the "Sound:" line under the
Kiosk column shows the seat's device.

1. If it says "no separate sound device given to the seat", assign the sound device to this
   seat only (a device assigned to several seats does not count).
2. If the seat's bottom bar says "no per-app sound device" or "could not be routed", Windows
   did not accept this setting; sound comes from seat 1's default device. This setting is
   made through an undocumented Windows interface and a Windows update can break it.
3. If an app's sound still plays from the old device, restart the app with "Sign out": some
   apps use the new device only for sound they open afresh.
4. Do not run the same program on two kiosk seats: Windows stores this setting per program,
   so two seats share the same device (the manager warns about this).
5. If an old entry is left on the Windows Settings > System > Sound > Volume mixer (app
   volume and device preferences) page, you can clear it there with "Reset".

## 11. Language

The manager follows the Windows display language (English, or Turkish on a Turkish
Windows). To choose it by hand: **Basic settings > Appearance > Language**; the manager
reloads in the chosen language. The choice is per user. Texts that the service or the
installer writes (Event Log, tray menu of the seat agent, kiosk bar, sign-in tiles, the
service description) follow the Windows UI language of the process that writes them
(HYPOTHESIS: for the service, running as LocalSystem, this is the PC's default UI
language). To force one language for these, set the system environment variable
`PULSAR_SEAT_LANG` to `en` or `tr` and restart the PC (services read system environment
variables only at Windows start). The MSI is English; a Turkish
MSI can be built with `installer\scripts\build-installer.ps1 -Language tr-TR`.

## 12. Remote seat: "Pulsar display not used", or the mode / HDR does not change

Remote seats use the Pulsar virtual display (with mode and HDR control) only when
**Settings > Virtual display method** is "Pulsar virtual display driver". Pulsar Seat then
registers its own Remote Desktop protocol provider (a listener named `PulsarSeat-Idd`; the
standard `RDP-Tcp` listener is not changed). If that path cannot be used, the seat still
starts, on the Remote Desktop display, and the seat page shows why under "Own display not
used":

- *registered; restart the Remote Desktop service or the PC*: Windows loads a new listener
  only when the Remote Desktop service starts. Stop the seats, then restart the PC.
- *the provider file is missing or for another processor*: repair the installation.
- *the Pulsar display driver is not installed*: the driver is not signed yet (Windows
  Server needs a WHQL-signed driver); the seat keeps running on the Remote Desktop display.

On the Remote Desktop display the mode follows the client; "Display mode now" answers
"this display cannot change mode".

To keep the provider off whatever the settings say (for example if the Remote Desktop
service misbehaves after it was registered):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v WrdsDisabled /t REG_DWORD /d 1 /f
sc stop PulsarSeat
sc start PulsarSeat
```

The service then removes its listener and COM registration at start. To undo, delete the
value and restart the service. Uninstalling Pulsar Seat removes both as well.
