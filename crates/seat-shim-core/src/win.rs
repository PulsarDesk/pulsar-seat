// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the crate's traits. Nothing here runs in unit tests
//! (they use [`crate::fake`]); see README "untested".

use crate::dxvk::Fetcher;
use crate::error::ShimError;
use crate::install::{ShimSystem, SystemDirs, VersionInfo};
use crate::regval::{RegValue, multi_sz_to_utf16, utf16_to_multi_sz};
use crate::sdb::{SdbNode, SdbWriter, WriteOp, write_ops};
use crate::seatmap::{SeatMapEntry, SeatMapReader, SeatMapWriter, decode_entry, encode_entry};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_OPTION_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegSetValueExW,
};
use winreg::RegKey;
use winreg::enums::{KEY_READ, KEY_WOW64_64KEY, KEY_WRITE, RegType};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn reg_err(ctx: &str, e: impl std::fmt::Display) -> ShimError {
    ShimError::Registry(format!("{ctx}: {e}"))
}

/// winreg value -> ours (DWORD, SZ, EXPAND_SZ, MULTI_SZ; others are skipped).
pub fn from_winreg(v: &winreg::RegValue) -> Option<RegValue> {
    let words = || -> Vec<u16> { v.bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() };
    let sz = || {
        let w = words();
        let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    };
    match v.vtype {
        RegType::REG_DWORD if v.bytes.len() >= 4 => {
            Some(RegValue::Dword(u32::from_le_bytes([v.bytes[0], v.bytes[1], v.bytes[2], v.bytes[3]])))
        }
        RegType::REG_SZ => Some(RegValue::Sz(sz())),
        RegType::REG_EXPAND_SZ => Some(RegValue::ExpandSz(sz())),
        RegType::REG_MULTI_SZ => Some(RegValue::MultiSz(utf16_to_multi_sz(&words()))),
        _ => None,
    }
}

pub fn to_winreg(v: &RegValue) -> winreg::RegValue<'static> {
    let utf16 = |w: Vec<u16>| -> Vec<u8> { w.iter().flat_map(|c| c.to_le_bytes()).collect() };
    match v {
        RegValue::Dword(d) => winreg::RegValue { bytes: d.to_le_bytes().to_vec().into(), vtype: RegType::REG_DWORD },
        RegValue::Sz(s) => winreg::RegValue { bytes: utf16(wide(s)).into(), vtype: RegType::REG_SZ },
        RegValue::ExpandSz(s) => winreg::RegValue { bytes: utf16(wide(s)).into(), vtype: RegType::REG_EXPAND_SZ },
        RegValue::MultiSz(m) => {
            winreg::RegValue { bytes: utf16(multi_sz_to_utf16(m)).into(), vtype: RegType::REG_MULTI_SZ }
        }
    }
}

// ---------------------------------------------------------------- HKLM, 64-bit view
//
// Every HKLM access goes through the helpers below, which always add `KEY_WOW64_64KEY`:
// the x86 module, `pulsar-split-launch32` and a 32-bit `shimctl` must see the same
// `SOFTWARE\PulsarSplit` keys as the 64-bit service, not the `WOW6432Node` copy. The test
// `every_hklm_access_uses_the_helpers` keeps it that way.

/// Access mask for an HKLM open or create: always the 64-bit registry view.
pub const fn hklm_access(access: u32) -> u32 {
    access | KEY_WOW64_64KEY
}

fn hklm_open(path: &str, access: u32) -> std::io::Result<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(path, hklm_access(access))
}

fn hklm_create(path: &str, access: u32) -> std::io::Result<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey_with_flags(path, hklm_access(access)).map(|(k, _)| k)
}

/// Deletes `path` and everything below it, in the 64-bit view (through the parent key,
/// opened with the view flag). A missing key is `NotFound`.
fn hklm_delete_tree(path: &str) -> std::io::Result<()> {
    use winreg::enums::{DELETE, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE};
    let Some((parent, leaf)) = path.rsplit_once('\\').filter(|(p, l)| !p.is_empty() && !l.is_empty()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "refusing to delete a top-level HKLM key"));
    };
    let access = DELETE | KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE | KEY_SET_VALUE;
    hklm_open(parent, access)?.delete_subkey_all(leaf)
}

pub fn read_values(path: &str) -> Result<Vec<(String, RegValue)>, ShimError> {
    let k = hklm_open(path, KEY_READ).map_err(|e| reg_err(path, e))?;
    Ok(k.enum_values().filter_map(Result::ok).filter_map(|(n, v)| from_winreg(&v).map(|v| (n, v))).collect())
}

pub fn read_value(path: &str, name: &str) -> Option<RegValue> {
    let k = hklm_open(path, KEY_READ).ok()?;
    k.get_raw_value(name).ok().and_then(|v| from_winreg(&v))
}

// ---------------------------------------------------------------- seat map

/// SYSTEM full; Everyone, all application packages: read. Inherited by each seat subkey.
pub const SEATMAP_SDDL: &str = "D:P(A;OICI;KA;;;SY)(A;OICI;KR;;;WD)(A;OICI;KR;;;AC)";

struct Sd(PSECURITY_DESCRIPTOR);
impl Sd {
    fn new(sddl: &str) -> Result<Self, ShimError> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated string; out pointer valid.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(ShimError::System(format!("SDDL: {}", std::io::Error::last_os_error())));
        }
        Ok(Self(sd))
    }
}
impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor… with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

/// Creates (or opens) a volatile HKLM key with the given security descriptor.
fn create_volatile(path: &str, sd: &Sd) -> Result<HKEY, ShimError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let w = wide(path);
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: all pointers valid for the call.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_VOLATILE,
            hklm_access(KEY_ALL_ACCESS),
            &sa,
            &mut h,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(reg_err(path, std::io::Error::from_raw_os_error(rc as i32)));
    }
    Ok(h)
}

fn set_value(h: HKEY, name: &str, v: &RegValue) -> Result<(), ShimError> {
    let (ty, data): (u32, Vec<u8>) = match v {
        RegValue::Dword(d) => (REG_DWORD, d.to_le_bytes().to_vec()),
        RegValue::Sz(s) => (REG_SZ, wide(s).iter().flat_map(|c| c.to_le_bytes()).collect()),
        RegValue::ExpandSz(s) => (REG_EXPAND_SZ, wide(s).iter().flat_map(|c| c.to_le_bytes()).collect()),
        RegValue::MultiSz(m) => (REG_MULTI_SZ, multi_sz_to_utf16(m).iter().flat_map(|c| c.to_le_bytes()).collect()),
    };
    let n = wide(name);
    // SAFETY: valid key handle and buffers.
    let rc = unsafe { RegSetValueExW(h, n.as_ptr(), 0, ty, data.as_ptr(), data.len() as u32) };
    if rc != 0 {
        return Err(reg_err(name, std::io::Error::from_raw_os_error(rc as i32)));
    }
    Ok(())
}

/// Service writes, module reads (`HKLM\SOFTWARE\PulsarSplit\Runtime\Seats`).
pub struct RegSeatMap;

impl SeatMapWriter for RegSeatMap {
    fn publish(&self, e: &SeatMapEntry) -> Result<(), ShimError> {
        if e.session_id == 0 {
            return Err(ShimError::InvalidValue("session 0 is never a seat".into()));
        }
        let sd = Sd::new(SEATMAP_SDDL)?;
        let parent = crate::REG_SEATMAP.rsplit_once('\\').map(|(p, _)| p).unwrap_or(crate::REG_SEATMAP);
        for p in [parent, crate::REG_SEATMAP] {
            let h = create_volatile(p, &sd)?;
            // SAFETY: handle from RegCreateKeyExW.
            unsafe { RegCloseKey(h) };
        }
        let path = format!(r"{}\{}", crate::REG_SEATMAP, e.session_id);
        // Replace: no stale values from an earlier seat in this session.
        let _ = hklm_delete_tree(&path);
        let h = create_volatile(&path, &sd)?;
        let res = encode_entry(e).iter().try_for_each(|(n, v)| set_value(h, n, v));
        // SAFETY: handle from RegCreateKeyExW.
        unsafe { RegCloseKey(h) };
        res
    }

    fn remove(&self, session_id: u32) -> Result<(), ShimError> {
        match hklm_delete_tree(&format!(r"{}\{}", crate::REG_SEATMAP, session_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(reg_err("seat map remove", e)),
            _ => Ok(()),
        }
    }

    fn clear(&self) -> Result<(), ShimError> {
        match hklm_delete_tree(crate::REG_SEATMAP) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(reg_err("seat map clear", e)),
            _ => Ok(()),
        }
    }
}

impl SeatMapReader for RegSeatMap {
    fn read_all(&self) -> Result<Vec<SeatMapEntry>, ShimError> {
        let root = match hklm_open(crate::REG_SEATMAP, KEY_READ) {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(reg_err("seat map", e)),
        };
        let mut out = Vec::new();
        for name in root.enum_keys().filter_map(Result::ok) {
            let Ok(vals) = read_values(&format!(r"{}\{}", crate::REG_SEATMAP, name)) else { continue };
            match decode_entry(&vals) {
                Ok(e) if e.session_id.to_string() == name => out.push(e),
                Ok(_) => log::warn!("seat map entry {name}: SessionId differs from key name; skipped"),
                Err(err) => log::warn!("seat map entry {name}: {err}"),
            }
        }
        Ok(out)
    }
}

/// Service writes, module reads (`HKLM\SOFTWARE\PulsarSplit\Runtime\KioskSeats`).
pub struct RegKioskMap;

impl crate::kiosk::KioskMapWriter for RegKioskMap {
    fn publish(&self, e: &crate::kiosk::KioskSeatEntry) -> Result<(), ShimError> {
        if e.session_id == 0 || e.seat == 0 || e.seat > crate::kiosk::MAX_KIOSK_SEAT {
            return Err(ShimError::InvalidValue("kiosk map: bad seat or session".into()));
        }
        let sd = Sd::new(SEATMAP_SDDL)?;
        let parent = crate::REG_KIOSKMAP.rsplit_once('\\').map(|(p, _)| p).unwrap_or(crate::REG_KIOSKMAP);
        for p in [parent, crate::REG_KIOSKMAP] {
            let h = create_volatile(p, &sd)?;
            // SAFETY: handle from RegCreateKeyExW.
            unsafe { RegCloseKey(h) };
        }
        let path = format!(r"{}\{}", crate::REG_KIOSKMAP, crate::kiosk::key_name(e.seat));
        let _ = hklm_delete_tree(&path);
        let h = create_volatile(&path, &sd)?;
        let res = crate::kiosk::encode_kiosk_entry(e).iter().try_for_each(|(n, v)| set_value(h, n, v));
        // SAFETY: handle from RegCreateKeyExW.
        unsafe { RegCloseKey(h) };
        res
    }

    fn remove(&self, seat: u32) -> Result<(), ShimError> {
        match hklm_delete_tree(&format!(r"{}\{}", crate::REG_KIOSKMAP, crate::kiosk::key_name(seat))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(reg_err("kiosk map remove", e)),
            _ => Ok(()),
        }
    }

    fn clear(&self) -> Result<(), ShimError> {
        match hklm_delete_tree(crate::REG_KIOSKMAP) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(reg_err("kiosk map clear", e)),
            _ => Ok(()),
        }
    }
}

impl crate::kiosk::KioskMapReader for RegKioskMap {
    fn read_all(&self) -> Result<Vec<crate::kiosk::KioskSeatEntry>, ShimError> {
        let root = match hklm_open(crate::REG_KIOSKMAP, KEY_READ) {
            Ok(k) => k,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(reg_err("kiosk map", e)),
        };
        let mut out = Vec::new();
        for name in root.enum_keys().filter_map(Result::ok) {
            let Ok(vals) = read_values(&format!(r"{}\{}", crate::REG_KIOSKMAP, name)) else { continue };
            match crate::kiosk::decode_kiosk_entry(&vals) {
                Ok(e) if crate::kiosk::key_name(e.seat) == name => out.push(e),
                Ok(_) => log::warn!("kiosk map entry {name}: Seat differs from key name; skipped"),
                Err(err) => log::warn!("kiosk map entry {name}: {err}"),
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------- process facts

/// Session of the current process.
pub fn current_session_id() -> u32 {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut s = 0u32;
    // SAFETY: plain out parameter.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut s) } == 0 { 0 } else { s }
}

pub fn active_console_session() -> u32 {
    // SAFETY: no arguments.
    unsafe { windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId() }
}

pub fn safe_mode() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CLEANBOOT};
    // SAFETY: no pointers.
    unsafe { GetSystemMetrics(SM_CLEANBOOT) != 0 }
}

pub fn current_exe_path() -> String {
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: buffer valid for its length.
        let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n < buf.len() || buf.len() >= 32768 {
            return String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        }
        buf.resize(buf.len() * 2, 0);
    }
}

/// `%ProgramFiles%\Pulsar Split\` (both views), lowercase, trailing backslash.
pub fn install_roots() -> Vec<String> {
    ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(|p| format!(r"{}\{}\", p.trim_end_matches('\\'), crate::INSTALL_DIR_NAME).to_ascii_lowercase())
        .collect()
}

pub fn windows_build() -> u32 {
    read_value(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

pub fn kill_switch() -> bool {
    read_value(crate::REG_SHIM, crate::values::SHIM_DISABLED).and_then(|v| v.as_dword()) == Some(1)
}

pub fn read_shim_config() -> crate::decide::ShimConfig {
    let multi = |n: &str| -> Vec<String> {
        read_value(crate::REG_SHIM, n).and_then(|v| v.as_multi().map(<[String]>::to_vec)).unwrap_or_default()
    };
    crate::decide::ShimConfig {
        // Absent verbosity = errors only (spec 50 §3.3).
        verbosity: read_value(crate::REG_SHIM, crate::values::VERBOSITY).and_then(|v| v.as_dword()).unwrap_or(1),
        mask_apps: multi(crate::values::MASK_APPS).into_iter().map(|s| s.to_ascii_lowercase()).collect(),
        disabled_targets: multi(crate::values::DISABLED_TARGETS),
        wnf_builds: multi(crate::values::WNF_BUILDS).iter().filter_map(|s| s.trim().parse().ok()).collect(),
    }
}

/// All facts for [`crate::decide::role`], read before any hook is installed.
pub fn process_facts() -> crate::decide::ProcessFacts {
    let exe = current_exe_path().to_ascii_lowercase();
    let ours = install_roots().iter().any(|r| exe.starts_with(r));
    crate::decide::ProcessFacts {
        session_id: current_session_id(),
        console_session: active_console_session(),
        safe_mode: safe_mode(),
        kill_switch: kill_switch(),
        exe_lower: exe,
        is_our_binary: ours,
        kiosk_seat: crate::kiosk::parse_kiosk_env(std::env::var(crate::kiosk::KIOSK_SEAT_ENV).ok().as_deref()),
    }
}

// ---------------------------------------------------------------- installer system

fn sys_dir(f: unsafe extern "system" fn(*mut u16, u32) -> u32) -> Option<PathBuf> {
    let mut buf = vec![0u16; 520];
    // SAFETY: buffer valid for its length.
    let n = unsafe { f(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// Version strings via `GetFileVersionInfoW` / `VerQueryValueW` (first translation).
pub fn file_version_info(p: &Path) -> Option<VersionInfo> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let w = wide_path(p);
    let mut handle = 0u32;
    // SAFETY: valid path; out parameter.
    let size = unsafe { GetFileVersionInfoSizeW(w.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: buffer of `size` bytes.
    if unsafe { GetFileVersionInfoW(w.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let query = |sub: &str| -> Option<(*const u8, u32)> {
        let q = wide(sub);
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `data` holds the version block; out pointers valid.
        let ok = unsafe { VerQueryValueW(data.as_ptr().cast(), q.as_ptr(), &mut ptr, &mut len) };
        (ok != 0 && !ptr.is_null() && len > 0).then_some((ptr as *const u8, len))
    };
    let (tp, tl) = query(r"\VarFileInfo\Translation")?;
    if tl < 4 {
        return None;
    }
    // SAFETY: at least 4 bytes inside `data`.
    let (lang, cp) = unsafe { (std::ptr::read_unaligned(tp as *const u16), std::ptr::read_unaligned(tp.add(2) as *const u16)) };
    let string = |name: &str| -> Option<String> {
        let (p, len) = query(&format!(r"\StringFileInfo\{lang:04x}{cp:04x}\{name}"))?;
        // SAFETY: `len` UTF-16 units inside `data`.
        let s = unsafe { std::slice::from_raw_parts(p as *const u16, len as usize) };
        let end = s.iter().position(|c| *c == 0).unwrap_or(s.len());
        let v = String::from_utf16_lossy(&s[..end]).trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    Some(VersionInfo {
        product_name: string("ProductName"),
        file_description: string("FileDescription"),
        file_version: string("FileVersion"),
    })
}

/// Random bytes good enough for EXE ids (uniqueness, not secrecy).
fn random16() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for half in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out[7] = (out[7] & 0x0F) | 0x40; // version 4 (Data3 high nibble, little-endian layout)
    out[8] = (out[8] & 0x3F) | 0x80;
    out
}

pub struct WinShimSystem;

impl ShimSystem for WinShimSystem {
    fn reg_read(&self, path: &str, name: &str) -> Result<Option<RegValue>, ShimError> {
        match hklm_open(path, KEY_READ) {
            Ok(k) => Ok(k.get_raw_value(name).ok().and_then(|v| from_winreg(&v))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(reg_err(path, e)),
        }
    }
    fn reg_write(&self, path: &str, name: &str, v: &RegValue) -> Result<(), ShimError> {
        let k = hklm_create(path, KEY_WRITE).map_err(|e| reg_err(path, e))?;
        k.set_raw_value(name, &to_winreg(v)).map_err(|e| reg_err(name, e))
    }
    fn reg_key_exists(&self, path: &str) -> bool {
        hklm_open(path, KEY_READ).is_ok()
    }
    fn reg_subkeys(&self, path: &str) -> Result<Vec<String>, ShimError> {
        match hklm_open(path, KEY_READ) {
            Ok(k) => Ok(k.enum_keys().filter_map(Result::ok).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(reg_err(path, e)),
        }
    }
    fn reg_values(&self, path: &str) -> Result<Vec<(String, RegValue)>, ShimError> {
        match read_values(path) {
            Err(ShimError::Registry(_)) if !self.reg_key_exists(path) => Ok(Vec::new()),
            r => r,
        }
    }
    fn reg_delete_tree(&self, path: &str) -> Result<(), ShimError> {
        match hklm_delete_tree(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(reg_err(path, e)),
            _ => Ok(()),
        }
    }
    fn dirs(&self) -> SystemDirs {
        use windows_sys::Win32::System::SystemInformation::{
            GetSystemDirectoryW, GetSystemWow64DirectoryW, GetWindowsDirectoryW,
        };
        let windir = sys_dir(GetWindowsDirectoryW).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        SystemDirs {
            system32: sys_dir(GetSystemDirectoryW).unwrap_or_else(|| windir.join("System32")),
            syswow64: sys_dir(GetSystemWow64DirectoryW),
            data_root: PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into()))
                .join(crate::install::PRODUCT_DIR)
                .join(crate::install::SHIM_DATA_DIR),
            windir,
        }
    }
    fn file_exists(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn file_version(&self, p: &Path) -> Option<VersionInfo> {
        file_version_info(p)
    }
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        std::fs::copy(from, to)?;
        Ok(())
    }
    fn rename_file(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        std::fs::rename(from, to)?;
        Ok(())
    }
    fn delete_file(&self, p: &Path) -> Result<bool, ShimError> {
        match std::fs::remove_file(p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e)
                if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    || e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn delete_on_reboot(&self, p: &Path) -> Result<(), ShimError> {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
        let w = wide_path(p);
        // SAFETY: valid path; NULL target = delete.
        if unsafe { MoveFileExW(w.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) } == 0 {
            return Err(ShimError::System(format!("MoveFileEx: {}", std::io::Error::last_os_error())));
        }
        Ok(())
    }
    fn write_sdb(&self, path: &Path, tree: &SdbNode) -> Result<(), ShimError> {
        // Only into a folder below the data root that SYSTEM / Administrators alone can
        // write: no user can swap the file between this write and `sdbinst`.
        let parent = path.parent().ok_or_else(|| ShimError::InvalidValue("sdb path".into()))?;
        crate::winfs::ensure_private_dir(&self.dirs().data_root, parent)?;
        let tmp = crate::winfs::temp_sibling(path);
        if let Err(e) = AppHelpSdbWriter.write(&tmp, tree) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        crate::winfs::replace(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
    fn run_sdbinst(&self, args: &[String]) -> Result<u32, ShimError> {
        for a in args.iter().filter(|a| a.to_ascii_lowercase().ends_with(".sdb")) {
            let p = Path::new(a);
            if !p.starts_with(self.dirs().data_root) {
                return Err(ShimError::InvalidValue(format!("sdb outside the data root: {a}")));
            }
            crate::winfs::check_file(p)?;
        }
        let exe = self.dirs().system32.join("sdbinst.exe");
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args);
        // In the service: the shared spawn lock (seat_core::spawn). The module build has
        // no password pipes to protect.
        #[cfg(feature = "proto")]
        let mut child = seat_core::spawn::spawn_locked(&mut cmd)?;
        #[cfg(not(feature = "proto"))]
        let mut child = cmd.spawn()?;
        let st = child.wait()?;
        Ok(st.code().unwrap_or(-1) as u32)
    }
    fn new_guid(&self) -> [u8; 16] {
        random16()
    }
    fn unique_suffix(&self) -> String {
        random16()[..4].iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ---------------------------------------------------------------- apphelp writer

type Pdb = *mut c_void;
type FnCreate = unsafe extern "system" fn(*const u16, u32) -> Pdb;
type FnBegin = unsafe extern "system" fn(Pdb, u16) -> u32;
type FnEnd = unsafe extern "system" fn(Pdb, u32) -> i32;
type FnStr = unsafe extern "system" fn(Pdb, u16, *const u16) -> i32;
type FnDword = unsafe extern "system" fn(Pdb, u16, u32) -> i32;
type FnBin = unsafe extern "system" fn(Pdb, u16, *const u8, u32) -> i32;
type FnNull = unsafe extern "system" fn(Pdb, u16) -> i32;
type FnClose = unsafe extern "system" fn(Pdb);

/// Writes the database with apphelp.dll's own writer functions (loaded from System32).
pub struct AppHelpSdbWriter;

impl SdbWriter for AppHelpSdbWriter {
    fn write(&self, path: &Path, tree: &SdbNode) -> Result<(), ShimError> {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
        let name = wide("apphelp.dll");
        // SAFETY: system-directory-only load of a Windows DLL.
        let m = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if m.is_null() {
            return Err(ShimError::System(format!("apphelp.dll: {}", std::io::Error::last_os_error())));
        }
        macro_rules! sym {
            ($n:literal, $t:ty) => {{
                // SAFETY: NUL-terminated name; the transmute target matches the export's ABI.
                match unsafe { GetProcAddress(m, concat!($n, "\0").as_ptr()) } {
                    Some(f) => unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $t>(f) },
                    None => return Err(ShimError::Unsupported(concat!("apphelp lacks ", $n).into())),
                }
            }};
        }
        let create = sym!("SdbCreateDatabase", FnCreate);
        let begin = sym!("SdbBeginWriteListTag", FnBegin);
        let end = sym!("SdbEndWriteListTag", FnEnd);
        let wstr = sym!("SdbWriteStringTag", FnStr);
        let wdword = sym!("SdbWriteDWORDTag", FnDword);
        let wbin = sym!("SdbWriteBinaryTag", FnBin);
        let wnull = sym!("SdbWriteNULLTag", FnNull);
        let close = sym!("SdbCloseDatabaseWrite", FnClose);

        let mut ops = Vec::new();
        write_ops(tree, &mut ops);
        let p = wide_path(path);
        // SAFETY: DOS_PATH = 0.
        let db = unsafe { create(p.as_ptr(), 0) };
        if db.is_null() {
            return Err(ShimError::System("SdbCreateDatabase failed".into()));
        }
        let mut stack: Vec<u32> = Vec::new();
        let mut failed = None;
        for op in &ops {
            // SAFETY: `db` is open for writing; strings NUL-terminated; buffers valid.
            let ok = unsafe {
                match op {
                    WriteOp::Begin(t) => {
                        let id = begin(db, *t);
                        stack.push(id);
                        id != 0
                    }
                    WriteOp::End(_) => stack.pop().is_some_and(|id| end(db, id) != 0),
                    WriteOp::Str(t, s) => wstr(db, *t, wide(s).as_ptr()) != 0,
                    WriteOp::Dword(t, v) => wdword(db, *t, *v) != 0,
                    WriteOp::Binary(t, b) => wbin(db, *t, b.as_ptr(), b.len() as u32) != 0,
                    WriteOp::Null(t) => wnull(db, *t) != 0,
                }
            };
            if !ok {
                failed = Some(format!("{op:?}"));
                break;
            }
        }
        // SAFETY: closes the handle opened above.
        unsafe { close(db) };
        match failed {
            Some(op) => {
                let _ = std::fs::remove_file(path);
                Err(ShimError::System(format!("sdb write failed at {op}")))
            }
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------- https fetch

/// `https://host[:port]/path` -> (host, port, path-and-query).
pub fn split_https_url(url: &str) -> Result<(String, u16, String), ShimError> {
    let rest = url.strip_prefix("https://").ok_or_else(|| ShimError::InvalidValue("https only".into()))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if hostport.is_empty() || hostport.contains('@') {
        return Err(ShimError::InvalidValue("bad url host".into()));
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| ShimError::InvalidValue("bad url port".into()))?),
        None => (hostport, 443),
    };
    Ok((host.to_string(), port, path.to_string()))
}

/// WinHTTP client (system proxy, TLS validation by the OS, redirects followed).
pub struct WinHttpFetcher;

struct HInternet(*mut c_void);
impl Drop for HInternet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle from WinHttp*.
            unsafe { windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0) };
        }
    }
}

impl Fetcher for WinHttpFetcher {
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, ShimError> {
        use windows_sys::Win32::Networking::WinHttp::*;
        let (host, port, path) = split_https_url(url)?;
        let err = |what: &str| ShimError::Download(format!("{what}: {}", std::io::Error::last_os_error()));
        let agent = wide("PulsarSplit/0.1");
        // SAFETY: WinHTTP calls with valid NUL-terminated strings; handles closed by Drop.
        unsafe {
            let s = HInternet(WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            ));
            if s.0.is_null() {
                return Err(err("WinHttpOpen"));
            }
            let h = wide(&host);
            let c = HInternet(WinHttpConnect(s.0, h.as_ptr(), port, 0));
            if c.0.is_null() {
                return Err(err("WinHttpConnect"));
            }
            let verb = wide("GET");
            let p = wide(&path);
            let r = HInternet(WinHttpOpenRequest(
                c.0,
                verb.as_ptr(),
                p.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if r.0.is_null() {
                return Err(err("WinHttpOpenRequest"));
            }
            if WinHttpSendRequest(r.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
                return Err(err("WinHttpSendRequest"));
            }
            if WinHttpReceiveResponse(r.0, std::ptr::null_mut()) == 0 {
                return Err(err("WinHttpReceiveResponse"));
            }
            let mut status = 0u32;
            let mut len = 4u32;
            if WinHttpQueryHeaders(
                r.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut len,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(err("WinHttpQueryHeaders"));
            }
            if status != 200 {
                return Err(ShimError::Download(format!("HTTP {status}")));
            }
            let mut out = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut n = 0u32;
                if WinHttpReadData(r.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n) == 0 {
                    return Err(err("WinHttpReadData"));
                }
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
                if out.len() > max_bytes {
                    return Err(ShimError::Download("response too large".into()));
                }
            }
            Ok(out)
        }
    }
}

/// `%ProgramData%\Pulsar Seat\shim\journal\shim.jsonl`
pub fn journal_path() -> PathBuf {
    WinShimSystem.dirs().data_root.join("journal").join("shim.jsonl")
}

/// The journal, after its folder is locked to SYSTEM / Administrators and the file (if
/// any) is checked (no link, trusted owner). Elevated restore trusts what it reads here.
pub fn open_journal() -> Result<crate::journal::JsonlFileLog, ShimError> {
    let path = journal_path();
    let dir = path.parent().ok_or_else(|| ShimError::InvalidValue("journal path".into()))?;
    crate::winfs::ensure_private_dir(&WinShimSystem.dirs().data_root, dir)?;
    crate::winfs::check_file(&path)?;
    Ok(crate::journal::JsonlFileLog { path })
}

/// `%ProgramData%\Pulsar Seat\shim\dxvk`
pub fn dxvk_root() -> PathBuf {
    WinShimSystem.dirs().data_root.join("dxvk")
}

/// [`dxvk_root`], locked to SYSTEM / Administrators (releases below inherit the DACL).
pub fn ensure_dxvk_root() -> Result<PathBuf, ShimError> {
    let root = dxvk_root();
    crate::winfs::ensure_private_dir(&WinShimSystem.dirs().data_root, &root)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        assert_eq!(
            split_https_url("https://github.com/a/b.tar.gz").unwrap(),
            ("github.com".into(), 443, "/a/b.tar.gz".into())
        );
        assert_eq!(split_https_url("https://h:8443").unwrap(), ("h".into(), 8443, "/".into()));
        assert!(split_https_url("http://h/").is_err());
        assert!(split_https_url("https://u@h/").is_err());
        assert!(split_https_url("https://h:x/").is_err());
    }

    #[test]
    fn winreg_value_conversion() {
        for v in [
            RegValue::Dword(7),
            RegValue::Sz("abc".into()),
            RegValue::ExpandSz("%x%".into()),
            RegValue::MultiSz(vec!["a".into(), "b".into()]),
        ] {
            assert_eq!(from_winreg(&to_winreg(&v)), Some(v));
        }
    }

    #[test]
    fn every_hklm_access_uses_the_helpers() {
        assert_eq!(hklm_access(KEY_READ) & KEY_WOW64_64KEY, KEY_WOW64_64KEY);
        assert_eq!(hklm_access(KEY_ALL_ACCESS) & KEY_WOW64_64KEY, KEY_WOW64_64KEY);
        let src = include_str!("win.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        // One call each, inside hklm_open / hklm_create / hklm_delete_tree.
        assert_eq!(body.matches("open_subkey_with_flags(").count(), 1);
        assert_eq!(body.matches("create_subkey_with_flags(").count(), 1);
        assert_eq!(body.matches(".delete_subkey_all(").count(), 1);
        assert_eq!(body.matches("RegKey::predef(").count(), 2);
        assert_eq!(body.matches("RegCreateKeyExW(").count(), 1);
        assert!(body.contains("hklm_access(KEY_ALL_ACCESS)"));
        assert!(!body.contains("open_subkey("));
        assert!(!body.contains("create_subkey("));
    }

    #[test]
    fn random_ids_differ() {
        assert_ne!(random16(), random16());
    }
}
