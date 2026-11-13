// SPDX-License-Identifier: GPL-3.0-only
//! Read-only `SystemProbe` over the real machine: registry, SCM, files, LocalMachine
//! certificate stores, firewall rules and the PnP tree. Nothing here changes state.

use super::registry::{open_read, read_string};
use super::{devices::WinDevices, firewall::WinFirewall, wide};
use crate::coexist::{ServiceFacts, SystemProbe};
use crate::system::DeviceManager;
use std::path::Path;
use windows_sys::Win32::Security::Cryptography::{
    CERT_FIND_SHA1_HASH, CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_LOCAL_MACHINE,
    CRYPT_INTEGER_BLOB, CertCloseStore, CertFindCertificateInStore, CertFreeCertificateContext, CertOpenStore,
    PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatus,
    SC_MANAGER_CONNECT, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct WinProbe;

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Read a NUL-terminated UTF-16 string from a raw pointer (null -> empty).
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
pub(crate) unsafe fn pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

impl SystemProbe for WinProbe {
    fn reg_value(&self, path: &str, value: &str) -> Option<String> {
        read_string(path, value)
    }

    fn reg_key_exists(&self, path: &str) -> bool {
        open_read(path).is_some()
    }

    fn reg_subkeys(&self, path: &str) -> Vec<String> {
        open_read(path).map(|k| k.enum_keys().filter_map(Result::ok).collect()).unwrap_or_default()
    }

    fn reg_value_names(&self, path: &str) -> Vec<String> {
        open_read(path).map(|k| k.enum_values().filter_map(Result::ok).map(|(n, _)| n).collect()).unwrap_or_default()
    }

    fn service(&self, name: &str) -> Option<ServiceFacts> {
        let wname = wide(name);
        // SAFETY: plain SCM calls; every handle is closed below; buffers sized by the API.
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() {
                return None;
            }
            let svc = OpenServiceW(scm, wname.as_ptr(), SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG);
            if svc.is_null() {
                CloseServiceHandle(scm);
                return None;
            }
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            let running = QueryServiceStatus(svc, &mut st) != 0 && st.dwCurrentState == SERVICE_RUNNING;
            let mut needed = 0u32;
            QueryServiceConfigW(svc, std::ptr::null_mut(), 0, &mut needed);
            let (mut image_path, mut display_name) = (String::new(), String::new());
            if needed > 0 {
                // u64 buffer keeps the struct aligned.
                let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
                let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
                if QueryServiceConfigW(svc, cfg, needed, &mut needed) != 0 {
                    image_path = pwstr((*cfg).lpBinaryPathName);
                    display_name = pwstr((*cfg).lpDisplayName);
                }
            }
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            Some(ServiceFacts { running, image_path, display_name })
        }
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn cert_in_store(&self, store: &str, thumbprint: &str) -> bool {
        let Some(mut hash) = hex_to_bytes(thumbprint) else { return false };
        let wstore = wide(store);
        // SAFETY: read-only system store opened by name; context and store freed below.
        unsafe {
            let h = CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_READONLY_FLAG,
                wstore.as_ptr() as *const _,
            );
            if h.is_null() {
                return false;
            }
            let blob = CRYPT_INTEGER_BLOB { cbData: hash.len() as u32, pbData: hash.as_mut_ptr() };
            let ctx = CertFindCertificateInStore(
                h,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                CERT_FIND_SHA1_HASH,
                &blob as *const _ as *const _,
                std::ptr::null(),
            );
            let found = !ctx.is_null();
            if found {
                CertFreeCertificateContext(ctx);
            }
            CertCloseStore(h, 0);
            found
        }
    }

    fn firewall_rule_names(&self) -> Vec<String> {
        WinFirewall.all_rules().map(|v| v.into_iter().map(|(n, _)| n).collect()).unwrap_or_default()
    }

    fn devices_matching(&self, needle: &str) -> Vec<String> {
        let n = needle.to_ascii_uppercase();
        WinDevices
            .list()
            .map(|v| {
                v.into_iter()
                    .filter(|d| {
                        d.instance_id.to_ascii_uppercase().contains(&n)
                            || d.hardware_ids.iter().any(|h| h.to_ascii_uppercase().contains(&n))
                    })
                    .map(|d| d.instance_id)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only smoke test against the dev machine: never writes anything.
    #[test]
    fn read_only_probe_smoke() {
        let p = WinProbe;
        assert!(p.reg_key_exists(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"));
        assert!(p.reg_value(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber").is_some());
        assert!(!p.reg_subkeys(r"SOFTWARE\Microsoft").is_empty());
        assert!(p.service("EventLog").is_some_and(|s| s.running));
        assert!(p.service("NoSuchService-PulsarSplitTest").is_none());
        assert!(!p.cert_in_store("Root", "00"));
        assert_eq!(hex_to_bytes("0aFF"), Some(vec![0x0a, 0xff]));
    }
}
