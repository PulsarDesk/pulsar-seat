// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V VM seats (spec/remote 10 §8, 20 "sandbox", 90 §2.4; hyperv-seats 2026-11-17).
//!
//! A VM seat is a remote seat (`kind = remote`) whose `remote.vm` is set: its Windows
//! session runs in a Hyper-V virtual machine of its own instead of an RDP loopback session
//! on the host. The VM boots from a per-seat differencing disk (`sandbox.vhdx`) over one
//! shared, read-only base disk (the "zygote", `zygote.vhdx`) that the service builds from an
//! operator-chosen Windows image (`install.wim` / `.esd`, or a prepared `.vhdx`). The guest
//! runs Pulsar Seat in guest mode (`pulsar-seat-service vm-guest`): it gets its seat
//! configuration from the host over a Hyper-V socket ([`GuestMsg`]), signs the seat user in
//! at the VM's console and runs the seat's stream server there. The host forwards the
//! seat's stream ports (and the seat's extra [`ForwardedPort`]s) to the guest through a NAT
//! on an internal switch, and offers the seat's [`SharedFolder`]s as SMB shares.
//!
//! This module is the pure part: the config types, their validation, the guest addresses,
//! the forwarded port list and the guest link messages. The Hyper-V side (PowerShell
//! scripts, journal) is `seat_compat::hyperv_vm`; the service side is
//! `seat-service/src/vm_seats.rs`.
//!
//! HYPOTHESIS: nothing of this ran on a Hyper-V host yet.

use crate::error::{ErrorCode, ProtoError};
use crate::remote::ports;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Default NAT prefix of the VM network (`settings.remote.vm.nat_prefix`). Private range,
/// chosen to be rare on home networks.
pub const DEFAULT_NAT_PREFIX: &str = "172.31.240.0/24";
/// Guest address of seat `n` = network + `GUEST_HOST_OFFSET + n`.
pub const GUEST_HOST_OFFSET: u32 = 10;
/// Smallest VM memory (MB): Windows 11 needs 4 GB, Windows 10 2 GB.
pub const MIN_MEMORY_MB: u32 = 2048;
/// Largest VM memory we accept (MB).
pub const MAX_MEMORY_MB: u32 = 1 << 20;
/// `memory_percent` range (share of the host's RAM for all VM seats together).
pub const MEMORY_PERCENT_RANGE: (u8, u8) = (10, 90);
/// `storage_gb` range (maximum size of the base disk; children grow up to it).
pub const STORAGE_GB_RANGE: (u32, u32) = (32, 4096);
/// At most this many shared folders / forwarded ports per seat.
pub const MAX_SHARED_FOLDERS: usize = 16;
pub const MAX_FORWARDED_PORTS: usize = 64;
/// Default mount point when the host folder is a drive root (spec 10 §8).
pub const HOST_DRIVE_MOUNT: &str = r"C:\HostDrive";
/// Suffix of a VM seat's advertised name unless `hide_vm_suffix` (spec 80 `sunshine_name`).
pub const VM_SUFFIX: &str = " (VM)";
/// Hyper-V socket service id of the guest link (registered under
/// `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices`
/// on the host). Our own random GUID.
pub const GUEST_LINK_SERVICE_ID: &str = "6c3c2b9e-5f1a-4f7e-9d2b-8a41c0e7b513";
/// Guest link protocol version ([`GuestMsg::Hello`]). 2 (parity-2 review, 2026-11-17):
/// `request` / `reply` / `notice` for the seat page's requests on VM seats.
pub const GUEST_LINK_PROTOCOL: u32 = 2;
/// Longest guest link line (bytes, JSON + `\n`).
pub const GUEST_LINK_MAX_LINE: usize = 64 * 1024;

/// What happens to the VM when the seat stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmStopMode {
    /// Guest shutdown through the integration service (`Stop-VM`), the default.
    #[default]
    ShutDown,
    /// Save the VM state (`Save-VM`); the next start resumes. Not with a GPU partition
    /// (Hyper-V cannot save a VM with an assigned GPU partition).
    Save,
    /// Power off (`Stop-VM -TurnOff`).
    TurnOff,
}

/// GPU partitioning (GPU-P) of the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuPartitionMode {
    /// No GPU: the guest renders with WARP (software).
    Off,
    /// A partition of the first partitionable GPU when the host has one, else off.
    #[default]
    Auto,
    /// A partition of the GPU named by `gpu_adapter` (instance path of
    /// `Get-VMHostPartitionableGpu`); the seat does not start when it is missing.
    Adapter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl PortProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

/// A host folder offered to the guest (spec 10 §8 `SharedDirectories`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedFolder {
    /// GUID string, stable key of the entry.
    pub id: String,
    /// Absolute host path (`D:\Games`, `E:\`).
    pub host_path: String,
    /// Mount point in the guest: a drive letter other than `C:` (`D:`), or a `C:\` path with
    /// at least one folder and no trailing backslash.
    pub guest_path: String,
    pub read_only: bool,
}

/// A host port forwarded to the guest (spec 10 §8 `ForwardedPorts`), besides the seat's
/// stream ports, which are always forwarded.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ForwardedPort {
    pub id: String,
    pub description: String,
    pub protocol: PortProtocol,
    pub host_port: u16,
    pub guest_port: u16,
}

/// The VM part of a remote seat (`RemoteSeat::vm`). `Some` = VM seat.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VmSeat {
    /// Startup memory in MB; 0 = the seat's share of `settings.remote.vm.memory_percent`.
    pub memory_mb: u32,
    /// Virtual processors; 0 = half of the host's logical processors (at least 2).
    pub processors: u8,
    pub gpu: GpuPartitionMode,
    /// Instance path of the partitionable GPU for `gpu = adapter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_adapter: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_folders: Vec<SharedFolder>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forwarded_ports: Vec<ForwardedPort>,
    /// Advertise the display name without the ` (VM)` suffix (spec 20 `HideVMSuffix`).
    pub hide_vm_suffix: bool,
    pub stop: VmStopMode,
    /// Guest IPv4 address on the VM network; empty = assigned by the service
    /// ([`guest_address`]). Service-owned once assigned.
    pub guest_address: String,
}

/// Where the base disk comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    /// No image chosen yet: VM seats cannot start.
    #[default]
    None,
    /// A Windows setup image (`install.wim` / `install.esd`); `image_index` picks the
    /// edition. The service applies it to a new base disk and adds an unattend file.
    Wim,
    /// A prepared, generalized (sysprep) `.vhdx` with Windows; copied as the base disk.
    Vhdx,
}

/// Machine-wide VM seat settings (`settings.remote.vm`, spec 10 §3 hidden sandbox values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmSettings {
    /// Folder of the base disk and the seats' disks; empty = `<data dir>\vm`
    /// (spec 10 §8 `SandboxStorageLocation`).
    pub storage_location: String,
    /// Share of the host's RAM for all VM seats together (`SandboxMemory`, default 50).
    pub memory_percent: u8,
    /// Maximum size of the base disk in GB (`SandboxStorage`).
    pub storage_gb: u32,
    pub image_kind: ImageKind,
    /// Path of the `.wim` / `.esd` / `.vhdx`.
    pub image_path: String,
    /// Edition index inside a `.wim` / `.esd` (1-based).
    pub image_index: u32,
    /// Optional language pack (`.cab`) added to the base disk (spec 10 §8 `language.cab`).
    pub language_cab: String,
    /// Restart the host's display adapters after a VM with a GPU partition stopped
    /// (`RestartGPUsOnSandboxShutdown`; works around drivers that keep the partition busy).
    pub restart_gpus_on_shutdown: bool,
    /// IPv4 prefix of the NAT network (`a.b.c.0/len`, len 16..=28).
    pub nat_prefix: String,
}

impl Default for VmSettings {
    fn default() -> Self {
        Self {
            storage_location: String::new(),
            memory_percent: 50,
            storage_gb: 128,
            image_kind: ImageKind::None,
            image_path: String::new(),
            image_index: 1,
            language_cab: String::new(),
            restart_gpus_on_shutdown: false,
            nat_prefix: DEFAULT_NAT_PREFIX.into(),
        }
    }
}

fn err(field: &str, reason: &str, msg: impl Into<String>) -> ProtoError {
    ProtoError::new(ErrorCode::InvalidValue, msg).with_field(field).with_reason(reason)
}

/// `a.b.c.d/len` as (network, len); the host part must be zero.
pub fn parse_prefix(s: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, len) = s.trim().split_once('/')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    let len: u8 = len.parse().ok()?;
    if !(16..=28).contains(&len) {
        return None;
    }
    let mask = u32::MAX << (32 - u32::from(len));
    (u32::from(ip) & !mask == 0).then_some((ip, len))
}

/// Host side address of the NAT network (network + 1, the gateway of every guest).
pub fn gateway(prefix: &str) -> Option<Ipv4Addr> {
    let (net, _) = parse_prefix(prefix)?;
    Some(Ipv4Addr::from(u32::from(net) + 1))
}

/// Guest address of seat `seat` on `prefix`: network + 10 + seat number.
pub fn guest_address(prefix: &str, seat: u8) -> Option<Ipv4Addr> {
    let (net, len) = parse_prefix(prefix)?;
    let host = GUEST_HOST_OFFSET + u32::from(seat);
    let size = 1u32 << (32 - u32::from(len));
    (host < size - 1).then(|| Ipv4Addr::from(u32::from(net) + host))
}

/// Whether `ip` is a usable host address inside `prefix` (not network, gateway, broadcast).
pub fn in_prefix(prefix: &str, ip: Ipv4Addr) -> bool {
    let Some((net, len)) = parse_prefix(prefix) else { return false };
    let mask = u32::MAX << (32 - u32::from(len));
    let v = u32::from(ip);
    v & mask == u32::from(net) && v != u32::from(net) && v != u32::from(net) + 1 && v != (u32::from(net) | !mask)
}

/// Absolute Windows path: `X:\...` or `\\server\share\...`, no control characters, no
/// `..` component, no quote (paths go into PowerShell literals and SMB share paths).
pub fn is_abs_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let unc = p.starts_with(r"\\") && p.len() > 2;
    (drive || unc)
        && p.len() <= 260
        // ' and U+2018 to U+201B all delimit a PowerShell single-quoted literal.
        && !p.chars().any(|c| c.is_control() || matches!(c, '"' | '\'' | '`' | '$' | '\u{2018}'..='\u{201b}'))
        && !p.split('\\').any(|c| c == "..")
}

/// Guest mount point rule (spec 10 §6 "invalid mount point (sandbox)"): a drive letter
/// other than `C:` (`D:` / `D:\`), or a `C:\` path with at least one folder, no trailing
/// backslash.
pub fn mount_point_ok(p: &str) -> bool {
    let b = p.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return false;
    }
    let c_drive = b[0].eq_ignore_ascii_case(&b'C');
    if b.len() == 2 || (b.len() == 3 && b[2] == b'\\') {
        return !c_drive;
    }
    c_drive
        && b[2] == b'\\'
        && !p.ends_with('\\')
        && !p.contains(r"\\")
        && is_abs_windows_path(p)
        && !p[3..].chars().any(|c| ":*?<>|/".contains(c))
}

/// Default mount point of a host folder: `C:\HostDrive` for a drive root, else
/// `C:\HostFolders\<last folder>`.
pub fn default_guest_path(host: &str) -> String {
    let t = host.trim_end_matches('\\');
    if t.len() == 2 && t.as_bytes()[1] == b':' {
        return HOST_DRIVE_MOUNT.into();
    }
    let last = t.rsplit('\\').next().unwrap_or("Shared");
    format!(r"C:\HostFolders\{last}")
}

/// Global VM settings.
pub fn validate_settings(s: &VmSettings) -> Result<(), ProtoError> {
    if !s.storage_location.is_empty() && !is_abs_windows_path(&s.storage_location) {
        return Err(err("vm.storage_location", "invalid_path", "storage location must be an absolute path"));
    }
    if !(MEMORY_PERCENT_RANGE.0..=MEMORY_PERCENT_RANGE.1).contains(&s.memory_percent) {
        return Err(err("vm.memory_percent", "out_of_range", "memory share must be 10-90 %"));
    }
    if !(STORAGE_GB_RANGE.0..=STORAGE_GB_RANGE.1).contains(&s.storage_gb) {
        return Err(err("vm.storage_gb", "out_of_range", "storage must be 32-4096 GB"));
    }
    match s.image_kind {
        ImageKind::None => {}
        ImageKind::Wim => {
            let l = s.image_path.to_ascii_lowercase();
            if !is_abs_windows_path(&s.image_path) || !(l.ends_with(".wim") || l.ends_with(".esd")) {
                return Err(err("vm.image_path", "invalid_image", "image must be an absolute .wim or .esd path"));
            }
            if s.image_index == 0 || s.image_index > 64 {
                return Err(err("vm.image_index", "out_of_range", "image index must be 1-64"));
            }
        }
        ImageKind::Vhdx => {
            if !is_abs_windows_path(&s.image_path) || !s.image_path.to_ascii_lowercase().ends_with(".vhdx") {
                return Err(err("vm.image_path", "invalid_image", "image must be an absolute .vhdx path"));
            }
        }
    }
    if !s.language_cab.is_empty()
        && (!is_abs_windows_path(&s.language_cab) || !s.language_cab.to_ascii_lowercase().ends_with(".cab"))
    {
        return Err(err("vm.language_cab", "invalid_path", "language pack must be an absolute .cab path"));
    }
    if parse_prefix(&s.nat_prefix).is_none() {
        return Err(err("vm.nat_prefix", "invalid_prefix", "NAT prefix must be a.b.c.0/16-28"));
    }
    Ok(())
}

/// One VM seat on its own (`port` = the seat's stream base port).
pub fn validate_seat(v: &VmSeat, port: u16) -> Result<(), ProtoError> {
    if v.memory_mb != 0 && !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&v.memory_mb) {
        return Err(err("vm.memory_mb", "out_of_range", "VM memory must be 0 (automatic) or 2048 MB or more"));
    }
    if v.processors > 64 {
        return Err(err("vm.processors", "out_of_range", "at most 64 virtual processors"));
    }
    match (v.gpu, &v.gpu_adapter) {
        (GpuPartitionMode::Adapter, None) => {
            return Err(err("vm.gpu_adapter", "missing_adapter", "choose the GPU to partition"));
        }
        (_, Some(a)) if a.is_empty() || a.len() > 512 || a.chars().any(|c| c.is_control() || c == '\'') => {
            return Err(err("vm.gpu_adapter", "invalid_adapter", "invalid GPU instance path"));
        }
        _ => {}
    }
    if v.stop == VmStopMode::Save && v.gpu != GpuPartitionMode::Off {
        return Err(err("vm.stop", "save_with_gpu", "a VM with a GPU partition cannot be saved; shut it down"));
    }
    if v.shared_folders.len() > MAX_SHARED_FOLDERS {
        return Err(err("vm.shared_folders", "too_many", "at most 16 shared folders"));
    }
    for (i, f) in v.shared_folders.iter().enumerate() {
        if !is_abs_windows_path(&f.host_path) || f.host_path.starts_with(r"\\") {
            return Err(err("vm.shared_folders.host_path", "invalid_path", "host folder must be an absolute local path"));
        }
        if !mount_point_ok(&f.guest_path) {
            return Err(err("vm.shared_folders.guest_path", "invalid_mount_point", "invalid mount point"));
        }
        if v.shared_folders[..i].iter().any(|o| o.guest_path.eq_ignore_ascii_case(&f.guest_path) || o.id == f.id) {
            return Err(err("vm.shared_folders.guest_path", "duplicate", "mount point used twice"));
        }
    }
    if v.forwarded_ports.len() > MAX_FORWARDED_PORTS {
        return Err(err("vm.forwarded_ports", "too_many", "at most 64 forwarded ports"));
    }
    let stream = stream_forwards(port);
    for (i, p) in v.forwarded_ports.iter().enumerate() {
        if p.host_port == 0 || p.guest_port == 0 {
            return Err(err("vm.forwarded_ports.host_port", "invalid_port", "ports must be 1-65535"));
        }
        if p.description.chars().count() > 128 || p.description.chars().any(char::is_control) {
            return Err(err("vm.forwarded_ports.description", "invalid_value", "invalid description"));
        }
        if ports::is_valid_base(port) && ports::block(port).contains(&p.host_port) {
            return Err(err("vm.forwarded_ports.host_port", "port_in_stream_block", "host port is inside the seat's stream ports"));
        }
        if stream.iter().any(|s| s.protocol == p.protocol && s.guest_port == p.guest_port) {
            return Err(err("vm.forwarded_ports.guest_port", "port_in_stream_block", "guest port is a stream port"));
        }
        if v.forwarded_ports[..i].iter().any(|o| o.protocol == p.protocol && o.host_port == p.host_port) {
            return Err(err("vm.forwarded_ports.host_port", "duplicate", "host port forwarded twice"));
        }
    }
    if !v.guest_address.is_empty() && v.guest_address.parse::<Ipv4Addr>().is_err() {
        return Err(err("vm.guest_address", "invalid_address", "guest address must be IPv4"));
    }
    Ok(())
}

/// A VM seat against another VM seat (`port`s = their stream base ports): no host port in
/// the other seat's stream block, no host port forwarded by both, distinct guest addresses.
pub fn validate_against(v: &VmSeat, port: u16, other: &VmSeat, other_port: u16) -> Result<(), ProtoError> {
    for p in &v.forwarded_ports {
        if ports::is_valid_base(other_port) && ports::block(other_port).contains(&p.host_port) {
            return Err(err("vm.forwarded_ports.host_port", "port_overlap", "host port is inside another seat's stream ports"));
        }
        if other.forwarded_ports.iter().any(|o| o.protocol == p.protocol && o.host_port == p.host_port) {
            return Err(err("vm.forwarded_ports.host_port", "port_overlap", "host port is forwarded by another seat"));
        }
    }
    let _ = port;
    if !v.guest_address.is_empty() && v.guest_address == other.guest_address {
        return Err(err("vm.guest_address", "duplicate", "guest address used by another seat"));
    }
    Ok(())
}

/// A host port of any remote seat's forwards inside a non-VM remote seat's block.
pub fn forward_hits_block(v: &VmSeat, block_base: u16) -> bool {
    ports::is_valid_base(block_base) && v.forwarded_ports.iter().any(|p| ports::block(block_base).contains(&p.host_port))
}

/// One port mapping of the NAT (host port -> guest port).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Forward {
    pub protocol: PortProtocol,
    pub host_port: u16,
    pub guest_port: u16,
}

/// The seat's external stream ports, forwarded 1:1 (the guest's stream server listens on
/// the same base port). The web UI port (`p+1`) is never forwarded.
pub fn stream_forwards(port: u16) -> Vec<Forward> {
    if !ports::is_valid_base(port) {
        return Vec::new();
    }
    ports::seat_ports(port)
        .into_iter()
        .filter(|p| p.external)
        .map(|p| Forward {
            protocol: match p.proto {
                ports::Proto::Tcp => PortProtocol::Tcp,
                ports::Proto::Udp => PortProtocol::Udp,
            },
            host_port: p.port,
            guest_port: p.port,
        })
        .collect()
}

/// Every forward of a VM seat: the stream ports, then the seat's own.
pub fn all_forwards(v: &VmSeat, port: u16) -> Vec<Forward> {
    let mut out = stream_forwards(port);
    out.extend(v.forwarded_ports.iter().map(|p| Forward { protocol: p.protocol, host_port: p.host_port, guest_port: p.guest_port }));
    out
}

/// Memory of a VM seat in MB: its own value, else `percent` of `host_mb` divided by the
/// number of VM seats, rounded down to 2 MB (Hyper-V wants even MB), at least
/// [`MIN_MEMORY_MB`].
pub fn memory_for(v: &VmSeat, percent: u8, host_mb: u64, vm_seats: usize) -> u32 {
    if v.memory_mb != 0 {
        return v.memory_mb & !1;
    }
    let share = host_mb.saturating_mul(u64::from(percent)) / 100 / (vm_seats.max(1) as u64);
    (share.min(u64::from(MAX_MEMORY_MB)) as u32 & !1).max(MIN_MEMORY_MB)
}

/// Virtual processors of a VM seat: its own value, else half of `host_logical` (>= 2).
pub fn processors_for(v: &VmSeat, host_logical: u32) -> u32 {
    if v.processors != 0 {
        return u32::from(v.processors).min(host_logical.max(1));
    }
    (host_logical / 2).max(2).min(host_logical.max(1))
}

/// Advertised host name: display name + ` (VM)` unless hidden.
pub fn advertised_name(display_name: &str, v: &VmSeat) -> String {
    if v.hide_vm_suffix { display_name.to_string() } else { format!("{display_name}{VM_SUFFIX}") }
}

// ---- guest link (host <-> guest over a Hyper-V socket, JSON lines) ----------------------

/// A shared folder as the guest maps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestShare {
    /// `\\<gateway>\<share>`.
    pub unc: String,
    pub guest_path: String,
    pub read_only: bool,
}

/// Everything the guest needs to run the seat (sent once per connection). The passwords
/// travel only over the guest link and live only in memory on the host side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestConfig {
    /// Seat id (GUID) and name of the seat on the host.
    pub seat_id: String,
    pub seat_name: String,
    /// Host name the stream server advertises ([`advertised_name`]).
    pub advertised_name: String,
    pub port: u16,
    pub stream_backend: crate::remote::StreamBackendKind,
    pub stream: crate::remote::SeatStreamSettings,
    pub audio: crate::remote::SeatAudioSettings,
    pub verbosity: crate::remote::Verbosity,
    /// Seat user in the guest (created there when missing) and its password.
    pub user_name: String,
    pub password: crate::Secret,
    /// Guest network: address / prefix length / gateway (also the DNS forwarder).
    pub address: String,
    pub prefix_len: u8,
    pub gateway: String,
    /// DNS servers of the host (the NAT does not answer DNS).
    #[serde(default)]
    pub dns: Vec<String>,
    pub shares: Vec<GuestShare>,
    /// SMB account of the shares on the host and its password.
    pub share_user: String,
    pub share_password: crate::Secret,
    /// Apps started in the seat session (the seat's `apps`).
    pub apps: Vec<crate::remote::AutostartApp>,
    /// The seat's stream app list (`set_apps`), kept on the host; `None` = the stream
    /// server's defaults. The guest writes it before the server starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_apps: Option<Vec<serde_json::Value>>,
    /// The Pulsar backend's permanent connect password, kept on the host (`None` = none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_password: Option<crate::Secret>,
    /// `regenerate_seat_identity` asked while the VM was off: the guest makes a new stream
    /// identity before the server starts.
    #[serde(default)]
    pub regenerate_identity: bool,
    /// Random per seat start: the guest does one-time work of a start (the identity reset)
    /// once, even when the link reconnects and the same config comes again.
    #[serde(default)]
    pub start_nonce: String,
}

impl GuestConfig {
    /// The seat as a remote seat record inside the guest (stream server config): the
    /// advertised name, the seat's port and stream settings; no RDP client name, no VM.
    pub fn to_record(&self) -> crate::remote::RemoteSeatRecord {
        crate::remote::RemoteSeatRecord {
            id: self.seat_id.clone(),
            name: self.seat_name.clone(),
            display_name: self.advertised_name.clone(),
            user_name: self.user_name.clone(),
            port: self.port,
            start_with_service: true,
            audio: self.audio,
            stream: self.stream.clone(),
            stream_backend: self.stream_backend,
            autostart: self.apps.clone(),
            ..Default::default()
        }
    }

    /// Checks before the guest applies anything (the host is trusted, but every value
    /// reaches a script or the registry).
    pub fn check(&self) -> Result<(), String> {
        crate::remote::validate::seat_name(&self.seat_name).map_err(|e| e.message)?;
        crate::remote::validate::display_name(&self.advertised_name).map_err(|e| e.message)?;
        crate::remote::validate::port(self.port).map_err(|e| e.message)?;
        if self.user_name.is_empty() {
            return Err("no seat user".into());
        }
        crate::remote::validate::user_name(&self.user_name).map_err(|e| e.message)?;
        for a in [&self.address, &self.gateway].into_iter().chain(self.dns.iter()) {
            a.parse::<Ipv4Addr>().map_err(|_| format!("not an IPv4 address: {a}"))?;
        }
        if !(16..=28).contains(&self.prefix_len) {
            return Err("bad prefix length".into());
        }
        for s in &self.shares {
            // Hidden share names end in `$`.
            let unc = s.unc.strip_suffix('$').unwrap_or(&s.unc);
            if !mount_point_ok(&s.guest_path) || !is_abs_windows_path(unc) || !unc.starts_with(r"\\") {
                return Err(format!("bad share {}", s.guest_path));
            }
        }
        if self.share_user.is_empty() || !self.share_user.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("bad share user".into());
        }
        Ok(())
    }
}

/// Phase the guest reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestPhase {
    /// Guest service up, waiting for its config.
    Waiting,
    /// Applying network, user, shares.
    Configuring,
    /// Seat user signed in at the VM console.
    SignedIn,
    /// Stream server running.
    Streaming,
    Failed,
}

/// One guest link message (one JSON object per line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMsg {
    /// Guest -> host, first line.
    Hello { protocol: u32, version: String },
    /// Host -> guest.
    Config { config: Box<GuestConfig> },
    /// Guest -> host.
    State {
        phase: GuestPhase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Host -> guest: a Moonlight pairing PIN for the guest's stream server.
    SubmitPin { pin: String, client_name: String },
    /// Host -> guest: forget every paired client.
    UnpairAll,
    /// Guest -> host: answer of a host request (`ok` false = `detail` says why).
    Result {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Host -> guest: the seat stops; sign out and shut down. The guest keeps its stream
    /// server down until the next `config`.
    Shutdown,
    /// Host -> guest: one protocol 3 control request of the seat page (`RequestBody` as
    /// JSON, `type` tagged) that the guest's stream server or account answers. `id` pairs
    /// it with the [`GuestMsg::Reply`].
    Request { id: u64, request: serde_json::Value },
    /// Guest -> host: answer of a [`GuestMsg::Request`]: `response` is the `ResponseBody`
    /// as JSON, or `error` says why not.
    Reply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ProtoError>,
    },
    /// Guest -> host: a stream server notice (`pair_pending`, `pair_ended`,
    /// `stream_started`, `stream_ended`, `otp_changed`, `trust_changed`,
    /// `relay_auth_required`); the host publishes it as the seat's `stream` event.
    Notice { kind: String, data: serde_json::Value },
}

/// Guest link notices the host turns into `stream` events (anything else is dropped).
pub const GUEST_NOTICE_KINDS: [&str; 7] =
    ["pair_pending", "pair_ended", "stream_started", "stream_ended", "otp_changed", "trust_changed", "relay_auth_required"];

/// One line for the link (`\n` terminated). `None` when it would exceed the line limit.
pub fn encode_line(m: &GuestMsg) -> Option<Vec<u8>> {
    let mut v = serde_json::to_vec(m).ok()?;
    v.push(b'\n');
    (v.len() <= GUEST_LINK_MAX_LINE).then_some(v)
}

/// Splits complete lines off `buf` and parses them; a malformed line is an `Err` entry
/// (the caller drops the link). An over-long partial line is an error too.
pub fn decode_lines(buf: &mut Vec<u8>) -> Vec<Result<GuestMsg, String>> {
    let mut out = Vec::new();
    while let Some(i) = buf.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buf.drain(..=i).collect();
        let line = &line[..line.len() - 1];
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        out.push(serde_json::from_slice(line).map_err(|e| format!("guest link: {e}")));
    }
    if buf.len() > GUEST_LINK_MAX_LINE {
        buf.clear();
        out.push(Err("guest link: line too long".into()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat() -> VmSeat {
        VmSeat::default()
    }

    #[test]
    fn prefixes_and_addresses() {
        assert_eq!(parse_prefix("172.31.240.0/24"), Some((Ipv4Addr::new(172, 31, 240, 0), 24)));
        assert!(parse_prefix("172.31.240.1/24").is_none(), "host bits set");
        assert!(parse_prefix("10.0.0.0/8").is_none(), "too wide");
        assert!(parse_prefix("10.0.0.0/30").is_none(), "too narrow");
        assert!(parse_prefix("x").is_none());
        assert_eq!(gateway(DEFAULT_NAT_PREFIX), Some(Ipv4Addr::new(172, 31, 240, 1)));
        assert_eq!(guest_address(DEFAULT_NAT_PREFIX, 2), Some(Ipv4Addr::new(172, 31, 240, 12)));
        assert_eq!(guest_address("10.9.0.0/28", 3), Some(Ipv4Addr::new(10, 9, 0, 13)));
        assert_eq!(guest_address("10.9.0.0/28", 5), None, "past the broadcast");
        assert!(in_prefix(DEFAULT_NAT_PREFIX, Ipv4Addr::new(172, 31, 240, 12)));
        assert!(!in_prefix(DEFAULT_NAT_PREFIX, Ipv4Addr::new(172, 31, 240, 1)), "gateway");
        assert!(!in_prefix(DEFAULT_NAT_PREFIX, Ipv4Addr::new(172, 31, 240, 255)));
        assert!(!in_prefix(DEFAULT_NAT_PREFIX, Ipv4Addr::new(172, 31, 241, 5)));
    }

    #[test]
    fn mount_points_follow_the_spec_rule() {
        for ok in ["D:", r"D:\", r"C:\HostDrive", r"C:\Games\Steam", "z:"] {
            assert!(mount_point_ok(ok), "{ok}");
        }
        for bad in ["C:", r"C:\", r"C:\Games\", r"D:\Games", "", "C", r"C:\a\\b", r"C:\a:b", r"C:\..\x", r"\\srv\x"] {
            assert!(!mount_point_ok(bad), "{bad}");
        }
        assert_eq!(default_guest_path(r"E:\"), HOST_DRIVE_MOUNT);
        assert_eq!(default_guest_path("E:"), HOST_DRIVE_MOUNT);
        assert_eq!(default_guest_path(r"D:\Games\Steam"), r"C:\HostFolders\Steam");
        assert!(is_abs_windows_path(r"D:\x y"));
        assert!(!is_abs_windows_path(r"D:\x'; rm"));
        assert!(!is_abs_windows_path("D:\\Bob\u{2019}s Games"), "typographic quote");
        assert!(!is_abs_windows_path("D:\\a\u{201a}b"));
        assert!(!is_abs_windows_path("relative"));
    }

    #[test]
    fn settings_validation() {
        let mut s = VmSettings::default();
        assert!(validate_settings(&s).is_ok());
        s.memory_percent = 95;
        assert_eq!(validate_settings(&s).unwrap_err().field.as_deref(), Some("vm.memory_percent"));
        s.memory_percent = 50;
        s.image_kind = ImageKind::Wim;
        s.image_path = r"D:\sources\install.esd".into();
        assert!(validate_settings(&s).is_ok());
        s.image_index = 0;
        assert!(validate_settings(&s).is_err());
        s.image_index = 6;
        s.image_kind = ImageKind::Vhdx;
        assert_eq!(validate_settings(&s).unwrap_err().reason.as_deref(), Some("invalid_image"));
        s.image_path = r"D:\vm\base.vhdx".into();
        assert!(validate_settings(&s).is_ok());
        s.nat_prefix = "192.168.1.7/24".into();
        assert!(validate_settings(&s).is_err());
        s.nat_prefix = DEFAULT_NAT_PREFIX.into();
        s.storage_location = "vm".into();
        assert!(validate_settings(&s).is_err());
    }

    #[test]
    fn seat_validation() {
        let base = 48100;
        let mut v = seat();
        assert!(validate_seat(&v, base).is_ok());
        v.memory_mb = 1024;
        assert!(validate_seat(&v, base).is_err());
        v.memory_mb = 8192;
        v.gpu = GpuPartitionMode::Adapter;
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("missing_adapter"));
        v.gpu_adapter = Some(r"\\?\PCI#VEN_10DE&DEV_2684#4&1".into());
        assert!(validate_seat(&v, base).is_ok());
        v.stop = VmStopMode::Save;
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("save_with_gpu"));
        v.gpu = GpuPartitionMode::Off;
        v.gpu_adapter = None;
        assert!(validate_seat(&v, base).is_ok());
        v.shared_folders.push(SharedFolder { id: "a".into(), host_path: r"E:\".into(), guest_path: r"C:\HostDrive".into(), read_only: true });
        v.shared_folders.push(SharedFolder { id: "b".into(), host_path: r"F:\x".into(), guest_path: r"c:\hostdrive".into(), read_only: false });
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("duplicate"));
        v.shared_folders[1].guest_path = r"C:\".into();
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("invalid_mount_point"));
        v.shared_folders[1].guest_path = "F:".into();
        assert!(validate_seat(&v, base).is_ok());
        v.forwarded_ports.push(ForwardedPort { id: "p".into(), description: "RDP".into(), protocol: PortProtocol::Tcp, host_port: 48099, guest_port: 3389 });
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("port_in_stream_block"));
        v.forwarded_ports[0].host_port = 13389;
        assert!(validate_seat(&v, base).is_ok());
        v.forwarded_ports.push(ForwardedPort { id: "q".into(), protocol: PortProtocol::Tcp, host_port: 13389, guest_port: 22, ..Default::default() });
        assert_eq!(validate_seat(&v, base).unwrap_err().reason.as_deref(), Some("duplicate"));
        v.forwarded_ports[1].protocol = PortProtocol::Udp;
        assert!(validate_seat(&v, base).is_ok(), "same port, other protocol");
        let mut o = seat();
        o.forwarded_ports.push(ForwardedPort { id: "r".into(), protocol: PortProtocol::Tcp, host_port: 13389, guest_port: 80, ..Default::default() });
        assert_eq!(validate_against(&v, base, &o, 48200).unwrap_err().reason.as_deref(), Some("port_overlap"));
        o.forwarded_ports.clear();
        assert!(validate_against(&v, base, &o, 48200).is_ok());
        v.forwarded_ports[0].host_port = 48200;
        assert!(validate_against(&v, base, &o, 48200).is_err(), "inside the other seat's block");
        assert!(forward_hits_block(&v, 48200));
    }

    #[test]
    fn forwards_memory_and_names() {
        let f = stream_forwards(48100);
        assert_eq!(f.len(), 6, "web UI port stays local");
        assert!(!f.iter().any(|x| x.host_port == 48101));
        assert!(f.iter().any(|x| x.host_port == 48109 && x.protocol == PortProtocol::Udp));
        let mut v = seat();
        v.forwarded_ports.push(ForwardedPort { id: "p".into(), protocol: PortProtocol::Udp, host_port: 9000, guest_port: 9001, ..Default::default() });
        assert_eq!(all_forwards(&v, 48100).last(), Some(&Forward { protocol: PortProtocol::Udp, host_port: 9000, guest_port: 9001 }));
        assert_eq!(memory_for(&v, 50, 32 * 1024, 2), 8192);
        assert_eq!(memory_for(&v, 50, 4 * 1024, 4), MIN_MEMORY_MB);
        v.memory_mb = 6001;
        assert_eq!(memory_for(&v, 50, 32 * 1024, 2), 6000);
        assert_eq!(processors_for(&v, 16), 8);
        assert_eq!(processors_for(&v, 2), 2);
        v.processors = 32;
        assert_eq!(processors_for(&v, 16), 16);
        assert_eq!(advertised_name("Salon", &v), "Salon (VM)");
        v.hide_vm_suffix = true;
        assert_eq!(advertised_name("Salon", &v), "Salon");
    }

    #[test]
    fn guest_link_request_reply_notice_round_trip() {
        let rq = GuestMsg::Request { id: 7, request: serde_json::json!({"type": "list_paired_clients", "seat": 3}) };
        let ok = GuestMsg::Reply { id: 7, response: Some(serde_json::json!({"type": "ack"})), error: None };
        let no = GuestMsg::Reply { id: 8, response: None, error: Some(ProtoError::new(ErrorCode::Unsupported, "x").with_reason("y")) };
        let nt = GuestMsg::Notice { kind: "pair_ended".into(), data: serde_json::json!({"result": "paired"}) };
        let mut buf = Vec::new();
        for m in [&rq, &ok, &no, &nt] {
            buf.extend(encode_line(m).unwrap());
        }
        let got: Vec<GuestMsg> = decode_lines(&mut buf).into_iter().map(Result::unwrap).collect();
        assert_eq!(got, vec![rq, ok, no, nt]);
        let line = String::from_utf8(encode_line(&GuestMsg::Reply { id: 1, response: None, error: None }).unwrap()).unwrap();
        assert_eq!(line.trim_end(), "{\"type\":\"reply\",\"id\":1}", "empty fields are left out");
        assert!(GUEST_NOTICE_KINDS.contains(&"pair_ended"));
    }

    #[test]
    fn guest_link_lines() {
        let m = GuestMsg::State { phase: GuestPhase::SignedIn, detail: None };
        let mut buf = encode_line(&m).unwrap();
        buf.extend_from_slice(b"{\"type\":\"shutdown\"}\n\n{\"type\":\"res");
        let got = decode_lines(&mut buf);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].as_ref().unwrap(), &m);
        assert_eq!(got[1].as_ref().unwrap(), &GuestMsg::Shutdown);
        assert_eq!(buf, b"{\"type\":\"res");
        buf.extend_from_slice(b"ult\",\"ok\":true}\nnot json\n");
        let got = decode_lines(&mut buf);
        assert_eq!(got[0].as_ref().unwrap(), &GuestMsg::Result { ok: true, detail: None });
        assert!(got[1].is_err());
        let mut long = vec![b'x'; GUEST_LINK_MAX_LINE + 1];
        assert!(decode_lines(&mut long)[0].is_err());
        assert!(long.is_empty());
        let big = GuestMsg::State { phase: GuestPhase::Failed, detail: Some("y".repeat(GUEST_LINK_MAX_LINE)) };
        assert!(encode_line(&big).is_none());
    }

    #[test]
    fn config_message_carries_the_password_once() {
        let c = GuestConfig {
            seat_id: "id".into(),
            seat_name: "Salon".into(),
            advertised_name: "Salon (VM)".into(),
            port: 48100,
            stream_backend: crate::remote::StreamBackendKind::Sunshine,
            stream: Default::default(),
            audio: Default::default(),
            verbosity: Default::default(),
            user_name: "oyuncu".into(),
            password: crate::Secret::new("pw"),
            address: "172.31.240.12".into(),
            prefix_len: 24,
            gateway: "172.31.240.1".into(),
            dns: vec!["192.168.1.1".into()],
            shares: vec![],
            share_user: "PulsarVmShare".into(),
            share_password: crate::Secret::new("sp"),
            apps: vec![],
            stream_apps: None,
            connect_password: None,
            regenerate_identity: false,
            start_nonce: String::new(),
        };
        assert!(c.check().is_ok());
        let rec = c.to_record();
        assert_eq!((rec.display_name.as_str(), rec.port, rec.vm.is_none()), ("Salon (VM)", 48100, true));
        let mut bad = c.clone();
        bad.address = "x".into();
        assert!(bad.check().is_err());
        bad = c.clone();
        bad.shares.push(GuestShare { unc: r"\\172.31.240.1\s$".into(), guest_path: r"C:\".into(), read_only: true });
        assert!(bad.check().is_err());
        let m = GuestMsg::Config { config: Box::new(c) };
        let line = encode_line(&m).unwrap();
        let mut b = line.clone();
        assert_eq!(decode_lines(&mut b)[0].as_ref().unwrap(), &m);
        assert!(!format!("{m:?}").contains("\"pw\""), "Debug never shows the password");
    }
}
