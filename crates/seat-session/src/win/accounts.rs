// SPDX-License-Identifier: GPL-3.0-only
//! Local accounts through the Net* APIs only (adapted from pulsar-split
//! `split-session::win::accounts`). Never the SAM hive.

use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    FILTER_NORMAL_ACCOUNT, LOCALGROUP_MEMBERS_INFO_0, MAX_PREFERRED_LENGTH, NetApiBufferFree, NetLocalGroupGetMembers, NetUserEnum,
    NetUserGetInfo, UF_ACCOUNTDISABLE, USER_INFO_20, USER_INFO_23,
};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE};

use super::{from_wide_ptr, wide};
use crate::accounts::{ADMINISTRATORS_SID, Accounts, LocalUser, REMOTE_DESKTOP_USERS_SID, pickable};
use crate::error::SysError;

const ERROR_MORE_DATA: u32 = 234;

fn sid_to_string(sid: PSID) -> Result<String, SysError> {
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID; string freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return Err(SysError::last_win32("ConvertSidToStringSidW"));
        }
        let out = from_wide_ptr(s);
        LocalFree(s as HLOCAL);
        Ok(out)
    }
}

/// A SID from its string form, freed on drop.
struct OwnedSid(PSID);

impl OwnedSid {
    fn parse(s: &str) -> Result<Self, SysError> {
        let w = wide(s);
        let mut p: PSID = std::ptr::null_mut();
        // SAFETY: valid string; freed with LocalFree on drop.
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
            return Err(SysError::last_win32("ConvertStringSidToSidW"));
        }
        Ok(Self(p))
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSidToSidW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Localised name of a well-known group SID (e.g. "Yöneticiler").
fn group_name(sid: &str) -> Result<String, SysError> {
    let s = OwnedSid::parse(sid)?;
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut n, mut d) = (name.len() as u32, domain.len() as u32);
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers and lengths match.
    if unsafe { LookupAccountSidW(std::ptr::null(), s.0, name.as_mut_ptr(), &mut n, domain.as_mut_ptr(), &mut d, &mut use_) } == 0 {
        return Err(SysError::last_win32("LookupAccountSidW"));
    }
    Ok(String::from_utf16_lossy(&name[..n as usize]))
}

fn group_member_sids(group_sid: &str) -> Result<Vec<String>, SysError> {
    let group = wide(&group_name(group_sid)?);
    let mut buf: *mut u8 = std::ptr::null_mut();
    let (mut read, mut total) = (0u32, 0u32);
    let mut resume = 0usize;
    // SAFETY: level 0 returns LOCALGROUP_MEMBERS_INFO_0 rows; freed below.
    let rc = unsafe { NetLocalGroupGetMembers(std::ptr::null(), group.as_ptr(), 0, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total, &mut resume) };
    if rc != 0 {
        if !buf.is_null() {
            // SAFETY: allocated by the API.
            unsafe { NetApiBufferFree(buf.cast()) };
        }
        return Err(SysError::Win32 { api: "NetLocalGroupGetMembers", code: rc });
    }
    // SAFETY: `read` rows of LOCALGROUP_MEMBERS_INFO_0.
    let rows = unsafe { std::slice::from_raw_parts(buf.cast::<LOCALGROUP_MEMBERS_INFO_0>(), read as usize) };
    let out = rows.iter().filter_map(|r| sid_to_string(r.lgrmi0_sid).ok()).collect();
    // SAFETY: allocated by the API.
    unsafe { NetApiBufferFree(buf.cast()) };
    Ok(out)
}

fn user_sid(name: &str) -> Result<String, SysError> {
    let n = wide(name);
    let mut buf: *mut u8 = std::ptr::null_mut();
    // SAFETY: level 23; freed below.
    let rc = unsafe { NetUserGetInfo(std::ptr::null(), n.as_ptr(), 23, &mut buf) };
    if rc != 0 {
        return Err(SysError::Win32 { api: "NetUserGetInfo", code: rc });
    }
    // SAFETY: valid USER_INFO_23 from the API.
    let r = sid_to_string(unsafe { (*buf.cast::<USER_INFO_23>()).usri23_user_sid });
    // SAFETY: allocated by the API.
    unsafe { NetApiBufferFree(buf.cast()) };
    r
}

/// Real accounts (read-only).
#[derive(Debug, Clone, Copy, Default)]
pub struct NetAccounts;

impl Accounts for NetAccounts {
    fn list_local_users(&self) -> Result<Vec<LocalUser>, SysError> {
        let admins = group_member_sids(ADMINISTRATORS_SID).unwrap_or_default();
        let mut out = Vec::new();
        let mut resume = 0u32;
        loop {
            let mut buf: *mut u8 = std::ptr::null_mut();
            let (mut read, mut total) = (0u32, 0u32);
            // SAFETY: level 20 rows; freed below.
            let rc = unsafe {
                NetUserEnum(std::ptr::null(), 20, FILTER_NORMAL_ACCOUNT, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total, &mut resume)
            };
            if rc != 0 && rc != ERROR_MORE_DATA {
                if !buf.is_null() {
                    // SAFETY: allocated by the API.
                    unsafe { NetApiBufferFree(buf.cast()) };
                }
                return Err(SysError::Win32 { api: "NetUserEnum", code: rc });
            }
            // SAFETY: `read` rows of USER_INFO_20.
            let rows = unsafe { std::slice::from_raw_parts(buf.cast::<USER_INFO_20>(), read as usize) };
            for r in rows {
                // SAFETY: NUL-terminated strings owned by the buffer.
                let name = unsafe { from_wide_ptr(r.usri20_name) };
                // SAFETY: as above.
                let full_name = unsafe { from_wide_ptr(r.usri20_full_name) };
                let sid = user_sid(&name).unwrap_or_default();
                out.push(LocalUser {
                    is_admin: admins.iter().any(|a| a.eq_ignore_ascii_case(&sid)),
                    disabled: r.usri20_flags & UF_ACCOUNTDISABLE != 0,
                    name,
                    full_name,
                    sid,
                });
            }
            // SAFETY: allocated by the API.
            unsafe { NetApiBufferFree(buf.cast()) };
            if rc != ERROR_MORE_DATA {
                break;
            }
        }
        Ok(pickable(out))
    }

    /// Direct members only (nested groups are not expanded).
    fn may_use_remote_desktop(&self, user: &str) -> Result<bool, SysError> {
        let sid = user_sid(user)?;
        for g in [REMOTE_DESKTOP_USERS_SID, ADMINISTRATORS_SID] {
            if group_member_sids(g)?.iter().any(|m| m.eq_ignore_ascii_case(&sid)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_local_users_readonly() {
        // Read-only Net* query; any user may enumerate local accounts.
        let users = NetAccounts.list_local_users().unwrap();
        assert!(users.iter().all(|u| !u.name.is_empty()));
    }
}
