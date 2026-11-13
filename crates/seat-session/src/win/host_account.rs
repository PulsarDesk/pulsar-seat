// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of [`crate::host_account`]: the contained host token (batch logon of the
//! host account + `hardened_primary` + session id), the token facts of the start-up check,
//! and the window-station / desktop DACL edit run by the `--desktop-acl` helper inside the
//! target session. Documented Win32 only (`LogonUserW`, `CreateRestrictedToken`,
//! `GetSecurityInfo` / `SetEntriesInAclW` / `SetSecurityInfo`, `OpenWindowStationW`,
//! `OpenDesktopW`). **Untested on hardware**; the DACL edit and the token reads are tested
//! here on a temp file and the test process's own token.

use std::os::windows::io::OwnedHandle;

use seat_core::Secret;
use windows_sys::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS,
    SE_OBJECT_TYPE, SE_WINDOW_OBJECT, SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, LOGON32_LOGON_BATCH, LOGON32_PROVIDER_DEFAULT, LogonUserW, NO_INHERITANCE,
    PSECURITY_DESCRIPTOR, PSID, SE_PRIVILEGE_ENABLED, SetTokenInformation,
    TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_PRIVILEGES, TOKEN_USER, TokenGroups, TokenIntegrityLevel,
    TokenPrivileges, TokenSessionId, TokenUser,
};

/// Group attribute bits (winnt.h).
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x0000_0010;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

use super::{owned, raw, wide, wide_secret};
use crate::error::SysError;
use crate::host_account::{AclOp, DESKTOP_ACCESS, TokenFacts, host_default_dacl_sddl, is_sid_string, winsta_access};

/// A contained host token for `session_id` and the logon SID that has to be granted on the
/// session's window station and desktop.
pub struct HostToken {
    pub token: OwnedHandle,
    pub logon_sid: String,
}

/// Batch logon of the host account, hardened (no privileges, Administrators deny-only, Low
/// integrity) and moved into `session_id` (needs SeTcbPrivilege: the service).
pub fn account_token_in_session(account: &str, password: &Secret, session_id: u32) -> Result<HostToken, SysError> {
    let user = wide(account);
    let dom = wide(".");
    let pw = wide_secret(password.expose());
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: NUL-terminated strings; the password buffer is wiped on drop.
    if unsafe { LogonUserW(user.as_ptr(), dom.as_ptr(), pw.as_ptr(), LOGON32_LOGON_BATCH, LOGON32_PROVIDER_DEFAULT, &mut h) } == 0 {
        return Err(SysError::last_win32("LogonUserW"));
    }
    drop(pw);
    let logon = owned(h, "LogonUserW")?;
    let token = super::launch::hardened_primary(raw(&logon))?;
    // SAFETY: 4-byte session id.
    if unsafe { SetTokenInformation(raw(&token), TokenSessionId, (&session_id as *const u32).cast(), 4) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenSessionId)"));
    }
    let logon_sid = token_logon_sid(raw(&token))?.ok_or_else(|| SysError::Other("batch token without a logon SID".into()))?;
    // Every host shares the account SID at Low integrity: without this the batch logon's
    // default DACL (account SID + SYSTEM full access) would let one host open the threads
    // and objects of another.
    set_host_default_dacl(raw(&token), &logon_sid)?;
    Ok(HostToken { token, logon_sid })
}

/// Set the token's default DACL to [`host_default_dacl_sddl`] (SYSTEM + `logon_sid` full
/// access, Owner Rights `READ_CONTROL`, nothing for the user SID). Needs
/// `TOKEN_ADJUST_DEFAULT` on `token`.
pub(crate) fn set_host_default_dacl(token: HANDLE, logon_sid: &str) -> Result<(), SysError> {
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, TOKEN_DEFAULT_DACL, TokenDefaultDacl};
    let sddl = host_default_dacl_sddl(logon_sid).ok_or_else(|| SysError::Other(format!("bad logon SID {logon_sid:?}")))?;
    let w = wide(&sddl);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: NUL-terminated SDDL; descriptor freed by LocalMem.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
        return Err(SysError::last_win32("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
    }
    let _sd_mem = LocalMem(sd);
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: valid descriptor; `dacl` points into it.
    if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 || dacl.is_null() {
        return Err(SysError::Other("default DACL descriptor without a DACL".into()));
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
    // SAFETY: TOKEN_DEFAULT_DACL whose ACL outlives the call (the token copies it).
    if unsafe { SetTokenInformation(token, TokenDefaultDacl, (&info as *const TOKEN_DEFAULT_DACL).cast(), std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenDefaultDacl)"));
    }
    Ok(())
}

/// Token information in pointer-aligned storage. SID pointers inside the structure point
/// into this same buffer, so the buffer must outlive every use of them.
struct TokenInfo(Vec<u64>);

impl TokenInfo {
    fn ptr<T>(&self) -> *const T {
        self.0.as_ptr().cast()
    }
}

fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<TokenInfo, SysError> {
    let mut len = 0u32;
    // SAFETY: size query.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(SysError::last_win32("GetTokenInformation"));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes.
    if unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(SysError::last_win32("GetTokenInformation"));
    }
    Ok(TokenInfo(buf))
}

fn sid_string(sid: PSID) -> Option<String> {
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID; string freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return None;
        }
        let out = super::from_wide_ptr(s);
        LocalFree(s as HLOCAL);
        Some(out)
    }
}

/// Groups of a token (the slice borrows `info`).
fn groups(info: &TokenInfo) -> &[windows_sys::Win32::Security::SID_AND_ATTRIBUTES] {
    // SAFETY: TOKEN_GROUPS written by GetTokenInformation; GroupCount entries follow.
    unsafe {
        let tg = &*info.ptr::<TOKEN_GROUPS>();
        std::slice::from_raw_parts(tg.Groups.as_ptr(), tg.GroupCount as usize)
    }
}

/// Logon SID (`S-1-5-5-X-Y`) of a token, if it has one.
pub fn token_logon_sid(token: HANDLE) -> Result<Option<String>, SysError> {
    let info = token_info(token, TokenGroups)?;
    Ok(groups(&info).iter().find(|g| g.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID).and_then(|g| sid_string(g.Sid)))
}

/// What [`crate::host_account::check_token`] needs from `token`.
pub fn token_facts(token: HANDLE) -> Result<TokenFacts, SysError> {
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, EqualSid, GetSidSubAuthority, GetSidSubAuthorityCount, LookupPrivilegeNameW, SECURITY_MAX_SID_SIZE,
        WinBuiltinAdministratorsSid,
    };
    let mut facts = TokenFacts::default();
    let user = token_info(token, TokenUser)?;
    // SAFETY: TOKEN_USER written by GetTokenInformation.
    facts.user_sid = sid_string(unsafe { (*user.ptr::<TOKEN_USER>()).User.Sid }).unwrap_or_default();

    let il = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: TOKEN_MANDATORY_LABEL written by GetTokenInformation; its SID is valid.
    facts.integrity_rid = unsafe {
        let sid = (*il.ptr::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
        let n = *GetSidSubAuthorityCount(sid);
        if n == 0 { 0 } else { *GetSidSubAuthority(sid, u32::from(n) - 1) }
    };

    let mut admins = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut alen = admins.len() as u32;
    // SAFETY: buffer of `alen` bytes.
    if unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, std::ptr::null_mut(), admins.as_mut_ptr().cast(), &mut alen) } == 0 {
        return Err(SysError::last_win32("CreateWellKnownSid"));
    }
    let g = token_info(token, TokenGroups)?;
    facts.admin_enabled = groups(&g).iter().any(|x| {
        // SAFETY: both SIDs valid.
        (unsafe { EqualSid(x.Sid, admins.as_mut_ptr().cast()) } != 0)
            && x.Attributes & SE_GROUP_ENABLED != 0
            && x.Attributes & SE_GROUP_USE_FOR_DENY_ONLY == 0
    });

    let privs = token_info(token, TokenPrivileges)?;
    // SAFETY: TOKEN_PRIVILEGES written by GetTokenInformation; PrivilegeCount entries follow.
    let list = unsafe {
        let tp = &*privs.ptr::<TOKEN_PRIVILEGES>();
        std::slice::from_raw_parts(tp.Privileges.as_ptr(), tp.PrivilegeCount as usize)
    };
    facts.enabled_privileges = list
        .iter()
        .filter(|l| l.Attributes & SE_PRIVILEGE_ENABLED != 0)
        .filter_map(|l| {
            let mut name = [0u16; 128];
            let mut n = name.len() as u32;
            // SAFETY: buffer of `n` elements.
            (unsafe { LookupPrivilegeNameW(std::ptr::null(), &l.Luid, name.as_mut_ptr(), &mut n) } != 0)
                .then(|| String::from_utf16_lossy(&name[..n as usize]))
        })
        .collect();
    Ok(facts)
}
/// [`token_facts`] of the current process.
pub fn own_token_facts() -> Result<TokenFacts, SysError> {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut t: HANDLE = std::ptr::null_mut();
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) } == 0 {
        return Err(SysError::last_win32("OpenProcessToken"));
    }
    let t = owned(t, "OpenProcessToken")?;
    token_facts(raw(&t))
}

struct LocalMem(*mut core::ffi::c_void);

impl Drop for LocalMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: memory from LocalAlloc-based APIs.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// Grant (`mask` for `sid`, no inheritance) or revoke (every ACE of `sid`) on the DACL of
/// the object behind `handle`. The handle needs `READ_CONTROL | WRITE_DAC`.
pub(crate) fn edit_dacl(handle: HANDLE, kind: SE_OBJECT_TYPE, sid: &str, mask: u32, op: AclOp) -> Result<(), SysError> {
    if !is_sid_string(sid) {
        return Err(SysError::Other(format!("bad SID {sid:?}")));
    }
    let ws = wide(sid);
    let mut psid: PSID = std::ptr::null_mut();
    // SAFETY: NUL-terminated SID string; freed by LocalMem.
    if unsafe { ConvertStringSidToSidW(ws.as_ptr(), &mut psid) } == 0 {
        return Err(SysError::last_win32("ConvertStringSidToSidW"));
    }
    let _sid_mem = LocalMem(psid);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: valid handle; out pointers; sd freed by LocalMem (dacl points into it).
    let rc = unsafe {
        GetSecurityInfo(handle, kind, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    if rc != 0 {
        return Err(SysError::Win32 { api: "GetSecurityInfo", code: rc });
    }
    let _sd_mem = LocalMem(sd);
    if dacl.is_null() {
        // NULL DACL = everyone allowed. SetEntriesInAclW would build a new ACL from nothing:
        // a grant would lock every other principal out, a revoke would leave an empty DACL
        // (nobody allowed). Never turn one into the other.
        return match op {
            AclOp::Revoke => Ok(()),
            AclOp::Grant => Err(SysError::Other("object has a NULL DACL; not edited".into())),
        };
    }
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: if op == AclOp::Grant { mask } else { 0 },
        grfAccessMode: if op == AclOp::Grant { GRANT_ACCESS } else { REVOKE_ACCESS },
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid.cast(),
        },
    };
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: one entry; the old DACL from GetSecurityInfo; result freed by LocalMem.
    let rc = unsafe { SetEntriesInAclW(1, &ea, dacl, &mut new_acl) };
    if rc != 0 {
        return Err(SysError::Win32 { api: "SetEntriesInAclW", code: rc });
    }
    let _acl_mem = LocalMem(new_acl.cast());
    // SAFETY: valid handle and ACL.
    let rc = unsafe { SetSecurityInfo(handle, kind, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), new_acl, std::ptr::null()) };
    if rc != 0 {
        return Err(SysError::Win32 { api: "SetSecurityInfo", code: rc });
    }
    Ok(())
}

/// The helper's work: grant / revoke `sid` on `WinSta0` and its `Default` desktop of the
/// **caller's** session. Run by the service as SYSTEM (no privileges, System integrity) in
/// the target session; SYSTEM has full access to both objects there. `clipboard` (grant
/// only) adds `WINSTA_ACCESSCLIPBOARD` to the window-station ACE.
pub fn edit_session_desktop(sid: &str, op: AclOp, clipboard: bool) -> Result<(), SysError> {
    use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, CloseWindowStation, OpenDesktopW, OpenWindowStationW};
    /// `READ_CONTROL | WRITE_DAC`.
    const ACCESS: u32 = 0x0002_0000 | 0x0004_0000;
    let ws_name = wide("WinSta0");
    // SAFETY: NUL-terminated name.
    let ws = unsafe { OpenWindowStationW(ws_name.as_ptr(), 0, ACCESS) };
    if ws.is_null() {
        return Err(SysError::last_win32("OpenWindowStationW"));
    }
    let r = edit_dacl(ws as HANDLE, SE_WINDOW_OBJECT, sid, winsta_access(clipboard), op);
    // OpenDesktopW opens in the process's window station, which for the helper is WinSta0
    // (the service starts it on `winsta0\default`).
    let dname = wide("Default");
    // SAFETY: NUL-terminated name.
    let desk = unsafe { OpenDesktopW(dname.as_ptr(), 0, 0, ACCESS) };
    let d = if desk.is_null() {
        Err(SysError::last_win32("OpenDesktopW"))
    } else {
        let d = edit_dacl(desk as HANDLE, SE_WINDOW_OBJECT, sid, DESKTOP_ACCESS, op);
        // SAFETY: handle from OpenDesktopW.
        unsafe { CloseDesktop(desk) };
        d
    };
    // SAFETY: handle from OpenWindowStationW.
    unsafe { CloseWindowStation(ws) };
    r.and(d)
}

/// The calling thread is attached to a desktop (user32 attached the process to the one the
/// service named). `false` = the desktop refused the token.
pub fn has_thread_desktop() -> bool {
    use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    // SAFETY: no preconditions.
    !unsafe { GetThreadDesktop(GetCurrentThreadId()) }.is_null()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::Authorization::{ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT};

    fn dacl_sddl(h: HANDLE) -> String {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid file handle.
        let rc = unsafe {
            GetSecurityInfo(h, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), &mut sd)
        };
        assert_eq!(rc, 0);
        let _m = LocalMem(sd);
        let mut s: *mut u16 = std::ptr::null_mut();
        // SAFETY: valid descriptor.
        assert!(unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, std::ptr::null_mut()) } != 0);
        // SAFETY: NUL-terminated string from the API.
        let out = unsafe { super::super::from_wide_ptr(s) };
        let _s = LocalMem(s.cast());
        out
    }

    #[test]
    fn grant_and_revoke_on_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("acl.bin");
        std::fs::write(&p, b"x").unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        // READ_CONTROL | WRITE_DAC.
        let f = std::fs::OpenOptions::new().access_mode(0x0002_0000 | 0x0004_0000).open(&p).unwrap();
        let h = f.as_raw_handle() as HANDLE;
        let sid = "S-1-5-5-0-987654";
        edit_dacl(h, SE_FILE_OBJECT, sid, 0x0012_0089, AclOp::Grant).unwrap();
        assert!(dacl_sddl(h).contains(sid), "{}", dacl_sddl(h));
        edit_dacl(h, SE_FILE_OBJECT, sid, 0, AclOp::Revoke).unwrap();
        assert!(!dacl_sddl(h).contains(sid));
        assert!(edit_dacl(h, SE_FILE_OBJECT, "S-1-5;(A;;GA;;;WD)", 1, AclOp::Grant).is_err());
    }

    #[test]
    fn a_null_dacl_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("null.bin");
        std::fs::write(&p, b"x").unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new().access_mode(0x0002_0000 | 0x0004_0000).open(&p).unwrap();
        let h = f.as_raw_handle() as HANDLE;
        // SAFETY: valid handle with WRITE_DAC; a null ACL = NULL DACL.
        let info = DACL_SECURITY_INFORMATION | windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
        // SAFETY: as above.
        let rc = unsafe { SetSecurityInfo(h, SE_FILE_OBJECT, info, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) };
        assert_eq!(rc, 0);
        let sid = "S-1-5-5-0-987655";
        assert!(edit_dacl(h, SE_FILE_OBJECT, sid, 0x0012_0089, AclOp::Grant).is_err());
        edit_dacl(h, SE_FILE_OBJECT, sid, 0, AclOp::Revoke).unwrap();
        // Still a NULL DACL ("D:NO_ACCESS_CONTROL"), not a one-ACE or empty one.
        let s = dacl_sddl(h);
        assert!(s.contains("NO_ACCESS_CONTROL"), "{s}");
    }

    /// SID strings of the ACEs in a token's default DACL.
    fn default_dacl_sids(token: HANDLE) -> Vec<String> {
        use windows_sys::Win32::Security::{ACCESS_ALLOWED_ACE, GetAce, TOKEN_DEFAULT_DACL, TokenDefaultDacl};
        let info = token_info(token, TokenDefaultDacl).unwrap();
        // SAFETY: TOKEN_DEFAULT_DACL written by GetTokenInformation; its ACL is in `info`.
        unsafe {
            let acl = (*info.ptr::<TOKEN_DEFAULT_DACL>()).DefaultDacl;
            assert!(!acl.is_null());
            (0..u32::from((*acl).AceCount))
                .map(|i| {
                    let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                    assert!(GetAce(acl, i, &mut ace) != 0);
                    let ace = ace as *const ACCESS_ALLOWED_ACE;
                    sid_string(std::ptr::addr_of!((*ace).SidStart) as PSID).unwrap()
                })
                .collect()
        }
    }

    #[test]
    fn host_default_dacl_leaves_out_the_shared_account() {
        use windows_sys::Win32::Security::{TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut me: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo-handle of the current process.
        assert!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut me) } != 0);
        let me = owned(me, "OpenProcessToken").unwrap();
        let t = super::super::launch::hardened_primary(raw(&me)).unwrap();
        let user = token_facts(raw(&t)).unwrap().user_sid;
        let logon = token_logon_sid(raw(&t)).unwrap().expect("interactive test logon has a logon SID");
        set_host_default_dacl(raw(&t), &logon).unwrap();
        let sids = default_dacl_sids(raw(&t));
        assert!(!sids.iter().any(|s| s == &user), "{sids:?} has the user {user}");
        assert_eq!(sids, vec!["S-1-5-18".to_string(), logon, "S-1-3-4".to_string()]);
        assert!(set_host_default_dacl(raw(&t), "S-1-5-5-0-1)(A;;GA;;;WD").is_err());
    }

    #[test]
    fn own_token_facts_read() {
        let f = own_token_facts().unwrap();
        assert!(is_sid_string(&f.user_sid), "{f:?}");
        assert!(f.integrity_rid >= 0x1000, "{f:?}");
        assert!(f.enabled_privileges.iter().any(|p| p == "SeChangeNotifyPrivilege"), "{f:?}");
    }

    #[test]
    fn hardened_own_token_passes_the_start_check() {
        use windows_sys::Win32::Security::{TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut me: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo-handle of the current process.
        assert!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut me) } != 0);
        let me = owned(me, "OpenProcessToken").unwrap();
        let t = super::super::launch::hardened_primary(raw(&me)).unwrap();
        let f = token_facts(raw(&t)).unwrap();
        assert_eq!(f.integrity_rid, 0x1000);
        assert!(!f.admin_enabled);
        crate::host_account::check_token(&f).unwrap();
        // The test runs in an interactive logon: its token has a logon SID.
        let logon = token_logon_sid(raw(&t)).unwrap();
        assert!(logon.as_deref().is_some_and(|s| s.starts_with("S-1-5-5-")), "{logon:?}");
    }
}
