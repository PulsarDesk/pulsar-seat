// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V VM seats, host side (hyperv-seats 2026-11-17; spec/remote 10 §8, 90 §2.4): the
//! PowerShell scripts the service runs to build the base disk ("zygote"), prepare the VM
//! network, create / start / stop / reset / remove a seat's VM, read the VM state, and undo
//! everything at uninstall; plus the journal of what Pulsar created.
//!
//! Only documented cmdlets are used: Hyper-V (`New-VM`, `New-VHD -Differencing`,
//! `Add-VMGpuPartitionAdapter`, `Set-VMFirmware`, `Enable-VMTPM`, `Get-VMHostPartitionableGpu`,
//! ...), Storage / DISM (`Mount-VHD`, `Initialize-Disk`, `New-Partition`, `Expand-WindowsImage`,
//! `Add-WindowsPackage`, `bcdboot`), NetNat (`New-NetNat`, `Add-NetNatStaticMapping`),
//! NetSecurity (`New-NetFirewallRule`) and SmbShare (`New-SmbShare`). The scripts run in a
//! non-interactive `powershell.exe` started by the service; passwords reach them on stdin
//! (first line), never in the script text.
//!
//! Naming: every object is called `Pulsar Seat VM...` / `PulsarSeat-<seat id>` so status and
//! uninstall can find it. The Hyper-V feature itself is never turned on here: that stays the
//! visible opt-in of the Hyper-V helper (`hyperv.rs`, reboot notice).
//!
//! HYPOTHESIS: no script ran on a Hyper-V host yet. GPU partitioning on client Windows
//! needs the host's display driver files inside the guest (`HostDriverStore`); the copy step
//! follows the publicly documented GPU-P guest requirements and is unverified.

use crate::hyperv::ps_quote;
use seat_core::remote::vm::{Forward, GpuPartitionMode, ImageKind, VmSettings, VmStopMode};
use serde::{Deserialize, Serialize};

/// Internal switch and NAT of the VM network.
pub const VM_SWITCH: &str = "Pulsar Seat VM";
pub const VM_NAT: &str = "Pulsar Seat VM";
/// Firewall rule group of the forwarded host ports.
pub const FIREWALL_GROUP: &str = "Pulsar Seat VM";
/// The one share account of every VM seat before the parity-2 review (2026-11-17). Now
/// each seat has its own ([`share_account`]); this one is only removed (upgrade, uninstall).
pub const LEGACY_SHARE_ACCOUNT: &str = "PulsarSeatVmShare";
/// Prefix of the per-seat share accounts.
pub const SHARE_ACCOUNT_PREFIX: &str = "PulsarVm";
/// VM name prefix.
pub const VM_PREFIX: &str = "PulsarSeat-";
/// Journal file (under the service's journal folder).
pub const JOURNAL_FILE: &str = "hyperv-vm.json";
/// Install folder of Pulsar Seat inside the guest (copied from the host's install dir).
pub const GUEST_INSTALL_DIR: &str = r"C:\Program Files\Pulsar Seat";
/// Guest-mode service name inside the VM.
pub const GUEST_SERVICE: &str = "PulsarSeatGuest";

/// VM name of a seat (by its stable id).
pub fn vm_name(seat_id: &str) -> String {
    format!("{VM_PREFIX}{seat_id}")
}

/// Seat id of a VM name.
pub fn seat_id_of(vm: &str) -> Option<&str> {
    vm.strip_prefix(VM_PREFIX).filter(|s| !s.is_empty())
}

/// Base disk path.
pub fn base_disk(storage: &str) -> String {
    format!(r"{}\zygote.vhdx", storage.trim_end_matches('\\'))
}

/// A seat's folder and differencing disk.
pub fn seat_dir(storage: &str, seat_id: &str) -> String {
    format!(r"{}\seats\{seat_id}", storage.trim_end_matches('\\'))
}
pub fn seat_disk(storage: &str, seat_id: &str) -> String {
    format!(r"{}\sandbox.vhdx", seat_dir(storage, seat_id))
}

fn short_id(seat_id: &str) -> String {
    seat_id.chars().filter(char::is_ascii_hexdigit).take(8).collect::<String>().to_ascii_lowercase()
}

/// SMB share name of a seat's shared folder `index` (hidden share, `$`).
pub fn share_name(seat_id: &str, index: usize) -> String {
    format!("{SHARE_ACCOUNT_PREFIX}{}-{index}$", short_id(seat_id))
}

/// Host account of one VM seat's shared folders (review 2026-11-17: one account per seat,
/// so a guest's credentials open only its own seat's shares, and a seat start rotates only
/// its own password). 16 characters, within the 20 of a local account name.
pub fn share_account(seat_id: &str) -> String {
    format!("{SHARE_ACCOUNT_PREFIX}{}", short_id(seat_id))
}

/// Share account that owns share `name` (`PulsarVm<short>-<n>$`).
pub fn share_account_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix(SHARE_ACCOUNT_PREFIX)?;
    let short = rest.split('-').next()?;
    (short.len() == 8 && short.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("{SHARE_ACCOUNT_PREFIX}{short}"))
}

/// Firewall rule display name of a seat.
pub fn firewall_rule(seat_id: &str) -> String {
    format!("Pulsar Seat VM {seat_id}")
}

/// Seat ids are GUIDs: only hex digits and dashes reach a script.
pub fn seat_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// GPU choice of a seat after validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "adapter", rename_all = "snake_case")]
pub enum GpuChoice {
    Off,
    Auto,
    Adapter(String),
}

impl GpuChoice {
    pub fn from_seat(mode: GpuPartitionMode, adapter: Option<&str>) -> Self {
        match (mode, adapter) {
            (GpuPartitionMode::Off, _) => Self::Off,
            (GpuPartitionMode::Auto, _) => Self::Auto,
            (GpuPartitionMode::Adapter, Some(a)) => Self::Adapter(a.to_string()),
            (GpuPartitionMode::Adapter, None) => Self::Off,
        }
    }
    pub fn is_on(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// One shared folder as the host offers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareSpec {
    pub name: String,
    pub host_path: String,
    pub read_only: bool,
}

/// Everything the scripts need about one VM seat (built by the service from the config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSpec {
    pub seat_id: String,
    pub storage: String,
    pub memory_mb: u32,
    pub processors: u32,
    pub gpu: GpuChoice,
    pub forwards: Vec<Forward>,
    pub guest_address: String,
    pub shares: Vec<ShareSpec>,
    pub stop: VmStopMode,
}

/// Machine-wide inputs of the host scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    pub storage: String,
    pub nat_prefix: String,
    /// Install dir of Pulsar Seat on the host (copied into the base disk).
    pub payload_dir: String,
    pub settings: VmSettings,
}

/// One action of the Platform (`Platform::hyperv_vm`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum VmOp {
    /// Host facts and every Pulsar VM.
    Status { storage: String },
    /// Switch + host address + NAT + guest link registration.
    PrepareHost { host: HostSpec },
    /// Build the base disk from the image (minutes).
    BuildBase { host: HostSpec },
    /// Delete the base disk and every seat disk (all VM seats off).
    ResetBase { storage: String, seat_ids: Vec<String> },
    /// Create the seat's VM (and disk) when missing, apply memory / CPU / GPU / forwards /
    /// shares. The share account password comes on stdin.
    Ensure { spec: VmSpec },
    Start { seat_id: String },
    Stop { seat_id: String, mode: VmStopMode },
    /// Delete and recreate the seat's differencing disk (VM off).
    ResetSandbox { spec: VmSpec },
    /// Remove the VM, its disk, forwards, firewall rule and shares.
    Remove { spec: VmSpec },
    /// Restart the host's display adapters (`restart_gpus_on_shutdown`).
    RestartGpus,
}

/// State of one Pulsar VM.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmState {
    pub seat_id: String,
    /// Hyper-V VM id (GUID): the guest link names the VM by it.
    #[serde(default)]
    pub vm_id: String,
    /// Hyper-V state lower case: `off`, `running`, `saved`, `paused`, `starting`, `stopping`, ...
    pub state: String,
    pub uptime_s: u64,
}

/// Answer of `VmOp::Status` (and of the other ops, after them).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HostFacts {
    /// `enabled` / `disabled` / `enable_pending` / `unavailable` (Microsoft-Hyper-V-All).
    pub feature: String,
    /// The Hyper-V management service (`vmms`) is present and running.
    pub vmms: bool,
    pub switch_present: bool,
    pub nat_present: bool,
    pub base_present: bool,
    /// Instance paths of the partitionable GPUs.
    pub gpus: Vec<String>,
    pub host_memory_mb: u64,
    pub logical_processors: u32,
    pub vms: Vec<VmState>,
    /// IPv4 DNS servers of the host (handed to the guests).
    #[serde(default)]
    pub dns: Vec<String>,
}

fn q(s: &str) -> String {
    ps_quote(s)
}

/// Status script: one JSON object.
pub fn script_status(storage: &str) -> String {
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         $f = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All; \
         $state = if ($f) {{ [string]$f.State }} else {{ 'unavailable' }}; \
         $svc = Get-Service -Name vmms; \
         $cs = Get-CimInstance Win32_ComputerSystem; \
         $gpus = @(Get-VMHostPartitionableGpu | ForEach-Object {{ $_.Name }}); \
         $vms = @(Get-VM | Where-Object {{ $_.Name -like {prefix} }} | ForEach-Object {{ @{{ name = $_.Name; id = [string]$_.Id; state = [string]$_.State; uptime = [int64]$_.Uptime.TotalSeconds }} }}); \
         @{{ feature = $state; vmms = ($svc -and $svc.Status -eq 'Running'); \
            switch = $null -ne (Get-VMSwitch -Name {sw}); nat = $null -ne (Get-NetNat -Name {nat}); \
            base = (Test-Path -LiteralPath {base}); gpus = $gpus; \
            memory_mb = [int64]($cs.TotalPhysicalMemory / 1MB); cpus = [int]$cs.NumberOfLogicalProcessors; vms = $vms; \
            dns = @(Get-DnsClientServerAddress -AddressFamily IPv4 | ForEach-Object {{ $_.ServerAddresses }} | Where-Object {{ $_ -notlike '127.*' }} | Select-Object -Unique) }} \
         | ConvertTo-Json -Depth 4 -Compress",
        prefix = q(&format!("{VM_PREFIX}*")),
        sw = q(VM_SWITCH),
        nat = q(VM_NAT),
        base = q(&base_disk(storage)),
    )
}

#[derive(Deserialize)]
struct RawFacts {
    feature: Option<String>,
    vmms: Option<bool>,
    switch: Option<bool>,
    nat: Option<bool>,
    base: Option<bool>,
    gpus: Option<serde_json::Value>,
    memory_mb: Option<u64>,
    cpus: Option<u32>,
    vms: Option<serde_json::Value>,
    dns: Option<serde_json::Value>,
}

/// ConvertTo-Json writes one object for a one-element array and a bare string for a
/// one-element string array.
fn as_list(v: Option<serde_json::Value>) -> Vec<serde_json::Value> {
    match v {
        Some(serde_json::Value::Array(a)) => a,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(other) => vec![other],
    }
}

/// Parse the status script's output.
pub fn parse_status(json: &str) -> Result<HostFacts, String> {
    let raw: RawFacts = serde_json::from_str(json.trim()).map_err(|e| format!("vm status: {e}"))?;
    let feature = match raw.feature.unwrap_or_default().to_ascii_lowercase().as_str() {
        "enabled" => "enabled",
        "disabled" | "disabledwithpayloadremoved" => "disabled",
        "enablepending" => "enable_pending",
        _ => "unavailable",
    }
    .to_string();
    let mut vms: Vec<VmState> = as_list(raw.vms)
        .iter()
        .filter_map(|v| {
            let id = seat_id_of(v.get("name")?.as_str()?)?.to_string();
            Some(VmState {
                seat_id: id,
                vm_id: v.get("id").and_then(|s| s.as_str()).unwrap_or_default().to_ascii_lowercase(),
                state: v.get("state").and_then(|s| s.as_str()).unwrap_or("unknown").to_ascii_lowercase(),
                uptime_s: v.get("uptime").and_then(|s| s.as_u64()).unwrap_or(0),
            })
        })
        .collect();
    vms.sort_by(|a, b| a.seat_id.cmp(&b.seat_id));
    Ok(HostFacts {
        feature,
        vmms: raw.vmms.unwrap_or(false),
        switch_present: raw.switch.unwrap_or(false),
        nat_present: raw.nat.unwrap_or(false),
        base_present: raw.base.unwrap_or(false),
        gpus: as_list(raw.gpus).iter().filter_map(|g| g.as_str().map(str::to_string)).collect(),
        host_memory_mb: raw.memory_mb.unwrap_or(0),
        logical_processors: raw.cpus.unwrap_or(0),
        vms,
        dns: as_list(raw.dns).iter().filter_map(|g| g.as_str().map(str::to_string)).filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok()).collect(),
    })
}

/// Switch, host gateway address, NAT and the guest link service registration.
pub fn script_prepare_host(nat_prefix: &str) -> Result<String, String> {
    let (net, len) = seat_core::remote::vm::parse_prefix(nat_prefix).ok_or("invalid NAT prefix")?;
    let gw = seat_core::remote::vm::gateway(nat_prefix).ok_or("invalid NAT prefix")?;
    let reg = format!(
        r"HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\{}",
        seat_core::remote::vm::GUEST_LINK_SERVICE_ID
    );
    Ok(format!(
        "$ErrorActionPreference='Stop'; \
         if (-not (Get-VMSwitch -Name {sw} -ErrorAction SilentlyContinue)) {{ New-VMSwitch -Name {sw} -SwitchType Internal | Out-Null }}; \
         $alias = 'vEthernet (' + {sw} + ')'; \
         if (-not (Get-NetIPAddress -InterfaceAlias $alias -IPAddress {gw} -ErrorAction SilentlyContinue)) {{ \
           Get-NetIPAddress -InterfaceAlias $alias -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false; \
           New-NetIPAddress -InterfaceAlias $alias -IPAddress {gw} -PrefixLength {len} | Out-Null }}; \
         if (-not (Get-NetNat -Name {nat} -ErrorAction SilentlyContinue)) {{ New-NetNat -Name {nat} -InternalIPInterfaceAddressPrefix {prefix} | Out-Null }}; \
         if (-not (Test-Path -LiteralPath {reg})) {{ New-Item -Path {reg} -Force | Out-Null }}; \
         New-ItemProperty -Path {reg} -Name ElementName -Value 'Pulsar Seat guest link' -PropertyType String -Force | Out-Null",
        sw = q(VM_SWITCH),
        nat = q(VM_NAT),
        gw = q(&gw.to_string()),
        prefix = q(&format!("{net}/{len}")),
        reg = q(&reg),
    ))
}

/// Unattend answer file of the base disk: skips the out-of-box screens and names no
/// account and no password (the guest service creates the seat user with the password it
/// gets over the guest link). HYPOTHESIS: the OOBE skip settings are honoured by current
/// Windows 11 builds.
pub fn unattend_xml(arch: &str) -> String {
    let arch = if arch == "arm64" { "arm64" } else { "amd64" };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
        <HideLocalAccountScreen>true</HideLocalAccountScreen>
        <ProtectYourPC>3</ProtectYourPC>
        <SkipMachineOOBE>true</SkipMachineOOBE>
        <SkipUserOOBE>true</SkipUserOOBE>
      </OOBE>
    </component>
  </settings>
</unattend>
"#
    )
}

/// `SetupComplete.cmd` of the base disk: registers the guest mode as a boot task running
/// as LocalSystem (`pulsar-seat-service vm-guest`; a plain process, no SCM handshake), and
/// starts it once.
pub fn setup_complete_cmd() -> String {
    format!(
        "@echo off\r\nschtasks.exe /Create /F /TN {svc} /SC ONSTART /RU SYSTEM /RL HIGHEST /TR \"\\\"{dir}\\pulsar-seat-service.exe\\\" vm-guest\"\r\n\
         schtasks.exe /Run /TN {svc}\r\n",
        svc = GUEST_SERVICE,
        dir = GUEST_INSTALL_DIR,
    )
}

/// Guest side: apply a seat's config inside the VM. Reads the seat password (line 1) and
/// the share account password (line 2) from stdin. Network (static address, gateway, the
/// host's DNS servers), the seat user (created when missing, password set, member of
/// Users), shared folders (global SMB mappings with the share account; drive letters
/// mapped, `C:\...` mount points as symbolic links to the share). `cfg` must have passed
/// [`seat_core::remote::vm::GuestConfig::check`].
pub fn script_guest_apply(cfg: &seat_core::remote::vm::GuestConfig) -> String {
    let dns = if cfg.dns.is_empty() { q(&cfg.gateway) } else { cfg.dns.iter().map(|d| q(d)).collect::<Vec<_>>().join(",") };
    let mut s = format!(
        "$ErrorActionPreference='Stop'; $pw = [Console]::In.ReadLine(); $spw = [Console]::In.ReadLine(); \
         $if = Get-NetAdapter | Where-Object {{ $_.Status -eq 'Up' }} | Sort-Object ifIndex | Select-Object -First 1; \
         if (-not (Get-NetIPAddress -InterfaceIndex $if.ifIndex -IPAddress {ip} -ErrorAction SilentlyContinue)) {{ \
           Get-NetIPAddress -InterfaceIndex $if.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false; \
           Get-NetRoute -InterfaceIndex $if.ifIndex -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false; \
           New-NetIPAddress -InterfaceIndex $if.ifIndex -IPAddress {ip} -PrefixLength {len} -DefaultGateway {gw} | Out-Null }}; \
         Set-DnsClientServerAddress -InterfaceIndex $if.ifIndex -ServerAddresses @({dns}); \
         $sec = ConvertTo-SecureString -String $pw -AsPlainText -Force; \
         if (Get-LocalUser -Name {user} -ErrorAction SilentlyContinue) {{ Set-LocalUser -Name {user} -Password $sec; Enable-LocalUser -Name {user} }} \
         else {{ New-LocalUser -Name {user} -Password $sec -PasswordNeverExpires | Out-Null }}; \
         if (-not (Get-LocalGroupMember -SID 'S-1-5-32-545' -Member {user} -ErrorAction SilentlyContinue)) {{ Add-LocalGroupMember -SID 'S-1-5-32-545' -Member {user} }}; \
         $cred = New-Object System.Management.Automation.PSCredential ({gwq} + '\\' + {suser}), (ConvertTo-SecureString -String $spw -AsPlainText -Force); ",
        ip = q(&cfg.address),
        len = cfg.prefix_len,
        gw = q(&cfg.gateway),
        gwq = q(&cfg.gateway),
        dns = dns,
        user = q(&cfg.user_name),
        suser = q(&cfg.share_user),
    );
    for sh in &cfg.shares {
        let p = &sh.guest_path;
        let drive = p.len() <= 3 && !p[..1].eq_ignore_ascii_case("c");
        if drive {
            s.push_str(&format!(
                "Get-SmbGlobalMapping -RemotePath {unc} -ErrorAction SilentlyContinue | Remove-SmbGlobalMapping -Force; \
                 New-SmbGlobalMapping -RemotePath {unc} -Credential $cred -LocalPath {l} -Persistent $true | Out-Null; ",
                unc = q(&sh.unc),
                l = q(&p[..2]),
            ));
        } else {
            s.push_str(&format!(
                "Get-SmbGlobalMapping -RemotePath {unc} -ErrorAction SilentlyContinue | Remove-SmbGlobalMapping -Force; \
                 New-SmbGlobalMapping -RemotePath {unc} -Credential $cred -Persistent $true | Out-Null; \
                 if (Test-Path -LiteralPath {p}) {{ (Get-Item -LiteralPath {p}).Delete() }}; \
                 New-Item -ItemType Directory -Force -Path (Split-Path -Parent {p}) | Out-Null; \
                 New-Item -ItemType SymbolicLink -Path {p} -Target {unc} | Out-Null; ",
                unc = q(&sh.unc),
                p = q(p),
            ));
        }
    }
    s.push_str("'ok'");
    s
}

/// Copy the host's payload, the unattend file and `SetupComplete.cmd` into the Windows
/// volume `$w` (a drive root such as `F:\`).
fn inject_payload(payload_dir: &str) -> String {
    format!(
        "$dst = Join-Path $w 'Program Files\\Pulsar Seat'; New-Item -ItemType Directory -Force -Path $dst | Out-Null; \
         Copy-Item -Path (Join-Path {src} '*') -Destination $dst -Recurse -Force; \
         $panther = Join-Path $w 'Windows\\Panther'; New-Item -ItemType Directory -Force -Path $panther | Out-Null; \
         Set-Content -LiteralPath (Join-Path $panther 'unattend.xml') -Value $unattend -Encoding UTF8; \
         $scripts = Join-Path $w 'Windows\\Setup\\Scripts'; New-Item -ItemType Directory -Force -Path $scripts | Out-Null; \
         Set-Content -LiteralPath (Join-Path $scripts 'SetupComplete.cmd') -Value $setup -Encoding ASCII",
        src = q(payload_dir),
    )
}

/// Build the base disk: from a `.wim` / `.esd` (new GPT disk, EFI + MSR + Windows
/// partitions, image applied, optional language pack, boot files) or from a prepared
/// `.vhdx` (copied). Then the payload is injected and the disk made read-only.
pub fn script_build_base(host: &HostSpec, arch: &str) -> Result<String, String> {
    let s = &host.settings;
    let base = base_disk(&host.storage);
    let head = format!(
        "$ErrorActionPreference='Stop'; New-Item -ItemType Directory -Force -Path {dir} | Out-Null; \
         if (Test-Path -LiteralPath {base}) {{ throw 'base disk exists; reset it first' }}; \
         $unattend = @'\n{unattend}'@\n; $setup = @'\n{setup}'@\n; ",
        dir = q(&host.storage),
        base = q(&base),
        unattend = unattend_xml(arch),
        setup = setup_complete_cmd(),
    );
    let body = match s.image_kind {
        ImageKind::None => return Err("no base image chosen".into()),
        ImageKind::Wim => {
            let cab = if s.language_cab.is_empty() {
                String::new()
            } else {
                format!("Add-WindowsPackage -Path $w -PackagePath {} | Out-Null; ", q(&s.language_cab))
            };
            format!(
                "New-VHD -Path {base} -SizeBytes {size}GB -Dynamic | Out-Null; \
                 $d = Mount-VHD -Path {base} -Passthru | Get-Disk; \
                 try {{ Initialize-Disk -Number $d.Number -PartitionStyle GPT; \
                   $efi = New-Partition -DiskNumber $d.Number -Size 260MB -GptType '{{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}}' -AssignDriveLetter; \
                   Format-Volume -Partition $efi -FileSystem FAT32 -Confirm:$false | Out-Null; \
                   New-Partition -DiskNumber $d.Number -Size 16MB -GptType '{{e3c9e316-0b5c-4db8-817d-f92df00215ae}}' | Out-Null; \
                   $win = New-Partition -DiskNumber $d.Number -UseMaximumSize -AssignDriveLetter; \
                   Format-Volume -Partition $win -FileSystem NTFS -Confirm:$false | Out-Null; \
                   $w = $win.DriveLetter + ':\\'; $s = $efi.DriveLetter + ':'; \
                   Expand-WindowsImage -ImagePath {img} -Index {idx} -ApplyPath $w | Out-Null; \
                   {cab}& bcdboot.exe (Join-Path $w 'Windows') /s $s /f UEFI | Out-Null; \
                   {inject} }} finally {{ Dismount-VHD -Path {base} }}; ",
                base = q(&base),
                size = s.storage_gb,
                img = q(&s.image_path),
                idx = s.image_index,
                cab = cab,
                inject = inject_payload(&host.payload_dir),
            )
        }
        ImageKind::Vhdx => format!(
            "Copy-Item -LiteralPath {img} -Destination {base}; \
             $d = Mount-VHD -Path {base} -Passthru | Get-Disk; \
             try {{ $win = Get-Partition -DiskNumber $d.Number | Where-Object {{ $_.DriveLetter -and (Test-Path ($_.DriveLetter + ':\\Windows\\System32')) }} | Select-Object -First 1; \
               if (-not $win) {{ throw 'no Windows volume in the image' }}; $w = $win.DriveLetter + ':\\'; \
               {inject} }} finally {{ Dismount-VHD -Path {base} }}; ",
            img = q(&s.image_path),
            base = q(&base),
            inject = inject_payload(&host.payload_dir),
        ),
    };
    Ok(format!("{head}{body}Set-ItemProperty -LiteralPath {base} -Name IsReadOnly -Value $true", base = q(&base)))
}

/// Delete the base disk and the seats' disks (their VMs are removed too: they point at
/// the disks). Every listed seat must be off (the service checks).
pub fn script_reset_base(storage: &str, seat_ids: &[String]) -> String {
    let mut s = String::from("$ErrorActionPreference='Stop'; ");
    for id in seat_ids.iter().filter(|i| seat_id_ok(i)) {
        s.push_str(&format!(
            "if (Get-VM -Name {vm} -ErrorAction SilentlyContinue) {{ Stop-VM -Name {vm} -TurnOff -Force -ErrorAction SilentlyContinue; Remove-VM -Name {vm} -Force }}; \
             Remove-Item -LiteralPath {dir} -Recurse -Force -ErrorAction SilentlyContinue; ",
            vm = q(&vm_name(id)),
            dir = q(&seat_dir(storage, id)),
        ));
    }
    s.push_str(&format!(
        "if (Test-Path -LiteralPath {b}) {{ Set-ItemProperty -LiteralPath {b} -Name IsReadOnly -Value $false; Remove-Item -LiteralPath {b} -Force }}",
        b = q(&base_disk(storage))
    ));
    s
}

/// Copy the host's display driver packages into the guest disk's `HostDriverStore` (GPU-P
/// guests load the host's user-mode driver from there). Expects the disk mounted with its
/// Windows volume at `$w`. HYPOTHESIS.
fn copy_gpu_drivers() -> &'static str {
    "$infs = @(Get-CimInstance Win32_PnPSignedDriver | Where-Object { $_.DeviceClass -eq 'DISPLAY' -and $_.InfName } | ForEach-Object { $_.InfName } | Select-Object -Unique); \
     $repo = Join-Path $w 'Windows\\System32\\HostDriverStore\\FileRepository'; New-Item -ItemType Directory -Force -Path $repo | Out-Null; \
     foreach ($inf in $infs) { $drv = Get-WindowsDriver -Online -Driver $inf -ErrorAction SilentlyContinue | Select-Object -First 1; \
       if ($drv) { $src = Split-Path -Parent $drv.OriginalFileName; Copy-Item -LiteralPath $src -Destination $repo -Recurse -Force } }"
}

fn create_disk(spec: &VmSpec) -> String {
    let disk = seat_disk(&spec.storage, &spec.seat_id);
    let mut s = format!(
        "New-Item -ItemType Directory -Force -Path {dir} | Out-Null; \
         if (-not (Test-Path -LiteralPath {disk})) {{ New-VHD -Path {disk} -ParentPath {base} -Differencing | Out-Null; ",
        dir = q(&seat_dir(&spec.storage, &spec.seat_id)),
        disk = q(&disk),
        base = q(&base_disk(&spec.storage)),
    );
    if spec.gpu.is_on() {
        s.push_str(&format!(
            "$d = Mount-VHD -Path {disk} -Passthru | Get-Disk; \
             try {{ $win = Get-Partition -DiskNumber $d.Number | Where-Object {{ $_.DriveLetter -and (Test-Path ($_.DriveLetter + ':\\Windows\\System32')) }} | Select-Object -First 1; \
               if ($win) {{ $w = $win.DriveLetter + ':\\'; {copy} }} }} finally {{ Dismount-VHD -Path {disk} }}; ",
            disk = q(&disk),
            copy = copy_gpu_drivers(),
        ));
    }
    s.push_str("}; ");
    s
}

/// Create the seat's VM when missing and (re)apply its settings, forwards, firewall rule
/// and shares. Reads the share account's password from stdin (first line).
pub fn script_ensure(spec: &VmSpec) -> Result<String, String> {
    if !seat_id_ok(&spec.seat_id) {
        return Err("invalid seat id".into());
    }
    let vm = q(&vm_name(&spec.seat_id));
    let disk = q(&seat_disk(&spec.storage, &spec.seat_id));
    let mut s = String::from("$ErrorActionPreference='Stop'; $pw = [Console]::In.ReadLine(); ");
    s.push_str(&format!(
        "if (-not (Test-Path -LiteralPath {base})) {{ throw 'base disk missing' }}; ",
        base = q(&base_disk(&spec.storage))
    ));
    s.push_str(&create_disk(spec));
    s.push_str(&format!(
        "if (-not (Get-VM -Name {vm} -ErrorAction SilentlyContinue)) {{ \
           New-VM -Name {vm} -Generation 2 -MemoryStartupBytes {mem}MB -VHDPath {disk} -SwitchName {sw} | Out-Null; \
           Set-VMFirmware -VMName {vm} -EnableSecureBoot On -SecureBootTemplate MicrosoftWindows; \
           Set-VMKeyProtector -VMName {vm} -NewLocalKeyProtector; Enable-VMTPM -VMName {vm}; \
           Enable-VMIntegrationService -VMName {vm} -Name 'Guest Service Interface' }}; \
         $v = Get-VM -Name {vm}; \
         if ($v.State -eq 'Off') {{ \
           Set-VM -Name {vm} -StaticMemory -MemoryStartupBytes {mem}MB -CheckpointType Disabled -AutomaticStartAction Nothing -AutomaticStopAction ShutDown; \
           Set-VMProcessor -VMName {vm} -Count {cpus}; \
           Get-VMGpuPartitionAdapter -VMName {vm} -ErrorAction SilentlyContinue | Remove-VMGpuPartitionAdapter; ",
        vm = vm,
        mem = spec.memory_mb,
        disk = disk,
        sw = q(VM_SWITCH),
        cpus = spec.processors,
    ));
    match &spec.gpu {
        GpuChoice::Off => {}
        GpuChoice::Auto => s.push_str(
            "if (Get-VMHostPartitionableGpu -ErrorAction SilentlyContinue) { ",
        ),
        GpuChoice::Adapter(_) => s.push_str("if ($true) { "),
    }
    if spec.gpu.is_on() {
        let add = match &spec.gpu {
            GpuChoice::Adapter(a) => format!("Add-VMGpuPartitionAdapter -VMName {vm} -InstancePath {}", q(a)),
            _ => format!("Add-VMGpuPartitionAdapter -VMName {vm}"),
        };
        s.push_str(&format!(
            "{add}; Set-VM -Name {vm} -GuestControlledCacheTypes $true -LowMemoryMappedIoSpace 1GB -HighMemoryMappedIoSpace 32GB }}; "
        ));
    }
    s.push_str("}; ");
    // Port forwards: drop this seat's old mappings, add the current ones.
    s.push_str(&format!(
        "Get-NetNatStaticMapping -NatName {nat} -ErrorAction SilentlyContinue | Where-Object {{ $_.InternalIPAddress -eq {ip} }} | Remove-NetNatStaticMapping -Confirm:$false; ",
        nat = q(VM_NAT),
        ip = q(&spec.guest_address),
    ));
    for f in &spec.forwards {
        s.push_str(&format!(
            "Add-NetNatStaticMapping -NatName {nat} -Protocol {p} -ExternalIPAddress '0.0.0.0' -ExternalPort {h} -InternalIPAddress {ip} -InternalPort {g} | Out-Null; ",
            nat = q(VM_NAT),
            p = f.protocol.as_str(),
            h = f.host_port,
            ip = q(&spec.guest_address),
            g = f.guest_port,
        ));
    }
    // Firewall: one rule per protocol for the forwarded host ports.
    let rule = q(&firewall_rule(&spec.seat_id));
    s.push_str(&format!("Get-NetFirewallRule -DisplayName {rule} -ErrorAction SilentlyContinue | Remove-NetFirewallRule; "));
    for proto in [seat_core::remote::vm::PortProtocol::Tcp, seat_core::remote::vm::PortProtocol::Udp] {
        let ports: Vec<String> = spec.forwards.iter().filter(|f| f.protocol == proto).map(|f| f.host_port.to_string()).collect();
        if !ports.is_empty() {
            s.push_str(&format!(
                "New-NetFirewallRule -DisplayName {rule} -Group {grp} -Direction Inbound -Action Allow -Protocol {p} -LocalPort {list} -Profile Private,Domain | Out-Null; ",
                grp = q(FIREWALL_GROUP),
                p = proto.as_str(),
                list = ports.join(","),
            ));
        }
    }
    // This seat's own share account (its password rotates with every start of this seat
    // only) and its shares. The shared account of older builds loses its grants.
    let acct = q(&share_account(&spec.seat_id));
    s.push_str(&format!(
        "$sec = ConvertTo-SecureString -String $pw -AsPlainText -Force; \
         if (Get-LocalUser -Name {acct} -ErrorAction SilentlyContinue) {{ Set-LocalUser -Name {acct} -Password $sec }} \
         else {{ New-LocalUser -Name {acct} -Password $sec -PasswordNeverExpires -UserMayNotChangePassword -Description 'Pulsar Seat VM shared folders (one seat)' | Out-Null }}; \
         Get-SmbShare -Name {pat} -ErrorAction SilentlyContinue | Remove-SmbShare -Force; ",
        pat = q(&share_name(&spec.seat_id, 0).replace("-0$", "-*")),
    ));
    let legacy = q(LEGACY_SHARE_ACCOUNT);
    for sh in &spec.shares {
        let access = if sh.read_only { "-ReadAccess" } else { "-ChangeAccess" };
        let ntfs = if sh.read_only { "(OI)(CI)RX" } else { "(OI)(CI)M" };
        s.push_str(&format!(
            "New-SmbShare -Name {n} -Path {p} {access} ($env:COMPUTERNAME + '\\' + {acct}) | Out-Null; \
             if (Get-LocalUser -Name {legacy} -ErrorAction SilentlyContinue) {{ & icacls.exe {p} /remove {legacy} | Out-Null }}; \
             & icacls.exe {p} /grant ({acct} + ':{ntfs}') | Out-Null; ",
            n = q(&sh.name),
            p = q(&sh.host_path),
        ));
    }
    Ok(s)
}

pub fn script_start(seat_id: &str) -> String {
    format!("$ErrorActionPreference='Stop'; Start-VM -Name {}", q(&vm_name(seat_id)))
}

pub fn script_stop(seat_id: &str, mode: VmStopMode) -> String {
    let vm = q(&vm_name(seat_id));
    match mode {
        VmStopMode::ShutDown => format!("$ErrorActionPreference='Stop'; Stop-VM -Name {vm} -Force"),
        VmStopMode::Save => format!("$ErrorActionPreference='Stop'; Save-VM -Name {vm}"),
        VmStopMode::TurnOff => format!("$ErrorActionPreference='Stop'; Stop-VM -Name {vm} -TurnOff -Force"),
    }
}

/// Recreate the seat's differencing disk (the VM keeps its settings).
pub fn script_reset_sandbox(spec: &VmSpec) -> Result<String, String> {
    if !seat_id_ok(&spec.seat_id) {
        return Err("invalid seat id".into());
    }
    Ok(format!(
        "$ErrorActionPreference='Stop'; \
         $v = Get-VM -Name {vm} -ErrorAction SilentlyContinue; if ($v -and $v.State -ne 'Off') {{ throw 'VM is not off' }}; \
         Remove-Item -LiteralPath {disk} -Force -ErrorAction SilentlyContinue; {create}",
        vm = q(&vm_name(&spec.seat_id)),
        disk = q(&seat_disk(&spec.storage, &spec.seat_id)),
        create = create_disk(spec),
    ))
}

/// Remove everything of one seat.
pub fn script_remove(spec: &VmSpec) -> Result<String, String> {
    if !seat_id_ok(&spec.seat_id) {
        return Err("invalid seat id".into());
    }
    let mut s = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         if (Get-VM -Name {vm}) {{ Stop-VM -Name {vm} -TurnOff -Force; Remove-VM -Name {vm} -Force }}; \
         Remove-Item -LiteralPath {dir} -Recurse -Force; \
         Get-NetFirewallRule -DisplayName {rule} | Remove-NetFirewallRule; ",
        vm = q(&vm_name(&spec.seat_id)),
        dir = q(&seat_dir(&spec.storage, &spec.seat_id)),
        rule = q(&firewall_rule(&spec.seat_id)),
    );
    if !spec.guest_address.is_empty() {
        s.push_str(&format!(
            "Get-NetNatStaticMapping -NatName {nat} | Where-Object {{ $_.InternalIPAddress -eq {ip} }} | Remove-NetNatStaticMapping -Confirm:$false; ",
            nat = q(VM_NAT),
            ip = q(&spec.guest_address),
        ));
    }
    let acct = q(&share_account(&spec.seat_id));
    for sh in &spec.shares {
        s.push_str(&format!(
            "Remove-SmbShare -Name {n} -Force; & icacls.exe {p} /remove {acct} | Out-Null; ",
            n = q(&sh.name),
            p = q(&sh.host_path),
        ));
    }
    s.push_str(&format!("Remove-LocalUser -Name {acct}; "));
    Ok(s)
}

/// Restart the host's display adapters (opt-in `restart_gpus_on_shutdown`).
pub fn script_restart_gpus() -> String {
    "$ErrorActionPreference='SilentlyContinue'; \
     Get-PnpDevice -Class Display -Status OK | Where-Object { $_.InstanceId -notlike 'ROOT\\*' -and $_.InstanceId -notlike 'SWD\\*' } \
     | ForEach-Object { Disable-PnpDevice -InstanceId $_.InstanceId -Confirm:$false; Enable-PnpDevice -InstanceId $_.InstanceId -Confirm:$false }"
        .to_string()
}

/// Uninstall: every Pulsar VM and its disks, the base disk, forwards, NAT, switch, firewall
/// group, shares, the share account and the guest link registration.
pub fn script_uninstall(storage: &str, shares: &[ShareSpec]) -> String {
    let reg = format!(
        r"HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\{}",
        seat_core::remote::vm::GUEST_LINK_SERVICE_ID
    );
    let mut s = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Get-VM | Where-Object {{ $_.Name -like {prefix} }} | ForEach-Object {{ Stop-VM -VM $_ -TurnOff -Force; Remove-VM -VM $_ -Force }}; \
         Remove-Item -LiteralPath {seats} -Recurse -Force; \
         if (Test-Path -LiteralPath {base}) {{ Set-ItemProperty -LiteralPath {base} -Name IsReadOnly -Value $false; Remove-Item -LiteralPath {base} -Force }}; \
         Get-SmbShare | Where-Object {{ $_.Name -like 'PulsarVm*$' }} | Remove-SmbShare -Force; ",
        prefix = q(&format!("{VM_PREFIX}*")),
        seats = q(&format!(r"{}\seats", storage.trim_end_matches('\\'))),
        base = q(&base_disk(storage)),
    );
    for sh in shares {
        let mut accts = vec![LEGACY_SHARE_ACCOUNT.to_string()];
        accts.extend(share_account_of(&sh.name));
        for a in accts {
            s.push_str(&format!("& icacls.exe {p} /remove {acct} | Out-Null; ", p = q(&sh.host_path), acct = q(&a)));
        }
    }
    s.push_str(&format!(
        "Get-NetFirewallRule -Group {grp} | Remove-NetFirewallRule; \
         Get-NetNat -Name {nat} | Remove-NetNat -Confirm:$false; \
         Remove-VMSwitch -Name {sw} -Force; \
         Remove-LocalUser -Name {legacy}; \
         Get-LocalUser | Where-Object {{ $_.Name -match {pat} -and $_.Description -like 'Pulsar Seat VM shared folders*' }} | Remove-LocalUser; \
         Remove-Item -Path {reg} -Recurse -Force",
        grp = q(FIREWALL_GROUP),
        nat = q(VM_NAT),
        sw = q(VM_SWITCH),
        legacy = q(LEGACY_SHARE_ACCOUNT),
        pat = q(&format!("^{SHARE_ACCOUNT_PREFIX}[0-9a-f]{{8}}$")),
        reg = q(&reg),
    ));
    s
}

/// Script of one op (the share password, when any, goes on stdin; see [`VmOp::Ensure`]).
pub fn script_for(op: &VmOp, arch: &str) -> Result<String, String> {
    Ok(match op {
        VmOp::Status { storage } => script_status(storage),
        VmOp::PrepareHost { host } => script_prepare_host(&host.nat_prefix)?,
        VmOp::BuildBase { host } => script_build_base(host, arch)?,
        VmOp::ResetBase { storage, seat_ids } => script_reset_base(storage, seat_ids),
        VmOp::Ensure { spec } => script_ensure(spec)?,
        VmOp::Start { seat_id } if seat_id_ok(seat_id) => script_start(seat_id),
        VmOp::Stop { seat_id, mode } if seat_id_ok(seat_id) => script_stop(seat_id, *mode),
        VmOp::Start { .. } | VmOp::Stop { .. } => return Err("invalid seat id".into()),
        VmOp::ResetSandbox { spec } => script_reset_sandbox(spec)?,
        VmOp::Remove { spec } => script_remove(spec)?,
        VmOp::RestartGpus => script_restart_gpus(),
    })
}

/// What Pulsar created (uninstall removes it; the scripts also find objects by name).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmJournal {
    /// Storage folder used (base disk, seat disks).
    pub storage: String,
    pub host_prepared: bool,
    pub base_built: bool,
    /// Seats whose VM was created.
    pub seats: Vec<String>,
    /// Shared host folders (their NTFS grant is removed at uninstall).
    pub shares: Vec<ShareSpec>,
}

impl VmJournal {
    pub fn add_seat(&mut self, id: &str) {
        if !self.seats.iter().any(|s| s == id) {
            self.seats.push(id.to_string());
            self.seats.sort();
        }
    }
    pub fn remove_seat(&mut self, id: &str) {
        self.seats.retain(|s| s != id);
    }
    pub fn add_shares(&mut self, shares: &[ShareSpec]) {
        for s in shares {
            if !self.shares.iter().any(|o| o.name == s.name && o.host_path == s.host_path) {
                self.shares.push(s.clone());
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        !self.host_prepared && !self.base_built && self.seats.is_empty() && self.shares.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::remote::vm::PortProtocol;

    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    fn spec() -> VmSpec {
        VmSpec {
            seat_id: ID.into(),
            storage: r"D:\Pulsar VM\".into(),
            memory_mb: 8192,
            processors: 4,
            gpu: GpuChoice::Auto,
            forwards: seat_core::remote::vm::stream_forwards(48100),
            guest_address: "172.31.240.12".into(),
            shares: vec![ShareSpec { name: share_name(ID, 0), host_path: r"E:\Games".into(), read_only: true }],
            stop: VmStopMode::ShutDown,
        }
    }

    #[test]
    fn names_and_paths() {
        assert_eq!(vm_name(ID), format!("PulsarSeat-{ID}"));
        assert_eq!(seat_id_of(&vm_name(ID)), Some(ID));
        assert_eq!(seat_id_of("Other"), None);
        assert_eq!(base_disk(r"D:\Pulsar VM\"), r"D:\Pulsar VM\zygote.vhdx");
        assert_eq!(seat_disk(r"D:\v", ID), format!(r"D:\v\seats\{ID}\sandbox.vhdx"));
        assert_eq!(share_name(ID, 2), "PulsarVm0f8fad5b-2$");
        assert_eq!(share_account(ID), "PulsarVm0f8fad5b");
        assert!(share_account(ID).len() <= 20, "local account name limit");
        assert_eq!(share_account_of("PulsarVm0f8fad5b-2$").as_deref(), Some("PulsarVm0f8fad5b"));
        assert_eq!(share_account_of("Other-1$"), None);
        assert_eq!(share_account_of("PulsarVmzz-1$"), None);
        assert!(seat_id_ok(ID));
        assert!(!seat_id_ok("x'; Remove-Item"));
        assert_eq!(GpuChoice::from_seat(GpuPartitionMode::Adapter, None), GpuChoice::Off);
    }

    #[test]
    fn ensure_script_has_every_part_and_no_password() {
        let s = script_ensure(&spec()).unwrap();
        assert!(s.starts_with("$ErrorActionPreference='Stop'; $pw = [Console]::In.ReadLine();"));
        assert!(s.contains(&format!("New-VHD -Path 'D:\\Pulsar VM\\seats\\{ID}\\sandbox.vhdx' -ParentPath 'D:\\Pulsar VM\\zygote.vhdx' -Differencing")));
        assert!(s.contains("New-VM -Name 'PulsarSeat-0f8fad5b-d9cb-469f-a165-70867728950e' -Generation 2 -MemoryStartupBytes 8192MB"));
        assert!(s.contains("Enable-VMTPM"));
        assert!(s.contains("Set-VMProcessor -VMName 'PulsarSeat-0f8fad5b-d9cb-469f-a165-70867728950e' -Count 4"));
        assert!(s.contains("Add-VMGpuPartitionAdapter -VMName"));
        assert!(s.contains("HostDriverStore"));
        assert!(s.contains("-ExternalPort 48095 -InternalIPAddress '172.31.240.12' -InternalPort 48095"));
        assert!(!s.contains("-ExternalPort 48101"), "web UI never forwarded");
        assert!(s.contains("-Protocol UDP -LocalPort 48109,48110,48111"));
        assert!(s.contains("New-SmbShare -Name 'PulsarVm0f8fad5b-0$' -Path 'E:\\Games' -ReadAccess"));
        assert!(s.contains("(OI)(CI)RX"));
        // One share account per seat (review 2026-11-17): granted only this seat's shares.
        assert!(s.contains("Set-LocalUser -Name 'PulsarVm0f8fad5b' -Password $sec"));
        assert!(s.contains("-ReadAccess ($env:COMPUTERNAME + '\\' + 'PulsarVm0f8fad5b')"));
        assert!(s.contains("/grant ('PulsarVm0f8fad5b' + ':(OI)(CI)RX')"));
        assert!(!s.contains("/grant ('PulsarSeatVmShare'"), "the old shared account gets no grant");
        assert!(s.contains("/remove 'PulsarSeatVmShare'"), "and loses its old one");
        let other = script_ensure(&VmSpec { seat_id: "1b4e28ba-2fa1-11d2-883f-0016d3cca427".into(), ..spec() }).unwrap();
        assert!(other.contains("'PulsarVm1b4e28ba'") && !other.contains("'PulsarVm0f8fad5b'"));
        let mut sp = spec();
        sp.gpu = GpuChoice::Off;
        let s = script_ensure(&sp).unwrap();
        assert!(!s.contains("Add-VMGpuPartitionAdapter"));
        assert!(!s.contains("HostDriverStore"));
        sp.gpu = GpuChoice::Adapter(r"\\?\PCI#X".into());
        assert!(script_ensure(&sp).unwrap().contains(r"-InstancePath '\\?\PCI#X'"));
        sp.seat_id = "bad id".into();
        assert!(script_ensure(&sp).is_err());
    }

    #[test]
    fn host_and_base_scripts() {
        let p = script_prepare_host("172.31.240.0/24").unwrap();
        assert!(p.contains("New-VMSwitch -Name 'Pulsar Seat VM' -SwitchType Internal"));
        assert!(p.contains("-IPAddress '172.31.240.1' -PrefixLength 24"));
        assert!(p.contains("New-NetNat -Name 'Pulsar Seat VM' -InternalIPInterfaceAddressPrefix '172.31.240.0/24'"));
        assert!(p.contains(seat_core::remote::vm::GUEST_LINK_SERVICE_ID));
        assert!(script_prepare_host("nope").is_err());
        let mut host = HostSpec {
            storage: r"D:\vm".into(),
            nat_prefix: "172.31.240.0/24".into(),
            payload_dir: r"C:\Program Files\Pulsar Seat".into(),
            settings: VmSettings::default(),
        };
        assert!(script_build_base(&host, "arm64").is_err(), "no image");
        host.settings.image_kind = ImageKind::Wim;
        host.settings.image_path = r"D:\iso\install.wim".into();
        host.settings.image_index = 6;
        host.settings.language_cab = r"D:\iso\tr-tr.cab".into();
        let b = script_build_base(&host, "arm64").unwrap();
        assert!(b.contains("New-VHD -Path 'D:\\vm\\zygote.vhdx' -SizeBytes 128GB -Dynamic"));
        assert!(b.contains("Expand-WindowsImage -ImagePath 'D:\\iso\\install.wim' -Index 6"));
        assert!(b.contains("Add-WindowsPackage -Path $w -PackagePath 'D:\\iso\\tr-tr.cab'"));
        assert!(b.contains("bcdboot.exe"));
        assert!(b.contains("processorArchitecture=\"arm64\""));
        assert!(b.contains("vm-guest"));
        assert!(b.contains("IsReadOnly -Value $true"));
        assert!(!b.to_ascii_lowercase().contains("<password>"), "no password in the answer file");
        host.settings.image_kind = ImageKind::Vhdx;
        host.settings.image_path = r"D:\base.vhdx".into();
        let b = script_build_base(&host, "amd64").unwrap();
        assert!(b.contains("Copy-Item -LiteralPath 'D:\\base.vhdx' -Destination 'D:\\vm\\zygote.vhdx'"));
        assert!(b.contains("processorArchitecture=\"amd64\""));
        let r = script_reset_base(r"D:\vm", &[ID.into(), "bad'id".into()]);
        assert!(r.contains("Remove-VM -Name 'PulsarSeat-0f8fad5b"));
        assert!(!r.contains("bad'id"));
    }

    #[test]
    fn stop_reset_remove_uninstall() {
        assert!(script_stop(ID, VmStopMode::Save).contains("Save-VM"));
        assert!(script_stop(ID, VmStopMode::TurnOff).contains("-TurnOff"));
        assert!(script_stop(ID, VmStopMode::ShutDown).ends_with("-Force"));
        assert!(script_reset_sandbox(&spec()).unwrap().contains("throw 'VM is not off'"));
        let r = script_remove(&spec()).unwrap();
        assert!(r.contains("Remove-VM"));
        assert!(r.contains("Remove-SmbShare -Name 'PulsarVm0f8fad5b-0$'"));
        assert!(r.contains("Remove-NetNatStaticMapping"));
        assert!(r.contains("/remove 'PulsarVm0f8fad5b'"));
        assert!(r.contains("Remove-LocalUser -Name 'PulsarVm0f8fad5b'"), "the seat's account goes with the seat");
        let u = script_uninstall(r"D:\vm", &spec().shares);
        for part in [
            "Remove-VMSwitch -Name 'Pulsar Seat VM'",
            "Remove-NetNat",
            "Remove-LocalUser -Name 'PulsarSeatVmShare'",
            "GuestCommunicationServices",
            "/remove 'PulsarSeatVmShare'",
            "/remove 'PulsarVm0f8fad5b'",
            "'^PulsarVm[0-9a-f]{8}$'",
        ] {
            assert!(u.contains(part), "{part}");
        }
        assert!(script_for(&VmOp::Start { seat_id: "x y".into() }, "amd64").is_err());
        assert!(script_for(&VmOp::RestartGpus, "amd64").unwrap().contains("Disable-PnpDevice"));
        let _ = PortProtocol::Tcp;
    }

    #[test]
    fn status_parsing() {
        let f = parse_status(&format!(
            r#"{{"feature":"Enabled","vmms":true,"switch":true,"nat":false,"base":true,"gpus":"\\\\?\\PCI#A","memory_mb":32768,"cpus":16,"dns":"192.168.1.1",
               "vms":{{"name":"PulsarSeat-{ID}","id":"AB-CD","state":"Running","uptime":42}}}}"#
        ))
        .unwrap();
        assert_eq!(f.feature, "enabled");
        assert!(f.vmms && f.switch_present && !f.nat_present && f.base_present);
        assert_eq!(f.gpus, vec![r"\\?\PCI#A".to_string()]);
        assert_eq!(f.dns, vec!["192.168.1.1".to_string()]);
        assert_eq!(f.vms, vec![VmState { seat_id: ID.into(), vm_id: "ab-cd".into(), state: "running".into(), uptime_s: 42 }]);
        let f = parse_status(r#"{"feature":"unavailable","vms":[{"name":"Other"}],"gpus":null}"#).unwrap();
        assert_eq!(f.feature, "unavailable");
        assert!(f.vms.is_empty() && f.gpus.is_empty());
        assert!(parse_status("x").is_err());
    }

    #[test]
    fn journal() {
        let mut j = VmJournal::default();
        assert!(j.is_empty());
        j.add_seat("b");
        j.add_seat("a");
        j.add_seat("b");
        assert_eq!(j.seats, vec!["a", "b"]);
        j.remove_seat("a");
        j.add_shares(&spec().shares);
        j.add_shares(&spec().shares);
        assert_eq!(j.shares.len(), 1);
        assert!(!j.is_empty());
        let back: VmJournal = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
        assert_eq!(back, j);
    }
}

#[cfg(test)]
mod guest_tests {
    use super::*;
    use seat_core::remote::vm::{GuestConfig, GuestShare};

    #[test]
    fn guest_apply_script_reads_passwords_from_stdin() {
        let cfg = GuestConfig {
            seat_id: "id".into(),
            seat_name: "Salon".into(),
            advertised_name: "Salon (VM)".into(),
            port: 48100,
            stream_backend: Default::default(),
            stream: Default::default(),
            audio: Default::default(),
            verbosity: Default::default(),
            user_name: "oyuncu".into(),
            password: seat_core::Secret::new("SEATPW"),
            address: "172.31.240.12".into(),
            prefix_len: 24,
            gateway: "172.31.240.1".into(),
            dns: vec!["192.168.1.1".into()],
            shares: vec![
                GuestShare { unc: r"\\172.31.240.1\PulsarVm0f8fad5b-0$".into(), guest_path: r"C:\HostDrive".into(), read_only: true },
                GuestShare { unc: r"\\172.31.240.1\PulsarVm0f8fad5b-1$".into(), guest_path: "E:".into(), read_only: false },
            ],
            share_user: "PulsarSeatVmShare".into(),
            share_password: seat_core::Secret::new("SHAREPW"),
            apps: vec![],
            stream_apps: None,
            connect_password: None,
            regenerate_identity: false,
            start_nonce: String::new(),
        };
        let s = script_guest_apply(&cfg);
        assert!(!s.contains("SEATPW") && !s.contains("SHAREPW"), "passwords only on stdin");
        assert!(s.contains("-IPAddress '172.31.240.12' -PrefixLength 24 -DefaultGateway '172.31.240.1'"));
        assert!(s.contains("-ServerAddresses @('192.168.1.1')"));
        assert!(s.contains("New-LocalUser -Name 'oyuncu'"));
        assert!(s.contains("-LocalPath 'E:'"));
        assert!(s.contains("New-Item -ItemType SymbolicLink -Path 'C:\\HostDrive'"));
        assert!(setup_complete_cmd().contains("/SC ONSTART /RU SYSTEM"));
    }
}
