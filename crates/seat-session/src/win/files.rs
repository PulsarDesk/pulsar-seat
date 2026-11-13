// SPDX-License-Identifier: GPL-3.0-only
//! File facts: binary version, Authenticode, DACL, final local path, protected directories.
//! Adapted from pulsar-split `split-termwrap`.

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CERT_E_REVOCATION_FAILURE, CERT_E_REVOKED, CRYPT_E_NO_REVOCATION_CHECK, CRYPT_E_REVOCATION_OFFLINE,
    CRYPT_E_REVOKED, HANDLE, HLOCAL, LocalFree, TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN,
    TRUST_E_SUBJECT_FORM_UNKNOWN,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL,
    WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WinVerifyTrust,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, GetDriveTypeW, GetFileVersionInfoSizeW,
    GetFileVersionInfoW, GetFinalPathNameByHandleW, VOLUME_NAME_DOS, VS_FIXEDFILEINFO, VerQueryValueW,
};

use super::{wide, wide_path};
use crate::acl::{Ace, SecurityInfo};
use crate::pe::SignatureState;
use crate::version::FileVersion;

/// `VS_FIXEDFILEINFO` file version (never the string resource).
pub fn file_version(path: &Path) -> Option<FileVersion> {
    let p = wide_path(path);
    let mut handle = 0u32;
    // SAFETY: p is NUL-terminated.
    let size = unsafe { GetFileVersionInfoSizeW(p.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buf has `size` bytes.
    if unsafe { GetFileVersionInfoW(p.as_ptr(), 0, size, buf.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let root = wide("\\");
    let mut info: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: buf holds the version block; out pointers are valid.
    if unsafe { VerQueryValueW(buf.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) } == 0
        || info.is_null()
        || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }
    // SAFETY: VerQueryValueW returned a pointer into buf of at least sizeof(VS_FIXEDFILEINFO).
    let fixed = unsafe { std::ptr::read_unaligned(info as *const VS_FIXEDFILEINFO) };
    if fixed.dwSignature != 0xFEEF_04BD {
        return None;
    }
    Some(FileVersion::from_ms_ls(fixed.dwFileVersionMS, fixed.dwFileVersionLS))
}

/// WinVerifyTrust, generic Authenticode policy, no UI. Revocation is checked for the whole
/// chain from cached CRL / OCSP data only (`WTD_CACHE_ONLY_URL_RETRIEVAL`: never goes to the
/// network). A revoked certificate is `Untrusted`. When revocation cannot be decided offline
/// (nothing cached), the signature counts as `Trusted` and a warning is logged: a cache-only
/// check cannot do better, and refusing would block every signed wrapper on a box that
/// never fetched that CRL. Checked on the resolved local path when it resolves.
pub fn signature(path: &Path) -> SignatureState {
    let canonical = local_fixed_path(path).ok();
    let p = wide_path(canonical.as_deref().unwrap_or(path));
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: p.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    data.Anonymous.pFile = &mut file;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: data/file live for both calls; hwnd -1 = no UI (INVALID_HANDLE_VALUE).
    let hr = unsafe { WinVerifyTrust(-1isize as _, &mut action, (&mut data as *mut WINTRUST_DATA).cast()) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: closes the state opened above.
    unsafe { WinVerifyTrust(-1isize as _, &mut action, (&mut data as *mut WINTRUST_DATA).cast()) };
    match hr {
        0 => SignatureState::Trusted,
        CERT_E_REVOKED | CRYPT_E_REVOKED => SignatureState::Untrusted,
        CERT_E_REVOCATION_FAILURE | CRYPT_E_REVOCATION_OFFLINE | CRYPT_E_NO_REVOCATION_CHECK => {
            log::warn!("termwrap: {}: revocation status unknown offline; signature accepted", path.display());
            SignatureState::Trusted
        }
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => SignatureState::Unsigned,
        _ => SignatureState::Untrusted,
    }
}

const DRIVE_FIXED: u32 = 3;
/// ACE types whose SID follows `ACE_HEADER` + `Mask` directly (allowed, denied, callback).
const SIMPLE_SID_ACE_TYPES: [u8; 4] = [0x0, 0x1, 0x9, 0xA];

fn sid_string(sid: *mut core::ffi::c_void) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: sid points to a SID inside a live security descriptor; out receives a
    // LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: out is NUL-terminated; freed with LocalFree once.
    let s = unsafe {
        while *out.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
        LocalFree(out as HLOCAL);
        s
    };
    Some(s)
}

struct FreeSd(PSECURITY_DESCRIPTOR);
impl Drop for FreeSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by Get(Named)SecurityInfoW.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// Copies owner + DACL out of pointers that live inside a security descriptor.
fn parse_security(owner: *mut core::ffi::c_void, dacl: *mut ACL) -> SecurityInfo {
    let owner = sid_string(owner);
    if dacl.is_null() {
        return SecurityInfo { owner, dacl: None };
    }
    // SAFETY: dacl points into the caller's live security descriptor.
    let count = unsafe { (*dacl).AceCount };
    let mut aces = Vec::with_capacity(usize::from(count));
    for i in 0..u32::from(count) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: i < AceCount.
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            // Unreadable ACE: record it as an allow ACE of unknown type (fail closed).
            aces.push(Ace { ace_type: 0xFF, flags: 0, mask: u32::MAX, sid: None });
            continue;
        }
        // SAFETY: every ACE starts with ACE_HEADER, followed by a 32-bit mask.
        let header = unsafe { std::ptr::read_unaligned(ace as *const ACE_HEADER) };
        // SAFETY: as above; the mask is at offset 4.
        let mask = unsafe { std::ptr::read_unaligned((ace as *const u8).add(4) as *const u32) };
        let sid = if SIMPLE_SID_ACE_TYPES.contains(&header.AceType) && header.AceSize >= 16 {
            // SAFETY: for these types the SID starts at offset 8 (after header + mask).
            sid_string(unsafe { (ace as *mut u8).add(8) }.cast())
        } else {
            None
        };
        aces.push(Ace { ace_type: header.AceType, flags: header.AceFlags, mask, sid });
    }
    SecurityInfo { owner, dacl: Some(aces) }
}

const SEC_INFO: u32 = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;

/// Owner + DACL of a named file or directory.
pub fn security_of_path(path: &Path) -> Result<SecurityInfo, String> {
    let p = wide_path(path);
    let mut owner: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: out pointers valid; sd freed by FreeSd.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            p.as_ptr(),
            SE_FILE_OBJECT,
            SEC_INFO,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let _free = FreeSd(sd);
    if rc != 0 {
        return Err(format!("GetNamedSecurityInfoW({}) failed: {rc}", path.display()));
    }
    Ok(parse_security(owner, dacl))
}

/// Creates one directory with the security descriptor given as SDDL (no inheritance from
/// the parent when the SDDL says `D:P`). An existing directory is left as is (the caller
/// verifies it).
pub fn create_dir_with_sddl(path: &Path, sddl: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let w = wide(sddl);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: w is NUL-terminated; sd receives a LocalAlloc'd descriptor freed by FreeSd.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut())
    };
    if ok == 0 {
        return Err(format!("bad SDDL: {}", super::last_error()));
    }
    let _free = FreeSd(sd);
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let p = wide_path(path);
    // SAFETY: p is NUL-terminated; sa and sd live for the call.
    if unsafe { CreateDirectoryW(p.as_ptr(), &sa) } == 0 {
        let e = super::last_error();
        if e != ERROR_ALREADY_EXISTS {
            return Err(format!("CreateDirectoryW({}) failed: {e}", path.display()));
        }
    }
    Ok(())
}

/// Owner + DACL of an open handle (needs READ_CONTROL). Checking the handle we read from
/// leaves no window between the check and the read.
pub fn security_of_handle(file: &std::fs::File) -> Result<SecurityInfo, String> {
    use std::os::windows::io::AsRawHandle;
    let handle = file.as_raw_handle() as HANDLE;
    let mut owner: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: handle is open; out pointers valid; sd freed by FreeSd.
    let rc = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            SEC_INFO,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let _free = FreeSd(sd);
    if rc != 0 {
        return Err(format!("GetSecurityInfo failed: {rc}"));
    }
    Ok(parse_security(owner, dacl))
}

/// Final (symlink / junction / subst resolved) path of an existing file or directory, as
/// `X:\...`. Refuses anything that is not on a local fixed drive (network share, mapped or
/// removable drive, device path): TermService must not load code from there, and the ACL
/// of a remote share is controlled by the remote side.
pub fn local_fixed_path(path: &Path) -> Result<PathBuf, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let f = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0u16; 1024];
    let mut n = 0u32;
    for _ in 0..2 {
        // SAFETY: f is open; buf has buf.len() elements.
        n = unsafe {
            GetFinalPathNameByHandleW(
                f.as_raw_handle() as HANDLE,
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if n == 0 {
            return Err(format!("GetFinalPathNameByHandleW({}) failed: {}", path.display(), super::last_error()));
        }
        if (n as usize) < buf.len() {
            break;
        }
        buf.resize(n as usize + 1, 0);
    }
    if n as usize >= buf.len() {
        return Err(format!("{}: final path too long", path.display()));
    }
    let s = String::from_utf16_lossy(&buf[..n as usize]);
    let local = classify_final_path(&s)
        .ok_or_else(|| format!("{}: resolves to {s}, not a local drive path", path.display()))?;
    let root = wide(&local[..3]);
    // SAFETY: root is a NUL-terminated "X:\".
    let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
    if ty != DRIVE_FIXED {
        return Err(format!("{}: drive {} is not a local fixed drive (type {ty})", path.display(), &local[..2]));
    }
    Ok(PathBuf::from(local))
}

/// Raw final path (`\\?\X:\...`, symlinks / junctions resolved) of an open handle.
pub fn final_path_of(file: &std::fs::File) -> Result<String, String> {
    use std::os::windows::io::AsRawHandle;
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        // SAFETY: file is open; buf has buf.len() elements.
        let n = unsafe {
            GetFinalPathNameByHandleW(file.as_raw_handle() as HANDLE, buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)
        } as usize;
        if n == 0 {
            return Err(format!("GetFinalPathNameByHandleW failed: {}", super::last_error()));
        }
        if n < buf.len() {
            return Ok(String::from_utf16_lossy(&buf[..n]));
        }
        buf.resize(n + 1, 0);
    }
    Err("final path too long".into())
}

/// The path itself (not its target) is a reparse point: junction, symlink, mount point.
pub fn is_reparse_point(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let m = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Opens (creates) a file that SYSTEM appends to, refusing every redirection a standard
/// user could plant: a reparse point as the file itself (`FILE_FLAG_OPEN_REPARSE_POINT`),
/// a junction / symlink anywhere in the path (final path of the handle must equal the
/// requested path), a hard link (link count 1) and a file someone else pre-created (owner
/// SYSTEM or Administrators). The handle has `READ_CONTROL` for the check.
pub fn open_trusted_append(path: &Path) -> Result<std::fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle};
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fin = final_path_of(&f)?;
    let expected = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !crate::acl::same_final_path(&fin, &expected.to_string_lossy()) {
        return Err(format!("{} resolves to {fin} (redirected; refused)", path.display()));
    }
    // SAFETY: plain-old-data out struct; open handle.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { GetFileInformationByHandle(f.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(format!("GetFileInformationByHandle({}) failed: {}", path.display(), super::last_error()));
    }
    if info.nNumberOfLinks != 1 {
        return Err(format!("{} has {} hard links (refused)", path.display(), info.nNumberOfLinks));
    }
    let sec = security_of_handle(&f)?;
    if let Some(why) = crate::acl::owner_untrusted_reason(&sec) {
        return Err(format!("{}: {why} (refused)", path.display()));
    }
    Ok(f)
}

/// Pure: `\\?\C:\x` -> `C:\x`; `\\?\UNC\...`, volume GUID or device paths -> None.
pub fn classify_final_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix(r"\\?\").unwrap_or(s);
    let b = rest.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\').then(|| rest.to_owned())
}

/// UTF-16LE with BOM, else UTF-8 (lossy; section names are ASCII).
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let w: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&w)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_path_classification() {
        assert_eq!(classify_final_path(r"\\?\C:\Windows\x.dll").as_deref(), Some(r"C:\Windows\x.dll"));
        assert_eq!(classify_final_path(r"\\?\UNC\server\share\x.dll"), None);
        assert_eq!(classify_final_path(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x.dll"), None);
        assert_eq!(classify_final_path(r"\\.\PhysicalDrive0"), None);
    }

    #[test]
    fn text_decoding() {
        assert_eq!(decode_text(b"[Main]"), "[Main]");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("[Main]".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_text(&utf16), "[Main]");
    }
}
