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

## BLOCKED (2026-11-15)
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
