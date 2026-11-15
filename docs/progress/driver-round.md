# Driver round in the test VM (progress note, crash-safe: VM may bugcheck, PC may restart)

Goal: test-signed drivers (seat-input-filter KMDF, seat-idd IddCx both roles,
seat-storage-filter minifilter, split-pad if built) installed and loaded in VM
PulsarSeatTest-arm64 only. Approval: user answer relayed by the coordinator 2026-11-15
(Secure Boot off + testsigning in the VM; host test cert under %USERPROFILE%\.pulsar-seat\testsign\
trusted in the VM; load our drivers + Driver Verifier). Memory Integrity / HVCI / VBS untouched.
Drivers are installed with pnputil from test-signed packages; the MSI stays driver-free.
Minifilter: TEST ONLY altitude in the staged copy only. Checkpoint testsigning-clean after
testsigning + cert; clean-logged-on unchanged. End: restore testsigning-clean.

## Done
- 42bc26a plan.md entry
- drivers built ARM64 Release (build-drivers.ps1: zero warnings, PE 0xAA64, infverif ok)
- test cert created on the host (CurrentUser\My, thumbprint ED9E4C4D...; files in
  %USERPROFILE%\.pulsar-seat\testsign\: testsign.cer, testsign.pfx, testsign.pfx.dpapi)
- fd39b61 tools/vm/testsign.ps1 + check-payload `test_signed` rule (2 tests); packages
  test-signed into target\vm\testsigned\ARM64 (minifilter TEST ONLY altitude 409990);
  check-payload flags all 9 signed artefacts

## VM state (2026-11-15, coordinator ran vmprep after the user's go)
- Secure Boot off, testsigning Yes, VBS status 0 before and after (never running in this
  VM, so HVCI could not be exercised here), cert in Root + TrustedPublisher (certutil -f),
  checkpoint testsigning-clean re-taken 23:02 with both stores.
- input filter: pnputil oem2.inf, service PulsarSeatInput Running; stacks kbdclass >
  PulsarSeatInput > hyperkbd (TEST ONLY VMBus keyboard model line, 2997c66) and mouclass >
  PulsarSeatInput > mouhid.
- seat-idd: root devnode ROOT\DISPLAY\0000 (hwid Root\PulsarSeatIdd, scratchpad
  vm\mkroot.ps1); first start failed: IddCxAdapterInitAsync STATUS_INVALID_PARAMETER
  (MaxMonitorsSupported 64; >16 refused) -> fixed e340421, device OK after the fix.
- minifilter: oem4.inf, fltmc load ok, instances on C:, a letterless volume, G: (TEST ONLY altitude 409990)
- split-pad: oem5.inf in the store (no devnode: seat-pads creates them on demand)
- IDD monitors: ADD_MONITOR -> IddCxMonitorCreate INVALID_PARAMETER with INDIRECT_VIRTUAL
  (fixed efa0090: INDIRECT_WIRED), then IddCxMonitorArrival INVALID_PARAMETER with every
  variant tried (EDID / none, random container GUID, HDMI type, WIRED_OTHER transmission,
  ORIGIN_DRIVER, MaxDisplayPipelineRate): OPEN. Tool: example idd_monitor (6ea5c61), run as
  SYSTEM via scheduled task PsIddTest in the VM.
- storage test disk: scratchpad psgate.vhdx (1 GB) on SCSI 1, G: NTFS with hello.txt
- next: MSI 6ea5c61, product backends (input kmdf_filter, storage_minifilter), uninstall,
  Verifier, docs, restore testsigning-clean, detach/delete psgate.vhdx

## (resolved) BLOCKED (2026-11-15)
- VM step (Stop-VM, Set-VMFirmware -EnableSecureBoot Off, bcdedit testsigning on, cert
  into Root + TrustedPublisher, reboot) was denied by the Claude Code permission system
  (auto mode classifier, "Undermining Oversight"). Not worked around. Needs the user to
  allow it (permission rule) or run the script themselves:
  scratchpad vm\vmprep.ps1 (copy kept in the report). VM untouched (clean-logged-on).

## Next (after the go)
1. vmprep (state before/after incl. VBS/HVCI), checkpoint testsigning-clean
2. pnputil /add-driver <pkg>\*.inf /install per package; fltmc; driverquery; CodeIntegrity log
3. product backends (kmdf_filter, pulsar_idd, storage_minifilter), U3 seats 2+3
4. uninstall; Verifier; docs; restore testsigning-clean
