// SPDX-License-Identifier: GPL-3.0-only
//! Trusted folders under `%ProgramData%\PulsarSplit` (review fix, 2026-11-12).
//!
//! `%ProgramData%` lets `BUILTIN\Users` create folders and files, and whoever pre-creates a
//! folder owns it. Elevated `shimctl` and the service trust what they read below the data
//! root (journal, database copy, DXVK releases), so every folder we use is:
//!
//! - created with [`PRIVATE_DIR_SDDL`] from the start (no moment with an inherited ACL), or
//! - when it already exists: opened without following reparse points, refused when it is a
//!   junction / symlink or its owner is neither SYSTEM nor Administrators, and then given
//!   the same protected DACL through the open handle.
//!
//! Files are checked before they are read ([`check_file`]): no reparse point, one link,
//! owner SYSTEM or Administrators.
//!
//! The DACL is the one the other packages use for the same data root
//! (`split-setup` `DATA_DIR_SDDL`, `split-stream` `SEAT_DIR_SDDL`), so re-applying it never
//! fights them. No `BUILTIN\Users` read entry: nothing outside SYSTEM / Administrators
//! reads these folders (the module reads only the registry).

use crate::error::ShimError;
use std::path::{Path, PathBuf};
use std::ptr;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
    READ_CONTROL, WRITE_DAC,
};

/// SYSTEM and Administrators full control, inherited, protected (no ProgramData ACEs).
pub const PRIVATE_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn untrusted(p: &Path, why: &str) -> ShimError {
    ShimError::System(format!("refusing {}: {why}", p.display()))
}

fn os(what: &str) -> ShimError {
    ShimError::System(format!("{what}: {}", std::io::Error::last_os_error()))
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle owned by this guard.
        unsafe { CloseHandle(self.0) };
    }
}

struct Sd(PSECURITY_DESCRIPTOR);
impl Sd {
    fn new(sddl: &str) -> Result<Self, ShimError> {
        let w: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: NUL-terminated string; out pointer valid.
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut()) };
        if ok == 0 {
            return Err(os("SDDL"));
        }
        Ok(Self(sd))
    }
    fn dacl(&self) -> Result<*mut ACL, ShimError> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: valid descriptor and out pointers.
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 {
            return Err(os("SDDL has no DACL"));
        }
        Ok(dacl)
    }
}
impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor… with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

/// Opens `p` itself, never the target of a junction or symlink. No `FILE_SHARE_DELETE`:
/// nobody can rename the object away while we hold it. `Ok(None)` = missing.
fn open_no_follow(p: &Path, access: u32) -> Result<Option<Handle>, ShimError> {
    let w = wide_path(p);
    // SAFETY: NUL-terminated path; no template, no security attributes.
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
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        // SAFETY: trivially safe.
        let e = unsafe { GetLastError() };
        if e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        return Err(ShimError::System(format!("open {}: {}", p.display(), std::io::Error::from_raw_os_error(e as i32))));
    }
    Ok(Some(Handle(h)))
}

fn file_info(h: &Handle) -> Result<BY_HANDLE_FILE_INFORMATION, ShimError> {
    // SAFETY: plain-data out struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle and out struct.
    if unsafe { GetFileInformationByHandle(h.0, &mut info) } == 0 {
        return Err(os("GetFileInformationByHandle"));
    }
    Ok(info)
}

/// Owner is SYSTEM or Administrators.
fn owner_trusted(h: &Handle) -> Result<bool, ShimError> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: handle opened with READ_CONTROL; `owner` points into `sd`, freed below.
    let rc = unsafe {
        GetSecurityInfo(h.0, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &mut owner, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), &mut sd)
    };
    if rc != 0 {
        return Err(ShimError::System(format!("GetSecurityInfo: {}", std::io::Error::from_raw_os_error(rc as i32))));
    }
    // SAFETY: `owner` is a valid SID inside `sd`.
    let ok = !owner.is_null()
        && unsafe { IsWellKnownSid(owner, WinLocalSystemSid) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0 };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd) };
    Ok(ok)
}

/// Creates (or checks and locks) one folder; its parent must already exist.
pub fn ensure_dir_with(p: &Path, sddl: &str) -> Result<(), ShimError> {
    let sd = Sd::new(sddl)?;
    let dacl = sd.dacl()?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let w = wide_path(p);
    // SAFETY: valid path and attributes.
    let created = unsafe { CreateDirectoryW(w.as_ptr(), &sa) } != 0;
    // SAFETY: trivially safe.
    if !created && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Err(os(&format!("CreateDirectory {}", p.display())));
    }
    let h = open_no_follow(p, READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)?.ok_or_else(|| untrusted(p, "vanished after create"))?;
    let info = file_info(&h)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(untrusted(p, "reparse point (junction or symlink)"));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(untrusted(p, "not a directory"));
    }
    if !owner_trusted(&h)? {
        return Err(untrusted(p, "owner is neither SYSTEM nor Administrators"));
    }
    // On the handle, never by path. Existing children keep their own ACL; files we read are
    // checked one by one with `check_file`.
    // SAFETY: handle with WRITE_DAC; `dacl` lives as long as `sd`.
    let rc = unsafe {
        SetSecurityInfo(
            h.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if rc != 0 {
        return Err(ShimError::System(format!("SetSecurityInfo {}: {}", p.display(), std::io::Error::from_raw_os_error(rc as i32))));
    }
    Ok(())
}

/// `data_root` and each folder from it down to `dir`, top down, with [`PRIVATE_DIR_SDDL`].
/// `dir` must be `data_root` or below it.
pub fn ensure_private_dir(data_root: &Path, dir: &Path) -> Result<(), ShimError> {
    let rel = dir.strip_prefix(data_root).map_err(|_| untrusted(dir, "outside the data root"))?;
    ensure_dir_with(data_root, PRIVATE_DIR_SDDL)?;
    let mut cur = data_root.to_path_buf();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(n) => cur.push(n),
            _ => return Err(untrusted(dir, "unexpected path component")),
        }
        ensure_dir_with(&cur, PRIVATE_DIR_SDDL)?;
    }
    Ok(())
}

/// A file we are about to trust: missing is fine; else no reparse point, one link, a
/// regular file, owner SYSTEM or Administrators.
pub fn check_file(p: &Path) -> Result<(), ShimError> {
    let Some(h) = open_no_follow(p, READ_CONTROL | FILE_READ_ATTRIBUTES)? else { return Ok(()) };
    let info = file_info(&h)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(untrusted(p, "reparse point"));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(untrusted(p, "directory where a file is expected"));
    }
    if info.nNumberOfLinks != 1 {
        return Err(untrusted(p, "file has extra hard links"));
    }
    if !owner_trusted(&h)? {
        return Err(untrusted(p, "owner is neither SYSTEM nor Administrators"));
    }
    Ok(())
}

/// A folder we are about to trust (e.g. a DXVK release): it and everything in it pass the
/// same checks as [`check_file`] (directories: no reparse point, trusted owner).
pub fn check_tree(dir: &Path) -> Result<(), ShimError> {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let h = open_no_follow(&d, READ_CONTROL | FILE_READ_ATTRIBUTES)?.ok_or_else(|| untrusted(&d, "missing"))?;
        let info = file_info(&h)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(untrusted(&d, "reparse point"));
        }
        if !owner_trusted(&h)? {
            return Err(untrusted(&d, "owner is neither SYSTEM nor Administrators"));
        }
        drop(h);
        for e in std::fs::read_dir(&d)? {
            let p = e?.path();
            let md = std::fs::symlink_metadata(&p)?;
            if md.is_dir() {
                stack.push(p);
            } else {
                check_file(&p)?;
            }
        }
    }
    Ok(())
}

/// Writes `bytes` to a fresh temporary sibling of `p` (created new, inside the locked
/// folder) and moves it over `p` with write-through.
pub fn write_replace(p: &Path, bytes: &[u8]) -> Result<(), ShimError> {
    use std::io::Write;
    let tmp = temp_sibling(p);
    let res = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        replace(&tmp, p)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// `<p>.tmp-<pid>-<nanos>`
pub fn temp_sibling(p: &Path) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut s = p.as_os_str().to_os_string();
    s.push(format!(".tmp-{}-{n:08x}", std::process::id()));
    PathBuf::from(s)
}

/// Moves `from` over `to` (replacing it), write-through.
pub fn replace(from: &Path, to: &Path) -> Result<(), ShimError> {
    let (f, t) = (wide_path(from), wide_path(to));
    // SAFETY: valid NUL-terminated paths.
    if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(os(&format!("MoveFileEx {}", to.display())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a temp folder owned by the test user. The production SDDL is not applied here
    /// (owner checks need SYSTEM / Administrators, which a non-elevated test is not).
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("split-shim-winfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn user_owned_folder_is_refused() {
        let d = tmp("owner");
        std::fs::create_dir_all(&d).unwrap();
        // Created by a non-elevated user: the owner is that user, not SYSTEM / Administrators.
        let elevated = owner_trusted(&open_no_follow(&d, READ_CONTROL).unwrap().unwrap()).unwrap();
        if !elevated {
            assert!(ensure_dir_with(&d, "D:P(A;OICI;FA;;;OW)").is_err());
            let f = d.join("f");
            std::fs::write(&f, b"x").unwrap();
            assert!(check_file(&f).is_err());
            assert!(check_tree(&d).is_err());
        }
        assert!(check_file(&d.join("missing")).is_ok());
        assert!(ensure_private_dir(&d, Path::new(r"C:\elsewhere")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_replace_round_trip() {
        let d = tmp("replace");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.bin");
        write_replace(&f, b"one").unwrap();
        write_replace(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temp file left");
        let _ = std::fs::remove_dir_all(&d);
    }
}
