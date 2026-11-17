// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementations: [`WinOps`] (registry, power, IP Helper, pnputil) and the
//! probes / helpers the other modules call. Documented Win32 APIs only.
//!
//! Untested on hardware beyond compile + the read-only probes exercised by
//! `cargo test` on the dev box (registry reads, CPU sets, native machine).

#![allow(unsafe_code)]

use crate::ops::{
    DriverStoreInfo, Hive, LocalAddress, OpsError, OpsResult, PowerSetting, RegData, SystemOps,
};
use seat_proto::messages::CpuSetInfo;
use std::ffi::OsStr;
use std::net::Ipv4Addr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_EXISTS, GetLastError, HANDLE, LocalFree,
};
use windows_sys::core::GUID;
use winreg::RegKey;
use winreg::enums::*;

pub(crate) fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Owned kernel handle.
pub struct Handle(pub HANDLE);
// SAFETY: a kernel handle value may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// ---- registry ---------------------------------------------------------------------------

fn root(h: Hive) -> RegKey {
    match h {
        Hive::Hklm => RegKey::predef(HKEY_LOCAL_MACHINE),
        Hive::Hkcu => RegKey::predef(HKEY_CURRENT_USER),
        Hive::Users => RegKey::predef(HKEY_USERS),
    }
}

/// Enable a privilege the token already holds (SeBackup / SeRestore for hive loading).
fn enable_privilege(name: windows_sys::core::PCWSTR) -> OpsResult<()> {
    use windows_sys::Win32::Foundation::LUID;
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: pseudo handle + out handle.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut h,
        )
    } == 0
    {
        return Err(OpsError::win32("OpenProcessToken", unsafe {
            GetLastError()
        }));
    }
    let h = Handle(h);
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: valid name + out LUID.
    if unsafe { LookupPrivilegeValueW(std::ptr::null(), name, &mut luid) } == 0 {
        return Err(OpsError::win32("LookupPrivilegeValueW", unsafe {
            GetLastError()
        }));
    }
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: valid token and structure.
    let ok = unsafe {
        AdjustTokenPrivileges(h.0, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut())
    };
    // ERROR_NOT_ALL_ASSIGNED (1300): the token does not hold it.
    let err = unsafe { GetLastError() };
    if ok == 0 || err == 1300 {
        return Err(OpsError::win32("AdjustTokenPrivileges", err));
    }
    Ok(())
}

fn expand_env(s: &str) -> String {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    let w = wide(s);
    let mut buf = vec![0u16; 1024];
    // SAFETY: valid buffers.
    let n = unsafe { ExpandEnvironmentStringsW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize > buf.len() {
        return s.to_string();
    }
    from_wide(&buf)
}

/// Profile folder of a SID from `ProfileList` (`None` = no such profile).
fn profile_path(sid: &str) -> Option<String> {
    let k = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}"),
            KEY_READ | KEY_WOW64_64KEY,
        )
        .ok()?;
    let p: String = k.get_value("ProfileImagePath").ok()?;
    Some(expand_env(&p))
}

fn io_err(op: &str, e: std::io::Error) -> OpsError {
    OpsError {
        op: op.into(),
        message: e.to_string(),
        win32: e.raw_os_error().map(|c| c as u32),
    }
}

fn is_not_found(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

fn utf16_strings(b: &[u8]) -> Vec<String> {
    let u: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    u.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

pub(crate) fn regdata_of(v: &winreg::RegValue) -> Option<RegData> {
    to_regdata(v)
}

fn to_regdata(v: &winreg::RegValue) -> Option<RegData> {
    let b = &v.bytes;
    Some(match v.vtype {
        REG_DWORD if b.len() >= 4 => RegData::Dword(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        REG_QWORD if b.len() >= 8 => RegData::Qword(u64::from_le_bytes(b[..8].try_into().ok()?)),
        REG_SZ => RegData::Sz(utf16_strings(b).into_iter().next().unwrap_or_default()),
        REG_EXPAND_SZ => RegData::ExpandSz(utf16_strings(b).into_iter().next().unwrap_or_default()),
        REG_MULTI_SZ => RegData::MultiSz(utf16_strings(b)),
        REG_BINARY | REG_NONE => RegData::Binary(b.to_vec()),
        _ => RegData::Binary(b.to_vec()),
    })
}

fn sz_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|u| u.to_le_bytes())
        .collect()
}

fn from_regdata(d: &RegData) -> winreg::RegValue<'static> {
    let (bytes, vtype) = match d {
        RegData::Dword(v) => (v.to_le_bytes().to_vec(), REG_DWORD),
        RegData::Qword(v) => (v.to_le_bytes().to_vec(), REG_QWORD),
        RegData::Sz(s) => (sz_bytes(s), REG_SZ),
        RegData::ExpandSz(s) => (sz_bytes(s), REG_EXPAND_SZ),
        RegData::MultiSz(v) => {
            let mut b: Vec<u8> = Vec::new();
            for s in v {
                b.extend(sz_bytes(s));
            }
            b.extend([0, 0]);
            (b, REG_MULTI_SZ)
        }
        RegData::Binary(b) => (b.clone(), REG_BINARY),
    };
    winreg::RegValue {
        bytes: bytes.into(),
        vtype,
    }
}

// ---- power ------------------------------------------------------------------------------

/// Parse `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (braces optional).
pub(crate) fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

pub(crate) fn guid_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

fn guids(s: &PowerSetting) -> OpsResult<(GUID, GUID, GUID)> {
    let bad = || OpsError::new("power", "invalid GUID");
    Ok((
        parse_guid(&s.scheme).ok_or_else(bad)?,
        parse_guid(&s.subgroup).ok_or_else(bad)?,
        parse_guid(&s.setting).ok_or_else(bad)?,
    ))
}

/// Production [`SystemOps`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WinOps;

impl SystemOps for WinOps {
    fn reg_read(&self, hive: Hive, key: &str, name: &str) -> OpsResult<Option<RegData>> {
        let k = match root(hive).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(io_err("reg_read", e)),
        };
        match k.get_raw_value(name) {
            Ok(v) => Ok(to_regdata(&v)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(io_err("reg_read", e)),
        }
    }

    fn reg_write(&self, hive: Hive, key: &str, name: &str, data: &RegData) -> OpsResult<()> {
        let (k, _) = root(hive)
            .create_subkey_with_flags(key, KEY_WRITE | KEY_READ | KEY_WOW64_64KEY)
            .map_err(|e| io_err("reg_write", e))?;
        k.set_raw_value(name, &from_regdata(data))
            .map_err(|e| io_err("reg_write", e))
    }

    fn reg_delete(&self, hive: Hive, key: &str, name: &str) -> OpsResult<()> {
        let k = match root(hive).open_subkey_with_flags(key, KEY_SET_VALUE | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if is_not_found(&e) => return Ok(()),
            Err(e) => return Err(io_err("reg_delete", e)),
        };
        match k.delete_value(name) {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(io_err("reg_delete", e)),
        }
    }

    fn reg_key_exists(&self, hive: Hive, key: &str) -> OpsResult<bool> {
        match root(hive).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
            Ok(_) => Ok(true),
            Err(e) if is_not_found(&e) => Ok(false),
            // Access denied still proves the key exists.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
            Err(e) => Err(io_err("reg_key_exists", e)),
        }
    }

    fn reg_subkeys(&self, hive: Hive, key: &str) -> OpsResult<Vec<String>> {
        match root(hive).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
            Ok(k) => Ok(k.enum_keys().filter_map(Result::ok).collect()),
            Err(e) if is_not_found(&e) => Ok(Vec::new()),
            Err(e) => Err(io_err("reg_subkeys", e)),
        }
    }

    fn power_active_scheme(&self) -> OpsResult<String> {
        use windows_sys::Win32::System::Power::PowerGetActiveScheme;
        let mut p: *mut GUID = std::ptr::null_mut();
        // SAFETY: out pointer; the GUID is freed with LocalFree.
        let rc = unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut p) };
        if rc != 0 || p.is_null() {
            return Err(OpsError::win32("PowerGetActiveScheme", rc));
        }
        // SAFETY: p points to a GUID allocated by the API.
        let s = guid_string(unsafe { &*p });
        unsafe { LocalFree(p as _) };
        Ok(s)
    }

    fn power_read_ac(&self, s: &PowerSetting) -> OpsResult<Option<u32>> {
        use windows_sys::Win32::System::Power::PowerReadACValueIndex;
        let (a, b, c) = guids(s)?;
        let mut v = 0u32;
        // SAFETY: GUID pointers are valid for the call.
        let rc = unsafe { PowerReadACValueIndex(std::ptr::null_mut(), &a, &b, &c, &mut v) };
        match rc {
            0 => Ok(Some(v)),
            2 | 1168 => Ok(None), // file / element not found: setting absent on this PC
            e => Err(OpsError::win32("PowerReadACValueIndex", e)),
        }
    }

    fn power_write_ac(&self, s: &PowerSetting, value: u32) -> OpsResult<()> {
        use windows_sys::Win32::System::Power::{PowerSetActiveScheme, PowerWriteACValueIndex};
        let (a, b, c) = guids(s)?;
        // SAFETY: as above.
        let rc = unsafe { PowerWriteACValueIndex(std::ptr::null_mut(), &a, &b, &c, value) };
        if rc != 0 {
            return Err(OpsError::win32("PowerWriteACValueIndex", rc));
        }
        if self.power_active_scheme().ok().as_deref() == Some(s.scheme.as_str()) {
            // Re-activate so the new index takes effect now.
            let rc = unsafe { PowerSetActiveScheme(std::ptr::null_mut(), &a) };
            if rc != 0 {
                return Err(OpsError::win32("PowerSetActiveScheme", rc));
            }
        }
        Ok(())
    }

    fn ip_list(&self) -> OpsResult<Vec<LocalAddress>> {
        ip::list()
    }
    fn ip_add(&self, interface_luid: u64, address: Ipv4Addr, prefix_len: u8) -> OpsResult<()> {
        ip::add(interface_luid, address, prefix_len)
    }
    fn ip_remove(&self, interface_luid: u64, address: Ipv4Addr) -> OpsResult<()> {
        ip::remove(interface_luid, address)
    }

    fn driver_delete(&self, published_inf: &str) -> OpsResult<()> {
        let name = oem_name(published_inf)?;
        // No `/force`: a package still in use by a device stays and is retried later.
        match pnputil(&["/delete-driver", &name, "/uninstall"])? {
            0 | 3010 => Ok(()),
            code => Err(OpsError {
                op: "pnputil /delete-driver".into(),
                message: format!("exit code {code}"),
                win32: Some(code),
            }),
        }
    }

    fn driver_info(&self, published_inf: &str) -> OpsResult<Option<DriverStoreInfo>> {
        let path = store_inf_path(published_inf)?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(io_err("driver_info", e)),
        };
        Ok(Some(DriverStoreInfo {
            original_name: inf_original_name(&path)?,
            inf_sha256: Some(crate::drivers::sha256_hex(&bytes)),
        }))
    }

    fn reg_delete_empty_key(&self, hive: Hive, key: &str) -> OpsResult<bool> {
        let k = match root(hive).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if is_not_found(&e) => return Ok(false),
            Err(e) => return Err(io_err("reg_delete_empty_key", e)),
        };
        let info = k
            .query_info()
            .map_err(|e| io_err("reg_delete_empty_key", e))?;
        if info.sub_keys > 0 || info.values > 0 {
            return Ok(false);
        }
        drop(k);
        let (parent, leaf) = match key.trim_matches('\\').rsplit_once('\\') {
            Some((p, l)) => (p.to_string(), l.to_string()),
            None => (String::new(), key.trim_matches('\\').to_string()),
        };
        let p = if parent.is_empty() {
            root(hive)
        } else {
            root(hive)
                .open_subkey_with_flags(&parent, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
                .map_err(|e| io_err("reg_delete_empty_key", e))?
        };
        match p.delete_subkey_with_flags(&leaf, KEY_WOW64_64KEY) {
            Ok(()) => Ok(true),
            Err(e) if is_not_found(&e) => Ok(false),
            Err(e) => Err(io_err("reg_delete_empty_key", e)),
        }
    }

    fn with_user_hive(
        &self,
        sid: &str,
        f: &mut dyn FnMut(&str) -> OpsResult<()>,
    ) -> OpsResult<bool> {
        use windows_sys::Win32::Security::{SE_BACKUP_NAME, SE_RESTORE_NAME};
        use windows_sys::Win32::System::Registry::{HKEY_USERS as HKU, RegLoadKeyW, RegUnLoadKeyW};
        if !crate::ops::is_sid_string(sid) {
            return Err(OpsError::new("with_user_hive", format!("not a SID: {sid}")));
        }
        // Logged-on user: the hive is already under HKU\<sid>.
        if self.reg_key_exists(Hive::Users, sid)? {
            f(sid)?;
            return Ok(true);
        }
        let Some(profile) = profile_path(sid) else {
            return Ok(false);
        };
        let dat = Path::new(&profile).join("NTUSER.DAT");
        if !dat.is_file() {
            return Ok(false);
        }
        enable_privilege(SE_BACKUP_NAME)?;
        enable_privilege(SE_RESTORE_NAME)?;
        let mount = format!("PulsarSeat-{sid}");
        let wm = wide(&mount);
        let wd = wide(&dat.to_string_lossy());
        // SAFETY: valid strings; HKU is a predefined key.
        let rc = unsafe { RegLoadKeyW(HKU, wm.as_ptr(), wd.as_ptr()) };
        if rc != 0 {
            return Err(OpsError::win32("RegLoadKeyW", rc));
        }
        let r = f(&mount);
        // Every RegKey opened by `f` is dropped by now.
        // SAFETY: we loaded it above.
        let rc = unsafe { RegUnLoadKeyW(HKU, wm.as_ptr()) };
        r?;
        if rc != 0 {
            return Err(OpsError::win32("RegUnLoadKeyW", rc));
        }
        Ok(true)
    }

    fn reg_values(&self, hive: Hive, key: &str) -> OpsResult<Vec<(String, RegData)>> {
        crate::win_extra::reg_values(root(hive), key)
    }

    fn defender_exclusions(&self) -> OpsResult<Option<crate::ops::DefenderExclusions>> {
        crate::win_extra::defender_exclusions()
    }

    fn defender_exclusion(&self, kind: crate::ops::ExclusionKind, value: &str, add: bool) -> OpsResult<()> {
        crate::win_extra::defender_exclusion(kind, value, add)
    }

    fn account_rights(&self, sid: &str) -> OpsResult<Vec<String>> {
        crate::win_extra::account_rights(sid)
    }

    fn account_right(&self, sid: &str, right: &str, add: bool) -> OpsResult<()> {
        crate::win_extra::account_right(sid, right, add)
    }

    fn cert_thumbprints(&self, store: crate::certs::CertStore) -> OpsResult<Vec<String>> {
        crate::win_certs::cert_thumbprints(store)
    }

    fn cert_add(&self, store: crate::certs::CertStore, der: &[u8]) -> OpsResult<()> {
        crate::win_certs::cert_add(store, der)
    }

    fn cert_remove(&self, store: crate::certs::CertStore, thumbprint: &str) -> OpsResult<()> {
        crate::win_certs::cert_remove(store, thumbprint)
    }

    fn feature_state(&self, name: &str) -> OpsResult<crate::features::FeatureState> {
        crate::win_certs::feature_state(name)
    }

    fn feature_enable(&self, name: &str) -> OpsResult<bool> {
        crate::win_certs::feature_enable(name)
    }
}

pub use crate::win_certs::inspect_cert;

/// Leaf signer of a verified catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSigner {
    /// Simple display name of the subject.
    pub name: String,
    /// SHA-1 thumbprint (upper-case hex).
    pub thumbprint: String,
}

/// `oemNN.inf` (file name only, digits only between `oem` and `.inf`).
fn oem_name(published_inf: &str) -> OpsResult<String> {
    let name = Path::new(published_inf)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let l = name.to_ascii_lowercase();
    let ok = l
        .strip_prefix("oem")
        .and_then(|r| r.strip_suffix(".inf"))
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    if !ok {
        return Err(OpsError::new(
            "driver_store",
            format!("refusing non-OEM package name {name}"),
        ));
    }
    Ok(name)
}

/// Original INF name of a published INF (`SetupGetInfInformationW` +
/// `SetupQueryInfOriginalFileInformationW`).
fn inf_original_name(path: &Path) -> OpsResult<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        INFINFO_INF_NAME_IS_ABSOLUTE, SP_INF_INFORMATION, SP_ORIGINAL_FILE_INFO_W,
        SetupGetInfInformationW, SetupQueryInfOriginalFileInformationW,
    };
    let w = wide(&path.to_string_lossy());
    let mut need = 0u32;
    // SAFETY: size query.
    unsafe {
        SetupGetInfInformationW(
            w.as_ptr() as *const _,
            INFINFO_INF_NAME_IS_ABSOLUTE,
            std::ptr::null_mut(),
            0,
            &mut need,
        )
    };
    if need == 0 {
        return Err(OpsError::win32("SetupGetInfInformationW", unsafe {
            GetLastError()
        }));
    }
    // u64 buffer keeps the structure aligned.
    let mut buf = vec![0u64; (need as usize).div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut SP_INF_INFORMATION;
    // SAFETY: buffer of `need` bytes.
    if unsafe {
        SetupGetInfInformationW(
            w.as_ptr() as *const _,
            INFINFO_INF_NAME_IS_ABSOLUTE,
            info,
            need,
            &mut need,
        )
    } == 0
    {
        return Err(OpsError::win32("SetupGetInfInformationW", unsafe {
            GetLastError()
        }));
    }
    // SAFETY: zeroed POD with cbSize set.
    let mut ofi: SP_ORIGINAL_FILE_INFO_W = unsafe { std::mem::zeroed() };
    ofi.cbSize = std::mem::size_of::<SP_ORIGINAL_FILE_INFO_W>() as u32;
    // SAFETY: info filled above.
    if unsafe { SetupQueryInfOriginalFileInformationW(info, 0, std::ptr::null(), &mut ofi) } == 0 {
        return Err(OpsError::win32(
            "SetupQueryInfOriginalFileInformationW",
            unsafe { GetLastError() },
        ));
    }
    // By-value copy first: the struct is packed (1-byte aligned) on x86, so a reference to
    // the field would be unaligned (E0793 on i686-pc-windows-msvc).
    let name = ofi.OriginalInfName;
    let full = from_wide(&name);
    Ok(Path::new(&full)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or(full))
}

/// Published INF path in the store (`%SystemRoot%\INF\oemNN.inf`).
pub fn store_inf_path(published_inf: &str) -> OpsResult<std::path::PathBuf> {
    let name = oem_name(published_inf)?;
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Ok(Path::new(&sys).join("INF").join(name))
}

/// Run `%SystemRoot%\System32\pnputil.exe` without a window; returns the exit code.
pub fn pnputil(args: &[&str]) -> OpsResult<u32> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = Path::new(&sys).join("System32").join("pnputil.exe");
    let st = std::process::Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| io_err("pnputil", e))?;
    Ok(st.code().map(|c| c as u32).unwrap_or(1))
}

// ---- probes -----------------------------------------------------------------------------

/// The current token is elevated (Administrators enabled) — or SYSTEM.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: pseudo handle + out handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut h) } == 0 {
        return false;
    }
    let h = Handle(h);
    let mut el = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut ret = 0u32;
    // SAFETY: buffer of the right size.
    let ok = unsafe {
        GetTokenInformation(
            h.0,
            TokenElevation,
            &mut el as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
    };
    ok != 0 && el.TokenIsElevated != 0
}

/// Native machine of the PC (`IsWow64Process2`; an x64 process on ARM64 still gets ARM64).
/// Second probe when that fails: `GetMachineTypeAttributes` (ARM64 user mode runs
/// natively only on an ARM64 PC). `None` when both fail: callers must not assume a match.
pub fn native_machine() -> Option<u16> {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetMachineTypeAttributes, IsWow64Process2, UserEnabled,
    };
    let mut process = 0u16;
    let mut native = 0u16;
    // SAFETY: out parameters.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) };
    if ok != 0 && native != 0 {
        return Some(native);
    }
    let mut attrs = 0;
    // SAFETY: out parameter. Absent before Windows 11; the call then fails.
    let hr =
        unsafe { GetMachineTypeAttributes(crate::preflight::IMAGE_FILE_MACHINE_ARM64, &mut attrs) };
    if hr != 0 {
        return None;
    }
    Some(if attrs & UserEnabled != 0 {
        crate::preflight::IMAGE_FILE_MACHINE_ARM64
    } else {
        crate::preflight::IMAGE_FILE_MACHINE_AMD64
    })
}

/// Windows booted in Safe Mode.
pub fn is_safe_mode() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CLEANBOOT};
    // SAFETY: trivial call.
    unsafe { GetSystemMetrics(SM_CLEANBOOT) != 0 }
}

/// `GetSystemCpuSetInformation` for the whole system.
pub fn cpu_sets() -> OpsResult<Vec<CpuSetInfo>> {
    use windows_sys::Win32::System::SystemInformation::{
        CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };
    let mut len = 0u32;
    // SAFETY: size query.
    unsafe {
        GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut len, std::ptr::null_mut(), 0)
    };
    if len == 0 {
        return Err(OpsError::win32("GetSystemCpuSetInformation", unsafe {
            GetLastError()
        }));
    }
    // u64 buffer keeps the entries 8-byte aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of `len` bytes.
    let ok = unsafe {
        GetSystemCpuSetInformation(
            buf.as_mut_ptr() as *mut _,
            len,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return Err(OpsError::win32("GetSystemCpuSetInformation", unsafe {
            GetLastError()
        }));
    }
    let base = buf.as_ptr() as *const u8;
    let mut off = 0usize;
    let mut out = Vec::new();
    while off + 8 <= len as usize {
        // SAFETY: within the buffer; each entry begins with its Size.
        let e = unsafe { &*(base.add(off) as *const SYSTEM_CPU_SET_INFORMATION) };
        if e.Size == 0 {
            break;
        }
        if e.Type == CpuSetInformation {
            // SAFETY: Type says the union holds CpuSet.
            let c = unsafe { &e.Anonymous.CpuSet };
            out.push(CpuSetInfo {
                id: c.Id,
                group: c.Group,
                logical_index: c.LogicalProcessorIndex,
                core_index: c.CoreIndex,
                efficiency_class: c.EfficiencyClass,
                numa_node: c.NumaNodeIndex as u32,
            });
        }
        off += e.Size as usize;
    }
    Ok(out)
}

// ---- signing / driver store -------------------------------------------------------------

/// Leaf signer display name from a verified WinVerifyTrust state.
///
/// # Safety
/// `state` is the `hWVTStateData` of a successful verify, not yet closed.
unsafe fn signer_name(state: HANDLE) -> OpsResult<CatalogSigner> {
    use windows_sys::Win32::Security::Cryptography::{
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
    };
    use windows_sys::Win32::Security::WinTrust::*;
    // SAFETY: per the function contract.
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        let sgnr = if prov.is_null() {
            std::ptr::null_mut()
        } else {
            WTHelperGetProvSignerFromChain(prov, 0, 0, 0)
        };
        if sgnr.is_null()
            || (*sgnr).pasCertChain.is_null()
            || (*(*sgnr).pasCertChain).pCert.is_null()
        {
            return Err(OpsError::new("WinVerifyTrust", "no signer certificate"));
        }
        let cert = (*(*sgnr).pasCertChain).pCert;
        let mut buf = [0u16; 512];
        let n = CertGetNameStringW(
            cert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            std::ptr::null(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        );
        Ok(CatalogSigner { name: from_wide(&buf[..n as usize]), thumbprint: crate::win_certs::context_thumbprint(cert) })
    }
}

/// Owned catalog admin context.
struct CatAdmin(isize);
impl Drop for CatAdmin {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Cryptography::Catalog::CryptCATAdminReleaseContext;
        // SAFETY: acquired by CryptCATAdminAcquireContext2.
        unsafe { CryptCATAdminReleaseContext(self.0, 0) };
    }
}

/// Verify that `member` (the INF) is listed in catalog `cat` and that the catalog's
/// Authenticode signature is trusted (`WinVerifyTrust`, `WTD_CHOICE_CATALOG`, generic
/// verify v2, no UI, no revocation fetch). Returns the catalog's leaf signer (display
/// name and thumbprint). A catalog copied from the OS that does not list our INF fails here.
/// Tries a SHA-256 member hash first, then SHA-1 (older catalogs).
pub fn catalog_member_signer(cat: &Path, member: &Path) -> OpsResult<CatalogSigner> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::Cryptography::Catalog::{
        CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    };
    use windows_sys::Win32::Security::WinTrust::*;
    let file = std::fs::File::open(member).map_err(|e| io_err("open INF", e))?;
    let cat_w = wide(&cat.to_string_lossy());
    let member_w = wide(&member.to_string_lossy());
    let mut last = OpsError::new("WinVerifyTrust", "member not in catalog");
    for alg in ["SHA256", "SHA1"] {
        let alg_w = wide(alg);
        let mut h: isize = 0;
        // SAFETY: out handle; default subsystem; released by CatAdmin.
        if unsafe {
            CryptCATAdminAcquireContext2(
                &mut h,
                std::ptr::null(),
                alg_w.as_ptr(),
                std::ptr::null(),
                0,
            )
        } == 0
        {
            last = OpsError::win32("CryptCATAdminAcquireContext2", unsafe { GetLastError() });
            continue;
        }
        let admin = CatAdmin(h);
        let mut cb = 0u32;
        // SAFETY: size query on a valid handle.
        unsafe {
            CryptCATAdminCalcHashFromFileHandle2(
                admin.0,
                file.as_raw_handle() as _,
                &mut cb,
                std::ptr::null_mut(),
                0,
            )
        };
        if cb == 0 {
            last = OpsError::win32("CryptCATAdminCalcHashFromFileHandle2", unsafe {
                GetLastError()
            });
            continue;
        }
        let mut hash = vec![0u8; cb as usize];
        // SAFETY: buffer of cb bytes.
        if unsafe {
            CryptCATAdminCalcHashFromFileHandle2(
                admin.0,
                file.as_raw_handle() as _,
                &mut cb,
                hash.as_mut_ptr(),
                0,
            )
        } == 0
        {
            last = OpsError::win32("CryptCATAdminCalcHashFromFileHandle2", unsafe {
                GetLastError()
            });
            continue;
        }
        // Member tag = hash as uppercase hex (how catalogs name file members).
        let tag: String = hash.iter().map(|b| format!("{b:02X}")).collect();
        let tag_w = wide(&tag);
        // SAFETY: zeroed POD structs, filled below.
        let mut ci: WINTRUST_CATALOG_INFO = unsafe { std::mem::zeroed() };
        ci.cbStruct = std::mem::size_of::<WINTRUST_CATALOG_INFO>() as u32;
        ci.pcwszCatalogFilePath = cat_w.as_ptr();
        ci.pcwszMemberTag = tag_w.as_ptr();
        ci.pcwszMemberFilePath = member_w.as_ptr();
        ci.hMemberFile = file.as_raw_handle() as _;
        ci.pbCalculatedFileHash = hash.as_mut_ptr();
        ci.cbCalculatedFileHash = cb;
        ci.hCatAdmin = admin.0;
        let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
        data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_CATALOG;
        data.Anonymous.pCatalog = &mut ci;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: valid structures for the call (null hwnd = no UI).
        let rc = unsafe {
            WinVerifyTrust(
                std::ptr::null_mut(),
                &mut action,
                &mut data as *mut _ as *mut _,
            )
        };
        let result = if rc != 0 {
            Err(OpsError {
                op: "WinVerifyTrust".into(),
                message: format!(
                    "INF not signed by this catalog ({alg}, 0x{:08x})",
                    rc as u32
                ),
                win32: Some(rc as u32),
            })
        } else {
            // SAFETY: state valid until WTD_STATEACTION_CLOSE below.
            unsafe { signer_name(data.hWVTStateData) }
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        // SAFETY: closes the state opened above.
        unsafe {
            WinVerifyTrust(
                std::ptr::null_mut(),
                &mut action,
                &mut data as *mut _ as *mut _,
            )
        };
        match result {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Publish an INF into the driver store (`SetupCopyOEMInfW`, no-overwrite). Returns the
/// published `oemNN.inf` name and whether this call added it (`false` = it was there).
pub fn publish_inf(inf: &Path) -> OpsResult<(String, bool)> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SP_COPY_NOOVERWRITE, SPOST_PATH, SetupCopyOEMInfW,
    };
    let src = wide(&inf.to_string_lossy());
    let dir = inf
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let media = wide(&dir);
    let mut dest = [0u16; 520];
    let mut req = 0u32;
    // SAFETY: valid buffers.
    let ok = unsafe {
        SetupCopyOEMInfW(
            src.as_ptr(),
            media.as_ptr(),
            SPOST_PATH,
            SP_COPY_NOOVERWRITE,
            dest.as_mut_ptr(),
            dest.len() as u32,
            &mut req,
            std::ptr::null_mut(),
        )
    };
    let err = if ok == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    let name = Path::new(&from_wide(&dest))
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    match (ok, err) {
        (0, e) if e == ERROR_FILE_EXISTS && !name.is_empty() => Ok((name, false)),
        (0, e) => Err(OpsError::win32("SetupCopyOEMInfW", e)),
        _ if name.is_empty() => Err(OpsError::new("SetupCopyOEMInfW", "no published name")),
        _ => Ok((name, true)),
    }
}

// ---- System Restore ---------------------------------------------------------------------

/// Create a restore point of type "application install" (begin + end). Windows limits
/// creation frequency (by default one per 24 h); a skipped creation still returns a
/// sequence number. Fails when System Protection is off. Untested on hardware.
pub fn create_restore_point(description: &str) -> OpsResult<i64> {
    use windows_sys::Win32::System::Restore::{
        APPLICATION_INSTALL, BEGIN_SYSTEM_CHANGE, END_SYSTEM_CHANGE, RESTOREPOINTINFOW,
        SRSetRestorePointW, STATEMGRSTATUS,
    };
    let mut info = RESTOREPOINTINFOW {
        dwEventType: BEGIN_SYSTEM_CHANGE,
        dwRestorePtType: APPLICATION_INSTALL,
        llSequenceNumber: 0,
        szDescription: [0; 256],
    };
    for (i, u) in description.encode_utf16().take(255).enumerate() {
        info.szDescription[i] = u;
    }
    let mut st = STATEMGRSTATUS {
        nStatus: 0,
        llSequenceNumber: 0,
    };
    // SAFETY: valid structures.
    if unsafe { SRSetRestorePointW(&info, &mut st) } == 0 {
        return Err(OpsError::win32("SRSetRestorePointW", st.nStatus));
    }
    let seq = st.llSequenceNumber;
    info.dwEventType = END_SYSTEM_CHANGE;
    info.llSequenceNumber = seq;
    // SAFETY: as above.
    if unsafe { SRSetRestorePointW(&info, &mut st) } == 0 {
        return Err(OpsError::win32("SRSetRestorePointW(end)", st.nStatus));
    }
    Ok(seq)
}

// ---- IP Helper --------------------------------------------------------------------------

mod ip {
    use super::*;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceLuidToAlias, CreateUnicastIpAddressEntry, DeleteUnicastIpAddressEntry,
        FreeMibTable, GetUnicastIpAddressTable, InitializeUnicastIpAddressEntry,
        MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, IpPrefixOriginManual};

    fn v4(row: &MIB_UNICASTIPADDRESS_ROW) -> Option<Ipv4Addr> {
        // SAFETY: si_family discriminates the union.
        unsafe {
            if row.Address.si_family != AF_INET {
                return None;
            }
            let raw = row.Address.Ipv4.sin_addr.S_un.S_addr;
            Some(Ipv4Addr::from(raw.to_ne_bytes()))
        }
    }

    pub fn list() -> OpsResult<Vec<LocalAddress>> {
        let mut t: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
        // SAFETY: the table is freed with FreeMibTable.
        let rc = unsafe { GetUnicastIpAddressTable(AF_INET, &mut t) };
        if rc != 0 {
            return Err(OpsError::win32("GetUnicastIpAddressTable", rc));
        }
        let mut out = Vec::new();
        // SAFETY: NumEntries rows follow the header.
        unsafe {
            let n = (*t).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), n);
            for r in rows {
                let Some(a) = v4(r) else { continue };
                let mut alias = [0u16; 257];
                let luid = r.InterfaceLuid;
                let _ = ConvertInterfaceLuidToAlias(&luid, alias.as_mut_ptr(), alias.len());
                out.push(LocalAddress {
                    address: a,
                    prefix_len: r.OnLinkPrefixLength,
                    interface_luid: luid.Value,
                    interface_index: r.InterfaceIndex,
                    interface_alias: from_wide(&alias),
                    is_static: r.PrefixOrigin == IpPrefixOriginManual,
                });
            }
            FreeMibTable(t as _);
        }
        Ok(out)
    }

    fn row(luid: u64, a: Ipv4Addr, prefix: u8) -> MIB_UNICASTIPADDRESS_ROW {
        // SAFETY: zeroed POD, initialised by the API.
        let mut r: MIB_UNICASTIPADDRESS_ROW = unsafe { std::mem::zeroed() };
        unsafe {
            InitializeUnicastIpAddressEntry(&mut r);
            r.Address.Ipv4.sin_family = AF_INET;
            r.Address.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(a.octets());
        }
        r.InterfaceLuid.Value = luid;
        r.OnLinkPrefixLength = prefix;
        r
    }

    pub fn add(luid: u64, a: Ipv4Addr, prefix: u8) -> OpsResult<()> {
        let r = row(luid, a, prefix);
        // SAFETY: valid row.
        let rc = unsafe { CreateUnicastIpAddressEntry(&r) };
        if rc != 0 {
            Err(OpsError::win32("CreateUnicastIpAddressEntry", rc))
        } else {
            Ok(())
        }
    }

    pub fn remove(luid: u64, a: Ipv4Addr) -> OpsResult<()> {
        let r = row(luid, a, 0);
        // SAFETY: valid row (address + interface identify it).
        let rc = unsafe { DeleteUnicastIpAddressEntry(&r) };
        match rc {
            0 | 1168 => Ok(()), // 1168 = not found
            e => Err(OpsError::win32("DeleteUnicastIpAddressEntry", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_roundtrip() {
        let g = parse_guid("{238C9FA8-0AAD-41ED-83F4-97BE242C8F20}").unwrap();
        assert_eq!(guid_string(&g), "238c9fa8-0aad-41ed-83f4-97be242c8f20");
    }

    #[test]
    fn regdata_conversion() {
        for d in [
            RegData::Dword(7),
            RegData::Qword(1 << 40),
            RegData::Sz("ç".into()),
            RegData::ExpandSz("%SystemRoot%".into()),
            RegData::MultiSz(vec!["a".into(), "b".into()]),
            RegData::Binary(vec![1, 2]),
        ] {
            assert_eq!(to_regdata(&from_regdata(&d)), Some(d));
        }
    }

    // Read-only probes on the dev box.
    #[test]
    fn read_only_probes() {
        assert!(native_machine().is_some());
        let sets = cpu_sets().unwrap();
        assert!(!sets.is_empty());
        let ops = WinOps;
        let b = ops
            .reg_read(
                Hive::Hklm,
                r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                "CurrentBuildNumber",
            )
            .unwrap();
        assert!(b.and_then(|d| d.as_str().map(str::to_string)).is_some());
        assert_eq!(
            ops.reg_read(Hive::Hklm, r"SOFTWARE\Pulsar Seat Nonexistent", "X")
                .unwrap(),
            None
        );
        assert!(ops.ip_list().is_ok());
        assert!(ops.power_active_scheme().is_ok());
    }
}
