// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in import of the package's publisher certificate at install (spec/remote 90 §2.5,
//! parity round 2026-11-17, package remote-smalls).
//!
//! The reference product imports its own certificate into `LocalMachine\Root` and
//! `LocalMachine\TrustedPublisher` silently, so that its self-signed driver package
//! installs. Pulsar Seat offers the same result only as a **visible, unchecked** installer
//! choice (MSI `PSIMPORTCERT=1`, shown only when the package carries a certificate), and only
//! for the one certificate file the package itself ships (`certs\publisher.cer` next to the
//! service binary; built in by `build-installer.ps1 -PublisherCert`, absent from every
//! package built without it).
//!
//! Rules (checked before anything is written):
//! - DER X.509, at most [`MAX_CERT_BYTES`];
//! - a leaf code-signing certificate: no CA basic constraint, the code-signing extended key
//!   usage present ([`publisher_cert_allowed`]);
//! - `TrustedPublisher` always; `Root` only when the certificate is self-signed (a
//!   certificate issued by a CA the PC already trusts needs no root entry);
//! - a store that already holds the certificate is left alone and **not** journaled (it
//!   was not ours to add, so it is not ours to remove);
//! - every add is journaled ([`crate::journal::Change::Certificate`]) before it is made;
//!   uninstall removes exactly those entries again.
//!
//! Signed driver packages: a catalog signed by this certificate is accepted by
//! [`crate::drivers`] only while the certificate is in `TrustedPublisher`
//! ([`trusted_publishers`]). HYPOTHESIS: never run on hardware; kernel-mode packages still
//! need a signature the PC's code-integrity policy accepts (with Secure Boot on, a
//! self-signed kernel driver does not load whatever the stores hold; installer/README.md).

use crate::journal::{Change, Journal, JournalError};
use crate::ops::{OpsError, SystemOps};
use serde::{Deserialize, Serialize};

/// Largest certificate file accepted.
pub const MAX_CERT_BYTES: usize = 16 * 1024;
/// Shipped certificate, relative to the folder of the service binary.
pub const SHIPPED_CERT: &str = r"certs\publisher.cer";
/// Journal owner of the import.
pub const OWNER: &str = "install:certificate";
/// OID of the code-signing extended key usage.
pub const OID_CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";

/// Machine certificate stores Pulsar may write (nothing else is ever touched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertStore {
    Root,
    TrustedPublisher,
}

impl CertStore {
    /// System store name (`CERT_SYSTEM_STORE_LOCAL_MACHINE`).
    pub fn system_name(self) -> &'static str {
        match self {
            Self::Root => "Root",
            Self::TrustedPublisher => "TrustedPublisher",
        }
    }
}

/// What Windows reports about a certificate (`crate::win::inspect_cert`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertFacts {
    pub subject: String,
    /// Basic constraints say CA.
    pub is_ca: bool,
    /// Extended key usages (OIDs); empty = no EKU extension.
    pub ekus: Vec<String>,
    /// Issuer equals subject and the signature verifies with its own key.
    pub self_signed: bool,
}

/// SHA-1 thumbprint (upper-case hex, how Windows shows it).
pub fn thumbprint(der: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

/// 40 hex digits.
pub fn is_thumbprint(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Cheap shape check of a DER certificate (outer SEQUENCE whose length covers the file).
pub fn der_shape_ok(der: &[u8]) -> bool {
    if der.len() < 64 || der.len() > MAX_CERT_BYTES || der[0] != 0x30 {
        return false;
    }
    let (len, hdr) = match der[1] {
        n if n < 0x80 => (n as usize, 2),
        0x81 => (der[2] as usize, 3),
        0x82 => (((der[2] as usize) << 8) | der[3] as usize, 4),
        _ => return false,
    };
    hdr + len == der.len()
}

/// Only a leaf code-signing certificate may be imported.
pub fn publisher_cert_allowed(f: &CertFacts) -> Result<(), String> {
    if f.is_ca {
        return Err("certificate is a CA (basic constraints); only a leaf publisher certificate is imported".into());
    }
    if !f.ekus.iter().any(|e| e == OID_CODE_SIGNING) {
        return Err("certificate has no code-signing extended key usage".into());
    }
    Ok(())
}

/// Stores the certificate goes into.
pub fn target_stores(f: &CertFacts) -> Vec<CertStore> {
    if f.self_signed { vec![CertStore::TrustedPublisher, CertStore::Root] } else { vec![CertStore::TrustedPublisher] }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub thumbprint: String,
    pub subject: String,
    /// Stores we added the certificate to (journaled).
    pub added: Vec<CertStore>,
    /// Stores that already held it (left alone).
    pub present: Vec<CertStore>,
}

#[derive(Debug)]
pub enum CertError {
    Refused(String),
    Journal(JournalError),
    Ops(OpsError),
}

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) => write!(f, "refused: {m}"),
            Self::Journal(e) => write!(f, "{e}"),
            Self::Ops(e) => write!(f, "{e}"),
        }
    }
}

impl From<JournalError> for CertError {
    fn from(e: JournalError) -> Self {
        Self::Journal(e)
    }
}

impl From<OpsError> for CertError {
    fn from(e: OpsError) -> Self {
        Self::Ops(e)
    }
}

/// Import `der` (facts from Windows) into its target stores, journaled.
pub fn import(j: &Journal, ops: &dyn SystemOps, der: &[u8], facts: &CertFacts) -> Result<ImportReport, CertError> {
    if !der_shape_ok(der) {
        return Err(CertError::Refused("not a DER certificate of an acceptable size".into()));
    }
    publisher_cert_allowed(facts).map_err(CertError::Refused)?;
    let tp = thumbprint(der);
    let mut rep = ImportReport { thumbprint: tp.clone(), subject: facts.subject.clone(), ..Default::default() };
    for store in target_stores(facts) {
        if ops.cert_thumbprints(store)?.iter().any(|t| t.eq_ignore_ascii_case(&tp)) {
            rep.present.push(store);
            continue;
        }
        j.record(OWNER, Change::Certificate { store, thumbprint: tp.clone() })?;
        ops.cert_add(store, der)?;
        rep.added.push(store);
    }
    Ok(rep)
}

/// Thumbprints of `candidates` that are in `TrustedPublisher` now: the publishers whose
/// signed driver catalogs [`crate::drivers`] accepts besides Microsoft's.
pub fn trusted_publishers(ops: &dyn SystemOps, candidates: &[String]) -> Vec<String> {
    let Ok(have) = ops.cert_thumbprints(CertStore::TrustedPublisher) else { return Vec::new() };
    candidates.iter().filter(|c| have.iter().any(|h| h.eq_ignore_ascii_case(c))).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    /// Minimal DER-shaped blob (outer SEQUENCE, one-byte long length form).
    fn fake_der(fill: u8) -> Vec<u8> {
        let body = vec![fill; 200];
        let mut v = vec![0x30, 0x81, body.len() as u8];
        v.extend(body);
        v
    }

    fn leaf(self_signed: bool) -> CertFacts {
        CertFacts { subject: "Pulsar Seat Publisher".into(), is_ca: false, ekus: vec![OID_CODE_SIGNING.into()], self_signed }
    }

    #[test]
    fn shape_and_policy() {
        assert!(der_shape_ok(&fake_der(1)));
        assert!(!der_shape_ok(&[0x30, 0x03, 1, 2, 3]));
        let mut bad = fake_der(1);
        bad.push(0);
        assert!(!der_shape_ok(&bad), "trailing bytes");
        assert!(publisher_cert_allowed(&leaf(true)).is_ok());
        assert!(publisher_cert_allowed(&CertFacts { is_ca: true, ..leaf(true) }).is_err());
        assert!(publisher_cert_allowed(&CertFacts { ekus: vec![], ..leaf(true) }).is_err());
        assert_eq!(target_stores(&leaf(true)), vec![CertStore::TrustedPublisher, CertStore::Root]);
        assert_eq!(target_stores(&leaf(false)), vec![CertStore::TrustedPublisher]);
        assert!(is_thumbprint(&thumbprint(b"x")));
        assert!(!is_thumbprint("xyz"));
    }

    #[test]
    fn import_journals_adds_and_uninstall_removes_only_ours() {
        let j = Journal::memory();
        let ops = MemoryOps::new();
        let der = fake_der(7);
        let tp = thumbprint(&der);
        // Root already holds it (the operator's entry): left alone, not journaled.
        ops.cert_add(CertStore::Root, &der).unwrap();
        let rep = import(&j, &ops, &der, &leaf(true)).unwrap();
        assert_eq!(rep.added, vec![CertStore::TrustedPublisher]);
        assert_eq!(rep.present, vec![CertStore::Root]);
        assert_eq!(trusted_publishers(&ops, std::slice::from_ref(&tp)), vec![tp.clone()]);
        // Second run: nothing new.
        let again = import(&j, &ops, &der, &leaf(true)).unwrap();
        assert!(again.added.is_empty());
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.failed.is_empty(), "{r:?}");
        assert!(ops.cert_thumbprints(CertStore::TrustedPublisher).unwrap().is_empty());
        assert_eq!(ops.cert_thumbprints(CertStore::Root).unwrap(), vec![tp]);
    }

    #[test]
    fn import_refuses_a_ca_or_garbage() {
        let j = Journal::memory();
        let ops = MemoryOps::new();
        assert!(matches!(import(&j, &ops, b"nope", &leaf(true)), Err(CertError::Refused(_))));
        let ca = CertFacts { is_ca: true, ..leaf(true) };
        assert!(matches!(import(&j, &ops, &fake_der(1), &ca), Err(CertError::Refused(_))));
        assert!(ops.cert_thumbprints(CertStore::Root).unwrap().is_empty());
    }
}
