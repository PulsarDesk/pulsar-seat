Design the complete user interface of **Pulsar Seat Manager**, a Windows desktop app. All visual and interaction design decisions are yours: layout, navigation model, information architecture, visual style, typography, colour, density, motion, iconography and wording tone. This brief only defines what the app must let people do. It deliberately gives no design direction. The current app's look is not a reference; do not try to match it.

Attached: `manager-functional-spec.md` is the complete functional inventory of the current app: every screen, dialog, field, allowed value, limit, default, action, state and message code. It is authoritative for behaviour. Where this brief and the spec disagree, the spec wins. Also attached: `icon.png`, the app icon.

## 1. The product

Pulsar Seat turns one Windows PC (x64 or ARM64) into several independent workplaces called **seats**. Each seat runs its own Windows session with its own user.

- **Local seat:** a person sits at the PC with their own monitor(s), keyboard, mouse, gamepad, audio device and USB ports, all plugged into the same PC.
- **Remote seat:** a person uses a seat from another device (TV, phone, tablet, another computer) over the network. The PC creates a virtual display for the seat and streams it with Pulsar or Moonlight.
- **Seat 1** is the PC's normal console session (the owner's own desktop).
- Part of the Pulsar family (a free, open-source remote desktop and game streaming app). Free and open source (GPL-3).

**Users of the manager:** only the PC owner/administrator. They set up seats, assign hardware, change settings, pair streaming clients, start and stop seats, read status and diagnostics. People using a seat never open the manager.

**Typical tasks, most frequent first:**
1. See at a glance which seats exist, which run, who is signed in, and whether anything needs attention.
2. Start / stop / restart seats; turn the whole product on or off.
3. Assign hardware (monitors, keyboards, mice, gamepads, audio, cameras, USB ports) to seats by dragging or picking, and identify which physical device is which.
4. First-time setup with a guided wizard (each monitor becomes a seat; press a key / click on each seat's keyboard and mouse to identify them).
5. Create a remote seat, pair a client device (PIN or QR code), start it.
6. Change per-seat options (name, sign-in account, startup programs, kiosk mode, redirection, IP binding, CPU cores, GPU preference, environment) and machine-wide settings.
7. Diagnose problems: status page, pre-flight checks, logs, diagnostic report, compare/import configuration.

## 2. Platform and technical constraints (facts, not style)

- Windows 10/11 desktop app (Tauri 2 window with a web UI in SvelteKit). Initial window 1000×700, minimum 720×520; must work from that minimum up to large monitors. It also runs inside a plain browser in a demo mode with fake data.
- Themes: light, dark, and "follow Windows" (per-user choice).
- Languages: English and Turkish, switchable in Settings (plus "Windows display language"). Every string in the design must exist in both. Turkish is ~4 % longer on average and up to ~45 % longer for 1 in 10 strings; the spec (section 7.1) lists the 25 longest strings per language. No right-to-left support needed.
- Fully operable by keyboard; Escape closes the topmost dialog; Ctrl+Shift+S saves. Screen-reader labels are required for every control, icon-only button and status indicator.
- Elevation: the app starts unelevated and is then **read-only** (a banner explains, with a "Restart as administrator" action). Editing needs an elevated administrator. An admin can also lock the manager so only admins can even view settings ("restricted").
- System tray: the app keeps running in the tray when the window closes. Tray menu: Show the window, Start seats, Turn Pulsar Seat on/off, Quit. The first hide shows a one-time hint. The tray icon can be hidden in Settings.
- The manager talks to a background Windows service. Seat state, devices and hot-plug events update live.
- Several displays at once: during the setup wizard, a separate full-screen page appears on **every** monitor (number overlay plus instructions) in addition to the main window.

## 3. Logos (keep exactly as they are, do not redesign)

**a) App icon** (exe, taskbar, tray): two rounded vertical bars side by side, left bar `#3A5BD9`, right bar `#7D97FF`; on a 128 px canvas each bar is about 53×112 px with a ~6 px gap. The attached `icon.png` is the exact artwork. Approximation:

```svg
<svg viewBox="0 0 128 128" xmlns="http://www.w3.org/2000/svg">
  <rect x="8" y="8" width="53" height="112" rx="22" fill="#3A5BD9"/>
  <rect x="67" y="8" width="53" height="112" rx="22" fill="#7D97FF"/>
</svg>
```

**b) In-app brand mark** ("Pulsar" pulse rings): four concentric circles that fade outward. Colour on light backgrounds `oklch(0.555 0.205 272)`, on dark backgrounds `oklch(0.74 0.15 272)`. Exact artwork:

```svg
<svg viewBox="0 0 52 52" fill="none" xmlns="http://www.w3.org/2000/svg">
  <circle cx="26" cy="26" r="24" stroke="currentColor" stroke-width="1.5" opacity=".2"/>
  <circle cx="26" cy="26" r="16" stroke="currentColor" stroke-width="1.75" opacity=".45"/>
  <circle cx="26" cy="26" r="8.5" stroke="currentColor" stroke-width="2"/>
  <circle cx="26" cy="26" r="3.5" fill="currentColor"/>
</svg>
```

Product names: "Pulsar Seat" (product), "Pulsar Seat Manager" / "Pulsar Seat Yönetici" (window title). Do not invent other names or taglines for the product.

## 4. Global behaviour every screen must account for

- **Service states:** not installed, stopped, starting/stopping/restarting (in progress), running, running but the live link is reconnecting, unreachable, version mismatch. While the service is unavailable, only the Status area works; everything else shows a service-down state with start/repair guidance.
- **Product state:** Off; On with seats not started; On with seats running; Safe mode (seat 1 only, with a reason: Windows safe mode, unclean shutdown, crash loop, etc.). Turning on/off and starting seats go through confirmation dialogs.
- **Save model:** most edits are staged in a draft and saved together (a pending-changes indicator with a count, Save and Discard; a guard asks "save / discard / cancel" when leaving with unsaved changes; a conflict dialog when saving clashes; a notice when settings changed in another window; validation errors block saving and are shown next to the cause). Some things apply immediately (start/stop, enable/disable, identify, update opt-in, remote seat actions; the remote area has its own staged draft and save). Some saves require a seat restart, a service restart or a PC reboot, which must be visible.
- **Messages:** persistent notices (link lost, safe mode, restricted, admin needed, reboot pending, Windows was updated, changed elsewhere, every error with its code; several notices collapse into a group) and short-lived confirmations. Errors never disappear on their own. The spec's section 4.5 lists every issue, check, note, refusal and error code with its meaning and severity; each needs a place in the design.
- **Disabled-with-reason:** many controls are unavailable for a stated reason (not admin, seat running, backend does not support it, ARM64, Windows edition). The reason must always be discoverable.
- **Live updates:** seat states, signed-in users, device hot-plug ("just connected" marking), identify (flash a device's seat / make a monitor show its number), camera previews, update download progress, wizard progress.

## 5. Areas, screens and dialogs to design

The current app has these areas. You may restructure navigation and merge or split screens, but every function below must remain reachable. Each item is specified in detail in the spec (section 3 and the coverage table in section 8).

**5.1 App shell:** navigation between Seats, Remote seats, Settings (with an Advanced sub-area) and Status; service status indicator; seat counts; notices; pending-changes save; read-only/restricted banners; service-down screen; changed-elsewhere and Windows-updated notices; first-run state (no seats yet → offer the wizard).

**5.2 Seats (local seats board):**
- Product power status line and actions (turn on and start, start, stop, turn off), "Add seat", overflow menu (setup wizard, reset, import, compare, diagnostics, view settings…).
- Unassigned devices pool.
- One card per local seat: title, status, fact line (user, state), seat menu, Restart when required, its monitors (shown as monitor faces in their real arrangement), its devices (keyboard, mouse, pad, audio, camera, USB port/hub tree), missing-device lines, password-failing line.
- Moving devices between seats and the pool by drag and drop and by a per-device "Move to…" menu; a confirmation when the seat's only keyboard/mouse moves.
- Remote seats listed as link rows with a shortcut into the Remote area.
- Invitation to add a second seat when only one exists.
- Dialogs: Assign device, Device info, Display outputs (resolution, refresh, HDR, VRR, software cursor), Split screen picker (up to 4 seats on one monitor: halves/quarters), Camera preview / camera grid, USB dock wizard (assign a whole USB hub/dock to a seat), View settings (board display preferences, "just connected" duration), Input moved, Same account warning.

**5.3 Settings:**
- Start mode (by hand / when Windows starts / when someone signs in on seat 1).
- Per seat: auto-start, sign-in account and password (+ credential test), seat menu with: name, startup programs, kiosk seat (apps per monitor, idle reset), redirection (clipboard, drives, smart cards, printers), IP address binding, processor cores, GPU preference, gamepads that SDL games ignore, environment variables and sign-in options.
- Behaviour switches, appearance (theme, language, tray icon), maintenance (updates, advanced settings link), help links, reset actions (two-step).
- Dialogs: Seat name, Seat sign-in account, Startup programs, Kiosk seat, Redirection, IP address, Processor cores, GPU preference, Pad ignore list, Environment and sign-in, Update dialog (check, download, install, progress, signature/hash failures).

**5.4 Advanced settings:** methods (session, display, input, audio backends, each with availability, notes and an experimental client-SKU option with its own warning dialog requiring explicit acknowledgement), timings, safety (crash-loop limit, audio wait…), devices (keyboard/mouse switch hotkey, device pictures, USB input capture incl. emergency restore), network addresses per seat (program list), passwordless-seat port, optional system settings (journaled Windows tweaks, each revertible), apps on seats (compatibility options), console guard / dedicated host console options, save-time conflicts.

**5.5 Status:** service state with start/stop/restart; product and seat run states; methods in use with fallbacks and "every method on this PC"; limitations; Pulsar drivers; pre-flight checks (run, results with severity and technical detail); diagnostic report entry.

**5.6 Setup wizard:** full-window flow in the main window plus one page on every monitor. Start screen (preconditions, what will happen), then per monitor: decide whether the monitor becomes a seat, press a key on that seat's keyboard, click with that seat's mouse; progress across monitors; finish/cancel without saving; error and timeout states.

**5.7 Tool dialogs:** Compare with saved configuration (diff), Import another configuration, Diagnostic report (create, save, what it contains), Experimental session method dialog (local and remote variants).

**5.8 Remote seats:**
- Home: list of remote seats (state, client, avatar), "New remote seat", links to "This PC" sub-pages; new seat dialog; delete confirmation; unsaved-changes dialog.
- Remote seat page: state and start/stop, start phases, streaming client, Windows session, crash count; pairing (PIN and QR code, paired clients); connection, session/user (+ credential test), stream server (Sunshine/Pulsar), display (resolution, refresh, HDR, live mode), sound, client app list editor, virtual machine seat editor (Hyper-V), removal. Settings are locked while the seat runs.
- Host PC page: Windows edition and multiseat readiness, Remote Desktop state, licensing, warnings, coexistence with other multiseat/streaming software (incl. "running alongside" and "the other product's service is running" warnings), an operator-installed external session wrapper (detected, "use for remote seats" with acknowledgement, "stop using", failing check), virtual display and gamepad drivers, remote display protocol provider, stale audio endpoint cleanup.
- Remote seat settings: isolation (HID isolation, process patching with an install flow, Steam split, module app lists), gamepads, virtual display method, stream server log level.
- Logs page, DXVK games page, VM seats page, stage bar (the remote area's own save).

## 6. Platform variants to show

- ARM64 vs x64 (some options are x64-only or opt-in on ARM64; the spec section 6 lists them).
- Windows Home / Pro / Server: which session methods are available, what is experimental, what is greyed out and why.
- Non-admin (read-only), restricted, demo mode.

## 7. What to deliver

1. The information architecture / navigation you choose, with a short rationale.
2. Every screen and dialog in section 5, each in its relevant states: default, empty, loading, error, disabled-with-reason, read-only (non-admin), service down, and with live changes (a seat starting, a device plugged in).
3. Both themes (light and dark) and both languages; at least show the longest Turkish strings in place.
4. The wizard's per-monitor page and the main-window wizard together.
5. A component inventory (every reusable part, with its states and keyboard behaviour) so it can be implemented consistently in SvelteKit.
6. Exact copy for every new or changed string, in English and Turkish.

## 8. Rules about scope

- Do not add features, settings or screens that are not in this brief or the spec. Do not remove any. If something seems missing, unclear or contradictory, do not design around it: list it as a question at the end of your answer. The spec's appendix "open questions" already lists known gaps; note your choice for each one that affects the design.
- Strings currently in the code but unused (listed in the spec) are not features; ignore them.
- Keep both logos exactly as given.
