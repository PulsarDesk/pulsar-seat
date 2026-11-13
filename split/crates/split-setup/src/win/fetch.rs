// SPDX-License-Identifier: GPL-3.0-only
//! HTTPS download client over WinHTTP (system proxy, system certificate validation,
//! redirects followed but never HTTPS -> HTTP). A client only: no listener, no server.

use super::wide;
use crate::drivers::Fetcher;
use crate::error::SetupError;
use std::ffi::c_void;
use std::io::Write;
use std::path::Path;
use windows_sys::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
};

/// Upper bound for one artefact (driver installers are < 64 MiB).
pub const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy)]
pub struct WinHttpFetcher;

/// `https://host[:port]/path?query` -> (host, port, path+query). Only https.
pub fn split_https_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, INTERNET_DEFAULT_HTTPS_PORT),
    };
    Some((host.to_string(), port, path.to_string()))
}

struct H(*mut c_void);

impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle returned by WinHttpOpen/Connect/OpenRequest.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn fail(url: &str, what: &str) -> SetupError {
    SetupError::Download { url: url.into(), reason: format!("{what}: {}", std::io::Error::last_os_error()) }
}

impl Fetcher for WinHttpFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), SetupError> {
        let (host, port, path) =
            split_https_url(url).ok_or_else(|| SetupError::Download { url: url.into(), reason: "not an https url".into() })?;
        let agent = wide("PulsarSplitSetup/1");
        let whost = wide(&host);
        let wpath = wide(&path);
        let verb = wide("GET");
        // SAFETY: WinHTTP synchronous API; handles closed by the guards in reverse order.
        unsafe {
            let session = H(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0));
            if session.0.is_null() {
                return Err(fail(url, "WinHttpOpen"));
            }
            let policy = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
            WinHttpSetOption(session.0, WINHTTP_OPTION_REDIRECT_POLICY, &policy as *const u32 as *const c_void, 4);
            let conn = H(WinHttpConnect(session.0, whost.as_ptr(), port, 0));
            if conn.0.is_null() {
                return Err(fail(url, "WinHttpConnect"));
            }
            let req = H(WinHttpOpenRequest(
                conn.0,
                verb.as_ptr(),
                wpath.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if req.0.is_null() {
                return Err(fail(url, "WinHttpOpenRequest"));
            }
            if WinHttpSendRequest(req.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
                return Err(fail(url, "WinHttpSendRequest"));
            }
            if WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0 {
                return Err(fail(url, "WinHttpReceiveResponse"));
            }
            let mut status = 0u32;
            let mut len = 4u32;
            if WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                &mut status as *mut u32 as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(fail(url, "WinHttpQueryHeaders"));
            }
            if status != 200 {
                return Err(SetupError::Download { url: url.into(), reason: format!("HTTP {status}") });
            }
            let mut f = std::fs::File::create(dest).map_err(|e| SetupError::io(dest, e))?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0u64;
            loop {
                let mut read = 0u32;
                if WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) == 0 {
                    drop(f);
                    let _ = std::fs::remove_file(dest);
                    return Err(fail(url, "WinHttpReadData"));
                }
                if read == 0 {
                    break;
                }
                total += u64::from(read);
                if total > MAX_DOWNLOAD {
                    drop(f);
                    let _ = std::fs::remove_file(dest);
                    return Err(SetupError::Download { url: url.into(), reason: "too large".into() });
                }
                f.write_all(&buf[..read as usize]).map_err(|e| SetupError::io(dest, e))?;
            }
            f.sync_all().map_err(|e| SetupError::io(dest, e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        assert_eq!(
            split_https_url("https://github.com/a/b/releases/download/v1/x.exe?y=1"),
            Some(("github.com".into(), 443, "/a/b/releases/download/v1/x.exe?y=1".into()))
        );
        assert_eq!(split_https_url("https://h:8443"), Some(("h".into(), 8443, "/".into())));
        assert!(split_https_url("http://h/x").is_none());
        assert!(split_https_url("https://user@h/x").is_none());
        assert!(WinHttpFetcher.fetch("http://example.invalid/x", Path::new("unused")).is_err());
    }
}
