// SPDX-License-Identifier: GPL-3.0-only
//! DPAPI machine-scope protection for seat passwords. Machine scope means any process on
//! this machine can decrypt the blob, so the blob itself must sit behind an ACL
//! (see `registry_store::SECRETS_SDDL`).

use std::io;
use std::ptr;
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: the system allocated `cbData` bytes at `pbData` with LocalAlloc.
    unsafe {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as HLOCAL);
        v
    }
}

pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(plain);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };
    // SAFETY: input borrows `plain` for the duration of the call; out is valid.
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take(out))
}

pub fn unprotect(cipher: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(cipher);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };
    // SAFETY: as above.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take(out))
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let c = super::protect(b"s3cret").unwrap();
        assert_ne!(c, b"s3cret");
        assert_eq!(super::unprotect(&c).unwrap(), b"s3cret");
        assert!(super::unprotect(b"not a blob").is_err());
    }
}
