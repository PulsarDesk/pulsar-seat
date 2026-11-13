// SPDX-License-Identifier: GPL-3.0-only
//! Security descriptors from SDDL, folder ACLs, and token queries (user SID, session,
//! Administrators membership, logon SID). Adapted from pulsar-split
//! (`split-service::security`), extended with token helpers.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GetLastError,
    HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, ProgressInvokeNever,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo, TreeResetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, CheckTokenMembership, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorDacl,
    GetSecurityDescriptorOwner, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE,
    TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER, TokenGroups, TokenSessionId, TokenUser, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, MoveFileExW, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// NUL-terminated UTF-16 pointer to a String.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// UTF-16 buffer (possibly NUL-terminated early) to a String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Owned self-relative security descriptor allocated by `LocalAlloc`.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is immutable after creation and only read by the kernel.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid NUL-terminated string, out pointer is valid.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    /// Owner SID (null when the SDDL has none) and DACL, pointing into this descriptor.
    pub fn owner_and_dacl(&self) -> io::Result<(PSID, *mut ACL)> {
        let (mut owner, mut dacl): (PSID, *mut ACL) = (ptr::null_mut(), ptr::null_mut());
        let (mut defaulted, mut present) = (0, 0);
        // SAFETY: valid descriptor; out pointers valid.
        unsafe {
            if GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) == 0
                || GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((owner, if present != 0 { dacl } else { ptr::null_mut() }))
    }

    /// `SECURITY_ATTRIBUTES` borrowing this descriptor (no handle inheritance).
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Owned kernel handle (closed on drop).
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owned handle from CreateFileW.
        unsafe { CloseHandle(self.0) };
    }
}

/// Open a path **without following a reparse point** (the link itself is opened).
fn open_no_follow(path: &Path, access: u32) -> io::Result<Handle> {
    let w = wide_path(path);
    // SAFETY: valid NUL-terminated path; no template, no security attributes.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(h))
}

fn file_info(h: &Handle) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: zeroed struct is a valid out buffer.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: open handle, valid out pointer.
    if unsafe { GetFileInformationByHandle(h.0, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// Owner of the object is SYSTEM, Administrators, or `extra` (the owner we set).
fn owner_trusted(h: &Handle, extra: PSID) -> io::Result<bool> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: handle opened with READ_CONTROL; out pointers valid; `sd` freed below.
    let rc = unsafe {
        GetSecurityInfo(
            h.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    // SAFETY: `owner` points into `sd`, which stays alive until LocalFree below.
    let trusted = unsafe {
        !owner.is_null()
            && (IsWellKnownSid(owner, WinLocalSystemSid) != 0
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
                || (!extra.is_null() && EqualSid(owner, extra) != 0))
    };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd as HLOCAL) };
    Ok(trusted)
}

/// Rename an untrusted object out of the way (`<name>.untrusted-<pid>-<n>`). A rename
/// moves a junction / symbolic link itself, never its target.
fn move_aside(path: &Path) -> io::Result<PathBuf> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".untrusted-{}-{n}", std::process::id()));
    let to = PathBuf::from(name);
    let (a, b) = (wide_path(path), wide_path(&to));
    // SAFETY: valid NUL-terminated paths.
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    crate::log::warn(&format!("untrusted object moved aside: {} -> {}", path.display(), to.display()));
    Ok(to)
}

fn is_access_denied(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
}

/// Create or take over a data folder (ProgramData squatting / reparse points).
///
/// - A new folder is created with `CreateDirectoryW` and the descriptor from `sddl`
///   (owner + protected DACL), so there is no window with inherited ACLs.
/// - An existing folder is opened without following reparse points. A junction / symbolic
///   link, a non-directory, a folder whose owner is not SYSTEM / Administrators / the
///   `sddl` owner, or one the service cannot open for `WRITE_DAC` is renamed aside and a
///   fresh folder is created.
/// - A trusted existing folder gets the owner and the protected DACL set by handle; with
///   `reset_children`, every child's explicit ACL is replaced by inheritance
///   (`TreeResetNamedSecurityInfoW`).
///
/// The parent must already be trusted (ProgramData, or a folder made by this function).
pub fn ensure_dir(path: &Path, sddl: &str, reset_children: bool) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    let (owner, dacl) = sd.owner_and_dacl()?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let w = wide_path(path);
    let mut access = READ_CONTROL | WRITE_DAC;
    let mut info_flags = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    if !owner.is_null() {
        access |= WRITE_OWNER;
        info_flags |= OWNER_SECURITY_INFORMATION;
    }
    for attempt in 0..3 {
        let sa = sd.attributes();
        // SAFETY: valid path and security attributes that outlive the call.
        if unsafe { CreateDirectoryW(w.as_ptr(), &sa) } != 0 {
            return Ok(());
        }
        // SAFETY: trivially safe.
        let e = unsafe { GetLastError() };
        if e != ERROR_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(e as i32));
        }
        let h = match open_no_follow(path, access) {
            Ok(h) => Some(h),
            Err(e) if is_access_denied(&e) => None,
            Err(e) => return Err(e),
        };
        let trusted = match &h {
            None => false,
            Some(h) => {
                let info = file_info(h)?;
                info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
                    && info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
                    && owner_trusted(h, owner)?
            }
        };
        let Some(h) = h.filter(|_| trusted) else {
            if attempt == 2 {
                return Err(io::Error::other(format!("{} keeps coming back untrusted", path.display())));
            }
            move_aside(path)?;
            continue;
        };
        // SAFETY: handle opened with WRITE_DAC (+ WRITE_OWNER); SIDs / ACL point into `sd`.
        let rc = unsafe { SetSecurityInfo(h.0, SE_FILE_OBJECT, info_flags, owner, ptr::null_mut(), dacl, ptr::null()) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        drop(h);
        if reset_children {
            // SAFETY: valid path; ACL points into `sd`; no progress callback.
            let rc = unsafe {
                TreeResetNamedSecurityInfoW(
                    w.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    dacl,
                    ptr::null(),
                    0,
                    None,
                    ProgressInvokeNever,
                    ptr::null(),
                )
            };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
        }
        return Ok(());
    }
    Err(io::Error::other("unreachable"))
}

/// A file the SYSTEM service reads or appends to must be a plain file with one link: a
/// reparse point, a hard link (`nNumberOfLinks > 1`), a folder, or an object the service
/// cannot open is renamed aside. Returns whether something was moved. Call it after the
/// folder was secured with [`ensure_dir`] (nobody else can add links there then).
pub fn sanitize_file(path: &Path) -> io::Result<bool> {
    let bad = match open_no_follow(path, READ_CONTROL) {
        Err(e) if matches!(e.raw_os_error(), Some(c) if c == ERROR_FILE_NOT_FOUND as i32 || c == ERROR_PATH_NOT_FOUND as i32) => {
            return Ok(false);
        }
        Err(e) if is_access_denied(&e) => true,
        Err(e) => return Err(e),
        Ok(h) => {
            let info = file_info(&h)?;
            info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 || info.nNumberOfLinks > 1
        }
    };
    if bad {
        move_aside(path)?;
    }
    Ok(bad)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut s: *mut u16 = ptr::null_mut();
    // SAFETY: sid is a valid SID.
    if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: NUL-terminated string allocated by the system, freed below.
    let out = unsafe { from_wide_ptr(s) };
    // SAFETY: allocated with LocalAlloc by the system.
    unsafe { LocalFree(s as HLOCAL) };
    Ok(out)
}

/// Token information as a byte buffer (8-byte aligned).
fn token_info(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer.
    unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes, aligned for the structures.
    if unsafe { GetTokenInformation(token, class, buf.as_mut_ptr() as *mut c_void, len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

pub fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let buf = token_info(token, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into the buffer.
    let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_string(tu.User.Sid)
}

pub fn token_session_id(token: HANDLE) -> io::Result<u32> {
    let buf = token_info(token, TokenSessionId)?;
    Ok(buf[0] as u32)
}

/// Logon SID (`S-1-5-5-X-Y`) of an interactive token.
pub fn token_logon_sid(token: HANDLE) -> io::Result<Option<String>> {
    let buf = token_info(token, TokenGroups)?;
    // SAFETY: the buffer holds a TOKEN_GROUPS with GroupCount entries.
    unsafe {
        let tg = &*(buf.as_ptr() as *const TOKEN_GROUPS);
        let groups = std::slice::from_raw_parts(tg.Groups.as_ptr(), tg.GroupCount as usize);
        for g in groups {
            if g.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32 {
                return sid_string(g.Sid).map(Some);
            }
        }
    }
    Ok(None)
}

/// Administrators enabled in the calling thread's impersonation token (or `token`).
pub fn token_is_admin(token: HANDLE) -> bool {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    // SAFETY: buffer large enough for any SID.
    if unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, ptr::null_mut(), sid.as_mut_ptr() as PSID, &mut size) } == 0 {
        return false;
    }
    let mut member = 0;
    // SAFETY: token is an impersonation token (or null = thread token), SID valid.
    let ok = unsafe { CheckTokenMembership(token, sid.as_mut_ptr() as PSID, &mut member) };
    ok != 0 && member != 0
}

/// String SID of the user running this process (for the `--allow-current-user` dev flag).
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: pseudo handle of our own process; out pointer valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let r = token_user_sid(token);
    // SAFETY: token opened above.
    unsafe { CloseHandle(token) };
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_parses() {
        assert!(SecurityDescriptor::from_sddl(seat_proto::PIPE_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl(seat_core::consts::DATA_DIR_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl(seat_core::consts::PRIVATE_DIR_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl(crate::win::scm::SERVICE_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl("garbage(").is_err());
    }

    #[test]
    fn current_sid_looks_like_a_sid() {
        assert!(current_user_sid().unwrap().starts_with("S-1-"));
    }

    fn test_sddl() -> String {
        let sid = current_user_sid().unwrap();
        // Owner = ourselves (a non-elevated test cannot set BA); keep us in the DACL so the
        // test can clean up.
        format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})")
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("seat-service-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn folder_acl_applies_new_and_existing() {
        let dir = tmp("acl");
        ensure_dir(&dir, &test_sddl(), false).unwrap();
        std::fs::write(dir.join("x.txt"), "1").unwrap();
        // Existing trusted folder: re-applied in place, children reset, contents kept.
        ensure_dir(&dir, &test_sddl(), true).unwrap();
        assert!(dir.join("x.txt").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn junction_is_moved_aside_not_followed() {
        let base = tmp("junction");
        std::fs::create_dir_all(base.join("target")).unwrap();
        std::fs::write(base.join("target").join("keep.txt"), "x").unwrap();
        let link = base.join("link");
        // `mklink /J` needs no privilege.
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(base.join("target"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("mklink /J not available; skipped");
            return;
        }
        ensure_dir(&link, &test_sddl(), true).unwrap();
        // `link` is now a real, empty folder; the target is untouched.
        let info = std::fs::symlink_metadata(&link).unwrap();
        assert!(info.is_dir() && !info.file_type().is_symlink());
        assert!(std::fs::read_dir(&link).unwrap().next().is_none());
        assert!(base.join("target").join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn hard_links_and_missing_files() {
        let dir = tmp("hardlink");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!sanitize_file(&dir.join("none.json")).unwrap());
        std::fs::write(dir.join("plain.json"), "{}").unwrap();
        assert!(!sanitize_file(&dir.join("plain.json")).unwrap());
        std::fs::write(dir.join("outside.txt"), "secret").unwrap();
        std::fs::hard_link(dir.join("outside.txt"), dir.join("state.json")).unwrap();
        assert!(sanitize_file(&dir.join("state.json")).unwrap());
        assert!(!dir.join("state.json").exists());
        assert_eq!(std::fs::read_to_string(dir.join("outside.txt")).unwrap(), "secret");
        let _ = std::fs::remove_dir_all(dir);
    }
}
