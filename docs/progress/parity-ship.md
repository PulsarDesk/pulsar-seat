# parity-ship (2026-11-17)

Goal: ci-local.ps1 -Payload -Drivers -Installers green on main; rebuild both MSIs from a
clean committed tree; verify PE machines, build records (dirty: false), SHA256SUMS.txt;
record hashes + final parity summary in docs/agents/status.md.

Done:
- Run 1 failed at "drivers x64 (WDK code analysis)": relative -OutRoot was resolved by
  msbuild against each project folder (drivers\<name>\target\drivers-analyze), so seat-wfp
  had no package dir and the others were checked against stale packages. Fixed in
  installer/scripts/build-drivers.ps1 (OutRoot made absolute); stray drivers\*\target removed.

In progress: full ci-local rerun (log in the agent scratchpad).

Next: MSI arm64 + x64 via build-installer.ps1, PE check, status.md + journal, commit,
delete this file.

Verify: powershell -File installer\scripts\ci-local.ps1 -Payload -Drivers -Installers
