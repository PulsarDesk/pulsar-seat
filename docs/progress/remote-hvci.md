# Remote IDD role + HVCI in the test VM (progress note, crash-safe)

Goal: (1) seat-idd remote role: adapter (hwid PulsarSplit_IndirectDisplay) + monitor via the
remote control interface, display settings, modes / HDR / refresh, frame channel opens,
clean remove; fix bugs. (2) VBS + HVCI in the VM (find out first if possible: nested
virtualization exposure, Secure Boot off for test-signing); if it runs: load our 4
drivers, CI events, DeviceGuard; checkpoint testsigning-hvci. Start / end at
testsigning-clean.

## Done
- plan.md entry (b341f1e)
- remote role: root devnode with hwid PulsarSplit_IndirectDisplay starts, IddCxAdapterInitAsync
  ok (IddCx 0x1B01, 1.10 functions present) but EvtIddCxAdapterInitFinished never comes and
  IddCx fails the device ~9 s later (CM_PROB_FAILED_POST_START): a REMOTE_SESSION_DRIVER
  adapter needs a device from a Remote Desktop protocol stack. Not testable here.
  Tool: example idd_remote (a9c1a05).
- HVCI capability: AvailableSecurityProperties 1,3,5,7 (hypervisor support present, Secure
  Boot absent: off for testsigning); VBS not running; host ExposeVirtualizationExtensions
  False (left unchanged, not needed for property 1)
- IN PROGRESS: Memory Integrity on via Scenarios\HypervisorEnforcedCodeIntegrity Enabled=1
  (the toggle's key, no RequirePlatformSecurityFeatures), reboot, check

## Next
1. read remote role README + seat_display::remote client
