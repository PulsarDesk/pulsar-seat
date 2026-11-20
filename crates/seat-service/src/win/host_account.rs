// SPDX-License-Identifier: GPL-3.0-only
//! [`AccountOps`] on Windows for the seat hosts' account (`crate::host_account`): local SAM
//! through the NetUser / NetLocalGroup API, logon rights through `LsaAddAccountRights` /
//! `LsaRemoveAccountRights`, the password as LSA private data (`win::lsa`), the logon-screen
//! entry under Winlogon `SpecialAccounts\UserList`, the host group through
//! `NetLocalGroupAdd` / `NetLocalGroupGetInfo` / `NetLocalGroupDel`, profiles of removed seat
//! accounts through `DeleteProfileW`. Needs SYSTEM / elevation: never run in unit tests.
//! **Untested on hardware.**

use crate::host_account::{AccountOps, GroupLookup, HostAccount, Lookup};
use seat_core::{SeatNumber, Secret};
use seat_session::host_account::{GROUP_COMMENT, PASSWORD_SECRET};
use std::ptr;
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree, NTSTATUS, STATUS_OBJECT_NAME_NOT_FOUND};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    LOCALGROUP_MEMBERS_INFO_0, NetApiBufferFree, NetLocalGroupAddMembers, NetLocalGroupGetInfo, NetUserAdd, NetUserDel, NetUserGetInfo,
    NetUserSetInfo, UF_DONT_EXPIRE_PASSWD, UF_NORMAL_ACCOUNT, UF_PASSWD_CANT_CHANGE, UF_SCRIPT, USER_INFO_1, USER_INFO_1003, USER_INFO_1008,
    USER_INFO_1011, USER_INFO_23, USER_PRIV_USER,
};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose, LsaNtStatusToWinError, LsaOpenPolicy,
    LsaRemoveAccountRights, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::{CreateWellKnownSid, LookupAccountSidW, PSID, SECURITY_MAX_SID_SIZE, SID_NAME_USE, WinBuiltinUsersSid};

/// `NERR_UserNotFound`, `NERR_GroupNotFound`, `ERROR_MEMBER_IN_ALIAS`.
const NERR_USER_NOT_FOUND: u32 = 2221;
const ERROR_MEMBER_IN_ALIAS: u32 = 1378;

const USER_LIST_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_secret(s: &Secret) -> zeroize::Zeroizing<Vec<u16>> {
    zeroize::Zeroizing::new(wide(s.expose()))
}

fn net(api: &str, rc: u32) -> Result<(), String> {
    if rc == 0 { Ok(()) } else { Err(format!("{api} failed (win32 {rc})")) }
}

fn sid_to_string(sid: PSID) -> Option<String> {
    let mut s: *mut u16 = ptr::null_mut();
    // SAFETY: valid SID; string freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return None;
        }
        let out = crate::win::security::from_wide_ptr(s);
        LocalFree(s as HLOCAL);
        Some(out)
    }
}

/// Binary SID from its string form (freed on drop).
struct Sid(PSID);

impl Sid {
    fn parse(s: &str) -> Result<Self, String> {
        if !seat_session::host_account::is_sid_string(s) {
            return Err(format!("bad SID {s:?}"));
        }
        let w = wide(s);
        let mut p: PSID = ptr::null_mut();
        // SAFETY: NUL-terminated SID string.
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
            return Err(format!("ConvertStringSidToSidW: {}", std::io::Error::last_os_error()));
        }
        Ok(Self(p))
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSidToSidW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Read a NUL-terminated UTF-16 string (at most 4096 units); empty for null.
///
/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
unsafe fn wide_str(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    // SAFETY: caller guarantees the terminator.
    while unsafe { *p.add(n) } != 0 && n < 4096 {
        n += 1;
    }
    // SAFETY: `n` units read above.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// Localised name of `BUILTIN\Users`.
fn users_group_name() -> Result<Vec<u16>, String> {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    let mut name = [0u16; 256];
    let mut name_len = name.len() as u32;
    let mut dom = [0u16; 256];
    let mut dom_len = dom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers sized as declared.
    unsafe {
        if CreateWellKnownSid(WinBuiltinUsersSid, ptr::null_mut(), sid.as_mut_ptr() as PSID, &mut size) == 0
            || LookupAccountSidW(ptr::null(), sid.as_mut_ptr() as PSID, name.as_mut_ptr(), &mut name_len, dom.as_mut_ptr(), &mut dom_len, &mut use_) == 0
        {
            return Err(format!("Users group: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(name[..=name_len as usize].to_vec())
}

/// SID string of a local group name (NUL-terminated). `Err` when it does not resolve: the
/// membership check then fails closed.
fn group_sid(name: &[u16]) -> Result<String, String> {
    use windows_sys::Win32::Security::LookupAccountNameW;
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    let mut dom = [0u16; 256];
    let mut dom_len = dom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers sized as declared.
    let ok = unsafe { LookupAccountNameW(ptr::null(), name.as_ptr(), sid.as_mut_ptr() as PSID, &mut size, dom.as_mut_ptr(), &mut dom_len, &mut use_) };
    if ok == 0 {
        return Err(format!("group {:?}: {}", String::from_utf16_lossy(&name[..name.len().saturating_sub(1)]), std::io::Error::last_os_error()));
    }
    sid_to_string(sid.as_mut_ptr() as PSID).ok_or_else(|| "group SID not readable".into())
}

struct Policy(LSA_HANDLE);

impl Drop for Policy {
    fn drop(&mut self) {
        // SAFETY: handle from LsaOpenPolicy.
        unsafe { LsaClose(self.0) };
    }
}

fn lsa_err(api: &str, st: NTSTATUS) -> String {
    // SAFETY: pure conversion.
    format!("{api} failed (win32 {})", unsafe { LsaNtStatusToWinError(st) })
}

fn policy() -> Result<Policy, String> {
    // SAFETY: zeroed attributes are what LsaOpenPolicy expects.
    let attrs: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    let mut h: LSA_HANDLE = 0 as LSA_HANDLE;
    // SAFETY: local policy, valid out pointer.
    let st = unsafe { LsaOpenPolicy(ptr::null(), &attrs, (POLICY_LOOKUP_NAMES | POLICY_CREATE_ACCOUNT) as u32, &mut h) };
    if st < 0 {
        return Err(lsa_err("LsaOpenPolicy", st));
    }
    Ok(Policy(h))
}

#[derive(Debug, Default)]
pub struct WinAccountOps;

impl AccountOps for WinAccountOps {
    fn lookup(&self, name: &str) -> Result<Lookup, String> {
        let w = wide(name);
        let mut buf: *mut u8 = ptr::null_mut();
        // SAFETY: local SAM; buffer freed with NetApiBufferFree.
        let rc = unsafe { NetUserGetInfo(ptr::null(), w.as_ptr(), 23, &mut buf) };
        if rc == NERR_USER_NOT_FOUND {
            let mut g: *mut u8 = ptr::null_mut();
            // SAFETY: as above.
            let grc = unsafe { NetLocalGroupGetInfo(ptr::null(), w.as_ptr(), 0, &mut g) };
            if !g.is_null() {
                // SAFETY: NetApi buffer.
                unsafe { NetApiBufferFree(g as *const _) };
            }
            return Ok(if grc == 0 { Lookup::Other } else { Lookup::Missing });
        }
        net("NetUserGetInfo", rc)?;
        // SAFETY: level-23 buffer from NetUserGetInfo; the comment is a NUL-terminated
        // string inside it (or null).
        let (sid, comment) = unsafe {
            let u = &*(buf as *const USER_INFO_23);
            let s = sid_to_string(u.usri23_user_sid);
            let c = if u.usri23_comment.is_null() {
                String::new()
            } else {
                let p = u.usri23_comment;
                let mut n = 0usize;
                while *p.add(n) != 0 && n < 4096 {
                    n += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
            };
            NetApiBufferFree(buf as *const _);
            (s, c)
        };
        // Only the account Pulsar created (its comment) is ours to maintain or delete.
        let ours = if seat_session::kiosk_account::is_kiosk_account_name(name) {
            &seat_session::kiosk_account::KIOSK_COMMENT
        } else if seat_session::console_account::is_console_account_name(name) {
            &seat_session::console_account::CONSOLE_COMMENT
        } else {
            &seat_session::host_account::ACCOUNT_COMMENT
        };
        if !ours.matches(&comment) {
            return Ok(Lookup::Foreign);
        }
        sid.map(Lookup::User).ok_or_else(|| "user SID not readable".into())
    }

    fn create(&self, name: &str, password: &Secret, full_name: &str, comment: &str) -> Result<(), String> {
        let mut n = wide(name);
        let mut pw = wide_secret(password);
        let mut c = wide(comment);
        let info = USER_INFO_1 {
            usri1_name: n.as_mut_ptr(),
            usri1_password: pw.as_mut_ptr(),
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: ptr::null_mut(),
            usri1_comment: c.as_mut_ptr(),
            usri1_flags: UF_SCRIPT | UF_NORMAL_ACCOUNT | UF_DONT_EXPIRE_PASSWD | UF_PASSWD_CANT_CHANGE,
            usri1_script_path: ptr::null_mut(),
        };
        let mut parm = 0u32;
        // SAFETY: level-1 struct with buffers that outlive the call.
        net("NetUserAdd", unsafe { NetUserAdd(ptr::null(), 1, (&info as *const USER_INFO_1).cast(), &mut parm) })?;
        drop(pw);
        let mut f = wide(full_name);
        let full = USER_INFO_1011 { usri1011_full_name: f.as_mut_ptr() };
        // SAFETY: level-1011 struct; cosmetic, failure ignored.
        unsafe { NetUserSetInfo(ptr::null(), n.as_ptr(), 1011, (&full as *const USER_INFO_1011).cast(), &mut parm) };
        Ok(())
    }

    fn set_password(&self, name: &str, password: &Secret) -> Result<(), String> {
        let n = wide(name);
        let mut pw = wide_secret(password);
        let info = USER_INFO_1003 { usri1003_password: pw.as_mut_ptr() };
        let mut parm = 0u32;
        // SAFETY: level-1003 struct; the buffer is wiped on drop.
        net("NetUserSetInfo(1003)", unsafe { NetUserSetInfo(ptr::null(), n.as_ptr(), 1003, (&info as *const USER_INFO_1003).cast(), &mut parm) })
    }

    fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        use windows_sys::Win32::NetworkManagement::NetManagement::UF_ACCOUNTDISABLE;
        let n = wide(name);
        let mut flags = UF_SCRIPT | UF_NORMAL_ACCOUNT | UF_DONT_EXPIRE_PASSWD | UF_PASSWD_CANT_CHANGE;
        if !enabled {
            flags |= UF_ACCOUNTDISABLE;
        }
        let info = USER_INFO_1008 { usri1008_flags: flags };
        let mut parm = 0u32;
        // SAFETY: level-1008 struct.
        net("NetUserSetInfo(1008)", unsafe { NetUserSetInfo(ptr::null(), n.as_ptr(), 1008, (&info as *const USER_INFO_1008).cast(), &mut parm) })
    }

    fn set_flags(&self, name: &str) -> Result<(), String> {
        let n = wide(name);
        // Without UF_ACCOUNTDISABLE / UF_LOCKOUT: enabled.
        let info = USER_INFO_1008 { usri1008_flags: UF_SCRIPT | UF_NORMAL_ACCOUNT | UF_DONT_EXPIRE_PASSWD | UF_PASSWD_CANT_CHANGE };
        let mut parm = 0u32;
        // SAFETY: level-1008 struct.
        net("NetUserSetInfo(1008)", unsafe { NetUserSetInfo(ptr::null(), n.as_ptr(), 1008, (&info as *const USER_INFO_1008).cast(), &mut parm) })
    }

    fn local_groups(&self, name: &str) -> Result<Vec<String>, String> {
        use windows_sys::Win32::NetworkManagement::NetManagement::{LG_INCLUDE_INDIRECT, LOCALGROUP_USERS_INFO_0, MAX_PREFERRED_LENGTH, NetUserGetLocalGroups};
        let w = wide(name);
        let mut buf: *mut u8 = ptr::null_mut();
        let (mut read, mut total) = (0u32, 0u32);
        // SAFETY: local SAM; LG_INCLUDE_INDIRECT also lists local groups reached through a
        // domain (global) group; buffer freed with NetApiBufferFree.
        let rc = unsafe { NetUserGetLocalGroups(ptr::null(), w.as_ptr(), 0, LG_INCLUDE_INDIRECT, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total) };
        let names: Vec<Vec<u16>> = if buf.is_null() {
            Vec::new()
        } else {
            // SAFETY: `read` level-0 entries from NetUserGetLocalGroups.
            let v = unsafe {
                std::slice::from_raw_parts(buf as *const LOCALGROUP_USERS_INFO_0, read as usize)
                    .iter()
                    .map(|g| {
                        let mut n = crate::win::security::from_wide_ptr(g.lgrui0_name).encode_utf16().collect::<Vec<u16>>();
                        n.push(0);
                        n
                    })
                    .collect()
            };
            // SAFETY: NetApi buffer.
            unsafe { NetApiBufferFree(buf as *const _) };
            v
        };
        net("NetUserGetLocalGroups", rc)?;
        if read != total {
            return Err(format!("NetUserGetLocalGroups: {read} of {total} groups"));
        }
        names.iter().map(|n| group_sid(n)).collect()
    }

    fn ensure_users_member(&self, sid: &str) -> Result<(), String> {
        let group = users_group_name()?;
        let s = Sid::parse(sid)?;
        let m = LOCALGROUP_MEMBERS_INFO_0 { lgrmi0_sid: s.0 };
        // SAFETY: one level-0 entry; the SID outlives the call.
        let rc = unsafe { NetLocalGroupAddMembers(ptr::null(), group.as_ptr(), 0, (&m as *const LOCALGROUP_MEMBERS_INFO_0).cast(), 1) };
        if rc == ERROR_MEMBER_IN_ALIAS { Ok(()) } else { net("NetLocalGroupAddMembers", rc) }
    }

    fn add_rights(&self, sid: &str, rights: &[&str]) -> Result<(), String> {
        let p = policy()?;
        let s = Sid::parse(sid)?;
        let mut bufs: Vec<Vec<u16>> = rights.iter().map(|r| r.encode_utf16().collect()).collect();
        let strings: Vec<LSA_UNICODE_STRING> = bufs
            .iter_mut()
            .map(|b| LSA_UNICODE_STRING { Length: (b.len() * 2) as u16, MaximumLength: (b.len() * 2) as u16, Buffer: b.as_mut_ptr() })
            .collect();
        // SAFETY: valid policy and SID; strings outlive the call.
        let st = unsafe { LsaAddAccountRights(p.0, s.0, strings.as_ptr(), strings.len() as u32) };
        if st < 0 { Err(lsa_err("LsaAddAccountRights", st)) } else { Ok(()) }
    }

    fn remove_all_rights(&self, sid: &str) -> Result<(), String> {
        let p = policy()?;
        let s = Sid::parse(sid)?;
        // SAFETY: AllRights = TRUE removes every right and the LSA account object.
        let st = unsafe { LsaRemoveAccountRights(p.0, s.0, true, ptr::null(), 0) };
        if st < 0 && st != STATUS_OBJECT_NAME_NOT_FOUND { Err(lsa_err("LsaRemoveAccountRights", st)) } else { Ok(()) }
    }

    fn hide_from_logon(&self, name: &str, hide: bool) -> Result<(), String> {
        use winreg::RegKey;
        use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_SET_VALUE};
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if hide {
            let (k, _) = hklm.create_subkey(USER_LIST_KEY).map_err(|e| e.to_string())?;
            k.set_value(name, &0u32).map_err(|e| e.to_string())
        } else {
            match hklm.open_subkey_with_flags(USER_LIST_KEY, KEY_SET_VALUE) {
                Ok(k) => match k.delete_value(name) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                    _ => Ok(()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }

    fn store_password(&self, secret: &str, password: Option<&Secret>) -> Result<(), String> {
        super::lsa::set_named(secret, password).map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let n = wide(name);
        // SAFETY: NUL-terminated name.
        let rc = unsafe { NetUserDel(ptr::null(), n.as_ptr()) };
        if rc == NERR_USER_NOT_FOUND { Ok(()) } else { net("NetUserDel", rc) }
    }

    fn group_lookup(&self, name: &str) -> Result<GroupLookup, String> {
        use windows_sys::Win32::NetworkManagement::NetManagement::LOCALGROUP_INFO_1;
        let w = wide(name);
        let mut buf: *mut u8 = ptr::null_mut();
        // SAFETY: local SAM; buffer freed with NetApiBufferFree.
        let rc = unsafe { NetLocalGroupGetInfo(ptr::null(), w.as_ptr(), 1, &mut buf) };
        if rc != 0 {
            if !buf.is_null() {
                // SAFETY: NetApi buffer.
                unsafe { NetApiBufferFree(buf as *const _) };
            }
            // NERR_GroupNotFound (2220) / ERROR_NO_SUCH_ALIAS (1376): no local group of
            // that name. A user of that name would block the group: Foreign.
            return match rc {
                2220 | 1376 => {
                    let mut u: *mut u8 = ptr::null_mut();
                    // SAFETY: as above.
                    let urc = unsafe { NetUserGetInfo(ptr::null(), w.as_ptr(), 0, &mut u) };
                    if !u.is_null() {
                        // SAFETY: NetApi buffer.
                        unsafe { NetApiBufferFree(u as *const _) };
                    }
                    Ok(if urc == 0 { GroupLookup::Foreign } else { GroupLookup::Missing })
                }
                _ => Err(format!("NetLocalGroupGetInfo failed (win32 {rc})")),
            };
        }
        // SAFETY: level-1 buffer from NetLocalGroupGetInfo.
        let comment = unsafe {
            let g = &*(buf as *const LOCALGROUP_INFO_1);
            let c = wide_str(g.lgrpi1_comment);
            NetApiBufferFree(buf as *const _);
            c
        };
        if !GROUP_COMMENT.matches(&comment) {
            return Ok(GroupLookup::Foreign);
        }
        group_sid(&w).map(GroupLookup::Ours)
    }

    fn create_group(&self, name: &str, comment: &str) -> Result<(), String> {
        use windows_sys::Win32::NetworkManagement::NetManagement::{LOCALGROUP_INFO_1, NetLocalGroupAdd};
        let mut n = wide(name);
        let mut c = wide(comment);
        let info = LOCALGROUP_INFO_1 { lgrpi1_name: n.as_mut_ptr(), lgrpi1_comment: c.as_mut_ptr() };
        let mut parm = 0u32;
        // SAFETY: level-1 struct with buffers that outlive the call.
        net("NetLocalGroupAdd", unsafe { NetLocalGroupAdd(ptr::null(), 1, (&info as *const LOCALGROUP_INFO_1).cast(), &mut parm) })
    }

    fn add_to_group(&self, group: &str, sid: &str) -> Result<(), String> {
        let g = wide(group);
        let s = Sid::parse(sid)?;
        let m = LOCALGROUP_MEMBERS_INFO_0 { lgrmi0_sid: s.0 };
        // SAFETY: one level-0 entry; the SID outlives the call.
        let rc = unsafe { NetLocalGroupAddMembers(ptr::null(), g.as_ptr(), 0, (&m as *const LOCALGROUP_MEMBERS_INFO_0).cast(), 1) };
        if rc == ERROR_MEMBER_IN_ALIAS { Ok(()) } else { net("NetLocalGroupAddMembers", rc) }
    }

    fn delete_group(&self, name: &str) -> Result<(), String> {
        use windows_sys::Win32::NetworkManagement::NetManagement::NetLocalGroupDel;
        let n = wide(name);
        // SAFETY: NUL-terminated name.
        let rc = unsafe { NetLocalGroupDel(ptr::null(), n.as_ptr()) };
        if rc == 2220 || rc == 1376 { Ok(()) } else { net("NetLocalGroupDel", rc) }
    }

    fn delete_profile(&self, sid: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::DeleteProfileW;
        if !seat_session::host_account::is_sid_string(sid) {
            return Err(format!("bad SID {sid:?}"));
        }
        let w = wide(sid);
        // SAFETY: NUL-terminated SID string; default profile path and local computer.
        if unsafe { DeleteProfileW(w.as_ptr(), ptr::null(), ptr::null()) } != 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND: the account never had a profile.
        if matches!(e.raw_os_error(), Some(2 | 3)) { Ok(()) } else { Err(format!("DeleteProfileW: {e}")) }
    }

    fn random32(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        let mut b = zeroize::Zeroizing::new([0u8; 32]);
        // SAFETY: system RNG into a 32-byte buffer.
        let st = unsafe { BCryptGenRandom(ptr::null_mut(), b.as_mut_ptr(), 32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if st < 0 { Err(format!("BCryptGenRandom failed (NTSTATUS {st:#x})")) } else { Ok(b) }
    }
}

/// [`crate::host_account::ensure_account`] for the shared account, as a member of the host
/// group `group_sid` when known.
pub fn ensure_shared(group_sid: Option<&str>) -> Result<HostAccount, String> {
    crate::host_account::ensure_account(&WinAccountOps, seat_session::host_account::ACCOUNT_NAME, PASSWORD_SECRET, group_sid)
}

/// [`crate::host_account::ensure_group`] with the real system.
pub fn ensure_group() -> Result<String, String> {
    crate::host_account::ensure_group(&WinAccountOps)
}

/// [`crate::host_account::ensure_seat`] with the real system.
pub fn ensure_seat(seat: SeatNumber, group_sid: &str) -> Result<HostAccount, String> {
    crate::host_account::ensure_seat(&WinAccountOps, seat, group_sid)
}

/// [`crate::host_account::prune`] with the real system.
pub fn prune(keep: &[SeatNumber]) -> Vec<String> {
    crate::host_account::prune(&WinAccountOps, keep)
}

/// [`crate::host_account::ensure_kiosk`] with the real system.
pub fn ensure_kiosk(seat: SeatNumber) -> Result<HostAccount, String> {
    crate::host_account::ensure_kiosk(&WinAccountOps, seat)
}

/// [`crate::host_account::disable_unused_kiosk`] with the real system (service start).
pub fn disable_unused_kiosk(keep: &[SeatNumber]) -> Vec<String> {
    crate::host_account::disable_unused_kiosk(&WinAccountOps, keep)
}

/// [`crate::host_account::remove_kiosk_user`] with the real system (manager request).
pub fn remove_kiosk_user(seat: SeatNumber, in_use: bool) -> Result<Vec<String>, String> {
    crate::host_account::remove_kiosk_user(&WinAccountOps, seat, in_use)
}

/// [`crate::host_account::remove_all`] with the real system (uninstall).
pub fn remove(remove_user_data: bool) -> Vec<String> {
    crate::host_account::remove_all(&WinAccountOps, remove_user_data)
}

/// A host account's password from the LSA private data `secret`.
pub fn password(secret: &str) -> Result<Secret, String> {
    super::lsa::get_named(secret).map_err(|e| e.to_string())?.ok_or_else(|| "host account password not stored".into())
}
