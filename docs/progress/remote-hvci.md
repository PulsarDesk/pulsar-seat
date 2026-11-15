# Remote IDD role + HVCI in the test VM (progress note, crash-safe)

Goal: (1) seat-idd remote role: adapter (hwid PulsarSplit_IndirectDisplay) + monitor via the
remote control interface, display settings, modes / HDR / refresh, frame channel opens,
clean remove; fix bugs. (2) VBS + HVCI in the VM (find out first if possible: nested
virtualization exposure, Secure Boot off for test-signing); if it runs: load our 4
drivers, CI events, DeviceGuard; checkpoint testsigning-hvci. Start / end at
testsigning-clean.

## Done
- plan.md entry

## Next
1. read remote role README + seat_display::remote client
