// SPDX-License-Identifier: GPL-3.0-only
//! Local accounts through the Net* APIs only (spec 30 §4). Never the SAM hive.

use super::{check, from_wide_ptr, wide, wide_secret};
use crate::SessionError;
use crate::users::{ADMINISTRATORS_SID, Accounts, Profiles, REMOTE_DESKTOP_USERS_SID};
use seat_core::remote::validate::UserFacts;
use seat_proto::remote::Secret;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    LOCALGROUP_MEMBERS_INFO_0, MAX_PREFERRED_LENGTH, NERR_UserNotFound, NetApiBufferFree, NetLocalGroupAddMembers, NetLocalGroupGetMembers,
    NetUserAdd, NetUserGetInfo, NetUserSetInfo, UF_ACCOUNTDISABLE, UF_LOCKOUT, UF_NORMAL_ACCOUNT, UF_SCRIPT, USER_INFO_1, USER_INFO_4,
    USER_INFO_1003, USER_INFO_1008, USER_PRIV_USER,
};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::{
    LOGON32_LOGON_INTERACTIVE, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW, LookupAccountSidW, PSID, SID_NAME_USE,
};
use windows_sys::Win32::UI::Shell::{LoadUserProfileW, PROFILEINFOW, UnloadUserProfile};

const ERROR_MEMBER_IN_ALIAS: u32 = 1378;
const ERROR_LOGON_FAILURE: u32 = 1326;
const ERROR_PASSWORD_EXPIRED: u32 = 1330;
const ERROR_PASSWORD_MUST_CHANGE: u32 = 1907;
const ERROR_ACCOUNT_DISABLED: u32 = 1331;
const ERROR_ACCOUNT_LOCKED_OUT: u32 = 1909;
const ERROR_LOGON_TYPE_NOT_GRANTED: u32 = 1385;
const PI_NOUI: u32 = 1;

/// `NetUserGetInfo` level 4 result, freed on drop.
struct UserInfo4(*mut USER_INFO_4);

impl Drop for UserInfo4 {
    fn drop(&mut self) {
        // SAFETY: allocated by NetUserGetInfo.
        unsafe { NetApiBufferFree(self.0.cast()) };
    }
}

fn user_info4(user: &str) -> Result<Option<UserInfo4>, SessionError> {
    let name = wide(user);
    let mut buf: *mut u8 = std::ptr::null_mut();
    // SAFETY: local server; buffer freed by UserInfo4.
    let rc = unsafe { NetUserGetInfo(std::ptr::null(), name.as_ptr(), 4, &mut buf) };
    match rc {
        0 => Ok(Some(UserInfo4(buf.cast()))),
        rc if rc == NERR_UserNotFound => Ok(None),
        rc => Err(SessionError::Win32 { api: "NetUserGetInfo", code: rc }),
    }
}

fn sid_to_string(sid: PSID) -> Result<String, SessionError> {
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID; string freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return Err(SessionError::last_win32("ConvertSidToStringSidW"));
        }
        let out = from_wide_ptr(s);
        LocalFree(s as HLOCAL);
        Ok(out)
    }
}

/// A SID from its string form, freed on drop.
struct OwnedSid(PSID);

impl OwnedSid {
    fn parse(s: &str) -> Result<Self, SessionError> {
        let w = wide(s);
        let mut p: PSID = std::ptr::null_mut();
        // SAFETY: valid string; freed with LocalFree on drop.
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
            return Err(SessionError::last_win32("ConvertStringSidToSidW"));
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

/// Localised group name of a well-known group SID (e.g. "Uzak Masaüstü Kullanıcıları").
fn group_name(sid: &str) -> Result<String, SessionError> {
    let s = OwnedSid::parse(sid)?;
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut n, mut d) = (name.len() as u32, domain.len() as u32);
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers and lengths match.
    if unsafe { LookupAccountSidW(std::ptr::null(), s.0, name.as_mut_ptr(), &mut n, domain.as_mut_ptr(), &mut d, &mut use_) } == 0 {
        return Err(SessionError::last_win32("LookupAccountSidW"));
    }
    Ok(String::from_utf16_lossy(&name[..n as usize]))
}

fn group_member_sids(group_sid: &str) -> Result<Vec<String>, SessionError> {
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
        return Err(SessionError::Win32 { api: "NetLocalGroupGetMembers", code: rc });
    }
    // SAFETY: `read` rows of LOCALGROUP_MEMBERS_INFO_0.
    let rows = unsafe { std::slice::from_raw_parts(buf.cast::<LOCALGROUP_MEMBERS_INFO_0>(), read as usize) };
    let out = rows.iter().filter_map(|r| sid_to_string(r.lgrmi0_sid).ok()).collect();
    // SAFETY: allocated by the API.
    unsafe { NetApiBufferFree(buf.cast()) };
    Ok(out)
}

/// Real accounts (needs SYSTEM or an administrator for the write calls).
#[derive(Debug, Clone, Copy, Default)]
pub struct NetAccounts;

impl NetAccounts {
    fn in_group(&self, user: &str, group_sid: &str) -> Result<bool, SessionError> {
        let sid = self.sid(user)?;
        Ok(group_member_sids(group_sid)?.iter().any(|m| m.eq_ignore_ascii_case(&sid)))
    }
}

impl Accounts for NetAccounts {
    fn facts(&self, user: &str) -> Result<UserFacts, SessionError> {
        let Some(info) = user_info4(user)? else { return Ok(UserFacts::default()) };
        // SAFETY: valid USER_INFO_4 from the API.
        let i = unsafe { &*info.0 };
        Ok(UserFacts {
            exists: true,
            enabled: i.usri4_flags & UF_ACCOUNTDISABLE == 0,
            locked_out: i.usri4_flags & UF_LOCKOUT != 0,
            password_expired: i.usri4_password_expired != 0,
        })
    }

    fn sid(&self, user: &str) -> Result<String, SessionError> {
        let info = user_info4(user)?.ok_or_else(|| SessionError::Account("user does not exist".into()))?;
        // SAFETY: valid USER_INFO_4.
        sid_to_string(unsafe { (*info.0).usri4_user_sid })
    }

    fn create(&self, user: &str, password: &Secret, comment: &str) -> Result<(), SessionError> {
        let mut name = wide(user);
        let mut pw = wide_secret(&password.0);
        let mut cm = wide(comment);
        let info = USER_INFO_1 {
            usri1_name: name.as_mut_ptr(),
            usri1_password: pw.as_mut_ptr(),
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: std::ptr::null_mut(),
            usri1_comment: cm.as_mut_ptr(),
            usri1_flags: UF_SCRIPT | UF_NORMAL_ACCOUNT,
            usri1_script_path: std::ptr::null_mut(),
        };
        let mut parm = 0u32;
        // SAFETY: level-1 struct with valid strings for the call's duration.
        let rc = unsafe { NetUserAdd(std::ptr::null(), 1, (&info as *const USER_INFO_1).cast(), &mut parm) };
        check(rc, "NetUserAdd")
    }

    fn set_password(&self, user: &str, password: &Secret) -> Result<(), SessionError> {
        let name = wide(user);
        let mut pw = wide_secret(&password.0);
        let info = USER_INFO_1003 { usri1003_password: pw.as_mut_ptr() };
        let mut parm = 0u32;
        // SAFETY: level-1003 struct valid for the call.
        let rc = unsafe { NetUserSetInfo(std::ptr::null(), name.as_ptr(), 1003, (&info as *const USER_INFO_1003).cast(), &mut parm) };
        check(rc, "NetUserSetInfo")
    }

    fn in_remote_desktop_users(&self, user: &str) -> Result<bool, SessionError> {
        self.in_group(user, REMOTE_DESKTOP_USERS_SID)
    }

    fn add_to_remote_desktop_users(&self, user: &str) -> Result<(), SessionError> {
        let sid = OwnedSid::parse(&self.sid(user)?)?;
        let group = wide(&group_name(REMOTE_DESKTOP_USERS_SID)?);
        let m = LOCALGROUP_MEMBERS_INFO_0 { lgrmi0_sid: sid.0 };
        // SAFETY: one level-0 entry.
        let rc = unsafe { NetLocalGroupAddMembers(std::ptr::null(), group.as_ptr(), 0, (&m as *const LOCALGROUP_MEMBERS_INFO_0).cast(), 1) };
        if rc == ERROR_MEMBER_IN_ALIAS { Ok(()) } else { check(rc, "NetLocalGroupAddMembers") }
    }

    /// Direct members only (nested groups are not expanded).
    fn is_admin(&self, user: &str) -> Result<bool, SessionError> {
        self.in_group(user, ADMINISTRATORS_SID)
    }
}

/// Profile creation inside the service: `LogonUserW` + `LoadUserProfileW`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProfiles;

fn logon(user: &str, password: &Secret, kind: u32) -> Result<HANDLE, u32> {
    let u = wide(user);
    let dot = wide(".");
    let pw = wide_secret(&password.0);
    let mut tok: HANDLE = std::ptr::null_mut();
    // SAFETY: valid strings; token closed by the caller.
    if unsafe { LogonUserW(u.as_ptr(), dot.as_ptr(), pw.as_ptr(), kind, LOGON32_PROVIDER_DEFAULT, &mut tok) } == 0 {
        // SAFETY: no preconditions.
        return Err(unsafe { GetLastError() });
    }
    Ok(tok)
}

impl Profiles for WindowsProfiles {
    fn ensure_profile(&self, user: &str, password: &Secret) -> Result<(), SessionError> {
        // Interactive first; seat users may be denied local logon (spec 30 §10), then a
        // network logon token is used. HYPOTHESIS: LoadUserProfileW creates the profile
        // from a network logon token as well; verify on the test host.
        let tok = match logon(user, password, LOGON32_LOGON_INTERACTIVE) {
            Ok(t) => t,
            Err(ERROR_LOGON_TYPE_NOT_GRANTED) => logon(user, password, LOGON32_LOGON_NETWORK).map_err(map_logon)?,
            Err(c) => return Err(map_logon(c)),
        };
        let mut name = wide(user);
        let mut pi = PROFILEINFOW {
            dwSize: std::mem::size_of::<PROFILEINFOW>() as u32,
            dwFlags: PI_NOUI,
            lpUserName: name.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: token from LogonUserW; profile unloaded and token closed below.
        unsafe {
            let r = if LoadUserProfileW(tok, &mut pi) == 0 {
                Err(SessionError::Profile(format!("LoadUserProfileW failed with {}", GetLastError())))
            } else {
                UnloadUserProfile(tok, pi.hProfile);
                Ok(())
            };
            CloseHandle(tok);
            r
        }
    }
}

/// Credential test (spec 10 §4.4): a network logon against the local machine validates the
/// password without needing a logon right or loading a profile. `Err(win32 code)` on
/// failure (1326 wrong password, 1331 disabled, 1909 locked out, 1330 / 1907 expired).
/// Nothing is changed on the account.
pub fn test_logon(user: &str, password: &Secret) -> Result<(), u32> {
    let tok = logon(user, password, LOGON32_LOGON_NETWORK)?;
    // SAFETY: token from LogonUserW, owned here.
    unsafe { CloseHandle(tok) };
    Ok(())
}

/// Clears `UF_ACCOUNTDISABLE` (`enable`) and / or `UF_LOCKOUT` (`unlock`). Called only after
/// the operator confirmed in the manager (spec 10 §4.4: never as a silent side effect).
pub fn enable_unlock(user: &str, enable: bool, unlock: bool) -> Result<(), SessionError> {
    let Some(info) = user_info4(user)? else { return Err(SessionError::Account("user does not exist".into())) };
    // SAFETY: valid USER_INFO_4 from the API.
    let mut flags = unsafe { (*info.0).usri4_flags };
    if enable {
        flags &= !UF_ACCOUNTDISABLE;
    }
    if unlock {
        flags &= !UF_LOCKOUT;
    }
    let name = wide(user);
    let set = USER_INFO_1008 { usri1008_flags: flags };
    let mut parm = 0u32;
    // SAFETY: level-1008 struct valid for the call.
    let rc = unsafe { NetUserSetInfo(std::ptr::null(), name.as_ptr(), 1008, (&set as *const USER_INFO_1008).cast(), &mut parm) };
    check(rc, "NetUserSetInfo")
}

fn map_logon(code: u32) -> SessionError {
    match code {
        ERROR_LOGON_FAILURE => SessionError::Account("the stored password does not match the account".into()),
        ERROR_PASSWORD_EXPIRED | ERROR_PASSWORD_MUST_CHANGE => SessionError::PasswordExpired,
        ERROR_ACCOUNT_DISABLED => SessionError::Account("user account is disabled".into()),
        ERROR_ACCOUNT_LOCKED_OUT => SessionError::Account("user account is locked out".into()),
        c => SessionError::Profile(format!("LogonUserW failed with {c}")),
    }
}
