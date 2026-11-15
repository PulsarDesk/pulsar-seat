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
- scratch 0.1.1 MSI (scratchpad msi/ps-0.1.1.msi, dirty build, version files reverted)
- 1b upgrade 0.1.0 -> 0.1.1: PASS (one product, config rev 2, secrets kept)
- 1c uninstall REMOVEUSERDATA=0: config kept, reinstall picks it up; seat passwords + host
  accounts always deleted (by design, documented in installer/README.md)
- 1d uninstall REMOVEUSERDATA=1: nothing left (service, accounts, ProgramData, journal,
  HKLM PulsarSplit / Pulsar Seat, credprov, LSA, firewall, tasks)
- 2 remote seat on client Win11: PASS (apply warning session_unavailable, start refused
  session_unavailable, TS registry hash unchanged, no listener / fw / sessions); manager
  shows the remote seat "Kapalı" without a hint on the seats page (UX note)
- 4 Verifier under HVCI: PASS (no violation / bugcheck / dump / CI warning; reset)
- NEXT: full gates, both MSIs, status + journal, VM clean-logged-on, delete psgate2.vhdx

## Denied / for the morning
- (none)
