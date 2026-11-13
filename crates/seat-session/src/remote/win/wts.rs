// SPDX-License-Identifier: GPL-3.0-only
//! Remote seats' WTS access over the one shared enumeration ([`crate::win::wts::WindowsWts`],
//! merge step 7): enumeration, log-off of remote sessions only, user tokens, console user.

use crate::remote::discover::{Wts, WtsSession};
use crate::remote::{OwnedHandle, SessionError};
use crate::wts::Wts as _;
use windows_sys::Win32::Foundation::{ERROR_NO_TOKEN, GetLastError, HANDLE};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTSGetActiveConsoleSessionId, WTSIsRemoteSession, WTSLogoffSession, WTSQuerySessionInformationW,
    WTSQueryUserToken, WTSFreeMemory,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsWts;

fn is_remote(id: u32) -> bool {
    let mut p: *mut u16 = std::ptr::null_mut();
    let mut bytes = 0u32;
    // SAFETY: output freed with WTSFreeMemory; the value is a BOOL.
    unsafe {
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, id, WTSIsRemoteSession, &mut p, &mut bytes) == 0 || p.is_null() {
            return false;
        }
        let r = bytes as usize >= std::mem::size_of::<i32>() && *(p as *const i32) != 0;
        WTSFreeMemory(p.cast());
        r
    }
}

impl Wts for WindowsWts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError> {
        Ok(crate::win::wts::WindowsWts.sessions()?)
    }

    fn logoff(&self, id: u32, wait: bool) -> Result<(), SessionError> {
        // Never the console or a local session (spec/remote 30 §9.1).
        if !is_remote(id) {
            return Err(SessionError::InvalidArgument(format!("session {id} is not a remote session")));
        }
        // SAFETY: plain call.
        if unsafe { WTSLogoffSession(WTS_CURRENT_SERVER_HANDLE, id, i32::from(wait)) } == 0 {
            return Err(SessionError::last_win32("WTSLogoffSession"));
        }
        Ok(())
    }

    fn user_token(&self, id: u32) -> Result<OwnedHandle, SessionError> {
        crate::win::wts::user_token(id).map_err(|e| SessionError::Token(format!("WTSQueryUserToken({id}) failed: {e}")))
    }

    fn console_user_sid(&self) -> Result<Option<String>, SessionError> {
        // SAFETY: plain call.
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        if id == u32::MAX {
            return Ok(None);
        }
        let mut h: HANDLE = std::ptr::null_mut();
        // SAFETY: needs SeTcbPrivilege; ERROR_NO_TOKEN = nobody signed in at the console.
        if unsafe { WTSQueryUserToken(id, &mut h) } == 0 {
            // SAFETY: plain thread-local read.
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_TOKEN {
                return Ok(None);
            }
            return Err(SessionError::Token(format!("WTSQueryUserToken(console {id}) failed: {code}")));
        }
        let tok = super::owned(h, "WTSQueryUserToken")?;
        Ok(crate::win::wts::token_user_sid(super::raw(&tok)).map(Some)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_address_decoding_is_the_shared_one() {
        let mut a = [0u8; 20];
        a[2..6].copy_from_slice(&[127, 0, 0, 1]);
        assert_eq!(crate::win::wts::decode_client_address(2, &a), Some("127.0.0.1".parse().unwrap()));
        let mut b = [0u8; 20];
        b[17] = 1;
        assert_eq!(crate::win::wts::decode_client_address(23, &b), Some("::1".parse().unwrap()));
        assert_eq!(crate::win::wts::decode_client_address(0, &a), None);
    }

    #[test]
    fn enumeration_is_read_only_and_never_logs_off_local_sessions() {
        let all = WindowsWts.sessions().unwrap();
        assert!(all.iter().any(|s| s.is_system_or_listener()));
        // Session 0 is never remote: the log-off guard refuses before any WTS call.
        assert!(WindowsWts.logoff(0, false).is_err());
    }
}
