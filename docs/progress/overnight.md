# Overnight batch 2026-11-16 (progress note, crash-safe)

Rules: VM only, short bounded commands, local commits, denied step = skip + note.
1. MSI lifecycle on clean-logged-on (install, repair /fa, major upgrade via scratch version
   bump never committed, uninstall REMOVEUSERDATA=1 -> nothing left, =0 -> data kept +
   reinstall picks it up)
2. remote seat on client Win11: refusal, no partial state / RDS writes, manager screenshot
3. U1 regression (switch links both lock screens, password-less preselect, switch_seat)
4. Verifier under HVCI (testsigning-hvci, input filter + minifilter, reboot, activity)
5. fixes + full gates + both MSIs (SHA-256 in status) + docs + VM clean-logged-on

## Done
- 1a install (MSI ba917a9 = scratchpad msi/ps-0.1.0.msi) + U1 config + repair /fa: PASS
  (service, config rev 2, 3 seats, LSA L$PulsarSeat.HostAccount / Seat.02 / Seat.03, credprov)
- 3 U1 regression: seat 2 lock links PASS, switch_seat 1 preselect PASS, switch_seat 3 FAIL ->
  fixed 3e1003d (stale switch_to record superseded by a new hand-over), test + mutation check
- IN PROGRESS: scratch 0.1.1 MSI build (Cargo.toml / package.json / tauri.conf.json /
  Cargo.lock / apihelper Cargo.lock bumped, NEVER COMMIT: git checkout those 5 files after)

## Denied / for the morning
- (none)
