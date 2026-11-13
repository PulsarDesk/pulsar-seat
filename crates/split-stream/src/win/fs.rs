// SPDX-License-Identifier: GPL-3.0-only
//! [`SecureFs`] on NTFS: owner + protected DACL from SDDL (set through a handle that does
//! not follow reparse points), write-through atomic replace.

use super::{os_err, wide_os, OwnedHandle, SecurityDescriptor};
use crate::seatdir::{rotated_names, SecureFs};
use crate::StreamError;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr;
use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE, HLOCAL,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    EqualSid, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, IsWellKnownSid, WinBuiltinAdministratorsSid,
    WinLocalSystemSid, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetFileInformationByHandle, MoveFileExW, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};

/// [`SecureFs`] on NTFS. Existing directories are opened without following reparse points
/// and checked (owner SYSTEM / Administrators / the SDDL's owner) before they are trusted.
///
/// Setting the owner to SYSTEM needs no privilege when the caller runs as SYSTEM (the
/// service); other callers can only name themselves as owner (see the test).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinSecureFs;

fn untrusted(p: &Path, why: &str) -> StreamError {
    StreamError::Io(format!("refusing {}: {why}", p.display()))
}

/// Open `p` itself, never the target of a junction or symlink. No `FILE_SHARE_DELETE`, so
/// nobody can rename or replace the object while the handle is open. `Ok(None)` = missing.
fn open_no_follow(p: &Path, access: u32) -> Result<Option<OwnedHandle>, StreamError> {
    let w = wide_os(p.as_os_str());
    // SAFETY: NUL-terminated path, no template, no security attributes.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    match OwnedHandle::new(h) {
        Some(h) => Ok(Some(h)),
        None => {
            // SAFETY: trivially safe.
            let e = unsafe { GetLastError() };
            if e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND {
                Ok(None)
            } else {
                Err(StreamError::io("open (no reparse)", std::io::Error::from_raw_os_error(e as i32)))
            }
        }
    }
}

fn file_info(h: HANDLE) -> Result<BY_HANDLE_FILE_INFORMATION, StreamError> {
    // SAFETY: plain-data out struct; valid handle.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { GetFileInformationByHandle(h, &mut info) } == 0 {
        return Err(os_err("GetFileInformationByHandle"));
    }
    Ok(info)
}

/// Owner is SYSTEM, Administrators, or `extra` (the SDDL's owner; may be null).
fn owner_trusted(h: HANDLE, extra: PSID) -> Result<bool, StreamError> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: handle opened with READ_CONTROL; `owner` points into `sd`, freed below.
    let rc = unsafe {
        GetSecurityInfo(
            h,
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
        return Err(StreamError::io("GetSecurityInfo", std::io::Error::from_raw_os_error(rc as i32)));
    }
    // SAFETY: `owner` is a valid SID inside `sd`; `extra` is null or a valid SID.
    let ok = !owner.is_null()
        && unsafe {
            IsWellKnownSid(owner, WinLocalSystemSid) != 0
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
                || (!extra.is_null() && EqualSid(owner, extra) != 0)
        };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd as HLOCAL) };
    Ok(ok)
}

/// Owner (null when the SDDL names none) and DACL of a parsed SDDL; both point into `sd`.
fn sddl_parts(sd: &SecurityDescriptor) -> Result<(PSID, *mut ACL), StreamError> {
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = ptr::null_mut();
    // SAFETY: valid descriptor and out pointers.
    if unsafe { GetSecurityDescriptorDacl(sd.as_ptr(), &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 {
        return Err(os_err("SDDL has no DACL"));
    }
    let mut owner: PSID = ptr::null_mut();
    // SAFETY: as above.
    if unsafe { GetSecurityDescriptorOwner(sd.as_ptr(), &mut owner, &mut defaulted) } == 0 {
        return Err(os_err("GetSecurityDescriptorOwner"));
    }
    Ok((owner, dacl))
}

fn tmp_name(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(s)
}

impl SecureFs for WinSecureFs {
    fn ensure_dir(&self, p: &Path, sddl: &str) -> Result<(), StreamError> {
        let sd = SecurityDescriptor::from_sddl(sddl)?;
        let (owner, dacl) = sddl_parts(&sd)?;
        let w = wide_os(p.as_os_str());
        let sa = sd.attributes();
        // Created with the final owner + DACL: no moment with an inherited ACL. The parent
        // must exist (callers create the chain top down, each level checked here).
        // SAFETY: valid path and attributes.
        let created = unsafe { CreateDirectoryW(w.as_ptr(), &sa) } != 0;
        // SAFETY: trivially safe.
        if !created && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Err(os_err("CreateDirectoryW"));
        }
        let h = open_no_follow(p, READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES)?
            .ok_or_else(|| untrusted(p, "vanished after create"))?;
        let info = file_info(h.raw())?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(untrusted(p, "reparse point (junction or symlink)"));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(untrusted(p, "not a directory"));
        }
        if !created && !owner_trusted(h.raw(), owner)? {
            return Err(untrusted(p, "existing directory has a foreign owner"));
        }
        let mut what = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        if !owner.is_null() {
            what |= OWNER_SECURITY_INFORMATION;
        }
        // On the handle, never by path, so no reparse point can redirect it. Existing
        // children are not re-inherited; everything below is created under this ACL.
        // SAFETY: handle with WRITE_DAC | WRITE_OWNER; owner and DACL live as long as `sd`.
        let rc = unsafe { SetSecurityInfo(h.raw(), SE_FILE_OBJECT, what, owner, ptr::null_mut(), dacl, ptr::null()) };
        if rc != 0 {
            return Err(StreamError::io("SetSecurityInfo", std::io::Error::from_raw_os_error(rc as i32)));
        }
        Ok(())
    }

    fn check_file(&self, p: &Path) -> Result<(), StreamError> {
        let Some(h) = open_no_follow(p, READ_CONTROL | FILE_READ_ATTRIBUTES)? else { return Ok(()) };
        let info = file_info(h.raw())?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(untrusted(p, "reparse point"));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(untrusted(p, "directory where a file is expected"));
        }
        if info.nNumberOfLinks != 1 {
            return Err(untrusted(p, "file has extra hard links"));
        }
        if !owner_trusted(h.raw(), ptr::null_mut())? {
            return Err(untrusted(p, "foreign owner"));
        }
        Ok(())
    }

    fn write_atomic(&self, p: &Path, bytes: &[u8]) -> Result<(), StreamError> {
        let tmp = tmp_name(p);
        let res = (|| {
            // The temp file is created inside the seat directory and inherits its ACL.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| StreamError::io("create temp file", e))?;
            f.write_all(bytes).map_err(|e| StreamError::io("write temp file", e))?;
            f.sync_all().map_err(|e| StreamError::io("flush temp file", e))?;
            drop(f);
            let (a, b) = (wide_os(tmp.as_os_str()), wide_os(p.as_os_str()));
            // SAFETY: NUL-terminated paths.
            if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
                return Err(os_err("MoveFileExW"));
            }
            Ok(())
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res
    }

    fn rotate_log(&self, p: &Path, keep: usize) -> Result<(), StreamError> {
        let names = rotated_names(p, keep);
        let mv = |a: &Path, b: &Path| match std::fs::rename(a, b) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StreamError::io("rotate log", e)),
        };
        if keep == 0 {
            return self.remove_file(p);
        }
        self.remove_file(&names[keep - 1])?;
        for i in (0..keep - 1).rev() {
            mv(&names[i], &names[i + 1])?;
        }
        mv(p, &names[0])
    }

    fn remove_dir_all(&self, p: &Path) -> Result<(), StreamError> {
        match std::fs::remove_dir_all(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StreamError::io("remove seat dir", e)),
        }
    }

    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>, StreamError> {
        match std::fs::read(p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StreamError::io("read", e)),
        }
    }

    fn remove_file(&self, p: &Path) -> Result<(), StreamError> {
        match std::fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StreamError::io("remove file", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Only touches a temp directory owned by the current user (no system change). The
    //! owner/DACL step uses an SDDL that names the current user as owner and grants it
    //! access, so the test can clean up afterwards (only SYSTEM may name SYSTEM as owner).
    use super::*;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn my_sid() -> String {
        // SAFETY: standard token query on our own process; buffers sized by the API.
        unsafe {
            let mut tok = ptr::null_mut();
            assert!(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) != 0);
            let tok = OwnedHandle::new(tok).unwrap();
            let mut buf = vec![0u8; 256];
            let mut len = 0;
            assert!(GetTokenInformation(tok.raw(), TokenUser, buf.as_mut_ptr().cast(), 256, &mut len) != 0);
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut w: *mut u16 = ptr::null_mut();
            assert!(ConvertSidToStringSidW(user.User.Sid, &mut w) != 0);
            let n = (0..).take_while(|&i| *w.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(w, n));
            LocalFree(w as HLOCAL);
            s
        }
    }

    fn my_sddl() -> String {
        format!("O:{}D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)", my_sid())
    }

    #[test]
    fn atomic_write_rotate_and_remove_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WinSecureFs;
        let seats = dir.path().join("seats");
        let seat = seats.join("x");
        let sddl = my_sddl();
        assert!(fs.ensure_dir(&seat, &sddl).is_err(), "parent must exist (top-down creation)");
        fs.ensure_dir(&seats, &sddl).unwrap();
        fs.ensure_dir(&seat, &sddl).unwrap();
        fs.ensure_dir(&seat, &sddl).unwrap();
        let f = seat.join("server.conf");
        fs.write_atomic(&f, b"a").unwrap();
        fs.write_atomic(&f, b"b").unwrap();
        assert_eq!(fs.read(&f).unwrap().unwrap(), b"b");
        assert!(fs.read(&seat.join("nope")).unwrap().is_none());
        assert!(fs.check_file(&seat.join("nope")).is_ok(), "missing file is fine");
        assert!(fs.check_file(&seat).is_err(), "directory is not a file");
        std::fs::hard_link(&f, seat.join("link")).unwrap();
        assert!(fs.check_file(&f).is_err(), "hard-linked file is refused");
        let log = seat.join("server.log");
        for i in 0..5 {
            std::fs::write(&log, format!("{i}")).unwrap();
            fs.rotate_log(&log, 3).unwrap();
        }
        let names = rotated_names(&log, 3);
        assert_eq!(std::fs::read_to_string(&names[0]).unwrap(), "4");
        assert_eq!(std::fs::read_to_string(&names[2]).unwrap(), "2");
        assert!(!log.exists());
        fs.remove_dir_all(&seat).unwrap();
        fs.remove_dir_all(&seat).unwrap();
        assert!(!seat.exists());
    }

    #[test]
    fn existing_dir_with_foreign_owner_or_junction_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WinSecureFs;
        // Pre-created by "someone else": owner is this (non-SYSTEM) user, the SDDL names
        // SYSTEM. (Elevated, the owner would be Administrators and SetSecurityInfo then
        // fails to name SYSTEM, so this is an error either way.)
        let pre = dir.path().join("pre");
        std::fs::create_dir(&pre).unwrap();
        assert!(fs.ensure_dir(&pre, crate::seatdir::SEAT_DIR_SDDL).is_err());
        // A junction is never followed. mklink /J needs no privilege; skip if unavailable.
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let j = dir.path().join("junction");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&j)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let e = fs.ensure_dir(&j, &my_sddl()).unwrap_err();
            assert!(e.to_string().contains("reparse"), "{e}");
            assert!(fs.check_file(&j).is_err());
            std::fs::remove_dir(&j).unwrap();
        }
    }
}
