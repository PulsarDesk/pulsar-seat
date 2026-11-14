# seat-display

Display backends for both seat kinds. GPL-3.0-only.

| Seat kind | Where | Operation trait | Backends | Contract / behaviour |
| --- | --- | --- | --- | --- |
| `local` | `src/` (crate root) | `seat_core::backend::display::DisplayBinding` (service) + `FrameSink` (seat host) | `window_presenter`, `display_core`, `pulsar_idd` (`drivers/seat-idd`), `console_only` | `docs/agents/architecture.md` §4.2, `spec/local/40-display.md` |
| `remote` | `src/remote/` (was the `split-display` crate until merge step 8) | `seat_display::remote::DisplayBackend` | `rdp_session`, `vdd`, `pulsar_idd` (`drivers/seat-idd`, remote role) | `docs/split/agents/contracts.md` §3, `spec/remote/40-display-hdr.md` |

**One display trait over both kinds:** `seat_display::SeatDisplayBackend` (`src/kinds.rs`)
gives every backend of either kind a `DisplayBackendId` (`{"kind": "local" | "remote",
"backend": "<wire name>"}`, the per-kind wire names unchanged) and one
`DisplayBackendStatus` (available, signed, HDR, reason). `seat_display::Local` and
`seat_display::Remote` adapt the two operation traits to it; `statuses_for(kind, ..)`
lists one kind's backends. The operation traits stay per kind because the work differs
(physical CCD targets vs. a session display).

Driver: since 2026-11-14 both kinds use **one own IddCx driver**, `drivers/seat-idd`
(built, unsigned, never loaded), so it is signed once (`drivers/README.md`). It picks its
role per devnode (`Root\PulsarSeatIdd` = local, `PulsarSplit_IndirectDisplay` = remote) and
keeps one control interface per role, so `idd_proto` and `remote::ioctl` are unchanged
except for the remote `SET_SEAT_CONFIG` v2 (256 bytes, frame reader SID; the remote frame
objects grant SYSTEM plus that SID only, no LOCAL SERVICE any more).
`DisplayBackendId::own_iddcx` marks the two backends on it.

Both kinds' `pulsar_idd` share a wire name and the driver binary, not the control interface. They never meet in one config
field: local seats read `settings.backends.display`, remote seats
`settings.remote.display_backend`.

## Build and test

```
cargo test -p seat-display                         # both kinds (remote fakes are on in tests)
cargo clippy -p seat-display --all-targets --all-features
cargo check -p seat-display --target x86_64-pc-windows-msvc
cargo test -p seat-display remote -- --ignored --nocapture   # read-only smoke tests (remote impls)
```

Feature `fake` exports `seat_display::remote::fake` (in-memory fakes of every remote system
trait) for the integrator's tests.

# Remote kind (`seat_display::remote`)

Per-seat virtual display for remote seats: one `DisplayBackend` trait, three backends,
the pure mode / HDR / GPU rules and the wire formats shared with the own driver.
Module paths below are relative to `seat_display::remote` (`src/remote/`).

## Design

| Module | What | Pure |
| --- | --- | --- |
| `modes` | `resolve_mode` (refresh 15–500 Hz, size limits, super-sampling, optional pixel-rate cap: super-sampling first, then refresh), `dynamic_scale` (height / 1080, rounded to the Windows scale steps — deliberate deviation, spec 40 §4.4) | yes |
| `hdr` | `hdr_availability` gate (Force SDR → RDP display → driver → OS build → Server/WHQL → IddCx ≥ 1.10 → driver FP16 → GPU), `iddcx_for_build` estimate | yes |
| `adapters` | `RenderAdapter`, `resolve_adapter` (PnP id, then description, else default + warning), `default_adapter` (most dedicated memory), `filter_adapters` (hardware only, dedup) | yes |
| `seat` | `SeatDisplay::from_seat` (all range rules: SDR white 80–480 else 400, percentages 100–500, luminance validity), `ModeResult`, `ModeStatus`, `FrameFormat`, `DisplayState` | yes |
| `frame` | frame header v1 (`#[repr(C, packed)]`, offsets of spec 40 §6.4), `parse_header`, `object_names`, `OBJECT_SDDL` | yes |
| `ioctl` | IOCTL codes `0x0022E000`–`0x0022E014`, interface GUID `{B5FCB55F-1E09-4C81-A58F-0362792CFE99}` (fresh), versioned buffers encode / decode, Win32 error mapping | yes |
| `journal` | restore records (`kind: "file"`) + `plan_restore`; `RestoreLog` trait | yes |
| `backend` | `DisplayBackend`, `choose_backend`, and the three backends below | trait-based |
| `win` | real Windows implementations of every trait (`#[cfg(windows)]`, `windows` 0.62) | no |
| `fake` | in-memory fakes of every trait (feature `fake`) | — |

### Backends

| Kind | Type | When | HDR |
| --- | --- | --- | --- |
| `rdp_session` | `RdpSessionDisplay<P: SessionDisplayProbe>` | default for remote seats (Phase 2). The stock RDP display of the seat session; never changed. `attach` binds, `wait_ready` waits for a Display-class device with `DEVPKEY_Device_SessionId` = session; mode requests → `unsupported` | never (`rdp_display`) |
| `vdd` | `VddBackend<P: VddPipe, F: VddSettingsFile>` | Virtual-Display-Driver (third-party.md default), **console seat only** (`session_attach_unsupported` otherwise). Owns `monitors/count`, `gpu/friendlyname`, `resolutions`, `colour` in `vdd_settings.xml` (journaled; original restored when the last seat detaches), drives `\\.\pipe\MTTVirtualDisplayPipe` (`PING`, `SETDISPLAYCOUNT n`, `RELOAD_DRIVER`, `IDDCXVERSION`). Count is global: one monitor per seat, seat ↔ monitor by slot order. `state()` repairs drift caused through the Everyone-writable pipe (OQ 19). Mode / HDR via the documented display-config APIs (`ModeSetter`) | IddCx 1.10 of VDD + advanced colour |
| `pulsar_idd` | `PulsarIdd<D: DeviceIo>` | own driver (`drivers/seat-idd`, remote role), spec 40 §7 IOCTLs. Never chosen automatically while `OWN_DRIVER_SIGNED = false` | IddCx 1.10 FP16 |

`choose_backend(pref, infos, session_is_console)`: preference wins; else `PulsarIdd` if
installed + signed + (remote capable or console); else `Vdd` if installed and the seat is
the console session; else `RdpSession`.

## Additions to the contract (nothing renamed or dropped)

- `HdrCaps` += `driver_installed`, `server`; `BackendInfo` += `signed`;
  `ModeLimits` += `min_dimension`, `max_dimension`; `SeatDisplay` += `refresh_hz`
  (+ `with_luminance`, `with_last_mode`, `with_refresh`).
- `DeviceIo` += `close(&DeviceHandle)` (cancels pending I/O, idempotent) and
  `sessions(&Guid)`; `DeviceHandle` is an opaque shared handle with an in-flight guard
  (`enter()` -> `InFlight`, `mark_closed()`, `drain(cancel)`): a `DeviceIo` makes every OS
  call inside `enter()`, and `close` drains (cancelling) before closing the OS handle.
  `PulsarIdd::wait_event` (blocking `WAIT_EVENT`); no `PulsarIdd` call holds the seats lock
  across an IOCTL.
- `journal::restored_marker`, `journal::last_seq`: replay only uses records after the last
  `restored` marker of a target (display-8).
- `VddSettingsFile` += `path()`. `VddBackend::new(pipe, file, VddEnv { monitors, modes,
  console, adapters, journal, clock, sleeper, os_build, server, limits })`;
  `VddBackend::reconcile()`, `VddBackend::restore_all()`.
- `RdpSessionDisplay::new(probe, clock, sleeper)`; `PulsarIdd::new(io, clock, sleeper,
  os_build, server)`. `Clock` + `Sleeper` traits, `SystemClock`.
- `object_names` is infallible: it keeps only hex digits and `-` (lowercase), so no name
  can contain a backslash.
- Extra error codes: `invalid_value`, `unsupported`, `protocol`, `io`.

## Integration notes

- Real types: `win::os::{os_version, WinConsole}`, `win::dxgi::DxgiAdapters`,
  `win::devices::WinSessionProbe`, `win::deviceio::WinDeviceIo`,
  `win::vdd::{WinVddPipe, WinVddSettingsFile::discover(), WinVddMonitors, WinModeSetter}`,
  `win::journal::FileRestoreLog::new(<data root>\journal\display.jsonl)` (the service passes
  the Pulsar Seat data root, `%ProgramData%\Pulsar Seat`).
- **Display configuration is per session.** `WinVddMonitors` and `WinModeSetter` act on
  the calling process's session; from the service (session 0) they see nothing. For the
  VDD backend the integrator runs them in the console session (helper process) or adapts
  the traits over the helper. Everything else works from the service.
- `WinVddPipe` refuses a pipe whose server is not `%SystemRoot%\System32\WUDFHost.exe` in
  session 0 as LOCAL SERVICE (display-10), connects with identification-level impersonation
  only, and cancels its overlapped I/O after 10 s (no thread left behind).
- `WinVddSettingsFile` and `FileRestoreLog` use `win::secure::TrustedDir`: the folder must
  not be a junction, must be owned by SYSTEM / Administrators / TrustedInstaller and must
  grant no write-type right to anyone else, else `rejected` (display-9). The journal
  creates the data root and `journal` with `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`
  when absent. Files: no reparse point, one link; settings replaced via a `CREATE_NEW`
  random temp file + `MoveFileExW`.
- VDD seats bind to a `(adapter LUID, target id)` on first use; if a detach removes another
  seat's monitor, that seat moves to the freed one and `state().last_error` says
  `vdd_monitor_rebound` once (re-send the mode).
- The device ACL of the own driver admits SYSTEM only (plus Administrators read).

## Tests (remote kind)

Commands: see "Build and test" at the top. Unit tests (51) cover: mode rules and the spec 40 §4.3 examples, dynamic scale steps, the
HDR matrix, adapter resolution, frame header offsets (`offset_of!` + byte tests), IOCTL
buffer round trips and byte layout, Win32 error mapping, all three backends against fakes
(PulsarIdd against a simulated driver that decodes the real buffers), VDD drift repair
and journal restore, `choose_backend`. The ignored smoke test only reads (OS version,
console session, DXGI adapters + LUID→PnP map, display devices per session, VDD monitors,
own driver interfaces, VDD settings path); it changes nothing.

## Untested

- Every `win` implementation except the read-only queries above: VDD pipe commands and
  file writes (VDD not installed on the dev box), mode / advanced-colour changes,
  `WinDeviceIo` (the own driver does not exist on any machine), journal file writes.
- Whether VDD keeps target ids stable per monitor index across reloads (`HYPOTHESIS`,
  slot mapping depends on it), and that its monitors report `MTT1337` in the device path
  on every version.
- The whole own-driver path end to end (see `drivers/seat-idd/README.md`, remote role).
- Remote-session capture with the RDP display (spec 40 §6.5) — stream package / test VM.

Third-party: interoperates with Virtual-Display-Driver (MIT) through its documented pipe
and settings file only; no code or binary of it is included.
