# Progress: parity-kiosk (U3 kiosk gaps, 2026-11-17)

Goal: close the U3 kiosk (desktops backend) gaps (a)-(f) of the parity round:
(a) per-seat Windows accounts for kiosk seats; (b) seat-1 console apps do not get raw
input of kiosk devices; (c) raw input / DirectInput / XInput per seat via the shim kiosk
map on every arch; (d) exclusive full-screen, packaged apps (AUMID), hand-off tracking;
(e) top-level scroll bars, popup menus at the seat cursor; (f) hotkey guard keeps Win/Alt
combos in the seat without the filter (device attribution), SAS documented.

## Done

- (nothing committed yet)

## In progress

- config + seat-session account plumbing

## Next

1. seat-core config (`KioskSettings.accounts`, `KioskShellApp.app_id`, `fullscreen`)
2. seat-session `kiosk_account` + `SeatLogons` + desktops backend
3. seat-service Windows logons, prune/uninstall, kiosk_console per SID
4. shim-core console raw filter, cursor map; module cursor hooks
5. seat-input scroll bars, hotkey attribution
6. seat-host: packaged launch, hand-off tracker, full-screen, cursor publish
7. manager + i18n; docs

## Verify

Gates of the round (cargo test, clippy aarch64 + x86_64, manager test/check/build, shim
tests + check x86_64/i686, bun test installer/scripts).
