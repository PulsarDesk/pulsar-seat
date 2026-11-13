// SPDX-License-Identifier: GPL-3.0-only
//! Pinning the loopback listener's TLS certificate.
//!
//! The RDP-Tcp listener presents a self-signed certificate, so the usual chain check
//! cannot authenticate it. Instead the seat host accepts the TLS connection only when the
//! SHA-1 thumbprint of the certificate the server sent equals one of the thumbprints of the
//! certificates TermService itself may use (fail closed: no pins = no connection):
//!
//! 1. `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp`
//!    `SSLCertificateSHA1Hash` (REG_BINARY, 20 bytes) when an administrator configured a
//!    certificate for the listener (documented: Microsoft Learn, "Configure Remote Desktop
//!    certificates"); then only that certificate is accepted. A value that exists but
//!    cannot be read or is not 20 bytes pins nothing (fail closed, never the store);
//! 2. otherwise every certificate in the local machine's `Remote Desktop` certificate
//!    store, where TermService keeps its self-signed certificate. Only administrators and
//!    SYSTEM can write that store.
//!
//! The service reads the pins ([`listener_pins`]) and hands them to the seat host in the
//! host assignment (`HostAssignment::listener_cert_sha1`); the host can also read them
//! itself (it runs as SYSTEM in the host session) when an older service sent none.

use sha1::{Digest, Sha1};

/// One SHA-1 thumbprint.
pub type Thumbprint = [u8; 20];

/// The listener's registry key (same constant as `seat_session::rds::TS_RDP_TCP`).
pub const RDP_TCP_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp";
pub const CERT_HASH_VALUE: &str = "SSLCertificateSHA1Hash";
/// Local machine certificate store TermService uses for its self-signed certificate.
pub const RDP_CERT_STORE: &str = "Remote Desktop";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    #[error("no pinned listener certificate is known (fail closed)")]
    NoPins,
    #[error("server certificate {got} matches none of the pinned thumbprints {expected}")]
    Mismatch { got: String, expected: String },
}

/// Uppercase hex, no separators (the form `certutil` and PowerShell show).
pub fn hex(t: &Thumbprint) -> String {
    t.iter().map(|b| format!("{b:02X}")).collect()
}

/// Parse a thumbprint written as hex (any case; spaces and colons ignored).
pub fn parse_hex(s: &str) -> Option<Thumbprint> {
    let digits: Vec<u8> = s.bytes().filter(|b| !matches!(b, b' ' | b':' | b'\t')).collect();
    if digits.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, pair) in digits.chunks(2).enumerate() {
        let txt = std::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(txt, 16).ok()?;
    }
    Some(out)
}

/// SHA-1 of a DER certificate.
pub fn thumbprint(der: &[u8]) -> Thumbprint {
    Sha1::digest(der).into()
}

/// The set of acceptable listener certificates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CertPin {
    pins: Vec<Thumbprint>,
}

impl CertPin {
    pub fn new(pins: Vec<Thumbprint>) -> Self {
        let mut pins = pins;
        pins.sort_unstable();
        pins.dedup();
        Self { pins }
    }
    /// From hex strings; malformed entries are dropped (never widen the pin).
    pub fn from_hex<S: AsRef<str>>(items: &[S]) -> Self {
        Self::new(items.iter().filter_map(|s| parse_hex(s.as_ref())).collect())
    }
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }
    pub fn pins(&self) -> &[Thumbprint] {
        &self.pins
    }
    pub fn to_hex(&self) -> Vec<String> {
        self.pins.iter().map(hex).collect()
    }
    /// Fail closed: an empty pin set accepts nothing.
    pub fn check(&self, der: &[u8]) -> Result<(), PinError> {
        if self.pins.is_empty() {
            return Err(PinError::NoPins);
        }
        let got = thumbprint(der);
        if self.pins.contains(&got) {
            Ok(())
        } else {
            Err(PinError::Mismatch { got: hex(&got), expected: self.to_hex().join(",") })
        }
    }
}

/// What reading `SSLCertificateSHA1Hash` gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Configured {
    /// The value does not exist: TermService uses its self-signed certificate.
    NotSet,
    /// The value's bytes.
    Value(Vec<u8>),
    /// The value exists but could not be read (too long, wrong type, access denied).
    Unreadable(u32),
}

/// Pure choice between the configured listener certificate and the store contents. Only
/// an absent value (or an all-zero one, which TermService treats as unset) falls back to
/// the store; anything unreadable or malformed pins nothing (fail closed).
pub fn choose_pins(configured: Configured, store: Vec<Thumbprint>) -> CertPin {
    match configured {
        Configured::NotSet => CertPin::new(store),
        Configured::Value(v) => match <Thumbprint>::try_from(v.as_slice()) {
            Ok(t) if t == [0u8; 20] => CertPin::new(store),
            Ok(t) => CertPin::new(vec![t]),
            Err(_) => {
                eprintln!("seat-rdp: {CERT_HASH_VALUE} is not a 20-byte thumbprint ({} bytes); pinning nothing", v.len());
                CertPin::default()
            }
        },
        Configured::Unreadable(rc) => {
            eprintln!("seat-rdp: {CERT_HASH_VALUE} cannot be read (error {rc}); pinning nothing");
            CertPin::default()
        }
    }
}

/// Thumbprints TermService may present on RDP-Tcp (see the module docs). Read-only.
#[cfg(windows)]
pub fn listener_pins() -> CertPin {
    choose_pins(win::configured_hash(), win::store_thumbprints())
}

/// No listener outside Windows.
#[cfg(not(windows))]
pub fn listener_pins() -> CertPin {
    CertPin::default()
}

#[cfg(windows)]
mod win {
    use super::{CERT_HASH_VALUE, Configured, RDP_CERT_STORE, RDP_TCP_KEY, Thumbprint, thumbprint};
    use windows_sys::Win32::Security::Cryptography::{
        CERT_CONTEXT, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG,
        CERT_SYSTEM_STORE_LOCAL_MACHINE, CertCloseStore, CertEnumCertificatesInStore, CertOpenStore,
    };
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `ERROR_FILE_NOT_FOUND`: the key or the value does not exist.
    const ERROR_FILE_NOT_FOUND: u32 = 2;

    /// `SSLCertificateSHA1Hash` of RDP-Tcp.
    pub fn configured_hash() -> Configured {
        let (key, value) = (wide(RDP_TCP_KEY), wide(CERT_HASH_VALUE));
        let mut buf = [0u8; 64];
        let mut len = buf.len() as u32;
        // SAFETY: valid strings and an output buffer of `len` bytes.
        let rc = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
        };
        match rc {
            0 => Configured::Value(buf[..(len as usize).min(buf.len())].to_vec()),
            ERROR_FILE_NOT_FOUND => Configured::NotSet,
            rc => Configured::Unreadable(rc),
        }
    }

    /// SHA-1 of every certificate in the local machine `Remote Desktop` store.
    pub fn store_thumbprints() -> Vec<Thumbprint> {
        let name = wide(RDP_CERT_STORE);
        // SAFETY: documented system-store provider with a wide store name; closed below.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG,
                name.as_ptr().cast(),
            )
        };
        if store.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut ctx: *const CERT_CONTEXT = std::ptr::null();
        loop {
            // SAFETY: the previous context is passed back (and freed by the call).
            ctx = unsafe { CertEnumCertificatesInStore(store, ctx) };
            if ctx.is_null() {
                break;
            }
            // SAFETY: a valid context from the enumeration.
            let der = unsafe { std::slice::from_raw_parts((*ctx).pbCertEncoded, (*ctx).cbCertEncoded as usize) };
            out.push(thumbprint(der));
        }
        // SAFETY: the store handle from CertOpenStore.
        unsafe { CertCloseStore(store, 0) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let t = thumbprint(b"cert");
        assert_eq!(parse_hex(&hex(&t)), Some(t));
        let spaced = hex(&t).to_lowercase().as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(":");
        assert_eq!(parse_hex(&spaced), Some(t));
        assert_eq!(parse_hex("abc"), None);
        assert_eq!(parse_hex(&"zz".repeat(20)), None);
    }

    #[test]
    fn fail_closed_and_mismatch() {
        let der = b"the listener certificate";
        assert_eq!(CertPin::default().check(der), Err(PinError::NoPins));
        let good = CertPin::new(vec![thumbprint(der)]);
        assert!(good.check(der).is_ok());
        assert!(matches!(good.check(b"another"), Err(PinError::Mismatch { .. })));
        // Malformed hex never widens the set.
        let p = CertPin::from_hex(&["nope", &hex(&thumbprint(der))]);
        assert_eq!(p.pins().len(), 1);
        assert!(CertPin::from_hex(&["nope"]).check(der).is_err());
    }

    #[test]
    fn configured_certificate_wins() {
        let a = thumbprint(b"a");
        let b = thumbprint(b"b");
        assert_eq!(choose_pins(Configured::Value(a.to_vec()), vec![b]).pins(), &[a]);
        assert_eq!(choose_pins(Configured::NotSet, vec![b, b]).pins(), &[b]);
        // A zero value is "unset": the store.
        assert_eq!(choose_pins(Configured::Value(vec![0; 20]), vec![b]).pins(), &[b]);
        // Malformed or unreadable never widens to the store (fail closed).
        assert!(choose_pins(Configured::Value(vec![1, 2, 3]), vec![b]).is_empty());
        assert!(choose_pins(Configured::Unreadable(234), vec![b]).is_empty());
        assert!(choose_pins(Configured::Unreadable(5), vec![b]).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn listener_lookup_is_read_only() {
        // No assertion on content: the dev box may have no RDP listener certificate.
        let _ = listener_pins();
    }
}
