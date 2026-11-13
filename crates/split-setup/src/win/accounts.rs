// SPDX-License-Identifier: GPL-3.0-only
//! Local accounts: delete a seat user we created, with its profile (only with "remove
//! user data", spec 90 §4.3).

use super::wide;
use crate::error::SetupError;
use crate::system::UserAccounts;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NONE_MAPPED, ERROR_PATH_NOT_FOUND, HLOCAL, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::{NERR_UserNotFound, NetUserDel};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE, SidTypeUser};
use windows_sys::Win32::UI::Shell::DeleteProfileW;

#[derive(Debug, Default, Clone, Copy)]
pub struct WinAccounts;

/// String SID of a local user account; `None` when there is no such account.
fn local_user_sid(name: &str) -> Result<Option<String>, SetupError> {
    // `.\name` restricts the lookup to the local machine's account database.
    let w = wide(&format!(r".\{name}"));
    let mut sid = vec![0u8; 256];
    let mut sid_len = sid.len() as u32;
    let mut domain = vec![0u16; 256];
    let mut domain_len = domain.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers and their lengths are valid; on success `sid` holds a SID.
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            w.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_,
        )
    };
    if ok == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_NONE_MAPPED as i32) {
            return Ok(None);
        }
        return Err(SetupError::Other(format!("LookupAccountNameW({name}) failed: {e}")));
    }
    if use_ != SidTypeUser {
        return Err(SetupError::Other(format!("{name} is not a user account (SID type {use_})")));
    }
    let mut s: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; the string is freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut s) == 0 {
            return Err(SetupError::Other(format!("ConvertSidToStringSidW failed: {}", std::io::Error::last_os_error())));
        }
        let len = (0..).take_while(|&i| *s.add(i) != 0).count();
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, len));
        LocalFree(s as HLOCAL);
        Ok(Some(out))
    }
}

impl UserAccounts for WinAccounts {
    fn delete_user(&self, name: &str) -> Result<(), SetupError> {
        // Profile first (C:\Users\<seat> + ProfileList entry): it needs the SID, which is
        // gone once the account is deleted. A failure keeps the account so a later run
        // can retry both.
        if let Some(sid) = local_user_sid(name)? {
            let ws = wide(&sid);
            // SAFETY: NUL-terminated SID string; NULL path/computer = local profile.
            if unsafe { DeleteProfileW(ws.as_ptr(), std::ptr::null(), std::ptr::null()) } == 0 {
                let e = std::io::Error::last_os_error();
                let none = matches!(e.raw_os_error(), Some(c) if c == ERROR_FILE_NOT_FOUND as i32 || c == ERROR_PATH_NOT_FOUND as i32);
                if !none {
                    return Err(SetupError::Other(format!("DeleteProfileW({name}) failed: {e}")));
                }
            }
        }
        let w = wide(name);
        // SAFETY: local machine (NULL server), NUL-terminated name.
        let rc = unsafe { NetUserDel(std::ptr::null(), w.as_ptr()) };
        if rc == 0 || rc == NERR_UserNotFound {
            Ok(())
        } else {
            Err(SetupError::Other(format!("NetUserDel failed with {rc}")))
        }
    }
}

#[cfg(test)]
mod tests {
    /// Read-only: looks up accounts, deletes nothing.
    #[test]
    fn sid_lookup_read_only() {
        assert!(super::local_user_sid("pulsar-split-no-such-user-x9").unwrap().is_none());
    }
}
