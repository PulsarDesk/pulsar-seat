// SPDX-License-Identifier: GPL-3.0-only
//! Windows transport of the update check ([`crate::update::UpdateNet`]): WinHTTP (system
//! TLS and proxy settings, TLS 1.2+ only, redirects followed by hand so every hop is checked
//! against [`crate::update::url_allowed`]), `WinVerifyTrust` for the installer's Authenticode
//! signature (whole-chain revocation check), SHA-256 streamed with `sha2`, and `msiexec`
//! started detached.
//!
//! Untested against a real release (none published yet); the request path was only built.

use super::security::{from_wide, wide, wide_path};
use crate::update::{Fail, InstallerRun, Signer, USER_AGENT, UpdateNet, url_allowed};
use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Networking::WinHttp::*;

/// Handle that closes itself.
struct H(*mut c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a WinHTTP handle we opened.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn last(op: &str) -> Fail {
    Fail::new("network", format!("{op}: {}", std::io::Error::last_os_error()))
}

fn split(url: &str) -> Result<(String, String), Fail> {
    if !url_allowed(url) {
        return Err(Fail::new("network", format!("refused URL {url}")));
    }
    let rest = &url["https://".len()..];
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Ok((host.to_string(), path.to_string()))
}

/// Receives each body chunk and the running total.
type Sink<'a> = dyn FnMut(&[u8], u64) -> Result<(), Fail> + 'a;

pub struct WinUpdateNet {
    /// `--console` dev mode: plain folder instead of the protected one.
    pub console: bool,
}

impl WinUpdateNet {
    /// GET `url` and feed the body to `sink`, following up to 5 checked redirects.
    fn fetch(&self, url: &str, max: u64, sink: &mut Sink<'_>) -> Result<u64, Fail> {
        let ua = wide(USER_AGENT);
        // SAFETY: valid wide strings; null proxy names = automatic proxy.
        let session = H(unsafe {
            WinHttpOpen(ua.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0)
        });
        if session.0.is_null() {
            return Err(last("WinHttpOpen"));
        }
        let protocols: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        // SAFETY: DWORD option on our session handle. TLS 1.3 may be unknown on old builds:
        // then retry with TLS 1.2 only.
        unsafe {
            if WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &protocols as *const u32 as *const c_void, 4) == 0 {
                let p12: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2;
                WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, &p12 as *const u32 as *const c_void, 4);
            }
            WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
        }
        let mut url = url.to_string();
        for _hop in 0..6 {
            let (host, path) = split(&url)?;
            let host_w = wide(&host);
            // SAFETY: open session; 443 = INTERNET_DEFAULT_HTTPS_PORT.
            let conn = H(unsafe { WinHttpConnect(session.0, host_w.as_ptr(), 443, 0) });
            if conn.0.is_null() {
                return Err(last("WinHttpConnect"));
            }
            let verb = wide("GET");
            let path_w = wide(&path);
            // SAFETY: open connection; null version / referrer / accept types = defaults.
            let req = H(unsafe {
                WinHttpOpenRequest(
                    conn.0,
                    verb.as_ptr(),
                    path_w.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    WINHTTP_FLAG_SECURE,
                )
            });
            if req.0.is_null() {
                return Err(last("WinHttpOpenRequest"));
            }
            // Redirects must stay manual (every hop goes through `split` / `url_allowed`):
            // if WinHTTP cannot be told so, the request is not sent at all. The redirect
            // policy NEVER is a second guard on the same request.
            let no_redirect: u32 = WINHTTP_DISABLE_REDIRECTS;
            let never: u32 = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
            // SAFETY: DWORD options on our request handle.
            unsafe {
                if WinHttpSetOption(req.0, WINHTTP_OPTION_DISABLE_FEATURE, &no_redirect as *const u32 as *const c_void, 4) == 0 {
                    return Err(last("disable redirects"));
                }
                if WinHttpSetOption(req.0, WINHTTP_OPTION_REDIRECT_POLICY, &never as *const u32 as *const c_void, 4) == 0 {
                    return Err(last("redirect policy"));
                }
            }
            let accept = if host.eq_ignore_ascii_case("api.github.com") {
                "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n"
            } else {
                "Accept: application/octet-stream\r\n"
            };
            let hdr = wide(accept);
            // SAFETY: open request; headers NUL-terminated (length u32::MAX = "up to NUL").
            let sent = unsafe { WinHttpSendRequest(req.0, hdr.as_ptr(), u32::MAX, std::ptr::null(), 0, 0, 0) };
            if sent == 0 {
                return Err(last("WinHttpSendRequest"));
            }
            // SAFETY: request sent.
            if unsafe { WinHttpReceiveResponse(req.0, std::ptr::null_mut()) } == 0 {
                return Err(last("WinHttpReceiveResponse"));
            }
            let mut status: u32 = 0;
            let mut len: u32 = 4;
            // SAFETY: numeric status into a DWORD.
            let ok = unsafe {
                WinHttpQueryHeaders(
                    req.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    std::ptr::null(),
                    &mut status as *mut u32 as *mut c_void,
                    &mut len,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(last("WinHttpQueryHeaders"));
            }
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let mut buf = vec![0u16; 4096];
                let mut blen = (buf.len() * 2) as u32;
                // SAFETY: wide buffer of blen bytes.
                let ok = unsafe {
                    WinHttpQueryHeaders(
                        req.0,
                        WINHTTP_QUERY_LOCATION,
                        std::ptr::null(),
                        buf.as_mut_ptr() as *mut c_void,
                        &mut blen,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(last("redirect without Location"));
                }
                url = from_wide(&buf[..(blen as usize / 2)]);
                continue;
            }
            if status != 200 {
                return Err(Fail::new("network", format!("HTTP {status} from {host}")));
            }
            let mut total = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut got: u32 = 0;
                // SAFETY: buffer of buf.len() bytes.
                let ok = unsafe { WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut got) };
                if ok == 0 {
                    return Err(last("WinHttpReadData"));
                }
                if got == 0 {
                    return Ok(total);
                }
                total += u64::from(got);
                if total > max {
                    return Err(Fail::new("too_large", format!("more than {max} bytes from {host}")));
                }
                sink(&buf[..got as usize], total)?;
            }
        }
        Err(Fail::new("network", "too many redirects"))
    }
}

/// Leaf signer of a verified WinVerifyTrust state: subject and issuer simple names and
/// the SHA-256 of the DER-encoded SubjectPublicKeyInfo (the optional pin, `update::AUTHENTICODE_PINS`).
///
/// # Safety
/// `state` is the `hWVTStateData` of a successful verify, not yet closed.
unsafe fn signer_of(state: windows_sys::Win32::Foundation::HANDLE) -> Result<Signer, Fail> {
    use sha2::{Digest, Sha256};
    use windows_sys::Win32::Security::Cryptography::{
        CERT_NAME_ISSUER_FLAG, CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW, CryptEncodeObjectEx, PKCS_7_ASN_ENCODING,
        X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO,
    };
    use windows_sys::Win32::Security::WinTrust::*;
    // SAFETY: per the function contract; the certificate context lives as long as `state`.
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        let sgnr = if prov.is_null() { std::ptr::null_mut() } else { WTHelperGetProvSignerFromChain(prov, 0, 0, 0) };
        if sgnr.is_null() || (*sgnr).pasCertChain.is_null() || (*(*sgnr).pasCertChain).pCert.is_null() {
            return Err(Fail::new("unsigned", "no signer certificate"));
        }
        let cert = (*(*sgnr).pasCertChain).pCert;
        let name = |flags: u32| {
            let mut buf = [0u16; 512];
            let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, flags, std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32);
            from_wide(&buf[..(n as usize).saturating_sub(1).min(buf.len())])
        };
        let info = (*cert).pCertInfo;
        if info.is_null() {
            return Err(Fail::new("unsigned", "signer certificate without info"));
        }
        let spki = &(*info).SubjectPublicKeyInfo as *const _ as *const c_void;
        let enc = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
        let mut len: u32 = 0;
        if CryptEncodeObjectEx(enc, X509_PUBLIC_KEY_INFO, spki, 0, std::ptr::null(), std::ptr::null_mut(), &mut len) == 0 || len == 0 {
            return Err(Fail::new("unsigned", format!("encode public key: {}", std::io::Error::last_os_error())));
        }
        let mut der = vec![0u8; len as usize];
        if CryptEncodeObjectEx(enc, X509_PUBLIC_KEY_INFO, spki, 0, std::ptr::null(), der.as_mut_ptr() as *mut c_void, &mut len) == 0 {
            return Err(Fail::new("unsigned", format!("encode public key: {}", std::io::Error::last_os_error())));
        }
        der.truncate(len as usize);
        Ok(Signer {
            subject: name(0),
            issuer: name(CERT_NAME_ISSUER_FLAG),
            spki_sha256: Sha256::digest(&der).iter().map(|b| format!("{b:02x}")).collect(),
        })
    }
}

/// msiexec started by [`WinUpdateNet::launch_installer`].
struct MsiRun(std::process::Child);

impl InstallerRun for MsiRun {
    fn try_wait(&mut self) -> Result<Option<i32>, Fail> {
        match self.0.try_wait() {
            Ok(Some(st)) => Ok(Some(st.code().unwrap_or(-1))),
            Ok(None) => Ok(None),
            Err(e) => Err(Fail::new("install_failed", format!("wait for msiexec: {e}"))),
        }
    }
}

impl UpdateNet for WinUpdateNet {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, Fail> {
        let mut out = Vec::new();
        self.fetch(url, max, &mut |b, _| {
            out.extend_from_slice(b);
            Ok(())
        })?;
        Ok(out)
    }

    fn download(&self, url: &str, dest: &Path, max: u64, progress: &dyn Fn(u64)) -> Result<u64, Fail> {
        let mut f = std::fs::File::create(dest).map_err(|e| Fail::new("launch", format!("{}: {e}", dest.display())))?;
        let n = self.fetch(url, max, &mut |b, total| {
            f.write_all(b).map_err(|e| Fail::new("launch", format!("write: {e}")))?;
            progress(total);
            Ok(())
        })?;
        f.sync_all().map_err(|e| Fail::new("launch", format!("flush: {e}")))?;
        Ok(n)
    }

    fn signer(&self, file: &Path) -> Result<Signer, Fail> {
        use windows_sys::Win32::Security::WinTrust::*;
        let path_w = wide_path(file);
        // SAFETY: zeroed POD structs, filled below.
        let mut fi: WINTRUST_FILE_INFO = unsafe { std::mem::zeroed() };
        fi.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
        fi.pcwszFilePath = path_w.as_ptr();
        let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
        data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
        data.dwProvFlags = WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        data.Anonymous.pFile = &mut fi;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: valid structures (null hwnd = no UI).
        let rc = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut c_void) };
        let result = if rc != 0 {
            Err(Fail::new("unsigned", format!("Authenticode check failed (0x{:08x})", rc as u32)))
        } else {
            // SAFETY: state valid until the close below.
            unsafe { signer_of(data.hWVTStateData) }
        };
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        // SAFETY: closes the state opened above.
        unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut c_void) };
        result
    }

    fn sha256(&self, file: &Path) -> Result<String, Fail> {
        use sha2::{Digest, Sha256};
        let mut f = std::fs::File::open(file).map_err(|e| Fail::new("launch", format!("{}: {e}", file.display())))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf).map_err(|e| Fail::new("launch", format!("read: {e}")))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    fn launch_installer(&self, msi: &Path, log: &Path) -> Result<Box<dyn InstallerRun>, Fail> {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, DETACHED_PROCESS};
        let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let exe = sys.join("System32").join("msiexec.exe");
        let mk = |flags: u32| {
            std::process::Command::new(&exe)
                .arg("/i")
                .arg(msi)
                .args(["/qn", "/norestart", "/l*v"])
                .arg(log)
                .creation_flags(flags)
                .spawn()
        };
        // The service stops during the upgrade: msiexec must not be in a job that dies with it.
        let base = CREATE_NO_WINDOW | DETACHED_PROCESS;
        match mk(base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| mk(base)) {
            Ok(child) => Ok(Box::new(MsiRun(child))),
            Err(e) => Err(Fail::new("launch", format!("msiexec: {e}"))),
        }
    }

    fn native_arch(&self) -> Option<&'static str> {
        use windows_sys::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
        let mut process = 0u16;
        let mut native = 0u16;
        // SAFETY: pseudo handle; out pointers valid.
        if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
            return None;
        }
        match native {
            IMAGE_FILE_MACHINE_ARM64 => Some("arm64"),
            IMAGE_FILE_MACHINE_AMD64 => Some("x64"),
            _ => None,
        }
    }

    fn prepare_dir(&self, dir: &Path) -> Result<(), Fail> {
        let r = if self.console {
            std::fs::create_dir_all(dir)
        } else {
            super::security::ensure_dir(dir, &format!("O:BA{}", seat_core::consts::PRIVATE_DIR_SDDL), true)
        };
        r.map_err(|e| Fail::new("launch", format!("{}: {e}", dir.display())))
    }
}
