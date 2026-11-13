// SPDX-License-Identifier: GPL-3.0-only
//! Writing the release secret key with a protected DACL from the first byte.
//!
//! The file is created by `CreateFileW` with a security descriptor built from
//! `D:P(A;;FA;;;<current user>)(A;;FA;;;SY)`: no inherited entry of the folder (a drive
//! root grants Authenticated Users modify) ever applies to it, and there is no window
//! between "written" and "restricted". A volume without persistent ACLs (FAT, exFAT) is
//! refused unless `--allow-unprotected` is given; after creation the DACL is read back
//! from the handle and must be protected with exactly those two entries.
//!
//! Documented Win32 APIs only. Tested by `cargo test` on the dev box (NTFS); the FAT /
//! exFAT refusal is untested on hardware.

#![allow(unsafe_code)]

use std::fs::File;
use std::path::Path;

/// How the key file was protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    /// Protected DACL: current user + SYSTEM only.
    Restricted { user_sid: String },
    /// The volume has no ACLs and `--allow-unprotected` was given.
    Unprotected,
}

/// SDDL of the key file's DACL for `user_sid`.
pub fn key_dacl_sddl(user_sid: &str) -> String {
    format!("D:P(A;;FA;;;{user_sid})(A;;FA;;;SY)")
}

/// The DACL string read back is exactly protected + current user + SYSTEM (in any order,
/// SYSTEM may be written `SY` or `S-1-5-18`).
pub fn dacl_is_restricted(sddl: &str, user_sid: &str) -> bool {
    let Some(rest) = sddl.strip_prefix("D:P") else { return false };
    // Flags after P (AI / AR) are allowed; ACEs start at the first '('.
    let Some(start) = rest.find('(') else { return false };
    if !rest[..start].chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let aces: Vec<&str> = rest[start..].trim_start_matches('(').trim_end_matches(')').split(")(").collect();
    if aces.len() != 2 {
        return false;
    }
    let mut user = false;
    let mut system = false;
    for a in aces {
        let f: Vec<&str> = a.split(';').collect();
        if f.len() != 6 || f[0] != "A" || f[2] != "FA" {
            return false;
        }
        match f[5] {
            s if s.eq_ignore_ascii_case(user_sid) => user = true,
            "SY" | "S-1-5-18" => system = true,
            _ => return false,
        }
    }
    user && system
}

#[cfg(windows)]
mod imp {
    use super::{Protection, dacl_is_restricted, key_dacl_sddl};
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, GetVolumeInformationW,
        GetVolumePathNameW, READ_CONTROL,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// GetVolumeInformationW flag: the file system keeps ACLs (winnt.h).
    const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn last() -> String {
        std::io::Error::last_os_error().to_string()
    }

    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by the system with LocalAlloc.
                unsafe { LocalFree(self.0 as HLOCAL) };
            }
        }
    }

    fn from_wide_ptr(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0;
        // SAFETY: NUL-terminated string from the system.
        while unsafe { *p.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: n elements were just read.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
    }

    /// SID string of the user this process runs as.
    pub fn current_user_sid() -> Result<String, String> {
        let mut tok: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo handle of this process; out handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
            return Err(format!("OpenProcessToken: {}", last()));
        }
        let mut buf = vec![0u64; 64]; // 512 bytes, 8-aligned: TOKEN_USER + SID
        let mut len = 0u32;
        // SAFETY: valid token, buffer of the given size.
        let ok = unsafe {
            GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len)
        };
        let err = last();
        // SAFETY: handle opened above.
        unsafe { CloseHandle(tok) };
        if ok == 0 {
            return Err(format!("GetTokenInformation: {err}"));
        }
        // SAFETY: the buffer holds a TOKEN_USER now.
        let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut s: *mut u16 = std::ptr::null_mut();
        // SAFETY: valid SID; string freed by Local.
        if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
            return Err(format!("ConvertSidToStringSidW: {}", last()));
        }
        let _g = Local(s.cast());
        Ok(from_wide_ptr(s))
    }

    /// `true` when the volume holding `dir` keeps ACLs (NTFS / ReFS).
    pub fn volume_has_acls(dir: &Path) -> Result<bool, String> {
        let w = wide(dir.as_os_str());
        let mut root = vec![0u16; 1024];
        // SAFETY: valid path, buffer of the given length.
        if unsafe { GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return Err(format!("GetVolumePathNameW({}): {}", dir.display(), last()));
        }
        let mut flags = 0u32;
        // SAFETY: NUL-terminated root path; unused outputs are null.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                std::ptr::null_mut(),
                0,
            )
        };
        if ok == 0 {
            return Err(format!("GetVolumeInformationW: {}", last()));
        }
        Ok(flags & FILE_PERSISTENT_ACLS != 0)
    }

    /// DACL of an open handle as SDDL.
    fn dacl_of(h: HANDLE) -> Result<String, String> {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid handle opened with READ_CONTROL; sd freed by Local.
        let rc = unsafe {
            GetSecurityInfo(
                h,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        let _g = Local(sd);
        if rc != 0 {
            return Err(format!("GetSecurityInfo: Win32 error {rc}"));
        }
        let mut s: *mut u16 = std::ptr::null_mut();
        // SAFETY: valid descriptor; string freed by Local.
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                1,
                DACL_SECURITY_INFORMATION,
                &mut s,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!("ConvertSecurityDescriptorToStringSecurityDescriptorW: {}", last()));
        }
        let _s = Local(s.cast());
        Ok(from_wide_ptr(s))
    }

    pub fn create_secret_file(path: &Path, allow_unprotected: bool) -> Result<(File, Protection), String> {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let acls = volume_has_acls(dir)?;
        let w = wide(path.as_os_str());
        if !acls {
            if !allow_unprotected {
                return Err(format!(
                    "{} is on a volume without ACLs (FAT / exFAT): the secret key would be readable by every local user; use an NTFS folder (e.g. on a BitLocker To Go drive) or pass --allow-unprotected",
                    dir.display()
                ));
            }
            // SAFETY: valid path; default security (none on this volume).
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(format!("{}: {}", path.display(), last()));
            }
            // SAFETY: fresh handle, owned by the File from here on.
            return Ok((unsafe { File::from_raw_handle(h as _) }, Protection::Unprotected));
        }
        let sid = current_user_sid()?;
        let sddl: Vec<u16> = key_dacl_sddl(&sid).encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid SDDL; sd freed by Local.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut())
        } == 0
        {
            return Err(format!("SDDL: {}", last()));
        }
        let _g = Local(sd);
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        // SAFETY: valid path and security attributes; CREATE_NEW never opens an existing file.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                GENERIC_WRITE | READ_CONTROL,
                0,
                &sa,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(format!("{}: {}", path.display(), last()));
        }
        // SAFETY: fresh handle, owned by the File from here on (closed on every path).
        let f = unsafe { File::from_raw_handle(h as _) };
        let got = dacl_of(f.as_raw_handle() as HANDLE)?;
        if !dacl_is_restricted(&got, &sid) {
            drop(f);
            let _ = std::fs::remove_file(path);
            return Err(format!("{}: the restricted DACL did not apply (got {got}); nothing written", path.display()));
        }
        Ok((f, Protection::Restricted { user_sid: sid }))
    }
}

/// Create `path` (must not exist) for the secret key: protected DACL (current user +
/// SYSTEM) on an ACL volume; on a volume without ACLs only with `allow_unprotected`.
#[cfg(windows)]
pub fn create_secret_file(path: &Path, allow_unprotected: bool) -> Result<(File, Protection), String> {
    imp::create_secret_file(path, allow_unprotected)
}

/// Non-Windows (tests on other hosts only): mode 0600 through `create_new`.
#[cfg(not(windows))]
pub fn create_secret_file(path: &Path, _allow_unprotected: bool) -> Result<(File, Protection), String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    let f = o.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((f, Protection::Restricted { user_sid: String::new() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dacl_check() {
        let me = "S-1-5-21-1-2-3-1001";
        assert!(dacl_is_restricted(&key_dacl_sddl(me), me));
        assert!(dacl_is_restricted(&format!("D:PAI(A;;FA;;;SY)(A;;FA;;;{me})"), me));
        assert!(dacl_is_restricted(&format!("D:P(A;;FA;;;{me})(A;;FA;;;S-1-5-18)"), me));
        // Not protected, extra ACE, other user, partial rights, deny entries.
        assert!(!dacl_is_restricted(&format!("D:(A;;FA;;;{me})(A;;FA;;;SY)"), me));
        assert!(!dacl_is_restricted(&format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FR;;;AU)"), me));
        assert!(!dacl_is_restricted("D:P(A;;FA;;;S-1-5-21-9)(A;;FA;;;SY)", me));
        assert!(!dacl_is_restricted(&format!("D:P(A;;FR;;;{me})(A;;FA;;;SY)"), me));
        assert!(!dacl_is_restricted(&format!("D:P(D;;FA;;;{me})(A;;FA;;;SY)"), me));
        assert!(!dacl_is_restricted("", me));
    }

    #[cfg(windows)]
    #[test]
    fn secret_file_gets_the_protected_dacl() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!("pulsar-seat-keyfile-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("minisign.key");
        let _ = std::fs::remove_file(&p);
        let (mut f, prot) = create_secret_file(&p, false).unwrap();
        f.write_all(b"x").unwrap();
        drop(f);
        assert!(matches!(prot, Protection::Restricted { .. }), "{prot:?}");
        // CREATE_NEW: an existing file is never reopened.
        assert!(create_secret_file(&p, false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
