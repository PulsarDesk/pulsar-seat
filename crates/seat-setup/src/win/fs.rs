// SPDX-License-Identifier: GPL-3.0-only
//! File system: data folder with owner + protected DACL from SDDL, and deletes that fall
//! back to `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)` for files in use (spec 90 §4.3).
//!
//! The data folder is SYSTEM-trusted state (journals replayed by uninstall, downloaded
//! installers run as SYSTEM, the setup log). A standard user can create
//! `%ProgramData%\PulsarSplit` before install and would stay its owner, so:
//! * new folders are created with `CreateDirectoryW` and the full descriptor (owner
//!   included) from the start;
//! * an existing folder that is a reparse point or whose owner is not SYSTEM,
//!   Administrators or the account running setup is moved aside, then created fresh;
//! * owner + DACL are applied through a handle opened without following reparse points;
//! * below the folder, reparse points, hard-linked files and entries with an untrusted
//!   owner are deleted (the link itself, never what it points to). Trusted children keep
//!   their own ACL (other packages set tighter ones).
//!
//! Every walk uses `symlink_metadata` / directory-entry attributes and never descends
//! into a reparse point.

use super::wide;
use crate::error::SetupError;
use crate::system::{RemoveOutcome, SetupFs};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW,
    READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Debug, Default, Clone, Copy)]
pub struct WinFs;

/// Security descriptor parsed from SDDL, freed on drop.
struct Sd(PSECURITY_DESCRIPTOR);

impl Sd {
    fn parse(sddl: &str, path: &Path) -> Result<Self, SetupError> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated input; the descriptor is freed in Drop.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) }
            == 0
        {
            return Err(SetupError::io(path, std::io::Error::last_os_error()));
        }
        Ok(Sd(sd))
    }

    /// (owner or NULL, DACL or NULL).
    fn parts(&self) -> (PSID, *mut ACL) {
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let (mut defaulted, mut present) = (0, 0);
        // SAFETY: valid self-relative descriptor from ConvertString…; out pointers valid.
        unsafe {
            GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted);
            GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted);
        }
        if present == 0 {
            dacl = std::ptr::null_mut();
        }
        (owner, dacl)
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Open `path` itself (never the target of a reparse point), file or folder.
fn open_nofollow(path: &Path, access: u32) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn handle_info(f: &std::fs::File) -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: zeroed POD out-parameter; valid handle.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(f.as_raw_handle() as HANDLE, &mut info) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info)
    }
}

/// SID of the account running this process (SYSTEM for the installer custom actions).
fn with_process_user<R>(f: impl FnOnce(PSID) -> R) -> Option<R> {
    // SAFETY: token handle closed below; buffer sized by the first call.
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(tok, TokenUser, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        windows_sys::Win32::Foundation::CloseHandle(tok);
        if ok == 0 {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        Some(f(tu.User.Sid))
    }
}

/// Owner is SYSTEM, Administrators, or the account running setup.
fn owner_trusted(f: &std::fs::File) -> std::io::Result<bool> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: handle opened with READ_CONTROL; descriptor freed with LocalFree.
    unsafe {
        let rc = GetSecurityInfo(
            f.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        );
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        let ok = !owner.is_null()
            && (IsWellKnownSid(owner, WinLocalSystemSid) != 0
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
                || with_process_user(|me| EqualSid(owner, me) != 0).unwrap_or(false));
        LocalFree(sd as HLOCAL);
        Ok(ok)
    }
}

/// Why an existing entry may not be trusted; `None` = trusted.
fn untrusted_reason(path: &Path, expect_dir: Option<bool>) -> std::io::Result<Option<String>> {
    let f = open_nofollow(path, READ_CONTROL | FILE_READ_ATTRIBUTES)?;
    let info = handle_info(&f)?;
    let is_dir = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(Some("reparse point".into()));
    }
    if expect_dir.is_some_and(|d| d != is_dir) {
        return Ok(Some(if is_dir { "folder where a file was expected" } else { "file where a folder was expected" }.into()));
    }
    if !is_dir && info.nNumberOfLinks > 1 {
        return Ok(Some(format!("{} hard links", info.nNumberOfLinks)));
    }
    if !owner_trusted(&f)? {
        return Ok(Some("owner is not SYSTEM / Administrators".into()));
    }
    Ok(None)
}

/// Apply owner (when the SDDL names one) + protected DACL through a no-follow handle.
fn apply_sd(path: &Path, sd: &Sd) -> Result<(), SetupError> {
    let (owner, dacl) = sd.parts();
    let mut info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    let mut access = READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES;
    if !owner.is_null() {
        info |= OWNER_SECURITY_INFORMATION;
        access |= WRITE_OWNER;
    }
    let f = open_nofollow(path, access).map_err(|e| SetupError::io(path, e))?;
    let attrs = handle_info(&f).map_err(|e| SetupError::io(path, e))?.dwFileAttributes;
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(SetupError::Other(format!("{} became a reparse point", path.display())));
    }
    // SAFETY: valid handle with WRITE_DAC (+ WRITE_OWNER); owner/DACL from a live descriptor.
    let rc = unsafe {
        SetSecurityInfo(f.as_raw_handle() as HANDLE, SE_FILE_OBJECT, info, owner, std::ptr::null_mut(), dacl, std::ptr::null())
    };
    if rc != 0 {
        return Err(SetupError::io(path, std::io::Error::from_raw_os_error(rc as i32)));
    }
    Ok(())
}

/// `CreateDirectoryW` with the full descriptor; missing parents first (same descriptor).
/// `Ok(false)` when the folder already existed.
fn create_secure(path: &Path, sd: &Sd) -> Result<bool, SetupError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && std::fs::symlink_metadata(parent).is_err()
    {
        create_secure(parent, sd)?;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let p = wide(&path.display().to_string());
    // SAFETY: NUL-terminated path; `sa` points at a live descriptor.
    if unsafe { CreateDirectoryW(p.as_ptr(), &sa) } == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Ok(false);
        }
        return Err(SetupError::io(path, e));
    }
    Ok(true)
}

fn move_aside(path: &Path) -> Result<PathBuf, SetupError> {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut to = path.as_os_str().to_owned();
    to.push(format!(".untrusted-{ms}"));
    let to = PathBuf::from(to);
    // MoveFileEx on a reparse point moves the link itself.
    std::fs::rename(path, &to).map_err(|e| SetupError::io(path, e))?;
    Ok(to)
}

fn is_reparse(md: &std::fs::Metadata) -> bool {
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Remove a link itself (junction / symlink), never its target.
fn remove_link(p: &Path, md: &std::fs::Metadata) -> std::io::Result<()> {
    if md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 { std::fs::remove_dir(p) } else { std::fs::remove_file(p) }
}

/// Delete untrusted entries below `dir` (reparse points, hard-linked files, untrusted
/// owners) without following links; recurse into trusted folders.
fn sanitize_children(dir: &Path) -> Result<(), SetupError> {
    for e in std::fs::read_dir(dir).map_err(|e| SetupError::io(dir, e))? {
        let e = e.map_err(|e| SetupError::io(dir, e))?;
        let p = e.path();
        // DirEntry metadata comes from the directory listing: it does not follow links.
        let md = e.metadata().map_err(|e| SetupError::io(&p, e))?;
        if is_reparse(&md) {
            log::warn!("removing reparse point {} from the data folder", p.display());
            remove_link(&p, &md).map_err(|e| SetupError::io(&p, e))?;
            continue;
        }
        let reason = untrusted_reason(&p, None).map_err(|e| SetupError::io(&p, e))?;
        match reason {
            Some(why) => {
                log::warn!("removing untrusted {} from the data folder: {why}", p.display());
                if md.is_dir() {
                    remove_tree_nofollow(&p)?;
                } else {
                    std::fs::remove_file(&p).map_err(|e| SetupError::io(&p, e))?;
                }
            }
            None if md.is_dir() => sanitize_children(&p)?,
            None => {}
        }
    }
    Ok(())
}

/// Recursive delete that removes links instead of following them. Locked files are
/// scheduled for deletion at boot. Returns true when something was scheduled.
fn remove_tree_nofollow(root: &Path) -> Result<bool, SetupError> {
    let mut reboot = false;
    let mut dirs = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| SetupError::io(&d, e))? {
            let e = e.map_err(|e| SetupError::io(&d, e))?;
            let p = e.path();
            let md = e.metadata().map_err(|e| SetupError::io(&p, e))?;
            if is_reparse(&md) {
                if remove_link(&p, &md).is_err() {
                    delay_delete(&p)?;
                    reboot = true;
                }
            } else if md.is_dir() {
                dirs.push(p.clone());
                stack.push(p);
            } else {
                match std::fs::remove_file(&p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) if in_use(&e) => {
                        delay_delete(&p)?;
                        reboot = true;
                    }
                    Err(e) => return Err(SetupError::io(&p, e)),
                }
            }
        }
    }
    for d in dirs.iter().rev() {
        if std::fs::remove_dir(d).is_err() {
            delay_delete(d)?;
            reboot = true;
        }
    }
    Ok(reboot)
}

fn delay_delete(path: &Path) -> Result<RemoveOutcome, SetupError> {
    let p = wide(&path.display().to_string());
    // SAFETY: NULL destination + DELAY_UNTIL_REBOOT = delete at next boot (needs admin).
    if unsafe { MoveFileExW(p.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) } == 0 {
        return Err(SetupError::io(path, std::io::Error::last_os_error()));
    }
    Ok(RemoveOutcome::ScheduledForReboot)
}

fn in_use(e: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION 32, ERROR_LOCK_VIOLATION 33, ERROR_ACCESS_DENIED 5 (mapped image)
    matches!(e.raw_os_error(), Some(32) | Some(33) | Some(5))
}

impl SetupFs for WinFs {
    fn ensure_dir(&self, path: &Path, sddl: &str) -> Result<(), SetupError> {
        let sd = Sd::parse(sddl, path)?;
        if std::fs::symlink_metadata(path).is_ok() {
            let reason = untrusted_reason(path, Some(true)).map_err(|e| SetupError::io(path, e))?;
            if let Some(why) = reason {
                let to = move_aside(path)?;
                log::warn!("{} was not trusted ({why}); moved to {}", path.display(), to.display());
            }
        }
        create_secure(path, &sd)?;
        apply_sd(path, &sd)?;
        sanitize_children(path)
    }

    fn check_trusted(&self, path: &Path) -> Result<(), SetupError> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(());
        }
        let untrusted = |p: &Path, why: String| SetupError::Other(format!("untrusted data folder entry {}: {why}", p.display()));
        if let Some(why) = untrusted_reason(path, Some(true)).map_err(|e| SetupError::io(path, e))? {
            return Err(untrusted(path, why));
        }
        let mut stack = vec![path.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).map_err(|e| SetupError::io(&d, e))? {
                let e = e.map_err(|e| SetupError::io(&d, e))?;
                let p = e.path();
                let md = e.metadata().map_err(|e| SetupError::io(&p, e))?;
                if is_reparse(&md) {
                    return Err(untrusted(&p, "reparse point".into()));
                }
                if let Some(why) = untrusted_reason(&p, None).map_err(|e| SetupError::io(&p, e))? {
                    return Err(untrusted(&p, why));
                }
                if md.is_dir() {
                    stack.push(p);
                }
            }
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn remove_file(&self, path: &Path) -> Result<RemoveOutcome, SetupError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(RemoveOutcome::Removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RemoveOutcome::Missing),
            Err(e) if in_use(&e) => delay_delete(path),
            Err(e) => Err(SetupError::io(path, e)),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> Result<RemoveOutcome, SetupError> {
        let md = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RemoveOutcome::Missing),
            Err(e) => return Err(SetupError::io(path, e)),
        };
        if is_reparse(&md) {
            // The data root itself is a link: remove the link, never its target.
            remove_link(path, &md).map_err(|e| SetupError::io(path, e))?;
            return Ok(RemoveOutcome::Removed);
        }
        // std's remove_dir_all does not follow links (CVE-2022-21658 fix).
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(RemoveOutcome::Removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RemoveOutcome::Missing),
            // Delete what we can, schedule the rest; links are removed, never followed.
            Err(_) => Ok(if remove_tree_nofollow(path)? { RemoveOutcome::ScheduledForReboot } else { RemoveOutcome::Removed }),
        }
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<(), SetupError> {
        std::fs::copy(from, to).map(|_| ()).map_err(|e| SetupError::io(to, e))
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SetupError> {
        std::fs::write(path, bytes).map_err(|e| SetupError::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only touches a temp folder owned by the test user. The production SDDL
    /// (owner SYSTEM) is not used: a non-elevated test could neither set nor remove it.
    const TEST_SDDL: &str = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)";

    #[test]
    fn ensure_dir_and_remove_in_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("data").join("journal");
        WinFs.ensure_dir(&p, TEST_SDDL).unwrap();
        WinFs.check_trusted(&p).unwrap();
        std::fs::write(p.join("f"), b"x").unwrap();
        assert_eq!(WinFs.remove_file(&p.join("f")).unwrap(), RemoveOutcome::Removed);
        assert_eq!(WinFs.remove_file(&p.join("f")).unwrap(), RemoveOutcome::Missing);
        WinFs.ensure_dir(&p, TEST_SDDL).unwrap();
        assert_eq!(WinFs.remove_dir_all(&d.path().join("data")).unwrap(), RemoveOutcome::Removed);
        assert_eq!(WinFs.remove_dir_all(&d.path().join("data")).unwrap(), RemoveOutcome::Missing);
    }

    #[test]
    fn hard_links_are_removed_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let data = d.path().join("data");
        std::fs::create_dir_all(data.join("logs")).unwrap();
        std::fs::hard_link(&outside, data.join("logs").join("setup.log")).unwrap();
        assert!(WinFs.check_trusted(&data).is_err(), "hard link flagged");
        WinFs.ensure_dir(&data, TEST_SDDL).unwrap();
        assert!(!data.join("logs").join("setup.log").exists(), "link removed");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep", "target untouched");
        WinFs.check_trusted(&data).unwrap();
    }

    /// Junctions need no privilege; symlinks would. Created with `mklink /J` (cmd built-in).
    #[test]
    fn junctions_are_removed_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("victim.txt"), b"keep").unwrap();
        let data = d.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let link = data.join("downloads");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("mklink /J not available; skipping");
            return;
        }
        assert!(WinFs.check_trusted(&data).is_err(), "junction flagged");
        // remove_dir_all fallback path: lock nothing, but go through remove_tree_nofollow.
        assert!(!remove_tree_nofollow(&data).unwrap());
        assert!(outside.join("victim.txt").exists(), "junction target untouched");
        assert!(!data.exists());
        // Data root itself a junction: ensure_dir moves it aside, the target stays.
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&data).arg(&outside).output().unwrap();
        WinFs.ensure_dir(&data, TEST_SDDL).unwrap();
        assert!(outside.join("victim.txt").exists());
        assert!(std::fs::read_dir(&data).unwrap().next().is_none(), "fresh, empty data folder");
        let aside = std::fs::read_dir(d.path()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("data.untrusted-"));
        assert!(aside);
    }
}
