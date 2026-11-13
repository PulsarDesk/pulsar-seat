// SPDX-License-Identifier: GPL-3.0-only
//! WTS session enumeration, state, log-off, disconnect / connect and user SIDs (adapted
//! from pulsar-split `split-session::win::wts`). Needs SYSTEM (SeTcbPrivilege) for
//! `WTSQueryUserToken`. **Untested on hardware** except the pure address decoding.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows_sys::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CLIENT_ADDRESS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTS_SESSION_INFOW, WTSClientAddress, WTSClientName,
    WTSConnectSessionW, WTSDisconnectSession, WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSINFOEXW, WTSIsRemoteSession, WTSLogoffSession, WTSQuerySessionInformationW, WTSQueryUserToken, WTSSessionInfoEx,
    WTSUserName,
};

use super::{from_wide_ptr, owned, raw, wide_secret};
use crate::error::SysError;
use crate::wts::{LISTENER_ID_MIN, Wts, WtsSession, WtsState};

const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;
/// `WTS_SESSIONSTATE_LOCK` (Windows 8 / Server 2012 and later).
const SESSIONSTATE_LOCK: i32 = 0;
const SESSIONSTATE_UNLOCK: i32 = 1;

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

/// Lock state of session `id` (`WTSSessionInfoEx` flags); `None` = unknown. Public for the
/// session agent's seat switch (it waits for its own lock before asking the service).
pub fn locked(id: u32) -> Option<bool> {
    let b = query_raw(id, WTSSessionInfoEx)?;
    if b.len() < std::mem::size_of::<WTSINFOEXW>() {
        return None;
    }
    // SAFETY: size checked; level 1 is the only level defined.
    let info: WTSINFOEXW = unsafe { std::ptr::read_unaligned(b.as_ptr().cast()) };
    if info.Level != 1 {
        return None;
    }
    // SAFETY: Level 1 selects the WTSInfoExLevel1 member of the union.
    match unsafe { info.Data.WTSInfoExLevel1.SessionFlags } {
        SESSIONSTATE_LOCK => Some(true),
        SESSIONSTATE_UNLOCK => Some(false),
        _ => None,
    }
}

/// `TokenUser` SID of a token as a string.
pub(crate) fn token_user_sid(token: HANDLE) -> Result<String, SysError> {
    let mut len = 0u32;
    // SAFETY: size query.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(SysError::last_win32("GetTokenInformation(TokenUser)"));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes, 8-aligned.
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(SysError::last_win32("GetTokenInformation(TokenUser)"));
    }
    // SAFETY: the buffer now starts with a TOKEN_USER.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID inside `buf`; string freed with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
        return Err(SysError::last_win32("ConvertSidToStringSidW"));
    }
    // SAFETY: NUL-terminated, LocalAlloc'd.
    let out = unsafe { from_wide_ptr(s) };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(s as HLOCAL) };
    Ok(out)
}

/// `WTSQueryUserToken` (SYSTEM only).
pub(crate) fn user_token(id: u32) -> Result<std::os::windows::io::OwnedHandle, SysError> {
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: out pointer valid; handle owned below.
    if unsafe { WTSQueryUserToken(id, &mut h) } == 0 {
        return Err(SysError::last_win32("WTSQueryUserToken"));
    }
    owned(h, "WTSQueryUserToken")
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsWts;

impl Wts for WindowsWts {
    fn sessions(&self) -> Result<Vec<WtsSession>, SysError> {
        let mut p: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: level-1 enumeration; freed below.
        if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut p, &mut n) } == 0 {
            return Err(SysError::last_win32("WTSEnumerateSessionsW"));
        }
        // SAFETY: `n` entries returned by the API.
        let rows = unsafe { std::slice::from_raw_parts(p, n as usize) };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id = r.SessionId;
            // SAFETY: NUL-terminated name owned by the enumeration buffer.
            let winstation = unsafe { from_wide_ptr(r.pWinStationName) };
            let state = WtsState::from_raw(r.State);
            let listener = id >= LISTENER_ID_MIN || state == WtsState::Listen;
            let q = |c| if listener { String::new() } else { query_string(id, c) };
            out.push(WtsSession {
                id,
                winstation,
                state,
                client_name: q(WTSClientName),
                user: q(WTSUserName),
                domain: q(WTSDomainName),
                client_address: if listener { None } else { query_address(id) },
                is_remote: !listener && is_remote(id),
                locked: if listener { None } else { locked(id) },
            });
        }
        // SAFETY: allocated by WTSEnumerateSessionsW.
        unsafe { WTSFreeMemory(p.cast()) };
        Ok(out)
    }

    fn logoff(&self, id: u32, require_remote: bool) -> Result<(), SysError> {
        if id == 0 || (require_remote && !is_remote(id)) {
            return Err(SysError::Other(format!("session {id} may not be logged off here")));
        }
        // SAFETY: plain call; asynchronous (bWait = FALSE).
        if unsafe { WTSLogoffSession(WTS_CURRENT_SERVER_HANDLE, id, 0) } == 0 {
            return Err(SysError::last_win32("WTSLogoffSession"));
        }
        Ok(())
    }

    fn disconnect(&self, id: u32) -> Result<(), SysError> {
        // SAFETY: plain call.
        if unsafe { WTSDisconnectSession(WTS_CURRENT_SERVER_HANDLE, id, 1) } == 0 {
            return Err(SysError::last_win32("WTSDisconnectSession"));
        }
        Ok(())
    }

    fn connect(&self, id: u32, target: u32, password: Option<&seat_core::Secret>) -> Result<(), SysError> {
        let pw = wide_secret(password.map(|p| p.expose()).unwrap_or(""));
        // SAFETY: valid NUL-terminated password buffer (wiped on drop).
        if unsafe { WTSConnectSessionW(id, target, pw.as_ptr(), 1) } == 0 {
            return Err(SysError::last_win32("WTSConnectSessionW"));
        }
        Ok(())
    }

    fn console_session(&self) -> Option<u32> {
        // SAFETY: plain call.
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        (id != u32::MAX).then_some(id)
    }

    fn user_sid(&self, id: u32) -> Option<String> {
        let t = user_token(id).ok()?;
        token_user_sid(raw(&t)).ok()
    }

    fn account_sid(&self, domain: &str, user: &str) -> Option<String> {
        account_sid(domain, user)
    }
}

/// NetBIOS name of this computer (the "domain" of local accounts).
pub fn computer_name() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
    let mut buf = [0u16; 64];
    let mut n = buf.len() as u32;
    // SAFETY: buffer of n UTF-16 units.
    (unsafe { GetComputerNameExW(ComputerNameNetBIOS, buf.as_mut_ptr(), &mut n) } != 0)
        .then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// `LookupAccountNameW` of `DOMAIN\user` (`"."` = this computer, so a local name never
/// resolves to a domain account). Only user accounts whose referenced domain is the one
/// asked for count.
pub fn account_sid(domain: &str, user: &str) -> Option<String> {
    use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE, SidTypeUser};
    if user.is_empty() || user.contains('\\') || domain.contains('\\') {
        return None;
    }
    let dom = if domain == "." || domain.is_empty() { computer_name()? } else { domain.to_owned() };
    let name = super::wide(&format!("{dom}\\{user}"));
    let mut sid = vec![0u64; 16];
    let mut cb = (sid.len() * 8) as u32;
    let mut rdom = [0u16; 256];
    let mut cch = rdom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers sized as passed; SID buffer 8-aligned.
    let ok = unsafe {
        LookupAccountNameW(std::ptr::null(), name.as_ptr(), sid.as_mut_ptr().cast(), &mut cb, rdom.as_mut_ptr(), &mut cch, &mut use_)
    };
    if ok == 0 || use_ != SidTypeUser {
        return None;
    }
    let referenced = String::from_utf16_lossy(&rdom[..cch as usize]);
    if !referenced.eq_ignore_ascii_case(&dom) {
        return None;
    }
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID in `sid`; string freed with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut s) } == 0 {
        return None;
    }
    // SAFETY: NUL-terminated, LocalAlloc'd.
    let out = unsafe { from_wide_ptr(s) };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(s as HLOCAL) };
    Some(out)
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

    #[test]
    fn enumeration_runs_readonly() {
        // Read-only: any user may enumerate sessions on the local machine.
        let s = WindowsWts.sessions().unwrap();
        assert!(s.iter().any(|x| x.id == 0 || x.winstation.eq_ignore_ascii_case("Services")) || !s.is_empty());
    }
}
