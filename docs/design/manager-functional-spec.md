# Pulsar Seat manager: functional specification (inventory of today's app)

Status: generated 2026-11-25 from the code on `main` (`apps/manager`). This document lists what the manager does today: areas, screens, dialogs, controls, data, rules, states and messages. It gives no visual or layout direction. Where today's grouping of controls is mentioned, it is a fact about the current build, not a requirement. Items marked "unclear in code" could not be confirmed from the source; they are collected in the appendix.

Conventions: file names refer to `apps/manager/src/...` (`*.svelte`, `*.ts`) or `apps/manager/src-tauri/src/*.rs`. String keys refer to `src/lib/locales/en.ts`: `t.<ns>.<key>` for the main object, `rt.<ns>.<key>` for the remote-seat object; Turkish (`tr.ts`) has the same keys. Command names in backticks (`set_config`, `get_backends`, ...) are service pipe requests or Tauri commands.

## Contents

1. Product summary and users
2. Global behaviour
3. Screens, pages and dialogs
4. Data model
5. Live and real-time behaviour
6. Platform variants
7. Accessibility and i18n
8. Coverage check
Appendix: open questions (unclear in code)

## 1. Product summary and users

Pulsar Seat turns one Windows PC (x64 or ARM64) into several independent workplaces ("seats"), each running its own Windows session. A **local** seat gets its own monitors, keyboard, mouse, audio and USB ports on the same PC; a **remote** seat gets a virtual display and virtual gamepads and is streamed to a client device (Moonlight-compatible clients via Sunshine, or Pulsar clients). The manager is the desktop app (Tauri 2 + SvelteKit, English and Turkish) used to set up seats, assign devices, choose backends and options, start/stop seats, pair remote clients and diagnose problems. It does no privileged work itself: every change is a request to the Pulsar Seat Windows service over a named pipe.

Users:

- **PC owner / administrator**: the only intended user. Sets up seats, assigns hardware, changes settings, runs the wizard, pairs clients, reads status and diagnostics.
- **Seat users** (people sitting at a seat or streaming into one) do not use the manager; they only use their Windows session.
- **Admin rights**: changes require an elevated administrator. The manager starts unelevated (as the invoking user, in every session); unelevated it shows a read-only view with a banner (`t.notAdmin` "Read-only view: to make changes you must be an elevated administrator.") and offers "Restart as administrator" (`t.restartElevated`). An administrator can also configure the PC so that only administrators can see the settings at all (restricted mode, `t.restricted`). The remote-seat area is admin-only entirely. Details in section 2.

## 2. Global behaviour

Source: part A (`App.svelte`, `store.svelte.ts`, `api.ts`, `src-tauri/src/*.rs`, `src/lib/ui/*`).

### Entry, routes and windows
- `+layout.ts`: `ssr = false`, `prerender = false` (client-only SPA). `+layout.svelte` only loads `app.css` and renders the child.
- `+page.svelte` is the single entry. Start-up order:
  1. Pick the API: real service API inside Tauri (`createTauriApi`), the in-memory demo service in a plain browser (`createDemoApi`).
  2. Language: inside Tauri, ask the shell for the Windows UI language (`ui_language`), then `initLocale` (stored user choice wins), then tell the shell (`set_ui_language`).
  3. If this window is a wizard page window (Tauri: `wizard_page_monitor` returns a monitor key; browser: `?wizard-page=<monitor key>`), render only `WizardPage` (owned by another part). Browser-only extras: `&wizard-step=pick|keyboard`, `&wizard-seat1=<key>`.
  4. Otherwise create `ManagerStore`, render `App`, run `store.init()`.
  5. Until then the window shows `t.loading` ("Loading…").
- `src/routes/ui/+page.svelte` (`/ui`): dev-only gallery of the component kit (`src/lib/ui/`) for visual review. Query: `?theme=dark|light`, `?lang=tr|en`, `?show=modal|confirm|menu|pick`. Production builds render nothing there (`dev` guard). Not a user screen.
- Tauri windows: `main` is created in code (`lib.rs build_main_window`): title `copy::WINDOW_TITLE` ("Pulsar Seat Manager" / "Pulsar Seat Yönetici"), initial inner size 1000×700, minimum 720×520. It starts hidden when launched with `--tray`, `--autostart` or `-autostart`. Wizard page windows are labelled `wizard-page-*`. Capabilities (`capabilities/default.json`): windows `main`, `wizard-page-*`; permission `core:default` only.
- CSP (`tauri.conf.json`): `default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src ipc: http://ipc.localhost`.

### Navigation areas (views)
- `Tab` type (`prefs.ts`): `seats | remote | settings | advanced | status`. `advanced` is a sub-page of Settings, not a rail item.
- Rail places (`Rail.svelte`): Seats (`t.nav.seats`, count = local seats), Remote seats (`t.nav.remote`, count = remote seats), Settings (`t.nav.settings`, no count); Status (`t.nav.status`) at the rail foot with a service state dot. The brand ("Pulsar Seat") also navigates to Seats.
- The current view is persisted per user in prefs (`prefs.tab`, localStorage). Legacy stored values `basic` and `network` map to `settings` and `advanced`; unknown values fall back to `seats`.
- Switching views (`App.navigate`): runs the unsaved-changes guard (see "Save model"), except Settings <-> Advanced (same draft, no question). Leaving a view resets the remote seat pre-selection.
- Cross-view navigation from pages: Seats board "edit" on a remote seat opens Remote seats at that seat (`openRemote`, guarded); Settings "Advanced settings" link opens Advanced, optionally scrolled to a section; IP dialog "open program list" opens Advanced section `net`; Advanced back -> Settings; Remote tab "open settings" -> Settings; Enable dialog "Change" (start mode) -> Settings.
- There is no URL routing between views: rail links have `href="#<tab>"`, but a plain left click navigates in place. Ctrl/Shift/Alt/Meta-click or non-left buttons are not intercepted.

### Service connection states
Three independent sources:
1. **SCM state** (`service_state` Tauri command, `scm.rs`), polled; type `ServiceInfo {state, elevated}`. `state` values (`rt.serviceState`): `not_installed` "Not installed", `stopped` "Stopped", `running` "Running", `pending` "Changing" (SCM start/stop/continue/pause pending), `unknown` "Unknown". Works while the pipe is down. `elevated` = this manager process is elevated.
2. **Request reachability** (`store.serviceDown`): set when `hello` / `get_status` fail with `service_unavailable`, or after a service stop leaves the SCM state `stopped`. Cleared on a successful `hello` / `get_status`.
3. **Event link** (`store.link`): the shell's subscription connection (`service-link` event + `service_link` command). Topics `status`, `devices`, `activity`, `wizard`.
- Rail dot (`App.serviceLink`): `down` if `serviceDown`; else `ok` if `link`; else `link`. Screen-reader text: `t.nav.statusOk` ": service running", `t.nav.statusDown` ": service unreachable", `t.nav.statusLink` ": reconnecting to the service".
- Not installed: SCM `not_installed`; pipe requests fail `service_unavailable` (SCM error 1060 maps to `service_unavailable` "service is not installed"). The service-down screen shows; Start is disabled (`t.svc.notInstalled` "The service is not installed; reinstall Pulsar Seat.").
- Stopped / unreachable: service-down screen on every view except Status.
- Starting/stopping (manager-initiated): `store.serviceAction` = `start|stop|restart`; progress texts `t.svc.starting/stopping/restarting`. The SCM call returns when the target state is reached (timeout 60 s, `scm.rs WAIT`).
- Link down while requests work: notice `t.notices.linkLost` "The link to the service was lost; reconnecting…"; status is polled every 3 s.
- On link up after down: everything is re-read (`refreshAll`).
- Version mismatch: error code `version_mismatch` "The manager and service versions do not match. Reinstall Pulsar Seat." (shown as an error notice when it occurs).
- Pipe trust failure (`pipe_client.rs`): access denied with reason `pipe_server_untrusted` if the pipe server is not in session 0 or its PID is not the SCM's service PID. Shown as an error notice: `failureText.pipe_server_untrusted` "A program other than the Pulsar Seat service opened the control channel; no connection was made".

### Product state (Pulsar Seat on/off)
- `ProductState` (`types.ts`): `{state:'disabled'}`, `{state:'enabled', started}`, `{state:'safe_mode', reason}`. Labels (`productText`): disabled "Off", enabled "On", safe_mode "Safe mode (seat 1 only)", enabledStarted "On, seats running", enabledNotStarted "On, seats not started".
- `store.enabled` = status known and state is not `disabled` (safe mode counts as enabled).
- Turn on / off: always via the Enable dialog (see below), admin only. Entry points: Seats status line "Turn on and start" (`t.power.openAndStart`), Seats overflow "Turn Pulsar Seat off…" (`t.power.close`), tray "Turn Pulsar Seat on / off" (toggles to the opposite of the current state).
- Safe mode reasons (`safeModeText`): `windows_safe_mode` "Windows started in safe mode; Pulsar Seat is not running. Restart the PC normally."; `unclean_shutdown` "Windows did not shut down cleanly last time (crash or power cut). To avoid a loop, extra seats were not started. Fix the cause and restart; Pulsar Seat then starts normally."; `crash_loop` "The Pulsar Seat service failed to start several times in a row and stayed in safe mode. Create a diagnostic report and update Pulsar Seat." Shown as a danger notice on every view.
- Start seats gating (`App.requestStart`): safe mode -> error `t.start.safeMode`; disabled -> error `t.start.disabled` "Turn Pulsar Seat on first to start the seats."; status unknown -> error `t.start.noStatus`; not admin -> elevation notice + error `t.adminNeeded`. "Start all" with `prefs.askOnManualStart` true opens the Start confirm dialog. Success toast `t.start.queued` "Starting the seats…".

### Elevation (admin) and restricted mode
- `store.isAdmin` = `hello.caller.is_admin` (elevated token or SYSTEM per architecture §5). `store.restricted` = `hello.caller.restricted` (machine setting "admin only").
- `CallerInfo` also carries `client`, `session_id`, `seat` (seat this manager runs on, may be null).
- Not admin, effects in my files: Save bar Save disabled with reason `t.notAdmin` "Read-only view: to make changes you must be an elevated administrator."; Unsaved dialog Save disabled; Status "Run the checks" disabled with reason `t.adminNeeded`; tray "Start seats" / "Turn on/off" refused with an error and the elevation notice; Enable dialog not opened. Pages show `t.adminToView` "Restart as administrator to see the seat settings (button above)." when config could not be read (`needsElevation`).
- `store.needsElevation` is set when any request fails with `access_denied` or `admin_only`, or when an admin gate refuses. Then (and only while not admin) an info notice shows `t.adminNeeded` "You need to run as administrator for this action." with action `t.restartElevated` "Restart as administrator".
- Restart as administrator (`restart_elevated`): goes through the unsaved-changes guard first; then the shell starts a second copy via ShellExecute `runas` with `--elevated=<pid>` (UAC prompt) and exits on success. UAC "No" -> error notice `failureText.elevation_cancelled` "Administrator permission was not given". A failing ShellExecute gives reason `shell_failed` "The window could not open". The elevated copy waits up to 10 s for the old process to exit. The elevated copy uses its own WebView2 data folder (`<app local data>/elevated`), so its prefs, language choice and "don't ask again" flags are separate.
- Restricted (`admin_only` set by an administrator): info notice `t.restricted` "An administrator set this PC so that only administrators can see and change the settings."; the config is not read at all (`refreshAll` skips `get_config`). Without the lock, since 2026-11-25 (maintainer decision for the read-only Seats board) the service answers `get_config` for any standard user, redacted: per-seat environment variable values and the arguments of startup / kiosk / kiosk shell / remote seat apps come back empty (names and program paths stay); seats, accounts, devices, settings and the password markers are complete. So a standard user normally gets the read-only view with the config, not `t.adminToView`; every write (`validate_config`, `apply_config`, passwords, actions) stays admin only. Demo note: `demo.ts` `config` still refuses non-admins; it should refuse only when `restricted`. Error code `admin_only` text: "An administrator set the settings so that only administrators can change them."
- Service start/stop/restart need the elevated manager (`ServiceInfo.elevated`); SCM access denied maps to `access_denied`.

### Demo mode
- Entered automatically whenever the UI runs outside Tauri (plain browser, `bun run dev`); `isTauri()` checks `window.__TAURI_INTERNALS__`. No user-visible switch.
- `demo.ts createDemoApi({admin?, restricted?})`: in-memory config and device inventory; product starts `disabled`; seat 1 shows signed in as "Ayşe" when on; service version `0.1.0-demo`; SCM state `running`, stop/start toggles it; update check "finds" 0.2.0; diagnostic report returns a fake path; non-admin and restricted are test options only (default admin = true). Browser-only hooks: `globalThis.__demoConflict = true` makes the next save report two conflicts; `globalThis.__store` exposes the store in dev. Language in demo: browser language list (no `ui_language`).

### Save model
- Store holds `saved` (service config) and `draft` (user's working copy). `configDirty` = draft differs from saved (`sameConfig`). `pendingPasswords` (seat -> password or null) are held next to the draft, never in config. `dirty` = configDirty or any pending password.
- Edits from pages/dialogs go to the draft (`store.edit`) and are staged until Save. Examples in App: hotkey, CPU sets, outputs/HDR/VRR/split/software cursor, IP binding, "clear all custom pictures", account changes, imported reference config (import replaces the draft, not saved).
- Saved immediately (not staged), bypassing the Save bar:
  - Update opt-in (`setUpdateSettings`): applied on top of the saved config; the draft gets the same value.
  - Remote seat edits (`applyRemote`): validated + applied at once; merged into the draft. The Remote pages also have their own staged draft ("remote stage", `remote/stage.svelte.ts`) with its own Save bar.
  - Service-side actions: enable/disable, start/stop/relogon seats, move assignment to a new port (`move_assignment`), reset, reboot, identify, preflight, wizard.
  - Per-user prefs (theme, language, view prefs, "don't ask" flags): saved at once to localStorage.
- Save flow (`store.save`): password-only change -> send passwords only. Otherwise `validate_config` -> errors block (toast `t.saved.errors` "Not saved: fix the problems below first.") -> conflicts open the Conflict dialog (Force saves with `force=true`) -> `apply_config` with `base_revision` -> then passwords (`set_seat_password`) -> toast `t.saved.ok` "Settings saved." unless pref `noShowGoodApply` -> seats in `restart_seats` are marked "restart required" on their seat cards -> `reboot_needed` gives info toast `t.saved.reboot` "This change takes effect after the PC restarts." Failed passwords stay pending (draft stays dirty).
- Save bar (App): shown when `dirty`, view is not Remote, service not down. Count = `summarizeChanges` count (min 1); one change = one device assignment, one seat, one top-level settings group, or one pending password. Detail line only when the single change is one device moving to one seat / to the pool: `t.saveBar.moved(device, seat)` "<device> moved to <seat>." (pool = `t.canvas.system`). Buttons: Undo (`t.saveBar.undo`, = discard) and Save (`t.saveBar.save`). Ctrl+Shift+S saves. On Remote view a second Save bar for the remote stage (count/detail/invalid reason from the remote stage).
- Discard (`store.discard`): draft = saved, drops pending passwords, server issues and the changed-elsewhere flag.
- Unsaved-changes guard (`App.guard`) runs before: switching views (except Settings<->Advanced), opening a remote seat from the board, opening the wizard, restart as administrator, moving an assignment to a new port, tray Quit, and window close (see Tray). It covers the local draft and the remote stage.
- Changed elsewhere: when a `config_changed` event or reload brings a newer saved config while the draft is dirty, the draft is kept and a warn notice `t.changedElsewhere` "The settings were changed from another window in the meantime. Saving will not overwrite those changes; reload first." shows with action `t.reload` "Reload" (reload discards the draft). Saving against an old revision fails `revision_conflict` "The settings changed elsewhere in the meantime. Reload and try again."
- After a reload: devices assigned to seats that no longer exist become spare, info toast `t.saved.movedBeyond` "Devices on seats that no longer exist were made spare. Remember to save."
- The shell is told about dirty state (`set_ui_dirty`) so the window close asks first.

### Notifications: notices, toasts, dialogs
- **Notice region** (App, `notices`): at most one region per page, rendered under the page head (`NoticeHost`, when a `PageHead` is mounted) else at the page top. None while the service is down. Items, in order:
  1. Link lost: warn, `t.notices.linkLost`.
  2. Safe mode: danger, `safeModeText[reason]`.
  3. Restricted: info, `t.restricted`.
  4. Elevation needed (needsElevation and not admin): info, `t.adminNeeded`, action "Restart as administrator".
  5. Reboot pending (status `reboot_pending` and admin): warn, `t.saved.reboot`, action `t.enable.rebootNow` "Restart now" -> Reboot dialog.
  6. Windows updated (status `os_build_changed`, not yet acknowledged in this run): warn, `t.osBuildChanged` "Windows was updated. Review the seat settings; if something is wrong, turn Pulsar Seat off and on.", action OK (`t.notices.ok`) hides it for this run only (the service keeps the flag; it returns after restart of the manager).
  7. Changed elsewhere: warn, action Reload.
  8. Every error message: danger, text plus " (<code>)" when an error code tag exists, action OK dismisses. Errors never auto-dismiss.
  - Pages can add their own items (e.g. Seats board adds `t.seatOneAdvice`; owned by another part).
  - One item = one notice; two or more = one expandable group "N notices" (`t.notices.count`) + summary `t.notices.summary` "Changes you should review", open by default.
- **Toasts** (`Toasts.svelte`): kinds `ok`, `info` only (errors go to notices). Auto-dismiss after 4000 ms; timers pause while hovered or focused (resume with at least 500 ms left). Raised above the Save bar when one shows. At most 20 transient toasts kept in the store. Sources: save ok, service started/stopped/restarted (`t.svc.started` "Service started." etc.), seats queued, reset done, import done, wizard done/cancelled, reboot info, devices moved beyond, tray hidden notice, service `notice` events (`noticeText[code]`, prefixed "Seat N: " when the notice names a seat).
- Seat error events (`seat_state` with state `error`) produce an error (danger notice): "Seat N: <failure text>" with the reason as code.
- **Service `notice` codes** (`noticeText`, shown as info toasts unless noted): `gpu_preference_unavailable`, `os_build_changed` "Windows was updated. Review the seat settings and save them again if needed.", `service_gap_no_input`, `same_account_warning` (own seat -> Same-account dialog instead, unless the pref hides it), `seat_starting`, `run_entry_missing`, `program_files_damaged`, `hardware_changed`, `devices_beyond_limit`, `reboot_needed`, `seat_start_failed`, `password_failing`, `tweak_failed`, `keep_awake_failed`, `input_move_pending` (opens the Input-move dialog instead), `credprov_failed`, `credprov_logon_failed`, `switch_refused`, `dpi_mismatch`, `host_console`. Unknown codes show the raw code.
- **Error texts** (`errorText`, used by `errorLabel`; a `reason` with its own `failureText` wins): `service_unavailable` (= `t.serviceDown` "The Pulsar Seat service cannot be reached. Make sure the service is installed and running; security software may have stopped it."), `access_denied` (= `t.adminNeeded`), `admin_only`, `version_mismatch`, `disabled` "Pulsar Seat is off. Turn it on first.", `safe_mode`, `revision_conflict`, `conflicts_not_confirmed`, `invalid_config`, `invalid_value`, `not_found`, `seat_running`, `seat_not_running`, `unsupported`, `not_ready`, `timeout`, `driver_unavailable`, `coexistence` "Another conflicting multiseat product was found.", `policy_blocked`, `logon_failed`, `warning_not_acknowledged`, `wizard_state`, `os`, `store` "The settings file could not be written.", `not_implemented`, `wrong_seat_kind`, `session_unavailable`, `bad_request`, `internal` "An unexpected error occurred."; anything else "Error: <code>" (`misc.errorPrefix`). Code tag shown = `reason ?? code` plus " · Windows <win32>" when present (`errorCodeTag`).
- App-level modal dialogs are exclusive (`dialog` state, one at a time). Input-move and Same-account dialogs show only while no other App dialog is open.

### Language switch (en / tr)
- Where: Settings > Appearance > Language (`BasicTab`, owned by another part): "Windows display language" (`system`), "English", "Türkçe"; help `t.basic.languageRestart` "The manager reloads in the chosen language."
- Persisted: localStorage key `pulsar-seat.manager.lang.v1` (`en`/`tr`; `system` removes the key). Per user, per WebView data folder (elevated copy separate).
- Resolution: stored choice; else Windows user UI language from the shell (`ui_language`: `en`/`tr`, English for anything else); else `navigator.languages`; first supported primary tag, else English.
- A change stores the choice, tells the shell (`set_ui_language`: relabels tray items, window title, file dialogs) and reloads the window. Sets `<html lang>`.
- `?lang=` is honoured only on the dev `/ui` gallery.

### Theme
- Per-user pref `theme`: `system` (default), `light`, `dark`. Applied as `data-theme` on `<html>`; `system` removes it. Edited in Settings (other part).

### Update check (global parts)
- Opt-in, machine-wide config `settings.updates {check, weekly}`, saved immediately. The service does network, hash and signature work; the manager shows state and asks before installing. Opened from Settings > maintenance (`onDialog('updates')`). Details under "Update dialog".

### Tray, window close, quit, single instance
- Tray icon id `main-tray`, tooltip "Pulsar Seat". Left click (button up) or left double-click shows/unminimizes/focuses the main window. Menu (right click; left click does not open it): "Show the window" (`TRAY_SHOW`), "Start seats" (`TRAY_START`), "Turn Pulsar Seat on / off" (`TRAY_TOGGLE`), separator, "Quit" (`TRAY_QUIT`). Turkish: "Pencereyi göster", "Koltukları başlat", "Pulsar Seat'i aç / kapat", "Çıkış". Any menu item first shows the main window, then emits `tray-action`.
- Tray actions in UI (`App.onMount`): `start_seats` -> admin gate -> start flow (may open Start confirm); `toggle_enabled` -> admin gate -> Enable dialog for the opposite state; `quit` -> unsaved guard -> Quit dialog; `show` -> only shows the window.
- Hide tray icon: pref `hideTray` (Settings, other part) -> `set_tray_visible(!hideTray)`; applied at start and on change.
- Window close (X / Alt+F4 on main): never exits. If dirty -> event `close-requested` -> unsaved guard -> then `hide_to_tray`. Else the window hides at once. Both emit `hidden-to-tray`; the first time ever (pref `trayNoticeShown`) an info toast `t.tray.hidden` "The manager keeps running in the notification area. To quit completely, choose “Quit” in the icon’s menu." Only tray Quit -> Quit dialog -> `quit_app` ends the process. Seats keep running.
- Close on a wizard page window: emits `wizard-page-close` (quit wizard without saving; handled by Wizard, other part).
- Single instance per session (`tauri_plugin_single_instance`): a second launch shows and focuses the running window.
- Autostart: installer Run entry passes `--tray` (window starts hidden). `AppInfo.autostart` reflects these flags.

### Keyboard shortcuts (global)
- Ctrl+Shift+S: Save (Save bar with `shortcut`, both local and remote stage bars). Ignored while busy, while Save is disabled, or while any modal `<dialog>` is open or focus is inside a dialog. Save button carries `aria-keyshortcuts="Control+Shift+S"`.
- Escape: closes the topmost modal (`Modal`), = Cancel/Close.
- No other global hotkeys in App/Rail. Rail links ignore modified clicks. Wizard/HotkeyDialog/WizardPage have their own window key handlers (other parts).

### Polling and timers
- Store fallback poll every 3000 ms (`POLL_MS`): `get_status` only while the event link is down; SCM `service_state` every tick unless a service action runs.
- Event subscription reconnect back-off (shell): 500 ms doubling to max 5 s; reset after a connection that came up.
- Pipe open: up to 20 tries with 50 ms sleeps on ERROR_PIPE_BUSY.
- Seat restart wait: poll `get_status` every 500 ms, up to 60 s for stop and 60 s for start (`RESTART_WAIT_MS`, `RESTART_POLL_MS`); timeout text `t.seatCard.restartTimeout`.
- Update dialog: polls `get_update_status` every 1000 ms while state is `checking`, `downloading` or `installing`.
- SCM control waits up to 60 s, polling state every 250 ms.
- Toast lifetime 4000 ms. Device "just connected" mark lasts `prefs.flashSeconds` (2-20 s, even, default 4).
- Elevated copy waits up to 10 s for the parent to exit.

## 3. Screens, pages and dialogs

Each entry lists Purpose, Opens when, Controls, Actions, States, Messages/codes, Dependencies and Live behaviour. Codes shown by several screens are catalogued once in section 4.5.

### 3.1 App shell and global screens

#### App shell (`App.svelte`, `+page.svelte`)
- **Purpose**: Main window frame: rail, one scrolling page area (notice region + current view), docked save bar, toasts, and all app-level dialogs and flows (enable/disable, start, reboot, service stop/restart, save conflicts, unsaved changes, quit, reset, hotkey, CPU sets, outputs, IP binding, import, diagnostics, updates, wizard, input move, same account).
- **Opens when**: App start (any user; non-admin gets read-only behaviour).
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Rail place links | navigation | seats, remote, settings, status | stored `prefs.tab` (default `seats`) | `navigate(tab)` with unsaved guard; `setPref('tab')` |
| Notice actions | button per notice | see Global behaviour notice list | - | restart elevated / reboot dialog / ack OS build / reload config / dismiss error |
| Save bar Undo / Save | buttons | Save disabled when not admin (`t.notAdmin`) | hidden unless dirty | `store.discard()` / `store.save()` |
| Remote stage Undo / Save | buttons | Save disabled with remote stage `invalid` reason | hidden unless remote stage dirty and view = remote | `discardRemoteStage` / `saveRemoteStage` |

- **Actions**: page commands wired here (contracts in App header): Seats `startSeat(n)`, `wizard`, `reset`, `importConfig`, `cpu`, `hotkey`, `moveAssignment(from,to)` (guarded, `move_assignment`), `ipBinding(n)`, `openRemote(n)`, `enable(on)`, `startAll`, `stopAll` (`stopAllSeats`: every running seat except seat 1 and the manager's own seat, `stop_seat` each), `openSettings`, `restartSeat(n)` (local: `relogon_seat` then wait; remote: `stop_seat`, wait for off, `start_seats`, wait). Settings `onAdvanced(section?)`, `onDialog(diagnostics|updates|import|reset1|clearPictures)` (clearPictures edits the draft directly). Advanced `onBack`, `onHotkey`, `onCpu`, `onOutputs`, `onIp`. Status `onDiagnostics`, `onServiceAction` (start runs at once; stop/restart ask). Remote `onSeatsChanged` (re-read status), `onOpenSettings`.
- **States**: loading (`t.loading` before the store exists; pages show their own loading); service down (service-down screen replaces every view except Status, no Save bar); not admin (read-only, see Global behaviour); restricted (notice, no config); demo (browser).
- **Messages, warnings, codes**: see Global behaviour notice list.
- **Dependencies**: Tauri shell commands; service pipe; admin for any change.
- **Live behaviour**: notices, rail counts and dot, save bar count update from store state and events.

#### Rail (`Rail.svelte`)
- **Purpose**: Primary navigation and service health indicator.
- **Opens when**: Always visible in the main window.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Brand link "Pulsar Seat" | link | - | - | go to Seats (`aria-label` `t.appTitle`) |
| Seats | link + count | count = local seats (kind ≠ remote); hidden when 0 or config unknown | - | navigate `seats` |
| Remote seats | link + count | count = remote seats; hidden when 0 | - | navigate `remote` |
| Settings | link | current also when on `advanced` | - | navigate `settings` |
| Status | link + state dot | dot `ok` / `link` / `down` | - | navigate `status` |

- **Actions**: navigation only. Version line `t.nav.version(v)` "Version <v>" (manager version from `app_info`) and `t.title.onSeat(n)` "Running on seat <n>" (caller seat) when known.
- **States**: current item `aria-current="page"`; count `aria-label` `t.nav.count(n)` ("1 seat" / "N seats").
- **Messages, warnings, codes**: `t.nav.statusOk/statusDown/statusLink` (hidden text after "Status").
- **Dependencies**: none beyond store.
- **Live behaviour**: counts follow the saved config; dot follows `serviceDown` / `link`.

#### Service-down screen (`App.svelte`)
- **Purpose**: Replaces the current view (except Status) when the service cannot be reached.
- **Opens when**: `store.serviceDown` true and view ≠ status.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Start service | button (primary) | disabled with reason `t.svc.needElevated` "Run the manager as administrator to start or stop the service." when not elevated; busy while starting | - | `service_control('start')` |
| Diagnostic report | button | - | - | opens Diagnostic dialog (other part; `diagnostic_report`) |

- **Actions**: as above; after start the store re-reads everything (or stays down if SCM says stopped).
- **States**: page head title = current view title; empty state title `t.service.downTitle` "The service cannot be reached", body `t.service.downBody` "The Pulsar Seat service is not running, or security software may have stopped it." No notices, no Save bar.
- **Messages, warnings, codes**: `t.svc.started` toast on success; failures as error notices (e.g. `access_denied`, `service_unavailable` when not installed, `not_ready` for SCM 1056/1061/1062).
- **Dependencies**: SCM; elevated manager. Not-installed state is not distinguished on this screen (Start stays enabled if elevated; the SCM error follows) — the Status page shows `t.svc.notInstalled`.
- **Live behaviour**: SCM state polled every 3 s; screen clears when a request succeeds.

#### Service actions (`ServiceActions.svelte`)
- **Purpose**: Start / stop / restart buttons for the `PulsarSeat` service with progress and reason text. Note: not used by any production view today (only by its test); the Status page has its own copy of the same logic.
- **Opens when**: n/a (unused component). Would render only when SCM info is known.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Restart service | button | shown when running; disabled when not elevated, busy, or reason | - | confirm, then `service_control('restart')` |
| Stop service | button | shown when running; same | - | confirm, then `service_control('stop')` |
| Start service | button | shown when not running; enabled only when state `stopped`, elevated, not busy | - | `service_control('start')` (no confirm) |

- **Actions**: Stop and Restart confirm dialogs (`t.svc.confirmStopTitle` "Stop the service?", `t.svc.confirmStop` "All running seats (local and remote) close. Unsaved work on the seats may be lost."; `t.svc.confirmRestartTitle` "Restart the service?", `t.svc.confirmRestart` "All running seats close. You may need to start the seats again after the service is back."). Both confirms are danger here.
- **States**: reason text priority: not elevated `t.svc.needElevated`; `pending` `t.svc.pending` "The service is changing state right now; wait a moment."; `not_installed` `t.svc.notInstalled`.
- **Messages, warnings, codes**: progress `t.svc.starting/stopping/restarting`.
- **Dependencies**: SCM, elevated manager.
- **Live behaviour**: SCM poll every 3 s.

#### Status page (`StatusTab.svelte`)
- **Purpose**: Read-only report of service, product, seat run states, methods in use with fallbacks, limitations, Pulsar drivers and pre-flight checks; service start/stop/restart.
- **Opens when**: Rail foot "Status". Visible also while the service is down (the only view that is). Any user.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Restart service | head button | shown when SCM `running`; disabled with reason (see below) | - | App asks (Service confirm dialog), then `service_control('restart')` |
| Stop service | head button | shown when `running` | - | App asks (danger), then `service_control('stop')` |
| Start service | head button | shown when not running; enabled only when `stopped`, not busy, no reason | - | `service_control('start')` at once |
| Diagnostic report | compact button in "no status" notice | shown when no status and handler given | - | Diagnostic dialog |
| "Every method on this PC" | disclosure | - | closed | shows all backends per category |
| Run the checks | section button | disabled with reason `t.adminNeeded` when not admin; busy while running | - | `preflight` -> `store.checks` |
| technical detail | `<details>` per check | only when check has `detail` | closed | shows raw detail |

- **Actions**: as above. Head reason text: not elevated `t.svc.needElevated`; `pending` `t.svc.pending`; `not_installed` `t.svc.notInstalled`. Progress line while a service action runs.
- **States**:
  - No status: warn notice `t.statusPage.noStatus` "The service has not reported its state."
  - Head buttons hidden when SCM info unknown.
  - Sections shown conditionally: Seats only when status has seats; Methods when status or backends known; Limitations when backends or uncategorised fallback notes; Drivers when backends known; Checks always (help `t.statusTab.noChecks` "No check has run yet." until run).
  - Values: OK values plain text; non-OK values carry a status kind (warn/danger/off).
- **Messages, warnings, codes**:
  - Intro `t.statusPage.intro`.
  - Service section (`t.statusTab.service`): row `t.statusPage.serviceRow` "Pulsar Seat service" = `rt.serviceState[state]` (running ok, pending warn, unknown off, stopped/not_installed danger); without SCM info: "Running" if status known else "Unknown". Help `t.statusPage.versions(service, manager, arch)` "Version <s> · manager <m> · <arch>" (arch via `archLabel`: aarch64 "ARM64", x86_64 "x64", x86 "x86"). Row `t.statusPage.productRow` "Pulsar Seat" = `productLabel` (enabled ok, safe mode warn, off off). Row `t.statusTab.caller` "This window" = "Administrator (elevated)" / "Standard user", help "Running on seat N" / `t.title.noSeat` "Seat unknown". Row `t.statusTab.revision` "Configuration revision" = `config_revision`.
  - Seats section (`t.seats`): per runtime seat: name (or "Seat N"; remote: `t.statusPage.remoteSeat` "<name> (remote)"), state `seatLabel` (off off, error danger, waiting_for_display warn, others ok; signed_in/locked append ": <user>"; starting appends phase `t.phases.*`: queued, waiting for the host, binding the monitor, starting the seat host, connecting, waiting for the session, signing in; error appends failure text), help `t.statusPage.windowsSession(id)`; notes = degraded + audio notes + redirect notes (`noteLabel`); sound buffer line `t.statusPage.audioBuffer(underruns, drops)` only when > 0; redirect refusals list `t.redirect.refusals`, newest first (`refusalLabel`: channel, reason, "+N" suppressed).
  - Methods section (`t.statusTab.backends` "Methods"): one row per category `session` "Session", `display` "Display", `input` "Input", `audio` "Sound", `gates` "Device access": active backend name(s) (`t.backendNames`), or warn `t.statusPage.fallbackTo(method)` "Fallback: <method>" with help built from fallback notes (`t.statusPage.notUsed(wanted, why)` "<wanted> cannot be used: <why>", `t.fallback.forcedByFus`). Fallback note shapes: `cat:wanted->used:reason`, `cat:wanted:reason`, plain code (`gate` = gates). All-methods disclosure rows "<Category>: <Backend>" = `t.statusPage.inUse` "In use" / `selectedNotInUse` "Selected, not in use" / availability label (`t.availability.*`, with `t.unavailableReasons`, `t.needsSetup`, driver name); help = backend notes.
  - Limitations (`t.statusPage.restrictions`, help `restrictionsHelp`): notes of active backends + uncategorised fallback notes, deduplicated; info icon for `one_seat_at_a_time`, `fus_audio_per_user`, `kiosk_audio_shared`, `kiosk_audio_per_app`, `drive_served_as_seat_user`, `touch_emulated_as_mouse`, `gamepads_shared`, `xinput_pads_shared`, `audio_console_shared`; warn icon otherwise; empty -> `t.statusPage.noRestrictions`.
  - Drivers (`t.statusTab.drivers` "Pulsar drivers"): per driver name (`t.drivers`), state loaded ok (shows "major.minor · build"), not_installed off, installed_not_loaded warn, others danger (`driverStatusLabel`; version_mismatch shows found). Section help `t.statusPage.driversHelp` "Pulsar Seat drivers are not signed yet; user-mode methods are used." when any driver is not loaded.
  - Checks (`t.statusTab.checks` "Pre-flight checks"): per check id label `t.checks[id]`, help `t.checkText[id][outcome]`, outcome pass/info ok, warn warn, fail danger (`t.outcomes`).
- **Dependencies**: `get_status`, `get_backends`, `preflight` (admin), SCM. Backend availability differs by Windows edition and arch (reported by the service, not decided here).
- **Live behaviour**: seat states via `seat_state` events; product via `product_state`; SCM every 3 s; status poll every 3 s while link is down.

#### Update dialog (`UpdateDialog.svelte`)
- **Purpose**: Opt-in update check, download/verify and install of a new MSI, run by the service.
- **Opens when**: Settings maintenance link "updates" (`onDialog('updates')`). All service calls need admin (non-admin gets `access_denied` errors).
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Check for new versions (GitHub) | switch | on/off; disabled (shows `t.loading`) until status loaded; ignored while busy | from `UpdateStatus.enabled` (config `settings.updates.check`, default unclear in code) | `setUpdateSettings({check, weekly: check && weekly})`, saved at once (`apply_config`) |
| Check by itself once a week | switch | shown only when opt-in on | `UpdateStatus.weekly` | `setUpdateSettings({check:true, weekly})` |
| Install | primary button | shown when `canInstall`: enabled, `latest` known, state `ready` or failed with `install_*` reason | - | opens Install confirm |
| Download and verify | button | shown when `canDownload`: enabled, latest known, state `available` or `failed` with reason ≠ `no_asset`; disabled while busy/polling | - | `download_update` |
| Check now | button | disabled when opt-in off, busy, polling or installing | - | `check_for_update` |
| Close | button | - | - | close |

- **Actions**: Install confirm (danger): title `confirmTitle` "Install the update?", body `confirmBody(v)` "Pulsar Seat <v> will be installed. All running seats will stop and the sessions on the seats will close. Your settings and seat passwords are kept. If needed, you are asked to restart after the installation.", buttons "Install now" / "Cancel" -> `install_update{version, confirm:true}`.
- **States**: `UpdateState` values and lines (`t.update.state`): `idle` "No check done.", `checking` "Checking…", `up_to_date` "Pulsar Seat is up to date.", `available` "New version available: <v>", `downloading` "Downloading… <pct>%" (pct from downloaded/size, or no pct), `ready` "<v> downloaded and verified (signed by: <signer>).", `installing` "The installer package is verified again and installed. The Pulsar Seat service restarts during the installation; the manager connection may drop for a short time.", `failed` reason text or "The last step failed.". Status row help: `current(v)` "Installed version: <v>" · arch · "Last check: <date>" or "Not checked yet." (date localised `tr-TR` / `en-GB`). Release notes line `pageHint(url)` when latest has a page URL. Opt-in off: `needsOptIn` "Turn on the option above first to check." Loading: switch shows "Loading…". Error: danger notice (step reason text or generic error). During `installing`, lost connection does not show an error (polling continues).
- **Messages, warnings, codes**: intro `t.update.intro` ("If you want, Pulsar Seat checks for new versions on the release page on GitHub. While the check is off no connection is made; no information about your PC is sent."). Reasons `t.update.reasons`: `disabled`, `network`, `feed`, `no_asset` (no package for this PC's architecture), `no_checksum`, `hash_mismatch`, `unsigned`, `untrusted_signer`, `sums_unsigned`, `sums_untrusted`, `sums_mismatch`, `sums_rollback`, `no_release_key`, `too_large`, `launch`, `install_failed` (log `%ProgramData%\Pulsar Seat\logs\update-msi.log`, retry allowed), `install_timeout` (one hour), `busy`, `not_ready`, `not_confirmed`, `unsupported`.
- **Dependencies**: service (admin); internet only when opted in; per-architecture asset (x64 / ARM64).
- **Live behaviour**: 1 s polling while checking/downloading/installing.

#### Enable / disable dialog (`App.svelte`, kind `enable`)
- **Purpose**: Confirm turning Pulsar Seat on (then start seats) or off.
- **Opens when**: Seats status line "Turn on and start", Seats overflow "Turn Pulsar Seat off…", tray toggle. Admin only (gate before opening).
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Start mode (read-only value) | locked value | `t.settingsPage.startModeLabels[start_mode]` | draft `settings.start_mode` (`manual` fallback) | display only; only in the "on" dialog |
| Change | compact button | - | - | closes dialog, navigates to Settings (guarded) |
| Confirm | button | on: `t.power.openAndStart` "Turn on and start"; off: `t.bar.disable` "Turn Pulsar Seat off" (danger) | - | `set_enabled(on)`, then re-read status and config |
| Cancel | button | - | - | close |

- **Actions**: after success: if `reboot_pending` -> Reboot dialog; else if turned on and enabled -> start flow (`requestStart(null)`, may open Start confirm).
- **States**: busy while `store.busy`. Danger dialog (off) focuses Cancel first.
- **Messages, warnings, codes**: on: `t.enable.enableTitle` "Turn Pulsar Seat on?", `enableBody` "When Pulsar Seat is on, extra seats run according to the chosen start mode. A diagnostic report is saved before turning it on."; off: `disableTitle` "Turn Pulsar Seat off?", `disableBody` "Running extra seats close; unsaved work in open sessions may be lost."
- **Dependencies**: admin; service.
- **Live behaviour**: product state from events.

#### Reboot dialog (`App.svelte`, kind `reboot`)
- **Purpose**: Offer a Windows restart to finish a change.
- **Opens when**: after enable/disable with `reboot_pending`; notice action "Restart now".
- **Controls**: Restart now (`t.enable.rebootNow`) -> `request_reboot`; Later (`t.later`) closes.
- **Actions**: failure -> error `t.enable.rebootFailed` "The PC could not restart by itself. Please restart it by hand."
- **States**: None.
- **Messages, warnings, codes**: `rebootTitle` "Restart needed", `rebootBody` "Windows must restart to finish the change. Restart now?"
- **Dependencies**: admin.
- **Live behaviour**: None.

#### Start seats confirm (`App.svelte`, kind `start`)
- **Purpose**: Confirm starting all auto-start seats.
- **Opens when**: start-all (Seats status line, tray, after enabling) while pref `askOnManualStart` is true. Not for a single seat.
- **Controls**: checkbox "Don’t ask again" (`t.start.dontAsk`) -> sets pref `askOnManualStart=false`; Start seats (`t.bar.start`) -> `start_seats{seats:null}`; Cancel.
- **Actions**: as above; toast `t.start.queued`.
- **States**: None.
- **Messages, warnings, codes**: `t.start.confirmTitle` "Start the seats?", `confirmBody` "All seats marked to start automatically are started one after another."
- **Dependencies**: admin, product enabled, not safe mode.
- **Live behaviour**: None.

#### Service stop / restart confirm (`App.svelte`, kind `service`)
- **Purpose**: Confirm stopping or restarting the service from the Status page.
- **Opens when**: Status page Stop / Restart.
- **Controls**: Confirm (danger, `t.svc.stop` / `t.svc.restart`) -> `service_control`; Cancel.
- **Actions**: toast "Service stopped." / "Service restarted."; on stop the app shows the service-down state.
- **States**: None.
- **Messages, warnings, codes**: `t.svc.confirmStopTitle/confirmStop`, `confirmRestartTitle/confirmRestart` (texts under Service actions).
- **Dependencies**: elevated manager.
- **Live behaviour**: None.

#### Unsaved-changes dialog (`App.svelte`, kind `unsaved`)
- **Purpose**: Save / discard / cancel before an action that would leave or replace the draft.
- **Opens when**: guard triggers (see Save model) while local draft or remote stage is dirty.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Save | primary button | disabled when not admin | initial focus | save remote stage, then local save; continue only if both succeed (conflicts open Conflict dialog and stop) |
| Continue without saving | button | - | - | discard remote stage + local draft, then continue |
| Cancel | button | - | - | close; Escape also cancels |

- **Actions**: as above.
- **States**: alert dialog; shows the save-bar detail line when one exists.
- **Messages, warnings, codes**: `t.unsaved.title` "Unsaved changes", `body` "Your changes are not saved yet."
- **Dependencies**: admin to save.
- **Live behaviour**: None.

#### Quit dialog (`App.svelte`, kind `quit`)
- **Purpose**: Confirm ending the manager process.
- **Opens when**: tray "Quit" (after unsaved guard).
- **Controls**: Quit (`t.quit.confirm`) -> `quit_app`; Cancel.
- **Actions**: process exits; seats keep running.
- **States**: None.
- **Messages, warnings, codes**: `t.quit.title` "Close the manager?", `body` "When the window closes, the manager keeps running in the notification area. Do you want to quit the program completely? The seats keep running."
- **Dependencies**: None.
- **Live behaviour**: None.

#### Reset dialog (`App.svelte`, kinds `reset1`, `reset2`)
- **Purpose**: Reset all settings to defaults.
- **Opens when**: Settings reset button / Seats overflow "Reset all settings…" (other parts).
- **Controls**: typed confirmation field `t.reset.typeLabel(word)` "Type RESET to confirm" (case-insensitive, trimmed, locale-aware upper-case; Turkish word from tr.ts); Reset (danger, disabled until the word matches) -> `reset_config{confirm:true}`; Cancel.
- **Actions**: success toast `t.reset.done` "Settings reset."; everything re-read, draft replaced.
- **States**: `reset2` ("Are you sure?" / "Last confirmation: all Pulsar Seat settings will be deleted.") exists in code but nothing opens it (dead).
- **Messages, warnings, codes**: `t.reset.title` "Reset all settings?", `body` "Seats, device assignments, saved passwords and optional system settings return to their defaults. This cannot be undone."
- **Dependencies**: admin.
- **Live behaviour**: None.

#### Input-move dialog (`App.svelte`)
- **Purpose**: Confirm or undo keyboard/mouse moves made with the switch hotkey.
- **Opens when**: service notice `input_move_pending` (seat in notice, default 1), user is admin and no other App dialog is open.
- **Controls**: Save (`t.inputMoveDialog.save`) -> `resolve_input_move{accept:true}`; Cancel (`discard`) -> `accept:false`. Then config re-read.
- **States**: None.
- **Messages, warnings, codes**: title "Keyboard and mouse moved"; body(seat) "A keyboard (and the mice on the same hub) moved to seat N with the switch shortcut. The move holds for now; save to keep it. If you cancel, the devices go back to their saved seats."
- **Dependencies**: admin.
- **Live behaviour**: event-driven.

#### Same-account dialog (`App.svelte`)
- **Purpose**: Warn that the account of this manager's seat is also signed in on another seat.
- **Opens when**: notice `same_account_warning` concerning the caller's own seat (or naming no seat while the caller has one), pref `hideSameAccountWarning` false, no other dialog open.
- **Controls**: checkbox "Don’t show again" (applies to both answers, sets pref); Sign out -> `relogon_seat` for own seat; Keep signed in -> close.
- **States**: None.
- **Messages, warnings, codes**: title "The same account is signed in on another seat"; body "This account is also signed in on another seat. Using the same account on several seats is not recommended: files and settings can corrupt each other."
- **Dependencies**: own seat known.
- **Live behaviour**: event-driven.

#### Other App-hosted dialogs (owned by other parts)
- Conflict dialog (`ConflictDialog`, on save conflicts; Force -> save with `force`), Hotkey, CPU sets, Outputs, IP binding, Import, Diagnostic, Wizard. App only opens them and writes their results into the draft (staged) — Import replaces the draft; Diagnostic calls `diagnostic_report(mask_users)`.

#### Read-only / not-admin banner and restricted mode (`App.svelte`)
- **Purpose**: Explain why changes are not possible and offer elevation.
- **Opens when**: see Global behaviour "Elevation". Elevation notice appears after a refused action or a refused config read.
- **Controls**: "Restart as administrator" notice button -> guard -> `restart_elevated`.
- **States**: restricted notice has no action.
- **Messages, warnings, codes**: `t.adminNeeded`, `t.restricted`, `t.notAdmin` (save bar reason), `t.adminToView` (pages).
- **Dependencies**: UAC.
- **Live behaviour**: None.

#### Changed-elsewhere notice (`App.svelte`)
- See Save model. Action Reload -> `refreshConfig(true)` (discard draft, read saved).

#### OS-build-changed notice (`App.svelte`)
- Shows when `status.os_build_changed`; OK hides it for this manager run only. Text `t.osBuildChanged`. The service-side notice code `os_build_changed` (toast) has a different text (`noticeText.os_build_changed`).

#### First run (`App.svelte` comment; UI in `SeatsTab.svelte`, other part)
- No modal opens on launch. The "configured but off" offer is the Seats status line ("Turn on and start"). When only one local seat exists, the Seats board shows a section `t.canvas.addSecondTitle` "Add a second seat" with "Start the setup wizard" and "add empty seat" buttons (admin only). `t.wizard.firstRun` and `t.enable.offerEnable` strings exist but are not used by these files. The app does not track first-run in prefs.

### 3.2 Local seats board

Scope: the main "Seats" page of the manager (local seats), the unassigned-devices pool, seat cards,
device rows, monitor faces, missing-device lines, every menu on this page, and the dialogs opened
only from it. Base dir: `apps/manager/src/lib`.

General rule for this page: almost every edit here changes the **draft** config only
(`store.edit(...)`, functions in `model.ts`). Nothing is sent to the service until the user saves
with the global save bar (`apply_config`, specced by the store/global part). Exceptions that call
the service at once are marked "IPC" below.

#### Seats board (`SeatsTab.svelte`)

**Purpose**
Main page for local seats. Shows: page head (title `t.nav.seats`, a power status line, "Add seat",
an overflow menu), up to two notices, the unassigned-devices pool, one card per visible local seat,
an "add a second seat" invitation (only one local seat), a keyboard move hint, and the remote seats
as link rows. Devices move between seats by drag and drop or by the per-device "Move" menu.

**Opens when**
Navigation entry "Seats" (`t.nav.seats`, App.svelte renders `<SeatsTab {store} commands={seatCommands} />`).
Anyone can open it. Editing controls need an elevated admin (`store.isAdmin`); see States.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Power status line: "Start seats" (`t.power.startAll`) | compact button | shown when `powerLine().action === 'start'` (product on) and App passes `startAll`; disabled when `!isAdmin` or `store.busy` | - | App `startAll` -> admin gate -> `requestStart(null)`; if pref `askOnManualStart` (default true) a confirm (`t.start.confirmTitle`/`confirmBody`, "don't ask" checkbox `t.start.dontAsk`) then IPC `start_seats` with `seats: null` |
| Power status line: "Turn on and start" (`t.power.openAndStart`) | compact primary button | shown when product state `disabled`; disabled when `!isAdmin` or busy | - | App `askEnable(true)` -> enable confirm dialog (App part) -> IPC `set_enabled` then start (as above). If `reboot_pending`, a reboot dialog (`t.enable.rebootTitle`) instead |
| Add seat (`t.canvas.addSeat`, icon plus) | button | disabled when no draft or not admin; disabled with reason `t.canvas.seatLimit` "Seat limit reached (16)." when total seats (local + remote) >= 16 | primary; becomes secondary when product is off or the draft is dirty | `addSeat()` in draft: lowest free number 1..16, name = its number, `autostart: true`, account `logon_screen`, kind `local` |
| Overflow menu "Seats: more actions" (`t.canvas.more`) | menu | see Actions | - | - |
| Pool | drop zone | accepts only drags of type `application/x-pulsar-device` | - | drop -> device mask 0 (spare) |
| Seat card | drop zone, menu | see SeatCard | - | - |
| Device row / monitor face | button (opens Move menu), draggable | see DeviceRow / MonitorFace | - | - |
| "Start the setup wizard" (`t.canvas.startWizard`) | button in invitation | shown only when exactly 1 local seat; disabled when not admin | - | App `openWizard` (unsaved-changes guard, then wizard) |
| "Add an empty seat" (`t.canvas.addEmpty`) | quiet button in invitation | same; disabled when not admin | - | same as Add seat |
| Remote seat link row | link row | one per seat with `kind === 'remote'` | - | App `openRemote(n)` (navigates to the remote seats page) |

**Actions**

Overflow menu (`moreItems`), in order:

| Entry | en.ts key | Disabled when | Effect |
|---|---|---|---|
| Find by touch / Stop searching | `t.canvas.pickByTouch` / `t.canvas.pickCancel` | not searching and the event link is down (`!store.link`) | `store.startPicking()` / `stopPicking()` (local state). The next `device_activity` event from a known device ends the search and opens that device's Move menu (see Live) |
| Number the monitors / Hide the numbers | `t.canvas.identifyMonitors` / `identifyMonitorsStop` | not admin | for every present monitor IPC `identify_device {key, on}` one by one; local toggle `identifyingMonitors` |
| (separator) | | | |
| Setup wizard… | `t.systemMenu.wizard` | not admin | App `openWizard` |
| View settings… | `t.systemMenu.view` | never | opens View settings dialog |
| Compare with the saved configuration… | `t.systemMenu.compare` | not admin | IPC `compare_saved` -> CompareDialog (other part). Its callbacks: restore = discard draft; new monitors to spare = `addedMonitorsToSpare` in draft; move = App `moveAssignment` (IPC `move_assignment`) |
| USB dock wizard… | `t.systemMenu.usbDock` | not admin | opens USB dock wizard |
| Show all cameras | `t.systemMenu.cameras` | no present webcam | opens Camera grid (all cameras, title `t.camera.allTitle`) |
| (separator) | | | |
| Stop all seats | `t.power.stopAll` | not admin, or no seat other than seat 1 is running | App `store.stopAllSeats()`: IPC `stop_seat` for every runtime seat that is not seat 1, not the manager's own seat, not `off`. Fallback without App command: stop each running seat except 1 |
| Turn Pulsar Seat off… | `t.power.close` | not admin, product not enabled, or no `enable` command | App `askEnable(false)` -> confirm -> IPC `set_enabled false` |
| (separator) | | | |
| Windows settings (submenu) | `t.canvas.windowsSettings` | | each: Tauri command `open_system_panel {panel}` |
| - Device Manager | `t.systemMenu.deviceManager` | | panel `device_manager` |
| - Display settings | `t.systemMenu.displaySettings` | | `display_settings` |
| - Sound settings | `t.systemMenu.soundSettings` | | `sound_settings` |
| - Network Connections | `t.systemMenu.networkConnections` | | `network_connections` |
| - User accounts | `t.systemMenu.userAccounts` | | `user_accounts` |

Seat menu (per seat card, `seatMenu(s)`), in order. `up` = runtime state not `off` and not `error`:

| Entry | en.ts key | Shown / disabled | Effect |
|---|---|---|---|
| Start this seat | `t.seatMenu.start` | disabled for seat 1, when product not enabled, or when up | App `requestStart([n])`: blocked with toast `t.start.safeMode` in safe mode, `t.start.disabled` when off; else IPC `start_seats {seats:[n]}` (no confirm for a single seat), toast `t.start.queued` |
| Sign in again | `t.seatMenu.relogon` | disabled when not up | IPC `relogon_seat {seat}` |
| Restart the graphics driver | `t.seatMenu.resetGraphics` | disabled when not up, except seat 1 (always enabled) | IPC `reset_graphics {seat}`; toast `t.seatMenu.resetGraphicsSent` |
| Stop this seat | `t.seatMenu.stop` | disabled for seat 1 or when not up | IPC `stop_seat {seat}` |
| (separator) | | | |
| Rename… | `t.seatMenu.rename` | always | SeatNameDialog (other part) -> `renameSeat` in draft (name rules `t.seatName.errors`: empty, max 64 chars, no control chars) |
| Sign-in account… | `t.seatMenu.account` | always | SeatAccountDialog (other part); loads `list_local_users`; "open accounts" = panel `user_accounts`; save -> `setAccount` in draft, password kept pending and sent with save via `set_seat_password` |
| Kiosk seat… | `t.seatCard.kioskMenu` | shown only when session backend is `desktops` and seat != 1 | KioskShellDialog (other part) -> `setKioskShell` in draft |
| IP address… | `t.seatCard.ipMenu` | disabled when App gives no `ipBinding` command | App `openIp(n)` (IpBindingDialog, other part) |
| Processor cores… | `t.seatMenu.cpu` | always | App opens CPU sets dialog (machine-wide dialog, not per seat) |
| Graphics card… | `t.seatCard.gpuMenu` | always | GpuPreferenceDialog (other part) -> `setGpuPreference` in draft |
| Show this seat's cameras | `t.seatMenu.cameras` | disabled when no present webcam whose effective mask contains the seat | Camera grid, title `t.camera.seatTitle(<seat title>)` |
| Environment and sign-in… | `t.seatMenu.compat` | disabled when not admin | SeatCompatDialog (other part) -> `setSeatCompat` (environment, deny local logon, no power menu); deny allowed per `compat.ts denyLogonAllowed` |
| Turn the virtual gamepad on / off | `t.settingsPage.padsOn` / `padsOff` | always | toggles `local.virtual_pads` in draft |
| Capture the picture inside the session (experimental, restarts the seat) / Show the picture over Remote Desktop (restarts the seat) | `t.settingsPage.captureOn` / `captureOff` | only seat != 1 and local | toggles `local.frame_source` `session_capture` <-> `rdp_decode` (absent) in draft |
| Share this seat's gamepads with every seat / Give this seat's gamepads to this seat only | `t.settingsPage.padsShare` / `padsPassthrough` | only seat != 1 | toggles `local.pad_mode` `shared` <-> `passthrough` (absent) in draft |
| (separator) | | only seat != 1 | |
| Delete the seat's kiosk user… | `t.seatMenu.removeKioskUser` | only seat != 1; danger; disabled while the seat has a kiosk shell | confirm (`t.seatMenu.removeKioskUserTitle(n)`, body `removeKioskUserBody`, button `removeKioskUserConfirm`, danger) -> IPC `remove_kiosk_user {seat}`, toast `removeKioskUserDone` |
| Delete seat… | `t.seatCard.removeMenu` | only seat != 1; danger | confirm (title `t.remove.titleNamed(<title>)`, body `t.remove.body`, button `t.seatMenu.remove`, danger) -> `removeSeat` in draft: seat removed, its non-shared device masks lose that seat (devices become spare); shared stay shared |

Move menu (per device row or monitor face, `moveItems(item, from)`), menu label `t.device.actions(name)`. Policy from `policyOf(kind)`: `exclusive` = monitor, keyboard, mouse, hid; `never_gated` = network; others `shareable`.

| Entry | en.ts key | Shown / disabled | Effect |
|---|---|---|---|
| group header "Move" | `t.move.title` | | |
| one entry per local seat (seat title; hint "Seat N" when named) | `t.move.seatHint` | disabled when device is network (`never_gated`) or already "here" (non-shared mask contains this seat and the device is exclusive or the mask has exactly one seat) | `moveDevice(..., from, seat)` in draft; live region `t.move.announce(name, seat)` |
| To unassigned devices | `t.move.toPool` | disabled for network or when already spare (mask 0) | mask 0 in draft; announce `t.move.announcePool` |
| Share with all seats | `t.move.share` | only shareable kinds; disabled when already shared | mask `0xffffffff` (SHARED) in draft; announce `t.move.announceShare` |
| Assign to seats… | `t.deviceMenu.assign` | hidden for network | Assign dialog |
| Restore default | `t.deviceMenu.reset` | only when the mask source is `explicit` | `clearDeviceMask` in draft: the explicit record is removed; device inherits from hub/port/container or takes its kind default |
| Move the assignment to this port | `t.deviceMenu.moveHere` | one per device warning `moved_from` | App `moveAssignment(old_key, key)` behind the unsaved-changes guard -> IPC `move_assignment {from,to}`, then reload config and devices |
| (separator) | | | |
| Camera preview | `t.deviceMenu.preview` | webcams only; disabled when not plugged in | Camera preview dialog |
| Show (identify) / Stop showing | `t.device.identify` / `t.deviceMenu.indicateStop` | non-webcams; disabled unless kind is monitor or audio_render, present, and admin | IPC `identify_device {key, on}` |
| Change its picture… | `t.deviceMenu.picture` | disabled when the kind has no built-in pictures and no user picture category | PictureDialog (other part) -> `setPicture` (`options.picture` = `builtin:<id>` / `user:<file>` / absent) |
| Device details… | `t.device.infoMenu` | always | Device info dialog |
| (separator) | | monitors only | |
| Use a software mouse pointer / Use the hardware mouse pointer | `t.deviceMenu.softwareCursor` / `hardwareCursor` | monitors only | toggles `options.software_cursor` in draft |
| Display outputs… | `t.deviceMenu.outputs` | monitors only | Outputs dialog (all monitors) |

Moving rules (`model.ts moveDevice`): to pool -> mask 0. To a seat: exclusive kinds, a shared device, or a move from the pool -> mask = only that seat. A shareable device moved from one seat to another keeps its other seats (from-seat removed, to-seat added). Exclusive kinds are coerced to one seat (lowest bit wins; shared -> seat 1). Moving a split monitor to a single mask ends its split (`options.areas` removed).

**States**
- Loading: no canvas/draft and service reachable -> `t.loading` "Loading…".
- Not elevated and refused: `store.needsElevation` -> `t.adminToView` "Restart as administrator to see the seat settings (button above)."
- Service down: nothing rendered in the body (service banner is the global part).
- Read-only (not admin): page renders; disabled: Add seat, wizard buttons, identify, Number the monitors, Setup wizard, Compare, USB dock wizard, Stop all, Turn off, Start seats, Turn on and start, Restart (card), Environment and sign-in. Draft-only menu entries (move, rename, pads, etc.) are not disabled by admin state in this file (save is gated elsewhere; unclear in code whether a non-admin can stage edits).
- Product off (`disabled`): status line off + "Turn on and start"; Add seat secondary; "Start this seat" disabled.
- Safe mode: status line kind warn with product label, no action.
- Seat limit: Add seat disabled with `t.canvas.seatLimit`.
- One local seat: invitation section `t.canvas.addSecondTitle` / `addSecondBody`.
- Seats hidden by the view pref `visibleSeats` are not drawn (seat 1 is always drawn).
- Remote seats section only when at least one remote seat.
- Demo: unclear in code (a `demo.ts` API exists; this file has no demo branch).

**Messages, warnings, codes**
- Notices (page head): while finding by touch: info "`t.canvas.pickByTouch`:" + `t.canvas.pickByTouchHint` "press a key on the keyboard you want to find, or move the mouse. The device found is highlighted." with action `t.canvas.pickCancel`. Seat 1 advice (`seatOneFirstMonitorAdvice`: 2+ present monitors and the first monitor by label, e.g. 1.1, is not in seat 1): `t.seatOneAdvice` "For stable operation, give the first monitor of the first graphics card (1.1) to seat 1."
- Status line (`board.ts powerLine`): `t.power.on` "Pulsar Seat is on" with summary `t.power.summary(running, blocked)` e.g. "2 local seats are running, Seat 3 cannot start" (blocked = autostart or seat 1, not up, no screen or no keyboard) or `t.power.firstOnly` "Seat 1 is running, no other seat yet" when only one local seat; `t.power.off` "Pulsar Seat is off" with `t.power.offSummary` when >1 seat; safe mode = product label.
- Password failing line in a card: `t.account.passwordFailing` "The saved password was rejected at the last sign-in" (seat in `view.password_failing`).
- Toasts: `t.canvas.pickFound(name)` "Found: <name>"; `t.usbDock.done(n)` "<n> dock(s) assigned. Save to apply."; `t.seatMenu.resetGraphicsSent`; `t.seatMenu.removeKioskUserDone`; `t.start.queued`; `t.start.safeMode`; `t.start.disabled`; `t.adminNeeded`; seat error toasts `"Seat N: <failure label>"` from `seat_state` events.
- Keyboard hint line: `t.canvas.moveHint` "To move a device without dragging, focus it, press Enter and choose the seat in the “Move” menu." ("Enter" rendered as a key cap).
- Remote section: title `t.seatKind.remoteList` "Remote seats", help `t.canvas.remoteHint`; row help "Seat N · <state label>"; status `t.seatCard.running` / `t.seatCard.off`.

**Dependencies**
- Service: status (`get_status`), devices (`list_devices`), config (`get_config`; readable by standard users unless restricted, redacted, since 2026-11-25), events. Admin for every IPC action.
- Only `kind === 'local'` seats get cards and devices. Remote seats are listed only.
- Session backend `desktops` adds "Kiosk seat…".
- Missing-device lines depend on active backends (`status.active`): no "no screen" line when display backend is `console_only`; no keyboard/mouse lines when input backend is `none`.
- Platform: none in this file (see PART platform).

**Live behaviour**
Seat status, facts, device rows (plug/unplug, activity pulse, "New" flash), restart-required marks and the status line update from service events; see PART live.

#### Seat card (`SeatCard.svelte`)

**Purpose** One local seat: title, one status word, a fact line, the seat menu, optional Restart, the monitor faces, device rows (non-monitor, tree order), missing-device lines, password-failing line.

**Opens when** Rendered per visible local seat on the Seats board.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Title | text (h2) | seat name, or `t.seatN(n)` "Seat N" when name is empty or equals the number (`board.ts seatTitle`) | "Seat N" | accessible name of the card section |
| Status word | status (kind + text) | see below | | |
| Fact | text | see below | | |
| Seat menu | menu, label `t.seatCard.actions(title)` "<title>: seat actions" | entries above | | |
| Restart (`t.seatCard.restart`) | compact button | shown only when the seat is in `store.restartRequired` (a save needs a restart); disabled when not admin or busy | | App `store.restartSeat(n)`: local seat -> IPC `relogon_seat` then poll `get_status` every 500 ms up to 60 s until state is not `off`/`stopping`; remote seat -> `stop_seat`, wait for off/error (60 s), `start_seats`. Timeout error `t.seatCard.restartTimeout` "The seat did not stop in time. Try Restart again." |
| Drop zone | whole card | device drags only; ignored when dropped on its own source | | `moveDevice(..., from, seat)` + announce |

Status word (`board.ts seatStatus`), first match wins:
1. restart required -> warn `t.seatCard.restartNeeded` "Restart required"
2. state at_logon / signed_in / locked -> ok `t.seatCard.running` "Running"
3. state error -> danger `t.seatState.error` "Error"
4. other non-off state -> neutral `t.seatState[state]` (Waiting for a monitor / Starting / Stopping)
5. health no_display or no_keyboard -> warn `t.seatCard.blocked` "Cannot start"
6. else neutral `t.seatCard.off` "Off"

Fact (`board.ts seatFact`), joined by " · ": "Seat N" when the seat is named; then at_logon -> `t.seatCard.atLogon` "at the sign-in screen"; signed_in/locked -> "Signed in: <user>" / "Locked: <user>"; error -> "Error (<failure text>)" (for `loopback_listener_unavailable` also the listener state text from `loopbackStateText`); starting -> "Starting (<phase>)" with phases `t.phases` (queued, waiting for the host, binding the monitor, starting the seat host, connecting, waiting for the session, signing in); otherwise the auto sign-in account (`user` or `domain\user`) when account kind is not `logon_screen`.

**Actions** Seat menu and Restart, see Seats board.

**States** Drop-target active (while a drag is over it). Empty seat: only missing-device lines show. Restart-required. Password failing line.

**Messages, warnings, codes** `t.seatCard.*`, `t.seatState.*`, `t.phases.*`, `t.account.passwordFailing`.

**Dependencies** Local seats only. Status from service runtime; health computed client-side from the draft (`model.ts seatHealth`).

**Live behaviour** Status/fact update on `seat_state` events; restart mark clears when the seat goes `off` or `starting`.

#### Unassigned devices pool (`Pool.svelte`)

**Purpose** List of devices no seat has (spare, mask 0), devices every seat shares (SHARED mask), network adapters, and devices whose mask names only seats that do not exist.

**Opens when** Always at the top of the Seats board when a draft and canvas exist.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Section heading `t.canvas.system` "Unassigned devices", help `t.canvas.systemHint` "Devices that no seat has, or that all seats share." | text | | | |
| Device rows | DeviceRow list, tree order | | | |
| Drop zone | whole section | device drags only; ignored if dragged from the pool | | mask 0 (spare) in draft, announce `t.move.announcePool` |

**Actions** Drop. **States** Drop-target active; empty list (no empty text). **Messages** see above. **Dependencies** View pref `showAllHubs` hides empty hubs; `sharedInSeats` additionally lists shared devices inside every seat card. **Live behaviour** device events.

#### Device row (`DeviceRow.svelte`)

**Purpose** One non-monitor device (in a seat card) or any device (in the pool).

**Opens when** For each device in the pool and each non-monitor device of a seat.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Row | button (`role=button`, focusable), draggable | click, Enter, Space, ArrowDown, ContextMenu key, Shift+F10, right click | | opens the Move menu anchored at the row; `aria-expanded` reflects it |
| Drag | HTML5 drag, payload `application/x-pulsar-device` `{key, from}` | | | drop on pool/seat |
| Picture | DeviceIcon | built-in per kind, built-in alternative, or user picture | kind picture | |
| Name | text | wraps | | |
| Meta line | text + icons | in order: "Found" (`t.device.found`) when identified or found by touch; "New" (`t.device.arrived`) when just plugged in; "Moved, not saved" (`t.device.moved`) when its effective seats differ from the saved config, else the location label (e.g. `USB-2-1-1`, `1.2`), else the kind word `t.kinds[kind]`; then VID:PID or hardware id when "show IDs" pref is on; icons: share (`t.device.shared` "Shared: every seat uses it"), unplug (`t.device.unplugged` "Not plugged in"), alert (`t.canvas.attention` "Needs attention") | | |
| Tooltip | text | `describe.ts deviceLines` (kind+name, seat, location, adapter/connection/resolution for monitors, ports and plugged children for hubs, connection type for input, drive letters, serial, hardware id when show IDs, "Not plugged in") | | |

"Needs attention" shows when the device has any `warnings` (`key_collision`, `foreign_filter`, `hidden_by_other_software`, `not_routable`, `moved_from`) or a non-warning validation issue (local or server) names it.
Indentation: devices plugged into a hub listed in the same container are shown under it (`board.ts treeOrder`), max depth 3; a parent cycle is listed flat.
"Not in use" look: not present, or a display with presentation `none` that is not on the console desktop.

**Actions** Opens Move menu (entries in Seats board). **States** found/highlight, arrived ("New", lasts `flashSeconds`), moved, unplugged/off, dragging, activity pulse (1.2 s). **Messages** as above. **Dependencies** none. **Live behaviour** activity pulse on each `device_activity` event; arrival highlight.

#### Device picture (`DeviceIcon.svelte`)

**Purpose** Line picture per device kind; inactive variant when not in use.

Kind pictures: monitor, keyboard, mouse, hid, audio_render, audio_capture, webcam, smart_card_reader, usb_hub, storage, printer, serial_port, network, other (fallback). Built-in alternatives (`options.picture = builtin:<id>`): monitor-speakers, monitor-curved, tv, laptop, keyboard-compact, keyboard-numpad, keyboard-laptop, mouse-wireless, mouse-vertical, trackball, touchpad, trackpoint, pen, touchscreen, gamepad, headphones, speaker-single, headset, webcam-clip. User picture (`user:<file>` from `%ProgramData%\Pulsar Seat\pictures\<category>\`, Tauri `list_user_pictures`) drawn instead, scaled without keeping aspect. Decorative (`aria-hidden`).

**Opens when / Controls / Actions** None (display only). **States** active / inactive. **Messages** None. **Dependencies** user picture files. **Live behaviour** None.

#### Monitor face (`MonitorFace.svelte`)

**Purpose** A monitor of a seat as a screen thumbnail with the seat number (pulse rings, the same number "Number the monitors" paints on the real screen), name and location ID.

**Opens when** For every monitor in a seat card.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Face | button, draggable | click, Enter/Space (native button), ArrowDown, ContextMenu, Shift+F10, right click | | Move menu; aria-label `t.device.faceActions(name, id)` "<name> (<id>): move or details" |
| Split part | text | when the monitor is a valid split, this seat's area: `t.outputs.split.areaName` (left half, top right quarter, ...) and a smaller seat number in that area | | |
| Meta | text/icons | "Found", "Moved, not saved", unplugged icon, attention icon | | |

**Actions / States / Messages** as Device row (no "New" flash, no activity pulse on faces). **Dependencies** split view only when `area.ts checkSplit` is valid. **Live behaviour** identify highlight.

#### Missing-device line (`MissingSlot.svelte`)

**Purpose** One line per device kind a seat lacks (from its present devices plus shared pool devices).

| Kind | Blocks start | Text |
|---|---|---|
| display | yes | `t.slot.displayTitle` "No screen." + `t.slot.display` "This seat cannot start without a screen. Drag one here from the unassigned devices." |
| keyboard | yes | `t.slot.keyboardTitle` "No keyboard." + `t.slot.keyboard` |
| mouse (mouse or HID digitizer page 0x0D) | no | `t.slot.mouseTitle` "No mouse." + `t.slot.mouse` "Without a mouse the seat can only be used with the keyboard." |
| audio (audio_render) | no | `t.slot.audio` "No sound device (optional)." |

**Opens when** Inside each seat card. **Controls/Actions** None. **States** blocking vs. optional. **Dependencies** display line omitted under display backend `console_only`; keyboard and mouse lines omitted under input backend `none`. **Live behaviour** recomputed on device/draft change.

#### Assign dialog (`AssignDialog.svelte`)

**Purpose** Set exactly which seats a device (or its hub port, or its whole physical device) belongs to.

**Opens when** Move menu "Assign to seats…" (not for network adapters); also when "Find by touch" finds a device that is not visible on the board.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| What are you assigning? (`t.assign.target`) | choice list | "Only this device" (`targetDevice`), "This port" (`t.canvas.assignPort`, when `view.port_key`), "The whole device" (`t.canvas.assignContainer`, when `view.container_key`); shown only when a port or container key exists and the device is not network | Only this device | switching reloads the current mask of that target (absent = 0) |
| Target hint | text | `t.assign.targetHint` when target is port/container | | |
| Seats (exclusive kinds) | single choice | "No seat (spare)" (`t.assign.noSeat`) or each seat `t.assign.seatOption(n,name)` "Seat N" / "Seat N — name" | current single seat or spare | help `t.assign.exclusiveHint` |
| Seats (shareable kinds, and any port/container target) | checkboxes | "All seats" (`t.assign.allSeats`, = SHARED mask; disables the others) + one per seat | from current mask | help `t.assign.shareableHint` |
| Network | text only | `t.assign.networkHint` "Network adapters are always shared; shown for information only." | | |

Seat list includes all seats (local and remote).

**Actions** Save (`t.save`) -> device: `setDeviceMask` in draft; port target: `setKeyMask(key, 'usb_hub', mask, "This port")`; container target: `setKeyMask(key, 'other', mask, "<device> (The whole device)")`; mask 0 removes the port/container record. Cancel (`t.cancel`). Network: only Close (`t.close`).

**States** network read-only. **Messages** title `t.assign.title(name)` "<name>: seat assignment". **Dependencies** none. **Live behaviour** None.

#### Device info dialog (`DeviceInfoDialog.svelte`)

**Purpose** Read-only details of one device.

**Opens when** Move menu "Device details…".

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Description lines | key/value list | `deviceLines` split at ": " | | |
| Connection path (`t.deviceMenu.path`) | ordered list | device, then each hub up to the root (name + label), or the label, or `t.none` | | |
| Sharing state (`t.deviceMenu.sharing`) | text | mask label ("Spare" `t.canvas.spareSign` / "All seats" / "Seat 2" / "Seats 2, 3") · source ("Assigned by hand" `t.describe.explicit`, "Inherited from hub X" `t.canvas.inherited`, "Inherited from the port it is plugged into (<port>)" `inheritedPort`, "Inherited from the assignment of the whole device" `inheritedContainer`, "Default assignment" `defaultMask`) + isolation text | | |
| Hardware IDs (`t.describe.ids`) | text | device key and instance id; only when "show IDs" pref on | | |

Isolation texts `t.describe.isolation`: descriptor, session_letter, minifilter, camera_consent, shared, not_supported ("This device cannot be closed to a seat on this PC"), network, seat_filter.

**Actions** "Show (identify)" / "Stop showing" (only monitor or speaker, present, admin) -> IPC `identify_device`; Close.

**States** identifying toggles the button label. **Messages** sub `t.device.infoSub` "The details help when you look for a problem." **Dependencies** admin for identify. **Live behaviour** identify state.

#### Display outputs dialog (`OutputsDialog.svelte`)

**Purpose** Per monitor: which graphics-card output feeds it, cursor type, HDR, VRR, split screen; plus machine-wide output options.

**Opens when** Move menu of a monitor "Display outputs…".

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Output for <monitor> (`t.outputs.outputFor`) | select | "Automatic" (`t.outputs.automatic`, = none) or each output of the same adapter, label "<tech> · <label or target id>" + " (current)" for its own | saved `options.outputs[0]` | `options.outputs` (single) |
| Outputs (cloning) | checkboxes | shown instead of the select when cloning is allowed; several outputs allowed | saved list | `options.outputs` (sorted, unique) |
| Software mouse pointer (`t.outputs.softwareCursor`) | checkbox | | saved / false | `options.software_cursor` |
| HDR (`t.outputs.hdr`) | checkbox | | false | `options.hdr` |
| Variable refresh (VRR) (`t.outputs.vrr`) | checkbox | | false | `options.vrr` |
| Split screen (SplitPicker) | 4 selects | see Split picker | from `options.areas` | `options.areas` + mask |
| Allow cloning (`t.outputs.allowClone`) | switch | | false | `settings.outputs.allow_clone` |
| Reconnect each monitor to its saved output when seats start (`t.outputs.forceAtStart`) | switch | help `t.outputs.forceNote` | false | `settings.outputs.force_assign_at_start` |
| Monitor resolution follows the seat's session (`t.outputs.followMode`) | select | auto ("Automatic (only monitors outside the desktop)"), on, off (`t.outputs.followModeChoice`) | auto (field absent) | `settings.outputs.follow_session_mode` (absent / true / false) |
| Session frame rate (whole PC) (`t.outputs.sessionFps`) | select | Automatic (fastest seat monitor), Smooth (250 per second), fixed 60/75/90/100/120/144/165/240 (+ saved custom value), Windows default (about 30 per second) | auto, 60 (field absent) | `settings.outputs.session_frame_rate {mode, fps}`; help `t.outputs.sessionFpsNote` |

Monitors are grouped per graphics card (adapter LUID); heading "Graphics card: <name>" (in the subtitle when only one). Each monitor row shows name, location label, and `t.outputs.status.assigned` / `unassigned`.

**Actions** Save (writes all changed monitor options and output settings into the draft; only changed values are written) / Cancel. Save disabled when cloning blocked or a split is invalid.

**States** No monitors -> `t.outputs.none`. Split monitor -> output control replaced by `t.outputs.split.outputFixed` "A split monitor keeps its current output." and its outputs are saved empty.

**Messages, warnings, codes** intro `t.outputs.intro`; `t.outputs.hdrVrrHint` (HDR/VRR only for seats 2+, needs HDR monitor and Windows 11, no HDCP content); `t.outputs.split.note`; danger `t.outputs.cloneNeeded` "Allow cloning first to use more than one output." (a monitor has >1 output with cloning off); danger `t.outputs.split.invalid`.

**Dependencies** Applied by the service at the next seat start / reboot. HDR needs Windows 11 (text only, no code gate). Output tech names `t.outputTech` (VGA, DVI, HDMI, Built-in screen, DisplayPort, Miracast (wireless), Indirect display, Other).

**Live behaviour** None (snapshot of devices at open).

#### Split screen picker (`SplitPicker.svelte`)

**Purpose** Split one monitor between 2 to 4 local seats as a 2x2 grid.

**Opens when** Inside Outputs dialog, per monitor.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| 4 cell selects (top left, top right, bottom left, bottom right; label `t.outputs.split.cellFor(name, cell)`) | select | "Nobody" (`t.outputs.split.nobody`) or "Seat N" for each local seat | from saved areas, else all Nobody | grid |

Rules (`area.ts splitFromGrid` / `checkSplit`, mirror of seat-core): fewer than 2 distinct seats -> not split (the one seat, if any, gets the whole monitor). Each seat's cells must form a half (left, right, top, bottom) or a quarter; else invalid "shape". Seat 1 may share only half-and-half with exactly one other seat on the opposite half; else invalid "console_share_not_half". Max 4 seats. A split cannot have clone outputs. A shared (all-seats) monitor cannot be split. Service codes: `screen_area_overlap`, `console_share_not_half`, `invalid_screen_area`.

**States / Messages** `t.outputs.split.off` "Not split: the whole monitor belongs to one seat."; `t.outputs.split.on(n)` "Split between n seats."; `t.outputs.split.shape`; `t.outputs.split.consoleHalf`.

**Actions** none (value bound to the dialog). **Dependencies** local seats only. **Live behaviour** state text updates as cells change.

#### Camera preview (`CameraPreview.svelte`)

**Purpose** Live picture of one webcam (the "identify" of a camera).

**Opens when** Move menu "Camera preview" of a present webcam.

**Controls** None besides Close.

**Actions** Close (stops the stream).

**States** starting (`t.camera.starting` "Opening the camera…" with spinner); live; denied (`t.camera.denied`); unsupported (`t.camera.unsupported`); fallback when no WebView camera label matches the device name: `t.camera.notFound` "…showing the first camera."

**Messages** title `t.camera.title(name)` "<name>: camera preview".

**Dependencies** WebView `getUserMedia` in the manager window (not an IPC). Camera picked by name match (exact, then contains). Header comment: untested on hardware.

**Live behaviour** live video until closed.

#### Camera grid (`CameraGrid.svelte`)

**Purpose** Live pictures of all cameras, or of one seat's cameras, at once.

**Opens when** Overflow "Show all cameras" (title `t.camera.allTitle`) or seat menu "Show this seat's cameras" (title `t.camera.seatTitle(seat)`; names = present webcams whose effective mask contains the seat).

**Controls** None besides Close.

**Actions** Close (stops every stream).

**States** starting; denied; unsupported; live with no tiles -> `t.camera.none` "No camera is connected for this view."; per tile: waiting spinner, failed -> `t.camera.busy`, or video with its label.

**Dependencies** WebView `getUserMedia`, one stream per camera. Untested on hardware.

**Live behaviour** live video tiles.

#### USB dock wizard (`UsbDockWizard.svelte`)

**Purpose** Give each multi-seat USB dock (a USB hub with a monitor plus keyboard/mouse/HID or sound behind it) to one seat in one step.

**Opens when** Overflow "USB dock wizard…" (admin only).

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Look again (`t.usbDock.rescan`) | compact button | disabled while scanning | | IPC `list_devices` again; adds the dock count to a history (last 5) |
| Display settings (`t.usbDock.display`) | compact button | | | `open_system_panel display_settings` |
| Seat (per dock) | select | local seats except 1 (label "N", plus " (already has a monitor)" `t.usbDock.hasDisplay` when that seat already has a single-seat present monitor and is not this dock's seat), "New seat" (`newSeat`), "Leave as it is" (`skip`) | proposed: the seat owning all dock monitors, else the next free local seat (not 1, without a monitor, not used by another dock), else New seat | |
| Name (`t.seatName.label`) per dock | text input | max 64 chars | empty | new seat name, or renames the chosen seat when not empty |

**Actions** Assign (`t.usbDock.apply`, disabled when no docks or not admin) -> `docks.ts assignDocks` in draft: new seats added first (skipped at the 16 limit), the dock's container key (or hub key) gets the seat as a `usb_hub`/`other` record, and every monitor, input and sound device of the dock gets that seat; then toast `t.usbDock.done(n)`. Cancel.

**States** scanning (`t.usbDock.scanning`); found `t.usbDock.found(n)` + `stable` "The count is stable." (last 2 scans equal) or `unstable`; none `t.usbDock.none`; not admin footer `t.notAdmin`.

**Messages** title `t.usbDock.title`, intro `t.usbDock.intro`, per dock `t.usbDock.dock(label)` (label = hub serial, instance id or name), `t.usbDock.contents(m,i,a)`.

**Dependencies** admin. Nothing is sent until Save.

**Live behaviour** scan on open; manual rescans.

#### View settings dialog (`ViewSettingsDialog.svelte`)

**Purpose** Per-user view preferences of the Seats page (stored in the browser storage key `pulsar-seat.manager.prefs.v1`), applied at once.

**Opens when** Overflow "View settings…" (anyone).

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Show all USB hubs (`t.view.showAllHubs`) | switch | help `t.view.showAllHubsHint` (when off, empty hubs and hubs with only hubs are hidden) | on | pref `showAllHubs` |
| Also show shared devices in the seat boxes (`t.view.sharedInSeats`) | switch | | off | pref `sharedInSeats` |
| Show hardware IDs next to device names (`t.view.showIds`) | switch | | off | pref `showIds` |
| Highlight time of a just plugged-in device (s) (`t.view.flashSeconds`) | number | 2..20, step 2; rounded to even and clamped | 4 | pref `flashSeconds` |
| Seats shown (`t.view.visibleSeats`) | checkboxes per local seat | seat 1 always checked and disabled | all | pref `visibleSeats` (bit mask) |

**Actions** Close only. **States** None. **Messages** title `t.view.title`. **Dependencies** none (local prefs). **Live behaviour** board updates immediately.

#### USB input emergency restore (outside this board; pointer)

Not on the Seats board. Lives in `AdvancedTab.svelte` (another part): row `t.advanced.restoreUsb` "Give USB keyboards and mice back to Windows", button `t.advanced.restoreUsbConfirm`, admin only, confirm `t.advanced.restoreUsbTitle`/`restoreUsbBody` -> IPC `restore_usb_input {confirm:true}`; result `failed[]` and `capture_held`. Toast `t.advanced.restoreUsbDone` or `restoreUsbFailed(n)`. Held state text `t.notes.usb_capture_held` (key line 1234) "USB keyboards and mice were given back to Windows and stay with seat 1 until a seat starts". Only meaningful with input backend `winusb_capture`.

#### Keyboard and mouse moved dialog (`App.svelte`, input-move confirm)

**Purpose** Confirm a hotkey move of input devices the service already applied.
**Opens when** service `notice` event with code `input_move_pending` (seat in data, default 1), only for an admin, and no other App dialog open.
**Controls** None. **Actions** Save (`t.inputMoveDialog.save`) -> IPC `resolve_input_move {accept:true}`; Cancel (`t.inputMoveDialog.discard`) -> `{accept:false}` (devices go back); both reload config.
**States** None. **Messages** title `t.inputMoveDialog.title` "Keyboard and mouse moved", body `t.inputMoveDialog.body(seat)`.
**Dependencies** admin, hotkey feature. **Live behaviour** event-driven.

#### Same account dialog (`App.svelte`, same-account confirm)

**Purpose** Warn that the account of this manager's seat is also signed in on another seat.
**Opens when** service `notice` code `same_account_warning` for the manager's own seat (or no seat while the manager has one), pref `hideSameAccountWarning` off, no other dialog open.
**Controls** "Don't show again" checkbox (`t.sameAccountDialog.dontShow`) -> pref `hideSameAccountWarning`.
**Actions** Sign out (`t.sameAccountDialog.signOut`) -> IPC `relogon_seat` for the manager's seat; Keep signed in (`keep`) closes.
**Messages** title/body `t.sameAccountDialog.title` / `body`.
**Dependencies** policy `same_account` (BasicTab, other part). **Live behaviour** event-driven.

### 3.3 Local seat settings and system settings

Scope: the Settings page (`BasicTab.svelte`), its Advanced sub-page (`AdvancedTab.svelte`, `NetworkSection.svelte`, `CompatSettings.svelte`) and every settings dialog opened from them. Base dir for files: `apps/manager/src/lib`.

General rules that apply to every screen in this part:

- **Draft and Save bar.** Every config edit goes through `store.edit(...)` into an in-memory draft (`store.draft`). Nothing reaches the service until the user presses Save in the docked save bar (`t.saveBar.save`, keyboard shortcut Ctrl+Shift+S, `t.saveBar.shortcut`) or "Undo" (`t.saveBar.undo`) discards it. The save bar counts changes as "one device assignment, one seat or one top-level settings group" (`changes.ts` `summarizeChanges`, `t.saveBar.count`). A pending password counts as one change.
- **Save flow** (`store.svelte.ts` `save`): IPC `validate_config` first; any `error` issue blocks the save (toast `t.saved.errors` "Not saved: fix the problems below first."); `conflict` issues open `ConflictDialog`; then `apply_config {config, base_revision, force}`; then pending passwords are sent with `set_seat_password`. Success toast `t.saved.ok` "Settings saved." unless the per-user pref "Do not show the success message after saving" is on.
- **Seat restart after save.** The service returns `restart_seats` only for running seats whose monitors changed or whose `local.frame_source` changed (`seat-service/src/requests.rs`). Those seats are marked "restart required" (toast `t.saved.restartSeats`). All other fields in this part need no seat restart from the manager's point of view; when they take effect is described per field (from en.ts hints / architecture.md). `reboot_needed` is always `false` from this path today; toast `t.saved.reboot` exists.
- **Immediate (not via Save bar):** per-user manager preferences (theme, language, ask-before-start, hide tray icon, no success message), help links, Windows panel links, "Restart Remote Desktop Services", "Give USB keyboards and mice back to Windows", Hyper-V actions, Steam add-on DLL actions, and loading lists (users, addresses, CPU sets, pictures).
- **Read-only (not admin).** `readOnly = !store.isAdmin`. When the config is visible but the user is not an elevated admin, editable sections are wrapped in an `inert` container (no interaction possible) and an info notice is shown. When the config itself is hidden (`store.needsElevation`), the page shows `t.adminToView` "Restart as administrator to see the seat settings (button above)."
- **Validation mirror.** `model.ts` `localIssues` and `compat.ts` `compatIssues` mirror the service's `seat_core::validate` with the same issue codes; the service result (`validate_config`) is authoritative. Issue texts come from `issueText` in en.ts (via `issueLabel`, prefixed "seat N: ").

#### Settings page (`BasicTab.svelte`)

**Purpose**
Machine-wide start mode, one row per seat (auto-start, sign-in account, seat menu with all per-seat options), kiosk options, behaviour switches, appearance (per-user), maintenance links, help links and reset actions. Title `t.settingsPage.title` "Settings", sub `t.settingsPage.intro` "The defaults suit most setups."

**Opens when**
App navigation view `settings` (rail entry "Settings", `t.nav.settings`). Any user can open it; editing needs an elevated admin. Seats board "open settings" command also navigates here (App.svelte).

**Controls**

Section "Start" (`id=start`, `t.settingsPage.startup`, help `t.startup.question` "When should the extra seats start while Pulsar Seat is on?")

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Start mode | radio list (ChoiceList `start-mode`) | `manual` "By hand", `at_boot` "When Windows starts", `after_seat_one_logon` "When someone signs in on seat 1" (`t.settingsPage.startModeLabels`); `manual` carries the note `t.startup.recommended` "recommended for the first setup"; each option has help `t.startModeHints.<mode>` | `manual` (seat-core) | `settings.start_mode`; Save bar |

Section "Seats" (`id=seats`, `t.settingsPage.seats`, help `t.settingsPage.seatsHelp`). One row per seat in `draft.seats` (local and remote).

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Add seat (section header button) | button | disabled with reason `t.settingsPage.seatLimit` "A PC can have at most 16 seats." when 16 seats exist | — | `addSeat()`: lowest free number 1..16, new local seat (name = number, autostart true, logon screen, no CPU limit, wallpaper off); Save bar |
| Seat label line | text | name (`seatDisplayName`: own name, or "Seat N" when name equals the number) + fact line (see Messages) + live runtime state (`seatLabel(state)`) | — | read only |
| Starts automatically (seats 2+) | switch, aria-label `t.settingsPage.autoStartFor(name)` | on/off | `true` for a new seat | `seats[].autostart`; Save bar |
| Seat 1 auto-start | locked value `t.settingsPage.alwaysOn` "Always starts" | not editable; `normalize()` forces seat 1 `autostart = true` | true | — |
| Account | button, aria-label `t.settingsPage.accountFor(name)` | — | — | opens Seat account dialog |
| Seat menu | menu button, label `t.settingsPage.seatMore(name)` | items below | — | — |
| Kiosk audio line (only when session method = `desktops` and seat has kiosk shell) | text | `t.kiosk.audio.routed(render, capture)` / `noEndpoint` / `unsupported`; extra warning `t.kiosk.audio.sharedApp(seats)` | — | derived (`kioskAudio`) |
| Kiosk account line (same condition) | text | `t.kiosk.account.console` / `seat(user)` / `auto(user)`; for `seat` also warning `t.kiosk.account.personalWarning` | — | derived (`kioskAccountOf`) |

Seat menu items (`seatMenu()`), in order:

| Item | Shown when | Effect |
|---|---|---|
| `t.settingsPage.renameMenu` "Rename…" | always | Seat name dialog |
| `t.settingsPage.startupAppsMenu` "Startup programs…" | always | Startup programs dialog |
| `t.settingsPage.kioskMenu` "Kiosk…" | local seat AND (session method `desktops` OR seat already has `local.kiosk_shell`) | Kiosk seat dialog |
| `t.settingsPage.redirectMenu` "Redirection…" | local seat AND seat ≠ 1 | Redirection dialog |
| `t.settingsPage.ipMenu` "IP address…" | always | IP address dialog |
| `t.settingsPage.cpuMenu` "Processor cores…" | always | CPU cores dialog (all seats at once) |
| `t.settingsPage.gpuMenu` "Graphics card preference…" | local seat | GPU preference dialog |
| `t.settingsPage.padIgnoreMenu` "Gamepads that SDL games ignore…" | always | Pad ignore dialog |
| `t.seatCompat.menu` "Environment and sign-in…" | always | Environment and sign-in dialog |
| separator | — | — |
| `t.settingsPage.padsOn` / `padsOff` "Turn the virtual gamepad on/off" | local seat (seat 1 included) | toggles `local.virtual_pads` (`setVirtualPads`; off = field removed); Save bar |
| `t.settingsPage.wallpaperOn` / `wallpaperOff` "Turn the seat wallpaper on/off" | seat ≠ 1 (local or remote) | toggles `seats[].wallpaper` (`setWallpaper`); Save bar |

Kiosk seats sub-group (only when `settings.backends.session === 'desktops'`), sub-label `t.settingsPage.kioskSeats`:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.kiosk.seatOneDesktop` "Seat 1 stays a normal desktop" (help `t.kiosk.seatOneDesktopHint`) | switch | on/off | `true` (absent = true) | `settings.kiosk.seat1_desktop`; Save bar |
| `t.kiosk.perSeatAccounts` "Each kiosk seat runs as its own Windows user" (help `t.kiosk.perSeatAccountsHint`) | switch | on = `per_seat`, off = `shared` | `per_seat` (absent = per_seat) | `settings.kiosk.accounts`; Save bar |

Section "Behaviour" (`id=behaviour`, `t.settingsPage.behaviour`)

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.basic.adminOnly` "Only administrators can see and change the settings" | switch | on/off | off | `settings.admin_only`; Save bar |
| `t.basic.ownSeat` "Users can restart their own seat" | switch | on/off | on | `settings.users_may_control_own_seat`; Save bar |
| `t.basic.askOnStart` "Ask before starting seats by hand" | switch | on/off | on | per-user pref `askOnManualStart` (localStorage `pulsar-seat.manager.prefs.v1`); immediate |
| `t.basic.sameAccount` "Same account on several seats" (help `t.settingsPage.sameAccountHelp`) | select | `allow` "Allow", `warn` "Allow but warn", `deny` "Do not allow" (`t.basic.sameAllow/sameWarn/sameDeny`) | `warn` | `settings.same_account`; Save bar. `deny` makes duplicate seat accounts an error (`same_account_on_seats`) |
| `t.basic.hideTray` "Hide the notification area icon" | switch | on/off | off | per-user pref `hideTray`; immediate, also calls `set_tray_visible(!v)` |
| `t.settingsPage.noShowGoodApply` "Do not show the success message after saving" | switch | on/off | off | per-user pref `noShowGoodApply`; immediate |

Section "Appearance" (`id=appearance`, not inside the read-only wrapper; visible without a draft)

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.settingsPage.theme` "Theme" | segmented control | `system` "System", `light` "Light", `dark` "Dark" | `system` | per-user pref `theme`; immediate |
| `t.settingsPage.language` "Language" (help `t.basic.languageRestart` "The manager reloads in the chosen language.") | select | `system` "Windows display language", `en` "English", `tr` "Türkçe" | `system` | localStorage lang pref, IPC `set_ui_language`, then the window reloads; immediate |
| `t.settingsPage.lockScreenImage` "Seat picture on the lock screen" (help `lockScreenImageHelp`) | switch; locked value Yes/No when read-only | on/off | off | `settings.tweaks.lock_screen_image`; Save bar. Only when a draft exists |
| `t.basic.screensaver` "Screen saver settings" | link row | — | — | Tauri `open_system_panel('screen_saver')` (`control.exe desk.cpl,,@screensaver`); immediate |
| `t.basic.lockScreenSettings` "Lock screen settings" | link row | — | — | `open_system_panel('lock_screen')` (`ms-settings:lockscreen`); immediate |

Section "Maintenance" (`id=maintenance`)

| Control | Kind | Shown when | Effect |
|---|---|---|---|
| `t.settingsPage.diagnostics` "Diagnostic report" (help `diagnosticsHelp`) | link row | admin | App dialog `diagnostics` |
| `t.settingsPage.updates` "Updates" (help "Version X · check on/off") | link row | admin | App dialog `updates`; help line uses `get_update_status` once on mount (`updatesCheckOn` "check on" / `updatesCheckOff` "check off") |
| `t.settingsPage.import` "Import another configuration" (help `importHelp`) | link row | admin | App dialog `import` |
| `t.settingsPage.advanced` "Advanced settings" (help `advancedHelp`) | link row | always | navigates to the Advanced page |

Section "Help" (`id=help`)

| Control | Kind | Effect |
|---|---|---|
| `t.settingsPage.manual` "User manual" | link row | Tauri `open_help_link('manual')`, or `'manual_tr'` when the active language is Turkish (repository troubleshooting page) |
| `t.settingsPage.website` "Product website" | link row | `open_help_link('website')` |
| `t.settingsPage.contact` "Report a problem or ask a question" | link row | `open_help_link('contact')` (issue tracker) |
| `t.settingsPage.security` "Report a security problem" | link row | `open_help_link('security')` |

All four are disabled while the WebView reports offline; their help line and the section help become `t.settingsPage.helpOffline` "Needs an internet connection; this PC looks offline."

Section "Reset" (`id=reset`, only when a draft exists AND admin)

| Control | Kind | Effect |
|---|---|---|
| `t.settingsPage.clearPictures` "Clear all custom pictures" (help `clearPicturesHelp`) | button `t.settingsPage.clear` "Clear"; when no device has `options.picture` a locked value "—" (`t.none`) instead | App dialog `clearPictures` |
| `t.settingsPage.resetAll` "Reset all settings" (help `resetAllHelp`) | button `t.settingsPage.resetButton` "Reset" | App dialog `reset1` (typed-confirmation dialog, owned by App) |

**Actions**
Covered in the tables. Seat dialogs close on Save after calling the matching `model.ts` setter on the draft (`renameSeat`, `setStartupApps`, `setGpuPreference`, `setRedirect`, `setKioskShell`, `setPadIgnoreDevices`, `setSeatCompat`, `setIpBinding`, `setCpuSets`), or `store.saveAccount` for the account. Cancel closes without change. IP dialog "Edit the program list" closes the dialog and opens Advanced at section `net`.

**States**
- No draft and `needsElevation`: Start/Seats/Behaviour hidden; text `t.adminToView`. Appearance, Maintenance (Advanced link only), Help still shown.
- Read-only (not admin, draft visible): info notice `t.settingsPage.adminOnlyEdit` "Only an administrator can change these settings."; Start/Seats/Behaviour inert; lock-screen image shown as locked Yes/No; Maintenance shows only the Advanced link; Reset hidden.
- Offline: help links disabled (see above).
- Add seat disabled at 16 seats (`t.settingsPage.seatLimit`).
- Demo: unclear in code (no demo branch in this file).

**Messages, warnings, codes**
- Seat fact line parts (`settingsview.ts` `seatFacts`, joined with " · "): `remoteFact` "Remote seat"; `logonScreenFact` "Sign-in screen" or `autoLogonFact(user)` "Signs in automatically: user" (domain as `DOMAIN\user`); `passwordFailingFact` "saved password rejected"; `t.basic.appsCount(n)` lower-cased "n programs"; `kioskFact(n)` "Kiosk: n apps"; `gpuFact` "graphics card preference" (mode ≠ off); `padsFact` "virtual gamepad"; `padsSharedFact` "gamepads shared" (`local.pad_mode = shared`); `captureFact` "picture captured in the session" (`local.frame_source = session_capture`); `redirectFact` "redirection on" (seat ≠ 1 and any redirection on); then the runtime state label.
- `t.basic.virtualPadsHint` (info notice, when any seat has virtual pads): needs ViGEmBus or usbip-win2 installed (not installed by Pulsar Seat).
- `t.kiosk.systemKeys` (info notice, when session method is `desktops`): Ctrl+Alt+Del and Win+L cannot be blocked.
- `t.kiosk.backendOnly` (warning notice, when the method is not `desktops` but some seat has a kiosk shell): "Kiosk seats only work with the “Concurrent kiosk seats” session method."
- Kiosk audio: `routed` "Sound: output X, microphone Y"; `noEndpoint` "Sound: no separate sound device given to the seat (seat 1’s default device)"; `unsupported` "Sound: no per-app sound device on this Windows version (seat 1’s default device)" (backend note `kiosk_audio_shared`); `sharedApp(seats)` "The same program is also on seat …: they share the sound device setting".
- Kiosk account: `console` "User: seat 1’s user" (seat 1, or accounts mode shared); `seat(user)` "User: X" (seat has an account with a stored password and the name is not `PulsarKioskNN`); `auto(user)` "User: PulsarKioskNN (Pulsar account)"; `personalWarning` shown with `seat`.

**Dependencies**
- Kiosk rows, kiosk audio/account lines and kiosk menu: session method `desktops`.
- Redirection menu: local seats 2+ only. GPU preference, virtual gamepad: local seats only. Kiosk: local only.
- Remote seats appear in the same list with the subset of menu items above.
- Admin needed for edits; per-user prefs work for everyone.

**Live behaviour**
- Runtime seat state in each row follows `store.status` (event subscription; 3000 ms fallback polling, `POLL_MS`).
- Kiosk audio line follows `store.devices` and the `desktops` backend notes.
- Online/offline follows the WebView `online` / `offline` events.
- Password markers (`password_set`, `password_failing`) come from the config view (`get_config` response).

#### Seat name dialog (`SeatNameDialog.svelte`)

**Purpose** Rename one seat.

**Opens when** Settings seat menu "Rename…"; also from the seats board (`SeatsTab.svelte`). Admin (opened from inert page otherwise impossible).

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.seatName.label` "Name" | text input, `maxlength=80` | after trim 1..64 characters (code points); no control characters U+0000–U+001F, U+007F (`seatNameError`) | current `seats[].name` (new seat = its number as text) | `seats[].name` (trimmed) via `renameSeat`; Save bar |

**Actions** Save (submit; disabled while invalid) → `renameSeat`. Cancel. Enter submits (form).

**States** Inline field error while invalid; Save disabled.

**Messages, warnings, codes** `t.seatName.errors.empty` "The name cannot be empty."; `too_long` "The name can be at most 64 characters."; `control` "The name cannot contain invisible control characters." Service issue `invalid_seat_name` "Invalid seat name (1–64 characters)." Display rule: a name equal to the seat number shows as "Seat N" (`t.seatN`).

**Dependencies** None (local and remote seats).

**Live behaviour** Validation updates on every keystroke.

#### Seat sign-in account dialog (`SeatAccountDialog.svelte`)

**Purpose** Choose logon screen or auto-logon with a local or domain account, and set/keep/clear the seat password. The password is write-only: never put into the config, never shown again.

**Opens when** Settings seat row "Account" button; seats board "Sign-in account…" (`t.seatMenu.account`). Title `t.account.title(n)`; sub = seat name when it differs from "Seat N".

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.account.signIn` "Sign-in" | radio list | `logon_screen` "Show the normal sign-in screen (recommended)" (help `logonScreenHelp` "The user types their own password.") / `auto` "Sign in automatically with this account" | from `seats[].account.kind` | `seats[].account` |
| `t.account.accountType` "Account type" (auto only) | segmented | `local` "Local" / `domain` "Domain" | `domain` if stored kind is domain, else `local` | account kind |
| `t.account.domainName` "Domain" (domain only) | text input | non-empty after trim, ≤256 chars, none of `\ / " @` (`accountError`) | stored domain | `account.domain` |
| User (`t.account.user` "User" for local; `t.account.userName` "Account name" for domain) | select of local users (local scope and the user list loaded and non-empty) or text input | same rule as domain; select has placeholder `t.account.pickUser` (disabled), disabled Windows users are listed but not selectable; label "name (full name) · disabled · administrator" | stored user | `account.user` |
| `t.account.keepPassword` "Keep the saved password" | checkbox, only when a password is stored | — | checked when a password is stored and not failing | keep = password unchanged |
| `t.account.password` "Password" | password input (auto-logon, not keeping) | may be empty (`t.account.emptyOk`) | empty | pending password |
| `t.account.confirm` "Password (again)" | password input | must equal Password | empty | — |

**Actions**
- `t.account.openAccounts` "Open user accounts" (local scope): `open_system_panel('user_accounts')` (`ms-settings:otherusers`); immediate.
- Save (disabled while invalid) → `store.saveAccount(seat, account, password)`: account goes into the draft (Save bar); password semantics: `undefined` keep, `null` clear, string set. Choosing logon screen while a password is stored clears it (`null`). The password is sent with IPC `set_seat_password` only after `apply_config` succeeded; a failed send stays pending.
- Cancel.

**States**
- Loading users: IPC `list_local_users` on open; on failure the user field is a free text input with help `t.account.usersFailed`.
- Invalid: Save disabled; field errors shown only when the field is non-empty.

**Messages, warnings, codes**
- `t.account.seatOneWarning` (warning, seat 1 with auto-logon): "Automatic sign-in is not recommended on seat 1; use the normal sign-in screen here."
- `t.account.passwordFailing` (warning, under the keep checkbox): "The saved password was rejected at the last sign-in".
- `t.account.invalidUser`, `t.account.invalidDomain`, `t.account.mismatch` "The passwords do not match."
- Help `t.account.emptyOk` + `t.account.savedHint` "The password goes only to the service, right away; it is never written to files and cannot be shown later."
- Service issues: `invalid_account`, `same_account_on_seats` (policy deny), `same_account_fast_user_switch` (session method `fast_user_switch` needs one account per seat), `loopback_no_accounts` / `logon_screen_needs_listener` (see Advanced).

**Dependencies** Domain accounts are free text (no lookup). Local users list from the service.

**Live behaviour** None beyond the one-time user list load.

#### Startup programs dialog (`StartupAppsDialog.svelte`)

**Purpose** Programs started in the seat session after sign-in, inside the seat's job (CPU limits).

**Opens when** Settings seat menu "Startup programs…". Title `t.startupApps.title(n)`.

**Controls** (table, one row per program)

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.startupApps.program` "Program path" | text input (mono) | empty allowed (row dropped on save); otherwise absolute Windows path: `X:\` / `X:/` or `\\server\share…` (`isAbsoluteWindowsPath`) | — | `seats[].startup_apps[].command` (trimmed) |
| `t.startupApps.args` "Arguments" | text input | whitespace separates, double quotes group (`parseArgs`); shown back with `formatArgs` | — | `startup_apps[].args` (string array) |
| Remove (trash) | icon button, label `t.rowAction.remove(command or "row N")` | — | — | removes row |
| `t.startupApps.add` "Add program" | button | no count limit in code | — | adds empty row |

**Actions** Save (disabled while any row invalid) → `setStartupApps` (empty list removes the field). Cancel.

**States** Empty list: `t.startupApps.empty` "No startup program yet."

**Messages, warnings, codes** Hint `t.startupApps.hint`; row error `t.startupApps.notAbsolute` "A full path is needed (it must start with a drive letter or \\server\share)."

**Dependencies** Local and remote seats (menu item always shown). Takes effect at the next sign-in on the seat (hint text).

**Live behaviour** Row validation on every keystroke.

#### Kiosk seat dialog (`KioskShellDialog.svelte`)

**Purpose** Turn a local seat into a kiosk seat of the `desktops` method: the apps it may start, autostart, target monitor, idle reset. `null` = normal seat.

**Opens when** Settings seat menu "Kiosk…" (local seats; method `desktops` or seat already has a kiosk shell); also seats board. Title `t.kiosk.title(n)`, sub "<name>. " + `t.kiosk.sub`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.kiosk.enable` "Make this seat a kiosk seat" (help `enableHelp`) | switch | on/off | on when `local.kiosk_shell` exists | off → saves `null` (field removed) |
| `t.kiosk.appTitle` "Tile name" | text input `maxlength=64` | empty allowed: service default = program file name without `.exe`, or the part after `!` of the app id | — | `kiosk_shell.apps[].title` |
| `t.kiosk.program` "Program path" | text input | empty or absolute Windows path (`isAbsoluteWindowsPath`); ≤260 chars (service check) | — | `apps[].command` |
| `t.kiosk.monitor` "Monitor (1 = first)" | number input min 1 max 16 | stored as index 0..15 (clamped in `setKioskShell`) | 1 | `apps[].monitor` (0-based) |
| `t.kiosk.autostart` "Start when the seat opens" | switch, aria `autostartFor(name)` | on/off | off | `apps[].autostart` |
| Remove | icon button | — | — | removes row |
| `t.kiosk.args` "Arguments" (second row) | text input | parsed like startup args | empty | `apps[].args` (omitted when empty) |
| `t.kiosk.appId` "Store app ID (AUMID, optional)" (tooltip `appIdHint`) | text input | empty or `Package!App`: `[A-Za-z0-9._-]+![A-Za-z0-9._-]+`, ≤160 chars (`isAppUserModelId`) | empty | `apps[].app_id`; when set, no program path needed |
| `t.kiosk.fullscreen` "Full screen" | checkbox | on/off | off | `apps[].fullscreen` (only written when true) |
| `t.kiosk.add` "Add app" | button | disabled at 16 rows (`MAX_KIOSK_APPS`) | — | adds row |
| `t.kiosk.idleLabel` "Sign out after no activity" (help `idleHelp` "0 = off, at most 1440."; unit `t.advancedPage.unitMin` "min") | number input 0..1440 | integer 0..1440 | 0 | `kiosk_shell.idle_reset_minutes` |
| `t.kiosk.limitsTitle` "Limits" disclosure (summary `limitsSummary`) | disclosure | — | closed | shows `t.kiosk.intro` and `t.kiosk.limits` |

**Actions** Save (disabled when enabled AND any of: bad path, bad app id, idle out of range, >16 rows) → `setKioskShell`: rows with neither command nor app id are dropped; list capped at 16. Cancel.

**States** Enabled with no rows: `t.kiosk.empty` "No app yet." Disabled switch: only the Limits disclosure is shown.

**Messages, warnings, codes** `t.kiosk.notAbsolute`, `t.kiosk.notAppId` "Invalid app ID (must be in Package!App form).", `t.kiosk.tooMany` "At most 16 apps can be added.", `t.kiosk.idleBad` "Enter a number from 0 to 1440.", help `t.kiosk.appIdHint`. Service issues: `invalid_kiosk_shell` (error), `kiosk_shell_ignored_on_seat_one` (warning: seat 1 while "Seat 1 stays a normal desktop" is on), `kiosk_app_execution_alias` (error from the service: path is a Store app alias, use the AUMID), `ip_binding_shared_session` (warning: kiosk seat with an IP address).

**Dependencies** Session method `desktops` (architecture 4.1.3: concurrent kiosk seats in the console session, client SKUs, x64 and ARM64). Local seats only.

**Live behaviour** Validation on every keystroke.

#### Redirection dialog (`RedirectDialog.svelte`)

**Purpose** Clipboard direction and formats, the seat's own drives, smart card readers and printers for one seat session (IronRDP seat client).

**Opens when** Settings seat menu "Redirection…" (local seats 2+). Title `t.redirect.title(n)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.redirect.clipboard` "Clipboard" | radio list | `off`, `to_seat` "To the seat (paste from the host session into the seat)", `from_seat` "From the seat (copy from the seat to the host session)", `both` "Both ways" | `off` | `local.redirect.clipboard.direction` |
| `t.redirect.formats` "Formats passed" (only when direction ≠ off) | 5 checkboxes | `text` "Text", `html` "HTML and rich text", `images` "Images", `files` "Files", `cross_seat` "Data copied on other seats may also reach this seat" | text, html, images on; files, cross_seat off | `clipboard.<flag>` |
| `t.redirect.drives` "Drives" (help `drivesHint`) | radio list | `off`, `read_only` "Read-only", `read_write` "Read-write" | `off` | `local.redirect.drives` |
| `t.redirect.smartcards` "Smart card" (help `smartcardsHint`) | switch, label `smartcardsLabel` | on/off | off | `local.redirect.smartcards` |
| `t.redirect.printers` "Printers" (help `printersHint`) | radio list | `off`, `seat` "Printers given to this seat", `all` "All printers of the PC" | `off` | `local.redirect.printers` |

**Actions** Save → `setRedirect` (the all-off default is not written; seat 1 never writes). Cancel. For seat 1 only "Close".

**States** Seat 1: only text `t.redirect.seatOne` "Seat 1 runs in the host session; no redirection needed." (menu does not offer it for seat 1).

**Messages, warnings, codes** Info notice `t.redirect.hint` (settings fully apply only in the IronRDP seat client; the ActiveX client cannot filter). Warning `t.redirect.sharedWarning` when clipboard ≠ off. Warning `t.redirect.printersAllWarning` when printers = all. Runtime refusal codes (`t.refusalReasons.*`, e.g. `direction_to_seat_off`, `reader_not_assigned`, `drive_is_system_drive`, `clipboard_filter_needs_ironrdp`, `printers_need_ironrdp`) and redirection notes (`t.notes.clipboard_shared_with_host_session`, `no_seat_drives`, `drive_without_global_letter`, `drive_acl_volume_refused`, `drive_letter_moved`, `drive_volume_unknown`, `no_seat_readers`, `no_seat_printers`, `printers_all_queues`, `printers_print_as_host`, `drive_served_as_seat_user`) are shown on other screens (seat runtime), not in this dialog.

**Dependencies** Local seats 2+. Timing: drive changes reach a connected seat at once; printer changes at the seat's next connection (en.ts hints).

**Live behaviour** Formats group appears/disappears with the direction.

#### IP address dialog (`IpBindingDialog.svelte`)

**Purpose** Give one seat one of this PC's static IPv4 addresses (or none) and the rule for which programs use it. The program list itself is shared and edited in Advanced > Network addresses.

**Opens when** Settings seat menu "IP address…"; Advanced > Network addresses row "Edit"; App-level dialog `ip` (from the Advanced page `onIp`). Title `t.ipDialog.title(n)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.ipDialog.address` "IP address" (help `staticHelp`) | select | `''` "Not assigned" + static IPv4 addresses of this PC (`list_ipv4_addresses`, label "address (adapter)"); a saved address no longer static/present stays as "address (?)" | stored `ip_binding.address` or Not assigned | `seats[].ip_binding.address` |
| `t.ipDialog.rule` "Which programs should use this address?" (only when an address is chosen) | radio list | `all_programs` "All programs started on this seat", `only_listed` "Only the programs in the list", `all_except_listed` "All programs except those in the list"; the last two show help `listNote(n programs)` | `all_programs` | `ip_binding.rule` |

The address list (Tauri `net.rs`): IPv4 from `GetAdaptersAddresses`; loopback 127/8, APIPA 169.254/16 and 0.0.0.0 never offered; only addresses with prefix origin "manual" count as static and are offered.

**Actions**
- Save → `setIpBinding` (`null` when Not assigned removes `ip_binding`); Save bar.
- `t.ipDialog.openList` "Edit the program list" (when the caller provides it): closes and goes to Advanced `#net` / scrolls to the program list.
- `t.ipDialog.openNetwork` "Open Network Connections" → `open_system_panel('network_connections')` (`ncpa.cpl`).
- `t.ipDialog.openDevices` "Open Device Manager" → `open_system_panel('device_manager')`.
- Cancel.

**States** Loading (list empty until loaded); load error notice `t.ipDialog.loadFailed`; no static address: info `t.ipDialog.noStatic`.

**Messages, warnings, codes** Help `t.network.addressHelp`. Service issues for `ip_binding`: `invalid_ip_address` (error; dotted IPv4, not 0.0.0.0/255.255.255.255, not ≥224), `loopback_ip_address` (error; 127.x), `link_local_ip_address` (warning; 169.254.x), `duplicate_ip_address` (warning; same address on two seats), `ip_binding_shared_session` (warning; kiosk seat).

**Dependencies** Enforcement depends on the source IP driver (see Advanced > Network addresses). Local and remote seats.

**Live behaviour** None (one load on open).

#### Processor cores dialog (`CpuSetsDialog.svelte`)

**Purpose** CPU-set limit per seat, for all seats in one table.

**Opens when** Settings seat menu "Processor cores…" (any seat; the dialog always covers every seat); Advanced > Devices "Processor cores" link (App dialog `cpu`). Title `t.cpu.title`.

**Controls** Table: one row per seat (header = seat display name), one column per CPU set (header = logical index, plus "P"/"E" when the PC is hybrid; tooltip `t.cpu.group(g)`, `t.cpu.core(i)`), then two flag columns.

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| CPU set box (aria "Seat N, <logical index>") | checkbox | any subset; none ticked = all cores | from `seats[].cpu_sets` | `seats[].cpu_sets` (sorted ids) |
| `t.cpu.hard` "Hard limit (one processor group)" | checkbox | with hard on, all ticked sets must be in one processor group | `seats[].cpu_hard_limit` | `cpu_hard_limit` |
| `t.cpu.seatWide` "Whole session" (tooltip `seatWideHint`) | checkbox | stored only when on AND at least one set ticked | off | `seats[].cpu_seat_wide` |

**Actions** Save (disabled until the list is loaded and while invalid) → `setCpuSets` for every seat; Save bar. Cancel.

**States** Loading spinner (`t.loading`) while IPC `list_cpu_sets` runs; error notice `t.cpu.failed` + error text; invalid: danger notice `t.cpu.oneGroup` "With a hard limit the chosen cores must be in one processor group."

**Messages, warnings, codes** Hint `t.cpu.hint` "A seat with nothing ticked uses all cores. Each seat must keep at least one core."; legend "P = performance, E = efficiency" (hybrid only); `t.cpu.seatWideHint`. Service issues: `cpu_unknown_id` (some chosen cores do not exist; ignored), `cpu_no_core` (seat left with no core; cannot save), `cpu_only_efficiency_cores` (warning).

**Dependencies** P/E classes from `efficiency_class` (highest class = P). Processor groups matter only on PCs with more than one group.

**Live behaviour** None.

#### GPU preference dialog (`GpuPreferenceDialog.svelte`)

**Purpose** Which GPU chosen programs prefer for the seat user (Windows per-app graphics preference written by the service at each sign-in, undone when Pulsar Seat is turned off).

**Opens when** Settings seat menu "Graphics card preference…" (local seats); seats board. Title `t.gpuPref.title(n)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.gpuPref.mode` "Preference" | select | `off` "Off (Windows decides)", `seat_gpu` "The graphics card that drives the seat’s monitors", `high_performance` "High-performance graphics card", `power_saving` "Power-saving graphics card" | `off` | `local.gpu_preference.mode` |
| `t.gpuPref.program` "Program (full path, .exe)" rows | text inputs | empty (dropped) or: absolute path, ends `.exe`, ≤260 chars, no control chars, no leading/trailing blanks (`gpuPrefAppValid`) | — | `gpu_preference.apps[]` |
| Remove | icon button | — | — | removes row |
| `t.gpuPref.add` "Add program" | button | disabled at 64 rows (`MAX_GPU_PREF_APPS`) | — | adds row |

**Actions** Save (disabled while a row is invalid) → `setGpuPreference` (mode off with no programs removes the field). Cancel.

**States** No programs: `t.gpuPref.empty` "No program."

**Messages, warnings, codes** Hint `t.gpuPref.hint` (applies the next time the program opens; a preference only). Row error `t.gpuPref.invalid`. Service issue `invalid_gpu_preference`.

**Dependencies** Local seats only.

**Live behaviour** None.

#### Gamepads that SDL games ignore (`PadIgnoreDialog.svelte`)

**Purpose** Per-seat `SDL_GAMECONTROLLER_IGNORE_DEVICES` list for processes the service starts.

**Opens when** Settings seat menu "Gamepads that SDL games ignore…". Title `t.settingsPage.padIgnoreTitle(seat)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.settingsPage.padIgnoreLabel` "Gamepad IDs" (help `t.basic.padIgnoreHint`, placeholder `0x045E/0x0B09`) | textarea | entries split by whitespace, comma or semicolon; each `0xVVVV/0xPPPP` (the `0x` optional, any case, 4 hex digits each, `isVidPid`); at most 64 entries (service check `MAX_PAD_IGNORE_DEVICES`) | stored list, one per line | `seats[].pad_ignore_devices` |

**Actions** Save → saves only the valid entries (invalid entries are dropped silently on save); empty list removes the field. Cancel.

**States** Field error lists the invalid entries.

**Messages, warnings, codes** `t.settingsPage.padIgnoreBad(list)` "Not in 0xVVVV/0xPPPP form, ignored: …". Hint: pads given to other seats are added automatically. Service issue `invalid_pad_ignore_device` (also when >64 entries).

**Dependencies** Local and remote seats.

**Live behaviour** Error updates while typing.

#### Environment and sign-in dialog (`SeatCompatDialog.svelte`)

**Purpose** Per-seat environment variables, "Deny log on locally" for the seat account, and hiding shut down/restart for the seat user.

**Opens when** Settings seat menu "Environment and sign-in…" (`t.seatCompat.menu`); seats board `t.seatMenu.compat` (disabled when not admin). Title `t.seatCompat.title(n)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Variable `t.seatCompat.envName` "Name" | text input | 1..255 chars; not starting with a digit; only letters, digits, `_ - . ()`; not a reserved name (PATH, PATHEXT, SYSTEMROOT, WINDIR, COMSPEC, TEMP, TMP, USERPROFILE, USERNAME, USERDOMAIN, APPDATA, LOCALAPPDATA, PROGRAMDATA, PROGRAMFILES, HOMEDRIVE, HOMEPATH, __COMPAT_LAYER, SDL_GAMECONTROLLER_IGNORE_DEVICES, SDL_JOYSTICK_IGNORE_DEVICES, STEAM_MASTER_IPC_NAME_OVERRIDE); not starting `PULSAR_SEAT_`; names unique case-insensitively (`envNameOk`, `envListOk`) | — | `seats[].environment[].name` (trimmed) |
| Variable `t.seatCompat.envValue` "Value" | text input | ≤2048 code points, no control chars (C0, DEL, C1) (`envValueOk`) | — | `environment[].value` |
| Remove | icon button | — | — | removes row |
| `t.seatCompat.envAdd` "Add variable" | button | disabled at 32 rows (`MAX_SEAT_ENV`) | — | adds row |
| `t.seatCompat.deny` "Deny log on locally for this seat’s account" | switch; locked "No" when not allowed and off | allowed only when `denyLogonAllowed`: seat has an account (not logon screen) AND (remote seat OR (seat ≠ 1 AND session method `rds_loopback` or `termsrv_wrapper`)) | off | `seats[].deny_local_logon` (only written when true) |
| `t.seatCompat.noPowerMenu` "Hide shut down and restart for this seat’s user" (help `noPowerMenuHint`) | switch | on/off | off | `seats[].no_power_menu` (only written when true) |

Rows with empty name and empty value are ignored.

**Actions** Save (disabled when env invalid, or deny on while not allowed) → `setSeatCompat`. Cancel.

**States** Deny not allowed but stored on: switch stays usable plus warning `t.seatCompat.denyUnavailable`. Help line of deny row: `denyHint` when allowed, else `denyUnavailable`.

**Messages, warnings, codes** `t.seatCompat.envHint`, `envInvalid` "Check the names: letters, digits and _ - . () only, not a Windows variable, each name once, at most 32.", `denyUnavailable` "Only for seats with an account that sign in through Remote Desktop (remote seats, or seats 2 and up with the Remote Desktop session method)." Service issues `invalid_seat_environment`, `deny_local_logon_unsupported`.

**Dependencies** No power menu needs the seat's account (architecture "Console lock guard and seat power policies": per-user `NoClose`). Variables are written at every sign-in. All changes are journaled and undone when Pulsar Seat is turned off.

**Live behaviour** Validation while typing.

#### Advanced page (`AdvancedTab.svelte`)

**Purpose** Methods (backends), timings, safety, devices, network addresses, passwordless-seat port, optional system settings and app compatibility. Title `t.advancedPage.title` "Advanced", intro `t.advancedPage.intro` (nothing applies until Save). Breadcrumb "Settings › Advanced".

**Opens when** Settings > Maintenance "Advanced settings"; IP dialog "Edit the program list" (opens at section `net`). Optional `initialSection` scrolls to that section; `initialSeat` opens that seat's IP dialog first.

**Controls**

Contents list (`Toc`, label `t.advancedPage.toc` "On this page"): Methods, Timings, Safety, Devices, Network addresses, Passwordless seats (`tocRdp`), System settings (`tocTweaks`), Apps on seats (`tocApps`).

Section "Methods" (`id=methods`)

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advancedPage.sessionMethod` "Session method" | radio list (ChoiceList `be-session`) | `rds_loopback`, `termsrv_wrapper`, `fast_user_switch`, `desktops` (labels from `t.backendNames`, split into name + help sentence); experimental options carry note "experimental"; unavailable options stay selectable, help = availability label + backend notes | `rds_loopback` | `settings.backends.session`; Save bar. Picking an experimental method without `experimental_ack == 2` opens ExperimentalDialog first |
| `t.advancedPage.experimentalAcked` + `t.advancedPage.experimentalRead` "Read the warning" (only when session = `termsrv_wrapper`) | button | — | — | reopens ExperimentalDialog |
| `t.advancedPage.restartTs` "Restart Remote Desktop Services" (only when session is `termsrv_wrapper` or `rds_loopback`) | button `t.seatCard.restart` | disabled when not admin or RDS not active (no `rds_loopback`/`termsrv_wrapper` backend reports `active`) | — | confirm dialog, then IPC `restart_term_service {confirm:true}`; immediate |
| `t.advanced.topology` "Where seat hosts run" | select | `seat_one_console` "In seat 1’s own session (default)", `dedicated_host` "In a dedicated host session (full input isolation)" | `seat_one_console` | `settings.backends.host_topology`; Save bar |
| `t.advanced.hostAutologon` "Sign the host console in automatically" (help `hostAutologonHelp`; only under `dedicated_host`) | switch | turning on asks a consent ConfirmDialog; cancel leaves it off; turning off is direct | off | `settings.backends.host_autologon`; Save bar |
| `t.advanced.display` "Display method" | select | `window_presenter`, `display_core`, `pulsar_idd`, `console_only`; unavailable options get " (not available on this PC)" | `window_presenter` | `settings.backends.display`; Save bar |
| `t.advanced.input` "Input method" | select | `raw_input_host`, `winusb_capture`, `kmdf_filter`, `none` | `raw_input_host` | `settings.backends.input` |
| `t.advanced.audio` "Sound method" | select | `rdp_bridge`, `console_shared`, `none` | `rdp_bridge` | `settings.backends.audio` |
| `t.advanced.restoreUsb` "Give USB keyboards and mice back to Windows" (help `restoreUsbHint`) | button `restoreUsbConfirm` "Give them back" | disabled when not admin | — | confirm dialog, then IPC `restore_usb_input {confirm:true}`; immediate; backends refreshed |
| Device access control (`t.advancedPage.gates`, help `gatesHelp`): one switch per gate | switch each | `device_security`, `session_drive_letters`, `camera_consent`, `storage_minifilter`, `printer_acl`; help = availability label when not available | `device_security` + `session_drive_letters` on | `settings.backends.gates[]` add/remove; Save bar |

The select helps show the selected option's availability label and its notes (`selectedHelp`).

Section "Timings" (`id=timings`). Number inputs; the manager accepts any finite value ≥0 (rounded) on change; ranges below are the input min/max and the validation.

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advancedPage.startDelay` "Wait before the first seat starts" (ms) | number, step 500 | 0..600000 (error `timeout_out_of_range` above 600000) | 0 | `settings.timings.start_delay_ms` |
| `t.advancedPage.betweenSeats` "Wait between seats" (ms) | number, step 500 | 0..60000 | 2000 | `timings.between_seats_ms` |
| `t.advancedPage.seatTimeout` "Seat start timeout" (s, help "30 to 120 seconds.") | number, step 1, aria-invalid outside 30..120 | 30..120 | 60 | `timings.seat_start_timeout_s` |
| `t.advancedPage.relogonDelay` "Delay before signing in again after sign-out" (ms) | number, step 500 | 0..60000 | 3000 | `timings.relogon_delay_ms` |
| `t.advancedPage.audioWait` "Time to wait for the audio service" (s) | number, step 1 | input 0..600 (no client validation) | 60 | `timings.audio_wait_timeout_s` |
| `t.advancedPage.monitorIdle` "Turn off the screen of an idle extra seat" (s, help "0 = off.") | number, step 60 | 0, or 60..86400 | 0 | `timings.monitor_idle_off_s` |

Section "Safety" (`id=safety`)

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advanced.uncleanGuard` "Do not start extra seats automatically if Windows did not shut down cleanly" (help `uncleanHelp`) | switch | on/off | on | `settings.safety.unclean_shutdown_guard` |
| `t.advancedPage.crashLimit` (unit "times") | number 1..20 | value forced ≥1 | 3 | `settings.safety.crash_loop_limit` |
| `t.advanced.systemButtons` "Power and sleep buttons on extra seats may affect the whole PC" | switch | on/off | off | `settings.input.allow_system_buttons` |
| `t.advanced.padDeviceScope` "Lock physical gamepads to their seat" (help `padDeviceScopeHint`) | switch | on/off | off (absent from older service = on per types.ts comment) | `settings.input.pad_device_scope` |
| `t.advancedPage.hotkey` "Keyboard/mouse switch shortcut" (help `hotkeyHelp`) | key display (`Ctrl+<key>`) or locked "Off"; button `hotkeyChange` "Change" (aria `hotkeyChangeLabel`) | — | Ctrl+F12 (scan 0x58) | opens Hotkey dialog |
| `t.advancedPage.logLevel` "Log detail" (help `logLevelHelp`) | select | `errors` "Errors and warnings only", `info` "Information", `debug` "Debug", `trace` "Trace (very detailed)"; `custom` "Custom" listed only when the stored switches match no choice | `info` | `settings.log {info, debug, trace}` (`logSettingsFor`; custom keeps stored values) |

Section "Devices" (`id=devices`)

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advanced.adminsBypass` "Administrators can access the devices of all seats" | switch | on/off | on | `settings.devices.admins_bypass` |
| `t.advancedPage.classDefaults` "Class-level default access definitions" (help `t.advanced.classDefaultsSoon`) | switch | on/off | off | `settings.devices.class_default_descriptors` |
| `t.advancedPage.classDefaultsClasses` "Device classes covered" | disclosure with checkbox group (aria `t.advanced.classDefaultClasses`); summary lists chosen classes | `cdrom`, `ports`, `modem`, `multiport_serial`, `floppy`, `tape_drive`, `medium_changer` (labels `t.advanced.classTargets`); disk and volume never offered | empty = `cdrom` + `ports` | `settings.devices.class_default_classes` (empty list removes the field) |
| `t.advancedPage.outputs` "Display outputs" (help `outputsHelp`) | link row | — | — | opens OutputsDialog (owned by another part) |
| `t.advancedPage.cpu` "Processor cores" (help `cpuHelp`; value `cpuLimited(n)` or `cpuAll`) | link row | — | — | opens Processor cores dialog |
| `t.advancedPage.palmCheck` "Touchpad palm detection threshold" | select | `''` "Windows default" (`t.advanced.palmCheckOff`), 0, 1, 2, 3, 4 | Windows default (null) | `settings.tweaks.touchpad_palm_check` |

Section "Network addresses": see `NetworkSection.svelte`.

Section "Separate port for seats without a password" (`id=rdp`, `t.advancedPage.loopback`)

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advancedPage.loopbackEnabled` (help `t.advanced.loopbackHint`) | switch; when session ≠ `rds_loopback`: locked "Not available yet" with help `loopbackInactive` | on/off | on | `settings.backends.loopback_listener.enabled` |
| `t.advancedPage.loopbackPort` "Port" (help "1024 to 65535, not 3389.") (enabled only) | number input 1024..65535 | integer 1024..65535 and ≠ 3389 (`loopbackPortValid`); aria-invalid otherwise | 33389 | `loopback_listener.port` |
| `t.advancedPage.loopbackAccounts` (help `loopbackAccountsHelp`) (enabled only) | textarea | one per line or comma; trimmed, duplicates (case-insensitive) dropped; each `user` or `DOMAIN\user`, no `@`, one backslash at most (`accountEntryValid`) | empty | `loopback_listener.extra_accounts` |

Sections "Optional system settings" and "Apps on seats": see `CompatSettings.svelte`.

**Actions**
- ExperimentalDialog (local variant): Accept (needs the "I have read and understood the risks" checkbox, `t.experimental.ack`) writes `experimental_ack = 2` (`EXPERIMENTAL_WARNING_VERSION`) and `session = <kind>`; Cancel changes nothing.
- Host console consent ConfirmDialog: title `t.advanced.hostConsentTitle`, body `hostConsentBody`, warning `hostTradeoffs[1]`, confirm `hostConsentConfirm` "Turn on".
- Restart Remote Desktop Services ConfirmDialog (danger): `restartTsTitle`, `restartTsBody`, confirm `restartTsConfirm` "Stop seats and restart". Result toast `restartTsDone(n)` or `restartTsFailed(reason)` (reason `restart_needs_reboot` has its own text).
- Restore USB ConfirmDialog: `restoreUsbTitle`, `restoreUsbBody`, confirm `restoreUsbConfirm`. Result toast `restoreUsbDone` or `restoreUsbFailed(n)` with the failed list as detail.
- Breadcrumb "Settings" goes back.

**States**
- No draft: `needsElevation` → `t.adminToView`; otherwise `t.loading` "Loading…".
- Read-only: all sections inert (Network section receives `readOnly`).
- Backends unknown (`get_backends` failed or not yet loaded): a built-in fallback list of kinds is shown with no availability claims; `termsrv_wrapper` marked experimental.
- Restart TS disabled with help `t.advanced.restartTsNotActive` when RDS is not active.
- Loopback section locked when session ≠ `rds_loopback`.
- Experimental: `termsrv_wrapper` requires acknowledgement (issue `experimental_not_acknowledged`).

**Messages, warnings, codes**
- `t.advanced.inputNotIsolated` (warning; input `raw_input_host` + topology `seat_one_console`).
- `t.advanced.inputNotIsolatedNonUsb` (warning; input `winusb_capture` + `seat_one_console`).
- `t.advanced.hostTradeoffs` (3-item list; under `dedicated_host`).
- Host console state notice (`store.status.host_console.state` ≠ off): `t.advanced.hostState.on` / `pending_removal` (info), `refused` / `failed` / `drifted` (warning). Reason code field exists (`other_autologon`, `registry_password`, `winlogon`…) but is not shown here.
- Restart TS help: `restartTsHint` + `loopbackBinding` when active.
- Loopback notices (enabled only): info `t.advanced.loopbackAllowed(list)` when any account is allowed (seat accounts of RDP seats, seat 1 excluded under `seat_one_console`, plus valid extra accounts); else warning `t.advanced.loopbackNoAccounts(seats)` when logon-screen RDP seats exist.
- Availability, unavailable reasons, needs-setup, notes: see PART model "Backend availability".
- Issues: `experimental_not_acknowledged`, `loopback_port_invalid`, `loopback_account_invalid` (warning), `loopback_no_accounts` (warning), `logon_screen_needs_listener` (warning), `hide_fsu_with_fast_user_switch` (error), `timeout_out_of_range`, `invalid_hotkey`, `same_account_fast_user_switch`.

**Dependencies** Backend availability from `get_backends` (service probe). `rds_loopback` = Windows Server x64 with RD Session Host; `termsrv_wrapper` = client Windows, experimental, user-installed wrapper; `desktops` = kiosk seats in one session. Restart TS and restore USB need admin. Host console preset only under `dedicated_host`.

**Live behaviour** `get_backends` fetched on mount if missing and after restore USB; host console state follows `store.status`; the contents list highlights the section in view (`aria-current="location"`).

#### Network addresses section (`NetworkSection.svelte`)

**Purpose** Per-seat address overview, the program list shared by all seats, how addresses are enforced, and the Hyper-V adapter helper.

**Opens when** Part of the Advanced page (`id=net`, title `t.advancedPage.network`, help `networkHelp`).

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Seat row: name, rule text (`allPrograms` / `listedOnly` / `allButListed`), address or locked "Not assigned" | text | — | — | read only |
| `t.advancedPage.ipEdit` "Edit" (aria `ipEditFor(name)`) | button | — | — | opens IP address dialog |
| Program list table (caption `programList`; help `programListHelp`): `programEnabled` checkbox | checkbox | on/off | on for new rows | `settings.ip_programs[].enabled` |
| `programTitle` "Title" | text input | free | path's file name when empty and path set | `ip_programs[].title` (trimmed) |
| `programPath` "Path" (placeholder `C:\Games\game.exe`) | text input | non-empty, no control chars; a folder means every program in it | — | `ip_programs[].path` (trimmed) |
| Remove | icon button | — | — | removes row |
| `t.advancedPage.programAdd` "Add program" | button | disabled at 256 rows (`MAX_IP_PROGRAMS`) | — | adds empty row |
| `t.advancedPage.programBrowse` "Browse" | button | same limit | — | Tauri `pick_program`: file dialog on the Start menu Programs folder, filter `*.exe;*.lnk`; a shortcut adds its target; duplicates (case-insensitive path) skipped |
| `t.advancedPage.programFolder` "Add the programs in a folder" | button | same limit | — | Tauri `pick_program_folder`: every `.exe` and the `.exe` targets of `.lnk` files in the folder, one level, at most 200 |
| Hyper-V `t.network.hypervRefresh` "Check" | button | disabled when not admin | — | IPC `hyper_v {type:'status'}` |
| `t.network.hypervEnable` "Turn Hyper-V on" (only when feature = disabled) | button | admin | — | danger ConfirmDialog (`hypervEnableConfirm`), then `hyper_v {type:'enable_feature', confirm:true}` |
| Per seat 2+ (only when feature = enabled): `hypervCreate` "Create adapter" | button | seat must have an `ip_binding` address (else locked `hypervNoAddress`) | — | `hyper_v {type:'set_seat_nic', seat, address, prefix_len: 24}` |
| `hypervRemove` "Remove adapter" (when the seat has a NIC; shows its addresses) | button | admin | — | `hyper_v {type:'remove_seat_nic', seat}` |

All program-list edits go to the draft (Save bar). Hyper-V actions are immediate IPC.

**Actions** As above. List edits are capped at 256 rows when written (`slice(0, MAX_IP_PROGRAMS)`).

**States** Loading / needs elevation like the Advanced page. Empty list: `t.advancedPage.programNone`. A row with empty path: error `t.ipDialog.invalidPath` "The path cannot be empty." Picker results: `t.ipDialog.folderEmpty`, `t.ipDialog.pickFailed` (status text). Read-only: seat rows and program list inert; Hyper-V buttons disabled.

**Messages, warnings, codes**
- Enforcement notice (from `get_backends.source_ip`): `enforcement = driver` → `t.network.enforcedByDriver(seats)` or `t.network.driverReady`, plus `t.network.driverScope`; `environment` → `t.network.driverMissing(reason)` with reasons `t.network.driverReasons.{driver_not_loaded, driver_busy, driver_version_mismatch, driver_pass_through, map_invalid, map_refused}`; no report (older service) → `t.network.bestEffort`.
- `t.network.sharedSession(seats)` (warning; seats sharing a session, address not enforced by the driver).
- `t.network.droppedPaths(n)` + list (warning; paths that could not be passed to the driver).
- Hyper-V: intro `hypervIntro`; status `hypervFeature(state)`: `enabled` "on", `disabled` "off", `enable_pending` "turning on (restart needed)", `unavailable` "not available on this Windows edition"; warning `t.network.hypervReboot` when `reboot_required`.
- Issues `too_many_ip_programs` (warning), `invalid_ip_program` (error).

**Dependencies** Source IP driver (`source_ip_callout`); Hyper-V feature (Windows edition); admin for Hyper-V.

**Live behaviour** Enforcement notices follow `store.backends` (fetched on mount if missing). Hyper-V status only on the operator's action.

#### Optional system settings and Apps on seats (`CompatSettings.svelte`)

**Purpose** Visible, reversible system changes (each off by default, journaled, undone when turned off, when Pulsar Seat is turned off or uninstalled) and app compatibility options.

**Opens when** Last two sections of the Advanced page (`id=tweaks` title `t.advanced.tweaks`, help `tweaksHint` + `t.compat.hint`; `id=apps` title `t.compat.apps`).

**Controls**

Optional system settings:

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.advanced.hideFus` "Hide fast user switching" | switch; locked "Not available yet" when session = `fast_user_switch` and the tweak is off (help `hideFusConflict`) | on/off | off | `settings.tweaks.hide_fast_user_switching`; error `hide_fsu_with_fast_user_switch` with `fast_user_switch` |
| `t.advanced.excludeDrivers` "Leave driver updates out of quality updates" | switch | on/off | off | `tweaks.exclude_driver_updates` |
| `t.compat.fastStartup` "Turn off fast startup (restart needed)" (help) | switch | on/off | off | `tweaks.fast_startup_off`; Windows restart needed for effect |
| `t.compat.defender` "Exclude Pulsar Seat from Microsoft Defender scans" (help) | switch | on/off | off | `tweaks.defender_exclusions` |
| `t.compat.inactivityLock` "No inactivity lock of the console" (help) | switch | on/off | off | `tweaks.inactivity_lock_off` |
| `t.compat.usersShutdown` "Only administrators may shut down or restart the PC" (help) | switch | on/off | off | `tweaks.users_shutdown_right_off`; from each user's next sign-in |

Console lock (sub-label `t.compat.consoleLock`, help `consoleLockHint`):

| Control | Kind | Allowed values | Default | Effect |
|---|---|---|---|---|
| `t.compat.hostLockOff` "Lock and screen saver off for the host account" | switch (always shown) | on/off; help `hostLockOffHint` under `dedicated_host`, else `hostLockOffNa` "Only with the dedicated host topology (Advanced > Session)." | **on** | `settings.console_guard.host_lock_off` (effective only under `dedicated_host`) |
| `t.compat.seatOneLockOff` "Seat 1 cannot lock while other seats run" (only under `seat_one_console`) | switch | on/off | off | `console_guard.seat_one_lock_off` |

Windows Update for seat PCs (sub-label `t.compat.wu`, help `wuHint`; Windows Home ignores these policies):

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.compat.wuEnabled` "Use these Windows Update settings" | switch | on/off | off | `tweaks.windows_update.enabled` |
| `t.compat.wuFeature` "Delay feature updates by (days, 0–365)" (enabled only) | number 0..365 | integer 0..365 | 0 | `windows_update.feature_defer_days` |
| `t.compat.wuQuality` "Delay quality updates by (days, 0–30)" | number 0..30 | integer 0..30 | 0 | `windows_update.quality_defer_days` |
| `t.compat.wuActiveHours` "Active hours" From / to | two selects | `''` "Windows decides" or 00:00..23:00; both empty, or both set, different, window length (wrapping midnight) ≤18 h | both empty | `active_hours_start` / `active_hours_end` (null = Windows decides) |
| `t.compat.wuNoRestart` "Never restart automatically while someone is signed in" | switch | on/off | off | `windows_update.no_auto_restart_with_users` |

Block new devices of unused kinds (sub-label `t.compat.deny`, help `denyHint`):

| Control | Kind | Allowed values | Default | Effect |
|---|---|---|---|---|
| `t.advancedPage.denyClasses` "Kinds to block" | checkbox group | `printer` "Printers", `image` "Scanners and imaging devices", `camera` "Cameras", `wpd` "Phones and portable devices", `modem` "Modems", `cdrom` "CD / DVD drives", `ports` "Serial and parallel ports", `smart_card_reader` "Smart-card readers" | none | `tweaks.device_install_deny.classes` |
| `t.compat.denyAdmins` "Administrators may still install these devices" | switch | on/off | on | `device_install_deny.admins_override` |
| `t.compat.denyPickUnused` "Tick the unused kinds" (inside the unused notice) | button | — | — | adds all unused classes |

Apps on seats:

| Control | Kind | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.compat.steamPerSeat` "Steam on every seat (own Steam client per local seat)" (help) | switch | on/off | off | `settings.apps.steam_per_seat` |
| `t.compat.objectRules` "Single-instance programs (one copy per seat)" (help): per rule `objectExe` "Program (file name, e.g. launcher.exe)" | text input (committed on change, trimmed) | 1..260 chars, ends `.exe`, no `\ / tab : |` | — | `settings.apps.object_rules[].exe` |
| per rule `objectNames` "Object names (one per line, without Global\)" | textarea | 1..16 names; each 1..200 chars, not `*`, no `\`, tab, `|`, control chars; trailing `*` = prefix | — | `object_rules[].names` |
| per rule Remove (aria `rowAction.remove(exe or "Rule N")`) | icon button | — | — | removes rule |
| `t.compat.objectAdd` "Add program" | button | disabled at 32 rules (`MAX_OBJECT_RULES`) | — | adds empty rule |
| `t.compat.steamProxy` "Old Steam add-on DLL" (help) | button `steamProxyCheck` "Look for it" (busy while running) | — | — | IPC `steam_proxy_dll {type:'status'}`; immediate |
| per found path: `steamProxyQuarantine` "Move to quarantine" | button (disabled while busy) | — | — | `steam_proxy_dll {type:'quarantine', path}` |
| per quarantined file `steamProxyQuarantined(orig)`: `steamProxyRestore` "Put back" | button | — | — | `steam_proxy_dll {type:'restore', path}` |

**Actions** All switches/inputs edit the draft (Save bar) except the Steam DLL actions (immediate).

**States** Steam result row `t.advancedPage.steamProxyResult` "Result" with `steamProxyNoSteam` "Steam is not installed." or `steamProxyNone`. Invalid rule: error `t.compat.objectInvalid` under that rule. Invalid WU: danger notice `t.compat.wuInvalid`.

**Messages, warnings, codes** `t.compat.seatOneLockNotice` "Locking seat 1 hides every local seat until seat 1 unlocks." (info; `seat_one_console` and seat-1 guard off). `t.compat.denyUnused(list)` "No seat uses: …" (info; classes with no device assigned or present; WPD and modem always count as unused). Issues `invalid_windows_update_policy`, `invalid_object_rule`, `hide_fsu_with_fast_user_switch`.

**Dependencies** Topology decides which console-lock switch is relevant. Steam per seat and object rules need per-process patching with the program on the shim target list (en.ts hints). Defender exclusions do nothing when another antivirus is active.

**Live behaviour** Unused-class suggestion follows `store.devices`. Steam DLL report on demand only.

#### Keyboard/mouse switch shortcut dialog (`HotkeyDialog.svelte`)

**Purpose** Set the Ctrl + one key shortcut that moves the pressing keyboard (and the mice on its seat) to the next seat.

**Opens when** Advanced > Safety "Change" (App dialog `hotkey`). Title `t.hotkey.title`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.hotkey.field` "Shortcut" (help `t.hotkey.press` "Click the field and press a key") | read-only text field capturing a key press | physical key (`KeyboardEvent.code`) from a fixed table: Esc, 1–0, - =, Backspace, Q–P, [ ], Enter, A–L, ; ' `, \, Z–M, , . /, Num *, Space, F1–F12, numpad digits, Num - + , (decimal), IntlBackslash; stored as PC/AT set-1 make code. Ctrl/Shift/Alt/Meta ignored; CapsLock/NumLock/ScrollLock and unmapped keys rejected. Tab and Escape keep their normal function (not capturable). Service: scan ≠ 0, ≤0x7F, ≠ 0x1D | `Ctrl + F12` (0x58) | `settings.input.switch_hotkey_scan` |

**Actions** Save → writes scan (or null); Save bar. `t.hotkey.off` "Turn the shortcut off" sets null (shows "Off"). Cancel.

**States** Invalid key: field error and live region `t.hotkey.invalid` "This key cannot be used."

**Messages, warnings, codes** Body `t.hotkey.body` (modifier always Ctrl); warning `t.hotkey.warning`. Issue `invalid_hotkey`. Backend note `hotkey_needs_filter` "The shortcut only works with the console listener or the input filter".

**Dependencies** Console listener or input filter (backend note).

**Live behaviour** Field shows the label of the last pressed valid key.

#### Device picture dialog (`PictureDialog.svelte`)

**Purpose** Choose the picture of one device: default, a built-in drawing, or an admin-provided file.

**Opens when** Seats board device menu `t.deviceMenu.picture` (SeatsTab; disabled when no pictures are available for the kind). Title `t.picture.title(device name)`.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Built-in pictures (`t.picture.builtin`) | radio group of tiles | "Default picture" + built-ins for the device kind (`BUILTIN_PICTURES`, names `t.pictureNames`): monitor: monitor-speakers, monitor-curved, tv, laptop; keyboard: keyboard-compact, keyboard-numpad, keyboard-laptop; mouse: mouse-wireless, mouse-vertical, trackball; hid: touchpad, trackpoint, pen, touchscreen, gamepad; audio_render: headphones, speaker-single; audio_capture: headset; webcam: webcam-clip | stored value | `builtin:<id>` or undefined |
| Your own pictures (`t.picture.user`; only for kinds with a category) | radio group of tiles | files in `%ProgramData%\Pulsar Seat\pictures\<category>\` (categories monitor, keyboard, mouse, hid (optionally a usage-page sub-folder), audio_render, audio_capture, video_capture); png, jpg/jpeg, gif, bmp, webp, svg; ≤512 KiB each; ≤64 per category; `name_off.ext` files are not offered (they are the "not in use" picture of `name.ext`) | — | `user:<file name>` |

**Actions** `t.picture.choose` "Choose" → `setPicture` (writes `devices[].options.picture`, undefined removes); Save bar. Cancel.

**States** No own pictures: `t.picture.noUser`. Load failure of user pictures: treated as empty.

**Messages, warnings, codes** `t.picture.userHint` (where to put files; `_off` suffix).

**Dependencies** Admin writes the folder; normal users can read it. Tauri `list_user_pictures(category)` returns data URLs.

**Live behaviour** None.

#### Save-time conflicts dialog (`ConflictDialog.svelte`)

**Purpose** List `conflict`-severity issues returned by `validate_config` and let the user fix or force.

**Opens when** Save (App.svelte) when the validation has conflicts and no errors. Alert modal, initial focus on the primary button. Title `t.conflicts.title` "Assignments that cannot be applied".

**Controls** One row per issue: alert icon + `issueLabel(issue)` (prefixed with "seat N" and/or device name).

**Actions** `t.conflicts.fix` "Fix" (primary): closes, back to editing. `t.conflicts.saveAnyway` "Save anyway": `save(force=true)` → `apply_config` with `force`.

**States** None other.

**Messages, warnings, codes** Body `t.conflicts.body`. Issue texts from `issueText` (en.ts).

**Dependencies** Admin (save).

**Live behaviour** None.

### 3.4 Setup wizard and tool dialogs

#### Setup wizard, main window (`Wizard.svelte`)

**Purpose**
First-run / re-run setup. Turns each monitor into a seat and finds each seat's keyboard and mouse by key presses and clicks. The service runs the capture and draws a number overlay on every monitor; this view checks preconditions, optionally finds seat 1's main keyboard/mouse by touch, starts the service wizard, opens one full-screen page per monitor (`WizardPage.svelte`) and shows progress. Code comment: "Untested on hardware."

**Opens when**
- Seats board, "More" menu item `t.systemMenu.wizard` ("Setup wizard…") (`SeatsTab.svelte`), disabled when not admin.
- Seats board block shown when only one local seat exists: heading `t.canvas.addSecondTitle` ("Add a second seat"), body `t.canvas.addSecondBody`, button `t.canvas.startWizard` ("Start the setup wizard"), disabled when not admin. Sibling button `t.canvas.addEmpty` ("Add an empty seat").
- Both go through the unsaved-changes guard (`App.svelte` `openWizard` → `guard`): with unsaved changes the save / discard / cancel dialog comes first.
- Rendered as a full-window overlay (`role="dialog"`, `aria-modal`), over the whole app.
- `t.wizard.firstRun` ("No seat is set up yet. Do you want to run the setup wizard?") exists in en.ts but has no consumer in code.

The wizard has two phases in one component: **start screen** (`started = false`) and **running view** (`started = true`).

##### Phase 1: start screen

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Wizard mode (`t.wizard.variantsLabel` "Wizard mode", section `t.wizard.variants` "How do you want to go on?") | single choice list, `WizardVariant` | `all_seats`, `keep_seat_one`, `quick_two_seats`, `host_console` (labels/help below) | `keep_seat_one` | local state `variant`; sent with `wizard_start{variant}` |
| Consent checkbox `t.wizard.hostConsentAccept` ("I understand: create the console account and turn on its automatic sign-in") | checkbox | only visible when variant = `host_console` | unchecked | Required for Start when `host_console` |
| Main keyboard and mouse (`t.wizard.mainDevices`): button `t.wizard.findByTouch` ("Find by touch") / `t.wizard.gatherStop` ("Stop searching") | toggle button | disabled when no live service link (`store.link` false) | off | Local "gather" mode: listens to `device_activity`; first keyboard and first mouse touched (present devices only) are taken (`gatherStep`, `wizard.ts`); gathering stops by itself when both found |
| Extend desktop switch `t.wizard.extendLabel` ("Extend the desktop to all monitors"), help `t.wizard.extendHelp` ("Recommended. The wizard writes a number on each monitor.") | switch, boolean | — | on | If on, after start sends `wizard_command{cmd:'extend_desktop'}` |
| Start `t.wizard.start` ("Start the wizard") | primary button | disabled unless: no missing saved devices, resolution check `ok`, not busy, and (variant ≠ `host_console` or consent ticked). Disabled reason links to the blocked notices | — | See Actions |
| Cancel `t.cancel` | button | — | — | closes wizard (no service call) |

Variant options (`t.wizard.variantOptions`):

| Value | Label | Help |
|---|---|---|
| `all_seats` | "Each seat has its own keyboard and mouse" | "Identify all seats (seat 1 too) in turn." |
| `keep_seat_one` | "Keep seat 1’s current devices" | "Identify only seats 2 and later." |
| `quick_two_seats` | "Quick two seats" | "Seat 1’s devices stay on seat 1, everything else goes to seat 2." |
| `host_console` | "All seats equal" | "Identify all seats in turn. Every seat, seat 1 too, becomes a session of its own; a Pulsar account signs in at the console and only runs the seats." |

`host_console` consent section (`t.wizard.hostConsentHead` "Before you choose “All seats equal”"): body `t.advanced.hostConsentBody` ("Pulsar Seat creates a standard account named PulsarSeatConsole and turns on the Windows automatic sign-in for it. The old sign-in settings are saved and come back when you turn this off or uninstall."), list `t.advanced.hostTradeoffs` (3 items: seat 1 loses native graphics speed of the console; the account password is kept as an LSA secret, never in the registry; Ctrl+Alt+Del and the secure desktop at the physical console belong to the host account), then the checkbox.

Seat 1 device line: when devices were gathered by touch, shows `t.wizard.gathered` ("Main devices found by touch") · names; otherwise `t.wizard.currentDevices` ("Seat 1 uses now") · present monitors / keyboards / mice whose effective mask is seat 1 only (computed from the current draft). Help text `t.wizard.gatherHint` ("Press a key on the keyboard you will use on seat 1 and move the mouse."); while gathering it is shown as a live status with a spinner.

Below: `t.wizard.finishHint` ("When done, remember to choose a start mode and turn Pulsar Seat on.").

**Actions (start screen)**
- On open: calls `compare_saved` (`store.api.compareSaved`); `diff.missing` non-empty blocks Start. Failure of the call = treated as nothing missing.
- Start (`start()`):
  1. `wizard_start{variant}` (admin). On error: toast; stays on start screen.
  2. Switches to the running view immediately (so the running wizard always has a UI); gathering stops.
  3. If keyboard/mouse were gathered and variant is `keep_seat_one` or `quick_two_seats`: `wizard_command{cmd:'set_main_devices', keys}`.
  4. If extend switch on: `wizard_command{cmd:'extend_desktop'}` (failure = toast, wizard continues).
  5. `quick_two_seats`: immediately `wizard_stop{save:true}` and closes (no pages, no identification).
  6. Otherwise: refreshes devices, builds page specs (one per monitor with a desktop rectangle, in the wizard's monitor order, `pageSpecs`), and calls Tauri `wizard_pages_open`. Failure → toast `t.wizard.pagesFailed` ("The monitor pages could not open; the wizard runs only in this window.").
- Cancel / Escape key: closes without any service call.

**States (start screen)**
- Blocked notices (danger, grouped, id used as disabled reason of Start):
  - `t.wizard.notAllConnected`: "The wizard needs all devices plugged in. Some saved devices are not plugged in."
  - `t.wizard.minResolution(1024, 600)`: "The main monitor’s resolution must be at least 1024×600 for the wizard. Raise the resolution in the display settings."
  - `t.wizard.noMonitor`: "The wizard needs at least one monitor."
- Resolution check (`resolutionCheck`, `wizard.ts`): primary monitor = the display at desktop origin (0,0), else the first present display. Uses its mode (rotation 90/270 swaps width/height); without a mode, the window's screen size × devicePixelRatio; with neither, `ok`. Needs width ≥ 1024 and height ≥ 600 (`MIN_WIDTH`/`MIN_HEIGHT`, same as service `wizard::MIN_WIDTH/MIN_HEIGHT`).
- Busy: Start disabled while `store.busy`.
- Not admin: entry points disabled; service also refuses (`access_denied`).
- Find by touch disabled when service link is down.

##### Phase 2: running view

Top bar: title `t.wizard.title` ("Setup wizard"), buttons `t.wizard.quit` ("Quit without saving") and `t.wizard.saveAll` ("Save and finish").

Progress list (`t.wizard.progress`, ordered list with `aria-current="step"`): segments = "Seat 1’s monitor" (`t.wizard.stepNames.pick_console_monitor`), then one per seat `t.wizard.pageSeat(n)` ("Seat N") for N = 1..number of monitors in the run, then `t.wizard.finish` ("Finish"). Each segment is `done` / `now` / `next`:
- pick segment: `now` during step `pick_console_monitor`, else `done`.
- seat N: `done` if (variant `keep_seat_one` and N = 1) or finishing or N confirmed or seat has both keyboard and mouse assigned; `now` if it is the current seat; else `next`.
- Finish: `now` when step is `waiting`, `done`, `invalid` or no current seat.

Body per service step (`WizardProgress.step`):

| Step | Heading / content | Controls |
|---|---|---|
| `pick_console_monitor` | `t.wizard.pickHead` "Press the number shown on seat 1’s monitor"; sub `t.wizard.pickSub` "Each monitor shows a large number. Press it on any keyboard (0 for 10), or choose the monitor here."; if more than 10 monitors also `t.wizard.pickMonitorMany` "For monitors numbered above 10, click the button on that monitor or in the list below." | One row per present monitor in wizard order: name, help "Monitor · <label>", key cap with its digit (1..9, 0 for 10; only first 10), button `t.wizard.pickThis` "Choose" (aria `t.wizard.pickThisAria(name)` "Choose <name> as seat 1’s monitor") → `wizard_command{cmd:'pick_console_monitor', key}` |
| `keyboard` (with seat N) | Seats 1..12: heading "Press [F N] twice on seat N’s keyboard" (`t.wizard.keyHead`, key drawn inline). Seats 13+: `t.wizard.keyboard(n)` "When seat N’s turn comes, press any key twice on that seat’s keyboard." Sub: `t.wizard.seatStep(n,total,'keyboard')` "Seat N of T, keyboard step." plus for seats ≤ 12 `t.wizard.anyKey` "When it is this seat’s turn, pressing any key twice on its keyboard is enough too." | Live status: two F‑key caps (outlined until pressed: first after 1 press, second after 2) and `t.wizard.keyboardCount(k)` "Pressed: k / 2" |
| `mouse` (with seat N) | `t.wizard.mouse(n)` "With seat N’s mouse, click the two targets on that seat’s monitor."; sub `seatStep(...,'mouse')` | Live status `t.wizard.mouseCount(k)` "Clicked: k / 2" |
| `waiting` | `t.wizard.waiting` "This seat is done; waiting for the other seats." | — |
| `invalid` | heading `t.wizard.stepNames.invalid` "Not saved"; danger notice `t.wizard.invalid` "This assignment could not be saved. The wizard goes on: fix the problem and save again, or quit without saving."; list of issue codes from `progress.issues`, each as `issueText[code]` (raw code if unknown) | — |
| other (`done`, `cancelled`, unknown) | heading `t.wizard.stepNames[step]` ("Done", "Cancelled") or raw step; also announced as status | — |

"Devices found" section (`t.wizard.assigned`), all steps except the pick: rows for the current seat's monitor, the assigned units (current seat first, then other seats), then the last detected unit if unassigned. Each row: device name, kind icon, help = kind · "Seat N" (if other seat) · `t.wizard.lastDetected` ("last detected"); control shows the device's location label if any. Empty: `t.wizard.noneFound` "No device found yet."

Buttons row:
- With a current seat: `t.wizard.confirmSeat` "Confirm this seat" → `wizard_command{cmd:'confirm_seat', seat}`; `t.wizard.resetSeat` "Reset this seat" → `{cmd:'reset_seat', seat}`.
- Always: `t.wizard.resetAll` "Reset all" → `{cmd:'reset_all'}`.

Key legend (`t.wizard.keysIntro` "On any keyboard:"): visible: Enter "confirms this seat" (hidden during the pick step), Shift+Esc "quits without saving". Disclosure `t.wizard.moreKeys` "Other shortcuts": Backspace "resets this seat", Shift+Enter "saves all", Shift+Backspace "resets all", Alt+F4 "quits without saving".

**Actions (running view)**
- Quit without saving → `wizard_stop{save:false}`; Save and finish → `wizard_stop{save:true}`. After stop: closes pages (`wizard_pages_close`) and the wizard. Error handling: error code `wizard_state` (wizard already ended, e.g. Shift+Enter on a seat keyboard) is ignored; any other error → toast, and if it was a save the wizard stays open (step `invalid` shows why).
- Keys pressed in the main window (`keyAction`, `wizard.ts`), only when `progress.keys_seen` is false (else the service handles all keys itself):
  - Shift+Esc or Alt+F4 → stop without saving; Shift+Enter → stop and save; Shift+Backspace → `reset_all`.
  - During pick: digit 1..9 picks monitor N, 0 picks monitor 10; Enter / Backspace do nothing.
  - After pick: Enter → `confirm_seat` (current seat), Backspace → `reset_seat`.
- Page window closed (Alt+F4 on a page, Tauri event `wizard-page-close`) → stop without saving.
- Wizard auto-closes when step becomes `done` or `cancelled`.
- Store side (`store.svelte.ts`): on `done` reloads config and toasts `t.wizard.done` "All seats are identified."; on `cancelled` toasts `t.wizard.cancelled` "The wizard closed without saving."
- On component destroy, open pages are closed.

**What the wizard writes** (service, `docs/agents/architecture.md` "Wizard and manager additions"; demo `demo.ts`): the service saves the result config when all seats are confirmed or on save. The result sets device masks per seat; clears mask bits of seats that no longer exist (an orphaned device goes to seat 1); monitor audio / cameras join the monitor's seat by container id; the monitor's own USB hub and what sits on it join the monitor's seat unless a unit placed by hand on another seat sits there; built-in units stay on seat 1. `host_console`: also sets `settings.backends.host_topology = 'dedicated_host'` and `host_autologon = true`. The save is validated first; on errors the wizard keeps running with step `invalid` and `issues` (`store_failed` on a write error).

**Service wizard rules** (architecture.md): pick of seat 1's monitor is skipped with one monitor (demo: also skipped for `quick_two_seats`); F<N> twice per seat works on all pages at once, or any key twice for the seat next in line; two clicks per mouse (a page's `target_hit` within 1.5 s steers the click to that page's seat, otherwise the lowest seat with a keyboard but no mouse); control keys work from any unit (modifier = its make within 3 s before, marked HYPOTHESIS). Before the pick, `confirm_seat` is refused (`monitor_not_picked`). `target_hit` for a seat outside the run is refused (`seat_out_of_range`). Picking a key not in the run: `unknown_monitor`. A unit with no seat of its own confirms only a seat that already has keyboard + mouse and resets nothing. Wizard start refuses with reasons `no_monitor` / `resolution_too_low`.

**States (running view)**
- Live progress from `wizard_progress` events (topic `wizard`).
- Invalid save: step `invalid` with issue list (see above).
- Busy: commands go through `store.run` (busy flag, error toast).

**Messages, warnings, codes**
All `t.wizard.*` strings above; `issueText` codes in step `invalid`; failure reasons `no_monitor`, `resolution_too_low`, `unknown_monitor` (`failureText`); error code `wizard_state` ("The wizard does not accept this action right now."); service reasons `monitor_not_picked`, `seat_out_of_range`, `not_input_device` (no en.ts text; shown via the `wizard_state`/code fallback, unclear in code which code they carry).

**Dependencies**
Service running and reachable; admin (all `wizard_*` requests are Admin); local seats only (monitors / USB units of this PC); at least one monitor; primary monitor ≥ 1024×600. `host_console` creates a Windows account (`PulsarSeatConsole`) and turns on automatic sign-in.

**Live behaviour**
Progress bar, step body, press/click counters, found-device list update on every `wizard_progress` event. Gather-by-touch listens to `device_activity`. Number overlays on all monitors are drawn by the service (`display.identify`, until hidden).

#### Setup wizard, per-monitor page (`WizardPage.svelte`)

**Purpose**
One full-screen page on each monitor during the wizard, readable from across a room. Shows the monitor's big number during the pick, then what this monitor's seat needs (F<N> twice, two click targets, waiting/done text).

**Opens when**
Opened by the main wizard via Tauri `wizard_pages_open` (`wizard_pages.rs`): one borderless, always-on-top, non-resizable, no-taskbar WebView window per monitor, placed on the monitor's desktop rectangle (physical px); title from `copy.rs` `WIZARD_TITLE` ("Pulsar Seat setup wizard" / "Pulsar Seat kurulum sihirbazı"). Labels `wizard-page-<generation>-<n>`; a new generation per open. At most `MAX_SEATS` (16) pages. Rectangles rejected unless width/height 64..16384, |x|,|y| ≤ 65536, monitor key 1..512 chars. Pages are created hidden and not focused, then shown. The app's root route (`+page.svelte`) renders `WizardPage` instead of the app when `wizard_page_monitor` returns a monitor key. Browser preview: `?wizard-page=<key>` (`&wizard-step=pick|keyboard`, `&wizard-seat1=<key>`) drives the demo wizard.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.wizard.pagePickButton` "This monitor is seat 1" | primary button | pick step only | — | `wizard_command{cmd:'pick_console_monitor', key: <this monitor>}` |
| Click target (`t.wizard.pageTarget` "Click this target") | button at random position | mouse step of this monitor's seat; x 10–90 %, y 35–85 % (`randomTarget`) | — | counts a local hit, moves the target, sends `wizard_command{cmd:'target_hit', seat}` |
| Keyboard | — | same `keyAction` map as main window, with this page's own seat | — | Enter → `confirm_seat` (own seat), Backspace → `reset_seat`, Shift+Enter → `wizard_stop{save:true}`, Shift+Esc / Alt+F4 → `wizard_stop{save:false}`, Shift+Backspace → `reset_all`, digits during pick; ignored when `keys_seen` |

**Actions**
See controls. Errors of `wizard_command` show as a danger notice (`errorLabel`). Errors of `wizard_stop` are ignored.

**States** (`pageView`, `wizard.ts`)

| View | When | Content |
|---|---|---|
| `pick` | step `pick_console_monitor` and this monitor has a number | Rings with number (aria `t.wizard.pageNumber` "This monitor’s number: N"), heading `t.wizard.pagePickHead` "Is this seat 1’s monitor?", text `t.wizard.pagePick` "If this is seat 1’s monitor, press this number or click the button below." (numbers > 10: `t.wizard.pagePickButtonOnly` "…click the button below (there is no key for this number)."), pick button |
| `no_seat` | monitor not in the run | `t.wizard.pageNoSeat` "No seat was given to this monitor (seat limit)." |
| `kept` | variant `keep_seat_one`, seat 1 | `t.wizard.pageKept` "This seat’s devices stay as they are." |
| `done` | seat confirmed, or step done, or seat has keyboard and mouse | `t.wizard.pageDone` "This seat is done. Waiting for the other seats." |
| `keyboard` | current seat in keyboard step, or any seat without a keyboard yet (F<N> works on all pages at once) | Seats ≤ 12: "Press [F N] twice on this seat’s keyboard" (`pageKeyHead`) + `t.wizard.anyKey`; seats 13+: `t.wizard.keyboard(n)`. Status `t.wizard.pageOwner(n)` "This monitor is seat N" · "Pressed: k / 2" |
| `mouse` | current seat in mouse step | `t.wizard.mouse(n)`; status "This monitor is seat N · Clicked: k / 2" (k = max of service clicks and local hits capped at 2); click target |
| `other` | seat has keyboard, another seat is current | `t.wizard.pageOtherSeat(cur)` "Seat N is being set up now. Instructions show here when this seat’s turn comes." |

All seat views show rings with the seat number (aria "Seat N") and a list of devices assigned to this seat. When step is `invalid`: danger notice `t.wizard.invalid` + issue list (`issueText`). Bottom legend: Enter "confirm" (`t.wizard.pageKeysList.enter`), Shift+Esc "quit without saving".

**Messages, warnings, codes**
As listed; `issueText` codes; `errorLabel` of command errors.

**Dependencies**
Tauri shell (pages are native windows); same WebView2 data folder as the main window; on Windows the windows are built only from async commands.

**Live behaviour**
Starts from the cached last progress (`wizard_last_progress`, kept by the shell from the event thread), loads devices once (`devices`), then follows `service-event` `wizard_progress`. Local hit count resets when service clicks go down.

#### Experimental session method dialog (`ExperimentalDialog.svelte`)

**Purpose**
Explicit warning and acknowledgement before the EXPERIMENTAL client-SKU multi-session method (`termsrv_wrapper`: TermService redirected to a third-party open-source wrapper that the admin installed, giving concurrent sessions on Windows Pro/Enterprise). Nothing is enabled until the acknowledgement box is ticked. Two variants on one dialog.

**Opens when**
- `local` variant (`AdvancedTab.svelte`): choosing session method `termsrv_wrapper` in Advanced > methods when `settings.backends.experimental_ack` ≠ `EXPERIMENTAL_WARNING_VERSION` (2); also the button `t.advancedPage.experimentalRead` ("Read the warning") on the row `t.advancedPage.experimentalAcked` ("The experimental warning was acknowledged.") shown when `termsrv_wrapper` is selected.
- `remote` variant (`HostPage.svelte`, remote seats "Host PC" page): buttons `rt.host.twEnable` ("Turn on") or, when another program's wrapper is active, `rt.host.twExternalUse` ("Use for remote seats"); disabled while busy or passive.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| Warning text | read-only, paragraphs with bold parts | `warningText()` in active UI language (`WARNING_EN` / `WARNING_TR`, `warning.ts`, word-for-word copy of service `warning_en.txt`/`warning_tr.txt`) | — | — |
| Wrapper (`rt.experimental.wrapper` "Wrapper") | select, path string | remote variant, not external, ≥ 1 candidate; options = `status.candidates[].service_dll` | first candidate | wrapper path sent to `set_termwrap` |
| Acknowledge checkbox: local `t.experimental.ack` "I have read and understood the risks"; remote `rt.experimental.ack` "I have read and understood the risks; I accept the responsibility." | checkbox | — | unchecked | enables primary button |
| Primary: local `t.experimental.enable` "Choose the experimental method"; remote `rt.experimental.enable` "Turn on and restart the Remote Desktop service"; remote external `rt.experimental.external.enable` "Use it for remote seats" | button | disabled until ticked; remote also disabled with 0 candidates | — | see Actions |
| Cancel (`t.experimental.cancel` / `rt.experimental.cancel` "Cancel") | button | disabled while busy | initial focus | closes |

Disabled reason shown in footer: `t.experimental.ackFirst` "Tick the box first." or `rt.host.twNoCandidate` "No installed wrapper was found.".

Warning content (EN, summary of each line): experimental, read carefully; Pulsar Seat points `TermService\Parameters\ServiceDll` at the wrapper DLL **you installed** and restarts the service; Pulsar Seat does not contain/install the wrapper, does not change Windows files, kernel or LSM; the wrapper patches termsrv in memory (exception to Pulsar Seat's own rule); writes undocumented `EnableConcurrentSessions`, `AllowMultipleTSSessions`, restored with ServiceDll when turned off; the Remote Desktop service restarts on on/off and all open extra seat sessions close; licence grey area, licensed path is Windows Server + RDSH + RDS CALs; Windows updates can change termsrv.dll, on an unknown version Pulsar Seat turns the option off and seats fall back to fast user switching; antivirus may flag the wrapper; on ARM64 the wrapper must be native ARM64; never requires changing Memory Integrity, Secure Boot or other security settings; confirm to continue.

**Actions**
- Local: `onAccept(WARNING_VERSION)` → draft edit `settings.backends.experimental_ack = 2` and `settings.backends.session = 'termsrv_wrapper'` (saved with the normal save; validation issue `experimental_not_acknowledged` otherwise).
- Remote: `onEnable(wrapperPath|null, status.warning_version || 2)` → `set_termwrap{enabled:true, wrapper_path, ack_warning_version}`, then status refresh; dialog closes on success. Error → danger notice (`remoteErrorMessage`), dialog stays open. Busy spinner `rt.experimental.working` "Stopping the seats and restarting the service…" (external: `rt.experimental.external.working` "Stopping the seats…").
- External wrapper (`status.state.state === 'external'`, detect-only): no wrapper choice and no service restart; info notice `rt.host.twState.external` + the wrapper path; the primary only acknowledges the warning for remote seats.

**States**
Not acknowledged (primary disabled + reason); no candidate (danger notice + disabled); busy; error; external. Modal is an alert dialog (`alert`), subtitle `t.experimental.sub` "Experimental". Title local `t.experimental.title` "Experimental session method"; remote `rt.experimental.title` "Turn on the experimental feature".

**Messages, warnings, codes**
Error code `warning_not_acknowledged` ("The experimental method needs the warning to be acknowledged." / remote "Read and acknowledge the warning first."); issue `experimental_not_acknowledged`; notes `termsrv_wrapper_enabled`, `wrapper_managed_externally`, `termsrv_wrapper_external`; remote `rt.host.twState` (`off`, `on`, `foreign_active`, `external`, `blocked`).

**Dependencies**
Client SKU (Pro/Enterprise); a wrapper installed by the operator; on ARM64 a native ARM64 wrapper; admin. Ack version: `EXPERIMENTAL_WARNING_VERSION` = 2 (`types.ts`, same in `seat_core::config`); remote uses the service's `warning_version` if given.

**Live behaviour**
None (static warning; busy spinner during enable).

#### Compare with saved configuration (`CompareDialog.svelte`)

**Purpose**
Live devices vs the saved configuration: one summary notice and one row per changed device, plus whole-draft actions.

**Opens when**
Seats board "More" menu `t.systemMenu.compare` ("Compare with the saved configuration…"), disabled when not admin. Calls `compare_saved` first; error → toast. (Notices namespace has `t.notices.compare` "Compare" as an action label.)

**Controls / Actions**

| Control | Kind | Shown when | Effect |
|---|---|---|---|
| `t.compare.moveAssignment` "Move the assignment to the new port" (per moved device) | button | device has warning `moved_from` (old key) | closes dialog, `move_assignment{from,to}` through the unsaved guard, then reloads config/devices |
| `t.compare.newMonitorsSpare` "Make new monitors spare" | button | `diff.added` contains monitors | draft edit: those monitors set to spare (`addedMonitorsToSpare`); closes |
| `t.compare.restore` "Return to the saved assignment" | button | `diff.matches` false | discards the draft (`store.discard`); closes |
| Close `t.close` | button | always | closes |

Rows (kind icon + device name + help):
- moved: `t.compare.movedFrom` "This device seems to be plugged into another port."
- missing: `t.compare.missing` "Not plugged in (removed after the save)"
- changed: `t.compare.maskChanged` "Assignment changed": "before: X, now: Y" (mask labels: "Spare", "All seats", "Seat N", "Seats 1, 3").
- added: `t.compare.added` "Added after the save"

**States**
Same: ok notice `t.compare.matches` "The plugged-in devices match the saved configuration." Changed: warn notice `t.compare.changed` "The configuration changed since the last save." Title `t.compare.title` "Comparison with the saved configuration".

**Messages, warnings, codes**
Device warning code `moved_from`; notice code `hardware_changed` ("The plugged-in devices differ from the saved configuration.").

**Dependencies**
Service (`compare_saved`, `move_assignment`); admin for the menu entry.

**Live behaviour**
Diff is a snapshot taken on open; device names/kinds read from live device list.

#### Import another configuration (`ImportDialog.svelte`)

**Purpose**
Optional importer: the service maps another multiseat product's device-assignment export (JSON) to this config; the result only fills the draft (nothing saved by itself).

**Opens when**
Settings page row `t.settingsPage.import` "Import another configuration" (help "The device assignment file of another multiseat program") (`BasicTab.svelte`), and seats commands `importConfig` (`App.svelte`). `t.systemMenu.import` exists in en.ts with no consumer found.

**Controls**

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `t.import.file` "Choose file" | button opening a hidden file input | accept `.json`, `application/json` | — | file text replaces the content field |
| `t.import.content` "Content" | multi-line text (6 rows, placeholder `{ … }`) | must parse as JSON on Import; else field error `t.errorJson` | empty | local |
| `t.import.run` "Import" | primary button | disabled when content is blank; busy while running | — | `import_reference{json}` → on success draft = returned config (keeps current saved revision), toast `t.import.done` "The imported configuration is loaded. Review it and save.", dialog closes; on failure toast error, dialog stays |
| Cancel `t.cancel` | button | — | — | closes |

Body: `t.import.body` "Paste the content of a device assignment file (JSON) exported by another multiseat product. Nothing is saved by itself; review the result and save." Title `t.import.title`.

**States**
Empty (Import disabled), JSON parse error, busy. Not admin: service refuses (demo `needAdmin`).

**Messages, warnings, codes**
`t.errorJson` (text unclear here, other namespace); error codes via `errorLabel`.

**Dependencies**
Service `import_reference`. The source product format is not described in the UI.

**Live behaviour**
None.

#### Diagnostic report (`DiagnosticDialog.svelte`)

**Purpose**
Create a local diagnostic report; nothing is uploaded, never secrets.

**Opens when**
Settings page row `t.settingsPage.diagnostics` "Diagnostic report" (help "Add it when you report a problem. It is saved on this PC and sent nowhere."); Status page (`onDiagnostics`); the "service cannot be reached" empty state button `t.service.diagnostics` (`App.svelte`).

**Controls**

| Control | Kind/data type | Allowed values | Default | Effect |
|---|---|---|---|---|
| `t.diagnostics.maskUsers` "Hide user names (recommended)" | checkbox | — | checked | `mask_users` parameter |
| `t.diagnostics.create` "Create the report" | primary button | busy while running | — | `diagnostic_report{mask_users}` → path |
| Close `t.close` | button | — | — | closes |

Body `t.diagnostics.body` "The report is saved on this PC; it is sent nowhere. Passwords never go into the report."

**States**
Done: ok notice `t.diagnostics.done(path)` "Report saved: <path>" (demo path `C:\ProgramData\Pulsar Seat\diag\rapor-demo.zip`). Error: danger notice (`errorLabel`). Busy. `t.diagnostics.maskHelp` "Recommended." has no consumer.

**Dependencies**
Service `diagnostic_report` (demo requires admin; service access level unclear in code here). When opened from the "service cannot be reached" state the call needs the service, so it fails with `service_unavailable`.

**Live behaviour**
None.

### 3.5 Remote seats

Scope: the "Remote seats" view of the manager and everything below it. All visible strings are in
`en.ts` under the `rt` object (written below as `rt.<ns>.<key>`); error texts come from
`remoteFieldErrorText`, `remoteErrorText`, and the helpers `remoteErrorMessage` /
`remoteFailureText` in `i18n.ts`. Remote seats are "streamed" seats: a Windows session on this PC
(or in a Hyper-V VM) with a virtual display and virtual gamepads, streamed to a client device with a
stream server. There are two stream servers: **Sunshine** (Moonlight-compatible clients) and **Pulsar**
(Pulsar clients, with relay / Pulsar ID).

General facts that apply to every screen in this part:

- **Admin only.** Every remote request is admin-only (`api.ts`). If the manager is not elevated,
  the whole view shows only one info notice: `t.adminNeeded` "You need to run as administrator for
  this action." (`RemoteTab.svelte`). Nothing else is rendered.
- **Service reachable** = `store.status !== null && !store.serviceDown`. Many actions are disabled
  when it is not reachable.
- **Staged save (draft + save bar).** Seat page, Remote seat settings page and VM seats page keep a
  local draft. Changes are counted (`countChanges` in `stage.svelte.ts`: each changed leaf value
  counts 1, a changed list counts 1). The app shell docks one save bar (`StageBar.svelte` reports to
  `remoteStage`; `App.svelte` renders `SaveBar` with count, detail line, Save, Undo, Ctrl+Shift+S).
  If the draft is invalid, Save is disabled with a reason. Leaving a page (opening another remote
  page, another seat, or another app view) with staged changes asks first (see "Unsaved changes
  dialog").
- **Immediate operations** (not staged): start / stop seat, pair dialog actions, credential test,
  identity renewal, VM disk reset, multiseat toggle, experimental wrapper, audio cleanup, DXVK
  on/off, process patching flow, isolation target / mask lists, VM host actions, live display mode.
- **How remote edits are stored.** Seat create / edit / delete and shared settings are config
  edits: `store.applyRemote` builds the next config, calls validation (`validate`) and, if no
  blocking issue, `apply_config` at once (not the app's general unsaved draft). It then re-reads the
  config (the service fills seat id, port block, RDP client name). Local unsaved edits in the app
  draft are kept (`mergeRemoteInto`). A validation issue `remote_seat_invalid` comes back as a remote
  error code (see `PART: model`, "Error codes").
- **Polling.** `RemoteTab` refreshes every 3000 ms (see `PART: live`).

#### Remote seats home / seat list (`RemoteTab.svelte`)

- **Purpose**: Root page of the remote area. Lists all remote seats and links to the "This PC" sub-pages
  (Host PC, Remote seat settings, DXVK games, VM seats, Logs). Also hosts the New seat dialog, the
  Delete seat confirmation, the Pair dialog and the Unsaved changes dialog, and switches between
  sub-pages (there is no second navigation; sub-pages have a breadcrumb back to this list).
- **Opens when**: App rail entry `t.nav.remote` "Remote seats" (rail shows a count badge = number of
  remote seats in the saved config, `App.svelte` `remoteCount`). Also from the local Seats tab: a remote
  seat's edit button (`SeatsTab.svelte` `commands.openRemote(n)`) opens this view directly on that seat
  page (`initialSeat`). Admin only.
- **Controls**: none other than the action buttons and link rows below.
- **Actions**:
  - Page head title `t.nav.remote` "Remote seats", sub `rt.home.intro` "Seats used over the network from a
    TV, a phone or another computer, with Pulsar or Moonlight."
  - Button `rt.nav.newSeat` "New remote seat" (primary, plus icon): opens New seat dialog. Disabled when the
    service is not reachable or the config is not loaded.
  - One link row per remote seat (`LinkRow`): avatar (Windows account picture of the seat user, else first
    letter of the seat name), label = seat name, help line = parts joined with " · "
    (`rt.view.stateSummary`): `rt.home.seatNumber(n)` "Seat N"; `rt.home.streamWith('Pulsar'|'Sunshine')`
    "streaming with …"; `rt.view.vmOn` "VM" if VM seat; `rt.nav.session(id)` "Session N" if a session exists;
    `rt.nav.streamingTo(client)` "streaming: <client>" or `rt.nav.streaming` "streaming". Value = status pill
    (see States). Click opens the Seat page (asks first if a draft is staged).
  - Section `rt.home.thisPc` "This PC", help `rt.home.thisPcHelp` "Shared by all remote seats." with link rows:
    - `rt.nav.host` "Host PC" — help `rt.home.hostHelp` "Windows edition, multiseat readiness, drivers" → Host page.
    - `rt.home.settings` "Remote seat settings" — help `rt.home.settingsHelp` "Isolation, gamepads, virtual display" → Settings page.
    - `rt.nav.dxvk` "DXVK games" — help `rt.home.dxvkHelp` "Run old Direct3D games through Vulkan" → DXVK page.
    - `rt.nav.vm` "VM seats" — help `rt.home.vmHelp` "Seats that run in their own Hyper-V virtual machine" → VM page.
    - `rt.nav.logs` "Logs" — help `rt.home.logsHelp` "Service and seat logs" → Log page.
  - Delete seat confirm (opened from seat page): see "Delete seat confirmation".
- **States**:
  - Not admin: only `t.adminNeeded` notice.
  - Empty: `rt.nav.noSeats` "No remote seat yet".
  - Row status pill: `rt.seatState.*` — stopped "Stopped" (off), booting "Starting" (warn), running "Running"
    (ok), stopping "Stopping" (warn), failed "Failed" (danger). Override: when remote seats cannot get
    concurrent sessions (`remoteSessionsUnavailable`: configured or active session backend is not
    `rds_loopback` / `termsrv_wrapper`), a non-VM seat without a session shows warn pill
    `rt.view.sessionUnavailable` "Cannot start", and under the list `rt.nav.sessionUnavailableHint`
    ("Remote seats need several Windows sessions at the same time. The session method running on this PC
    does not provide that (for example a client edition of Windows or fast user switching). The seat cannot
    start.").
  - Progress: neutral notice with spinner and `rt.view.progress` "Working" (during delete).
  - Error banner: danger notice with `remoteErrorMessage(e)`.
  - Degraded: notices at top when Host info is loaded (it is loaded on this root page):
    - `rt.host.passive` (warn) if coexistence mode is `passive` (legacy; service no longer sends it).
    - one warn notice per `coexistence.notes` entry, text `rt.coexistNote[n]` (only known: `duo_service_running`).
    - External Remote Desktop wrapper notice (`externalWrapper.ts`): info if `in_use`, warn if `needs_ack` or `blocked`.
- **Messages, warnings, codes**:
  - `rt.coexistNote.duo_service_running`: "Another multiseat product is running. Remote seats are allowed, but
    both products may conflict over sessions, virtual displays and network ports. Stop the other product while
    you use remote seats."
  - `rt.externalWrapper.inUse`: "Remote seats use the Remote Desktop wrapper another program installed. Pulsar
    Seat does not change it; that program keeps it working after Windows updates."
  - `rt.externalWrapper.needsAck`: "Another program's Remote Desktop wrapper is active. Remote seats use it after
    you accept the experimental warning on the "Host PC" page."
  - `rt.externalWrapper.blocked(why)`: "Another program's Remote Desktop wrapper is active, but remote seats cannot
    use it: <why>" (why = `rt.failure[reason]` or the raw reason).
  - `rt.host.passive`: "Another multiseat product was found. Pulsar Seat is in passive mode: it changes nothing on the
    system and starts no seats."
  - Seat limit on create: if no free seat number, error with reason `seat_limit` → `rt.failure.seat_limit`.
- **Dependencies**: admin; service reachable; session backend with concurrent sessions for non-VM seats;
  host info (coexistence, termwrap).
- **Live behaviour**: 3 s poll (users, LAN addresses once; host info once on root/host page); row states follow
  `store.status` (updated by the app); avatars loaded once per user name.

#### Unsaved changes dialog (remote) (`RemoteTab.svelte`)

- **Purpose**: Ask before leaving a remote page that has staged changes.
- **Opens when**: opening another seat / sub-page / going back while `remoteDirty()`. (Leaving the remote view for
  another app view is guarded by `App.svelte` with the same question.)
- **Controls**: None.
- **Actions**: Title `t.unsaved.title` "Unsaved changes", body `t.unsaved.body` "Your changes are not saved yet.".
  Buttons: `t.unsaved.save` "Save" (saves the stage; on failure the dialog closes and the page stays);
  `t.unsaved.discard` "Continue without saving" (discards, then navigates); `t.unsaved.cancel` "Cancel".
- **States**: Save button busy while saving. Alert dialog, initial focus on Cancel.
- **Messages, warnings, codes**: none extra.
- **Dependencies**: None.
- **Live behaviour**: None.

#### New remote seat dialog (`NewSeatDialog.svelte`)

- **Purpose**: Create a remote seat with a name and a stream server.
- **Opens when**: "New remote seat" button on the remote home.
- **Controls**:

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `rt.newSeat.name` "Name" (help `rt.newSeat.hint` "English letters and digits only; no spaces.") | text | 1-128 chars, `^[A-Za-z0-9]+$`, unique among ALL seat names (local and remote), case-insensitive. maxlength 128. Errors shown after first input or submit. | empty | becomes `SeatConfig.name` and initial `remote.display_name` |
| `rt.seat.streamBackend` "Stream server" | segmented, 2 options | `sunshine` "Sunshine (Moonlight compatible)", `pulsar` "Pulsar" | sunshine | `remote.stream_backend` |

- **Actions**: `rt.newSeat.create` "Create" (primary; disabled while name invalid; busy while creating; Enter in the
  name field also submits) → `applyRemote(addRemoteSeat)` with next free seat number; on success the dialog closes
  and the new seat's page opens. `rt.newSeat.cancel` "Cancel".
- **States**: busy; server error notice (danger, `remoteErrorMessage`). If the chosen stream server is known to be not
  installed (`get_stream_backends` says `installed: false`), help line `rt.seat.backendMissingHint(b)` "<b> is not
  installed on this PC; the seat cannot start with this stream server." (still selectable here).
- **Messages, warnings, codes**: field errors `remoteFieldErrorText`: `name_empty` "Type a name.", `name_chars` "Only
  English letters and digits can be used.", `name_long` "The name can be at most 128 characters.", `name_duplicate`
  "A seat with this name already exists."; seat limit → `rt.failure.seat_limit` "The limit of seats that can run at the
  same time was reached." (thrown with reason `seat_limit` when no seat number is free).
- **Dependencies**: service reachable, config loaded. Backend availability list only after a seat page loaded it
  (`backends` is empty = unknown until then).
- **Live behaviour**: None.
- New seat defaults: see `PART: model` → Remote seat (new seat: account = logon screen (no user), port 0 = service picks,
  stop action log_off, autostart off).

#### Delete seat confirmation (`RemoteTab.svelte`, ConfirmDialog)

- **Purpose**: Confirm deleting a remote seat.
- **Opens when**: Seat page → Removal → "Delete seat".
- **Controls**: None.
- **Actions**: Title `rt.deleteSeat.title(name)` "Delete "<name>"?"; body `rt.deleteSeat.body` "The seat's settings and the
  files created for it are deleted."; warning `rt.deleteSeat.warning` "This cannot be undone."; confirm `rt.seat.delete`
  "Delete seat" (danger) → discards stage, `applyRemote(removeRemoteSeat)`, returns to the list; cancel
  `rt.deleteSeat.cancel` "Cancel".
- **States**: progress notice "Working"; error banner on failure.
- **Messages, warnings, codes**: service may refuse (e.g. VM seat active; error code text via `remoteErrorMessage`).
- **Dependencies**: service reachable.
- **Live behaviour**: None.

#### Remote seat page (`SeatPage.svelte`)

- **Purpose**: View and edit one remote seat: state, start / stop, pairing, runtime state, live display mode, and all
  seat settings (connection, session/user, stream server, display, sound, apps, virtual machine, removal).
- **Opens when**: click on a seat row; from local Seats tab edit; right after creating a seat; from VM seats page seat row.
  Breadcrumb `t.nav.remote` "Remote seats" › seat name.
- **Head**: avatar (account picture or initial), title = seat name, status line = state pill (`rt.seatState`)
  + summary parts joined by " · ": "Seat N"; start phase while booting (`rt.phase[phase]`, see model); "streaming: <client>" /
  "streaming"; "streaming with Sunshine|Pulsar" (only when not running); `rt.seat.session(id)` "Windows session: N";
  `rt.seat.crashes(n)` "The stream server crashed once" / "… crashed N times".
- **Controls** (all settings sit inside a block that is made **inert** (not operable) while the seat is booting, running or
  stopping, or the service is unreachable — notice `rt.seatView.locked` "Settings cannot change while the seat runs. Stop the
  seat to change them."):

Section `rt.view.connection` "Connection":

| Control | Kind/data type | Allowed values, validation, limits | Default | Effect |
|---|---|---|---|---|
| `rt.view.address` "Address" | read-only text + copy icon button (`rt.seatView.copyAddress` "Copy the address") | `<first LAN address>:<port>`; help `rt.view.addressHelp` "Type this address on your device, or scan the QR code in "Pair a device"." or `rt.view.noAddress` "No LAN address found yet." | — | clipboard; shows `rt.view.copied` "Copied." |
| `rt.seat.displayName` "Display name" | text, maxlength 128 | not empty, no leading/trailing space, no control chars; unique among other remote seats' display names (case-insensitive) | = seat name at creation | `remote.display_name` (name advertised to clients) |
| `rt.seat.port` "Port" | text, numeric, maxlength 5 | integer 1029-65514; block `port-5 … port+21` must not overlap another remote seat (bases ≥ 27 apart). Port 0 (not yet assigned) is valid until edited. Help `rt.seatView.portRange(from,to)` "Range in use: A–B." | 0 → service assigns | `remote.port` |
| `rt.view.streamServer` "Stream server" (select labelled `rt.seat.streamBackend`) | select | `sunshine` "Sunshine (Moonlight compatible)", `pulsar` "Pulsar"; a backend known as not installed gets suffix " (not installed)" (`rt.seat.backendMissing`) and is disabled unless it is the current one. Disabled while running (help `rt.seat.backendStopFirst` "Stop the seat first to change the stream server.") | sunshine | `remote.stream_backend`. If current backend missing: error line `rt.seat.backendMissingHint(b)` |

Section `rt.view.session` "Session":

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.seat.userName` "Windows user" (help `rt.view.userHelp` "The seat signs in with this account.") | select | `''` = `rt.seat.chooseUser` "Choose a user", then enabled local users (a disabled user is listed only if it is the current one) | none (logon screen) | account: user set → `{kind: local, user}`, empty → `{kind: logon_screen}`. Remote seats use local accounts only. |
| `rt.seat.password` "Password" | locked value + buttons | shows `rt.seatView.passwordSaved` "Saved" or `rt.view.passwordMissing` "Not saved" (from `password_set` of the config view) | — | — |
| ↳ button `rt.seatView.test` "Test" (aria `rt.seatView.testLabel` "Test the password") | button | disabled unless a user is chosen AND a password is saved; help then `rt.view.testNeeds` "Choose a user and save a password first." | — | opens Credential test dialog |
| ↳ button `rt.seatView.change` "Change" (aria `rt.seatView.changeLabel`) | toggle button (aria-expanded) | — | collapsed | shows "New password" row |
| `rt.view.newPassword` "New password" | password input (autocomplete new-password) + `rt.seat.show`/`rt.seat.hide` "Show"/"Hide" | any non-empty; staged; help `rt.view.passwordStaged` "The new password is stored when you save." | empty | on save → `set_seat_password` (`api.setPassword`), then password view refreshed. Counts as 1 change. |
| `rt.view.startWithService` "Start with Pulsar Seat" (help `rt.view.startWithServiceHelp` "When the PC starts, this seat is ready too.") | switch | bool | off | `SeatConfig.autostart` |
| `rt.view.stopAction` "When stopped" (help `rt.view.stopHelp` "“Disconnect” keeps the session open; apps keep running.") | segmented | `log_off` `rt.view.stopLogOff` "Sign out"; `disconnect` `rt.view.stopDisconnect` "Disconnect" | log_off | `remote.stop_action` |

Section `rt.view.streamServer` "Stream server" (help = current backend label):

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.seat.gamepad` "Gamepad type" (Sunshine only) | select | `auto` "Automatic", `x360` "Xbox 360", `ds4` "DualShock 4", `pulsar_ds4` "DualShock 4 (Pulsar pad, isolated)", `pulsar_ds5` "DualSense (Pulsar pad, isolated)", `pulsar_ds5e` "DualSense Edge (Pulsar pad, isolated)", `pulsar_xone` "Xbox One (Pulsar pad, isolated)". Help for `pulsar_*`: `rt.seat.pulsarBuildOnly` "Needs the Pulsar build of the stream server; with the standard build the server's default is used." | auto | `remote.stream.gamepad` |
| `rt.seat.capture` "Screen capture" (both backends) | select | `default` "Default", `ddx` "DDX", `wgc` "WGC", `pulsar_idd` "Pulsar virtual display (HDR)" (help pulsarBuildOnly for pulsar_idd) | default | `remote.stream.capture` |
| `rt.seat.pulsarRelay` "Relay server" (Pulsar only) | text, placeholder `relay.example.net:21116` | empty, or `host` / `host:port`; host = DNS name (labels 1-63 chars, letters/digits/hyphen, not starting/ending with hyphen, ≤253 total) or `[IPv6]`; port 1-65535; ≤260 chars; trimmed. Help `rt.seat.pulsarRelayHint` "host or host:port; empty uses the default relay."; error `rt.seat.pulsarRelayInvalid` "Enter a host name or address, optionally with :port." | empty | `remote.pulsar.relay` |
| `rt.seat.pulsarNetworkMode` "Connection type" (Pulsar) | select | `auto` "Direct when possible, otherwise through the relay", `p2p-only` "Direct only", `relay-only` "Relay only" | auto | `remote.pulsar.network_mode` |
| `rt.seat.pulsarAccess` "Who can connect" (Pulsar) | select | `approve` "Every connection needs approval here", `approve_or_password` "Approval here, or the seat's password" | approve_or_password | `remote.pulsar.access` |
| `rt.seat.pulsarDiscovery` "Show this seat to devices on the local network" (Pulsar) | switch | help when on `rt.seat.pulsarDiscoveryHint` "Opens UDP 21117 for the local subnet in the firewall while the seat runs with this option." | on | `remote.pulsar.lan_discovery` |

Disclosure `rt.view.streamMore` "More stream options" (collapsed by default):

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.seat.minFps` "Lowest FPS target" (Sunshine only) | number | min 1, max 240 (HTML limits only) | 1 | `remote.stream.min_fps` |
| `rt.seat.pulsarNodePort` "Network port (UDP)" (Pulsar) | select + number when fixed | `base` `rt.seat.pulsarNodePortBase(port)` "Seat port (N)", `ephemeral` "Any free port", `fixed` "This port:" + number input. Fixed: integer 1024-65535 (`rt.seat.pulsarNodePortInvalid` "The port must be 1024-65535."); must not be 21117, not inside another seat's block (base-5…base+21), not equal to another seat's fixed node port or a VM seat's forwarded UDP host port (`rt.seat.pulsarNodePortTaken` "Another seat already uses this port, or it is the Pulsar discovery port (21117).") | base, 0 | `remote.pulsar.node_port_mode`, `node_port` |
| `rt.seat.pulsarPortMapping` "Ask the router to open the port (UPnP)" (Pulsar) | switch | bool | off | `remote.pulsar.port_mapping` |
| `rt.seat.pulsarMaxClients` "Devices at the same time" (Pulsar) | number | min 1 max 4 (HTML limits; service validates 1-4) | 1 | `remote.pulsar.max_clients` |
| `rt.seat.firewallDomain` "Also open to domain networks in the firewall" | switch | bool | off | `remote.stream.firewall_domain` |
| `rt.seat.keepPairing` "Keep the pairings when the seat is deleted" | switch | bool | off | `remote.stream.keep_pairing_on_delete` |

Section `rt.view.display` "Display":

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.seat.renderAdapter` "Graphics card (empty: best fit)" | select | `''` `rt.seat.adapterAuto` "Best-fit graphics card", then each render adapter (`list_render_adapters`, label = description); a stored adapter not present is still listed with its stored name | auto | `remote.display.render_adapter_id` + `render_adapter` (name). GPU warnings below (see Messages). |
| `rt.seat.fixedResolution` "Stream resolution" | select (+2 numbers for custom) | `follow` "Follow the client" (0×0), presets 1280×720, 1920×1080, 2560×1440, 3440×1440, 3840×2160, `custom` "Fixed (custom)" → width `rt.seat.fixedWidth` "Width (px)" and height "Height (px)", each 640-8192 (HTML limits; service validates 640-8192). Custom starts at current or 1920×1080. | follow | `remote.display.fixed_width/fixed_height` |
| `rt.seat.refresh` "Refresh rate (Hz)" | number + "Hz" | 15-500 (HTML limits) | 60 | `remote.display.refresh_hz` |
| `rt.seat.lockRefresh` "Keep this refresh rate whatever the client asks for" | switch | bool | off | `remote.display.lock_refresh` |
| `rt.seat.scale` "Desktop scale" (help `rt.view.scaleHelp` "Dynamic: the desktop follows the connected device.") | segmented | `dynamic` "Dynamic", `static` "Fixed" | dynamic | `remote.display.scale_static` |
| `rt.view.scalePercent(p)` "Fixed scale: P %" (only when Fixed) | range slider | 100-500 step 25 | 100 | `remote.display.scale_percent` |
| `rt.seat.forceSdr` "Force SDR on HDR streams" (help `rt.view.forceSdrHelp` "Turn it on if colours look washed out.") | switch | bool | off | `remote.display.force_sdr` |

Disclosure `rt.view.moreDisplay` "HDR and image quality":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.seat.superSampling` "Supersampling" (help shows "P %") | range | 100-500 step 25 | 100 | `display.super_sampling_percent` |
| `rt.seat.sdrWhite` "SDR white level" (help "N nits") | range | 80-480 step 10 | 400 | `display.sdr_white_level_nits` |
| `rt.seat.maxNits` "Peak (nits)" | number | 1-10000 | 1000 | `display.luminance.max_nits` |
| `rt.seat.maxFullFrame` "Full-frame peak (nits)" | number | 0-10000 | 600 | `display.luminance.max_full_frame_nits` |

(`display.luminance.min_nits_x10000` default 10 exists in the record but has no control.)

Section `rt.view.sound` "Sound":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.seat.streamAudio` "Add the seat's sound to the stream" | switch | bool | on | `remote.audio.stream_audio` |
| `rt.seat.microphone` "Microphone (Pulsar client only)" (help `rt.seat.microphoneHint`: client mic given to seat apps; needs a pulsar-host with mic support; Moonlight carries no mic; applies on seat restart) | switch | bool | off | `remote.audio.microphone` |

Section `rt.view.apps` "Apps":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.view.autostartTitle` "Started at sign-in" | table (rows) | columns: `rt.seat.autostartPath` "App path (.exe)" (text + `rt.seat.browse` "Browse" → native .exe picker titled `rt.seat.pickAutostart` "Choose the program to start"), `rt.seat.autostartArgs` "Parameters" (text), `rt.view.asSystem` "SYSTEM" (checkbox, aria `rt.seat.autostartAsSystem` "Run as SYSTEM"), remove icon (`rt.view.removeItem(name)` "Remove: <file>"). Empty: `rt.view.noApps` "No app yet." Button `rt.seat.addApp` "Add app". No client-side validation of the path. | empty list | `remote.apps` (`AutostartApp[]`); the list counts as one change |
| `rt.view.clientApps` "Apps shown on the client" (`AppsEditor.svelte`) | see "Client app list editor" | | | stored by `set_apps` |

Section `rt.view.vm` "Virtual machine": see "VM seat editor" (only shown when the Hyper-V host status is known and the
feature is not `unavailable`, or the seat is already a VM seat).

Section `rt.seatView.removal` "Removal":

| Control | Kind | Effect |
|---|---|---|
| `rt.seatView.regenerateRow` "Renew the seat identity" — button `rt.seatView.regenerate` "Renew identity" (help Sunshine `rt.seat.regenerateConfirm` "The seat's certificate is renewed and every paired device must pair again." / Pulsar `rt.seat.regenerateConfirmPulsar` "The seat's identity is renewed: the Pulsar ID changes and every paired device must pair again.") | danger button → confirm dialog | `regenerate_seat_identity` (confirm true); success info `rt.seat.identityDone` "Identity renewed." |
| `rt.vm.resetSandbox` "Reset the seat's disk" (only VM seats; help `rt.vm.confirmResetSandbox` "Reset this seat's disk? Everything changed inside its VM is lost.") | danger button → confirm dialog | `vm_seats {type: reset_sandbox, seat, confirm: true}` |
| `rt.seat.delete` "Delete seat" (help `rt.view.deleteHelp` "The seat's settings and the files created for it are deleted.") | danger button | opens Delete seat confirmation |

- **Actions** (head, state-dependent):
  - stopped / failed: `rt.seat.start` "Start seat" (primary) → `start_seats [n]`. Disabled if service unreachable, busy, or
    start blocked; blocked reason shown next to it: `rt.view.sessionUnavailable` "Cannot start" (non-VM seat, no concurrent sessions).
  - booting: `rt.seat.stop` "Stop seat" → `stop_seat`.
  - running: "Stop seat" and `rt.seat.pair` "Pair a device" (primary) → Pair dialog.
  - stopping: no action button.
  - Save bar (staged): commit order: (1) seat record + VM part via `applyRemote` (only if changed), (2) new password via
    `set_seat_password`, (3) client app list via `set_apps`. Save bar detail `rt.view.stageSeat` "The seat uses the new settings
    the next time it starts."; invalid reason `rt.view.invalid` "Fix the marked fields first.". Undo resets to stored values.
  - Live display mode "Apply" (see runtime section).
  - Confirm dialogs: Renew identity: title `rt.view.regenerateTitle` "Renew the seat identity?", body as above, confirm "Renew
    identity", cancel `rt.newSeat.cancel` "Cancel". Reset VM disk: title/confirm `rt.vm.resetSandbox`, body `rt.vm.confirmResetSandbox`.
- **Runtime state section** `rt.seatView.runtime` "Runtime state" — shown only while booting/running/stopping and runtime loaded
  (`get_remote_runtime`). Rows (read-only):
  - `rt.seat.stream` "Stream server": help = backend label + "streaming: <client>"; value `rt.seat.streamActive` "Streaming"
    (stream active) or `rt.seat.streamRunning` "Running"; if not running, warn pill "Stopped".
  - Pulsar only: `rt.seat.relayId` "Pulsar ID" = 9-digit relay id or "—"; help `rt.seat.relayState(s)`: ok "connected",
    auth_required "waiting for sign-in", unreachable "unreachable", version_mismatch "version mismatch", else raw.
  - Pulsar only, mic not off: `rt.seat.mic` "Microphone": `rt.seat.micState` off "Off", unsupported "This pulsar-host version
    does not support the microphone", waiting "Waiting for a client connection", connected "Connected", failed "The microphone
    channel could not open", closed "Not connected".
  - `rt.seat.displayBackend` "Display": `rt.displayBackend[kind]`.
  - `rt.seat.displayHost` "Session path" (if known): provider → `rt.seat.hostProvider` "Pulsar protocol provider: own display with
    mode and HDR control"; loopback → `rt.seat.hostLoopback` "Remote Desktop client: stock display, mode follows the client".
  - `rt.seat.fallback` "Own display not used" (if fallback): warn pill `rt.wrdsReason[reason]`.
  - `rt.seat.hdr` "HDR": `rt.seat.hdrOk` "Available" or off pill `rt.hdrReason[code]`.
  - `rt.seat.audio` "Sound": `rt.audioState[state]` + one help line per limitation `rt.audioLimitation[l]`.
  - `rt.seat.pads` "Gamepads" (help always `rt.audioLimitation.no_controller_audio` "The gamepad speaker / headset jack is not
    supported."): `rt.seat.noPads` "No virtual gamepad connected" or list "slot+1: kind (backend label)".
  - `rt.seat.session(id)` "Windows session: N" with help `rt.seat.clientName` "RDP client name: <name>".
  - One row per isolation hook: `rt.seat.isolationHealth` "Isolation module: <exe>" → `rt.view.ready` "Ready" or warn
    `rt.host.notReady` "Not ready"; help = failure text of its error.
  - **Display mode now** `rt.seat.modeControl` (only if runtime has a display): hint `rt.seat.modeHint` "Changes the running
    seat's display at once, without a restart. The seat's fixed resolution and locked refresh rate still apply; the client may ask
    for another mode later." Inputs: width `rt.seat.modeWidth` (640-8192, default 1920), height (640-8192, default 1080), Hz
    `rt.seat.modeHz` (15-500, default 60), checkbox `rt.seat.modeHdr` "HDR" (default off), button `rt.seat.modeApply` "Apply" →
    `set_seat_display_mode`. Result line: for ok / ok_sdr_fallback / clamped `rt.seat.modeApplied(w,h,hz,rw,rh)` "Applied: W × H @ Hz
    Hz (rendered RW × RH)" + " · " + `rt.modeStatus[status]`; others only `rt.modeStatus[status]`. Not staged.
- **States**:
  - Locked (running / booting / stopping or service unreachable): settings inert + lock notice.
  - Failure: danger notice `rt.seat.failure` "Reason for failure: " + `remoteFailureText(failure)`.
  - Last error: warn notice `rt.seat.lastError` "Last error: <detail>" (monospace).
  - Session unavailable (non-VM, no session, no concurrent sessions): warn notice `rt.nav.sessionUnavailableHint`.
  - Error: danger notice from any failed action (`remoteErrorMessage`); info line (role status) for "Copied." / "Identity renewed.".
  - Apps list load failure on open → error notice.
  - Invalid draft: save disabled with "Fix the marked fields first.".
- **Messages, warnings, codes**:
  - Field errors (inline, role alert): display name `display_name` / `display_name_duplicate`; port `port` / `port_overlap`
    (shown only after the port was edited); relay invalid; node port invalid / taken; backend missing hint; VM problems (see VM editor).
  - GPU warnings (`gpu.ts`, under the graphics card row): `rt.seat.gpuNoHardware` "No hardware graphics card found: video is
    encoded on the processor, the stream may be slow." (no adapter marked hardware); `rt.seat.gpuHybrid` "There are several graphics
    cards (hybrid system). Choose the card with the encoder here by hand; automatic choice takes the one with the most memory."
    (several hardware adapters incl. Intel plus another vendor, and no adapter chosen); `rt.seat.gpuNoEncoder(name)` "No hardware video
    encoder is known for <name>; encoding may fall back to the processor." (chosen adapter, or with auto the one with most dedicated
    memory, has a vendor id other than NVIDIA 0x10DE / AMD 0x1002 / Intel 0x8086).
  - Failure codes: `rt.failure.*` (full list in `PART: model`).
- **Dependencies**: admin; service reachable; non-VM seats need a session backend with concurrent sessions (Windows Server RDSH /
  experimental wrapper); stream backend installed; Pulsar-only options shown only for Pulsar; Sunshine-only gamepad / min FPS;
  `pulsar_*` gamepads and `pulsar_idd` capture need the Pulsar build of Sunshine; VM section depends on Hyper-V.
- **Live behaviour**: runtime re-read every 3 s; stored record follows the poll but the draft is replaced only when nothing is staged;
  state, phase, streaming client, crash count update live; live mode apply result.

#### Credential test dialog (`CredentialTestDialog.svelte`)

- **Purpose**: Test a Windows logon with the seat user and stored password (password never returned); optionally enable/unlock the
  account after confirmation.
- **Opens when**: Seat page → Password → "Test" (needs user and saved password).
- **Controls**: None (buttons only).
- **Actions**: runs `test_seat_credentials` immediately on open. Footer: `rt.cred.retest` "Test again" (primary if failed, secondary
  if ok); `rt.cred.fix` "Enable / unlock the account" (only if result disabled or locked out) → confirm step; `rt.cred.close` "Close".
  Confirm step: `rt.cred.confirm` "Yes, change" (danger) → `enable_unlock_account {enable: disabled, unlock: locked_out, confirm: true}`
  then re-test; `t.common`/`rt.common.close` "Close" returns from confirm.
- **States** (title changes): pending `rt.cred.title` "Testing the user credentials" + spinner `rt.cred.pending` "Trying a Windows
  sign-in…"; ok `rt.cred.okTitle` "The user credentials are correct" + `rt.cred.okBody` "This user can sign in with the saved password.";
  failed: title = Windows error message (system language) or `rt.cred.failTitle` "Sign-in failed", body `rt.cred.failHint` "Check the user
  name and password and save them again."; error (request failed): title "Sign-in failed" + danger notice; confirm: title
  `rt.cred.confirmTitle` "Change the account?", warn `rt.cred.confirmBody` "Pulsar Seat will enable this Windows account and unlock it.
  This changes the account itself." (alert dialog).
- **Messages, warnings, codes**: flags `rt.cred.disabled` "This account is disabled.", `rt.cred.locked` "This account is locked (too many
  failed attempts).", `rt.cred.expired` "This account's password has expired; set a new password in Windows." (expired is not fixable here).
- **Dependencies**: admin; works on running or stopped seats (no state check in UI).
- **Live behaviour**: None besides the request.

#### Pair dialog (`PairDialog.svelte`, `QrCode.svelte`)

- **Purpose**: Pair client devices with a running seat inside the manager. Sunshine seats: PIN mode. Pulsar seats: approve mode,
  one-time password, permanent connection password, relay sign-in. Also lists and removes paired devices.
- **Opens when**: Seat page → "Pair a device" (only while state is running). Title `rt.pair.title(name)` "Pair a device with <display
  name or name>", sub `rt.view.pairSub` "A device that paired once connects directly afterwards." Large modal.
- **Left column**:
  - Sunshine: QR code of the first address `<LAN address>:<port>` (label `rt.pair.qr(a)` "QR code for the address: a"), heading
    `rt.pair.addresses` "Addresses", list of every LAN address `addr:port` (first marked `rt.pair.recommended` "recommended" when more
    than one), copy button each (`rt.view.copy(a)` "Copy: a"), help `rt.view.pairScan` "Scan with a phone, or type the address on the
    device." No address: `rt.pair.noAddress` "No LAN address found."
  - Pulsar: if relay id known: QR of the Pulsar ID, heading `rt.view.pulsarCode` "Pulsar ID", the ID + copy. (No QR/ID if unknown.)
  - After copy: "Copied.".
- **Right column — steps** (ordered list, current step marked `aria-current="step"`, earlier steps marked done):
  - Sunshine: `rt.pair.step1` "Open the Pulsar or Moonlight app on your device.", `rt.pair.step2` "Add a new host and type one of the
    addresses below.", `rt.view.stepPin` "Type the four-digit PIN shown on the device here." Current = step 2 (index 1) while waiting,
    step 3 when a request is pending.
  - Pulsar: `rt.pair.pulsarStep1` "Open the Pulsar app on your device.", `pulsarStep2` "Choose the Pulsar ID below or the seat name on
    the LAN.", `pulsarStep3` "The connection request shows here; allow or refuse it."
- **Controls**:

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.pair.pin` "PIN" (Sunshine, request pending) | text numeric, maxlength 4, autocomplete one-time-code; Enter submits | exactly 4 digits (`rt.pair.pinInvalid` "The PIN must be four digits.") | empty | `submit_pin` |
| `rt.pair.clientName` "Device name" (Sunshine) | text maxlength 64 | trimmed; empty → device name from request, else "Device" | pending request's device name | `submit_pin.client_name` |
| `rt.pair.relayPassword` "Relay server password" (Pulsar, relay asks) | password maxlength 128 | required for send | empty | `answer_relay_auth` |
| `rt.pair.relayTotp` "Verification code (6 digits, optional)" | text numeric maxlength 6 | optional | empty | `answer_relay_auth.totp` (null if empty) |
| `rt.pair.connectPassword` "Permanent connection password" (Pulsar) | password maxlength 128, help `rt.pair.connectPasswordHint` "Write only; the saved password is never shown." | non-empty to save | empty | `set_seat_connect_password` |

- **Actions**:
  - Sunshine footer: `rt.pair.submit` "Send PIN" (primary; disabled when busy, seat not running, or no pending request; footer start
    text `rt.pair.pinWhy` "You can type the PIN once a request arrives." when waiting) → `submit_pin` (asynchronous; result arrives as
    `pair_ended` event); success text `rt.pair.sent` "PIN sent.". `rt.pair.cancelRequest` "Refuse the request" (only when pending) →
    `cancel_pair`. `rt.pair.close` "Close".
  - Pulsar request pending: shows `rt.pair.requestTitle` "Connection request", `rt.pair.requestFrom(d, kind)` "“d” wants to pair." /
    "… wants to connect.", `rt.pair.keyFp` "Key fingerprint: <16 hex>"; warn `rt.pair.passwordWrong` "The device entered a wrong
    password." when password_state = wrong. Buttons `rt.pair.allow` "Allow" (allow, full), `rt.pair.viewOnly` "View only" (allow,
    view_only), `rt.pair.deny` "Refuse" → `answer_peer_request`; text `rt.pair.answered` "Answer sent.".
  - Pulsar one-time password block `rt.pair.otp` "One-time password" (only running + pairing available): `rt.pair.showOtp` "Show" →
    `get_seat_otp`; `rt.pair.refreshOtp` "Renew" → `rotate_seat_otp`; OTP shown in monospace; help `rt.view.otpHelp` "Shown only while
    this window is open." (cleared when dialog closes; re-read on `otp_changed` event if shown).
  - Pulsar relay sign-in block `rt.pair.relayAuth` "The relay server asks for a sign-in" (only when `relay_auth_required`): button
    `rt.pair.relayAuthSend` "Send the sign-in details" (disabled without password) → text `rt.pair.relayAuthSent`.
  - Pulsar connection password block: `rt.pair.connectPasswordSave` "Save password" → `rt.pair.connectPasswordSaved` "Connection password
    saved."; `rt.pair.connectPasswordRemove` "Remove password" → inline confirm notice `rt.pair.connectPasswordRemoveConfirm` "Remove the
    permanent connection password? Devices can no longer connect with it." with `rt.pair.connectPasswordRemoveYes` "Yes, remove" (sends
    empty password) / `rt.pair.connectPasswordRemoveNo` "Cancel"; result `rt.pair.connectPasswordCleared` "Connection password removed."
    (An empty field never clears.)
  - Paired devices block `rt.pair.clients` "Paired devices" (when pairing available): `rt.pair.noClients` "No paired device." or list of
    names each with `rt.pair.unpair` "Remove the pairing" (aria `rt.view.unpairLabel(name)`) → `unpair_client`; `rt.pair.unpairAll`
    "Remove all" → confirm `rt.view.unpairAllTitle` "Remove every pairing?" / `rt.view.unpairAllBody` "Every paired device must pair again
    before it can connect." → `unpair_all`.
- **States**:
  - Blocked: warn `rt.pair.notRunning` "The seat must be running to pair." or `rt.pair.unavailable` "In-app pairing is not available on
    this seat (the stream server has no control channel)." (pair state `available: false`).
  - Waiting: spinner + `rt.pair.waiting` "Waiting for a pairing request from the device…" (Sunshine) / `rt.pair.pulsarWaiting` "Waiting for a
    connection request…" (Pulsar).
  - Sunshine request pending: heading `rt.pair.request(name)` "“name” wants to pair", help `rt.view.pairFrom` "Waiting for your PIN.".
  - Result message (ok / danger notice): on `pair_ended` event: ok results `paired`, `allowed`, `allowed_view_only`, `password`; text
    Pulsar `rt.pair.results[r]` (allowed "Connection allowed.", allowed_view_only "View-only access allowed.", denied "Request refused.",
    password "The device connected with the password.", paired "Device paired.", timeout "The request timed out.", client_gone "The device
    disconnected.", locked_out "Too many wrong attempts; the device is blocked for a while.") else `remoteFailureText(r)`; Sunshine
    `paired` → `rt.pair.paired` "Device paired.", else `remoteFailureText` (e.g. `wrong_pin` "Wrong PIN.", `pair_timeout` "The pairing time
    ran out.", `no_pending_request`).
  - Busy: action buttons disabled.
  - Mode: the pair state's `backend` overrides the seat's configured backend once loaded.
- **Messages, warnings, codes**: error codes `no_pending_request`, `wrong_pin`, `service_unavailable`, failure `server_without_control_pipe`
  "In-app pairing is not available on this seat.", `backend_pulsar` / `backend_sunshine` (action not available on this stream server).
- **Dependencies**: seat running; stream server control channel (`available`); Pulsar-only blocks for Pulsar; LAN addresses from the
  manager's Rust side (`lan_addresses`, ordered).
- **Live behaviour**: long poll of `poll_events` (topics `stream` + `status`, timeout 1500 ms, filtered to this seat) while open and
  running; reacts to `pair_pending`, `pair_ended`, `otp_changed`, `trust_changed`, `relay_auth_required` by re-reading pair state and
  clients; on poll error retries after 2000 ms. Stops when closed.

#### Client app list editor (`AppsEditor.svelte`)

- **Purpose**: Edit the stream server's app list (what clients show), part of the seat page's Apps section.
- **Opens when**: always on seat page when the API supports it (`onLoadApps`/`onSaveApps` given).
- **Controls**: table caption `rt.view.clientApps` "Apps shown on the client", help `rt.view.clientAppsHelp` "Shown in the Moonlight and
  Pulsar app lists." Columns `rt.seat.appName` "Name" (text maxlength 128), `rt.seat.appCmd` "Command" (text), remove icon. Empty:
  `rt.view.noApps`. Button `rt.seat.addApp` "Add app" (adds empty row).
- **Actions**: staged with the seat page; saved by `set_apps` after record and password. Before save: rows with empty name are dropped,
  names trimmed, empty command removed. Unknown upstream keys of an entry are kept.
- **States**: loaded once on page open via `get_apps` (demo default: one entry "Desktop").
- **Messages, warnings, codes**: service may refuse with failure `bad_app_list` "The app list is invalid."
- **Dependencies**: VM seats: list kept on host and handed to guest.
- **Live behaviour**: None.

#### VM seat editor (`VmSeatEditor.svelte`)

- **Purpose**: The seat page's "Virtual machine" section: turn VM mode on/off and edit the VM part of a remote seat.
- **Opens when**: seat page, when Hyper-V host status is known (`vm_seats {type: status}`) and its feature is not `unavailable`, or the
  seat already is a VM seat. Section title `rt.view.vm` "Virtual machine", help `rt.view.vmHelp` "Hyper-V. The settings shared by all VM
  seats are on the VM seats page."
- **Controls**:

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.vm.vmMode` "Run this seat in a VM" | switch | on = default VM part; off = remove VM part (back to a shared-PC RDP seat) | off | `remote.vm` present/absent |
| `rt.vm.memory` "Memory (MB, 0 = automatic)" | number min 0 | 0 or ≥ 2048 | 0 (= seat's share of memory_percent) | `vm.memory_mb` |
| `rt.vm.processors` "Processors (0 = automatic)" | number 0-64 | HTML limits | 0 (= half of host logical processors) | `vm.processors` |
| `rt.vm.gpu` "Graphics" | select | `off` "none (software)", `auto` "partition of the first GPU", `adapter` "partition of this GPU" | auto | `vm.gpu` |
| `rt.vm.gpuAdapter` "GPU" (when adapter) | select | host's partitionable GPUs (+ stored one if missing); required | none | `vm.gpu_adapter` |
| `rt.vm.stop` "When the seat stops" | select | `shut_down` "shut the VM down", `save` "save the VM state", `turn_off` "turn the VM off"; `save` not allowed with GPU ≠ off | shut_down | `vm.stop` |
| `rt.vm.hideSuffix` "Advertise the name without “(VM)”" | switch | bool | off | `vm.hide_vm_suffix` |
| `rt.vm.shared` "Shared folders" | table + add row | columns `rt.vm.hostFolder` "Folder on this PC" (read-only text), `rt.vm.mountPoint` "Path in the VM" (text), `rt.vm.readOnly` "read only" (checkbox), remove (`rt.vm.remove`). Add: text (placeholder `D:\Games`) + `rt.vm.browse` "Browse" (folder picker) + `rt.vm.addFolder` "Add folder" (disabled if empty). New folder: guest path `C:\HostDrive` for a drive root, else `C:\HostFolders\<last folder>`; read-only on. | empty | `vm.shared_folders` |
| `rt.vm.forwards` "Forwarded ports (host to VM)" (help `rt.vm.forwardsHint` "The seat's streaming ports are always forwarded.") | table + add | columns `rt.vm.description` "Description", `rt.vm.protocol` "Protocol" (TCP/UDP), `rt.vm.hostPort` "Host port" (1-65535), `rt.vm.guestPort` "VM port" (1-65535), remove. Button `rt.vm.addForward` "Add port" (new row tcp, 0, 0). | empty | `vm.forwarded_ports` |

- **Actions**: all staged with the seat page draft (VM changes count separately; saved together with the record).
- **States**: info notice `rt.view.vmHostMissing` "Hyper-V is not on. Turn it on in the Network addresses settings first." when feature
  ≠ enabled. Row help `rt.vm.guestAddress(a)` "VM address: a" / "VM address: assigned when saved". When VM on and info known:
  `rt.vm.state(s)` "VM: <Hyper-V state>" · guest phase `rt.vm.guestPhases` (waiting "waiting for its settings", configuring "setting up",
  signed_in "signed in", streaming "streaming", failed "failed") · guest detail text.
- **Messages, warnings, codes** (`vmSeatProblem`, first problem shown as warn notice; also blocks save): `rt.vm.problems.memory` "Memory must be 0
  (automatic) or at least 2048 MB."; `gpuAdapter` "Choose the GPU."; `saveWithGpu` "A VM with a GPU cannot be saved; shut it down instead.";
  `hostPath` "The folder must be an absolute path on this PC." (`X:\…`, ≤260 chars, no control chars / quotes / backtick / `$`, no `..`);
  `mountPoint` "Use a drive letter other than C:, or a C:\ path with a folder and no trailing backslash." (also no `\\`, no `:*?<>|/`
  after `C:\`); `mountTwice` "The same path in the VM is used twice." (case-insensitive); `port` "Ports must be 1-65535."; `portInStream`
  "The host port is one of the seat's streaming ports." (inside port-5…port+21); `portTwice` "The same host port is forwarded twice."
  (same protocol + host port). Check order as listed.
- **Dependencies**: Hyper-V feature (Windows edition with Hyper-V); VM network and base disk on the VM seats page; GPU partitioning support.
- **Live behaviour**: VM state/phase from Hyper-V host status (loaded once per seat page visit, `vmHost` cached in RemoteTab; refreshed after
  disk reset).

#### Host PC page (`HostPage.svelte`)

- **Purpose**: Show how ready this PC is for remote seats: Windows edition, multiseat class, RDS state, licensing, warnings, compatibility
  findings, experimental multi-session wrapper (incl. another program's wrapper), virtual display / gamepad drivers, Remote display protocol
  provider; run stale audio endpoint cleanup.
- **Opens when**: Remote home → This PC → "Host PC". Breadcrumb "Remote seats › Host PC". Title `rt.host.title` "Host PC", sub
  `rt.view.hostIntro` "How ready this PC is for remote seats."
- **Controls**:

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.host.multiSeatToggle` "Prepare this PC for multiseat" (help `rt.host.multiSeatHint` "Changes the Remote Desktop settings for seats (the original values are kept and restored when turned off). Turning it off stops all seats.") | switch, applies at once | bool. Read-only locked value ("On"/"Off") when passive or host class `home` | from host info `multi_session` | `set_multi_session`, then config re-read (`settings.remote.multi_session` is service-owned) |

- **Actions**:
  - `rt.host.refresh` "Refresh" (head) → `get_host_info`.
  - Experimental row (section `rt.view.experimental` "Experimental", label `rt.host.experimental` "Experimental: multiple sessions on
    client Windows"; only when termwrap status is present). Button depends on state:
    - external wrapper `in_use`: `rt.host.twExternalStop` "Stop using" → `set_termwrap(false, null, 0)`; help = inUse text +
      `rt.externalWrapper.stopHint` "Remote seats stop using it; the wrapper itself stays on." + wrapper DLL path.
    - external `needs_ack`: `rt.host.twExternalUse` "Use for remote seats" → Experimental dialog (acknowledge only).
    - external `blocked`: warn pill `rt.failure[reason]`.
    - own state `on`: `rt.host.twDisable` "Turn off" (aria `rt.view.twDisableLabel` "Turn off multiple sessions on client Windows") →
      `set_termwrap(false)`.
    - `off` or `blocked`: `rt.host.twEnable` "Turn on" (aria `rt.view.twEnableLabel`) → Experimental dialog.
    - `foreign_active` (or other): warn pill `rt.host.twState[state]`.
    - help (not external): `rt.host.experimentalHint` "Only for Windows Pro / Enterprise, and needs an open-source wrapper that you install.
      Off by default." + `rt.view.twHelp(state)` "Now: <state>. Windows updates can break this feature."
    - All buttons disabled while busy or passive. Enabling stops every remote seat and restarts the Remote Desktop service (`api.ts`).
  - Audio cleanup (section `rt.host.audioCleanup` "Old sound devices"; only when service reachable): row `rt.view.cleanupRow` "Leftover device
    entries", help `rt.host.audioCleanupHint` (deletes leftover "Remote Audio" entries of ended RDP sessions; does not touch connected or
    console devices) or the result; button `rt.view.cleanup` "Clean up" (aria `rt.view.cleanupLabel`) → confirm `rt.view.cleanupTitle`
    "Clean up old sound devices?" / `rt.host.audioCleanupConfirm` "Delete the leftover device entries permanently? Every deletion is written to
    the service log." / `rt.host.audioCleanupYes` "Yes, clean up" (danger) / `rt.host.audioCleanupNo` "Cancel" → `cleanup_audio_endpoints
    {confirm: true}`; result `rt.host.audioCleanupDone(removed, failed)` "No old device to delete." or "R device(s) deleted, F device(s) left."
    Disabled while busy or passive.
- **Read-only rows**:
  - Section `rt.view.hostSection` "Multiseat": `rt.host.edition` "Windows edition" = "<edition> (<build>)"; `rt.view.hostClass` "Multiseat
    state" = "Ready" if `server_rdsh`, else pill `rt.hostClass[class]` (home = danger, others warn); `rt.host.rdsReady` "Remote Desktop
    service" = "Ready" / warn "Not ready"; `rt.host.port` "RDP port" (if listener port known); `rt.host.maxSeats` "Most seats at the same
    time" = number or `rt.host.unlimited` "Unlimited"; `rt.host.licensing` "RDS licensing" (if present) = `rt.host.licensingMode(m)` (2
    "Per device", 4 "Per user", else "Not configured"), help `rt.host.graceDays(d)` "1 day of the grace period left" / "N days …".
  - Section `rt.view.hostWarnings` "Warnings" (only if any): one row per warning, alert icon, `rt.hostWarning[kind]`, help graceDays if days.
  - Section `rt.host.coexistence` "Compatibility check": findings rows with alert icon `rt.signal[signal]`; none → help `rt.host.noFindings`
    "No problem found."
  - Section `rt.host.displayDrivers` "Virtual display" (if backends or WRDS info): per display backend `rt.displayBackend[kind]`: off pill
    `rt.host.installedNo` "Not installed", or off pill `rt.host.consoleOnly` "Physical screen only" (not remote-session capable), or
    "Ready"; help (installed) = "Signed"/"Unsigned", driver version, HDR text (`rt.host.hdrAvailable` "HDR available" or
    `rt.host.hdrUnavailable(why)` "No HDR: <hdrReason>"). Row `rt.host.wrds` "Remote display protocol provider": not wanted and absent →
    `rt.host.wrdsNotWanted` "Not used (display setting is not “Pulsar virtual display driver”)"; no reason → `rt.host.wrdsReady` "Ready: remote
    seats get the Pulsar display"; else pill `rt.wrdsReason[reason]` (warn if wanted, off if not).
  - Section `rt.host.padDrivers` "Virtual gamepad" (if any): per pad backend `rt.padBackend[id]`: "Installed" / off "Not installed"; help
    signed/unsigned + version.
- **States**: loading `rt.common.loading` "Loading…" (role status) until host info; error notice; passive warn notice; coexistence notes
  (`duo_service_running`) warn notices; busy on Refresh.
- **Messages, warnings, codes**: `rt.hostClass` (home "Windows Home: multiseat is not supported.", client_multi_session_unavailable "Client edition of
  Windows: only one remote session at a time.", server_without_rdsh "Windows Server without the Remote Desktop Session Host role: one trial seat
  only.", server_rdsh "Windows Server + Remote Desktop Session Host: multiseat ready.", client "Needs the experimental multi-session feature");
  `rt.hostWarning` (17 kinds, see model); `rt.signal` (18 signals, see model); `rt.host.twState` (off, on, foreign_active "Another tool redirected the
  Remote Desktop service; Pulsar Seat does not touch it.", external "Another program's wrapper is active; Pulsar Seat uses it and does not change it.",
  blocked "Turned off automatically (unsupported version or wrapper problem)"); termwrap failure reasons (`not_client_sku`, `foreign_wrapper`,
  `external_wrapper`, `concurrent_sessions_off`, `external_wrapper_inactive`, `unknown_termsrv_build`, `wrapper_missing`, `wrapper_unsigned`,
  `wrapper_insecure_acl`, `wrapper_arch`, `termservice_restart`, `warning_not_acknowledged`).
- **Dependencies**: Windows edition / SKU (class), RDSH role, RDS licensing; experimental wrapper only for client Pro/Enterprise; service reachable
  for audio cleanup.
- **Live behaviour**: host info loaded on open and on Refresh / after each action; not part of the 3 s poll once loaded.

#### Experimental dialog, remote variant (`../components/ExperimentalDialog.svelte`, variant `remote`; test in `remote/components/ExperimentalDialog.test.ts`)

- **Purpose**: Explicit risk acknowledgement before turning on multiple sessions on client Windows with a wrapper the operator installed, or before
  using another program's active wrapper for remote seats.
- **Opens when**: Host page experimental row "Turn on" (own wrapper) or "Use for remote seats" (external wrapper needing ack).
- **Controls**: warning text (service text, versioned `WARNING_VERSION`, with bold parts and bullet lines); `rt.experimental.wrapper` "Wrapper" select of
  installed wrapper candidates (`service_dll` paths; first preselected) — not shown for external mode (info notice `rt.host.twState.external` + path
  instead) or when no candidate (danger notice `rt.host.twNoCandidate` "No installed wrapper was found."); checkbox `rt.experimental.ack` "I have read and
  understood the risks; I accept the responsibility." (unchecked).
- **Actions**: primary `rt.experimental.enable` "Turn on and restart the Remote Desktop service" (external: `rt.experimental.external.enable` "Use it for
  remote seats") → `set_termwrap(true, path, warning_version)`; disabled until ack ticked and a candidate exists (reason text in footer: local
  `t.experimental.ackFirst` or `rt.host.twNoCandidate`). `rt.experimental.cancel` "Cancel".
- **States**: busy spinner `rt.experimental.working` "Stopping the seats and restarting the service…" / external "Stopping the seats…"; error notice. Alert
  dialog, initial focus Cancel. Title `rt.experimental.title` "Turn on the experimental feature".
- **Messages, warnings, codes**: see Host page termwrap reasons.
- **Dependencies**: client Pro/Enterprise; owned by area of `components/` (shared with local Advanced page) — only remote variant described here.
- **Live behaviour**: None.

#### Remote seat settings page (`SettingsPage.svelte`)

- **Purpose**: Settings shared by all remote seats: isolation (HID, process patching, Steam split, module app lists), gamepads, virtual display
  method, stream server log level.
- **Opens when**: Remote home → This PC → "Remote seat settings". Breadcrumb. Intro `rt.view.settingsIntro` "Shared by all remote seats. The start mode
  and the service log level are on the Settings page." + link `rt.view.openSettings` "Open Settings" (navigates to the app Settings view).
- **Controls**:

Section `rt.settings.isolation` "Isolation" (help `rt.settings.isolationNote`: HID isolation and Steam split apply to newly started seats; process
patching loads the module into chosen apps (x64 and ARM64); on ARM64, x64 apps get it only when Pulsar Seat starts them):

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.settings.hidIsolation` "Isolate HID devices" (help `rt.view.hidHelp` "Each seat sees only its own gamepads and devices.") | switch (staged); locked value when passive | bool | off | `settings.remote.hid_isolation` |
| `rt.settings.processPatching` "Turn on process patching" (help `rt.settings.installed` "Module installed" / `rt.settings.notInstalled` "Module not installed (device security only)") | switch showing STORED value; change opens Patching flow (not staged) | bool. Locked value when passive, or when module unsupported and currently off; extra help `rt.settings.unsupportedArch` "No process patching module for this architecture is installed on this PC." | off | `settings.remote.process_patching` (saved immediately by the flow) |
| `rt.settings.steamIsolation` "Split Steam per seat" | switch (staged); locked "Off" when passive or process patching off; saving with patching off forces false | bool | off | `settings.remote.steam_isolation` |
| `rt.settings.isolationState` "Isolation state" | read-only, one row per loaded module name | — | — | from `get_shim_status.modules` |
| `rt.settings.targets` "Apps the module loads into (file name)" (help `rt.settings.targetsGameInput`: adding GameInputSvc.exe turns on an experimental controller-focus filter) | list + text input (placeholder `rt.common.exampleExe` "game.exe") + `rt.settings.addTarget` "Add"; remove icons | trimmed non-empty file name; no other validation | from service | immediate `set_shim_targets` |
| `rt.settings.maskApps` "Apps the remote session is hidden from (full path)" (help `rt.settings.maskHint` "On the seat, the app does not see that it runs in a remote session. Turn it on only for games that need it.") | list + text (placeholder `C:\Games\game.exe`) + "Add"; remove icons | trimmed non-empty | from service | immediate `set_app_masking(path, on/off)` |

Notices under isolation: always warn `rt.settings.antiCheat` "Warning: some games that use anti-cheat software may see the module loaded into processes, or
remote session masking, as a problem."; `rt.settings.killSwitch` "Kill switch on: the module installs no hook in any process." (if kill switch);
`rt.settings.foreign(s)` "Another product's patching database was found (<signal text>). Turn off that product's process patching first." (if foreign).
Isolation lists shown only after `get_shim_status` loaded. List/add/remove buttons disabled when passive.

Section `rt.settings.pads` "Gamepads":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.settings.padBackend` "Virtual gamepad driver" | select | `''` `rt.settings.auto` "Automatic", `vigem` "ViGEmBus (Xbox 360 / DualShock 4)", `usbip_ds5` "usbip-win2 (DualSense)", `pulsar_pad` "Pulsar gamepad driver (unsigned, trial)" | Automatic (null) | `settings.remote.pad_backend` |
| `rt.settings.bluetoothConsoleOnly` "Use Bluetooth only on the physical screen (console session)" | switch | bool | on | `settings.remote.bluetooth_console_only` |
| `rt.settings.padAudio` "DualSense speaker, microphone and haptics audio also go to remote seats (Pulsar streaming only; applies after the service restarts)" | switch | bool | off | `settings.remote.pad_audio` |

Section `rt.view.display` "Display":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.settings.displayBackend` "Virtual display method" | select | `''` "Automatic", `rdp_session` "Remote Desktop display (default)", `vdd` "Virtual Display Driver (signed, physical screen only)", `pulsar_idd` "Pulsar virtual display driver (unsigned, trial)" | Automatic (null) | `settings.remote.display_backend` (pulsar_idd also makes the service register its Remote display protocol provider) |

Section `rt.view.remoteLogs` "Remote seat logs":

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.settings.verbosity` "Detail level" (help `rt.view.remoteLogsHelp` "Detail of the stream server logs of the remote seats. The service log has its own level in Settings.") | select | none "None", error "Error", warning "Warning", info "Information", debug "Debug" | info | `settings.remote.verbosity` |

- **Actions**: staged save (detail `rt.view.stageShared` "New remote seats and restarted seats use the new settings."); after save the isolation status
  is re-read. Process patching via Patching flow; reboot via `request_reboot`.
- **States**: whole form only after settings loaded; warn `rt.service.unreachable` "The service cannot be reached" when not running / no status;
  info "Version <v> (developer (console) mode)" in console mode (remote status sets console_mode false, so not shown in practice); error notice;
  passive → locked values.
- **Messages, warnings, codes**: failure `shim_foreign` "Another product's process patching is on.", `shim_blocked` "Windows application control
  blocked the module from loading."
- **Dependencies**: process patching module per architecture (x64 / ARM64); passive mode; admin.
- **Live behaviour**: stored settings follow the store; draft replaced only when nothing staged. `settings.remote.start_with_windows` and
  `multi_session` are not edited here.

#### Patching flow (`PatchingFlow.svelte`)

- **Purpose**: Confirmed install / removal of process patching (compatibility layer), then reboot prompt.
- **Opens when**: toggling "Turn on process patching" on the Remote seat settings page.
- **Controls**: None (buttons only).
- **Actions / steps**:
  - confirm: title `rt.patching.install.title` "Install process patching?" / `remove.title` "Remove process patching?"; bullets (install: "Improves
    controller (HID) isolation.", "Makes Steam isolation usable.", "Fixes apps that refuse to run in a remote session."; remove: "Controller (HID)
    isolation gets weaker.", "Steam isolation turns off.", "Apps that refuse to run in a remote session may stop working again."); question
    "Install the compatibility layer now?" / "Remove …"; install only: warn `rt.patching.warning` "Compatibility layers work in Windows' program loading
    path; a bug can make the whole system unusable." + `rt.patching.caution` "Be careful.". Buttons `confirm` "Install" (danger) / "Remove" (primary),
    `rt.patching.cancel` "Cancel".
  - progress: title "Installing the compatibility layer…" / "Removing …", spinner "Loading…", min ~3 s visible; no close.
  - reboot: title `rt.patching.rebootTitle` "Restart needed", body `rt.patching.rebootBody` "Windows must restart for the change to take full effect.";
    `rt.patching.rebootNow` "Restart now" (`request_reboot` via service) / `rt.patching.rebootLater` "I will restart later".
  - error: title `rt.common.error` "Error", danger notice, "Close".
- **States**: switch shows stored value until saved. Saving only stores `process_patching` (and steam_isolation false when turning off); other staged
  values stay staged.
- **Messages, warnings, codes**: as above.
- **Dependencies**: admin; module installable for the architecture.
- **Live behaviour**: None.

#### Log page (`LogPage.svelte`)

- **Purpose**: Read the service log or a remote seat's stream-server log (redacted by the service).
- **Opens when**: Remote home → This PC → "Logs". Title `rt.logs.title` "Logs", sub `rt.logs.redacted` "Lines that may contain a password, PIN or key are hidden."
- **Controls**:

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.logs.source` "Source" | select | `service` `rt.logs.service` "Service", then `seat:<n>` "Seat: <name>" per remote seat | service | `get_log_tail {source, lines: 300}` reloads on change |

- **Actions**: `rt.logs.refresh` "Refresh" (busy while loading).
- **States**: empty `rt.logs.empty` "The log has no lines."; error notice (lines cleared); lines with ERROR/Error/Hata marked as error, WARN/Warn/Warning/Uyarı
  as warning (visual level only).
- **Messages, warnings, codes**: non-remote seat source → `invalid_value`.
- **Dependencies**: admin; VM seats' logs forwarded from guest.
- **Live behaviour**: no auto refresh; last 300 lines.

#### DXVK games page (`DxvkPage.svelte`)

- **Purpose**: Turn DXVK (Direct3D 8/9 through Vulkan) on/off per game; the service copies the pinned DXVK release next to the game (journaled,
  originals backed up) or restores the originals.
- **Opens when**: Remote home → This PC → "DXVK games". Title `rt.dxvk.title`, sub `rt.dxvk.intro` "DXVK runs old Direct3D 8/9 games through Vulkan.
  Files are copied only into the game's folder; turning it off brings the old files back."
- **Controls** (section `rt.view.addGame` "Add a game"; inert when disabled, busy, or no release downloaded):

| Control | Kind | Allowed | Default | Effect |
|---|---|---|---|---|
| `rt.dxvk.exe` "Game file (.exe)" | text (placeholder `C:\Games\game.exe`) + `rt.dxvk.browse` "Browse" (picker titled `rt.dxvk.pickTitle`) | `^[a-zA-Z]:\\.+\.exe$` (case-insensitive), trimmed; else `rt.dxvk.needExe` "Enter the full .exe path of the game." | empty | — |
| `rt.dxvk.dlls` "Direct3D versions to redirect" | checkboxes "Direct3D 9", "Direct3D 8" | at least one (`rt.dxvk.needDll` "Choose at least one Direct3D version.") | D3D9 on, D3D8 off | dll list `d3d9`, `d3d8` |

- **Actions**: `rt.dxvk.enable` "Turn DXVK on" → `set_dxvk(exe, true, dlls)`, success `rt.dxvk.enabled` "DXVK turned on."; per game
  `rt.dxvk.disable` "Turn DXVK off" (aria `rt.view.dxvkDisableLabel(exe)`) → `set_dxvk(exe, false, [])`, success `rt.dxvk.disabled`; `rt.dxvk.refresh` "Refresh".
- **States**: section `rt.dxvk.games` "Games" with help `rt.dxvk.release(v)` "Downloaded DXVK version: v"; empty `rt.dxvk.noGames` "No game has DXVK turned
  on yet."; each game row: file name, full path, status `rt.dxvk.deployed(v, dlls)` "On (d3d9, version v)" / `rt.dxvk.notDeployed` "Off (files removed)" /
  `rt.dxvk.markerUnreadable` "The marker file could not be read; check the files by hand."; warn `rt.dxvk.noRelease` "DXVK is not downloaded yet. First run
  "pulsar-split-shimctl dxvk download" in an administrator command prompt."; disabled when service unreachable or passive; ok / danger notices.
- **Messages, warnings, codes**: failures `dxvk_not_downloaded` "DXVK is not downloaded yet.", `dxvk_hash_mismatch` "The DXVK files do not match the expected
  version; download again."
- **Dependencies**: DXVK release downloaded by CLI; service reachable.
- **Live behaviour**: reload after each action; manual refresh.

#### VM seats page (`VmPage.svelte`)

- **Purpose**: Hyper-V host state and its confirmed actions (VM network, base disk), machine-wide VM settings, and the list of remote seats with their VM state.
- **Opens when**: Remote home → This PC → "VM seats". Title `rt.vm.title` "VM seats", sub `rt.vm.intro` (VM seat runs its Windows session in its own
  Hyper-V VM from a common base disk and keeps its own changes).
- **Controls** (section `rt.view.vmSettingsSection` "Shared VM settings", staged):

| Control | Kind | Allowed / validation | Default | Effect |
|---|---|---|---|---|
| `rt.vm.storage` "Storage folder (empty = default)" | text (placeholder = service's current storage) + Browse (folder) | empty or absolute path (`rt.vm.problems.storage` "The storage folder must be an absolute path.") | empty | `settings.remote.vm.storage_location` |
| `rt.vm.memoryPercent` "Memory for all VM seats (% of this PC)" | number + % | 10-90 (`memoryPercent` "Memory share must be 10-90 %.") | 50 | `memory_percent` |
| `rt.vm.storageGb` "Base disk size (GB)" | number + GB | 32-4096 (`storageGb` "Size must be 32-4096 GB.") | 128 | `storage_gb` |
| `rt.vm.imageKind` "Windows image" | select | `none` "none chosen", `wim` "setup image (.wim / .esd)", `vhdx` "prepared disk (.vhdx)" | none | `image_kind` |
| `rt.vm.imagePath` "Image path" (not none) | text + Browse (`vm_image` picker) | absolute path ending .wim/.esd (wim) or .vhdx (vhdx) (`image` "Choose an absolute path to a .wim / .esd or .vhdx file.") | empty | `image_path` |
| `rt.vm.imageIndex` "Edition index" (wim) | number 1-64 | HTML limits | 1 | `image_index` |
| `rt.vm.languageCab` "Language pack (.cab, optional)" (wim) | text + Browse (`language_cab` picker) | empty or absolute .cab (`languageCab` "The language pack must be an absolute .cab path.") | empty | `language_cab` |
| `rt.vm.restartGpus` "Restart the graphics adapters after a VM with a GPU stops" | switch | bool | off | `restart_gpus_on_shutdown` |
| `rt.vm.natPrefix` "VM network (a.b.c.0/len)" | text | `^\d{1,3}\.\d{1,3}\.\d{1,3}\.0/(16..28)$` (`natPrefix` "Use a network such as 172.31.240.0/24 (length 16-28).") | 172.31.240.0/24 | `nat_prefix` |

- **Actions**:
  - `rt.vm.refresh` "Refresh" → `vm_seats {type: status}`.
  - Network row `rt.vm.network` "VM network": "Ready." or off pill "Not set up yet." + `rt.vm.prepareNetwork` "Set up the VM network" (disabled if
    disabled/busy/Hyper-V not enabled) → confirm `rt.vm.confirmNetwork` "Create an internal virtual switch and a NAT for the VM seats? Uninstall removes
    them." → `prepare_host`.
  - Base disk row `rt.vm.base` "Base disk": building → warn `rt.vm.building` "Building the base disk… this takes several minutes."; present → "Built." +
    `rt.vm.resetBase` "Delete the base disk" (danger) → confirm `rt.vm.confirmResetBase` "Delete the base disk and every VM seat's disk? All changes inside
    the VM seats are lost." → `reset_base`; missing → "Not built yet." + `rt.vm.buildBase` "Build the base disk" (disabled if disabled/busy/Hyper-V not
    enabled/image kind none — uses STORED settings) → confirm `rt.vm.confirmBuild` → `build_base`. Confirm dialogs: title and confirm = action label,
    cancel `t.cancel`.
  - Seat rows (section `rt.vm.seats` "Seats"): link row per remote seat → opens its seat page; value `rt.view.vmOn` "VM" / `rt.view.vmOff` "Shared PC
    session"; help "Seat N · VM address: … · VM: <state> · <guest phase>". Help of section `rt.view.vmSeatsHelp` "Turn VM mode on in a remote seat's page
    (Virtual machine section)." or empty `rt.vm.noRemoteSeats` "No remote seat yet. Create one, then turn VM mode on here."
  - Staged save of settings (detail "New remote seats and restarted seats use the new settings."; invalid = first problem text).
- **States**: host loading "Loading…"; Hyper-V row: "on" or warn pill `rt.vm.featureStates` (disabled "off", enable_pending "on after a restart", unavailable
  "not available on this Windows edition"); help `rt.vm.enableHint` "Turn Hyper-V on in Advanced > Network addresses first (a restart follows)." or
  `rt.vm.hostFacts(mb, cpus)` "N GB memory, M logical processors"; GPU row `rt.vm.gpu` "Graphics" help `rt.vm.gpus(n)` "No GPU can be partitioned: VM seats
  render in software." / "N GPU(s) can be partitioned."; last error row `rt.vm.lastError(e)` "Last error: e" with alert icon; settings problem warn notice;
  error notice; whole page actions disabled when service unreachable or passive.
- **Messages, warnings, codes**: start refusals (service side, shown via remoteErrorMessage when relevant): `hyperv_unavailable`, `vm_network_missing`,
  `vm_base_missing`, `vm_not_assigned`, `vm_gpu_missing`, `missing_credentials` (unclear which text they map to, see unclear).
- **Dependencies**: Hyper-V capable Windows edition; Hyper-V enabled elsewhere (Advanced > Network addresses); GPU partitioning.
- **Live behaviour**: host status loaded on open and after each action/refresh; not polled.

#### Stage bar (`StageBar.svelte`)

- **Purpose**: Report a remote page's draft to the shell (`remoteStage`) so the docked save bar and unsaved-changes guard work.
- **Opens when**: rendered by SeatPage, SettingsPage, VmPage. Inside the app the shell renders the bar; standalone (tests) it renders `SaveBar` itself when
  count > 0.
- **Controls**: Save (disabled with reason when invalid), Undo; Ctrl+Shift+S shortcut.
- **Actions**: Save → page `save()`; Undo → page `discard()`.
- **States**: count of changed values; detail line; invalid reason; busy.
- **Messages, warnings, codes**: details `rt.view.stageSeat`, `rt.view.stageShared`; invalid `rt.view.invalid` or VM problem text.
- **Dependencies**: service reachable (shell hides it when service down).
- **Live behaviour**: count updates as fields change.

## 4. Data model shown in the UI

### 4.1 Global: app, service, preferences, store

#### Prefs (`prefs.ts`, localStorage key `pulsar-seat.manager.prefs.v1`, per user, per WebView data folder)
- `scaleIcons` boolean, default true (where edited: other part).
- `evenRows` boolean, default false.
- `flashSeconds` number 2..20 step 2, default 4 ("just connected" frame).
- `showAllHubs` boolean, default true.
- `sharedInSeats` boolean, default false.
- `showIds` boolean, default false (View settings dialog).
- `visibleSeats` uint32 bitmask, default all seats.
- `askOnManualStart` boolean, default true (Settings; Start confirm "Don't ask again").
- `noShowGoodApply` boolean, default false (Settings; hides "Settings saved." toast).
- `hideTray` boolean, default false (Settings; tray icon visibility).
- `theme` `system|light|dark`, default `system` (Settings).
- `tab` `seats|remote|settings|advanced|status`, default `seats` (rail).
- `trayNoticeShown` boolean, default false (set after first hide-to-tray toast).
- `hideSameAccountWarning` boolean, default false (Same-account dialog).
- Invalid/unknown stored values fall back to defaults per field.

#### Language preference (`i18n.ts`)
- localStorage `pulsar-seat.manager.lang.v1`: `en` | `tr`; absent = `system`. `LangCode` = `en|tr`; `LangPref` = `en|tr|system`. Labels: `t.basic.languageSystem` "Windows display language", `t.basic.languageNames` en "English", tr "Türkçe".

#### CallerInfo (from `hello`)
- `client` ClientKind; `session_id` number; `seat` SeatNumber|null; `is_admin` boolean; `restricted` boolean. `HelloReply` also `service_version`, `protocol_version`, `min_protocol_version` (protocol 3).

#### AppInfo (`app_info`)
- `version` (manager crate version), `arch` (`x86_64`/`aarch64`/...), `autostart` (launched with `--tray`/`--autostart`/`-autostart`), `elevated_launch` (`--elevated[=pid]`).

#### ServiceInfo (`service_state`)
- `state` ServiceRunState: `not_installed` "Not installed", `stopped` "Stopped", `running` "Running", `pending` "Changing", `unknown` "Unknown". `elevated` boolean.
- `ServiceAction`: `start`, `stop`, `restart`.

#### ServiceStatus (global fields used here)
- `service_version`, `product` (ProductState), `seats` (SeatRuntime[]), `active` (session/display/input/audio/gates), `fallback_notes` string[], `config_revision` number, `reboot_pending` boolean, `os_build_changed` boolean, `wizard_running` boolean, `last_event_seq`, `host_console?`.

#### ProductState
- `disabled` "Off"; `enabled{started}` "On, seats running" / "On, seats not started"; `safe_mode{reason}` "Safe mode (seat 1 only)". `SafeModeReason`: `windows_safe_mode`, `unclean_shutdown`, `crash_loop` (texts in screens part).

#### SeatState (labels `t.seatState`, phases `t.phases`)
- `off`, `waiting_for_display`, `starting{phase}`, `at_logon`, `signed_in{user}`, `locked{user}`, `stopping`, `error{reason, detail}` (architecture §6). Phases: `queued`, `waiting_for_host`, `binding_display`, `launching_host`, `connecting`, `waiting_for_session`, `logging_on`.

#### UpdateSettings / UpdateStatus
- Config `settings.updates {check: boolean, weekly: boolean}`; weekly forced false when check is off.
- `UpdateStatus`: `enabled`, `weekly`, `current_version`, `arch`, `state` (`idle|checking|up_to_date|available|downloading|ready|installing|failed`), `latest` (`version`, `tag`, `page_url`, `published_at`, `asset_name`, `size`, ...), `checked_unix_s`, `downloaded`, `signer`, `reason`, `error`.

#### Store state (`store.svelte.ts`, per window)
- `link`, `serviceDown`, `service`, `serviceAction`, `hello`, `info`, `status`, `view` (ConfigView incl. `password_set`, `password_failing`), `saved`, `draft`, `devices`, `backends`, `checks`, `serverIssues`, `activity`, `arrivals`, `identifying`, `picking`/`picked`, `wizard`, `userPictures`, `toasts` (id, kind `info|ok|error`, text, code?), `needsElevation`, `configChangedElsewhere`, `prefs`, `busy`, `pendingPasswords`, `sameAccountSeat`, `pendingInputMove`, `restartRequired` (seats needing restart after save; cleared when the seat goes off/starting or after Restart).
- Derived: `configDirty`, `hasPendingPasswords`, `dirty`, `isAdmin`, `restricted`, `enabled`, `canvas`, `issues`.

#### SaveOutcome
- `saved{restartSeats, rebootNeeded}`, `errors{issues}`, `conflicts{issues}`, `failed`, `nothing`.

#### ChangeSummary (`changes.ts`)
- `count` (device assignments differing + seats differing + top-level settings keys differing + pending passwords), `moved` {key, name (label or name), to: seat | 'pool'} only when exactly one change and it is one device mask change to a single seat or to spare.

#### TrayAction
- `show`, `start_seats`, `toggle_enabled`, `quit`.

#### Tauri commands (`lib.rs`)
- `service_request(body)`, `service_hello`, `service_state`, `service_control(action)`, `service_link`, `app_info`, `open_system_panel(panel)` (panels: device_manager, display_settings, network_connections, user_accounts, sound_settings, screen_saver, lock_screen), `open_help_link(link)` (manual, manual_tr, website, contact, security; all under `https://github.com/PulsarDesk/pulsar-seat`), `restart_elevated`, `set_tray_visible(visible)`, `quit_app`, `list_ipv4_addresses`, `lan_addresses`, `list_user_pictures(category)`, `pick_program`, `pick_program_folder`, `pick_path(kind)`, `wizard_pages_open(pages)`, `wizard_pages_close`, `wizard_page_monitor`, `wizard_last_progress`, `set_ui_dirty(dirty)`, `hide_to_tray`, `ui_language`, `set_ui_language(lang)`.
- Tauri events: `service-event` (EventFrame), `service-link` (bool), `tray-action` ({action}), `hidden-to-tray`, `close-requested`, `wizard-page-close`.

#### Pipe requests used by global code (`api.ts`)
- `get_status`, `get_backends`, `preflight`, `get_config`, `validate_config`, `apply_config{config, base_revision, force}`, `reset_config{confirm}`, `import_reference{json}`, `set_seat_password{seat, password}`, `set_enabled{enabled}`, `resolve_input_move{accept}`, `start_seats{seats}`, `stop_seat`, `relogon_seat`, `request_reboot`, `diagnostic_report{mask_users}`, `get_update_status`, `check_for_update`, `download_update`, `install_update{version, confirm}`, `poll_events{after, wait_ms ≤ 2000, topics}`. Full list of other requests in `api.ts createRequestApi` (owned by other parts).

#### Native shell copy (`copy.rs`, en / tr)
- Window title, wizard window title "Pulsar Seat setup wizard", tray items, file dialog titles and filters (programs, VM images, language packs, folder).

### 4.2 Local seats, devices, monitors, USB

#### Seat (`SeatConfig`, types.ts) - fields used here
- `number`: SeatNumber 1..16 (`MAX_SEATS = 16`). Seat 1 = console seat: cannot be deleted, started or stopped from its menu; always visible.
- `name`: string; default = the number as text; card title rules in `board.ts seatTitle`; edited in SeatNameDialog (max 64, not empty, no control chars).
- `autostart`: boolean, default true (new seat). Used for health "not starting" and blocked summary.
- `kind`: `local` | `remote` (`t.seatKind.local` "Local", `remote` "Remote"); absent = local.
- `account`: `{kind:'logon_screen'}` | user | domain (fact shows `user` or `domain\user`).
- `local.virtual_pads`: boolean, absent = off. Seat menu toggle.
- `local.pad_mode`: `passthrough` (absent) | `shared`. Seat menu toggle (seats 2+).
- `local.frame_source`: `rdp_decode` (absent) | `session_capture` (experimental; restarts seat). Seat menu toggle (seats 2+).
- `local.kiosk_shell`: present = kiosk seat; disables "Delete the seat's kiosk user…".
- `local.gpu_preference`, `ip_binding`, `environment`, `deny_local_logon`, `no_power_menu`, `cpu_sets`: edited in other dialogs reached from the seat menu.

#### Seat runtime (`SeatRuntime.state`, `SeatState`)
Values and labels `t.seatState`: `off` Off, `waiting_for_display` Waiting for a monitor, `starting` Starting (+ `phase`: queued, waiting_for_host, binding_display, launching_host, connecting, waiting_for_session, logging_on), `at_logon` At the sign-in screen, `signed_in` Signed in (+user), `locked` Locked (+user), `stopping` Stopping, `error` Error (+`reason`, `detail`). "Up" = at_logon/signed_in/locked.

#### Seat health (`model.ts seatHealth`, client-side)
`colour` grey/red/yellow/green; `readiness`: `not_starting` (no autostart and not seat 1) "Will not start", `no_display` "No monitor", `no_keyboard` "No keyboard", `no_mouse` "No mouse", `ready` "All ready" (`t.health`). Blocked = no_display or no_keyboard.

#### Product state (status line)
`disabled` (off), `safe_mode`, other = on. Labels `t.power.*`.

#### Device (`Device`)
- `key` (DeviceKey string), `kind` (DeviceKind), `name`, `label` (location ID, e.g. `USB-2-1-1`, `1.2`), `instance_id`, `container_id`, `parent_hub`, `present`, `hardware_id`, `serial`, `warnings[]`, `details`.
- `DeviceKind` with `t.kinds`: monitor Monitor, keyboard Keyboard, mouse Mouse, hid Other input device, audio_render Speaker, audio_capture Microphone, webcam Camera, smart_card_reader Smart card reader, usb_hub USB hub, storage Storage, printer Printer, serial_port Serial port, network Network adapter, other Other device. Board order follows this list (`KIND_ORDER`, network last after other), then arrival time.
- Share policy (`policyOf`): exclusive (monitor, keyboard, mouse, hid; default mask seat 1), never_gated (network; always shared, cannot move), shareable (all other; default mask SHARED).
- `warnings` codes: `key_collision`, `foreign_filter{name}`, `hidden_by_other_software`, `not_routable`, `moved_from{old_key}` (offers "Move the assignment to this port"). Any warning shows "Needs attention". Specific warning texts: unclear in this part (not rendered by these files).
- `details`: display (adapter_name, adapter_luid, target_id, output_technology, monitor_name, edid, mode w/h/refresh_mhz/rotation, gdi_name, desktop_rect, in_console_desktop, presentation none/native/presenter/fallback), input (class, usage_page, usage, vendor_id, product_id, interface_path, bus, built_in, last_activity_ms), audio (flow, endpoint_id, form_factor, state), usb (vendor_id, product_id, port_count, hub_depth, port), storage (removable, optical, volumes, letters, file_system), none.

#### Device view (`DeviceView`)
`device`, `effective` mask, `source` (MaskSource: explicit / inherited{hub key, port key, or container key} / default), `isolation` (descriptor, session_letter, minifilter, camera_consent, shared, not_supported, network, seat_filter; texts `t.describe.isolation`), `arrived_ms`, `port_key` (`port;loc=…`), `container_key` (`cont;id=…`), `areas`.

#### Device assignment (`DeviceAssignment`, config `devices[]`)
`key`, `kind`, `mask`, `name`, `label`, `hardware_id`, `serial`, `container_id`, `options`. Absent record = inherited or default. Port records use kind `usb_hub`, container records kind `other`.
Inheritance (`model.ts effectiveMasks`, mirror of service): own record; else container record; else nearest port / hub up the chain (spare records skipped; an exclusive kind only inherits a single-seat mask, else falls back to its default).

#### Seat mask (`SeatMask`, u32)
0 = spare (`SPARE_MASK`, label "Spare"), `0xffffffff` = shared by all (`SHARED_MASK`, "All seats"), else bit (n-1) per seat ("Seat 2", "Seats 2, 3"). Removing a seat from SHARED expands to all seats first.

#### Assignment options (`AssignmentOptions`)
- `software_cursor` boolean (monitor), default false.
- `picture` string `builtin:<id>` | `user:<file>` | absent.
- `outputs` number[] (CCD target ids); empty/absent = automatic; >1 = clone (needs `allow_clone`).
- `hdr`, `vrr` boolean (monitors of seats 2+).
- `areas` SeatArea[] (`{seat, area}`), area values `full, left, right, top, bottom, top_left, top_right, bottom_left, bottom_right` (`t.outputs.split.areaName`).

#### Output settings (`settings.outputs`)
`force_assign_at_start` bool (false), `allow_clone` bool (false), `follow_session_mode` true/false/absent(auto), `session_frame_rate {mode: auto|smooth|fixed|off, fps}` absent = auto/60; service limits fps 30..500 (`MIN_SESSION_FPS`/`MAX_SESSION_FPS`), UI offers fixed values only.

#### View prefs (`prefs.ts`, per user, local storage)
`flashSeconds` 4 (2..20 even), `showAllHubs` true, `sharedInSeats` false, `showIds` false, `visibleSeats` all bits, `askOnManualStart` true, `hideSameAccountWarning` false.

#### USB dock (`compat.ts UsbDock`)
`key` (hub container key, else hub key), `hub`, `label`, `monitors`, `inputs` (keyboard/mouse/hid), `audio` (render/capture), `others`, `seat` (seat owning all its monitors, or null). Wizard choice per dock: seat N | new | skip.

#### Seat limits
Max 16 seats total (local + remote). New seat number = lowest free.

### 4.3 Settings, backends, compat, console, power

#### GlobalSettings (`settings.*`) fields edited in this part

| Field | Type | Allowed values | Default | Where |
|---|---|---|---|---|
| `start_mode` | enum `StartMode` | `manual` "By hand", `at_boot` "When Windows starts", `after_seat_one_logon` "When someone signs in on seat 1" (`t.settingsPage.startModeLabels`; long forms `t.startModes`, hints `t.startModeHints`) | `manual` | Settings > Start |
| `same_account` | enum `SameAccountPolicy` | `allow` / `warn` / `deny` | `warn` | Settings > Behaviour |
| `admin_only` | bool | | false | Settings > Behaviour |
| `users_may_control_own_seat` | bool | | true | Settings > Behaviour |
| `enabled` | bool | | — | not in this part (power on/off) |
| `timings.start_delay_ms` | u32 ms | 0..600000 | 0 | Advanced > Timings |
| `timings.between_seats_ms` | u32 ms | 0..60000 | 2000 | Advanced > Timings |
| `timings.seat_start_timeout_s` | u32 s | 30..120 | 60 | Advanced > Timings |
| `timings.relogon_delay_ms` | u32 ms | 0..60000 | 3000 | Advanced > Timings |
| `timings.audio_wait_timeout_s` | u32 s | input 0..600 | 60 | Advanced > Timings |
| `timings.monitor_idle_off_s` | u32 s | 0 or 60..86400 | 0 | Advanced > Timings |
| `safety.unclean_shutdown_guard` | bool | | true | Advanced > Safety |
| `safety.crash_loop_limit` | u32 | ≥1 (input 1..20) | 3 | Advanced > Safety |
| `input.switch_hotkey_scan` | u16 or null | set-1 make code; not 0, ≤0x7F, not 0x1D | 0x58 (Ctrl+F12) | Hotkey dialog |
| `input.allow_system_buttons` | bool | | false | Advanced > Safety |
| `input.pad_device_scope` | bool | | false | Advanced > Safety |
| `input.unfiltered_hid_usages`, `gap_buffer_ms`, `gap_buffer_packets` | | | `[(0x59,0)]`, 1000, 256 | not edited in the UI |
| `devices.admins_bypass` | bool | | true | Advanced > Devices |
| `devices.class_default_descriptors` | bool | | false | Advanced > Devices |
| `devices.class_default_classes` | `DeviceClassTarget[]` | `cdrom`, `ports`, `modem`, `multiport_serial`, `floppy`, `tape_drive`, `medium_changer` offered (`disk_drive`, `volume` exist in the type, refused by the service, never offered) | empty = cdrom + ports | Advanced > Devices |
| `tweaks.lock_screen_image` | bool | | false | Settings > Appearance |
| `tweaks.hide_fast_user_switching` | bool | | false | Advanced > Optional system settings |
| `tweaks.exclude_driver_updates` | bool | | false | same |
| `tweaks.touchpad_palm_check` | u32 or null | null (Windows default), 0..4 offered | null | Advanced > Devices |
| `tweaks.fast_startup_off` | bool (omitted when false) | | false | Optional system settings |
| `tweaks.windows_update` | `WindowsUpdatePolicy` | see below | off | same |
| `tweaks.device_install_deny` | `{classes?: DeviceInstallClass[], admins_override: bool}` | classes `printer`, `image`, `camera`, `wpd`, `modem`, `cdrom`, `ports`, `smart_card_reader` | `{classes: [], admins_override: true}` | same |
| `tweaks.defender_exclusions` | bool | | false | same |
| `tweaks.inactivity_lock_off` | bool | | false | same |
| `tweaks.users_shutdown_right_off` | bool | | false | same |
| `console_guard.host_lock_off` | bool | | true | Optional system settings > Console lock |
| `console_guard.seat_one_lock_off` | bool | | false | same |
| `log.{info, debug, trace}` | 3 bools | presented as one choice errors/info/debug/trace/custom | info only | Advanced > Safety |
| `ip_programs[]` | `{title, path, enabled}` | ≤256; path non-empty, no control chars | [] | Advanced > Network addresses |
| `kiosk.seat1_desktop` | bool | | true | Settings > Kiosk seats |
| `kiosk.accounts` | `per_seat` / `shared` | | `per_seat` | Settings > Kiosk seats |
| `apps.steam_per_seat` | bool | | false | Advanced > Apps on seats |
| `apps.object_rules[]` | `{exe, names[]}` | ≤32 rules; see CompatSettings | [] | same |
| `outputs.session_frame_rate` | `{mode: auto|smooth|fixed|off, fps}` | fixed fps 30..500 | `{auto, 60}` (not written) | Display outputs dialog (other part); issue `frame_rate_out_of_range` |

`WindowsUpdatePolicy`: `enabled` bool; `feature_defer_days` 0..365; `quality_defer_days` 0..30; `active_hours_start` / `active_hours_end` 0..23 or null (both null or both set, different, window ≤18 h); `no_auto_restart_with_users` bool. Default all off/0/null.

#### BackendSelection (`settings.backends`)

| Field | Type | Values (en.ts `t.backendNames`) | Default |
|---|---|---|---|
| `session` | `SessionBackendKind` | `rds_loopback` "Concurrent full sessions (Windows Server x64 only, Remote Desktop Session Host)"; `termsrv_wrapper` "Multiple sessions on client Windows (experimental)"; `fast_user_switch` "Real sessions, one at a time (fast user switching, one seat at a time)"; `desktops` "Concurrent kiosk seats (separate desktops in one session)" | `rds_loopback` |
| `host_topology` | `HostTopology` | `seat_one_console` "In seat 1’s own session (default)"; `dedicated_host` "In a dedicated host session (full input isolation)" | `seat_one_console` |
| `display` | `DisplayBackendKind` | `window_presenter` "Full-screen window (works everywhere)"; `display_core` "Dedicated display output"; `pulsar_idd` "Pulsar virtual display driver"; `console_only` "Console only" | `window_presenter` |
| `input` | `InputBackendKind` | `raw_input_host` "Raw input, user mode"; `winusb_capture` "Raw input, plus USB keyboards and mice taken from seat 1 (no driver of ours)"; `kmdf_filter` "Pulsar input filter driver"; `none` "Off" | `raw_input_host` |
| `audio` | `AudioBackendKind` | `rdp_bridge` "Route to the seat’s own sound device"; `console_shared` "All sound from seat 1’s devices"; `none` "Off" | `rdp_bridge` |
| `gates` | `GateBackendKind[]` | `device_security` "Device security definitions"; `session_drive_letters` "Per-session drive letters"; `camera_consent` "Camera permissions"; `storage_minifilter` "Storage filter driver"; `printer_acl` "Printer queue permissions" | `[device_security, session_drive_letters]` |
| `experimental_ack` | number | must equal 2 (`EXPERIMENTAL_WARNING_VERSION`) when session = `termsrv_wrapper` | 0 |
| `loopback_listener` | `{enabled, port, extra_accounts?}` | port 1024..65535 ≠ 3389; accounts `user` / `DOMAIN\user` | `{true, 33389, []}` |
| `host_autologon` | bool | effective only with `dedicated_host` | false |

#### Backend availability (`get_backends` → `BackendReport`)

`BackendReport` = lists `session`, `display`, `input`, `audio`, `gates` of `BackendInfo {kind, availability, experimental, selected, active, caps, notes[]}`, plus `drivers[] {driver, status}` and `source_ip?`.

`Availability` states (label `t.availability.*`, composed by `availabilityLabel`):
- `available` "Available"
- `needs_setup {what}` "Setup needed: <t.needsSetup[what]>": `specialized_display` "the monitor must be set up as a dedicated output", `enable_remote_desktop` "Remote Desktop must be turned on", `termservice_start` "the Remote Desktop service must be started", `enable_termsrv_wrapper` "the wrapper you installed must be enabled", `console_sign_in` "a user must sign in on seat 1".
- `driver_not_signed {driver}` "The driver is not signed yet and does not load (<driver name>)"
- `driver_version_mismatch {driver, found}` "Driver version does not match; update Pulsar Seat (<driver name>)"
- `unavailable {reason}` "Not available: <t.unavailableReasons[reason]>": `edition_unsupported`, `arch_unsupported`, `rds_not_ready`, `no_wrapper`, `not_native`, `not_implemented`, `not_built`, `feature_not_built`, `os_unsupported`, `not_client_sku`, `rdsh_role_missing`, `rdp_denied_by_policy`, `foreign_service_dll`, `external_wrapper_inactive`, `concurrent_sessions_off`, `unknown_termsrv_build`, `wrapper_missing`, `reference_product_present`, `audio_service_not_running`, `display_core_unavailable`, `experimental_not_acknowledged`, `fsu_disabled`, `console_not_ready` (texts in en.ts `unavailableReasons`).

Driver ids (`t.drivers`): `kbd_mou_filter` "Keyboard/mouse filter", `hid_filter` "HID filter", `idd` "Virtual display driver", `storage_minifilter` "Storage filter", `source_ip_callout` "Source IP driver". Driver status (`t.driverStatus`): `not_installed`, `installed_not_loaded`, `loaded {interface_major, interface_minor, build}`, `version_mismatch {found}`. The driver status list is not shown on the pages of this part (only inside availability labels); `t.fallback` (`instead(wanted, used)`, `notUsable`, `forcedByFus`, `unknownReason`, `gate`) and `t.advanced.active` / `selected` are not used in this part's files (other screens).

Backend notes (`noteLabel`, `t.notes.*`) appear in session option help and in the other methods' select help, e.g. `input_not_isolated`, `display_fallback`, `audio_console_shared`, `one_seat_at_a_time`, `fus_inactive_frozen`, `kiosk_audio_shared`, `kiosk_audio_per_app`, `no_user_isolation`, `hotkey_needs_filter`, `gamepads_shared`, `usb_input_captured`, `rdp_listener_on_all_interfaces`, `two_admin_sessions_only`, `rds_licensing_not_configured`, loopback listener notes. Full list in en.ts `notes`.

`SourceIpReport`: `enforcement` `driver` | `environment`; `reason` (`driver_not_loaded`, `driver_busy`, `driver_version_mismatch`, `driver_pass_through`, `map_invalid`, `map_refused`); `enforced[]`, `shared_session[]`, `dropped_paths[]`.

#### SeatConfig fields edited in this part

| Field | Type | Allowed values | Default | Where |
|---|---|---|---|---|
| `number` | 1..16 | unique | next free | Add seat |
| `name` | string | 1..64 chars trimmed, no control chars | number as text | Seat name dialog |
| `autostart` | bool | seat 1 always true | true | Settings seat row |
| `kind` | `local` / `remote` | absent = local | local | not edited here |
| `account` | `{kind:'logon_screen'}` / `{kind:'local', user}` / `{kind:'domain', domain, user}` | user/domain non-empty, ≤256, no `\ / " @` | logon_screen | Account dialog |
| password (not in config) | write-only | any (empty allowed) | — | Account dialog → `set_seat_password`; view flags `password_set[]`, `password_failing[]` |
| `cpu_sets` | number[] | CPU set ids; empty = all | [] | CPU dialog |
| `cpu_hard_limit` | bool | sets in one processor group | false | CPU dialog |
| `cpu_seat_wide` | bool (only when true and sets chosen) | | false | CPU dialog |
| `wallpaper` | bool | seats 2+ | false | Seat menu. Meaning beyond "seat wallpaper" (`t.basic.colWallpaper`): unclear in code |
| `ip_binding` | `{address, rule}` | IPv4 static; rule `all_programs` / `only_listed` / `all_except_listed` | absent | IP dialog |
| `startup_apps` | `{command, args[]}[]` | absolute path | absent | Startup programs dialog |
| `pad_ignore_devices` | string[] | `0xVVVV/0xPPPP`, ≤64 | absent | Pad ignore dialog |
| `environment` | `{name, value}[]` | ≤32; rules in SeatCompatDialog | absent | Environment and sign-in |
| `deny_local_logon` | bool | RDP-signed-in seats with an account | absent | Environment and sign-in |
| `no_power_menu` | bool | needs the seat's account | absent | Environment and sign-in |
| `local.kiosk_shell` | `{apps: KioskShellApp[], idle_reset_minutes}` | ≤16 apps; idle 0..1440 | absent | Kiosk dialog |
| `local.redirect` | `SeatRedirect` | see below | absent = all off | Redirection dialog |
| `local.virtual_pads` | bool | | absent | Seat menu toggle |
| `local.pad_mode` | `passthrough` / `shared` | | absent = passthrough | seats board menu (`t.settingsPage.padsShare` / `padsPassthrough`, seats 2+); fact `padsSharedFact` shown on Settings |
| `local.gpu_preference` | `{mode, apps?}` | mode `off` / `seat_gpu` / `high_performance` / `power_saving`; ≤64 .exe paths | absent | GPU dialog |
| `local.frame_source` | `rdp_decode` / `session_capture` | | absent = rdp_decode | seats board menu (`t.settingsPage.captureOn` "Capture the picture inside the session (experimental, restarts the seat)" / `captureOff` "Show the picture over Remote Desktop (restarts the seat)", local seats 2+); fact `captureFact`. Changing it puts a running seat in `restart_seats` |
| `local.kiosk` | `{command, args}` | | — | legacy field; not edited in this part (unclear) |

`KioskShellApp`: `title` (1..64 after service default), `command` (absolute, ≤260; may be empty with `app_id`), `args?`, `autostart`, `monitor` (0-based, 0..15), `app_id?` (AUMID ≤160), `fullscreen?`.

`SeatRedirect` default: `clipboard {direction: off, text: true, html: true, images: true, files: false, cross_seat: false}`, `drives: off`, `smartcards: false`, `printers: off`. Enums: `ClipboardDirection` `off` / `to_seat` / `from_seat` / `both` (labels `t.redirect.directions`, short forms `t.redirect.short`); `DriveRedirect` `off` / `read_only` / `read_write`; `PrinterRedirect` `off` / `seat` / `all`.

`GpuPreferenceMode` labels `t.gpuPref.modes`.

#### Per-user manager preferences (not in the config)

`prefs.ts` localStorage key `pulsar-seat.manager.prefs.v1`: `askOnManualStart` (default true), `noShowGoodApply` (false), `hideTray` (false), `theme` `system` / `light` / `dark` (system). Language: separate localStorage key; `system`, `en`, `tr`.

#### Device options touched here

`devices[].options.picture`: `builtin:<id>` or `user:<file name>`; absent = default picture.

#### Other data used by dialogs

- `LocalUser {name, full_name, sid, disabled, is_admin}` (`list_local_users`).
- `CpuSetInfo {id, group, logical_index, core_index, efficiency_class, numa_node}` (`list_cpu_sets`).
- `LocalAddress {address, adapter, static_address}` (Tauri `list_ipv4_addresses`).
- `PickedProgram {title, path}` (Tauri `pick_program`, `pick_program_folder`).
- `HyperVStatus {feature: enabled|disabled|enable_pending|unavailable, switch_present, seat_nics[{seat, addresses[]}], reboot_required?}`; `HyperVAction` `status` / `enable_feature{confirm}` / `set_seat_nic{seat, address, prefix_len}` / `remove_seat_nic{seat}`.
- `SteamProxyStatus {steam_dirs[], found[], quarantined[{original, quarantined}]}`.
- `HostConsoleReport {state: off|on|refused|failed|drifted|pending_removal, reason?}` in `ServiceStatus.host_console`.
- `Panel` (Tauri `open_system_panel`): `device_manager`, `display_settings`, `network_connections`, `user_accounts`, `sound_settings`, `screen_saver`, `lock_screen`. `HelpLink` (Tauri `open_help_link`): `manual`, `manual_tr`, `website`, `contact`, `security` (fixed repository URLs).

#### Issue codes relevant to this part (en.ts `issueText`)

Errors: `too_many_seats`, `invalid_seat_name`, `invalid_account`, `timeout_out_of_range`, `loopback_port_invalid`, `hide_fsu_with_fast_user_switch`, `invalid_hotkey`, `frame_rate_out_of_range`, `experimental_not_acknowledged`, `same_account_on_seats`, `same_account_fast_user_switch`, `invalid_pad_ignore_device`, `invalid_seat_environment`, `deny_local_logon_unsupported`, `invalid_windows_update_policy`, `invalid_object_rule`, `invalid_ip_address`, `loopback_ip_address`, `invalid_ip_program`, `invalid_kiosk_shell`, `kiosk_app_execution_alias` (service), `invalid_gpu_preference`, `cpu_no_core` (service).
Warnings: `loopback_account_invalid`, `loopback_no_accounts`, `logon_screen_needs_listener`, `link_local_ip_address`, `duplicate_ip_address`, `ip_binding_shared_session`, `too_many_ip_programs`, `kiosk_shell_ignored_on_seat_one`, `cpu_unknown_id`, `cpu_only_efficiency_cores` (severity of the last two: unclear in this code; texts suggest warning).

### 4.4 Wizard, diagnostics, issues

#### WizardVariant (`types.ts`)
`'all_seats' | 'keep_seat_one' | 'quick_two_seats' | 'host_console'`; labels in `t.wizard.variantOptions` (see screens). UI default `keep_seat_one`. `host_console` result also sets `settings.backends.host_topology = 'dedicated_host'` and `host_autologon = true`.

#### WizardCommand (`types.ts`, request `wizard_command{command}`)
- `pick_console_monitor{key: DeviceKey}`
- `extend_desktop`
- `confirm_seat{seat}`
- `reset_seat{seat}`
- `reset_all`
- `set_main_devices{keys: DeviceKey[]}`
- `target_hit{seat}`
Other requests: `wizard_start{variant}`, `wizard_stop{save: boolean}` (all Admin).

#### WizardProgress (`types.ts`, event `wizard_progress`, topic `wizard`)
- `step: string`: `pick_console_monitor`, `keyboard`, `mouse`, `waiting`, `done`, `cancelled`, `invalid` (labels `t.wizard.stepNames`: "Seat 1’s monitor", "Keyboard", "Mouse", "Waiting", "Done", "Cancelled", "Not saved").
- `seat: SeatNumber | null`: current seat.
- `key_presses: number` (0..2), `clicks: number` (0..2).
- `assigned: [SeatNumber, DeviceKey][]`.
- `monitors?: DeviceKey[]`: monitor per seat (index 0 = seat 1).
- `confirmed?: SeatNumber[]`.
- `keys_seen?: boolean`: service handles control keys itself; manager windows ignore keys.
- `last_unit?: DeviceKey`.
- `variant?: WizardVariant`.
- `issues?: string[]`: issue codes for step `invalid`.

#### Wizard constants (`wizard.ts`)
`MIN_WIDTH` 1024, `MIN_HEIGHT` 600, `FKEY_SEATS` 12 (F1..F12; seats 13+ "any key"), `DIGIT_MONITORS` 10 (digit 0 = monitor 10). `MAX_PAGES` = `MAX_SEATS` = 16 (`wizard_pages.rs`, `types.ts`).

#### WizardPageSpec (`api.ts`, `wizard_pages.rs` `PageSpec`)
`monitor: DeviceKey` (1..512 chars), `x, y: i32` (|v| ≤ 65536), `width, height: u32` (64..16384), physical pixels of the monitor's desktop rectangle.

#### PageView (`wizard.ts`)
`pick{number}`, `keyboard{seat,presses}`, `mouse{seat,clicks}`, `other{seat,current}`, `kept{seat}`, `done{seat}`, `no_seat`.

#### ConfigDiff (`types.ts`, `compare_saved` → `diff`)
`matches: boolean`, `added: DeviceKey[]`, `changed: MaskChange[]` (`key`, `saved: SeatMask`, `live: SeatMask`), `missing: DeviceKey[]`. Moved devices come separately as device warning `{code:'moved_from', old_key}`.

#### Experimental acknowledgement
`settings.backends.experimental_ack: number` (`types.ts`), must equal `EXPERIMENTAL_WARNING_VERSION` = 2 when `settings.backends.session = 'termsrv_wrapper'` (else issue `experimental_not_acknowledged`). Remote: `set_termwrap{enabled, wrapper_path, ack_warning_version}`; `TermwrapStatus` has `state` (`off`/`on`/`foreign_active`/`external`/`blocked`; `external` carries `wrapper`), `termsrv_version`, `supported`, `candidates[]{service_dll, config_file, dll_version, signed}`, `warning_version`.

#### Diagnostic report
Request `diagnostic_report{mask_users: boolean}` → `report{path}`. UI default `mask_users = true`.

#### Import
Request `import_reference{json: string}` → `config`. Draft replaced with result, revision kept from saved config.

#### Issue (`types.ts`)
`severity: 'error' | 'conflict' | 'warning'`, `code: string`, `seat?`, `device?`, `field?`, `reason?`. Codes: see catalogue.

#### Check (`types.ts`, `preflight` → `checks`)
`id: string`, `outcome: 'pass' | 'warn' | 'fail' | 'info'`, `detail: string` (detail not rendered in the check row; unclear in code whether shown elsewhere).

#### ProductState / SafeModeReason
`disabled` | `enabled{started}` | `safe_mode{reason: 'windows_safe_mode' | 'unclean_shutdown' | 'crash_loop'}`.

#### FailureReason (`types.ts`)
`edition_unsupported, rds_not_ready, seat_limit, account, logon_failed, client_auth, host_exited, session_timeout, display, driver, coexistence, same_account_denied, policy, internal, console_in_use, console_busy, session_not_locked, nla_required, kiosk_app_missing, console_not_ready, not_our_session, fsu_disabled, loopback_listener_unavailable` (`detail` carries `listener=loopback_listener:<state>`).

#### ProtoError (`types.ts`)
`code`, `message`, `field?`, `reason?`, `win32?`. Codes: `seat_core::error::ErrorCode` (see catalogue).

#### Notice event
`{code: string, seat: SeatNumber | null, detail: string}`.

#### Native copy (`copy.rs`)
EN/TR pairs: `WINDOW_TITLE` "Pulsar Seat Manager", `WIZARD_TITLE` "Pulsar Seat setup wizard", tray `TRAY_SHOW` "Show the window", `TRAY_START` "Start seats", `TRAY_TOGGLE` "Turn Pulsar Seat on / off", `TRAY_QUIT` "Quit", file dialogs `PICK_FOLDER`, `PICK_PROGRAM`, `FILTER_PROGRAMS`, `FILTER_ALL`, `PICK_VM_IMAGE`, `FILTER_VM_IMAGES`, `PICK_LANGUAGE_CAB`, `FILTER_CAB`, `PICK_HOST_FOLDER`. Language follows the Windows UI language until the UI sends `set_ui_language`.

### 4.5 Code catalogue (issues, checks, notes, refusals, product/safe-mode states, failures, errors, notices)

Sources: `en.ts` maps `issueText`, `checkText`/`checks`/`outcomes`, `notes`, `refusalReasons`, `productText`, `safeModeText`, `failureText`, `errorText`, `noticeText`, `loopbackStateText`, `remoteErrorText`; severities from `crates/seat-core/src/validate.rs`, `model.ts` (manager-local validation) and `seat-service/requests.rs`. Unknown codes are shown raw (fallback = the code string).

Text helpers: `issueLabel` prefixes "seat N" and/or device name ("seat 2, Keyboard: …"); `checkText(id,outcome)` falls back to the outcome word; `noteLabel` falls back to loopback note parsing then raw code; `refusalLabel` = "<channel>: <reason>" with "(+N)" when suppressed; `errorLabel` prefers `failureText[reason]`, then `failureText[code]`, then `errorText[code]`, else "Error: <code>"; error toasts become danger notices with "(code · Windows <win32>)" tag (`errorCodeTag`).

#### Validation issues (`Issue.code`, `issueText`)

Severity: E = error (blocks save, `apply_config` refused), C = conflict (save needs "Save anyway", `force`), W = warning. Where shown (all issues): save with errors → toast `t.saved.errors` "Not saved: fix the problems below first."; conflicts → `ConflictDialog` list (`issueLabel`); non-warning issues with a device mark that device card "attention" on the seats board; wizard step `invalid` lists error codes (Wizard + WizardPage). No other renderer of `issueText`/`issueLabel` was found. "L" = also checked in the manager (`model.ts`).

| Code | English text (short) | Severity | Where shown / notes |
|---|---|---|---|
| `too_many_seats` | There can be at most 16 seats. | E, L | field `seats` |
| `duplicate_seat` | The same seat number appears twice. | E, L | seat |
| `invalid_seat_name` | Invalid seat name (1–64 characters). | E, L | seat, field `name` |
| `invalid_account` | Invalid sign-in account. | E, L | seat, field `account` |
| `seat_one_no_display` | Seat 1 must have a monitor; cannot save. | E, L | seat 1 |
| `exclusive_on_several_seats` | This device can be on only one seat. | E, L | device |
| `mask_beyond_seats` | The device is assigned to a seat that does not exist. | E, L | device |
| `duplicate_device` | The same device is assigned twice. | E, L | device |
| `timeout_out_of_range` | A timing value is outside the allowed range. | E, L | `settings.timings.*` |
| `loopback_port_invalid` | Port for seats without a password must be 1024–65535, not 3389. | E, L | `settings.backends.loopback_listener.port` |
| `hide_fsu_with_fast_user_switch` | Hiding fast user switching turns off the “one seat at a time” method. | E, L | `settings.tweaks.hide_fast_user_switching` |
| `loopback_no_accounts` | Seat opens at sign-in screen but no account can use the passwordless port… | W, L | seat |
| `logon_screen_needs_listener` | Seat opens at sign-in screen; cannot start while the passwordless port is off… | W, L | seat |
| `loopback_account_invalid` | One of the extra accounts is invalid (user or DOMAIN\user, no @); ignored. | W, manager only | `…loopback_listener.extra_accounts` |
| `invalid_hotkey` | The switch shortcut is invalid. | E, L | `settings.input.switch_hotkey_scan` |
| `frame_rate_out_of_range` | Session frame rate must be 30–500 fps. | E, L | `settings.outputs.session_frame_rate.fps` |
| `experimental_not_acknowledged` | Warning for the experimental session method not acknowledged. | E, L | `settings.backends.session` |
| `seat_without_display` | The seat has no monitor; it waits until one is plugged in. | W, L | seat |
| `seat_without_keyboard` | The seat has no keyboard. | W, L | seat |
| `seat_without_mouse` | The seat has no mouse. | W, L | seat |
| `child_of_other_seat_hub` | Device plugged into a USB hub given to another seat. | C | device; ConflictDialog |
| `same_account_on_seats` | Same account on several seats and that is not allowed. | E | seat (policy = deny) |
| `key_collision` | Two devices cannot be told apart; use different ports. | W | device |
| `monitor_hub_mismatch` | Monitor's own USB hub is on a seat the monitor is not on. | W | seat + device |
| `cpu_unknown_id` | Some chosen cores do not exist on this PC; ignored. | W (service) | seat |
| `cpu_no_core` | The seat is left with no core; cannot save. | E (service) | seat |
| `cpu_only_efficiency_cores` | The seat got only efficiency cores; may run slowly. | W (service) | seat |
| `invalid_ip_address` | Invalid IP address. | E, L | seat, `ip_binding` |
| `invalid_pad_ignore_device` | Pads-to-ignore entry not 0xVVVV/0xPPPP (or list too long). | E, L | seat |
| `invalid_seat_environment` | A seat environment variable is invalid (name, value, duplicate, > 32). | E | seat |
| `deny_local_logon_unsupported` | “Deny log on locally” only for seats with an account signing in via RDP. | E | seat |
| `invalid_windows_update_policy` | Windows Update settings out of range. | E | `settings.tweaks.windows_update` |
| `invalid_object_rule` | A single-instance program rule is incomplete or invalid. | E | `settings.apps.object_rules` |
| `duplicate_ip_address` | Same IP address given to several seats. | W, L | seat |
| `too_many_ip_programs` | Program list too long; the rest is not saved. | W, L | `settings.ip_programs` |
| `invalid_ip_program` | Program list has a row with an empty path. | E, L | `settings.ip_programs` |
| `loopback_ip_address` | 127.x addresses only work inside this PC. | E, L | seat |
| `link_local_ip_address` | 169.254.x addresses are assigned by Windows and can change. | W, L | seat |
| `ip_binding_shared_session` | Kiosk seat shares its session; IP only used by programs that support it. | W, L | seat |
| `clone_not_allowed` | Several outputs for a monitor but cloning not allowed. | E, L | device, `options.outputs` |
| `invalid_screen_area` | Split screen invalid (one part per seat, ≤ 4 seats, no cloning). | E | device, `options.areas` |
| `screen_area_overlap` | Two seats’ parts of this split monitor overlap. | E | device |
| `console_share_not_half` | Seat 1 can share a monitor only half and half with one other seat. | E | device |
| `same_account_fast_user_switch` | With fast user switching each seat needs its own account. | E | seat |
| `invalid_gpu_preference` | GPU preference programs invalid (≤ 64, full .exe paths). | E, L | seat, `local.gpu_preference` |
| `invalid_kiosk_shell` | Kiosk settings invalid (≤ 16 apps, name+path, idle 0–1440 min). | E, L | seat, `local.kiosk_shell` |
| `kiosk_shell_ignored_on_seat_one` | Seat 1 stays a normal desktop; kiosk setting not used there. | W, L | seat 1 |
| `kiosk_app_execution_alias` | Kiosk app path is only a Store-app alias; fill in the AUMID instead. | E (service) | seat; `reason` = `ifeo_alias_redirect` / `appexeclink` |
| `seat_kind_mismatch` | Seat kind (local/remote) does not match its settings. | E | seat |
| `console_seat_remote` | Seat 1 is always a local seat. | E | seat 1 |
| `device_on_remote_seat` | Devices cannot be assigned to a remote seat. | E | device |
| `remote_seat_invalid` | The remote seat’s settings have errors. | E | seat; `reason` = remote error code |
| `session_unavailable` | Remote seats cannot start with the selected session method. | W | seat |
| `store_failed` | The settings could not be written to the file. | wizard pseudo-issue | only in wizard `invalid` step (write error) |

#### Preflight checks (`Check.id`, `t.checks`, `t.checkText`) and outcomes

Where shown: Status page, one row per check: label `t.checks[id]`, help `checkText(id,outcome)`, status word `t.outcomes[outcome]`. Outcome → status kind: pass ok, info ok, warn warn, fail danger (`StatusTab.svelte`).

Outcomes (`CheckOutcome`): `pass` "OK", `warn` "Warning", `fail` "Problem", `info` "Info".

| Check id | Label | pass | warn | fail | info |
|---|---|---|---|---|---|
| `admin` | Administrator rights | Running as administrator. | — | Administrator rights are needed. | — |
| `os_build` | Windows version | The Windows version is supported. | Newer than the newest tested version. | Too old. | — |
| `arch_native` | Native build | Package built for the PC’s processor. | Architecture could not be determined. | Package for another architecture; install the right one. | — |
| `edition` | Windows edition | — | Client edition: extra seats need a multi-session method (Advanced). | — | Windows edition reported. |
| `rdsh_role` | Remote Desktop Session Host role | Role installed. | Role not found; seats fall back to fast user switching. | — | Role state unknown. |
| `secure_boot` | Secure Boot | On. | — | — | Off or unknown (reported only; never changed). |
| `memory_integrity` | Memory Integrity | On. | — | — | Off or unknown (reported only; never changed). |
| `smart_app_control` | Smart App Control | — | — | — | State reported; when on, unsigned builds may be blocked. |
| `windows_safe_mode` | Windows Safe Mode | Normal boot. | Windows is in Safe Mode: extra seats do not start. | — | — |
| `hyperv_vm` | Hyper-V for VM seats | — | — | — | Needed only for VM seats; never turned on by itself (Advanced > Network addresses; restart follows). |
| `driver_signing` | Driver signature | — | — | — | Drivers not signed yet; user-mode methods are used. |
| `reference_product` | Another multiseat product | Not installed. | Installed but turned off; keep it off, else RDP seats and updates are refused. | Installed; uninstall or disable its service + driver and restart. | — |
| `termservice_stock` | Remote Desktop service is stock | Uses Windows’ own file. | Does not use the Windows default. | — | — |
| `foreign_input_filters` | Other input filter drivers | None. | Other input filters present (left untouched). | — | — |
| `steam_proxy_dll` | Old Steam add-on DLL | — | Old version.dll next to steam.exe; not removed; see Advanced > Compatibility. | — | — |
| `host_sleep_off` | Host console stays awake | Power plan keeps PC and displays on. | PC may sleep / displays off; turn on “Keep the PC awake”. | — | Power plan could not be read. |
| `console_lock` | Console lock | Console cannot lock while seats are in use. | Host account can lock the console; every local seat shows the lock screen. | — | Locking seat 1 hides every local seat until it unlocks. |

#### Notes (`t.notes`, backend / seat / redirection notes)

Where shown: Status page (seat runtime `degraded`, `audio_notes`, `redirect_notes` joined with " · "; backend note rows; `fallback_notes` plain codes), Advanced page backend option help (`b.notes`). Kind: informational/degradation notes (no severity field).

| Code | English text (short) |
|---|---|
| `input_not_isolated` | Input is not fully isolated |
| `seat_one_sees_other_endpoints` | Seat 1 also sees the other seats’ sound devices |
| `display_fallback` | Picture shown in a full-screen window (not hidden from seat 1) |
| `audio_console_shared` | Sound comes from seat 1’s devices |
| `audio_no_endpoint` | No sound device given to this seat; silent |
| `audio_not_bound` | Seat’s sound could not be set up (see log) |
| `audio_service_not_running` | Windows audio service not running; silent |
| `render_absent` | Seat’s speaker/headphones missing; silent |
| `render_open_failed` | Seat’s speaker/headphones could not be opened |
| `render_lost` | Seat’s speaker/headphones were unplugged |
| `capture_absent` | The seat has no microphone |
| `capture_open_failed` | Seat’s microphone could not be opened |
| `capture_lost` | Seat’s microphone was unplugged |
| `drive_served_as_seat_user` | NTFS/ReFS drives served as the seat’s user; drive permissions apply |
| `same_account_warning` | This account is also signed in on another seat |
| `one_seat_at_a_time` | Only one seat is active at a time |
| `fus_inactive_frozen` | Inactive seats are frozen (no picture/input/sound); GPU apps may close |
| `fus_audio_per_user` | Each seat’s user picks default sound device once (per user) |
| `kiosk_audio_shared` | Kiosk apps’ sound comes from seat 1’s default device |
| `kiosk_audio_per_app` | Kiosk apps use the seat’s sound device as per-app default (undocumented) |
| `no_user_isolation` | No user isolation between seats |
| `activity_needs_console_listener` | Console listener not running: activity not detected |
| `hotkey_needs_filter` | Shortcut only works with console listener or input filter |
| `gamepads_shared` | Gamepads are shared by all seats |
| `xinput_pads_shared` | XInput gamepads are shared by all seats |
| `touch_emulated_as_mouse` | Touch is passed as mouse input |
| `input_not_isolated_non_usb` | Non-USB keyboards and mice are not fully isolated |
| `usb_input_captured` | USB keyboards/mice of running seats are taken from seat 1 |
| `usb_capture_mixed_seats` | USB device with parts on different seats not taken from seat 1 |
| `usb_capture_last_console_input` | One USB device stays with seat 1 (it would have no input left) |
| `usb_capture_unsupported_collection` | USB device with gamepad/touch/pen part not taken from seat 1 |
| `usb_capture_failed` | A USB keyboard/mouse could not be moved (see log) |
| `usb_capture_held` | USB input given back to Windows; stays with seat 1 until a seat starts |
| `usb_restore_inexact` | A USB keyboard/mouse did not get its exact original driver back |
| `driver_not_signed` | The driver is not signed |
| `input_not_routed` | Input devices are not routed to seats |
| `rdp_listener_on_all_interfaces` | Remote Desktop listens on all adapters (NLA on) |
| `clipboard_shared_with_host_session` | Clipboard goes through the host session’s clipboard |
| `no_seat_drives` | This seat has no drive that can be redirected |
| `drive_without_global_letter` | Seat drive letter visible only in the seat session; not redirected |
| `drive_acl_volume_refused` | Drive file system changed after check; not redirected until reconnect |
| `drive_letter_moved` | Drive letter now points to another volume; not redirected |
| `drive_volume_unknown` | Drive volume could not be determined; not redirected |
| `no_seat_readers` | No smart card reader given to this seat |
| `no_seat_printers` | No printer with a queue given to this seat |
| `printers_all_queues` | All printers of the PC are redirected to this seat |
| `printers_print_as_host` | Jobs go as the seat’s background account, not the seat user |
| `no_per_user_isolation` | All seats run as the same Windows user |
| `one_kiosk_visible_at_a_time` | Only one kiosk desktop is visible at a time |
| `kiosk_shell_seat_aware_apps_only` | Kiosk input comes as window messages; raw input/DirectInput/XInput only with per-app isolation |
| `kiosk_per_seat_accounts` | Kiosk seats can run as their own users (from seat 2): files apart, not security |
| `kiosk_users_share_desktop` | Kiosk seats share one desktop: programs can see/read each other; no personal accounts |
| `kiosk_account_fallback` | Kiosk seat’s own user could not be opened; runs as seat 1’s user |
| `no_privacy_from_console` | The seat’s picture cannot be hidden from seat 1 |
| `audio_off` | Sound off |
| `shared_fallback_unconfigured` | No shared sound device chosen; seat may stay silent |
| `using_shared_fallback` | The seat uses the shared sound device |
| `session_reset_on_timeout` | Disconnected sessions are closed after a while (policy) |
| `hardware_gpu_not_enabled` | RDP sessions do not use the graphics card (policy off) |
| `rdp_disabled_until_prepare` | Remote Desktop is off; the preparation step turns it on |
| `reference_product_disabled` | Another multiseat product installed but off; keep it off |
| `rds_licensing_not_configured` | RD licensing not configured; sessions stop after grace period |
| `two_admin_sessions_only` | This Windows Server allows only two admin sessions (no RDSH role) |
| `termservice_restarted` | Remote Desktop Services restarted |
| `loopback_listener_created` | Port for seats without a password created |
| `loopback_listener_updated` | Port for seats without a password updated |
| `loopback_listener_firewall_rule` | Firewall rule added for the passwordless port |
| `loopback_listener_old_rule_removed` | Old firewall rule removed |
| `termsrv_wrapper_enabled` | Remote Desktop wrapper enabled |
| `wrapper_managed_externally` | Another program manages the wrapper; not touched |
| `termsrv_wrapper_external` | Seats use another program’s wrapper; not changed |
| `loopback_listener:<state>[:<x>]` | "Port for seats without a password: <state text>" (states below) |
| `loopback_listener_exposed:<port>` | WARNING: removed passwordless port still runs without NLA and unprotected; close RDP seats or restart |
| `loopback_listener_unload_pending:<port>` | Removed passwordless port runs until RDS restarts |

Fallback notes (`ServiceStatus.fallback_notes`) also come as `cat:wanted->used:reason`, `cat:wanted:reason` (category session/display/input/audio/gate + backend names + reason text from `t.unavailableReasons` / `needsSetup` / drivers); those reason tables belong to another part.

#### Passwordless-port listener states (`loopbackStateText`)

Where shown: in `loopback_listener:*` notes and in seat failure `loopback_listener_unavailable` detail (`listener=loopback_listener:<state>`).

| State | English text |
|---|---|
| `unknown` | not checked yet |
| `disabled` | turned off in the settings |
| `not_needed` | not needed: every seat has a saved password |
| `ready` | ready (shown as "ready (<port>)") |
| `pending_restart` | waiting for Remote Desktop Services to restart (after RDP seats close) |
| `port_invalid` | invalid port (1024–65535, not RDP’s own port) |
| `port_in_use` | port used by another program or listener |
| `firewall_off` | Windows Firewall off in at least one profile |
| `firewall_local_rules_ignored` | group policy makes firewall ignore local rules |
| `nla_forced_by_policy` | policy requires NLA on all connections |
| `tls_off_by_policy` | policy turns off TLS connections |
| `no_accounts` | no account can use this port, or not found |
| `rdp_tcp_missing` | RDP-Tcp listener not found |
| `write_failed` | settings could not be written to the registry |
| `termservice_restart_failed` | Remote Desktop Services could not restart |
| `restart_needs_reboot` | RDS cannot stop on this Windows version; restart the PC |
| `not_listening` | port not listening yet; wait or restart |
| `untrusted_listener` | another program listens on the port |
| `key_changed` | listener settings changed afterwards; fixed on seat restart |
| `firewall_rule_missing` | protecting firewall rule missing or changed |

#### Redirection refusal reasons (`t.refusalReasons`)

Where shown: Status page, per seat, list of refused redirection requests as "<channel>: <reason> (+N)" (`refusalLabel`; channel names `t.redirect.channels`), raw detail as tooltip.

| Code | English text |
|---|---|
| `direction_to_seat_off` | passing to the seat is off |
| `direction_from_seat_off` | passing from the seat is off |
| `format_not_allowed_to_seat` | this format may not be passed to the seat |
| `format_not_allowed_from_seat` | this format may not be passed from the seat |
| `format_not_offered` | the requested format was not offered |
| `paste_from_seat_refused` | paste from the seat refused |
| `files_to_seat_off` | file transfer to the seat is off |
| `files_from_seat_off` | file transfer from the seat is off |
| `clipboard_from_other_seat` | the data was copied on another seat |
| `stale_clipboard_response` | a clipboard reply from an earlier connection was dropped |
| `clipboard_backend_missing` | the clipboard could not start |
| `reader_not_assigned` | the reader does not belong to this seat |
| `reader_name_ambiguous` | reader name on several readers; ID unreadable |
| `reader_admin_refused` | no permission to change reader settings |
| `reader_source_missing` | the smart card service is not available |
| `smartcard_off` | smart card redirection is off |
| `list_readers_failed` | the readers could not be listed |
| `drive_letter_moved` | the drive letter now points to another volume |
| `drive_volume_unknown` | the drive’s volume could not be determined |
| `drive_acl_volume_refused` | file system changed after check; not redirected until next connection |
| `drive_unassigned` | drive no longer belongs to this seat; removed |
| `drive_broker_unavailable` | seat’s drive helper not running yet (user not signed in) |
| `drive_broker_lost` | connection to the seat’s drive helper was lost |
| `drive_broker_refused` | the seat’s drive helper refused the drive |
| `drive_add_failed` | new drive could not be added to the session |
| `printer_unassigned` | printer no longer belongs to this seat; removed |
| `printer_add_failed` | new printer could not be added |
| `printer_remove_failed` | printer could not be removed |
| `drive_remove_failed` | drive could not be removed |
| `drive_broker_answer_dropped` | invalid answer of the drive helper dropped |
| `list_reader_groups_failed` | reader groups could not be listed |
| `drive_is_system_drive` | the Windows drive is not redirected |
| `drive_root_invalid` | invalid drive root |
| `drive_root_unavailable` | the drive could not be opened |
| `printer_not_redirected` | the printer is not redirected to this seat |
| `no_printer_queue_found` | no printer queue found to redirect |
| `printer_discovery_failed` | the printers could not be read |
| `port_not_redirected` | ports are not redirected |
| `clipboard_filter_needs_ironrdp` | ActiveX client cannot filter the clipboard; clipboard off |
| `drives_need_ironrdp` | ActiveX client does not redirect drives |
| `smartcards_need_ironrdp` | ActiveX client does not redirect smart cards |
| `printers_need_ironrdp` | ActiveX client does not redirect printers |
| `clipboard_backend_error` | clipboard stopped working for this connection |
| `clipboard_pdu_error` | an invalid clipboard message was dropped |
| `clipboard_timer_error` | clipboard timer failed; clipboard off for this connection |
| `drive_configuration_invalid` | seat drive settings invalid; no drives redirected |
| `rdpdr_poll_error` | device redirection stopped working for this connection |
| `rdpdr_windows_only` | device redirection needs Windows |

#### Product states (`ProductState`, `productText`)

Where shown: Status page product row (`productLabel`), seats board status line (`board.ts`, safe mode as warn).

| State | English text |
|---|---|
| `disabled` | Off |
| `enabled` + `started: true` | On, seats running |
| `enabled` + `started: false` | On, seats not started |
| `safe_mode` (+ reason) | Safe mode (seat 1 only) |
| (`enabled` key) | On (map entry; `productLabel` uses the two variants above) |

#### Safe-mode reasons (`SafeModeReason`, `safeModeText`)

Where shown: app-wide notices area as a danger notice (`App.svelte`).

| Code | English text (short) |
|---|---|
| `windows_safe_mode` | Windows started in safe mode; Pulsar Seat not running. Restart normally. |
| `unclean_shutdown` | Windows did not shut down cleanly last time; extra seats not started to avoid a loop. Fix and restart. |
| `crash_loop` | Service failed to start several times in a row; stayed in safe mode. Create a diagnostic report and update. |

#### Failure reasons (`FailureReason` + wizard / shell reasons, `failureText`)

Where shown: seat state "error" label "Error (<reason text>)" (`seatLabel`: Status page, seats board cards, Settings seat list, `board.ts`); also preferred by `errorLabel` when a `ProtoError.reason` matches (toasts / danger notices, dialog errors). Kind: seat start failure unless noted.

| Code | English text (short) | Kind |
|---|---|---|
| `no_monitor` | The wizard needs at least one monitor | wizard start refusal |
| `resolution_too_low` | Main monitor’s resolution too low for the wizard | wizard start refusal |
| `unknown_monitor` | This monitor is not among the wizard’s monitors | wizard command refusal |
| `edition_unsupported` | This Windows edition does not support this session method | seat failure |
| `rds_not_ready` | Remote Desktop Session Host is not ready | seat failure |
| `seat_limit` | This many seats cannot run on this PC | seat failure |
| `account` | Account not found or disabled | seat failure |
| `logon_failed` | Sign-in failed (wrong or expired password) | seat failure |
| `client_auth` | Connection authentication refused | seat failure |
| `host_exited` | The seat host closed again and again | seat failure |
| `session_timeout` | The session did not start in time | seat failure |
| `display` | No monitor could be bound to the seat | seat failure |
| `driver` | Needed Pulsar driver not loaded or not signed; update | seat failure |
| `coexistence` | Another conflicting multiseat product was found | seat failure |
| `same_account_denied` | This account is already signed in on another seat | seat failure |
| `policy` | A Windows policy blocks this action | seat failure |
| `internal` | An unexpected error occurred | seat failure |
| `console_in_use` | Unlocked session on the console; lock it first (Win+L) | seat failure |
| `console_busy` | Another seat is on the console; lock that seat first | seat failure |
| `session_not_locked` | Seat session not locked and no password saved | seat failure |
| `nla_required` | Passwordless port is off; turn it on or save a password | seat failure |
| `loopback_listener_unavailable` | Passwordless port is not ready (+ listener state text) | seat failure |
| `kiosk_app_missing` | No program chosen for the kiosk seat | seat failure |
| `console_not_ready` | Console session not ready yet; try again after start | seat failure |
| `not_our_session` | Session found does not belong to this seat; restart seat | seat failure |
| `fsu_disabled` | Fast user switching turned off by a policy | seat failure |
| `no_account` | No account is chosen for the seat | reason (not in `FailureReason` type) |
| `account_unresolved` | The seat’s account was not found on this PC | reason (not in `FailureReason` type) |
| `elevation_cancelled` | Administrator permission was not given | manager shell |
| `pipe_server_untrusted` | Another program opened the control channel; no connection | manager shell |
| `shell_failed` | The window could not open | manager shell |

Wizard command refusal reasons without text: `monitor_not_picked`, `seat_out_of_range`, `not_input_device` (service `wizard.rs`).

#### Error codes (`ProtoError.code`, `errorText`)

Where shown: toasts → persistent danger notices with "(code · Windows N)" tag (`store.fail`), inline dialog errors. `access_denied` / `admin_only` also set the "needs elevation" state; `service_unavailable` sets "service down".

| Code | English text (short) |
|---|---|
| `service_unavailable` | = `t.serviceDown` (service cannot be reached) |
| `access_denied` | = `t.adminNeeded` (administrator rights needed) |
| `admin_only` | An administrator restricted changes to administrators. |
| `version_mismatch` | Manager and service versions do not match. Reinstall. |
| `disabled` | Pulsar Seat is off. Turn it on first. |
| `safe_mode` | Pulsar Seat is in safe mode; extra seats do not start. |
| `revision_conflict` | Settings changed elsewhere. Reload and try again. |
| `conflicts_not_confirmed` | Some assignments cannot be applied; not saved without confirmation. |
| `invalid_config` | The settings have errors; not saved. |
| `invalid_value` | Invalid value. |
| `not_found` | Not found. |
| `seat_running` | The seat is already running. |
| `seat_not_running` | The seat is not running. |
| `unsupported` | The selected method does not support this. |
| `not_ready` | Not ready yet; try again in a moment. |
| `timeout` | Timed out. |
| `driver_unavailable` | A needed Pulsar driver is not usable. |
| `coexistence` | Another conflicting multiseat product was found. |
| `policy_blocked` | A Windows policy blocks this action. |
| `logon_failed` | Sign-in failed. |
| `warning_not_acknowledged` | Experimental method needs the warning acknowledged. |
| `wizard_state` | The wizard does not accept this action right now. |
| `os` | A Windows operation failed. |
| `store` | The settings file could not be written. |
| `not_implemented` | This action is not in this version of the service yet. |
| `wrong_seat_kind` | Not possible on this kind of seat (local/remote). |
| `session_unavailable` | Remote seats need several sessions; method does not provide that. |
| `bad_request` | Invalid request. |
| `internal` | An unexpected error occurred. |

Remote-seat error codes (`remoteErrorText`, shown by remote pages via `remoteErrorMessage`, which prefers `rt.failure[reason]`):

| Code | English text (short) |
|---|---|
| `version_mismatch` | Manager and service versions do not match. |
| `bad_request` | Invalid request. |
| `not_found` | Seat not found. |
| `invalid_name` | Only English letters and digits can be used. |
| `duplicate_name` | A seat with this name already exists. (with field `display_name`: display name used on another seat) |
| `invalid_display_name` | Display name cannot be empty or start/end with a space. |
| `invalid_port` | Port must be 1029–65514. |
| `port_overlap` | Port range overlaps another seat. |
| `no_free_port` | No free port range left. |
| `invalid_value` | Invalid value. |
| `invalid_user` | Windows user cannot be used (missing, disabled, locked, password expired). |
| `user_in_use` | User in use on another seat or the console. |
| `missing_credentials` | Enter a user and password first. |
| `seat_running` | Cannot change while the seat is running. |
| `not_implemented` | Feature not ready yet. |
| `store` | Settings could not be saved. |
| `service_unavailable` | Service or the seat’s stream server cannot be reached. |
| `internal` | An unexpected error occurred. (also fallback) |
| `unsupported` | Feature not available on this PC. |
| `edition_unsupported` | This Windows edition does not support multiseat. |
| `coexistence` | Nothing done: another multiseat product found. |
| `seat_failed` | The seat could not start. |
| `timeout` | The operation timed out. |
| `driver_missing` | A needed driver is not installed. |
| `policy_blocked` | A group policy blocks this action. |
| `warning_not_acknowledged` | Read and acknowledge the warning first. |
| `no_pending_request` | No pending pairing request. |
| `wrong_pin` | Wrong PIN. |
| `hash_mismatch` | The file could not be verified. |
| `firewall` | The firewall could not be set up. |

Remote field errors (`remoteFieldErrorText`): `name_empty` "Type a name.", `name_chars` "Only English letters and digits…", `name_long` "at most 128 characters", `name_duplicate`, `display_name`, `display_name_duplicate`, `port` "1029 to 65514", `port_overlap`.

#### Notices (`notice` event codes, `noticeText`)

Where shown: info toast, prefixed "Seat N: " when the notice names a seat (`store.svelte.ts`). Exceptions: `input_move_pending` opens the input-move confirmation instead; `same_account_warning` about the manager's own seat opens the same-account dialog (unless hidden by preference).

| Code | English text (short) |
|---|---|
| `gpu_preference_unavailable` | Seat’s graphics card preference could not be applied (one GPU, or GPU class unknown). |
| `os_build_changed` | Windows was updated. Review seat settings and save again if needed. |
| `service_gap_no_input` | Service did not respond briefly; input may have stopped. |
| `same_account_warning` | Same account signed in on several seats; not recommended. |
| `seat_starting` | The seat is starting. |
| `run_entry_missing` | Manager startup entry for the start mode is missing; seats may not start. |
| `program_files_damaged` | Pulsar Seat files missing or damaged. Reinstall. |
| `hardware_changed` | Plugged-in devices differ from the saved configuration. |
| `devices_beyond_limit` | Devices above the seat limit were made spare. Remember to save. |
| `reboot_needed` | A restart is needed to finish the change. |
| `seat_start_failed` | A seat could not start. |
| `password_failing` | A seat’s saved password was rejected; enter it again. |
| `tweak_failed` | An optional system setting could not be applied or undone. |
| `keep_awake_failed` | PC could not be kept awake while extra seats are in use. |
| `input_move_pending` | Keyboards/mice moved with the shortcut wait for confirmation. |
| `credprov_failed` | Seat sign-in component could not be registered or removed. |
| `credprov_logon_failed` | Seat automatic sign-in failed with the saved password. |
| `switch_refused` | Could not switch to the seat. |
| `dpi_mismatch` | Seat’s display scale differs from its setting. |
| `host_console` | Host console automatic sign-in could not be turned on or was changed. |

### 4.6 Remote seats

#### Remote seat (protocol 3 `SeatConfig` with `kind: remote`; page record `R.SeatConfig` in `types.ts`, mapping `model.ts` toRecord/applyRecord)

- `number` SeatNumber — next free number at create; shown "Seat N".
- `name` string — 1-128, `[A-Za-z0-9]+`, unique case-insensitive among all seats. Set in New seat dialog; not editable on seat page (shown as title).
- `autostart` bool (record `start_with_service`) — default false; "Start with Pulsar Seat".
- `account` — `{kind: local, user}` or `{kind: logon_screen}` (record `user_name`, '' = logon screen). Local accounts only.
- Password — stored by service (`set_seat_password`), `password_set` list in config view → `has_password`.
- `remote.id` string — service-owned.
- `remote.display_name` string — non-empty, trimmed, no control chars, unique among remote seats. Default = name.
- `remote.port` number — 0 (service assigns) or 1029-65514; reserves `port-5..port+21`; no overlap with other remote seats (|a-b| ≤ 26 overlaps).
- `remote.stop_action` `log_off | disconnect` — default log_off.
- `remote.display` DisplaySettings: `render_adapter` (name|null), `render_adapter_id` (PnP id|null) default null; `scale_static` false;
  `scale_percent` 100 (100-500 step 25); `super_sampling_percent` 100 (100-500 step 25); `sdr_white_level_nits` 400 (80-480 step 10); `force_sdr` false;
  `refresh_hz` 60 (15-500); `luminance` {min_nits_x10000 10 (no UI), max_nits 1000 (1-10000), max_full_frame_nits 600 (0-10000)}; `fixed_width`/`fixed_height`
  0 (0×0 follow, else 640-8192); `lock_refresh` false.
- `remote.apps` AutostartApp[]: {id, path, arguments, run_as_system}. Default empty.
- `remote.rdp_client_name` string — service-owned; shown in runtime as "RDP client name".
- `remote.audio` {stream_audio true, microphone false}.
- `remote.stream` {gamepad `auto|x360|ds4|pulsar_ds4|pulsar_ds5|pulsar_ds5e|pulsar_xone` default auto; capture `default|ddx|wgc|pulsar_idd` default default;
  min_fps 1 (1-240); firewall_domain false; keep_pairing_on_delete false}.
- `remote.stream_backend` `sunshine | pulsar` — default sunshine; change only while stopped.
- `remote.pulsar` PulsarSeatOptions {relay '' ; network_mode `auto|p2p-only|relay-only` auto; node_port_mode `base|ephemeral|fixed` base; node_port 0 (fixed:
  1024-65535); port_mapping false; lan_discovery true; access `approve|approve_or_password` approve_or_password; max_clients 1 (1-4)}.
- `remote.last_mode` {width,height,hz} — service-owned, not shown.
- `remote.vm` VmSeat | absent (see VM seat).
- Fields of `SeatConfig` set but not editable in the remote UI: `cpu_sets` [], `cpu_hard_limit` false, `wallpaper` false.

#### Client app entry (`AppEntry`, stream server app list)

- `name` string (≤128, required to keep), `cmd` string optional, other upstream keys (`detached`, `working-dir`, `image-path`, …) kept untouched. Edited
  on seat page (Name, Command only).

#### Remote settings (`settings.remote`, `GlobalSettings`)

- `start_with_windows` bool false — read by nothing; no UI.
- `hid_isolation` bool false — "Isolate HID devices".
- `process_patching` bool false — "Turn on process patching" (Patching flow).
- `steam_isolation` bool false — "Split Steam per seat" (forced false when patching off).
- `verbosity` `none|error|warning|info|debug` default info — `rt.verbosity` None/Error/Warning/Information/Debug.
- `multi_session` bool false — service-owned; Host page toggle (`set_multi_session`).
- `pad_backend` `vigem|usbip_ds5|pulsar_pad|null` default null (Automatic).
- `bluetooth_console_only` bool true.
- `display_backend` `rdp_session|vdd|pulsar_idd|null` default null.
- `pad_audio` bool false.
- `vm` VmSettings (see below).

#### VM settings (`settings.remote.vm`)

- `storage_location` '' (absolute path or empty); `memory_percent` 50 (10-90); `storage_gb` 128 (32-4096); `image_kind` `none|wim|vhdx` none; `image_path`
  ''; `image_index` 1 (1-64); `language_cab` ''; `restart_gpus_on_shutdown` false; `nat_prefix` '172.31.240.0/24' (a.b.c.0/16-28).

#### VM seat (`remote.vm`, `VmSeat`)

- `memory_mb` 0 (0 or ≥2048); `processors` 0 (0-64); `gpu` `off|auto|adapter` auto (labels `rt.vm.gpuModes`); `gpu_adapter` string|null (required for adapter);
  `shared_folders` [{id (uuid), host_path, guest_path, read_only (default true)}]; `forwarded_ports` [{id, description, protocol tcp|udp (default tcp),
  host_port, guest_port (1-65535)}]; `hide_vm_suffix` false; `stop` `shut_down|save|turn_off` shut_down (labels `rt.vm.stopModes`); `guest_address` service
  assigned (network + 10 + seat number). Constants: `MIN_MEMORY_MB` 2048, `HOST_DRIVE_MOUNT` `C:\HostDrive`.

#### Hyper-V host status (`VmHostStatus`, `vm_seats {type: status}`)

- `feature` `enabled|disabled|enable_pending|unavailable` (`rt.vm.featureStates`), `vmms` bool (not shown), `network_ready`, `base_present`, `building`,
  `last_error?`, `storage` (default storage path), `gpus` string[], `host_memory_mb`, `logical_processors`, `seats` [{seat, state (Hyper-V state or `missing`),
  guest_address, guest_phase? (`rt.vm.guestPhases`), guest_detail?}]. Actions `VmAction`: status, prepare_host, build_base, reset_base, reset_sandbox (all
  with confirm true).

#### Seat status (page view; `seatStatus` / `remoteState` in `model.ts`)

- `state` `stopped|booting|running|stopping|failed` — labels `rt.seatState`. Mapping from protocol 3 runtime: off/none → stopped; waiting_for_display → booting
  phase display; starting{phase} → booting with phase (logging_on → `user`, waiting_for_session → `session`, binding_display → `display`, launching_host →
  `stream`, queued → none, other → `connect`); at_logon / signed_in / locked → running; stopping → stopping; error{reason, detail} → failed with failure =
  reason, last_error = detail.
- `phase` labels `rt.phase`: user "Checking the user", session "Opening the session", display "Preparing the display", audio "Preparing sound", stream
  "Starting the stream server", pads "Preparing gamepads", isolation "Setting up isolation", autostart "Starting apps". (`connect` has no label.)
- `streaming` bool (runtime degraded contains `streaming`), `stream_client` (runtime client name while streaming), `session_id`, `crash_count` (stream server
  crashes), `has_password`, `failure`, `last_error`.

#### Remote runtime (`SeatRuntime`, `get_remote_runtime`)

- `session_id`, `client_name`, `state`, `phase`, `failure`.
- `display` {backend `rdp_session|vdd|pulsar_idd` (`rt.displayBackend`: "Remote Desktop display (default)", "Virtual Display Driver (signed, physical screen
  only)", "Pulsar virtual display driver (unsigned, trial)"), `state` (not shown), `hdr` ('ok' or `rt.hdrReason` code), `host` `provider|loopback`,
  `fallback` (`rt.wrdsReason` code), `device_instance` (not shown)}.
- `audio` {state (`rt.audioState`: capturing "Sound is added to the stream", no_device "No sound device in the session", blocked_by_policy "A group policy
  blocks sound", disabled "Off", unknown "Unknown"), limitations (`rt.audioLimitation`: no_microphone, no_controller_audio, steam_remote_play_unsupported
  "Steam Remote Play is not supported on seats.", surround_is_stereo "Multi-channel sound is passed as stereo.", policy_blocks_audio, mic_policy_blocked,
  mic_needs_pulsar)}.
- `pads` [{slot, pad_id, kind, backend (`rt.padBackend`), phase}].
- `stream` {backend, flavour `upstream|pulsar_fork` (not shown), running, status {stream_active, clients_connected}, capabilities (not shown), crash_count,
  relay_state (`ok|auth_required|unreachable|version_mismatch`), relay_id (9 digits), node_port (not shown), relay_auth_required, mic
  (`off|unsupported|waiting|connected|failed|closed`)}.
- `isolation` [{exe, role, hooks_on, error}].

#### Live display mode result (`ModeResult`)

- `status` labels `rt.modeStatus`: ok "applied", ok_sdr_fallback "applied in SDR", clamped "applied with clamped values", no_display "the seat display is not
  ready", rejected "Windows refused the mode", timeout "the display driver did not answer in time", unsupported "this display cannot change mode (it follows the
  client)". Plus width, height, hz, hdr, render_width/height, format.

#### Pairing (`PairState`, `PendingPair`, `PairedClient`)

- PairState: `pending`, `last_result` (not shown), `available` (control channel), `backend`, `relay_auth_required`.
- PendingPair: `request_id`, `device_name`, `received_at` (not shown), `mode` `pin|approve`, `kind` `connect|pair`, `key_fp` (16 hex), `password_state`
  `none|wrong|wait`, `label` / `expires_in_s` (not shown).
- PairedClient: `uuid`, `name`.
- OTP: string, shown only while dialog open. Connection password: write-only.

#### Credential test (`CredentialTest`)

- `ok`, `error_code` (not shown), `message` (Windows text, system language; used as title on failure), `disabled`, `locked_out`, `password_expired`.

#### Host info (`HostInfo`, `get_host_info`)

- `edition`, `build`, `class` (`rt.hostClass`: home, client_multi_session_unavailable, server_without_rdsh, server_rdsh, client), `rds_ready`, `listener_port`,
  `licensing` {mode 2 per device / 4 per user / else not configured, grace_days_left}, `max_seats` (null = unlimited), `warnings` [{kind, days?, value?,
  user?, service_dll?}], `coexistence`, `termwrap`, `display_backends`, `pad_backends`, `multi_session`, `error` (not shown), `wrds`.
- Host warnings `rt.hostWarning`: rdsh_missing, licensing_not_configured, grace_ends, policy_override, seat_user_is_admin "The seat user is an administrator; a
  normal user is recommended.", foreign_wrapper, wrapper_managed_externally, autostart_run_as_system_ignored, autostart_failed, adapter_fallback, firewall,
  control_unavailable, capture_error, dpi_mismatch, stream_fallback "This stream-server build lacks some Pulsar features; the seat uses the standard behaviour
  for them.", display_fallback "The Pulsar display could not be used; the seat runs on the Remote Desktop display." (texts in en.ts lines 2779-2796).
- Coexistence report: `mode` `normal | alongside | passive | residue_only` — `alongside` = the other (reference remote) multiseat product is installed; blocks
  nothing; UI shows nothing for it except findings and notes. `passive` (legacy, no longer sent) → passive notice and locks. `residue_only`: no special UI.
  `reference_running` (not shown directly), `reason` (not shown), `findings` [{signal, detail (not shown)}], `upstream_ports` (not shown), `notes`
  (`duo_service_running`).
- Signals `rt.signal`: ref_arp, ref_service, termsrv_wrapped, bluetooth_wrapped, rdp_idd_redirected, ref_shim_db, ref_shim_exe_map, ref_shim_arp,
  ref_shim_file, compat_layer_forced, ref_pad_driver, ref_root_cert, kernel_helper_residue, defender_exclusions, ref_firewall_rules, rds_residue,
  upstream_sunshine "A separately installed Sunshine is present (ports may clash).", vigem_present "The ViGEmBus driver is installed.".
- Termwrap status: `state.state` `off|on|foreign_active|external|blocked`, `service_dll`, `reason`, `wrapper` (external candidate), `blocked` {reason}|null;
  `supported`, `candidates` [{service_dll, config_file, dll_version, signed trusted|untrusted|unsigned, project, kind}], `warning_version`,
  `ack_warning_version`, `warning_text` (en), `warning_text_tr`.
- External wrapper use (`externalWrapper.ts`, only when state = external): `blocked` (blocked.reason set) / `in_use` (ack version == warning version) /
  `needs_ack`.
- Display backend info: kind, installed, driver_version, iddcx_version (not shown), hdr Ok/Err(code), remote_session_capable, signed.
- Pad backend info: id, installed, version, signed, kinds (not shown), reason (not shown).
- WRDS status: registration `absent|ours|partial`, provider_connected, listening, restart_needed, recovery_disabled, dll_ok (these are not shown
  individually), reason (`rt.wrdsReason`: wrds_unavailable, wrds_disabled_by_recovery, wrds_dll_missing, wrds_not_registered, wrds_restart_needed,
  wrds_not_listening, wrds_connect_failed, wrds_timeout, display_timeout, driver_not_installed), wanted.
- HDR reasons `rt.hdrReason`: force_sdr, rdp_display, os_too_old, iddcx_too_old, driver_no_fp16, gpu_unsupported, driver_not_installed, server_needs_whql.

#### Stream backends (`StreamBackendInfo`, `get_stream_backends`)

- backend `sunshine|pulsar`, installed, reason (e.g. `backend_missing`, not shown directly), version (not shown).

#### Isolation status (`IsolationStatus`, `get_shim_status`)

- installed, mechanism (not shown), kill_switch, foreign (signal code), targets [{exe_name, product_name, path}], mask_apps [paths], health (not shown),
  modules (names), supported.

#### DXVK (`DxvkStatus`)

- release (version|null), games [{exe, deployed, version, dlls, error}].

#### Audio cleanup (`AudioCleanup`): removed[], failed[[name, error]] (only counts shown).

#### Account picture (`UserPicture`): mime (png/jpeg/bmp/gif accepted), data base64; else initial letter (Turkish locale upper-cases i → İ).

#### Error codes (`ErrorCode`, `remoteErrorText`) — shown via `remoteErrorMessage`

version_mismatch "The manager and service versions do not match."; bad_request "Invalid request."; not_found "Seat not found."; invalid_name "Only English
letters and digits can be used."; duplicate_name "A seat with this name already exists." (with field display_name → "This display name is used on another
seat."); invalid_display_name "The display name cannot be empty or start or end with a space."; invalid_port "The port must be a number from 1029 to 65514.";
port_overlap "This port range overlaps with another seat."; no_free_port "No free port range left."; invalid_value "Invalid value."; invalid_user "The Windows
user cannot be used (missing, disabled, locked or the password has expired)."; user_in_use "This user is in use on another seat or on the console.";
missing_credentials "Enter a user and password first."; seat_running "This cannot change while the seat is running."; not_implemented "This feature is not
ready yet (it comes in a later stage)."; store "The settings could not be saved."; service_unavailable "The service or the seat's stream server cannot be
reached."; internal "An unexpected error occurred."; unsupported "This feature is not available on this PC."; edition_unsupported "This Windows edition does
not support multiseat."; coexistence "Nothing was done because another multiseat product was found."; seat_failed "The seat could not start."; timeout "The
operation timed out."; driver_missing "A needed driver is not installed."; policy_blocked "A group policy blocks this action."; warning_not_acknowledged "Read
and acknowledge the warning first."; no_pending_request "There is no pending pairing request."; wrong_pin "Wrong PIN."; hash_mismatch "The file could not be
verified."; firewall "The firewall could not be set up."
Rules: an error with a `reason` that has a `rt.failure` text shows that text first; unknown codes show "An unexpected error occurred.". A blocking config
issue other than `remote_seat_invalid` becomes `invalid_value` with reason = issue code (shown as "Invalid value." unless a failure text exists for it).

#### Failure reasons (`rt.failure`, `remoteFailureText`)

seat_booting, dxvk_hash_mismatch, backend_missing "The chosen stream server is not installed on this PC.", backend_pulsar, backend_sunshine, edition_unsupported,
coexistence "The seat was not started because another multiseat product was found.", rds_not_ready "The Remote Desktop service is not ready.", seat_limit,
account "The seat user cannot be used.", password_expired, rdu_membership "The user could not be added to the “Remote Desktop Users” group.", profile, client_auth
"Sign-in failed: wrong user name or password.", cert_mismatch, session_timeout, client_exited, token, display_timeout, no_display, driver_not_installed,
session_attach_unsupported, stream_server "The stream server crashed again and again.", server_binary_missing, server_hash_mismatch, port_in_use "One of the seat's
ports is in use.", firewall, multi_session_off "Turn on multiseat on the “Host PC” page first.", shim_foreign, shim_blocked, server_without_control_pipe,
no_pending_request, wrong_pin, pair_timeout, warning_not_acknowledged, not_client_sku "This module is only for Windows Pro / Enterprise.", foreign_wrapper,
external_wrapper, concurrent_sessions_off, external_wrapper_inactive, unknown_termsrv_build, wrapper_missing, wrapper_unsigned, wrapper_insecure_acl,
wrapper_arch, termservice_restart, reference_product_present "Another multiseat product is installed.", dxvk_not_downloaded, bad_app_list, unsupported, cancelled
"The start was cancelled." (full texts en.ts lines 2884-2936). Unknown reason → error text if code matches, else "An unexpected error occurred. (<reason>)";
null reason → "This feature is not available on this PC.".

#### Field errors (`validation.ts`, `remoteFieldErrorText`)

`seatNameError`: empty → name_empty; >128 → name_long; not `[A-Za-z0-9]+` → name_chars; duplicate (case-insensitive) → name_duplicate.
`displayNameError`: empty, leading/trailing whitespace or control char U+0000-001F/U+007F → display_name; duplicate (case-insensitive) → display_name_duplicate.
`portError`: not 1-5 digits or outside 1029-65514 → port. `portOverlapError`: |a-b| ≤ 26 with any other remote seat base → port_overlap.
`nodePortConflict`: 21117, or within another base's block (base>0, base-5..base+21), or equal to another fixed node port / VM UDP forwarded host port.
`relayValid` (model.ts): see seat page.
Constants: PORT_MIN 1029, PORT_MAX 65514, NAME_MAX 128, PULSAR_DISCOVERY_PORT 21117.

#### Requests used (`api.ts`, all admin)

get_host_info, set_multi_session, get_termwrap_status (not used by pages; host info carries termwrap), set_termwrap, get_remote_runtime, list_render_adapters,
get_pad_backends (not used by pages), get_stream_backends, submit_pin, get_pair_state, cancel_pair, list_paired_clients, unpair_client, unpair_all,
regenerate_seat_identity, get_apps, set_apps, answer_peer_request, set_seat_connect_password, get_seat_otp, rotate_seat_otp, answer_relay_auth,
test_seat_credentials, enable_unlock_account, get_shim_status, set_shim_targets, set_app_masking, get_dxvk_status, set_dxvk, get_log_tail, get_user_picture,
cleanup_audio_endpoints, set_seat_display_mode. Plus shared requests: get_config/validate/apply_config (applyRemote), start_seats, stop_seat,
set_seat_password, request_reboot, poll_events, vm_seats, local_users, lan_addresses, pick_program, pick_path.

#### Demo mode (`demo.ts`, browser dev without Tauri / tests)

Host: Windows 11 Pro build 28000, class `client`, max_seats 1, coexistence normal, termwrap off and unsupported (enabling throws `wrapper_missing`), WRDS absent
(`wrds_not_registered`). One render adapter "Demo GPU" (NVIDIA, 8 GB). Stream backends: Sunshine installed (version "demo"), Pulsar missing
(`backend_missing`). A started demo seat runs: display `pulsar_idd` via provider, HDR ok, audio capturing, Sunshine streaming to 1 client. Pair state:
waiting; `globalThis.__demoPairRequest = true` shows a pending PIN request from "Salon TV". Apps default [Desktop]. DXVK release v2.7.1, no games. Credential
test ok. OTP "123456". Logs: one demo line. Live mode always `unsupported`. Every call checks admin.

## 5. Live and real-time behaviour

### 5.1 Global

- Event subscription (shell thread `service-events`): topics `status` (`product_state`, `seat_state`, `config_changed`, `notice`), `devices` (`device_arrived`, `device_changed`, `device_removed`), `activity` (`device_activity`), `wizard` (`wizard_progress`). Reconnect back-off 500 ms doubling to 5 s. Undecodable frames are skipped.
- Store event handling: `product_state` updates product; `seat_state` replaces the seat runtime (error -> error notice "Seat N: <failure>"); `config_changed` re-reads config when revision differs (keeps a dirty draft and raises changed-elsewhere) and status; `notice` -> toast / input-move dialog / same-account dialog; `device_arrived` adds device + "just connected" mark for `flashSeconds`; `device_removed` marks known devices unplugged, drops unknown ones; `device_activity` increments activity counter (flash), feeds identify-by-touch; `wizard_progress` updates wizard; step `done` reloads config (discard) + toast `t.wizard.done`; `cancelled` toast `t.wizard.cancelled`.
- Polling: 3 s store timer (status when link down; SCM state always unless a service action runs). Update dialog 1 s while busy. Seat restart 500 ms up to 60 s. SCM control 250 ms up to 60 s.
- Link state: re-read everything on link up after being down; `service_link` asked once at start (fixes a missed first event).
- Toasts: 4 s, paused on hover/focus.
- Notices are derived live from store state.
- Window dirty state pushed to the shell on every change (`set_ui_dirty`).

### 5.2 Local seats board

- Service events (store, `onEvent`): `seat_state` updates one seat's runtime (card status/fact, status line); `off`/`starting` clears its "Restart required"; `error` shows a toast "Seat N: <failure>". `config_changed` reloads config/status. `device_arrived` adds/replaces the device and marks it "New" for `flashSeconds` (default 4 s) with a highlight. `device_changed` replaces it. `device_removed`: device in config -> kept as not plugged in; else removed from the list. `device_activity` increments a counter -> 1.2 s pulse on the row; also ends "Find by touch".
- Fallback polling: every 3000 ms (`POLL_MS`) `get_status` while the event link is down.
- Find by touch: needs the event link; the first `device_activity` of a known device: toast "Found: <name>", the row/face is focused, marked "Found", and its Move menu opens; if the device is not on the board (hidden hub, hidden seat), the Assign dialog opens instead. "Stop searching" in the notice or menu ends it.
- Identify (IPC `identify_device {key,on}`): monitors and speakers only (present, admin). What the hardware shows is service-side (comment: number painted on the physical screen for monitors; for speakers unclear in code, probably a sound). The row/face shows "Found" highlight while on. "Number the monitors" turns identify on/off for every present monitor in sequence. No auto-timeout in the manager (unclear whether the service stops it by itself).
- Webcam "identify" = live WebView preview (single or grid); streams stop on close.
- Restart button: polls `get_status` every 500 ms for up to 60 s.
- Live region (`aria-live=polite`) announces each move.
- USB dock wizard: one scan on open, then manual; stability = last 2 scans equal.
- Test tone and microphone level meter: not present in these files (no code found in the manager for either).

### 5.3 Settings

- Service status: event subscription; fallback polling every 3000 ms (`POLL_MS`) while the link is down. Drives seat runtime labels on Settings rows and the host console state notice on Advanced.
- Seat restart wait (seats board "Restart" after a save): polls status every 500 ms up to 60 000 ms (not on this part's screens).
- `get_backends`: fetched on Advanced / Network section mount when missing; refreshed after "Give USB keyboards and mice back"; full refresh in `refreshAll`.
- `get_update_status`: once when the Settings page mounts (admin only), for the Updates help line.
- WebView `online` / `offline` events: enable/disable help links.
- Language change reloads the window.
- Dialog loads on open: `list_local_users` (account), `list_cpu_sets` (CPU), `list_ipv4_addresses` (IP), `list_user_pictures` (picture, forced reload each time from the seats board).
- On-demand only: Hyper-V status, Steam add-on DLL check, Restart Remote Desktop Services, restore USB input.
- Kiosk audio line and the "unused device kinds" suggestion are recomputed from the live device list (`store.devices`).
- Hotkey dialog captures key presses live.
- No meters, previews, identify or test tones in this part.

### 5.4 Wizard and tool dialogs

- Wizard progress: service event `wizard_progress` (topic `wizard`) drives the main wizard view and every monitor page. Pages start from the shell's cached last progress (`wizard_last_progress`).
- Number overlays on every monitor during the wizard: drawn by the service (`display.identify`, until hidden); pages also show big pulsing ring numbers.
- Press counter "Pressed: k / 2" and two F-key caps fill live; click counter "Clicked: k / 2"; click targets move to a new random spot after each hit.
- Gather-by-touch: `device_activity` events (topic `activity`); stops when one keyboard and one mouse were seen.
- `target_hit` steers a click to the page's seat if within 1.5 s (service); modifier window 3 s (service, HYPOTHESIS).
- Wizard closes itself on step `done` / `cancelled`; Alt+F4 on a page emits `wizard-page-close` → quit.
- Toasts on `done` / `cancelled`; config reload on `done`.
- Notices: `notice` events → toasts; seat state failures update live via `seat_state`; safe mode via `product_state`.
- Compare / Import / Diagnostic / Experimental dialogs: no live updates (one-shot calls; spinners while busy).
- No polling intervals in these files.

### 5.5 Remote seats

- `RemoteTab` poll: every **3000 ms** (`POLL_MS`) while the remote view is shown and the manager is admin and the service reachable. Per tick: local users and
  LAN addresses (only until first non-empty result), then only the page on screen: seat page → `get_remote_runtime` every tick, render adapters / stream
  backends / Hyper-V status once per manager run (once per RemoteTab mount); host / root page → host info once; settings page → isolation status once.
- Seat list states, phases, streaming client, session id come from `store.status` (app-level updates, not this poll).
- Seat page: stored record and VM part follow the store; the draft is reset from them only while nothing is staged.
- Pair dialog: long poll `poll_events(after, 1500 ms, ['stream','status'])` in a loop while open and seat running; on error wait 2000 ms; event kinds
  handled: `pair_pending`, `pair_ended` (result message), `otp_changed` (re-read OTP if shown), `trust_changed`, `relay_auth_required` → re-read pair
  state + clients. OTP cleared on close.
- Live display mode apply (`set_seat_display_mode`) with immediate result line.
- Patching flow progress shown at least 3000 ms.
- Credential test runs on open; "Test again" reruns.
- Account pictures: fetched once per user name (again after a user change).
- Copy-to-clipboard feedback "Copied." (seat address, pair addresses, Pulsar ID).
- Service events mapped (`mapFrame`): `stream` events of remote seats, `seat_state` → `seat_phase` / `seat_failed`, `notice` with code `host_warning` →
  `host_warning` (detail JSON). Only the pair dialog consumes them.
- No auto refresh on Host, Logs, DXVK, VM pages (manual Refresh buttons).

## 6. Platform variants

### 6.1 Global

- Arch: manager reports `arch` (`app_info`); shown as "ARM64" / "x64" / "x86" on Status (versions line) and in the Update dialog. Update reason `no_asset`: no installer for this PC's architecture. Nothing in my files is greyed out by arch.
- Windows edition / backends: Status page shows what the service reports (availability, fallbacks, limitations, drivers). The experimental client-SKU session wrapper warning (`warning.ts`, shown elsewhere) states: client editions (Pro, Enterprise) need a user-installed TermService wrapper for concurrent sessions; Windows Server + RDSH is the licensed path; on ARM64 the wrapper must be native ARM64. Warning acknowledgement version `EXPERIMENTAL_WARNING_VERSION` stored in `settings.backends.experimental_ack`.
- Elevation: service start/stop/restart only in an elevated manager (SCM checks). Config changes only for elevated admins. Elevated copy has separate WebView2 data (prefs, language).
- Windows-only native parts: SCM, ShellExecute (`runas`, panels, help links), pipe server checks; non-Windows builds stub these (`scm::info` returns unknown).
- Browser (no Tauri): demo service.

### 6.2 Local seats board

- No ARM64/x64 branch in these files. Camera preview/grid depend on WebView `getUserMedia` (header comment cites ARM64 notes; "Untested on hardware").
- Windows edition: none in these files. HDR text says it needs Windows 11 (text only).
- Backend-dependent UI: session backend `desktops` -> "Kiosk seat…" entry. Display backend `console_only` -> no "No screen" line. Input backend `none` -> no keyboard/mouse lines. Input backend `winusb_capture` -> emergency restore in Advanced (other part).
- Admin: all IPC actions on this page need an elevated admin; non-admin sees a read-only board or the `t.adminToView` text.
- Product off / safe mode change the status line and disable starting.

### 6.3 Settings

- Session methods: `rds_loopback` is labelled "Windows Server x64 only, Remote Desktop Session Host"; reasons `rdsh_role_missing`, `rds_not_ready`, `edition_unsupported`, `arch_unsupported`. `termsrv_wrapper` is for client Windows, experimental, needs a wrapper the user installed (Pulsar Seat never ships one); reasons `not_client_sku`, `no_wrapper` / `wrapper_missing`, `not_native` (wrapper not built for this processor), `unknown_termsrv_build`, `foreign_service_dll`, `external_wrapper_inactive`, `concurrent_sessions_off`. `fast_user_switch`: reason `fsu_disabled`; hiding fast user switching is blocked while it is selected. `desktops` (kiosk seats): client SKUs, x64 and ARM64 (architecture 4.1.3).
- Unavailable methods stay selectable; the service falls back and reports it on the Status page.
- `display_core` unavailable reason `display_core_unavailable`; `pulsar_idd` and `kmdf_filter` depend on signed drivers (`driver_not_signed`, `driver_version_mismatch`).
- Input layouts: `raw_input_host` under `seat_one_console` is not isolated (warning); `winusb_capture` isolates USB keyboards/mice only (warning, "Not yet tested on real hardware"); `dedicated_host` gives full input isolation.
- Restart Remote Desktop Services only while an RDS-based method is active; on some builds TermService cannot stop (`restart_needs_reboot`).
- Windows Update policies: Windows Home ignores them (`t.compat.wuHint`).
- Hyper-V helper: feature may be `unavailable` on the Windows edition; enabling needs a Windows restart.
- Kiosk per-app sound device: Windows 10 1803 or later (undocumented interface); otherwise `kiosk_audio_shared`.
- CPU dialog: P/E letters only on hybrid CPUs (more than one efficiency class); hard limit needs one processor group.
- Defender exclusions: no effect when another antivirus is active.
- Fast startup off: needs a Windows restart.
- Source IP enforcement needs the `source_ip_callout` driver loaded; otherwise environment-variable fallback (`PULSAR_SEAT_SOURCE_IP`).
- architecture.md: console guard, host console preset, pad pass-through and in-session capture share one code path on x64 and ARM64; most are marked HYPOTHESIS (not run on hardware).
- No ARM64-only or x64-only branch exists in this part's Svelte/TS files; differences come only from service availability data.

### 6.4 Wizard and tool dialogs

- Experimental `termsrv_wrapper` exists for client editions (Pro, Enterprise) where Windows gives one session at a time; licensed alternative is Windows Server + RDSH + RDS CALs (warning text). On ARM64 the wrapper must be native ARM64, else it is not used.
- A Windows update that changes termsrv.dll to an unknown version makes the service turn the option off; seats fall back to fast user switching.
- Checks reporting platform: `edition` (client edition warning), `rdsh_role`, `arch_native` (package architecture vs PC), `os_build`, `secure_boot`, `memory_integrity`, `smart_app_control`, `driver_signing` (drivers not signed yet: user-mode methods), `windows_safe_mode`, `hyperv_vm`.
- Notes `two_admin_sessions_only` (Windows Server without RDSH), `rds_licensing_not_configured`.
- Failure `edition_unsupported`, `fsu_disabled`, `rds_not_ready`; error `session_unavailable`.
- Wizard: native Tauri windows per monitor (Windows WebView2); building webviews only from async commands (sync deadlocks on Windows). Browser/demo mode renders pages via URL parameters.
- No ARM64/x64 difference in wizard or tool dialogs found in code.

### 6.5 Remote seats

- **Windows edition / SKU** (host class): Home → multiseat not supported (danger; multiseat toggle locked); client Pro/Enterprise without wrapper → "Needs the
  experimental multi-session feature" / one remote session at a time; Server without RDSH → one trial seat; Server + RDSH → ready. Experimental wrapper only on
  client Pro/Enterprise (`not_client_sku`). Remote seats (non-VM) cannot start without concurrent sessions: session backend must be `rds_loopback` or
  `termsrv_wrapper` (else "Cannot start" on rows and start button disabled with reason). VM seats are exempt.
- **Hyper-V**: feature state `unavailable` on editions without Hyper-V ("not available on this Windows edition"); the seat page hides the VM section then
  (unless the seat already is a VM seat). Enable Hyper-V elsewhere (Advanced > Network addresses), restart follows. GPU partition list depends on host GPUs.
- **ARM64 vs x64**: process patching module per architecture; if none installed for this architecture: "No process patching module for this architecture is
  installed on this PC." and the switch is locked while off. On ARM64, x64 apps are patched only when Pulsar Seat starts them (isolation note). Wrapper must
  fit the PC architecture (`wrapper_arch`). WRDS provider DLL must be native (`wrds_dll_missing` "… or for another processor").
- **Drivers / backends**: display backends `vdd` works on physical screen only (signed); `pulsar_idd` unsigned trial; provider path needs RDSH (HYPOTHESIS per
  architecture doc); HDR depends on Windows version, IddCx version, driver, GPU; Windows Server needs WHQL driver for HDR. Pad backends ViGEmBus,
  usbip-win2, Pulsar pad (unsigned). Stream backends Sunshine / Pulsar may be missing (option disabled, "not installed"). Pulsar-build-only Sunshine
  features (pulsar_* gamepads, pulsar_idd capture) fall back with a standard build (`stream_fallback` warning).
- **Microphone**: Pulsar client only; Moonlight has none (`mic_needs_pulsar`). Pad audio: Pulsar streaming only.
- **Coexistence**: another remote multiseat product installed → mode `alongside` (nothing blocked), running → note `duo_service_running`. Legacy `passive`
  locks: Host multiseat toggle, experimental buttons, audio cleanup, settings switches, isolation lists, DXVK page, VM page.
- **Admin**: whole remote view needs elevation.

## 7. Accessibility and i18n

### 7.1 Global

- Rail: `<aside aria-label={t.nav.app}>` "Application"; two `<nav>` with `aria-label` `t.nav.sections` "Sections" and `t.nav.status`; current item `aria-current="page"`; counts have `aria-label` "N seats"; status dot `aria-hidden`, state as visually hidden text; links carry `title` (tooltip label for the narrow rail).
- Modal (`Modal.svelte`): native `<dialog>` with `showModal()` (inert page), `aria-modal`, `aria-labelledby` title, `aria-describedby` sub line; `role="alertdialog"` when `alert`; Tab / Shift+Tab focus trap; Escape closes (also the native cancel event); backdrop click closes non-alert dialogs only; initial focus: `first` field, `primary` (first foot button) or `cancel` (last foot button, used for danger confirms); focus returns to the opener on close; close button labelled `t.close`.
- ConfirmDialog: always alert dialog; danger -> focus on Cancel; optional "don't ask" checkbox; optional typed confirmation (locale-aware case-insensitive compare).
- Button: disabled = `aria-disabled` (stays focusable) + visible reason linked with `aria-describedby` (`disabledReason` or `reasonId`); busy also `aria-disabled` + spinner.
- SaveBar: `role="region"` `aria-label` `t.saveBar.region` "Unsaved changes"; Ctrl+Shift+S (`aria-keyshortcuts`), blocked while a dialog is open; when the bar disappears with focus inside, focus moves to the page `h1` (tabindex -1) or `main`.
- Toasts: container `aria-live="polite"`, each toast `role="status"`; pause on focus/hover.
- Notices: danger notice `role="alert"`; group is a `<details>` with `aria-label` `t.notices.region`.
- Status page: progress lines `role="status"`; Update dialog state row inside `role="status"`.
- Menu (`Menu.svelte`, APG menu button): Enter / Space / ArrowDown open on first item, ArrowUp on last; arrows move; Home / End; type-ahead by first letters (locale-aware); Enter / Space activate; ArrowRight opens submenu, ArrowLeft closes; Escape closes and returns focus to the trigger; rendered in the top layer (Popover API, fixed fallback).
- Segmented: `role="radiogroup"`, arrows / Home / End.
- Toc: `<nav aria-label={t.nav.onThisPage}>`, `aria-current="location"` on the section in view (IntersectionObserver), click moves focus to the section heading; smooth scroll unless reduced motion.
- Kbd: renders "A+B" as separate `<kbd>` elements joined by a hidden "+".
- `<html lang>` follows the active locale.
- i18n facts:
  - Two locales, same keys (`i18n.test.ts`, `bun run check`). 2392 string leaves each; 191 function-valued entries in EN (strings built from parts).
  - Pluralisation is hand-written per string with `n === 1` tests, e.g. `t.nav.count`, `t.saveBar.count`, `t.notices.count`, `t.power.summary`, `t.statusPage.audioBuffer`. No ICU/Intl plural rules.
  - Lists: `seatList` joins "2, 3 and 4" with `misc.listAnd`.
  - Dates: update "Last check" via `toLocaleString` with `tr-TR` / `en-GB`.
  - Composite labels: `seatLabel` ("<state>: <user>", "<state> (<phase>)", "<state> (<failure>[: <listener detail>])"), `errorCodeTag` ("<reason> · Windows <win32>"), error notice "<text> (<code>)", notice toasts "Seat N: <text>", `availabilityLabel`, `driverStatusLabel`, `refusalLabel`, `fallbackNoteLabel`, `issueLabel` ("seat N, <device>: <text>"), `reasonLabel`. Unknown codes fall through to the raw code.
  - Experimental warning texts in `warning.ts` are separate EN/TR arrays with `**bold**` markup, not in en.ts.
  - Native shell strings (tray, titles, file dialogs) are in `copy.rs`, en/tr pairs.
- String length statistics (computed with bun over all string leaves of `en.ts` / `tr.ts`, all namespaces; functions excluded):
  - Matching keys: 2391 pairs. Total TR/EN length ratio 1.035; median per-key ratio 1.010; 90th percentile 1.444; max 4.00. For short strings (EN ≤ 30 chars, 1356 pairs) the ratio is 1.096.
  - Short labels that grow most in Turkish: `rt.vm.forwards` 28 -> 57; `rt.seat.pulsarNodePortEphemeral` 13 -> 37; `t.needsSetup.console_sign_in` 29 -> 52; `rt.vm.hostPort` 9 -> 31; `t.kiosk.appId` 30 -> 47; `rt.seat.pulsarNodePortInvalid` 28 -> 44; `rt.patching.install.bullets[1]` 29 -> 45; `t.basic.colPadIgnore` 16 -> 31; `t.kiosk.account.console` 19 -> 34; `t.advanced.cpuOpen` 19 -> 34.

25 longest English strings:

| Key | Chars |
|---|---|
| `t.kiosk.limits` | 1361 |
| `t.kiosk.perSeatAccountsHint` | 666 |
| `t.kiosk.seatOneDesktopHint` | 660 |
| `t.advanced.padDeviceScopeHint` | 633 |
| `t.compat.steamPerSeatHint` | 401 |
| `t.kiosk.intro` | 358 |
| `rt.settings.isolationNote` | 343 |
| `t.cpu.seatWideHint` | 330 |
| `t.basic.virtualPadsHint` | 323 |
| `t.redirect.drivesHint` | 316 |
| `t.outputs.split.note` | 305 |
| `t.advanced.inputNotIsolatedNonUsb` | 293 |
| `t.compat.objectRulesHint` | 289 |
| `t.advanced.classDefaultsSoon` | 286 |
| `t.outputs.sessionFpsNote` | 280 |
| `t.advanced.loopbackHint` | 280 |
| `t.kiosk.systemKeys` | 279 |
| `t.compat.consoleLockHint` | 269 |
| `t.compat.usersShutdownHint` | 265 |
| `t.redirect.printersHint` | 253 |
| `t.compat.defenderHint` | 248 |
| `issueText.loopback_no_accounts` | 248 |
| `t.picture.userHint` | 247 |
| `t.outputs.hdrVrrHint` | 242 |
| `t.gpuPref.hint` | 241 |

25 longest Turkish strings:

| Key | Chars |
|---|---|
| `t.kiosk.limits` | 1476 |
| `t.kiosk.perSeatAccountsHint` | 756 |
| `t.advanced.padDeviceScopeHint` | 735 |
| `t.kiosk.seatOneDesktopHint` | 716 |
| `t.compat.steamPerSeatHint` | 439 |
| `t.kiosk.intro` | 401 |
| `rt.settings.isolationNote` | 393 |
| `t.basic.virtualPadsHint` | 370 |
| `t.cpu.seatWideHint` | 358 |
| `t.outputs.split.note` | 345 |
| `t.advanced.loopbackHint` | 327 |
| `t.redirect.drivesHint` | 320 |
| `t.compat.objectRulesHint` | 320 |
| `t.outputs.sessionFpsNote` | 302 |
| `t.advanced.classDefaultsSoon` | 299 |
| `t.compat.usersShutdownHint` | 291 |
| `t.redirect.printersHint` | 284 |
| `t.advanced.inputNotIsolatedNonUsb` | 283 |
| `t.advanced.restoreUsbHint` | 276 |
| `t.kiosk.systemKeys` | 274 |
| `t.compat.consoleLockHint` | 274 |
| `issueText.kiosk_app_execution_alias` | 267 |
| `t.compat.defenderHint` | 261 |
| `t.seatCompat.noPowerMenuHint` | 261 |
| `t.seatMenu.removeKioskUserBody` | 259 |

- Not counted above: the experimental warning (`warning.ts`), whose longest paragraph is longer than any en.ts string (EN second paragraph ~480 chars; whole EN text ~2400 chars, estimate from reading, not computed).

### 7.2 Local seats board

- Device rows: `role=button`, `tabindex=0`, `aria-haspopup=menu`, `aria-expanded`, aria-label `t.device.actions(name)` "<name>: move or details", `aria-describedby` = meta line. Keys: Enter, Space, ArrowDown, ContextMenu, Shift+F10 open the Move menu; right click too.
- Monitor faces: native button, aria-label `t.device.faceActions(name,id)`; ArrowDown/ContextMenu/Shift+F10 plus native Enter/Space.
- Status icons on the meta line have tooltips plus sr-only text (", Shared…", ", Not plugged in", ", Needs attention"). Device pictures are `aria-hidden`.
- Menu component (`ui/Menu.svelte`): ArrowUp/Down, Home/End, type-ahead, Enter/Space, ArrowRight opens submenu, ArrowLeft closes, Escape closes and returns focus to the opener. Menu items `role=menuitem`, separators `role=separator`, group labels presentation.
- Keyboard alternative to drag and drop: the Move menu (seat entries, To unassigned devices, Share with all seats); hint line `t.canvas.moveHint`.
- Polite live region announces moves: `t.move.announce`, `announcePool`, `announceShare`.
- Seat card `section` with aria-label = seat title; pool `section` aria-label `t.canvas.system`.
- "Found" state is a dashed ring distinct from the focus ring (WCAG 2.4.7 comment). Reduced motion: pulse and "New" highlight shown as static fills for their full time.
- Split picker: `role=group` labelled `t.outputs.split.groupFor(name)`; each select labelled `cellFor(name, cell)`. Outputs dialog option groups labelled `perMonitor` / `cloneOutputs` + monitor name.
- USB dock wizard: scan state in `aria-live=polite`; each dock `section` labelled by its heading.
- Camera dialogs: "starting" has `role=status`; videos muted without captions.
- i18n: English + Turkish (same keys in `tr.ts`). Strings built from parts: `t.power.summary(running, blocked)` (singular "1 local seat is" / plural), `t.usbDock.found(n)` and `contents(m,i,a)` use "(s)" instead of plural forms, `t.describe.seatsOf` singular/plural, `t.assign.seatOption(n,name)`, seat fact joined with " · ", remote row help joined with " · ", output label "<tech> · <id> (current)", the move hint is split around the word "Enter" to render a key cap (the Turkish string must contain "Enter" literally for that).

### 7.3 Settings

- Modals use a native `<dialog>` with focus trap, Escape to close, focus return to the opener (`ui/Modal.svelte`). `ConflictDialog` is an alert modal with initial focus on the primary button.
- Seat menu: WAI-ARIA menu button (Enter/Space/ArrowDown open on first item, ArrowUp on last, arrows, Home/End, type-ahead, Escape returns focus) (`ui/Menu.svelte`).
- Segmented control (theme, account type): arrow keys and Home/End (`ui/Segmented.svelte`).
- Save shortcut Ctrl+Shift+S (`ui/SaveBar.svelte`).
- Advanced contents list sets `aria-current="location"` on the section in view (`ui/Toc.svelte`).
- Explicit aria-labels: seat switches `autoStartFor(name)`, account button `accountFor(name)`, seat menu `seatMore(name)`, IP edit `ipEditFor(name)`, hotkey change `hotkeyChangeLabel`, numbered row inputs ("Program path 2"; plain label when only one row), row remove buttons `t.rowAction.remove(what)` (falls back to `t.rowAction.row(n)` "row n"), program enabled `t.rowAction.enabled(what)`, CPU boxes "Seat N, <index>", object rule groups `t.advancedPage.objectRuleFor(n)`, Hyper-V buttons "<action>: <seat name>", WU hour selects "Active hours: From/to".
- `aria-invalid` on invalid inputs (seat timeout, loopback port, extra accounts, startup/kiosk/GPU/env rows, IP program path, app id); error paragraphs use `role="alert"`; startup app errors are linked with `aria-describedby`.
- Hotkey invalid key announced through an sr-only `role="alert"`. Picker notes use `role="status"`.
- Picture tiles are `role="radio"` in a `role="radiogroup"` labelled by the group label.
- Checkbox groups have `role="group"` with aria-label (class defaults, deny classes).
- Disabled-with-reason pattern: "Add seat" shows its reason text (`disabledReason`); values that cannot be changed are plain text (`LockedValue`), not disabled controls.
- i18n: English and Turkish (same keys). Plurals by function: `t.basic.appsCount`, `t.gpuPref.count`, `t.kiosk.count`, `t.settingsPage.kioskFact`, `t.advancedPage.cpuLimited`, `t.network.programsCount`, `t.saveBar.count`; `restartTsDone(n)`. Strings built from parts: seat fact line joined with " · "; issue labels "seat N, device: text"; availability "<state>: <reason>"; session option label split from "Name (meaning)" into label + capitalised help sentence; select option "<name> (not available on this PC)"; hotkey label "Ctrl + <key>"; key names partly localised (`t.keyNames`); seat list "2., 3."; Turkish manual link chosen by the active language.

### 7.4 Wizard and tool dialogs

- Wizard: `role="dialog"`, `aria-modal`, `aria-label` = title; progress as ordered list with `aria-label` "Progress" and `aria-current="step"` on the current segment; key status areas `role="status"`; screen-reader-only status of generic step headings; key legend `role="group"` labelled "On any keyboard:".
- Start button uses `reasonId` to point at the blocked-notices block as its disabled reason. Extend switch labelled by its row (`labelledby`).
- Monitor "Choose" buttons have `aria-label` "Choose <name> as seat 1’s monitor".
- Keyboard: whole wizard drivable by keys (digits, Enter, Backspace, Shift+Enter, Shift+Backspace, Shift+Esc, Alt+F4); Escape closes the start screen. Keys ignored when `keys_seen`.
- Wizard page: ring numbers are `role="img"` with aria labels ("This monitor’s number: N", "Seat N"); status lines `role="status"`; click target button has `aria-label` "Click this target"; legend group labelled.
- ExperimentalDialog: modal `alert` (alert dialog), initial focus on Cancel, disabled reason text in footer linked by `reasonId`.
- ImportDialog: hidden file input (`tabindex=-1`, aria-label "Choose file") triggered by a visible button; textarea with `aria-describedby`/`aria-invalid` from the field (JSON error).
- Compare/Diagnostic: result notices in `role="status"`.
- i18n: strings built from parts: `keyHead`/`pageKeyHead` return [before, after] around an inline key cap; `seatStep(n,total,kind)`; `pickThisAria(name)`; `minResolution(w,h)`; `keyboardCount`/`mouseCount` "k / 2"; `issueLabel` joins "seat N, <device>: <text>"; `refusalLabel` "<channel>: <reason> (+N)"; `seatList` "2, 3 and 4"; `describe.seatsOf` singular/plural ("Seat 1" / "Seats 1, 3"). The experimental warning has separate EN and TR texts (`WARNING_EN`/`WARNING_TR`) chosen by active language; native copy EN/TR in `copy.rs`. Turkish locale `tr.ts` has the same keys.

### 7.5 Remote seats

- Seat page inputs carry `aria-label`; invalid fields set `aria-invalid="true"`; inline field errors have `role="alert"`; info lines `role="status"`.
- Settings block on the seat page uses `inert` while locked (not focusable/operable); DXVK "Add a game" block also `inert` when locked.
- Buttons with icon-only or ambiguous labels have explicit aria labels: copy (`rt.view.copy(a)`, `rt.seatView.copyAddress`), remove (`rt.view.removeItem(n)`),
  browse per app (`rt.view.browseFor(i)`), Test / Change password (`testLabel`, `changeLabel`; Change has `aria-expanded`), experimental on/off
  (`twEnableLabel`/`twDisableLabel`), cleanup, DXVK off per game, unpair per device (`unpairLabel(name)`), VM browse buttons ("Browse <field>").
- Disabled Start button exposes its reason (`disabledReason`); Pair "Send PIN" points at the footer reason text (`reasonId`); experimental dialog primary
  points at its footer reason.
- Tables (`ListTable`) have captions and column headers; remove column header is screen-reader-only.
- Pair dialog steps: ordered list with `aria-current="step"`; addresses list `aria-labelledby` its heading; waiting / request blocks `role="status"`; QR is
  `role="img"` with label `rt.pair.qr(text)`; OTP span has aria-label. Inline confirm group `role="group"` with label.
- Log `<pre>` is focusable (`tabindex=0`) with aria-label "Logs".
- Keyboard: Enter submits New seat name and PIN; save bar Ctrl+Shift+S; modals are native dialogs with focus trap and Escape; alert-type dialogs
  (unsaved changes, patching flow, credential confirm, experimental) start focus on Cancel. Patching flow cannot be closed during progress.
- Section-level `aria-label` "Seat list" (`rt.view.rootHelp`) on the remote home list.
- i18n: pluralisation by hand in `rt.home.pairedCount` (unused), `rt.seat.crashes`, `rt.host.graceDays`, `rt.vm.gpus`, `rt.host.audioCleanupDone` ("device(s)").
  Strings built from parts: `rt.view.stateSummary` joins non-empty parts with " · "; `rt.view.driverHelp` joins with ", " + "."; "Reason for failure: " +
  text; "Last error: " + detail; PIN-mode result; `modeApplied` + " · " + status; pad list "slot: kind (backend)"; log source "Seat: <name>". Initials
  upper-cased with tr-TR locale in Turkish. Credential failure title is Windows text in the system language (not UI language). Experimental warning text is
  the service's text (`warning.ts`; English and Turkish copies checked against the service files by `warning.test.ts`). Log level colouring matches English and
  Turkish words (ERROR/Hata, WARN/Uyarı). Some labels are untranslated literals: "Xbox 360", "DualShock 4", "DDX", "WGC", "Hz", "GB", "%", "x", "TCP", "UDP",
  "Hyper-V", "Direct3D 9/8", "Device" (fallback client name).

### 7.6 Facts that apply everywhere

- Two UI locales: English (default, `en.ts`) and Turkish (`tr.ts`), same key set (enforced by `i18n.test.ts`). Right-to-left is not needed.
- Turkish strings run on average about 3.5% longer than English, with a long tail (90th percentile about 1.44x; see 7.1 for the longest strings). Turkish casing rules apply (i/İ, ı/I).
- Many strings are functions of counts, names or paths (see 7.1 for how they are formatted).

## 8. Coverage check

Every `.svelte` file under `apps/manager/src` and the section that covers it. Section numbers refer to the nearest numbered heading.

| File | Covered in |
| --- | --- |
| `lib/components/AdvancedTab.svelte` | 3.3 Advanced page (`AdvancedTab.svelte`) |
| `lib/components/App.svelte` | 3.1 App shell (`App.svelte`, `+page.svelte`); 3.1 Service-down screen (`App.svelte`); 3.1 Enable / disable dialog (`App.svelte`, kind `enable`); 3.1 Reboot dialog (`App.svelte`, kind `reboot`); 3.1 Start seats confirm (`App.svelte`, kind `start`); 3.1 Service stop / restart confirm (`App.svelte`, kind `service`); 3.1 Unsaved-changes dialog (`App.svelte`, kind `unsaved`); 3.1 Quit dialog (`App.svelte`, kind `quit`); 3.1 Reset dialog (`App.svelte`, kinds `reset1`, `reset2`); 3.1 Input-move dialog (`App.svelte`); 3.1 Same-account dialog (`App.svelte`); 3.1 Read-only / not-admin banner and restricted mode (`App.svelte`); 3.1 Changed-elsewhere notice (`App.svelte`); 3.1 OS-build-changed notice (`App.svelte`); 3.1 First run (`App.svelte` comment; UI in `SeatsTab.svelte`, other part); 3.2 Keyboard and mouse moved dialog (`App.svelte`, input-move confirm); 3.2 Same account dialog (`App.svelte`, same-account confirm) |
| `lib/components/AssignDialog.svelte` | 3.2 Assign dialog (`AssignDialog.svelte`) |
| `lib/components/BasicTab.svelte` | 3.3 Settings page (`BasicTab.svelte`) |
| `lib/components/CameraGrid.svelte` | 3.2 Camera grid (`CameraGrid.svelte`) |
| `lib/components/CameraPreview.svelte` | 3.2 Camera preview (`CameraPreview.svelte`) |
| `lib/components/CompareDialog.svelte` | 3.4 Compare with saved configuration (`CompareDialog.svelte`) |
| `lib/components/CompatSettings.svelte` | 3.3 Optional system settings and Apps on seats (`CompatSettings.svelte`) |
| `lib/components/ConflictDialog.svelte` | 3.3 Save-time conflicts dialog (`ConflictDialog.svelte`) |
| `lib/components/CpuSetsDialog.svelte` | 3.3 Processor cores dialog (`CpuSetsDialog.svelte`) |
| `lib/components/DeviceIcon.svelte` | 3.2 Device picture (`DeviceIcon.svelte`) |
| `lib/components/DeviceInfoDialog.svelte` | 3.2 Device info dialog (`DeviceInfoDialog.svelte`) |
| `lib/components/DeviceRow.svelte` | 3.2 Device row (`DeviceRow.svelte`) |
| `lib/components/DiagnosticDialog.svelte` | 3.4 Diagnostic report (`DiagnosticDialog.svelte`) |
| `lib/components/ExperimentalDialog.svelte` | 3.4 Experimental session method dialog (`ExperimentalDialog.svelte`); 3.5 Experimental dialog, remote variant (`../components/ExperimentalDialog.svelte`, variant `remote`; test in `remote/components/ExperimentalDialog.test.ts`) |
| `lib/components/GpuPreferenceDialog.svelte` | 3.3 GPU preference dialog (`GpuPreferenceDialog.svelte`) |
| `lib/components/HotkeyDialog.svelte` | 3.3 Keyboard/mouse switch shortcut dialog (`HotkeyDialog.svelte`) |
| `lib/components/ImportDialog.svelte` | 3.4 Import another configuration (`ImportDialog.svelte`) |
| `lib/components/IpBindingDialog.svelte` | 3.3 IP address dialog (`IpBindingDialog.svelte`) |
| `lib/components/KioskShellDialog.svelte` | 3.3 Kiosk seat dialog (`KioskShellDialog.svelte`) |
| `lib/components/MissingSlot.svelte` | 3.2 Missing-device line (`MissingSlot.svelte`) |
| `lib/components/MonitorFace.svelte` | 3.2 Monitor face (`MonitorFace.svelte`) |
| `lib/components/NetworkSection.svelte` | 3.3 Network addresses section (`NetworkSection.svelte`) |
| `lib/components/OutputsDialog.svelte` | 3.2 Display outputs dialog (`OutputsDialog.svelte`) |
| `lib/components/PadIgnoreDialog.svelte` | 3.3 Gamepads that SDL games ignore (`PadIgnoreDialog.svelte`) |
| `lib/components/PictureDialog.svelte` | 3.3 Device picture dialog (`PictureDialog.svelte`) |
| `lib/components/Pool.svelte` | 3.2 Unassigned devices pool (`Pool.svelte`) |
| `lib/components/Rail.svelte` | 3.1 Rail (`Rail.svelte`) |
| `lib/components/RedirectDialog.svelte` | 3.3 Redirection dialog (`RedirectDialog.svelte`) |
| `lib/components/SeatAccountDialog.svelte` | 3.3 Seat sign-in account dialog (`SeatAccountDialog.svelte`) |
| `lib/components/SeatCard.svelte` | 3.2 Seat card (`SeatCard.svelte`) |
| `lib/components/SeatCompatDialog.svelte` | 3.3 Environment and sign-in dialog (`SeatCompatDialog.svelte`) |
| `lib/components/SeatNameDialog.svelte` | 3.3 Seat name dialog (`SeatNameDialog.svelte`) |
| `lib/components/SeatsTab.svelte` | 3.1 First run (`App.svelte` comment; UI in `SeatsTab.svelte`, other part); 3.2 Seats board (`SeatsTab.svelte`) |
| `lib/components/ServiceActions.svelte` | 3.1 Service actions (`ServiceActions.svelte`) |
| `lib/components/SplitPicker.svelte` | 3.2 Split screen picker (`SplitPicker.svelte`) |
| `lib/components/StartupAppsDialog.svelte` | 3.3 Startup programs dialog (`StartupAppsDialog.svelte`) |
| `lib/components/StatusTab.svelte` | 3.1 Status page (`StatusTab.svelte`) |
| `lib/components/UpdateDialog.svelte` | 3.1 Update dialog (`UpdateDialog.svelte`) |
| `lib/components/UsbDockWizard.svelte` | 3.2 USB dock wizard (`UsbDockWizard.svelte`) |
| `lib/components/ViewSettingsDialog.svelte` | 3.2 View settings dialog (`ViewSettingsDialog.svelte`) |
| `lib/components/Wizard.svelte` | 3.4 Setup wizard, main window (`Wizard.svelte`) |
| `lib/components/WizardPage.svelte` | 3.4 Setup wizard, per-monitor page (`WizardPage.svelte`) |
| `lib/remote/components/AppsEditor.svelte` | 3.5 Client app list editor (`AppsEditor.svelte`) |
| `lib/remote/components/CredentialTestDialog.svelte` | 3.5 Credential test dialog (`CredentialTestDialog.svelte`) |
| `lib/remote/components/DxvkPage.svelte` | 3.5 DXVK games page (`DxvkPage.svelte`) |
| `lib/remote/components/HostPage.svelte` | 3.5 Host PC page (`HostPage.svelte`) |
| `lib/remote/components/LogPage.svelte` | 3.5 Log page (`LogPage.svelte`) |
| `lib/remote/components/NewSeatDialog.svelte` | 3.5 New remote seat dialog (`NewSeatDialog.svelte`) |
| `lib/remote/components/PairDialog.svelte` | 3.5 Pair dialog (`PairDialog.svelte`, `QrCode.svelte`) |
| `lib/remote/components/PatchingFlow.svelte` | 3.5 Patching flow (`PatchingFlow.svelte`) |
| `lib/remote/components/QrCode.svelte` | 3.5 Pair dialog (`PairDialog.svelte`, `QrCode.svelte`) |
| `lib/remote/components/RemoteTab.svelte` | 3.5 Remote seats home / seat list (`RemoteTab.svelte`); 3.5 Unsaved changes dialog (remote) (`RemoteTab.svelte`); 3.5 Delete seat confirmation (`RemoteTab.svelte`, ConfirmDialog) |
| `lib/remote/components/SeatPage.svelte` | 3.5 Remote seat page (`SeatPage.svelte`) |
| `lib/remote/components/SettingsPage.svelte` | 3.5 Remote seat settings page (`SettingsPage.svelte`) |
| `lib/remote/components/StageBar.svelte` | 3.5 Stage bar (`StageBar.svelte`) |
| `lib/remote/components/VmPage.svelte` | 3.5 VM seats page (`VmPage.svelte`) |
| `lib/remote/components/VmSeatEditor.svelte` | 3.5 VM seat editor (`VmSeatEditor.svelte`) |
| `lib/test/ui/KitHost.svelte` | test fixture only (not part of the app) |
| `lib/test/ui/MenuHost.svelte` | test fixture only (not part of the app) |
| `lib/test/ui/ModalHost.svelte` | test fixture only (not part of the app) |
| `lib/test/ui/NoticeHostFixture.svelte` | test fixture only (not part of the app) |
| `lib/ui/Avatar.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Breadcrumb.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Button.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Checkbox.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/ChoiceList.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/ConfirmDialog.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Disclosure.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/EmptyState.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Field.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Group.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Icon.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/IconButton.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Kbd.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/LinkRow.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/ListTable.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/LockedValue.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Mark.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Menu.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Modal.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Notice.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Notices.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/PageHead.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/PulseRings.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Row.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/SaveBar.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Section.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Segmented.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Select.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Spinner.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Status.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/StatusLine.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/SubLabel.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Switch.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Toasts.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `lib/ui/Toc.svelte` | 2 (shared UI primitive; behaviour described under Notifications / Save model / Keyboard shortcuts where relevant) and 7 |
| `routes/+layout.svelte` | 2 (Entry, routes and windows); 3.1 (App shell) |
| `routes/+page.svelte` | 2 (Entry, routes and windows); 3.1 (App shell) |
| `routes/ui/+page.svelte` | 2 (Entry, routes and windows): developer UI-kit gallery, not a user screen |

## Appendix: open questions (unclear in code)

### Global

- Default of `settings.updates.check` / `weekly` (service config default) is not in my files.
- `reset2` dialog (second reset confirmation) is defined but never opened.
- `ServiceActions.svelte` is not used by any production view (only its test); Status page duplicates its logic. Its restart confirm is danger; the Status page's own restart confirm is not.
- Several en.ts keys in the assigned namespaces appear unused by code: `t.linkDown`, `t.tabs.*`, `t.enable.offerEnable/offerEnableButton`, `t.saveBar.restarts/nextBoot/changed/shortcut`, `t.notices.compare`, `t.statusTab.seats/fallback/version/managerVersion/arch/session/since/degraded`, `rt.nav.startService/restartService/stopService`, `t.update.bar/page`, `t.wizard.firstRun`. `t.saved.restartSeats` is used only in a test.
- README describes a "Bottom bar" with enable/disable, start, reboot, diagnostic buttons and a header `ServiceControl.svelte`; current code has neither (redesign moved these to the Seats status line/overflow and the Status page).
- Service-down screen does not distinguish "not installed" from "stopped"; Start stays enabled (if elevated) when the service is not installed.
- The tray "Turn Pulsar Seat on / off" item label does not change with state; whether the tray reflects state otherwise is not in code.
- Whether the OS-build-changed flag is ever cleared by the service is not in my files.

### Local seats board

- Whether a non-admin can stage draft edits from this board (move, rename, toggles are not admin-gated here; saving is gated elsewhere).
- What the hardware does for "Show (identify)" of a speaker (no test tone code in the manager), and whether the service ends identify by itself after a time.
- Test tone and microphone level meter: no code found in the manager; they do not exist today.
- USB port capture / WinUSB "held" state: only the Advanced tab restore button and the note `usb_capture_held` exist; the Seats board shows no held state.
- English texts for device `warnings` codes (key_collision, foreign_filter, hidden_by_other_software, not_routable) are not rendered by these files; only the generic "Needs attention" icon.
- `t.advanced.restoreUsbFailed(n)` English string does not include the number `n` (starts with a space: " device(s) could not be given back now…"); looks like a missing `${n}`.
- Demo mode behaviour of this page (demo.ts exists; no demo branch in these files).
- Seat 1 "Restart the graphics driver" is enabled even when seat 1 runtime is not up; intent unclear.
- `t.view.scaleIcons` / `t.view.evenRows`, `t.canvas.emptySeat`, `dropHere`, `hubDepth`, `justConnected`, `sharedSign`, `t.deviceMenu.spare/showIds/hideIds/indicate`, `t.systemMenu.reset/import/hotkey/cpu/clearPictures/logLevel/outputs/panels`, `t.health` keys exist in en.ts but are not used by these files (leftovers or used elsewhere).
- The "Processor cores…" seat menu entry opens the machine-wide CPU sets dialog, not one pre-filtered to the seat (`commands.cpu` takes no seat).

### Settings

- Meaning of `seats[].wallpaper` ("seat wallpaper") beyond the menu label: unclear in code.
- Meaning of the touchpad palm-check values 0..4: unclear in code.
- `local.kiosk` (legacy single kiosk app): not edited by any file in this part; unclear whether any UI still writes it.
- Timing inputs accept any finite value ≥0 on change; only `seat_start_timeout_s`, the ms upper bounds and `monitor_idle_off_s` are validated client-side. Upper bound of `audio_wait_timeout_s` and `crash_loop_limit` (input max 600 / 20) is not checked in the manager; service validation: unclear in code.
- Windows Update day inputs store `Number(value)` without rounding; an emptied field gives 0, a decimal gives an invalid policy (shown by `wuInvalid`).
- Startup programs: no maximum count in the manager; service limit unclear.
- Kiosk monitor number: the dialog does not show an error outside 1..16; the value is clamped on save.
- Severity of `cpu_unknown_id`, `cpu_no_core`, `cpu_only_efficiency_cores` (service-side only).
- Exact time when most system changes take effect after Save (service sync timing) beyond the en.ts hints; the manager only marks seat restarts for monitor and frame-source changes.
- `host_console.reason` codes are not displayed in the Advanced page; their full list is not in this part's files.
- Demo mode behaviour of these screens: no demo branch in these files; `demo.ts` not inspected.
- `pad_device_scope` default: seat-core default is `false`, while the types.ts comment says "Absent (older service) = on"; the manager reads absent as off (`?? false`).

### Wizard, tools, codes

- `t.wizard.firstRun`, `t.wizard.extend`, `t.wizard.gather`, `t.wizard.pickMonitor`, `t.wizard.lastUnit`, `t.wizard.keys`, top-level `t.wizard.all_seats|keep_seat_one|quick_two_seats|host_console`, `t.diagnostics.maskHelp`, `t.systemMenu.import` have no consumer in code; whether a first-run prompt exists is unclear in code (App comment says the first-run offer is the seats board's "Add a second seat" block).
- Error-severity validation issues: the only visible texts are the toast "Not saved: fix the problems below first." and device "attention" markers; no list rendering `issueLabel` for errors/warnings was found outside ConflictDialog and the wizard (other parts may show field-level text).
- Which `ProtoError.code` the service uses for wizard refusals `monitor_not_picked`, `seat_out_of_range`, `not_input_device` (demo uses `invalid_value` + reason `unknown_monitor`); these reasons have no en.ts text.
- Access level of `diagnostic_report`, `compare_saved`, `import_reference` on the service (demo requires admin for report and import; the compare menu entry is admin-only).
- `Check.detail` is not rendered in the Status check row; unclear in code whether shown anywhere.
- `failureText` `no_account` / `account_unresolved` are not in the `FailureReason` type; where the service emits them is unclear in code.
- `t.errorJson` text (Import parse error) is in another namespace, not read here.
- Exact severity for `invalid_screen_area` / `screen_area_overlap` / `console_share_not_half` taken from `validate.rs` line using `Severity::Error` with a code variable (assumed all three).
- Wizard behaviour is marked "Untested on hardware" in code; the control-key modifier timing is marked HYPOTHESIS.

### Remote seats

- `rt.phase` has no label for the fallback phase word `connect` (`model.ts` maps unknown start phases to `connect`); the raw word would show. Unclear in code whether this can happen.
- Coexistence modes `alongside` and `residue_only` have no dedicated UI text; only findings and notes are shown. `passive` handling remains though the service no longer sends it.
- VM start refusal reasons (`hyperv_unavailable`, `vm_network_missing`, `vm_base_missing`, `vm_not_assigned`, `vm_gpu_missing`, `session_unavailable`, `wrong_seat_kind`) have no `rt.failure` / `remoteErrorText` entry; which text the UI shows for them is unclear in code (likely the generic "An unexpected error occurred." or a text from the local-seat error tables via `api.startSeats`).
- Many `rt` keys are defined but not used by the remote components (e.g. `rt.nav.startService/restartService/stopService`, `rt.home.pairedCount`, `rt.view.noRuntime`, `rt.view.patchingTitle`, `rt.view.dxvkRelease`, `rt.view.vmIntro`, `rt.view.clientsSection`, `rt.view.leaveTitle`, `rt.pair.step3/step4/pending/qrHint`, `rt.host.audioCleanupRun/noWarnings/remoteCapable`, `rt.vm.saveSeat/saveSettings/saved/feature`, `rt.seat.portHint/passwordSet/savePassword/lockedHint/saveApps/appsSaved/identity/pulsarOptions/audioSection/streamSection`, `rt.serviceState.*`, `rt.settings.startWithWindows/windows/log`). Unclear whether any other area uses them.
- `SettingsPage` console-mode notice can never show (remote status always sets `console_mode: false`).
- `vmHost` (Hyper-V status) and stream backends are loaded once per RemoteTab mount; whether they ever refresh during a session (other than VM disk reset) is unclear in code — appears not.
- Minimum / maximum limits on number inputs (refresh 15-500, min FPS 1-240, nits, VM processors 0-64, image index 1-64, live mode sizes) are HTML attributes only; the client does not block saving out-of-range values; the service's exact ranges for these are not in this folder.
- Whether "Test" (credentials) uses the saved password only or a newly typed, unsaved one: code uses the stored password only (staged new password is not tested) — consistent with `api.ts`, noted here for clarity.
- Pairing `PendingPair.label`, `expires_in_s`, `received_at` and `PairState.last_result` are received but not displayed.

