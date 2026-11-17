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
| `devnode.ps1` | VM (admin) | **TEST ONLY.** Root-enumerated devnodes over SetupAPI / newdev (devcon-like, no extra binary): `-Action Add -HardwareId Root\PulsarSeatIdd -Inf ...`, `Remove`, `List`. |
| `testsign.ps1` | host | **VM driver round only (TEST ONLY).** `-Action Cert` makes the self-signed test certificate under `%USERPROFILE%\.pulsar-seat\testsign\` (key DPAPI-protected, never printed, never in the repo); `-Action Sign` test-signs the built driver packages into `target\vm\testsigned\<Platform>\` (`-Source` = build-drivers.ps1 `-OutRoot`, default `target\drivers`; `-Projects` re-signs a subset and clears the others) (TEST ONLY minifilter altitude in the staged INF). `check-payload` refuses every such file (`test_signed`). |

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

## VM round notes (2026-11-17)

- `Restart-VM` is a hard reset: registry writes not yet flushed (`verifier /standard`)
  were lost. Reboot from inside (`shutdown /r /t 3`) and wait for PowerShell Direct.
- Programs that must run as SYSTEM (`idd_monitor`, `tscon`): a one-shot scheduled task
  `/ru SYSTEM` that writes its output to a file (status.md, VM round 2026-11-17).
- Back to pstest's console session after fast-user-switch tests: `tscon 1 /dest:console`
  as SYSTEM (no password typed).
- `ui.ps1 type:` (`Msvm_Keyboard.TypeText`) produced only a few raw-input packets for the
  kiosk shell; discrete `key:` presses arrive one by one. Use `key:` for kiosk typing.
- `crates/seat-display/examples/idd_monitor.rs`: `gpus` lists DXGI LUIDs, `multi <a> <b>`
  exercises one IddCx adapter per render GPU.
- Extra seat addresses: the VM's NIC set static with the DHCP address plus two addresses
  (`SkipAsSource`); `pktmon` shows the source address on the wire. The checkpoint
  restore undoes it.

## VM round 2 notes (2026-11-17)

- Nested virtualization: this ARM64 host refuses to start the VM with
  `Set-VMProcessor -ExposeVirtualizationExtensions $true` ("this platform does not support
  nested virtualization"), so Hyper-V VM seats cannot be tried in it. Set it back to `$false`.
- `seatctl.ps1 raw` takes the request envelope: `{"type":"test_seat_credentials","data":{"seat":2}}`
  (a bare `{"type":...,"seat":2}` is a bad frame).
- Elevated manager in the VM: Win+R, type the path, Ctrl+Shift+Enter (`key:0x11+0x10+0x0D`),
  then Left + Enter on the UAC prompt (`key:0x25; key:0x0D`). Close an earlier instance
  first: the single-instance plugin hands the new start to the running (unelevated) one.
  Keyboard save of a staged change: Ctrl+Shift+S (`key:0x11+0x10+0x53`).
- Logon type of the credential test: Security log 4624 / 4625 `LogonType` (2 interactive,
  3 network; `0xC000015B` = right not granted, the deny-logon case).
- Class `Properties` keys (class default descriptors) are readable by SYSTEM only: read
  them through a one-shot `/ru SYSTEM` task.
- A single-instance stand-in: any small native ARM64 program that calls
  `CreateMutexW(NULL, FALSE, L"Global\\<name>")` and logs `GetLastError()` (183 = second
  instance, 5 = another user's instance) and `GetModuleHandleW(L"PulsarSplitShim.dll")`.
  On ARM64 only the launcher path loads the module (shim/README.md).
