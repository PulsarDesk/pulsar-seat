// SPDX-License-Identifier: GPL-3.0-only
//! Client of the source-IP callout driver `drivers/seat-wfp` (spec 80 §4.2, §11.3).
//!
//! The driver (KMDF, Windows Filtering Platform, ALE bind-redirect and connect-redirect
//! layers, IPv4) rewrites the local address of TCP / UDP sockets of a seat's processes to
//! the seat's address. Which process belongs to which seat comes from a **session id ->
//! seat map** the service pushes over the control device `\\.\PulsarSeatWfp` (DACL: the
//! service SID only, exclusive open). This module holds:
//!
//! - the wire format ([`encode_map`], [`decode_map`], the IOCTL codes and the reply
//!   layouts), mirrored from `drivers/seat-wfp/include/seatwfp_ioctl.h` and pinned against
//!   it by the tests;
//! - [`lookup`], the same decision the driver makes (tests and diagnostics);
//! - [`dos_to_nt`]: the program list holds DOS paths (`C:\Games\Lan`), the driver sees NT
//!   image paths (`\device\harddiskvolume3\games\lan`);
//! - [`WfpControl`], the device handle as a trait, with [`FakeWfp`] for tests and
//!   `win::WinWfp` on Windows; [`Publisher`] sends a map only when it changed.
//!
//! The environment variable `PULSAR_SEAT_SOURCE_IP` ([`crate::srcip::launch_env`]) stays
//! as the user-mode fallback when the driver is not loaded (unsigned builds never are).
//! **HYPOTHESIS** (nothing loaded on hardware): binds and connects of seat processes leave
//! with the seat address; see `drivers/seat-wfp/README.md` for the test plan.

use crate::srcip::{AppRule, SeatAddress};
use std::net::Ipv4Addr;
use std::sync::Mutex;

pub const INTERFACE_MAJOR: u16 = 1;
pub const INTERFACE_MINOR: u16 = 0;

/// `NT SERVICE\PulsarSeat` (the only SID in the control device's DACL).
pub const SERVICE_SID: &str = "S-1-5-80-2060529063-4084430102-647711624-1206425353-118540754";

const FILE_DEVICE_UNKNOWN: u32 = 0x22;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;
const RW_ACCESS: u32 = 1 | 2;

const fn ctl_code(function: u32, access: u32) -> u32 {
    (FILE_DEVICE_UNKNOWN << 16) | (access << 14) | (function << 2) | METHOD_BUFFERED
}

pub const IOCTL_GET_VERSION: u32 = ctl_code(0x800, FILE_ANY_ACCESS);
pub const IOCTL_SET_MAP: u32 = ctl_code(0x801, RW_ACCESS);
pub const IOCTL_CLEAR: u32 = ctl_code(0x802, RW_ACCESS);
pub const IOCTL_GET_STATS: u32 = ctl_code(0x803, RW_ACCESS);

pub const MAX_SEATS: usize = 64;
pub const MAX_PATHS: usize = 4096;
pub const MAX_PATH_CHARS: usize = 1024;
pub const MAX_MAP_BYTES: usize = 1024 * 1024;

pub const VERSION_SIZE: usize = 16;
pub const HEADER_SIZE: usize = 24;
pub const SEAT_SIZE: usize = 32;
pub const PATH_SIZE: usize = 8;
pub const STATS_SIZE: usize = 40;

pub const RULE_ALL: u16 = 0;
pub const RULE_ONLY_LISTED: u16 = 1;
pub const RULE_ALL_EXCEPT_LISTED: u16 = 2;

/// Wildcard binds of TCP / UDP get the seat address.
pub const SF_BIND: u16 = 0x1;
/// Outbound connects get it as their local address.
pub const SF_CONNECT: u16 = 0x2;

pub const VF_PASS_THROUGH: u32 = 0x1;
pub const VF_CALLOUTS: u32 = 0x2;
pub const VF_FILTERS: u32 = 0x4;
pub const VF_MAP: u32 = 0x8;

fn rule_code(r: AppRule) -> u16 {
    match r {
        AppRule::All => RULE_ALL,
        AppRule::OnlyListed => RULE_ONLY_LISTED,
        AppRule::AllExceptListed => RULE_ALL_EXCEPT_LISTED,
    }
}

fn rule_from(code: u16) -> Option<AppRule> {
    match code {
        RULE_ALL => Some(AppRule::All),
        RULE_ONLY_LISTED => Some(AppRule::OnlyListed),
        RULE_ALL_EXCEPT_LISTED => Some(AppRule::AllExceptListed),
        _ => None,
    }
}

/// One seat of the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WfpSeat {
    pub seat: u32,
    pub session_id: u32,
    pub address: Ipv4Addr,
    pub rule: AppRule,
    /// [`SF_BIND`] | [`SF_CONNECT`].
    pub flags: u16,
    /// NT paths, lower case, no trailing backslash ([`dos_to_nt`]).
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("more than {MAX_SEATS} seats")]
    TooManySeats,
    #[error("more than {MAX_PATHS} paths")]
    TooManyPaths,
    #[error("path empty or longer than {MAX_PATH_CHARS} characters")]
    BadPath,
    #[error("address {0} cannot be a source address")]
    BadAddress(Ipv4Addr),
    #[error("session {0} appears twice")]
    DuplicateSession(u32),
    #[error("map larger than {MAX_MAP_BYTES} bytes")]
    TooLarge,
    #[error("malformed map: {0}")]
    Malformed(&'static str),
}

/// Never a source address: 0.0.0.0, loopback, multicast, broadcast and class E (same
/// rule as the driver's `SeatWfpMapValidate`).
pub fn usable_source(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_unspecified() || o[0] == 127 || o[0] >= 224)
}

/// The SET_MAP buffer. Validates exactly what the driver validates.
pub fn encode_map(generation: u32, seats: &[WfpSeat]) -> Result<Vec<u8>, MapError> {
    if seats.len() > MAX_SEATS {
        return Err(MapError::TooManySeats);
    }
    let path_count: usize = seats.iter().map(|s| s.paths.len()).sum();
    if path_count > MAX_PATHS {
        return Err(MapError::TooManyPaths);
    }
    for (i, s) in seats.iter().enumerate() {
        if !usable_source(s.address) {
            return Err(MapError::BadAddress(s.address));
        }
        if seats[..i].iter().any(|o| o.session_id == s.session_id) {
            return Err(MapError::DuplicateSession(s.session_id));
        }
    }
    let tables = HEADER_SIZE + seats.len() * SEAT_SIZE + path_count * PATH_SIZE;
    let mut chars: Vec<Vec<u16>> = Vec::with_capacity(path_count);
    for s in seats {
        for p in &s.paths {
            let w: Vec<u16> = p.encode_utf16().collect();
            if w.is_empty() || w.len() > MAX_PATH_CHARS {
                return Err(MapError::BadPath);
            }
            chars.push(w);
        }
    }
    let total = tables + chars.iter().map(|w| w.len() * 2).sum::<usize>();
    if total > MAX_MAP_BYTES {
        return Err(MapError::TooLarge);
    }
    let mut b = Vec::with_capacity(total);
    b.extend_from_slice(&(total as u32).to_le_bytes());
    b.extend_from_slice(&INTERFACE_MAJOR.to_le_bytes());
    b.extend_from_slice(&INTERFACE_MINOR.to_le_bytes());
    b.extend_from_slice(&generation.to_le_bytes());
    b.extend_from_slice(&(seats.len() as u32).to_le_bytes());
    b.extend_from_slice(&(path_count as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    let mut first = 0u32;
    for s in seats {
        b.extend_from_slice(&s.session_id.to_le_bytes());
        b.extend_from_slice(&s.address.octets());
        b.extend_from_slice(&rule_code(s.rule).to_le_bytes());
        b.extend_from_slice(&(s.flags & (SF_BIND | SF_CONNECT)).to_le_bytes());
        b.extend_from_slice(&first.to_le_bytes());
        b.extend_from_slice(&(s.paths.len() as u32).to_le_bytes());
        b.extend_from_slice(&s.seat.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]);
        first += s.paths.len() as u32;
    }
    let mut off = tables;
    for w in &chars {
        b.extend_from_slice(&(off as u32).to_le_bytes());
        b.extend_from_slice(&(w.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        off += w.len() * 2;
    }
    for w in &chars {
        for c in w {
            b.extend_from_slice(&c.to_le_bytes());
        }
    }
    debug_assert_eq!(b.len(), total);
    Ok(b)
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Parses and validates a SET_MAP buffer (the driver's rules; used by [`FakeWfp`] and the
/// tests). Returns the generation and the seats.
pub fn decode_map(b: &[u8]) -> Result<(u32, Vec<WfpSeat>), MapError> {
    if b.len() < HEADER_SIZE {
        return Err(MapError::Malformed("short"));
    }
    if b.len() > MAX_MAP_BYTES {
        return Err(MapError::TooLarge);
    }
    if u32_at(b, 0) as usize != b.len() || u16_at(b, 4) != INTERFACE_MAJOR {
        return Err(MapError::Malformed("size or major"));
    }
    let generation = u32_at(b, 8);
    let seat_count = u32_at(b, 12) as usize;
    let path_count = u32_at(b, 16) as usize;
    if seat_count > MAX_SEATS {
        return Err(MapError::TooManySeats);
    }
    if path_count > MAX_PATHS {
        return Err(MapError::TooManyPaths);
    }
    let tables = HEADER_SIZE + seat_count * SEAT_SIZE + path_count * PATH_SIZE;
    if tables > b.len() {
        return Err(MapError::Malformed("tables"));
    }
    let mut paths = Vec::with_capacity(path_count);
    for i in 0..path_count {
        let o = HEADER_SIZE + seat_count * SEAT_SIZE + i * PATH_SIZE;
        let off = u32_at(b, o) as usize;
        let n = u16_at(b, o + 4) as usize;
        if n == 0 || n > MAX_PATH_CHARS || off % 2 != 0 || off < tables || off + n * 2 > b.len() {
            return Err(MapError::BadPath);
        }
        let w: Vec<u16> = (0..n).map(|k| u16_at(b, off + k * 2)).collect();
        paths.push(String::from_utf16_lossy(&w));
    }
    let mut seats: Vec<WfpSeat> = Vec::with_capacity(seat_count);
    for i in 0..seat_count {
        let o = HEADER_SIZE + i * SEAT_SIZE;
        let session_id = u32_at(b, o);
        let address = Ipv4Addr::new(b[o + 4], b[o + 5], b[o + 6], b[o + 7]);
        let rule = rule_from(u16_at(b, o + 8)).ok_or(MapError::Malformed("rule"))?;
        let flags = u16_at(b, o + 10);
        if flags & !(SF_BIND | SF_CONNECT) != 0 {
            return Err(MapError::Malformed("flags"));
        }
        if !usable_source(address) {
            return Err(MapError::BadAddress(address));
        }
        let first = u32_at(b, o + 12) as usize;
        let count = u32_at(b, o + 16) as usize;
        if first + count > path_count {
            return Err(MapError::Malformed("path range"));
        }
        if seats.iter().any(|s| s.session_id == session_id) {
            return Err(MapError::DuplicateSession(session_id));
        }
        seats.push(WfpSeat {
            seat: u32_at(b, o + 20),
            session_id,
            address,
            rule,
            flags,
            paths: paths[first..first + count].to_vec(),
        });
    }
    Ok((generation, seats))
}

fn path_matches(path: &str, app: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let a = app.trim_end_matches('\0').to_ascii_lowercase();
    a == p || a.strip_prefix(&p).is_some_and(|rest| rest.starts_with('\\'))
}

/// The driver's decision: the address a socket of `session_id` whose image is `app_nt`
/// (NT path, `None` = unknown) gets at the layer `want` ([`SF_BIND`] / [`SF_CONNECT`]).
pub fn lookup(seats: &[WfpSeat], session_id: u32, app_nt: Option<&str>, want: u16) -> Option<Ipv4Addr> {
    let s = seats.iter().find(|s| s.session_id == session_id)?;
    if s.flags & want == 0 {
        return None;
    }
    let listed = match (s.rule, app_nt) {
        (AppRule::All, _) | (_, None) => false,
        (_, Some(a)) => s.paths.iter().any(|p| path_matches(p, a)),
    };
    match s.rule {
        AppRule::All => Some(s.address),
        AppRule::OnlyListed => listed.then_some(s.address),
        AppRule::AllExceptListed => (!listed).then_some(s.address),
    }
}

/// DOS path of the program list -> the NT image path WFP reports (`ALE_APP_ID`), lower
/// case, without a trailing backslash. `device_of("C:")` answers the drive's NT device
/// (`QueryDosDeviceW`: `\Device\HarddiskVolume3`). UNC paths go through the multiple UNC
/// provider (`\device\mup\server\share\…`). Relative paths and unknown drives: `None`.
/// HYPOTHESIS: WFP's app id of a program started from a mapped network drive is its
/// `\device\mup\…` path (verify on hardware).
pub fn dos_to_nt(path: &str, device_of: impl Fn(&str) -> Option<String>) -> Option<String> {
    let p = path.trim().trim_matches('"').replace('/', "\\");
    let p = p.strip_prefix(r"\\?\").or_else(|| p.strip_prefix(r"\??\")).map(str::to_string).unwrap_or(p);
    let p = if let Some(unc) = p.strip_prefix(r"UNC\") { format!(r"\\{unc}") } else { p };
    let nt = if let Some(rest) = p.strip_prefix(r"\\") {
        if rest.is_empty() || rest.starts_with('\\') {
            return None;
        }
        format!(r"\device\mup\{rest}")
    } else {
        let b = p.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return None;
        }
        let dev = device_of(&p[..2])?;
        let dev = dev.trim_end_matches('\\');
        if dev.is_empty() {
            return None;
        }
        format!("{dev}{}", &p[2..])
    };
    let nt = nt.trim_end_matches('\\').to_lowercase();
    (nt.len() > 1 && nt.encode_utf16().count() <= MAX_PATH_CHARS).then_some(nt)
}

/// One seat that is signed in in its own session, ready for the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSeat {
    pub session_id: u32,
    pub address: SeatAddress,
}

/// The map for these seats: disabled list entries dropped, list paths converted with
/// `device_of` (paths that do not convert are dropped and returned for the log), every
/// seat with [`SF_BIND`] | [`SF_CONNECT`]. Seats without an address or with an address that
/// is never a source are left out (config validation reports them).
pub fn build_seats(seats: &[SessionSeat], device_of: impl Fn(&str) -> Option<String>) -> (Vec<WfpSeat>, Vec<String>) {
    let mut out: Vec<WfpSeat> = Vec::new();
    let mut dropped = Vec::new();
    for s in seats {
        let Some(address) = s.address.address else { continue };
        if !usable_source(address) || out.iter().any(|o| o.session_id == s.session_id) || out.len() >= MAX_SEATS {
            continue;
        }
        let mut paths = Vec::new();
        if s.address.rule != AppRule::All {
            for a in s.address.apps.iter().filter(|a| a.enabled) {
                match dos_to_nt(&a.path, &device_of) {
                    Some(p) if !paths.contains(&p) => paths.push(p),
                    Some(_) => {}
                    None => dropped.push(a.path.clone()),
                }
            }
        }
        out.push(WfpSeat {
            seat: u32::from(s.address.seat.get()),
            session_id: s.session_id,
            address,
            rule: s.address.rule,
            flags: SF_BIND | SF_CONNECT,
            paths,
        });
    }
    // Keep the whole map under the driver's path limit: later seats lose their lists
    // first (rare: 64 seats x 256 entries would be needed).
    let mut total: usize = out.iter().map(|s| s.paths.len()).sum();
    for s in out.iter_mut().rev() {
        if total <= MAX_PATHS {
            break;
        }
        total -= s.paths.len();
        dropped.append(&mut s.paths);
    }
    (out, dropped)
}

/// `SEATWFP_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u32,
    pub flags: u32,
}

pub fn parse_version(b: &[u8]) -> Option<DriverVersion> {
    (b.len() >= VERSION_SIZE).then(|| DriverVersion { major: u16_at(b, 0), minor: u16_at(b, 2), build: u32_at(b, 4), flags: u32_at(b, 8) })
}

/// `SEATWFP_STATS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DriverStats {
    pub bind_redirects: u64,
    pub connect_redirects: u64,
    pub skipped: u64,
    pub generation: u32,
    pub seat_count: u32,
    pub flags: u32,
}

pub fn parse_stats(b: &[u8]) -> Option<DriverStats> {
    (b.len() >= STATS_SIZE).then(|| DriverStats {
        bind_redirects: u64_at(b, 0),
        connect_redirects: u64_at(b, 8),
        skipped: u64_at(b, 16),
        generation: u32_at(b, 24),
        seat_count: u32_at(b, 28),
        flags: u32_at(b, 32),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WfpError {
    /// No control device: the driver is not installed or not loaded.
    #[error("source-IP driver not loaded")]
    NotLoaded,
    /// The device is held by another handle (exclusive).
    #[error("source-IP driver busy")]
    Busy,
    #[error("source-IP driver interface {found}, want {INTERFACE_MAJOR}")]
    VersionMismatch { found: u16 },
    /// Safe Mode or `Parameters\PassThrough`: the driver registers no filter.
    #[error("source-IP driver in pass-through mode")]
    PassThrough,
    #[error(transparent)]
    Map(#[from] MapError),
    #[error("source-IP driver call failed: {0}")]
    Os(String),
}

/// The control device.
pub trait WfpControl: Send + Sync {
    fn version(&self) -> Result<DriverVersion, WfpError>;
    fn set_map(&self, buf: &[u8]) -> Result<(), WfpError>;
    fn clear(&self) -> Result<(), WfpError>;
    fn stats(&self) -> Result<DriverStats, WfpError>;
}

/// In-memory driver for tests: applies the driver's validation.
#[derive(Default)]
pub struct FakeWfp {
    pub state: Mutex<FakeState>,
}

#[derive(Debug, Default, Clone)]
pub struct FakeState {
    pub loaded: bool,
    pub major: u16,
    pub pass_through: bool,
    pub generation: u32,
    pub seats: Vec<WfpSeat>,
    pub set_calls: u32,
}

impl FakeWfp {
    pub fn loaded() -> Self {
        Self { state: Mutex::new(FakeState { loaded: true, major: INTERFACE_MAJOR, ..Default::default() }) }
    }
    fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn seats(&self) -> Vec<WfpSeat> {
        self.st().seats.clone()
    }
    pub fn set_calls(&self) -> u32 {
        self.st().set_calls
    }
}

impl WfpControl for FakeWfp {
    fn version(&self) -> Result<DriverVersion, WfpError> {
        let s = self.st();
        if !s.loaded {
            return Err(WfpError::NotLoaded);
        }
        let mut flags = VF_CALLOUTS | VF_FILTERS;
        if s.pass_through {
            flags = VF_PASS_THROUGH;
        }
        if !s.seats.is_empty() {
            flags |= VF_MAP;
        }
        Ok(DriverVersion { major: s.major, minor: INTERFACE_MINOR, build: 1, flags })
    }
    fn set_map(&self, buf: &[u8]) -> Result<(), WfpError> {
        let mut s = self.st();
        if !s.loaded {
            return Err(WfpError::NotLoaded);
        }
        if s.pass_through {
            return Err(WfpError::PassThrough);
        }
        let (generation, seats) = decode_map(buf)?;
        s.generation = if seats.is_empty() { 0 } else { generation };
        s.seats = seats;
        s.set_calls += 1;
        Ok(())
    }
    fn clear(&self) -> Result<(), WfpError> {
        let mut s = self.st();
        s.seats.clear();
        s.generation = 0;
        Ok(())
    }
    fn stats(&self) -> Result<DriverStats, WfpError> {
        let s = self.st();
        Ok(DriverStats { generation: s.generation, seat_count: s.seats.len() as u32, ..Default::default() })
    }
}

/// How the seat addresses are applied right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    /// The driver holds the map; these seats are enforced.
    Driver { generation: u32, seats: Vec<u32> },
    /// No driver (or it refused): only programs that read `PULSAR_SEAT_SOURCE_IP` use
    /// the address. `reason` is a snake_case code.
    EnvironmentOnly { reason: String },
}

/// Sends the map when it changed (or after an error), with a rising generation.
#[derive(Default)]
pub struct Publisher {
    last: Mutex<Option<(Vec<WfpSeat>, u32)>>,
}

impl Publisher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget what was sent (the handle was reopened: the driver cleared its map).
    pub fn reset(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn publish(&self, ctl: &dyn WfpControl, seats: &[WfpSeat]) -> Enforcement {
        let env = |reason: &str| Enforcement::EnvironmentOnly { reason: reason.into() };
        let v = match ctl.version() {
            Ok(v) => v,
            Err(e) => {
                self.reset();
                return env(match e {
                    WfpError::Busy => "driver_busy",
                    _ => "driver_not_loaded",
                });
            }
        };
        if v.major != INTERFACE_MAJOR {
            self.reset();
            return env("driver_version_mismatch");
        }
        if v.flags & VF_PASS_THROUGH != 0 {
            self.reset();
            return env("driver_pass_through");
        }
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let enforced = |g: u32| Enforcement::Driver { generation: g, seats: seats.iter().map(|s| s.seat).collect() };
        // The driver drops its map when our handle closes; VF_MAP gone with seats sent
        // means a reopen happened underneath: send again.
        if let Some((prev, g)) = last.as_ref()
            && prev.as_slice() == seats
            && (seats.is_empty() || v.flags & VF_MAP != 0)
        {
            return enforced(*g);
        }
        let generation = last.as_ref().map(|(_, g)| g.wrapping_add(1).max(1)).unwrap_or(1);
        let buf = match encode_map(generation, seats) {
            Ok(b) => b,
            Err(_) => {
                *last = None;
                return env("map_invalid");
            }
        };
        match ctl.set_map(&buf) {
            Ok(()) => {
                *last = Some((seats.to_vec(), generation));
                enforced(generation)
            }
            Err(_) => {
                *last = None;
                env("map_refused")
            }
        }
    }
}

#[cfg(windows)]
pub mod win {
    //! `\\.\PulsarSeatWfp` through `DeviceIoControl`. The handle stays open while the
    //! service runs (the driver drops the map when it closes: fail open on a crash).
    use super::*;
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, GENERIC_READ,
        GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING, QueryDosDeviceW};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The drive's NT device (`C:` -> `\Device\HarddiskVolume3`).
    pub fn dos_device(drive: &str) -> Option<String> {
        let name = wide(drive);
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated name, buffer length in characters.
        let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(n as usize);
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.is_empty()).then_some(s)
    }

    #[derive(Default)]
    pub struct WinWfp {
        h: Mutex<Option<usize>>,
    }

    impl WinWfp {
        pub fn new() -> Self {
            Self::default()
        }

        /// Is a handle open (the probe then asks through it)?
        pub fn is_open(&self) -> bool {
            self.h.lock().unwrap_or_else(|e| e.into_inner()).is_some()
        }

        fn ioctl(&self, code: u32, input: &[u8], out: &mut [u8]) -> Result<usize, WfpError> {
            let mut g = self.h.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_none() {
                let path = wide(seat_core::consts::WFP_CONTROL_DEVICE);
                // SAFETY: NUL-terminated path; no security attributes, no template.
                let h = unsafe {
                    CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, ptr::null(), OPEN_EXISTING, 0, ptr::null_mut())
                };
                if h == INVALID_HANDLE_VALUE {
                    // SAFETY: no arguments.
                    let e = unsafe { GetLastError() };
                    return Err(match e {
                        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => WfpError::NotLoaded,
                        ERROR_SHARING_VIOLATION => WfpError::Busy,
                        ERROR_ACCESS_DENIED => WfpError::Os("access denied (not the service SID)".into()),
                        e => WfpError::Os(format!("open failed ({e})")),
                    });
                }
                *g = Some(h as usize);
            }
            let h = g.unwrap_or_default() as HANDLE;
            let mut got = 0u32;
            // SAFETY: valid handle; buffers live for the synchronous call.
            let ok = unsafe {
                DeviceIoControl(
                    h,
                    code,
                    if input.is_empty() { ptr::null() } else { input.as_ptr().cast() },
                    input.len() as u32,
                    if out.is_empty() { ptr::null_mut() } else { out.as_mut_ptr().cast() },
                    out.len() as u32,
                    &mut got,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: no arguments.
                let e = unsafe { GetLastError() };
                // A failed call may mean the driver went away: reopen next time.
                // SAFETY: our handle.
                unsafe { CloseHandle(h) };
                *g = None;
                return Err(WfpError::Os(format!("ioctl {code:#x} failed ({e})")));
            }
            Ok(got as usize)
        }
    }

    impl Drop for WinWfp {
        fn drop(&mut self) {
            if let Some(h) = self.h.lock().unwrap_or_else(|e| e.into_inner()).take() {
                // SAFETY: our handle.
                unsafe { CloseHandle(h as HANDLE) };
            }
        }
    }

    impl WfpControl for WinWfp {
        fn version(&self) -> Result<DriverVersion, WfpError> {
            let mut b = [0u8; VERSION_SIZE];
            let n = self.ioctl(IOCTL_GET_VERSION, &[], &mut b)?;
            parse_version(&b[..n]).ok_or_else(|| WfpError::Os("short version reply".into()))
        }
        fn set_map(&self, buf: &[u8]) -> Result<(), WfpError> {
            self.ioctl(IOCTL_SET_MAP, buf, &mut []).map(|_| ())
        }
        fn clear(&self) -> Result<(), WfpError> {
            self.ioctl(IOCTL_CLEAR, &[], &mut []).map(|_| ())
        }
        fn stats(&self) -> Result<DriverStats, WfpError> {
            let mut b = [0u8; STATS_SIZE];
            let n = self.ioctl(IOCTL_GET_STATS, &[], &mut b)?;
            parse_stats(&b[..n]).ok_or_else(|| WfpError::Os("short stats reply".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srcip::AppEntry;
    use seat_core::SeatNumber;

    const HEADER: &str = include_str!("../../../drivers/seat-wfp/include/seatwfp_ioctl.h");

    fn define(name: &str) -> String {
        let line = HEADER
            .lines()
            .find(|l| l.trim_start().starts_with("#define") && l.split_whitespace().nth(1) == Some(name))
            .unwrap_or_else(|| panic!("{name} missing in the header"));
        line.split_whitespace().skip(2).collect::<Vec<_>>().join(" ")
    }

    fn num(name: &str) -> u64 {
        let v = define(name);
        let v = v.split("/*").next().unwrap().trim().trim_matches(['(', ')']);
        if let Some((a, b)) = v.split_once('*') {
            return a.trim().parse::<u64>().unwrap() * b.trim().parse::<u64>().unwrap();
        }
        match v.strip_prefix("0x") {
            Some(h) => u64::from_str_radix(h, 16).unwrap(),
            None => v.parse().unwrap(),
        }
    }

    #[test]
    fn header_matches() {
        assert_eq!(num("SEATWFP_INTERFACE_MAJOR"), u64::from(INTERFACE_MAJOR));
        assert_eq!(num("SEATWFP_INTERFACE_MINOR"), u64::from(INTERFACE_MINOR));
        assert_eq!(num("SEATWFP_MAX_SEATS"), MAX_SEATS as u64);
        assert_eq!(num("SEATWFP_MAX_PATHS"), MAX_PATHS as u64);
        assert_eq!(num("SEATWFP_MAX_PATH_CHARS"), MAX_PATH_CHARS as u64);
        assert_eq!(num("SEATWFP_MAX_MAP_BYTES"), MAX_MAP_BYTES as u64);
        assert_eq!(num("SEATWFP_RULE_ALL"), u64::from(RULE_ALL));
        assert_eq!(num("SEATWFP_RULE_ONLY_LISTED"), u64::from(RULE_ONLY_LISTED));
        assert_eq!(num("SEATWFP_RULE_ALL_EXCEPT_LISTED"), u64::from(RULE_ALL_EXCEPT_LISTED));
        assert_eq!(num("SEATWFP_SF_BIND"), u64::from(SF_BIND));
        assert_eq!(num("SEATWFP_SF_CONNECT"), u64::from(SF_CONNECT));
        assert_eq!(num("SEATWFP_VF_PASS_THROUGH"), u64::from(VF_PASS_THROUGH));
        assert_eq!(num("SEATWFP_VF_CALLOUTS"), u64::from(VF_CALLOUTS));
        assert_eq!(num("SEATWFP_VF_FILTERS"), u64::from(VF_FILTERS));
        assert_eq!(num("SEATWFP_VF_MAP"), u64::from(VF_MAP));
        assert_eq!(define("SEATWFP_SERVICE_SID"), format!("L\"{SERVICE_SID}\""));
        assert_eq!(define("SEATWFP_DEVICE_SDDL"), format!("L\"D:P(A;;GA;;;{SERVICE_SID})\""));
        assert!(define("SEATWFP_SYMLINK_NAME").contains(r"PulsarSeatWfp"));
        assert!(seat_core::consts::WFP_CONTROL_DEVICE.ends_with(r"\PulsarSeatWfp"));
        for (name, f, a) in [
            ("IOCTL_SEATWFP_GET_VERSION", "0x800", "FILE_ANY_ACCESS"),
            ("IOCTL_SEATWFP_SET_MAP", "0x801", "SEATWFP_RW_ACCESS"),
            ("IOCTL_SEATWFP_CLEAR", "0x802", "SEATWFP_RW_ACCESS"),
            ("IOCTL_SEATWFP_GET_STATS", "0x803", "SEATWFP_RW_ACCESS"),
        ] {
            assert_eq!(define(name), format!("CTL_CODE(FILE_DEVICE_UNKNOWN, {f}, METHOD_BUFFERED, {a})"));
        }
        assert_eq!(IOCTL_GET_VERSION, 0x0022_2000);
        assert_eq!(IOCTL_SET_MAP, 0x0022_E004);
        assert_eq!(IOCTL_CLEAR, 0x0022_E008);
        assert_eq!(IOCTL_GET_STATS, 0x0022_E00C);
        // Struct sizes as asserted in the header.
        for (s, n) in [("SEATWFP_VERSION", VERSION_SIZE), ("SEATWFP_MAP_HEADER", HEADER_SIZE), ("SEATWFP_SEAT", SEAT_SIZE), ("SEATWFP_PATH", PATH_SIZE), ("SEATWFP_STATS", STATS_SIZE)] {
            assert!(HEADER.contains(&format!("C_ASSERT(sizeof({s}) == {n});")), "{s}");
        }
    }

    /// The SID in the DACL is `NT SERVICE\<SERVICE_NAME>`: S-1-5-80 + SHA-1 of the
    /// upper-case name in UTF-16 LE, as five little-endian sub-authorities.
    #[test]
    fn service_sid_is_derived_from_the_service_name() {
        use sha1::{Digest, Sha1};
        let name: Vec<u8> = seat_core::consts::SERVICE_NAME.to_uppercase().encode_utf16().flat_map(u16::to_le_bytes).collect();
        let h = Sha1::digest(&name);
        let subs: Vec<String> = h.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]).to_string()).collect();
        assert_eq!(format!("S-1-5-80-{}", subs.join("-")), SERVICE_SID);
    }

    fn seat(n: u32, session: u32, a: [u8; 4], rule: AppRule, paths: &[&str]) -> WfpSeat {
        WfpSeat { seat: n, session_id: session, address: Ipv4Addr::from(a), rule, flags: SF_BIND | SF_CONNECT, paths: paths.iter().map(|p| p.to_string()).collect() }
    }

    #[test]
    fn map_round_trip_and_layout() {
        let seats = vec![
            seat(2, 3, [192, 168, 1, 52], AppRule::OnlyListed, &[r"\device\harddiskvolume3\games\lan", r"\device\mup\srv\share\x.exe"]),
            seat(3, 4, [192, 168, 1, 53], AppRule::All, &[]),
        ];
        let b = encode_map(7, &seats).unwrap();
        assert_eq!(u32_at(&b, 0) as usize, b.len());
        assert_eq!(u16_at(&b, 4), 1);
        assert_eq!(u32_at(&b, 8), 7);
        assert_eq!((u32_at(&b, 12), u32_at(&b, 16)), (2, 2));
        // seat 0: session, octets in dotted order, rule, flags, first, count, seat number
        let o = HEADER_SIZE;
        assert_eq!(u32_at(&b, o), 3);
        assert_eq!(&b[o + 4..o + 8], &[192, 168, 1, 52]);
        assert_eq!((u16_at(&b, o + 8), u16_at(&b, o + 10)), (RULE_ONLY_LISTED, 3));
        assert_eq!((u32_at(&b, o + 12), u32_at(&b, o + 16), u32_at(&b, o + 20)), (0, 2, 2));
        assert_eq!((u32_at(&b, o + SEAT_SIZE + 12), u32_at(&b, o + SEAT_SIZE + 16)), (2, 0));
        let p0 = HEADER_SIZE + 2 * SEAT_SIZE;
        assert_eq!(u32_at(&b, p0) as usize, p0 + 2 * PATH_SIZE);
        assert_eq!(decode_map(&b).unwrap(), (7, seats));
        assert_eq!(decode_map(&encode_map(1, &[]).unwrap()).unwrap(), (1, vec![]));
    }

    #[test]
    fn map_rules_match_the_driver() {
        let ok = seat(2, 3, [10, 0, 0, 2], AppRule::All, &[]);
        assert_eq!(encode_map(1, &[seat(2, 3, [127, 0, 0, 2], AppRule::All, &[])]), Err(MapError::BadAddress(Ipv4Addr::new(127, 0, 0, 2))));
        assert!(encode_map(1, &[seat(2, 3, [224, 0, 0, 1], AppRule::All, &[])]).is_err());
        assert!(encode_map(1, &[seat(2, 3, [0, 0, 0, 0], AppRule::All, &[])]).is_err());
        assert_eq!(encode_map(1, &[ok.clone(), WfpSeat { seat: 3, ..ok.clone() }]), Err(MapError::DuplicateSession(3)));
        assert_eq!(encode_map(1, &[seat(2, 3, [10, 0, 0, 2], AppRule::OnlyListed, &[""])]), Err(MapError::BadPath));
        let many: Vec<WfpSeat> = (0..65).map(|i| WfpSeat { session_id: i, ..ok.clone() }).collect();
        assert_eq!(encode_map(1, &many), Err(MapError::TooManySeats));
        // Tampered buffers are refused like the driver does.
        let mut b = encode_map(1, &[seat(2, 3, [10, 0, 0, 2], AppRule::OnlyListed, &[r"\device\x"])]).unwrap();
        let mut bad = b.clone();
        bad[0] ^= 1;
        assert!(decode_map(&bad).is_err());
        let mut bad = b.clone();
        bad[HEADER_SIZE + 8] = 9; // rule
        assert!(decode_map(&bad).is_err());
        let p0 = HEADER_SIZE + SEAT_SIZE;
        b[p0] = 1; // odd offset inside the tables
        assert_eq!(decode_map(&b), Err(MapError::BadPath));
    }

    #[test]
    fn lookup_follows_the_seat_rule() {
        let only = seat(2, 3, [10, 0, 0, 2], AppRule::OnlyListed, &[r"\device\harddiskvolume3\games\lan"]);
        let except = seat(3, 4, [10, 0, 0, 3], AppRule::AllExceptListed, &[r"\device\harddiskvolume3\games\lan"]);
        let all = seat(4, 5, [10, 0, 0, 4], AppRule::All, &[]);
        let m = vec![only, except, all];
        let game = Some(r"\device\harddiskvolume3\games\lan\bin\game.exe");
        let other = Some(r"\device\harddiskvolume3\games\lan2\x.exe");
        assert_eq!(lookup(&m, 3, game, SF_BIND), Some(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(lookup(&m, 3, Some(r"\DEVICE\HarddiskVolume3\Games\LAN"), SF_CONNECT), Some(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(lookup(&m, 3, other, SF_BIND), None);
        assert_eq!(lookup(&m, 3, None, SF_BIND), None);
        assert_eq!(lookup(&m, 4, game, SF_BIND), None);
        assert_eq!(lookup(&m, 4, other, SF_BIND), Some(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(lookup(&m, 4, None, SF_BIND), Some(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(lookup(&m, 5, Some("\\device\\x.exe\0"), SF_CONNECT), Some(Ipv4Addr::new(10, 0, 0, 4)));
        assert_eq!(lookup(&m, 9, game, SF_BIND), None);
        let bind_only = vec![WfpSeat { flags: SF_BIND, ..m[2].clone() }];
        assert_eq!(lookup(&bind_only, 5, None, SF_CONNECT), None);
    }

    #[test]
    fn dos_paths_become_nt_paths() {
        let dev = |d: &str| (d.eq_ignore_ascii_case("C:")).then(|| r"\Device\HarddiskVolume3".to_string());
        assert_eq!(dos_to_nt(r"C:\Games\Lan\", dev).as_deref(), Some(r"\device\harddiskvolume3\games\lan"));
        assert_eq!(dos_to_nt(r#""c:/Games/x.exe""#, dev).as_deref(), Some(r"\device\harddiskvolume3\games\x.exe"));
        assert_eq!(dos_to_nt(r"\\?\C:\Tools", dev).as_deref(), Some(r"\device\harddiskvolume3\tools"));
        assert_eq!(dos_to_nt(r"\\srv\share\app.exe", dev).as_deref(), Some(r"\device\mup\srv\share\app.exe"));
        assert_eq!(dos_to_nt(r"\\?\UNC\srv\share", dev).as_deref(), Some(r"\device\mup\srv\share"));
        assert_eq!(dos_to_nt(r"D:\x", dev), None);
        assert_eq!(dos_to_nt(r"games\x.exe", dev), None);
        assert_eq!(dos_to_nt(r"\\", dev), None);
        assert_eq!(dos_to_nt("", dev), None);
    }

    fn sa(n: u8, a: Option<[u8; 4]>, rule: AppRule, apps: &[(&str, bool)]) -> SeatAddress {
        SeatAddress {
            seat: SeatNumber::new(n).unwrap(),
            address: a.map(Ipv4Addr::from),
            rule,
            apps: apps.iter().map(|(p, e)| AppEntry { path: p.to_string(), enabled: *e }).collect(),
        }
    }

    #[test]
    fn build_drops_what_the_driver_cannot_use() {
        let dev = |d: &str| (d == "C:").then(|| r"\Device\HarddiskVolume3".to_string());
        let seats = vec![
            SessionSeat { session_id: 3, address: sa(2, Some([192, 168, 1, 52]), AppRule::OnlyListed, &[(r"C:\Games\Lan", true), (r"C:\Off.exe", false), (r"Z:\x.exe", true), (r"C:\Games\Lan\", true)]) },
            SessionSeat { session_id: 4, address: sa(3, None, AppRule::All, &[]) },
            SessionSeat { session_id: 5, address: sa(4, Some([127, 0, 0, 4]), AppRule::All, &[]) },
            SessionSeat { session_id: 3, address: sa(5, Some([192, 168, 1, 55]), AppRule::All, &[]) },
            SessionSeat { session_id: 6, address: sa(6, Some([192, 168, 1, 56]), AppRule::All, &[(r"C:\ignored", true)]) },
        ];
        let (m, dropped) = build_seats(&seats, dev);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].seat, m[0].session_id, m[0].paths.clone()), (2, 3, vec![r"\device\harddiskvolume3\games\lan".to_string()]));
        assert_eq!(m[0].flags, SF_BIND | SF_CONNECT);
        assert_eq!((m[1].seat, m[1].paths.len()), (6, 0));
        assert_eq!(dropped, vec![r"Z:\x.exe".to_string()]);
        assert!(encode_map(1, &m).is_ok());
    }

    #[test]
    fn publisher_sends_on_change_and_falls_back() {
        let fake = FakeWfp::default();
        let p = Publisher::new();
        let m = vec![seat(2, 3, [10, 0, 0, 2], AppRule::All, &[])];
        assert_eq!(p.publish(&fake, &m), Enforcement::EnvironmentOnly { reason: "driver_not_loaded".into() });
        fake.state.lock().unwrap().loaded = true;
        fake.state.lock().unwrap().major = 1;
        assert_eq!(p.publish(&fake, &m), Enforcement::Driver { generation: 1, seats: vec![2] });
        assert_eq!(p.publish(&fake, &m), Enforcement::Driver { generation: 1, seats: vec![2] });
        assert_eq!(fake.set_calls(), 1);
        assert_eq!(fake.seats(), m);
        // The driver lost the map (handle reopened): sent again.
        fake.clear().unwrap();
        assert_eq!(p.publish(&fake, &m), Enforcement::Driver { generation: 2, seats: vec![2] });
        assert_eq!(fake.set_calls(), 2);
        // Changed map: new generation; empty map clears.
        assert_eq!(p.publish(&fake, &[]), Enforcement::Driver { generation: 3, seats: vec![] });
        assert!(fake.seats().is_empty());
        fake.state.lock().unwrap().major = 2;
        assert_eq!(p.publish(&fake, &m), Enforcement::EnvironmentOnly { reason: "driver_version_mismatch".into() });
        fake.state.lock().unwrap().major = 1;
        fake.state.lock().unwrap().pass_through = true;
        assert_eq!(p.publish(&fake, &m), Enforcement::EnvironmentOnly { reason: "driver_pass_through".into() });
    }

    #[test]
    fn replies_parse() {
        let mut v = [0u8; VERSION_SIZE];
        v[0] = 1;
        v[4] = 7;
        v[8] = (VF_CALLOUTS | VF_FILTERS) as u8;
        assert_eq!(parse_version(&v), Some(DriverVersion { major: 1, minor: 0, build: 7, flags: 6 }));
        assert_eq!(parse_version(&v[..8]), None);
        let mut s = [0u8; STATS_SIZE];
        s[0] = 5;
        s[8] = 6;
        s[16] = 1;
        s[24] = 3;
        s[28] = 2;
        s[32] = 8;
        assert_eq!(parse_stats(&s), Some(DriverStats { bind_redirects: 5, connect_redirects: 6, skipped: 1, generation: 3, seat_count: 2, flags: 8 }));
    }
}
