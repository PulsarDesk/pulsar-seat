# 90 — Installer, uninstaller, upgrade and coexistence

Behaviour of the reference product's setup program and uninstaller (reference: multi-seat
host 1.6.0.0), plus the system changes that are tied to install-time even though some of
them are made later by the service. Each mechanism ends with a **Pulsar Split approach**
subsection. No code; prose and tables only.

Evidence markers: **[S]** static (shipped files, the uninstall log, binary strings),
**[L]** live (read-only observation of one installed host), **[I]** inferred,
**[?]** unknown. Related specs: `00-architecture.md` §6/§8/§10, `10-manager.md` §3.2
(shim toggle), `20-service.md` §5 (boot phase), `30-session-rds.md` (RDS registry
values), `40-display-hdr.md` (IddCx driver, HDR gate), `80-stream-server.md` §8–§9 (web
UI, ports, firewall).

Sources used: the uninstaller's log file (Inno Setup binary uninstall log, parsed by
record), the shipped driver INFs, binary string tables of the service and the controller
driver, the installed host's registry, certificate stores, firewall, SCM, PnP tree and
driver store, and — for Windows defaults — a clean Windows 11 ARM64 machine (build 28000)
that never had the reference product. The original setup executable was **not**
available, so install-time steps that leave no uninstall record are partly [?].

Two machines are cited below:

| Label | Machine | Relevant state |
| --- | --- | --- |
| **host** | x64 test PC with the reference installed, service stopped | Windows 11 build 26300; Secure Boot **off**, Memory Integrity **off**, test-signing **off** [L] |
| **clean** | ARM64 dev laptop, reference never installed | Windows 11 build 28000; Memory Integrity **on** (HVCI and System Guard running) [L] |

---

## 1. Installer technology and identity

| Item | Reference behaviour |
| --- | --- |
| Engine | Inno Setup **6.7.0**, Unicode, 64-bit install mode [S][L] |
| AppId | `{DB5AA12E-1299-4244-8651-F363406E89F6}` [S][L] |
| ARP key | `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{DB5AA12E-1299-4244-8651-F363406E89F6}_is1` [L] |
| ARP values | `DisplayName`, `DisplayVersion` (`1.6.0.0`), `Publisher`, `DisplayIcon` (service exe), `InstallLocation`, `UninstallString` and `QuietUninstallString` (uninstaller with `/SILENT`), three vendor URLs, `NoModify=1`, `NoRepair=1`, `MajorVersion`/`MinorVersion` and `VersionMajor`/`VersionMinor`, `EstimatedSize` (≈ 74 MB), `InstallDate` (yyyymmdd), and the Inno-private values `Inno Setup: App Path`, `Inno Setup: Icon Group`, `Inno Setup: User`, `Inno Setup: Language` (`english`), `Inno Setup: Setup Version` [L] |
| Install folder | fixed `C:\Program Files\Duo` (several binaries hard-code it, so the folder is effectively not choosable) [S][L] |
| Start-menu group | common (all users) `Programs\Duo` with two shortcuts: manager, uninstaller [S][L]; no desktop shortcut observed [L] |
| Privileges | per-machine install, administrator required [S][I] |
| Languages | English only [S] |
| Uninstaller | `unins000.exe` + `unins000.dat` (log) in the install folder [S] |
| Scheduled tasks | none created [L] |

**Pulsar Split approach.** Own identity everywhere: product name, publisher, install
folder `%ProgramFiles%\Pulsar Split`, data folder `%ProgramData%\Pulsar Split`, a stable
MSI `UpgradeCode` (or NSIS registry key) that never changes across versions. Install
folder may be choosable, because Pulsar Split resolves every path from its own location
or from the registry. Turkish installer UI. No vendor web links beyond the project page.

---

## 2. Install-time actions

### 2.1 Overview [S][I]

The setup program's own logic (its compiled script) does, before and after copying files:
check for and install the WebView2 runtime (§2.3), enable optional Windows features
(§2.4), import a certificate (§2.5), and add Defender exclusions (§2.6). Service and
firewall registration (§2.7, §2.8) leave no uninstall-log record of how they were made.

### 2.2 Files

Laid down under the install folder [S][L]:

| Path (relative) | Content | Notes |
| --- | --- | --- |
| `Duo.exe` | service | also has a `--StartService` sub-command [S] |
| `DuoManager.exe` | manager (.NET WPF) | |
| `DuoRdp.exe` | headless RDP client | |
| `TermWrap.dll`, `DuoBluetooth.dll`, `DuoRds.dll` | service-DLL wrappers / protocol provider | |
| `DuoIdd.dll` | display module (copied by the service into `System32\drivers\UMDF`) | |
| `DuoAcRes32.dll`, `DuoAcRes64.dll` | shim modules (copied by the manager toggle to `SysWOW64\AcRes.dll` / `System32\AcRes.dll`) | 1.6 |
| `sunshine.exe`, `zlib1.dll` | stream server fork | |
| `msvcp140.dll`, `vcruntime140.dll`, `vcruntime140_1.dll`, `ucrtbase.dll` | app-local Microsoft C/C++ runtime | Microsoft binaries |
| `runtimes\win-x64\native\WebView2Loader.dll` | WebView2 loader for the manager | Microsoft binary |
| `DuoController\DuoController.inf/.cat/.dll` | virtual-pad UMDF driver package | 1.6 |
| `assets\…` | stream-server shaders, **browser web UI** (`assets\web`), cover images, `apps.json` template | |
| `credentials\cacert.pem`, `credentials\cakey.pem` | stream-server TLS identity **shipped inside the package** (same key pair for every install) [S] | not inspected (private key) |
| `config\` | per-seat files written at runtime; **not** in the install log, so never removed by the uninstaller [S][I] | |

Older versions additionally shipped (and the 1.6 upgrade left behind on disk) [S][L]:
`DuoVerifier32.dll`, `DuoVerifier64.dll` (earlier injection method through the
Application Verifier), `RdpIdd.dll` and `rfxvmt.dll` (copies of Windows system files),
`assets\dxvk\x32|x64\{d3d8,d3d9,d3d10core,d3d11,dxgi}.dll` (DXVK),
`assets\drivers\nefconw.exe` and `assets\drivers\ViGEmBus\ViGEmBus.{inf,cat,sys}`.

**Pulsar Split approach.**
- Ship only our own binaries plus GPL-compatible third-party code with notices. **No
  Microsoft DLLs**: build Rust and C++ with a static CRT; no `RdpIdd.dll`/`rfxvmt.dll`
  copies (licence prohibits redistribution of Windows system files). The WebView2 loader
  is not needed by Tauri (it links the loader statically).
- **Stream server without browser UI** (decision 2026-11-12): build the stream server with
  the control-pipe / no-web-listener patch of `80-stream-server.md` §8.3 option B, and
  **leave the web UI assets (`assets\web` equivalent) out of the package**. A build-time
  check fails the release if web UI files are present in the payload.
- **Never ship a fixed TLS key pair.** The stream server generates its own certificate
  and key on first start, per seat or per host, stored under `%ProgramData%\Pulsar Split`
  with an ACL of SYSTEM + Administrators.
- Runtime-generated data (seat configs, logs, keys) lives in `%ProgramData%`, never in
  the program folder, so file replacement on upgrade cannot touch it.
- Nothing is copied into `System32`/`SysWOW64` by the installer.

### 2.3 WebView2 runtime [S]

- Detection: string value `pv` under
  `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`
  (the public Evergreen runtime client id) [S]; observed present [L].
- If missing: download the Microsoft Evergreen bootstrapper from the public fwlink
  (`LinkId=2124703`) to the setup temp folder, run it with `/silent /install`, show a
  progress page. Failure aborts setup; an internet connection is required [S].

**Pulsar Split approach.** Use the Tauri bundler's `webviewInstallMode`
(`downloadBootstrapper` by default; `embedBootstrapper` or `offlineInstaller` for offline
media). The bootstrapper and offline installer are redistributable under Microsoft's
WebView2 distribution terms; that is the only Microsoft component the installer may carry.
Windows 11 already includes the runtime.

### 2.4 Optional Windows features (sandbox support) [S]

- DISM, online, no restart: enable feature `VirtualMachinePlatform` with all parent
  features and without Windows Update as a source; shown as a "sandbox support"
  dependency step [S].
- A separate DISM add-package call and a "container support" step, both gated on
  `CurrentBuildNumber`/`CurrentBuild` from `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`
  [S]. The package path and the exact build threshold are [?].
- The host has `VirtualMachinePlatform` and `Containers-DisposableClientVM` enabled
  [L] (cannot be attributed to this installer with certainty [?]).
- The sandbox seat mode itself is unreachable in the 1.6 UI (see `00-architecture.md` §9),
  yet the feature is enabled at install.

**Pulsar Split approach.** Do not enable optional features at install. If a later
feature needs one, the manager explains why and asks; the change is logged in the change
journal (§4.3) so uninstall can offer to revert it.

### 2.5 Certificate import — **do not copy**

- The 1.6 installer extracts a public certificate file to the setup temp folder and
  imports it with `certutil` into **both** `LocalMachine\Root` and
  `LocalMachine\TrustedPublisher` [S][L].
- Observed certificate [L] (interop identifiers for detection, §8.1):

  | Field | Value |
  | --- | --- |
  | Subject = Issuer (self-signed) | `CN=Black-Seraph` plus an e-mail RDN |
  | SHA-1 thumbprint | `0A4FCFD0F177A8BBAE3CF8F8893134D0BB036BB3` |
  | Validity | 2026-05-06 → 2126-05-06 (100 years) |
  | Key / hash | RSA, SHA-256 |
  | EKU | Code Signing |
  | Private key on host | no |

- Purpose [I]: make Windows trust the vendor-signed UMDF controller package
  (`DuoController.cat`, signer = this certificate) and the vendor-signed DLLs without
  Microsoft signing, so PnP installs them silently.
- The uninstaller does **not** remove the certificate [S] (no matching uninstall record).

Factual risk: a trusted root with Code Signing EKU lets anyone holding the matching
private key sign drivers and executables that this machine will trust, for a century,
even after the product is uninstalled.

**Pulsar Split approach.** Never install a root or TrustedPublisher certificate on a user
machine. Release driver packages carry a Microsoft signature (§3.5); executables, DLLs
and the MSI are Authenticode-signed with a publicly trusted code-signing certificate (§7).
The development signing path is an open maintainer decision (§3.5); test-signing mode is
**not** used (rules.md).

### 2.6 Microsoft Defender exclusions

Added with Defender's preference cmdlet in hidden PowerShell runs [S]:

| Kind | Target | Version |
| --- | --- | --- |
| process | `System32\WUDFHost.exe` (the host of **all** user-mode drivers on the machine) | 1.5 and 1.6 [S] |
| path | `System32\drivers\UMDF\DuoIdd.dll` | 1.5, 1.6 [S][L] |
| path | `System32\verifier.dll`, `SysWOW64\verifier.dll` | pre-1.6 [S][L] |
| path | `System32\AcRes.dll`, `SysWOW64\AcRes.dll` | 1.6 [S][L] |
| path | `System32\drivers\RTCore64.sys` (out-of-scope kernel helper, §4.2) | 1.6 [S][L] |
| path | `C:\Windows\SystemTemp` | 1.6 [S][L] |
| path | `C:\ProgramData\dbg` (symbol cache used by the service) | 1.6 [S][L] |
| path | install folder | all; the service also re-adds it at every start [S][L] |

None of them is removed on uninstall [S].

**Pulsar Split approach.** No AV exclusions, ever. False positives are handled by
signing and by submitting builds to Microsoft's file-submission portal. The installer
detects the reference product's leftover exclusions only to **report** them on the
coexistence page (§8); it does not change them.

### 2.7 Service registration

Observed baseline [L] (service stopped, "start with Windows" off):

| Property | Value |
| --- | --- |
| Name / display name | `DuoService` / `Duo` |
| Type | own process (`0x10`) |
| Account | LocalSystem |
| Start type | demand (3); the manager's "start with Windows" toggle writes the service's `Start` value (auto/demand) [S] |
| `DelayedAutostart` | not set [L]; the manager has no code path that writes it [S] |
| Image path | the service exe, no arguments |
| Dependencies | none |
| Failure actions | none (reset period 0, no actions, no command) |
| Service SID type | none |
| `RequiredPrivileges` | not set (the process enables its privileges itself, `20-service.md` §1) |

How the service is created at install is [?]: there is no uninstall record for creation
(Inno only logs removal steps); the service exe imports the SCM create/change APIs [S].
Uninstall stops it and deletes it (§4.1).

**Pulsar Split approach.** MSI `ServiceInstall`/`ServiceControl` (WiX) or, for NSIS, the
service binary's own `install`/`uninstall` sub-commands called from installer hooks (§9.1).
Differences from the reference baseline, all deliberate: service SID type *unrestricted*
(lets ACLs name `NT SERVICE\PulsarSplit`); `RequiredPrivileges` lists only what the
service uses; failure actions = restart after 5 s, 30 s, 120 s, reset after 1 day;
`DelayedAutostart` = 1 when "start with Windows" is on. The named-pipe IPC endpoint is
created by the service, not the installer.

### 2.8 Firewall rules

- Inbound allow rules, all profiles, all protocols/ports, program-scoped [L]:

| Rule display name | Program | Created |
| --- | --- | --- |
| `Duo` | service exe | install [I] |
| `Duo Sunshine` | bundled stream server exe | install [I] |

- Rule `Name` fields are random GUIDs and no rule group is set [L], consistent with
  rules created by the `netsh advfirewall` command line [I].
- The host has **two** `Duo` rules (one per install run) [L]. Cause [S][I]: the
  uninstaller's delete command for the `Duo` rule filters on the stream-server program
  path instead of the service exe, so it never matches; every reinstall adds another.
- The service separately enables Windows' built-in Remote Desktop rules at boot (see
  `20-service.md` §5).

**Pulsar Split approach.** Exactly the rules of `80-stream-server.md` §9.2, nothing more:

| Proto | Ports per seat (offsets from the seat's base port) | Not opened |
| --- | --- | --- |
| TCP | base−5, base, base+21 | base+1 (stream server web UI — not built, §2.2) |
| UDP | base+9, base+10, base+11 | everything else in [base−5, base+21] |

Program = the Pulsar stream-server binary; profiles Private (+ Domain if the operator
ticks it), never Public by default. All rules share one grouping string (`PulsarSplit`)
so removal is "delete every rule in the group" and cannot miss. No program-wide any-port
rule, no rule for the service binary (the manager-to-service channel is a named pipe).
Created and removed by the service through the Windows Firewall COM API
(`INetFwPolicy2`), not by shelling out. The installer creates no rules itself; uninstall
removes the group as a safety net.

---

## 3. Drivers

### 3.1 Virtual-pad UMDF driver (`DuoController`) [S][L]

| Item | Value |
| --- | --- |
| INF | `DuoController.inf`, class `DuoController`, ClassGuid `{070D9FF9-4F04-4419-90D2-C4CA9DFDB2EE}`, provider = vendor, `PnpLockDown=1`, DriverVer 1.6.0.0, target `NTamd64.10.0...17134` [S]; in the driver store as `oemNN.inf`, signer = the vendor certificate of §2.5, pnputil attribute "Legacy", WHCP version unknown [L] |
| Root devnode hardware ids (PnP) | `ROOT\VID_054C&PID_0DF2` (DualSense Edge), `ROOT\VID_054C&PID_0CE6` (DualSense), `ROOT\VID_054C&PID_05C4` (DualShock 4), `ROOT\VID_045E&PID_02FF&IG_00` (Xbox One Elite, includes the inbox `xinputhid.inf` sections) — all excluded from manual selection [S] |
| Stack | UMDF 2.15, `WUDFRd` reflector as lower filter, `mshidumdf` HID mini-port pass-through, high host priority, kernel-mode clients allowed [S] |
| Binary | `DuoController.dll`, run from the driver store (destination dir id 13) [S] |
| Install path | not by the installer: the DLL also exports client functions (initialize, create/remove controller, send report, uninitialize); on initialize it installs its own INF from its folder and creates root-enumerated device nodes through SetupAPI [S][I] |
| Removal | the service on stop removes the pad device nodes and uninstalls every OEM INF of class `DuoController` [S]; the host (service stopped) still has one `duocontroller.inf` package in the store and many non-present HID children [L] |
| INF licence | the INF header states Apache-2.0 [S] |

**Which identity games see** [L][I]. Two different id spaces are involved:

```
ROOT\VID_054C&PID_0CE6      <- PnP root devnode; hardware id chosen by whoever creates
   |                           the devnode (from the INF models). Games do not read it.
   | UMDF driver + mshidumdf reports HID attributes (VendorID, ProductID, version)
   v
HID\VID_054C&PID_0CE6&DUOCONTROLLER&<per-pad serial>\...   <- HID child, created by hidclass
   hardware ids: HID\VID_054C&PID_0CE6, HID\VID_054C&UP:0001_U:0005,
                 HID_DEVICE_SYSTEM_GAME, HID_DEVICE_UP:0001_U:0005, HID_DEVICE
```

- The HID child's VID/PID (what games read through HID attributes, SDL, Steam Input,
  DirectInput) comes from the HID attributes the driver reports, not from the root
  devnode's PnP id [I]. On the host the HID children show `VID_054C&PID_0CE6` with an
  instance-id suffix `DUOCONTROLLER&<serial>` [L].
- So a clean driver can keep its PnP hardware ids in its **own** namespace and still
  present 054C:0CE6 (etc.) to games [I] — subject to verification on the own driver
  (§10).
- Factual note: `054C` and `045E` are other companies' USB-IF vendor ids; presenting them
  is common practice for pad emulation (needed for game compatibility) but is not
  endorsed by those vendors.

### 3.2 ViGEmBus and `nefconw.exe`

- Pre-1.6 packages shipped `assets\drivers\ViGEmBus\{ViGEmBus.inf,.cat,.sys}` (Nefarius
  ViGEmBus 1.21.442, Microsoft-signed via WHCP) and `assets\drivers\nefconw.exe` (Nefarius
  "nefcon" device/driver CLI, Authenticode-signed by Nefarius) [S][L]. The 1.6 package
  no longer lists them [S]; the files remain on disk from the earlier install [L].
- Exact nefconw arguments used by the old installer are [?] (install-time steps are not
  logged). Publicly documented nefconw usage for ViGEmBus: create a root device node with
  the bus hardware id and the System class GUID, then install the INF onto it [I].
- On the host ViGEmBus is present as a **separate MSI product** with an install date
  earlier than the reference product, very likely installed independently [I]:

  | Identifier | Value [L] |
  | --- | --- |
  | MSI product name / version | `ViGEm Bus Driver` 1.22.0 |
  | MSI ProductCode (version-specific) | `{966606F3-2745-49E9-BF15-5C3EAA4E9077}` |
  | MSI UpgradeCode (stable across versions) | `{67175F6C-AA18-43A7-AE60-2FC3FD10BF79}` (from the Installer `UpgradeCodes` registry, packed form `C6F5717681AA7A34EA06F23CDF01FB97`) |
  | Devnode | `ROOT\SYSTEM\0006`, class `System` `{4d36e97d-e325-11ce-bfc1-08002be10318}` |
  | Bus hardware id | `Nefarius\ViGEmBus\Gen1` |
  | Driver package | `vigembus.inf` (published as `oemNN.inf`), 1.21.442.0, signer Microsoft Windows Hardware Compatibility Publisher |

- The bundled stream server uses ViGEm client calls for Xbox 360/DS4 targets [S].

### 3.3 Display module

Not a driver package: the service copies `DuoIdd.dll` into `System32\drivers\UMDF` and
re-points the inbox RdpIdd UMDF service image
(`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\RdpIdd`, value
`ImagePath`) at it during seat operation [S][L]. While the service is stopped the value
points back at the driver-store copy [L].

Uninstaller restore behaviour [S]:
- It looks only for a driver-store folder matching `rdpidd.inf_amd64_*`. On an x64 host
  it writes that folder's `RdpIdd.dll`, with the Windows folder replaced by `%SystemRoot%`,
  as `REG_EXPAND_SZ`.
- If no such folder exists (for example an ARM64 machine, whose folder is
  `rdpidd.inf_arm64_*`), the fallback value it writes is a **literal, unexpanded
  PowerShell-style variable path** (text beginning `$env:SystemRoot\System32\drivers\UMDF\`),
  which Windows cannot resolve. A plain `System32\drivers\UMDF\RdpIdd.dll` does not exist
  on a clean machine either [L clean].
- Consequence: after the reference's uninstall, `ImagePath` may name a file that does not
  exist. Residue detection must test the target on disk (§8.1), not only the prefix.

### 3.4 IddCx display driver and HDR gate (dependency on spec 40)

The reference has no own IddCx package (it hooks the inbox driver, §3.3). Pulsar Split's
installer carries its own IddCx driver package (`40-display-hdr.md`). Packaging rules:
- HDR / wide colour gamut only through the supported IddCx path. Per `40-display-hdr.md`
  §8, IddCx HDR10/WCG arrives with IddCx 1.10 (Windows 11 24H2, build 26100); on older
  builds seats run SDR and the manager shows the reason (rules.md HDR rule).
- The minimum OS build of the driver package and the exact IddCx version table are
  owned by spec 40; the installer refuses to install the driver on builds below spec 40's
  minimum and records "SDR only" in the service state for builds below the HDR minimum.
- The driver part of the installer is finished only after spec 40's version table is
  fixed [?].

### 3.5 Signing feasibility (evidence) [L][I]

| Observation | Evidence |
| --- | --- |
| The host loads the vendor's self-signed UMDF pad package, trusted only through `Root` + `TrustedPublisher` | [L] driver store signer = vendor cert; HID children existed |
| That host has test-signing **off**, Secure Boot **off**, Memory Integrity **off** | [L] boot configuration, Device Guard status, Secure Boot query |
| UMDF and IddCx drivers are user-mode DLLs in `WUDFHost.exe`; kernel code signing (DSE) and HVCI govern kernel images, not these DLLs | [I] Windows driver model |
| Whether a self-signed UMDF/IddCx package installs and starts with Secure Boot **on** and Memory Integrity **on** | [?] — not observable on the host; testable on the clean ARM64 laptop (Memory Integrity on) |

So the host shows that test-signing mode is **not needed** to load a UMDF package
signed by a locally trusted certificate, at least with Secure Boot off.

### 3.6 Pulsar Split approach (drivers)

- **Own driver packages only, each a normal PnP package** with its own class/ClassGuid,
  root-devnode hardware ids in an own namespace (games still see the emulated VID/PID
  through HID attributes, §3.1), and `PnpLockDown=1`. Installed by the installer (or the
  service on first need) with the supported APIs (`DiInstallDriver` /
  `UpdateDriverForPlugAndPlayDevices`, root devnode via `SwDeviceCreate` or SetupAPI) —
  never by copying into `System32\drivers\UMDF` and never by redirecting an inbox
  driver's image path. No kernel-mode driver is planned.
- **Release signing — open decision** (plan.md "Known risks"): Microsoft attestation
  signing through Partner Center needs an **EV code-signing certificate (paid)**; it is
  accepted on Windows 10/11 client, not on Windows Server (which needs WHQL/HLK). This is
  the leading candidate, not yet chosen; it does not make the product paid for users.
- **Development signing — needs a maintainer ruling.** Test-signing mode (`bcdedit`
  testsigning) is **not** used: it relaxes DSE, which rules.md forbids. Proposed
  alternative, consistent with §3.5: the developer signs packages with a dev-only
  certificate and adds it by hand to their **own** dev machine's `Root` +
  `TrustedPublisher`, never through an installer, and removes it afterwards. This is
  still a trust-store change on that machine, so it needs explicit approval; on the x64
  host PC it additionally needs approval because that machine is read-only by default.
  First verify on the clean ARM64 laptop (Memory Integrity on) that a UMDF/IddCx package
  loads this way (§10).
- **ViGEmBus:** do not bundle by default. It is end-of-life upstream; licence BSD-3
  (redistribution allowed with notice). If a feature needs it, detect an existing install
  by **UpgradeCode** `{67175F6C-AA18-43A7-AE60-2FC3FD10BF79}` or by a present devnode with
  hardware id `Nefarius\ViGEmBus\Gen1` (System class), not by a ProductCode; if missing,
  offer to run the official signed installer with the user's consent. Prefer Pulsar
  Split's own HID pad driver (spec 60). `nefconw` is not needed.
- **Removal:** uninstall removes our devnodes (including non-present ones) and our
  driver-store packages by matching our ClassGuid / provider, through
  `DiUninstallDriver` or `pnputil /delete-driver <oemNN.inf> /uninstall`.
- Separate packages for x64 and ARM64 when an ARM64 host build exists; the INF targets
  only the architectures that were signed.

---

## 4. Uninstaller

### 4.1 What the reference uninstall restores or removes [S]

The uninstall log holds one set of hidden command runs per install; each has a run-once
id, so after an upgrade the same step is not run twice [I]. They run before the logged
files are deleted. Effects (unordered, except as noted below):

- **Service:** `DuoService` stopped and deleted.
- **TermService:** `Parameters\ServiceDll` reset to `%SystemRoot%\System32\termsrv.dll`
  (`REG_EXPAND_SZ`).
- **RDP audio:** `WinStations\RDP-Tcp\AudioEnumeratorDll` reset to `rdpendp.dll`
  (`REG_SZ`).
- **Display hook:** `WUDF\Services\RdpIdd\ImagePath` rewritten (§3.3, including the broken
  fallback).
- **Legacy verifier injection:** Application Verifier values (`GlobalFlag`,
  `VerifierFlags`, `VerifierDlls`, `VerifierActivationFilter`) removed from **every**
  subkey of `Image File Execution Options` machine-wide, and subkeys emptied by that are
  deleted — this also hits other software's entries.
- **Legacy verifier replacement:** `verifier.dll` restored from a `.bak` copy in
  `System32` and `SysWOW64` when one exists.
- **Shim:** `System32\AcRes.dll` and `SysWOW64\AcRes.dll` deleted; machine environment
  variable `__COMPAT_LAYER` deleted; shim database `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}`
  uninstalled by GUID with `sdbinst`. (Pre-1.6 logs lack the shim items.)
- **Display module:** `System32\drivers\UMDF\DuoIdd.dll` deleted.
- **Firewall:** rule `Duo Sunshine` deleted; the `Duo` rule delete never matches (§2.8).
- **Sandbox networking:** NAT `Duo NAT` and Hyper-V switch `Duo Switch` removed.
- Then Inno deletes the logged files and empty folders, the start-menu group and the ARP
  key.

Order facts that matter [S]: the service is stopped before anything else; TermService is
**stopped before** `ServiceDll` is reset and **started after** (with the audio value
reset in between); file deletions come after the registry restores.

**TermService restart cost** [L][I]:
- TermService runs in its own `svchost -k NetworkService` process [L]; it depends on
  `RPCSS` and `StateRepository`; its only dependent is `UmRdpService` (Remote Desktop
  Services UserMode Port Redirector), which is stopped with it [L, both machines].
- The service's strings also name `SessionEnv` and `CertPropSvc` near the TermService
  handling [S]; neither depends on TermService [L]; their role in the stop is [?].
- Stopping TermService drops every RDP connection on the machine, including every seat
  that runs on an RDP-backed session; those sessions end up disconnected or logged off
  [I]. Any change to `ServiceDll` takes effect only through such a restart.

### 4.2 Left behind after uninstall [S][L][I]

| Residue | Effect |
| --- | --- |
| self-signed code-signing root in `Root` and `TrustedPublisher` (§2.5) | machine keeps trusting the vendor key |
| all Defender exclusions (§2.6), including the `WUDFHost.exe` process exclusion | AV blind spots remain |
| kernel helper files `System32\drivers\RTCore64.sys` and `System32\drivers\DuoDriver.sys` | vulnerable signed driver and a second kernel image stay on disk [L]. Their kernel services (`RTCore64`, `DuoDriver`) are created and deleted by the service at run time [S]; no service key remains on the host [L] |
| `C:\ProgramData\dbg` symbol cache | disk use |
| `BluetoothUserService\Parameters\ServiceDll` still pointing at the deleted wrapper DLL (the service does not restore it on stop [L]; the uninstaller has no step for it [S]) | per-user Bluetooth service likely fails to start after uninstall [I] |
| `HKLM\SOFTWARE\Duo` (seat list, settings, stored seat-user credentials) | secrets remain in the registry |
| `config\` folder (per-seat files) | |
| `DuoController` driver-store package(s) and non-present pad devnodes | |
| `Duo` firewall rule(s) | open inbound rule for a deleted path |
| event-log sources `Duo`, `DuoIdd`, `DuoAcRes` (message file `mscoree.dll`) [L] | cosmetic |
| RDS / policy registry values set at service boot (table §8.3) | changed OS behaviour persists |
| `RdpIdd` `ImagePath` naming a non-existent file (ARM64 or missing driver-store folder, §3.3) | inbox RDP display driver cannot load [I] |
| seat user accounts and their profiles | local accounts remain |
| IFEO values/keys of **other** software deleted (§4.1) | collateral damage |

### 4.3 Pulsar Split approach (uninstall)

- **Change journal.** Every system change made by the installer, the service or the
  manager is recorded **before** it is made, then marked done after. Uninstall replays it
  in reverse, restoring exact previous values. Nothing outside the journal is touched; no
  machine-wide sweeps like the IFEO clean-up above. Format proposal in §9.2.
- Default uninstall removes: service, drivers/devnodes, firewall group, shim database and
  env var (if enabled), program files. It **asks** (Turkish UI) whether to also remove:
  seat accounts and profiles, `%ProgramData%\Pulsar Split` (config, keys), multi-session
  settings. Silent uninstall keeps user data unless the MSI property of §9.1 is set.
- Seat-user credentials are removed from secure storage on uninstall regardless.
- Before any step that restarts TermService (restoring a ServiceDll or similar), stop all
  seats first and tell the user that every remote/seat session will drop.
- Uninstall must succeed when the service is already gone, the journal is partly
  missing, or files are locked (schedule delete-on-reboot and tell the user).

---

## 5. Upgrade behaviour [S][L][I]

- Same AppId → Inno upgrades in place and **appends** a new install section to the same
  uninstall log; the 1.6 log contains a full pre-1.6 section followed by the 1.6 section
  [S]. Files present before are flagged as pre-existing in the new section [S][I].
- Files dropped from the new version are **not** removed on upgrade (old verifier DLLs,
  Windows system-file copies, DXVK, ViGEmBus copies stay) [L].
- Registry configuration (`HKLM\SOFTWARE\Duo`) and `config\` survive [L][I].
- Duplicate firewall rules accumulate (§2.8) [L].
- Legacy-method clean-up is done at service start rather than by the installer: restore
  `verifier.dll` from `.bak`, remove verifier IFEO values, uninstall the "multibox"
  Steam add-on (`version.dll` proxy in the Steam folder) [S] (see `20-service.md`).
- Whether setup stops a running service before replacing files is [?] (no uninstall
  record; possibly Inno's Restart Manager support).

**Pulsar Split approach.** MSI `MajorUpgrade` (scheduled after `InstallInitialize`, so a
failed upgrade rolls back to the old version) or NSIS with explicit stop → replace →
start. Every upgrade: stop the service via SCM and wait, replace files, remove files not
in the new manifest, run the service's `reconcile` sub-command (§9.1), start the service
if it was running. Downgrades blocked unless forced. Config schema migrations are
forward-only, with a backup copy of the previous config.

---

## 6. Other system state touched at or around install

| Area | Reference | Pulsar Split |
| --- | --- | --- |
| Environment variables | machine `__COMPAT_LAYER=LoadAppHelp` set by the manager's shim toggle, removed on uninstall [S][L] | only if the shim feature is enabled (spec 50); journaled; never set by the installer |
| Shim database | `%windir%\AppPatch\CustomSDB\{558f7556-3078-a346-a1fa-1b3b45a8b1ff}.sdb`, own ARP entry, `AppCompatFlags\Custom\<exe>` mappings for 11 system/stream/Steam exes, installed with `sdbinst` by the manager [S][L] | own GUID; installed by the service on user request; removed by GUID on uninstall |
| Event log | sources registered by the service at boot [L] | register own source(s) at install (MSI `EventSource`/registry), with an own message DLL or none; removed at uninstall |
| Scheduled tasks | none [L] | none |
| Start menu | manager + uninstaller shortcuts [L] | manager shortcut only (uninstall via Settings → Apps) |
| Symbol cache, kernel helper drivers | service-created (`ProgramData\dbg`, kernel helpers) [S][L] | not applicable (no kernel patching, no symbol download) |
| Seat accounts | created by the manager per seat [S] | same, journaled; optional removal at uninstall |

---

## 7. Signing and legal notes (factual)

| Topic | Reference | Pulsar Split |
| --- | --- | --- |
| Executables / DLLs | Authenticode with vendor self-signed cert, DigiCert timestamp [S] | publicly trusted code-signing certificate (OV/EV, a cloud signing service, or a free OSS signing programme such as SignPath Foundation if accepted); RFC 3161 timestamp; sign MSI/NSIS output too |
| Driver packages | vendor self-signed catalog, trusted via installed root [S][L] | **open decision** (§3.6): attestation signing via Partner Center needs a paid EV cert; WHQL if Server support is needed |
| Microsoft binaries | ships VC runtime DLLs, `ucrtbase.dll`, `WebView2Loader.dll`; older versions shipped `RdpIdd.dll`/`rfxvmt.dll` [S][L] | none, except the WebView2 bootstrapper/offline installer under its redistribution terms |
| Bundled TLS key | identical stream-server key pair in every install [S] | generated per install |
| Third-party | ViGEmBus/nefcon (Nefarius, permissive), DXVK (zlib), stream-server fork (GPL-3) [S] | ship GPL-3 corresponding source for the stream server; notices file in the install folder and in the manager's About screen |
| Emulated pad ids | Sony / Microsoft USB vendor ids in HID attributes [S][L] | same ids for game compatibility (§3.1); no vendor names or logos in UI |
| SmartScreen | signed setup with reputation needed to avoid warnings [I] | expect warnings for new certificates until reputation builds; document it |

---

## 8. Coexistence with an installed reference product

### 8.1 Detection signals

The installer (pre-install check) and the service (each start) evaluate these. Paths are
under `HKLM` unless stated.

| # | Signal | Where (full) | Meaning |
| --- | --- | --- | --- |
| 1 | key `{DB5AA12E-1299-4244-8651-F363406E89F6}_is1` exists | `SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\` | reference installed; `DisplayVersion` gives the version |
| 2 | service `DuoService` | SCM | installed; running state = active |
| 3 | `ServiceDll` ≠ `%SystemRoot%\System32\termsrv.dll` (compare unexpanded, case-insensitive) | `SYSTEM\CurrentControlSet\Services\TermService\Parameters` | some termsrv wrapper active (reference: `%ProgramFiles%\Duo\TermWrap.dll`) |
| 4 | `ServiceDll` ≠ `%SystemRoot%\System32\Microsoft.Bluetooth.UserService.dll` | `SYSTEM\CurrentControlSet\Services\BluetoothUserService\Parameters` | Bluetooth wrapper present (reference: `%ProgramFiles%\Duo\DuoBluetooth.dll`), or residue |
| 5 | `ImagePath` not inside `%SystemRoot%\System32\DriverStore\FileRepository\rdpidd.inf_<arch>_*\` **or** its expanded target does not exist on disk | `SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\RdpIdd` | display hook active, or broken restore (§3.3) |
| 6 | key `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}` exists | `SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\InstalledSDB\` | reference shim database installed |
| 7 | value `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}.sdb` under any `<exe>` subkey | `SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Custom\<exe>` | per-exe shim mapping (the reference maps 11 exes) |
| 8 | ARP key `{558f7556-3078-a346-a1fa-1b3b45a8b1ff}.sdb` | `SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\` | shim database's own ARP entry |
| 9 | file `%windir%\AppPatch\CustomSDB\{558f7556-3078-a346-a1fa-1b3b45a8b1ff}.sdb` | file system | shim database file |
| 10 | machine `__COMPAT_LAYER` contains `LoadAppHelp` | `SYSTEM\CurrentControlSet\Control\Session Manager\Environment` | shim layer forced |
| 11 | driver package of class `DuoController` / ClassGuid `{070D9FF9-4F04-4419-90D2-C4CA9DFDB2EE}`; non-present HID devnodes whose instance id contains `DUOCONTROLLER` | driver store / PnP | reference pad driver present |
| 12 | certificate with thumbprint `0A4FCFD0F177A8BBAE3CF8F8893134D0BB036BB3` (subject `CN=Black-Seraph`) | `Cert:\LocalMachine\Root`, `Cert:\LocalMachine\TrustedPublisher` | reference trust anchor present |
| 13 | `System32\drivers\RTCore64.sys` or `System32\drivers\DuoDriver.sys` present; service keys `SYSTEM\CurrentControlSet\Services\RTCore64` / `\DuoDriver` (absent on the host, but check) | file system / registry | kernel-helper residue (warn only) |
| 14 | Defender exclusions of §2.6 | Defender preferences (read) | AV blind spots (report only) |
| 15 | firewall rules with display name `Duo` or `Duo Sunshine` | firewall | open program rules |
| 16 | RDS / policy values differing from the Windows defaults of §8.3 | registry | boot-time tweaks active or residue |
| 17 | **upstream Sunshine**: service `SunshineService` (display name `Sunshine Service`, image `"C:\Program Files\Sunshine\tools\sunshinesvc.exe"`, LocalSystem, automatic); program `C:\Program Files\Sunshine\sunshine.exe`; firewall rules display name `Sunshine` (one TCP, one UDP, program-scoped, any port) | SCM / firewall | another stream server; on the host it listens on TCP 47984, 47989, 47990 (web UI), 48010 on all interfaces [L]; its default block 47984–48010 must not overlap a seat block |
| 18 | ViGEmBus by UpgradeCode or hardware id (§3.2) | Installer registry / PnP | third-party bus present (informational) |

### 8.2 Behaviour

```
installer start
   |
   v
reference detected (signals 1-2)? --no--> residue (3-16)? --no--> normal install
   | yes                                       | yes
   v                                           v
coexistence page (Turkish): what was found, what Pulsar Split will NOT touch
   |
   +-- reference service running ----> install allowed; Pulsar Split service starts in
   |                                   "passive" mode: no termsrv/RDS/Bluetooth/RdpIdd/
   |                                   shim changes, seats disabled, manager explains why
   |
   +-- reference installed, stopped -> same passive mode until the operator disables or
   |                                   uninstalls the reference product
   |
   +-- only residue (no ARP key) ----> offer "repair residue": restore the Windows defaults
                                       of §8.3 for the registry signals, per item, opt-in,
                                       journaled; never deletes the reference's
                                       certificate, exclusions or data silently
upstream Sunshine (17) ---> independent of the above: seat port blocks avoid its ports;
                            never stop or reconfigure it
```

- Pulsar Split never edits the reference product's registry root, files, service, rules,
  shim database or driver packages.
- Repair items that need a TermService restart (signal 3) are grouped last, run only after
  the user confirms that all remote sessions will drop (§4.1).
- Optional, opt-in import: seat names, ports and display settings from the reference's
  registry into Pulsar Split config. Stored passwords are **not** imported; the operator
  re-enters them.
- Pulsar (the client umbrella app) keeps its existing support for hosts that run the
  reference product; this section only governs the Pulsar Split host stack.

### 8.3 Windows defaults for "repair residue"

Read from the **clean** machine (build 28000, ARM64) and compared with the **host**
(build 26300, x64) [L]. "absent" = value or key does not exist on a clean install; repair
deletes it. Pulsar Split's own journal still wins over this table when it holds a
recorded previous value.

| Key (HKLM) | Value | Type | Clean default | Host (reference) |
| --- | --- | --- | --- | --- |
| `SYSTEM\CurrentControlSet\Services\TermService\Parameters` | `ServiceDll` | REG_EXPAND_SZ | `%SystemRoot%\System32\termsrv.dll` | `%ProgramFiles%\Duo\TermWrap.dll` |
| `SYSTEM\CurrentControlSet\Services\BluetoothUserService\Parameters` | `ServiceDll` | REG_EXPAND_SZ | `%SystemRoot%\System32\Microsoft.Bluetooth.UserService.dll` | `%ProgramFiles%\Duo\DuoBluetooth.dll` |
| `SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\RdpIdd` | `ImagePath` | REG_EXPAND_SZ | `%SystemRoot%\System32\DriverStore\FileRepository\rdpidd.inf_<arch>_<hash>\RdpIdd.dll` — arch `amd64` or `arm64`, hash per build (clean: `rdpidd.inf_arm64_3a8811cf7c585fd8`; host: `rdpidd.inf_amd64_0e176717b3c7575d`). Repair resolves the folder at run time; if several exist, the one the installed `rdpidd.inf` driver package uses | same form (restored while stopped) |
| `SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp` | `AudioEnumeratorDll` | REG_SZ | `rdpendp.dll` | `rdpendp.dll` |
| same | `Shadow` | REG_DWORD | 1 | 0 |
| same | `UserAuthentication` | REG_DWORD | 1 | 1 |
| same | `SecurityLayer` | REG_DWORD | 2 | 2 |
| `SYSTEM\CurrentControlSet\Control\Terminal Server` | `fDenyTSConnections` | REG_DWORD | 1 | 0 |
| same | `fSingleSessionPerUser` | REG_DWORD | 1 | 0 |
| same | `ContainerMode` | — | absent | absent |
| `…\Terminal Server\Licensing Core` | `EnableConcurrentSessions` | REG_DWORD | absent (key absent) | 1 |
| `…\Terminal Server\WinStations` | `SetGpuRealtimePriority` | REG_DWORD | absent | 1 |
| `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon` | `AllowMultipleTSSessions` | REG_DWORD | absent | 1 |
| `…\Terminal Server\AddIns\Clip Redirector` | `Name` / `Type` | REG_SZ / REG_DWORD | `RDPClip` / 3 | same |
| `…\Terminal Server\AddIns\DND Redirector` | `Name` / `Type` | REG_SZ / REG_DWORD | `RDPDND` / 3 | same |
| `…\Terminal Server\AddIns\Dynamic VC` | `Type` | REG_DWORD | 0xFFFFFFFF | same |
| `SYSTEM\CurrentControlSet\Control` | `SvcHostSplitThresholdInKB` | REG_DWORD | 3670016 | 3670016 |
| `SYSTEM\CurrentControlSet\Control\Lsa` | `LimitBlankPasswordUse` | REG_DWORD | 1 | 0 |
| `SYSTEM\CurrentControlSet\Control\GraphicsDrivers` | `TerminateIndirectOnStall` | REG_DWORD | absent | 0 |
| same | `IddCxDebugCtrl` | REG_DWORD | absent | 128 |
| `SYSTEM\CurrentControlSet\Control\Keyboard Layout` | `IgnoreRemoteKeyboardLayout` | REG_DWORD | absent | 1 |
| `SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services` | `bEnumerateHWBeforeSW`, `fEnableWddmDriver`, `fEnableVirtualizedGraphics`, `VGOptimization_CaptureFrameRate`, `VGOptimization_CompressionRatio`, `VisualExperiencePolicy`, `MaxCompressionLevel`, `ImageQuality`, `GraphicsProfile`, `AVCHardwareEncodePreferred`, `AVC444ModePreferred` | REG_DWORD | all absent (key holds no values) | first nine present (1,1,1,1,1,1,1,2,1); the two AVC values absent at probe time |
| same | `Shadow`, `fDenyTSConnections`, `AllowedAudioQualityMode` | REG_DWORD | absent | 0, 0, 3 — not in the service's string table; origin [?] (spec 70 / other software) — report, do not repair |
| `SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services\Client` | `EnableHardwareMode`, `fUsbRedirectionEnableMode` | REG_DWORD | absent | 1, 2 |
| `SOFTWARE\Microsoft\PolicyManager\default\Connectivity\DisableCrossDeviceResume` | `value` | REG_DWORD | 0 | 1 |
| `SYSTEM\CurrentControlSet\Control\Session Manager\Power` | `HiberbootEnabled` | REG_DWORD | 1 | 0 |
| `SOFTWARE\Microsoft\Input\Settings\ControllerProcessor\ControllerToVKMapping` | `Enabled` | REG_DWORD | absent (key absent) | 0 |
| `SYSTEM\CurrentControlSet\Control\FeatureManagement\Overrides\8\<id>` | `EnabledState`, `EnabledStateOptions` | REG_DWORD | key `Overrides\8` absent | ten feature-id subkeys (`205101711`, `940342926`, `1285421709`, `1633192078`, `1709592718`, `1882033807`, `2304247949`, `2413937293`, `2856873612`, `3805132942`), each with `EnabledStateOptions`=0 and `EnabledState` 1 or 2 |
| `SYSTEM\CurrentControlSet\Control\Hvsi` | (key) | — | absent | absent (the service deletes it) |
| `SYSTEM\CurrentControlSet\Control\Session Manager\Environment` | `__COMPAT_LAYER` | REG_SZ | absent | `LoadAppHelp` |

Notes [I]: the PolicyManager value under `DisableCrossDeviceResume` also has metadata
values (`Behavior`, `policytype`, ranges) that match on both machines; only `value`
differs. The Overrides IDs may also be written by Windows servicing or other tools;
repair removes only the ten listed IDs. Values marked "same" in both columns need no
repair.

---

## 9. Recommended Pulsar Split installer shape

| Concern | Choice |
| --- | --- |
| Format | Tauri 2 bundler: **WiX MSI** as the primary artefact (service tables, major upgrade, rollback, GPO/Intune friendly) plus optional NSIS `-setup.exe`; custom actions only call our own signed binaries |
| Arch | x64 first; ARM64 package when the service/drivers have ARM64 builds; installer refuses a mismatched arch |
| Elevation | per-machine, elevated |
| System changes | done by the service's idempotent sub-commands (§9.1), all journaled (§9.2) |
| Forbidden | root/TrustedPublisher certs, AV exclusions, copying files into `System32`, redirecting inbox driver images, vulnerable drivers, kernel patching, DSE/HVCI/test-signing changes, licence or activation checks, local web server or web UI assets |
| Network | firewall group with the per-seat port rules of §2.8 only |
| Data | `%ProgramData%\Pulsar Split` with SYSTEM + Administrators ACL; secrets via DPAPI machine scope |

The items below are **design proposals, not observed behaviour**; the implementer's
design doc fixes the details.

### 9.1 Service sub-commands and installer integration (proposal)

| Sub-command | Caller | Effect | Idempotent |
| --- | --- | --- | --- |
| `install` | MSI deferred custom action / NSIS hook (MSI may use `ServiceInstall` instead and call only `reconcile`) | register event source, create data folder + ACL, install driver packages | yes |
| `reconcile` | after install and every upgrade | apply journaled settings, migrate config schema, upgrade driver packages in place | yes |
| `uninstall [--remove-user-data]` | MSI/NSIS uninstall | replay journal in reverse, remove drivers, firewall group, event source; with the flag also data folder and seat accounts | yes; succeeds when nothing is left |
| `check-coexistence --json` | installer UI | evaluates §8.1 signals, prints a machine-readable report | read-only |

Exit codes: 0 success; 3010 success, reboot required (standard MSI meaning); 1 failure
(details in the log); 2 partial (some items skipped, listed in the log). Every
sub-command writes a log under `%ProgramData%\Pulsar Split\logs`.

MSI property for silent uninstall user-data removal: `REMOVEUSERDATA=1` (public, default
empty). NSIS equivalent: `/REMOVEUSERDATA`.

Tauri 2 integration: WiX — `bundle.windows.wix.fragmentPaths` adds a fragment with the
`ServiceInstall`/`ServiceControl` elements and custom actions, `componentRefs` /
`featureRefs` pull them into the product; NSIS — `bundle.windows.nsis.installerHooks`
points at a hook script that calls the sub-commands in its pre/post install and
uninstall macros.

### 9.2 Change journal (proposal)

- Location `%ProgramData%\Pulsar Split\journal\`, ACL SYSTEM + Administrators.
- One record per change, appended as one line of JSON (JSON Lines) with: sequence number,
  time, actor (installer/service/manager), kind (registry, file, service, firewall,
  driver, account, feature), target (full path / name), previous state (value + type, or
  `absent`), new state, status (`intent` then `done`).
- Atomicity: the `intent` record is flushed to disk (write-through) before the change; the
  `done` record after. Replay treats an `intent` without `done` as "maybe applied":
  compare live state with both previous and new state before restoring.
- Compaction: rewrite to a new file and atomically replace (`ReplaceFile`/`MoveFileEx`
  with replace-existing), keeping the previous file as `.bak`.

---

## 10. Open questions

- [?] Install-time steps of the reference setup that leave no uninstall record (service
  creation, firewall rule creation, old ViGEmBus/nefconw arguments) — original setup exe
  not available.
- [?] DISM add-package source and the build threshold for "container support".
- [?] Whether the reference setup stops the service before overwriting files.
- [?] Whether the service ever restores `BluetoothUserService` `ServiceDll`.
- [?] Role of `SessionEnv` and `CertPropSvc` in the reference's TermService restart.
- [?] Whether a UMDF / IddCx package signed by a locally trusted (non-Microsoft)
  certificate installs and starts with Secure Boot **on** and Memory Integrity **on**.
  Test on the clean ARM64 laptop once the maintainer rules on the dev-signing path
  (§3.6).
- [?] Release signing route (attestation with paid EV cert vs. other) — maintainer
  decision (plan.md risks).
- [?] Exact minimum OS build / IddCx version for the driver package and for HDR — owned by
  `40-display-hdr.md`; the installer's driver part waits on it.
- [?] Whether the HID-attribute VID/PID alone (with an own-namespace root hardware id)
  is enough for every target game/launcher; some may also inspect the parent devnode or
  bus type. Verify with the own pad driver (spec 60).
- [?] Origin of the extra policy values on the host (`Shadow`, `fDenyTSConnections`,
  `AllowedAudioQualityMode` under the Terminal Services policy key) and whether the two
  AVC policy values are written only under some condition.
- [?] Meaning of the ten FeatureManagement override IDs.
- [?] UDP ports of upstream Sunshine while streaming (only TCP listeners observed idle).
