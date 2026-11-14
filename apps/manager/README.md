# Pulsar Seat manager

Tauri 2 + SvelteKit + Svelte 5 (runes), `adapter-static`, Turkish UI. Runs as invoker in
every session. It does no privileged work itself: every change is a request to the
service over the control pipe (`docs/agents/architecture.md` §5). Behaviour target:
`spec/local/10-manager.md` (local seats) and `spec/remote/` (remote seats). Since merge step 9
it is the one manager for both seat kinds (`docs/agents/architecture.md` §10.7). The layout is our own and neutral; the maintainer redesigns it later.

## Commands

```
bun install
bun run test     # vitest: model, store, dialogs, App flows (demo service)
bun run check    # svelte-check
bun run build    # static build into build/ (needed before cargo builds the app)
bun run dev      # browser preview on :5175 with the in-memory demo service (no Tauri)
bun tauri build  # full app (frontend + Rust)
```

Rust side, from the repo root (private target dir for parallel agents):

```
set CARGO_TARGET_DIR=target\agent-manager
cargo build  -p pulsar-seat-manager
cargo clippy -p pulsar-seat-manager --all-targets -- -D warnings
cargo check  -p pulsar-seat-manager --target x86_64-pc-windows-msvc
```

## Layout

| Path | What |
| --- | --- |
| `src-tauri/src/pipe_client.rs` | One connection per request, `hello` first; opens the pipe with `SECURITY_IDENTIFICATION` QoS; fails closed unless the pipe server is in session 0 (`GetNamedPipeServerSessionId`) and its PID is the SCM's service PID (`QueryServiceStatusEx`); `subscribe_loop` keeps one event connection with back-off and skips frames it cannot decode |
| `src-tauri/src/shell.rs` | Fixed list of Windows panels (`ShellExecuteExW`), UAC relaunch (`runas`, `--elevated=<pid>`) |
| `src-tauri/src/lib.rs` | Tauri commands, main window (built in code: an elevated copy gets its own WebView2 data folder `<app_local_data>/elevated`), tray icon + menu, close-to-tray, `--autostart` (hidden start), wait for the unelevated parent PID before the single-instance check, event thread |
| `src/lib/types.ts` | TS mirror of the seat-proto / seat-core wire types |
| `src/lib/model.ts` | Pure config edits, effective masks (mirror of `seat_core::assign`), canvas grouping, seat colours, validation mirror |
| `src/lib/store.svelte.ts` | Window state: service view + unsaved draft, event handling, save flow |
| `src/lib/api.ts` / `demo.ts` | Tauri API / in-memory demo service with the same interface |
| `src/lib/i18n.ts`, `warning.ts` | All Turkish copy; the experimental-backend warning (version 1) |
| `src/lib/components/` | App shell, tabs (Seats, Basic, Advanced, Network, Status), dialogs, wizard |
| `src/lib/remote/` | Remote seats tab (from Pulsar Split's manager): `components/RemoteTab.svelte` + seat page, pair dialog (QR), host, logs, DXVK, settings; `api.ts` (protocol 3 remote requests), `model.ts` (record / status / event mapping), `types.ts` (remote payloads), `demo.ts`, `remote.css` (scoped to `.remote-ui`) |
| `src-tauri/src/net.rs` | Static IPv4 list (address dialog) and the LAN address order of the pair dialog (`lan_addresses`) |

## Tauri surface

Commands: `service_request(body)`, `service_hello`, `app_info`, `open_system_panel(panel)`,
`restart_elevated`, `set_tray_visible(visible)`, `quit_app`.
Events: `service-event` (`EventFrame`), `service-link` (bool), `tray-action`
(`{action: show|start_seats|toggle_enabled|quit}`), `hidden-to-tray`.

## Screens and flows (spec 10)

- Seats tab: system pool + one container per seat, drag and drop, device menu (assign,
  spare, back to default, indicate, connection path, sharing, ids, software cursor, move
  assignment to a new port), seat menu (rename, account, CPU cores, start, re-logon,
  stop, delete), system menu (compare with saved, view settings, CPU cores, hotkey, wizard,
  import, Windows panels, reset). Status colours, activity flash, "just connected" frame,
  unplugged / shared / attention signs, hub depth. "Dokunarak bul" (identify by touch, from
  `device_activity` events) and "Ekranları numarala" (monitor indication via
  `identify_device`).
- Basic tab: seat table (name, account + password marker, autostart, wallpaper, live
  state), start mode with hints, behaviour (admin-only, own-seat control, lock-screen
  image, same-account policy), appearance (theme, tray icon, confirmations).
- Advanced tab: backend picker with availability and notes from `get_backends`;
  the experimental client-SKU session backend only after the warning dialog (stores
  `experimental_ack`); host topology with the `input_not_isolated` note; timings, safety,
  hotkey, devices, reversible tweaks, CPU sets.
- Status tab: product / seat states, degradations, fallback notes, backends, our drivers,
  pre-flight checks.
- Bottom bar: save (validate → conflicts dialog → `apply_config` with `base_revision`),
  discard, enable / disable (confirmation, reboot offer only when `reboot_pending`), start
  seats (optional confirmation), reboot, diagnostic report. Ctrl+Shift+S saves.
- Global messages: service down, Windows / Pulsar safe mode, admin-only, elevation offer,
  OS build changed, config changed elsewhere, first-run wizard offer, "configured but
  disabled" offer, seat-1-should-use-1.1 advice, devices beyond the seats sent to spare.

## Contract assumptions and gaps

- Seat passwords entered with an account change wait next to the draft
  (`pendingPasswords`) and go to the service (`set_seat_password`) only after
  `apply_config` succeeded; discard drops them. A password-only change is saved with the
  save button too (no `apply_config`). The password marker therefore shows only after save.
- The elevated copy has its own WebView2 data folder, so its localStorage prefs (theme,
  "don't ask again" flags) are separate from the unelevated copy's.
- Same account on several seats (§6.2): a `same_account_warning` notice for the manager's
  own seat opens a dialog with "Bir daha gösterme" (per-user pref) and "Oturumu kapat"
  (`relogon_seat`: logs the session off, the seat then starts again). Spec 10 reads the
  other seats' users itself; here the service decides and sends the notice.
- Pre-flight checks are shown with Turkish text per (id, outcome); the service's `detail`
  is only behind a collapsed "teknik ayrıntı". Fallback notes are parsed
  (`cat:wanted->used:reason`) and rendered in Turkish.
- Gap: the wizard's minimum-resolution precondition (spec 10 §9.2) is not checked by the
  manager yet.

- HYPOTHESIS: during the wizard the service numbers monitors in `list_devices` order
  (present monitors, 1-based) and draws the per-monitor pages itself; the manager's number
  keys map to that order.
- `import_reference` is read as a `config` response (`ConfigView.config`).
- Spec 10 asks "save / discard / cancel" when a tab is left. Here the draft is shared by all
  tabs, so nothing is lost on a tab switch; the question is asked before quitting, before
  the wizard and before other actions that replace the draft.
- Devices are listed row by row in a container (spec: column-major).
- Not built (later phases in spec 10): per-seat IP binding, video-output assignment dialog,
  custom device pictures, webcam preview, log verbosity dialog.

## Untested on hardware

Everything that talks to Windows: the pipe client against the real service (no service yet
on the dev box), the pipe server check (session 0 + service PID, fails closed) and the
service's caller identification at `SECURITY_IDENTIFICATION` level, tray icon and menu,
close-to-tray, `--autostart`, UAC relaunch (`--elevated=<pid>` waits up to 10 s for the old
copy to exit before the single-instance check), the separate elevated WebView2 data
folder, the same-account dialog (needs the service's notice), wizard monitor numbering
(HYPOTHESIS above), the Windows panel shortcuts.

Remote seats tab (merge step 9): every remote page against a real service with a remote
seat (no remote seat has run on hardware yet), pairing through `poll_events` on the `stream`
topic, the LAN address order of the pair dialog, account pictures from `get_user_picture`.
Pulsar-backend pairing events that protocol 3 does not carry (`otp_changed`,
`trust_changed`, `relay_auth_required`; dropped by `map_legacy_event`) no longer refresh the
pair dialog by themselves.
