// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of [`crate::certs`] and [`crate::features`] (parity round 2026-11-17,
//! package remote-smalls): local-machine certificate stores through CryptoAPI, certificate
//! facts, and optional features through `dism.exe`. Never run on hardware (HYPOTHESIS).

use crate::certs::{CertFacts, CertStore};
use crate::features::FeatureState;
use crate::ops::{OpsError, OpsResult};
use std::path::Path;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Security::Cryptography::*;

const ENCODING: CERT_QUERY_ENCODING_TYPE = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;

struct Store(HCERTSTORE);

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: opened by CertOpenStore.
        unsafe { CertCloseStore(self.0, 0) };
    }
}

struct Ctx(*const CERT_CONTEXT);

impl Drop for Ctx {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a context we own (created or found).
            unsafe { CertFreeCertificateContext(self.0) };
        }
    }
}

fn last(op: &str) -> OpsError {
    // SAFETY: reads the thread's last error.
    OpsError::win32(op, unsafe { GetLastError() })
}

fn open_store(store: CertStore, write: bool) -> OpsResult<Store> {
    let name = crate::win::wide(store.system_name());
    let mut flags = CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_OPEN_EXISTING_FLAG;
    if !write {
        flags |= CERT_STORE_READONLY_FLAG;
    }
    // SAFETY: system store provider with a NUL-terminated wide name.
    let h = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if h.is_null() { Err(last("CertOpenStore")) } else { Ok(Store(h)) }
}

/// Thumbprints of every certificate in a local-machine store.
pub fn cert_thumbprints(store: CertStore) -> OpsResult<Vec<String>> {
    let s = open_store(store, false)?;
    let mut out = Vec::new();
    let mut prev: *const CERT_CONTEXT = std::ptr::null();
    loop {
        // SAFETY: enumerates; the previous context is freed by the call.
        let c = unsafe { CertEnumCertificatesInStore(s.0, prev) };
        if c.is_null() {
            break;
        }
        // SAFETY: valid context; encoded bytes owned by it.
        let der = unsafe { std::slice::from_raw_parts((*c).pbCertEncoded, (*c).cbCertEncoded as usize) };
        out.push(crate::certs::thumbprint(der));
        prev = c;
    }
    Ok(out)
}

/// Add a DER certificate (no replacement of an existing one).
pub fn cert_add(store: CertStore, der: &[u8]) -> OpsResult<()> {
    let s = open_store(store, true)?;
    // SAFETY: valid buffer; no context returned.
    let ok = unsafe {
        CertAddEncodedCertificateToStore(s.0, ENCODING, der.as_ptr(), der.len() as u32, CERT_STORE_ADD_USE_EXISTING, std::ptr::null_mut())
    };
    if ok == 0 { Err(last("CertAddEncodedCertificateToStore")) } else { Ok(()) }
}

/// Delete the certificate with this SHA-1 thumbprint.
pub fn cert_remove(store: CertStore, thumbprint: &str) -> OpsResult<()> {
    if !crate::certs::is_thumbprint(thumbprint) {
        return Err(OpsError::new("cert_remove", "bad thumbprint"));
    }
    let mut hash: Vec<u8> =
        (0..20).map(|i| u8::from_str_radix(&thumbprint[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
    let blob = CRYPT_INTEGER_BLOB { cbData: hash.len() as u32, pbData: hash.as_mut_ptr() };
    let s = open_store(store, true)?;
    // SAFETY: SHA-1 find over a valid blob.
    let c = unsafe {
        CertFindCertificateInStore(s.0, ENCODING, 0, CERT_FIND_SHA1_HASH, (&blob as *const CRYPT_INTEGER_BLOB).cast(), std::ptr::null())
    };
    if c.is_null() {
        return Ok(());
    }
    // SAFETY: the call frees the context in every case.
    if unsafe { CertDeleteCertificateFromStore(c) } == 0 {
        return Err(last("CertDeleteCertificateFromStore"));
    }
    Ok(())
}

/// Subject, CA flag, extended key usages and self-signature of a DER certificate.
pub fn inspect_cert(der: &[u8]) -> OpsResult<CertFacts> {
    // SAFETY: valid buffer; the context is freed by Ctx.
    let c = Ctx(unsafe { CertCreateCertificateContext(ENCODING, der.as_ptr(), der.len() as u32) });
    if c.0.is_null() {
        return Err(last("CertCreateCertificateContext"));
    }
    // SAFETY: c.0 is a valid context for every call below.
    unsafe {
        let info = &*(*c.0).pCertInfo;
        let mut buf = [0u16; 512];
        let n = CertGetNameStringW(c.0, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32);
        let subject = String::from_utf16_lossy(&buf[..n.saturating_sub(1) as usize]);

        let mut is_ca = false;
        let ext = CertFindExtension(szOID_BASIC_CONSTRAINTS2, info.cExtension, info.rgExtension);
        if !ext.is_null() {
            let mut bc = CERT_BASIC_CONSTRAINTS2_INFO { fCA: 0, fPathLenConstraint: 0, dwPathLenConstraint: 0 };
            let mut cb = std::mem::size_of::<CERT_BASIC_CONSTRAINTS2_INFO>() as u32;
            if CryptDecodeObject(
                ENCODING,
                X509_BASIC_CONSTRAINTS2,
                (*ext).Value.pbData,
                (*ext).Value.cbData,
                0,
                (&mut bc as *mut CERT_BASIC_CONSTRAINTS2_INFO).cast(),
                &mut cb,
            ) == 0
            {
                return Err(last("CryptDecodeObject"));
            }
            is_ca = bc.fCA != 0;
        }

        let mut ekus = Vec::new();
        let mut cb = 0u32;
        // Flag 2 = CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG: the extension, not properties.
        if CertGetEnhancedKeyUsage(c.0, 2, std::ptr::null_mut(), &mut cb) != 0 && cb > 0 {
            let mut mem = vec![0u64; (cb as usize).div_ceil(8)];
            let usage = mem.as_mut_ptr() as *mut CTL_USAGE;
            if CertGetEnhancedKeyUsage(c.0, 2, usage, &mut cb) != 0 {
                for i in 0..(*usage).cUsageIdentifier as usize {
                    let p = *(*usage).rgpszUsageIdentifier.add(i);
                    if !p.is_null() {
                        ekus.push(std::ffi::CStr::from_ptr(p as *const std::ffi::c_char).to_string_lossy().into_owned());
                    }
                }
            }
        }

        let same_name = CertCompareCertificateName(ENCODING, &info.Issuer, &info.Subject) != 0;
        let self_signed = same_name
            && CryptVerifyCertificateSignature(0, ENCODING, (*c.0).pbCertEncoded, (*c.0).cbCertEncoded, &info.SubjectPublicKeyInfo) != 0;
        Ok(CertFacts { subject, is_ca, ekus, self_signed })
    }
}

/// SHA-1 thumbprint of a certificate context (catalog signers).
///
/// # Safety
/// `c` must be a valid certificate context.
pub unsafe fn context_thumbprint(c: *const CERT_CONTEXT) -> String {
    // SAFETY: per the contract.
    let der = unsafe { std::slice::from_raw_parts((*c).pbCertEncoded, (*c).cbCertEncoded as usize) };
    crate::certs::thumbprint(der)
}

fn dism(args: &[&str]) -> OpsResult<(u32, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = Path::new(&sys).join("System32").join("dism.exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let child = seat_core::spawn::spawn_locked(&mut cmd).map_err(|e| OpsError::new("dism", e.to_string()))?;
    let out = child.wait_with_output().map_err(|e| OpsError::new("dism", e.to_string()))?;
    Ok((out.status.code().map(|c| c as u32).unwrap_or(1), String::from_utf8_lossy(&out.stdout).into_owned()))
}

fn check_name(name: &str) -> OpsResult<()> {
    if crate::features::OptionalFeature::from_dism_name(name).is_none() {
        return Err(OpsError::new("dism", "feature not on the allow list"));
    }
    Ok(())
}

/// `dism /Online /English /Get-FeatureInfo`.
pub fn feature_state(name: &str) -> OpsResult<FeatureState> {
    check_name(name)?;
    let fname = format!("/FeatureName:{name}");
    let (code, text) = dism(&["/Online", "/English", "/Get-FeatureInfo", &fname])?;
    crate::features::dism_info_outcome(code, &text).map_err(|m| OpsError { op: "dism".into(), message: m, win32: Some(code) })
}

/// `dism /Online /Enable-Feature /All /NoRestart /LimitAccess`; `Ok(true)` = restart needed.
pub fn feature_enable(name: &str) -> OpsResult<bool> {
    check_name(name)?;
    let fname = format!("/FeatureName:{name}");
    let (code, _) = dism(&["/Online", "/English", "/Enable-Feature", &fname, "/All", "/NoRestart", "/LimitAccess", "/Quiet"])?;
    match code {
        0 => Ok(false),
        3010 => Ok(true),
        c => Err(OpsError { op: "dism".into(), message: format!("Enable-Feature {name} exit {c}"), win32: Some(c) }),
    }
}
