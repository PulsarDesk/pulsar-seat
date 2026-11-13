// SPDX-License-Identifier: GPL-3.0-only
//! File system access for code that runs as SYSTEM in folders it does not fully own
//! (the Virtual-Display-Driver settings folder, the restore journal folder).
//!
//! Every operation starts from a [`TrustedDir`]: the folder is opened without following a
//! reparse point, its final path must equal the requested path (no junction anywhere on
//! the way), its owner must be SYSTEM, Administrators or TrustedInstaller, and no other
//! principal may hold a right that lets it create, replace, rename or delete entries
//! (write / append / delete-child / delete / WRITE_DAC / WRITE_OWNER / generic write).
//! The folder handle stays open without `FILE_SHARE_DELETE` while files in it are used,
//! so it cannot be renamed or swapped in between. Files are opened with
//! `FILE_FLAG_OPEN_REPARSE_POINT` and must be regular files with one link.

use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::BuildHasher;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, DeleteFileW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_NAME_NORMALIZED, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PWSTR;

use super::{from_wide, pcwstr, wide};

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const ERROR_ALREADY_EXISTS: u32 = 183;

/// Rights that let a principal change what a folder contains or who controls it.
pub const WRITE_RIGHTS: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
    | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x0200_0000 // MAXIMUM_ALLOWED
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE

pub const SID_SYSTEM: &str = "S-1-5-18";
pub const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
pub const SID_TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
/// CREATOR OWNER: resolves to whoever creates a child; only trusted principals can.
pub const SID_CREATOR_OWNER: &str = "S-1-3-0";

/// DACL for folders we create: SYSTEM + Administrators only, not inherited.
pub const PROTECTED_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Why a path is refused.
#[derive(Debug)]
pub enum FsError {
    NotFound(String),
    /// The folder or file is not safe to use as SYSTEM.
    Unsafe(String),
    Io(String),
}

fn io_err(e: std::io::Error, what: &str, p: &Path) -> FsError {
    if e.kind() == std::io::ErrorKind::NotFound {
        FsError::NotFound(format!("{} not found", p.display()))
    } else {
        FsError::Io(format!("{what} {}: {e}", p.display()))
    }
}

/// One access-allowed or -denied entry of a DACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AceInfo {
    pub allow: bool,
    /// `None` for an ACE type whose SID we do not parse (object / callback-object ACEs).
    pub sid: Option<String>,
    pub mask: u32,
}

fn trusted_principal(sid: &str) -> bool {
    [
        SID_SYSTEM,
        SID_ADMINISTRATORS,
        SID_TRUSTED_INSTALLER,
        SID_CREATOR_OWNER,
    ]
    .contains(&sid)
}

/// The folder policy, on already extracted owner + DACL (`None` = NULL DACL).
pub fn check_security(owner: &str, dacl: Option<&[AceInfo]>) -> Result<(), String> {
    if ![SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER].contains(&owner) {
        return Err(format!(
            "owner {owner} is not SYSTEM, Administrators or TrustedInstaller"
        ));
    }
    let Some(aces) = dacl else {
        return Err("NULL DACL (everyone has full access)".into());
    };
    // Every ACE counts, inherited or inherit-only too: an inheritable write ACE would
    // apply to the files we create here. Deny ACEs never widen access.
    for a in aces
        .iter()
        .filter(|a| a.allow && a.mask & WRITE_RIGHTS != 0)
    {
        match &a.sid {
            Some(s) if trusted_principal(s) => {}
            Some(s) => {
                return Err(format!(
                    "{s} may write to it (rights 0x{:08X})",
                    a.mask & WRITE_RIGHTS
                ));
            }
            None => return Err("an object ACE grants write rights".into()),
        }
    }
    Ok(())
}

fn sid_string(sid: PSID) -> Result<String, FsError> {
    let mut s = PWSTR::null();
    // SAFETY: valid SID from GetSecurityInfo / ACE; output freed with LocalFree.
    unsafe { ConvertSidToStringSidW(sid, &mut s) }
        .map_err(|e| FsError::Io(format!("ConvertSidToStringSidW: {e}")))?;
    // SAFETY: NUL-terminated string from the call above.
    let out = unsafe { s.to_string() }.unwrap_or_default();
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut core::ffi::c_void)));
    }
    Ok(out)
}

/// Owner SID string + DACL entries of an open handle.
fn security_of(h: HANDLE) -> Result<(String, Option<Vec<AceInfo>>), FsError> {
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: out pointers valid; `sd` owns the owner + DACL memory and is freed below.
    let r = unsafe {
        GetSecurityInfo(
            h,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut sd),
        )
    };
    if r.0 != 0 {
        return Err(FsError::Io(format!("GetSecurityInfo: {}", r.0)));
    }
    let result = (|| {
        let owner = sid_string(owner)?;
        if dacl.is_null() {
            return Ok((owner, None));
        }
        // SAFETY: DACL inside `sd`.
        let count = unsafe { (*dacl).AceCount } as u32;
        let mut aces = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
            // SAFETY: index below AceCount.
            unsafe { GetAce(dacl, i, &mut p) }.map_err(|e| FsError::Io(format!("GetAce: {e}")))?;
            // SAFETY: every ACE starts with an ACE_HEADER.
            let hdr = unsafe { *(p as *const ACE_HEADER) };
            // 0 ACCESS_ALLOWED, 1 ACCESS_DENIED, 9 ALLOWED_CALLBACK, 10 DENIED_CALLBACK:
            // header, mask, SID. Object ACE types (5, 6, 11, 12) have a variable layout.
            let (allow, simple) = match hdr.AceType {
                0 | 9 => (true, true),
                1 | 10 => (false, true),
                5 | 11 => (true, false),
                _ => (false, false),
            };
            // SAFETY: mask follows the header in every access ACE layout.
            let mask = unsafe { (*(p as *const ACCESS_ALLOWED_ACE)).Mask };
            let sid = if simple {
                // SAFETY: SidStart is the first DWORD of the SID in the simple layout.
                let sp = unsafe { &(*(p as *const ACCESS_ALLOWED_ACE)).SidStart } as *const u32;
                Some(sid_string(PSID(sp as *mut core::ffi::c_void))?)
            } else {
                None
            };
            aces.push(AceInfo { allow, sid, mask });
        }
        Ok((owner, Some(aces)))
    })();
    // SAFETY: descriptor allocated by GetSecurityInfo.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    result
}

fn file_info(f: &File) -> Result<BY_HANDLE_FILE_INFORMATION, FsError> {
    let mut i = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: valid handle owned by `f`.
    unsafe { GetFileInformationByHandle(HANDLE(f.as_raw_handle()), &mut i) }
        .map_err(|e| FsError::Io(format!("GetFileInformationByHandle: {e}")))?;
    Ok(i)
}

fn final_path(f: &File) -> Result<String, FsError> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: buffer length passed through the slice.
    let n = unsafe {
        GetFinalPathNameByHandleW(HANDLE(f.as_raw_handle()), &mut buf, FILE_NAME_NORMALIZED)
    } as usize;
    if n == 0 || n >= buf.len() {
        return Err(FsError::Io("GetFinalPathNameByHandleW failed".into()));
    }
    Ok(from_wide(&buf[..n]))
}

/// `\\?\C:\x\` and `C:\X` compare equal; `\\?\UNC\…` stays as is.
pub fn normalize_path(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.trim_end_matches('\\').to_ascii_lowercase()
}

/// A verified folder, held open (no `FILE_SHARE_DELETE`) for as long as this lives.
#[derive(Debug)]
pub struct TrustedDir {
    _handle: File,
    path: PathBuf,
}

impl TrustedDir {
    /// Opens and verifies `path` (see the module docs).
    pub fn open(path: &Path) -> Result<TrustedDir, FsError> {
        let f = std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| io_err(e, "open folder", path))?;
        let info = file_info(&f)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(FsError::Unsafe(format!(
                "{} is a reparse point (junction / link)",
                path.display()
            )));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
            return Err(FsError::Unsafe(format!(
                "{} is not a folder",
                path.display()
            )));
        }
        let fin = final_path(&f)?;
        if normalize_path(&fin) != normalize_path(&path.display().to_string()) {
            return Err(FsError::Unsafe(format!(
                "{} resolves to {fin} (a junction or link on the way)",
                path.display()
            )));
        }
        let (owner, dacl) = security_of(HANDLE(f.as_raw_handle()))?;
        check_security(&owner, dacl.as_deref())
            .map_err(|why| FsError::Unsafe(format!("{}: {why}", path.display())))?;
        Ok(TrustedDir {
            _handle: f,
            path: path.to_path_buf(),
        })
    }

    /// Creates `path` with [`PROTECTED_DIR_SDDL`] if it does not exist, then opens and
    /// verifies it (an existing folder is never re-permissioned, only checked).
    pub fn create_protected(path: &Path) -> Result<TrustedDir, FsError> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let sddl = wide(PROTECTED_DIR_SDDL);
        // SAFETY: NUL-terminated SDDL; descriptor freed with LocalFree below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(pcwstr(&sddl), 1, &mut sd, None)
        }
        .map_err(|e| FsError::Io(format!("SDDL: {e}")))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let w = wide(&path.display().to_string());
        // SAFETY: NUL-terminated path; `sa` outlives the call.
        let r = unsafe { CreateDirectoryW(pcwstr(&w), Some(&sa)) };
        // SAFETY: allocated by the SDDL conversion.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        if let Err(e) = r {
            let code = super::win32_of(&e);
            if code != ERROR_ALREADY_EXISTS {
                return Err(FsError::Io(format!(
                    "create folder {}: {e}",
                    path.display()
                )));
            }
        }
        TrustedDir::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn child(&self, name: &str) -> Result<PathBuf, FsError> {
        if name.is_empty() || name.contains(['\\', '/', ':']) || name == "." || name == ".." {
            return Err(FsError::Unsafe(format!("bad file name {name:?}")));
        }
        Ok(self.path.join(name))
    }

    /// Opens `name` with `opts` without following a reparse point and checks it is a
    /// regular file with exactly one link.
    fn open_checked(
        &self,
        name: &str,
        opts: &mut std::fs::OpenOptions,
    ) -> Result<(File, PathBuf), FsError> {
        let p = self.child(name)?;
        let f = opts
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH)
            .open(&p)
            .map_err(|e| io_err(e, "open", &p))?;
        let i = file_info(&f)?;
        if i.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_DIRECTORY.0) != 0 {
            return Err(FsError::Unsafe(format!(
                "{} is not a regular file",
                p.display()
            )));
        }
        if i.nNumberOfLinks != 1 {
            return Err(FsError::Unsafe(format!(
                "{} has {} hard links",
                p.display(),
                i.nNumberOfLinks
            )));
        }
        Ok((f, p))
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, FsError> {
        let (mut f, p) = self.open_checked(
            name,
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ),
        )?;
        let mut b = Vec::new();
        f.read_to_end(&mut b).map_err(|e| io_err(e, "read", &p))?;
        Ok(b)
    }

    /// Appends `data` (write-through + flush), creating the file if absent.
    pub fn append(&self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let (mut f, p) = self.open_checked(
            name,
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .share_mode(FILE_SHARE_READ),
        )?;
        f.write_all(data).map_err(|e| io_err(e, "write", &p))?;
        f.sync_all().map_err(|e| io_err(e, "flush", &p))
    }

    /// Replaces `name` atomically: new temp file under a random name (`CREATE_NEW`, so a
    /// planted link or file at that name fails the call), verified, written, flushed, then
    /// renamed over the target with `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`.
    pub fn replace(&self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let target = self.child(name)?;
        let tmp_name = format!("{name}.{:016x}.pulsar-tmp", random_u64());
        let (mut f, tmp) = self.open_checked(
            &tmp_name,
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0),
        )?;
        let wt = wide(&tmp.display().to_string());
        let result = (|| {
            f.write_all(data).map_err(|e| io_err(e, "write", &tmp))?;
            f.sync_all().map_err(|e| io_err(e, "flush", &tmp))?;
            drop(f);
            let wd = wide(&target.display().to_string());
            // SAFETY: NUL-terminated paths inside the verified, held-open folder.
            unsafe {
                MoveFileExW(
                    pcwstr(&wt),
                    pcwstr(&wd),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(|e| FsError::Io(format!("replace {}: {e}", target.display())))
        })();
        if result.is_err() {
            // SAFETY: NUL-terminated path of our own temp file.
            unsafe {
                let _ = DeleteFileW(pcwstr(&wt));
            }
        }
        result
    }
}

fn random_u64() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // RandomState is seeded from the OS RNG per process.
    RandomState::new().hash_one((t, std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ace(sid: &str, mask: u32) -> AceInfo {
        AceInfo {
            allow: true,
            sid: Some(sid.into()),
            mask,
        }
    }

    #[test]
    fn policy() {
        let admin_only = [ace(SID_SYSTEM, 0x1F01FF), ace(SID_ADMINISTRATORS, 0x1F01FF)];
        assert!(check_security(SID_SYSTEM, Some(&admin_only)).is_ok());
        assert!(check_security(SID_ADMINISTRATORS, Some(&admin_only)).is_ok());
        // a user as owner
        assert!(check_security("S-1-5-21-1-2-3-1001", Some(&admin_only)).is_err());
        // NULL DACL
        assert!(check_security(SID_SYSTEM, None).is_err());
        // Users read + execute is fine, Authenticated Users modify is not
        let mut v = admin_only.to_vec();
        v.push(ace("S-1-5-32-545", 0x1200A9));
        assert!(check_security(SID_SYSTEM, Some(&v)).is_ok());
        v.push(ace("S-1-5-11", 0x1301BF));
        assert!(
            check_security(SID_SYSTEM, Some(&v))
                .unwrap_err()
                .contains("S-1-5-11")
        );
        // ProgramData style "Users: create folders / append" (CI)
        let pd = [ace(SID_SYSTEM, 0x1F01FF), ace("S-1-5-32-545", 0x6)];
        assert!(check_security(SID_SYSTEM, Some(&pd)).is_err());
        // deny ACEs never count; object ACEs with write rights are refused
        let deny = [AceInfo {
            allow: false,
            sid: Some("S-1-1-0".into()),
            mask: 0x1F01FF,
        }];
        assert!(check_security(SID_SYSTEM, Some(&deny)).is_ok());
        let obj = [AceInfo {
            allow: true,
            sid: None,
            mask: 0x2,
        }];
        assert!(check_security(SID_SYSTEM, Some(&obj)).is_err());
        // CREATOR OWNER inherit-only full control is fine
        assert!(check_security(SID_SYSTEM, Some(&[ace(SID_CREATOR_OWNER, 0x1000_0000)])).is_ok());
    }

    #[test]
    fn path_normalisation() {
        assert_eq!(
            normalize_path(r"\\?\C:\VirtualDisplayDriver\"),
            normalize_path(r"c:\virtualdisplaydriver")
        );
        assert_ne!(normalize_path(r"C:\a"), normalize_path(r"C:\b"));
    }

    /// A folder the test user created is refused (owner and ACL are the user's). Creates
    /// and removes one empty folder under the user's temp folder; changes nothing else.
    #[test]
    fn user_folder_is_refused() {
        let d = std::env::temp_dir().join(format!("pulsar-display-test-{:x}", random_u64()));
        std::fs::create_dir(&d).unwrap();
        let r = TrustedDir::open(&d);
        let _ = std::fs::remove_dir(&d);
        match r {
            Err(FsError::Unsafe(_)) => {}
            other => panic!("expected Unsafe, got {other:?}"),
        }
        match TrustedDir::open(&d) {
            Err(FsError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
