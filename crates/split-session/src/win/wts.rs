// SPDX-License-Identifier: GPL-3.0-only
//! WTS session enumeration, log-off and user tokens (spec 30 §6, §8, §9).

use super::{from_wide_ptr, owned};
use crate::discover::{Wts, WtsSession, WtsState};
use crate::{OwnedHandle, SessionError};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use windows_sys::Win32::Foundation::{ERROR_NO_TOKEN, GetLastError, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CLIENT_ADDRESS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTS_SESSION_INFOW, WTSClientAddress, WTSClientName, WTSDomainName,
    WTSEnumerateSessionsW, WTSFreeMemory, WTSIsRemoteSession, WTSLogoffSession, WTSQuerySessionInformationW, WTSQueryUserToken,
    WTSUserName,
};

const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

/// Decodes `WTS_CLIENT_ADDRESS`: IPv4 sits in `Address[2..6]`, IPv6 in `Address[2..18]`
/// (HYPOTHESIS for IPv6: the docs only spell out IPv4).
pub fn decode_client_address(family: u32, a: &[u8; 20]) -> Option<IpAddr> {
    match family {
        AF_INET => Some(IpAddr::V4(Ipv4Addr::new(a[2], a[3], a[4], a[5]))),
        AF_INET6 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(&a[2..18]);
            Some(IpAddr::V6(Ipv6Addr::from(b)))
        }
        _ => None,
    }
}

fn query_raw(id: u32, class: WTS_INFO_CLASS) -> Option<Vec<u8>> {
    let mut p: *mut u16 = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: buffer freed with WTSFreeMemory.
    unsafe {
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, id, class, &mut p, &mut len) == 0 || p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p.cast::<u8>(), len as usize).to_vec();
        WTSFreeMemory(p.cast());
        Some(v)
    }
}

fn query_string(id: u32, class: WTS_INFO_CLASS) -> String {
    let Some(b) = query_raw(id, class) else { return String::new() };
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn query_address(id: u32) -> Option<IpAddr> {
    let b = query_raw(id, WTSClientAddress)?;
    if b.len() < std::mem::size_of::<WTS_CLIENT_ADDRESS>() {
        return None;
    }
    // SAFETY: size checked; unaligned read of a plain struct.
    let a: WTS_CLIENT_ADDRESS = unsafe { std::ptr::read_unaligned(b.as_ptr().cast()) };
    decode_client_address(a.AddressFamily, &a.Address)
}

fn is_remote(id: u32) -> bool {
    query_raw(id, WTSIsRemoteSession).is_some_and(|b| b.first().is_some_and(|x| *x != 0))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsWts;

impl Wts for WindowsWts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError> {
        let mut p: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: level-1 enumeration; freed below.
        if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut p, &mut n) } == 0 {
            return Err(SessionError::last_win32("WTSEnumerateSessionsW"));
        }
        // SAFETY: `n` entries returned by the API.
        let rows = unsafe { std::slice::from_raw_parts(p, n as usize) };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id = r.SessionId;
            // SAFETY: NUL-terminated name owned by the enumeration buffer.
            let winstation = unsafe { from_wide_ptr(r.pWinStationName) };
            let state = WtsState::from_raw(r.State);
            let listener = id >= crate::discover::LISTENER_ID_MIN || state == WtsState::Listen;
            out.push(WtsSession {
                id,
                winstation,
                state,
                client_name: if listener { String::new() } else { query_string(id, WTSClientName) },
                user: if listener { String::new() } else { query_string(id, WTSUserName) },
                domain: if listener { String::new() } else { query_string(id, WTSDomainName) },
                client_address: if listener { None } else { query_address(id) },
                is_remote: !listener && is_remote(id),
            });
        }
        // SAFETY: allocated by WTSEnumerateSessionsW.
        unsafe { WTSFreeMemory(p.cast()) };
        Ok(out)
    }

    fn logoff(&self, id: u32, wait: bool) -> Result<(), SessionError> {
        // Never the console or a local session (spec 30 §9.1).
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
        let mut h: HANDLE = std::ptr::null_mut();
        // SAFETY: needs SeTcbPrivilege (service as SYSTEM); handle owned below.
        if unsafe { WTSQueryUserToken(id, &mut h) } == 0 {
            return Err(SessionError::Token(format!("WTSQueryUserToken({id}) failed: {}", SessionError::last_win32("WTSQueryUserToken"))));
        }
        owned(h, "WTSQueryUserToken")
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
        let tok = owned(h, "WTSQueryUserToken")?;
        token_user_sid(super::raw(&tok)).map(Some)
    }
}

/// `TokenUser` SID of a token as a string.
fn token_user_sid(token: HANDLE) -> Result<String, SessionError> {
    let mut len = 0u32;
    // SAFETY: size query.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(SessionError::last_win32("GetTokenInformation(TokenUser)"));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes, 8-aligned.
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(SessionError::last_win32("GetTokenInformation(TokenUser)"));
    }
    // SAFETY: the buffer now starts with a TOKEN_USER.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID inside `buf`; string freed with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
        return Err(SessionError::last_win32("ConvertSidToStringSidW"));
    }
    // SAFETY: NUL-terminated, LocalAlloc'd.
    let out = unsafe { from_wide_ptr(s) };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(s as HLOCAL) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_address_decoding() {
        let mut a = [0u8; 20];
        a[2..6].copy_from_slice(&[127, 0, 0, 1]);
        assert_eq!(decode_client_address(AF_INET, &a), Some("127.0.0.1".parse().unwrap()));
        let mut b = [0u8; 20];
        b[17] = 1;
        assert_eq!(decode_client_address(AF_INET6, &b), Some("::1".parse().unwrap()));
        assert_eq!(decode_client_address(0, &a), None);
    }
}
