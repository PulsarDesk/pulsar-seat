// SPDX-License-Identifier: GPL-3.0-only
//! [`SecretStore`] on LSA private data (architecture §6, spec 00 §5.5.1): one local secret
//! per seat, `L$PulsarSeat.Seat.NN`, and one per remote seat slot,
//! `L$PulsarSeat.Remote.<seat-id>.<slot>` (Pulsar identity, connect password, relay key). Readable only by SYSTEM (and administrators with LSA
//! policy rights, by Windows design). The clear text never leaves this module except
//! as a [`Secret`] that is wiped on drop. Untested on hardware: needs SYSTEM / elevation.

use seat_core::consts::LSA_SEAT_SECRET_PREFIX;
use seat_core::store::{RemoteSecretSlot, SecretStore, StoreError, check_secret_len, remote_secret_name};
use seat_core::{SeatNumber, Secret};
use std::ptr;
use windows_sys::Win32::Foundation::{NTSTATUS, STATUS_OBJECT_NAME_NOT_FOUND};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaClose, LsaFreeMemory, LsaNtStatusToWinError, LsaOpenPolicy,
    LsaRetrievePrivateData, LsaStorePrivateData, POLICY_CREATE_SECRET, POLICY_GET_PRIVATE_INFORMATION,
};

/// Name of a seat's secret.
pub fn secret_name(seat: SeatNumber) -> String {
    format!("{LSA_SEAT_SECRET_PREFIX}{}", seat.two_digits())
}

#[derive(Debug, Default)]
pub struct LsaSecretStore;

struct Policy(LSA_HANDLE);

impl Drop for Policy {
    fn drop(&mut self) {
        // SAFETY: handle from LsaOpenPolicy.
        unsafe { LsaClose(self.0) };
    }
}

fn err(op: &str, st: NTSTATUS) -> StoreError {
    // SAFETY: pure conversion.
    let w = unsafe { LsaNtStatusToWinError(st) };
    StoreError::Secret(format!("{op} failed (win32 {w})"))
}

fn open(access: i32) -> Result<Policy, StoreError> {
    // SAFETY: zeroed object attributes are what LsaOpenPolicy expects.
    let attrs: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    let mut h: LSA_HANDLE = 0 as LSA_HANDLE;
    // SAFETY: local policy, valid out pointer.
    let st = unsafe { LsaOpenPolicy(ptr::null(), &attrs, access as u32, &mut h) };
    if st < 0 {
        return Err(err("LsaOpenPolicy", st));
    }
    Ok(Policy(h))
}

/// UNICODE_STRING borrowing `buf` (no terminating NUL counted). Fails instead of wrapping
/// when the byte length does not fit the 16-bit `Length`.
fn ustr(buf: &mut [u16]) -> Result<LSA_UNICODE_STRING, StoreError> {
    let bytes = buf
        .len()
        .checked_mul(2)
        .and_then(|b| u16::try_from(b).ok())
        .ok_or_else(|| StoreError::Secret("value too long for an LSA string".into()))?;
    Ok(LSA_UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: buf.as_mut_ptr() })
}

/// Retrieve a seat's secret and hand the clear text (wiped right after) to `f`.
fn with_secret<T>(seat: SeatNumber, f: impl FnOnce(Option<&[u16]>) -> T) -> Result<T, StoreError> {
    with_named(&secret_name(seat), f)
}

/// Retrieve the local secret `name` and hand the clear text (wiped right after) to `f`.
fn with_named<T>(secret: &str, f: impl FnOnce(Option<&[u16]>) -> T) -> Result<T, StoreError> {
    let p = open(POLICY_GET_PRIVATE_INFORMATION)?;
    let mut name: Vec<u16> = secret.encode_utf16().collect();
    let key = ustr(&mut name)?;
    let mut out: *mut LSA_UNICODE_STRING = ptr::null_mut();
    // SAFETY: valid handle and key; out receives LSA-allocated memory.
    let st = unsafe { LsaRetrievePrivateData(p.0, &key, &mut out) };
    if st == STATUS_OBJECT_NAME_NOT_FOUND {
        return Ok(f(None));
    }
    if st < 0 {
        return Err(err("LsaRetrievePrivateData", st));
    }
    if out.is_null() {
        return Ok(f(None));
    }
    // SAFETY: out points to a UNICODE_STRING allocated by LSA; wiped and freed here.
    let r = unsafe {
        let u = &*out;
        let n = (u.Length / 2) as usize;
        let r = if u.Buffer.is_null() || n == 0 {
            f(None)
        } else {
            let slice = std::slice::from_raw_parts_mut(u.Buffer, n);
            let r = f(Some(slice));
            slice.iter_mut().for_each(|c| std::ptr::write_volatile(c, 0));
            r
        };
        LsaFreeMemory(out as *const _);
        r
    };
    Ok(r)
}

/// Read the local secret `name` (the host account's password,
/// `seat_session::host_account::PASSWORD_SECRET`).
pub fn get_named(name: &str) -> Result<Option<Secret>, StoreError> {
    with_named(name, |v| v.map(|s| Secret::new(String::from_utf16_lossy(s))))
}

/// Store (`Some`) or delete (`None`; a missing secret is fine) the local secret `name`.
pub fn set_named(secret: &str, value: Option<&Secret>) -> Result<(), StoreError> {
    let p = open(POLICY_CREATE_SECRET)?;
    let mut name: Vec<u16> = secret.encode_utf16().collect();
    let key = ustr(&mut name)?;
    let st = match value {
        Some(pw) => {
            let mut data: zeroize::Zeroizing<Vec<u16>> = zeroize::Zeroizing::new(pw.expose().encode_utf16().collect());
            let val = ustr(&mut data)?;
            // SAFETY: valid policy handle and strings that outlive the call.
            unsafe { LsaStorePrivateData(p.0, &key, &val) }
        }
        // SAFETY: as above; a null value deletes the secret.
        None => unsafe { LsaStorePrivateData(p.0, &key, ptr::null()) },
    };
    if st < 0 && !(value.is_none() && st == STATUS_OBJECT_NAME_NOT_FOUND) {
        return Err(err("LsaStorePrivateData", st));
    }
    Ok(())
}

impl SecretStore for LsaSecretStore {
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError> {
        if let Some(p) = password {
            check_secret_len(p)?;
        }
        set_named(&secret_name(seat), password)
    }

    /// Presence only: the LSA buffer is wiped and freed without building a [`Secret`]
    /// (the service keeps the answer cached and asks once per seat).
    fn has(&self, seat: SeatNumber) -> bool {
        matches!(with_secret(seat, |v| v.is_some()), Ok(true))
    }

    fn get(&self, seat: SeatNumber) -> Result<Option<Secret>, StoreError> {
        with_secret(seat, |v| v.map(|s| Secret::new(String::from_utf16_lossy(s))))
    }

    /// One local secret per slot (`seat_core::store::remote_secret_name`); values are short
    /// (checked against `MAX_SECRET_CHARS`; LSA limits in the `seat_core::store` module doc).
    fn set_remote(&self, seat_id: &str, slot: RemoteSecretSlot, value: Option<&Secret>) -> Result<(), StoreError> {
        if let Some(v) = value {
            check_secret_len(v)?;
        }
        set_named(&remote_secret_name(seat_id, slot)?, value)
    }

    fn get_remote(&self, seat_id: &str, slot: RemoteSecretSlot) -> Result<Option<Secret>, StoreError> {
        get_named(&remote_secret_name(seat_id, slot)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_are_refused_not_wrapped() {
        let mut ok = vec![b'a' as u16; 256];
        assert_eq!(ustr(&mut ok).unwrap().Length, 512);
        let mut long = vec![b'a' as u16; 40_000];
        assert!(ustr(&mut long).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(secret_name(SeatNumber::new(2).unwrap()), "L$PulsarSeat.Seat.02");
        assert_eq!(secret_name(SeatNumber::new(16).unwrap()), "L$PulsarSeat.Seat.16");
    }
}
