# VM test harness

Scripts for testing an installed Pulsar Seat MSI inside a local Hyper-V test VM
(`PulsarSeatTest-arm64` by default; override with `$env:PSEAT_VM`). Machine-specific facts
(credential file, checkpoint names, host paths) live in `docs/local/README.md`.

| File | Runs on | What |
| --- | --- | --- |
| `seatctl.ps1` | VM (or any PC with the service) | Control pipe client, protocol 3: `hello`, `status`, `config`, `apply`, `enable` / `disable`, `start` / `stop`, `password`, `poll`, `subscribe`, `raw`, `seq`. Header lists every command. |
| `vm.ps1` | host (dot-source) | `Reset-TestVm`, `Install-TestMsi`, `Invoke-Vm`, `Invoke-Seatctl`, `Get-VmReport` over PowerShell Direct. |
| `ui.ps1` | host | Synthetic keyboard and screenshots over Hyper-V WMI (`-Do "key:0x5B+0x52; type:cmd; key:0x0D; shot:x"`); `cred:<name>` types a VM password from the credential file without printing it. |
| `scenarios.ps1` | host (dot-source after `vm.ps1`) | `Set-U1Config`, `Start-U1Seat`, `Set-U3Config`, `Update-VmConfig`, `Get-SeatSummary`. |
| `testsign.ps1` | host | **VM driver round only (TEST ONLY).** `-Action Cert` makes the self-signed test certificate under `%USERPROFILE%\.pulsar-seat\testsign\` (key DPAPI-protected, never printed, never in the repo); `-Action Sign` test-signs the built driver packages into `target\vm\testsigned\<Platform>\` (TEST ONLY minifilter altitude in the staged INF). `check-payload` refuses every such file (`test_signed`). |

Rules: Hyper-V Administrators membership is enough (no elevation on the host). Passwords
travel only as `PSCredential` objects into the VM and are never printed. PowerShell
Direct runs in session 0 with pstest's full admin token, so `seatctl.ps1` there passes the
service's `Admin` checks. Programs on the user's desktop are started with Win+R through
`ui.ps1` (scheduled tasks from PowerShell Direct did not start reliably there).

Typical run (host, repo root):

```powershell
. .\tools\vm\vm.ps1
Reset-TestVm
Install-TestMsi
Invoke-Seatctl status
Get-VmReport
```

Logon-screen keyboard notes (Win11 26300): Enter on a lock screen without the curtain
submits an empty password; Shift+Tab from the tray icons (network, accessibility, power)
reaches the user list; Tab from the password box goes to the arrow, then the links, then
"Sign-in options". Watch for the power menu (Shift+Tab from the password box).

What a Hyper-V VM cannot emulate: a second monitor, keyboard or mouse (one synthetic
keyboard and mouse, one virtual display), USB ports, per-seat audio endpoints (the basic
session has no audio device), GPU. Seat device assignments are therefore empty or share
the one keyboard / mouse. With one monitor, kiosk seats (U3) cannot get a display: seat 1
must keep one (`seat_one_no_display`) and monitors are exclusive.
