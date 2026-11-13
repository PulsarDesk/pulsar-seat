// SPDX-License-Identifier: GPL-3.0-only
//! Release signing of Pulsar Seat (installer/README.md "Signing and release"): the
//! release's `SHA256SUMS.txt` is signed with an Ed25519 key kept **offline** by the
//! maintainer; the service accepts a checksum file only with a valid signature from a key
//! listed in `installer/release-signers.txt` (compiled in). The Authenticode signature of
//! the MSI stays required on top of this; this signature makes the checksum file (and so
//! the installer hash) independent of whoever controls the GitHub release page.
//!
//! Formats are those of minisign (public spec, <https://jedisct1.github.io/minisign/>), so
//! the maintainer may use the minisign tool instead of `pulsar-seat-release`:
//!
//! - public key: `base64("Ed" || key_id[8] || ed25519_pk[32])`, optionally preceded by an
//!   `untrusted comment: …` line;
//! - signature file (`<file>.minisig`), four lines: `untrusted comment: …`,
//!   `base64("ED" || key_id[8] || sig[64])` with `sig = Ed25519(BLAKE2b-512(file))`,
//!   `trusted comment: <text>`, `base64(global_sig[64])` with
//!   `global_sig = Ed25519(sig || text)`. Only the pre-hashed `ED` form is accepted (the
//!   default of minisign 0.10+); the legacy `Ed` form is refused;
//! - secret key (feature `sign`): `base64("Ed" || kdf_alg[2] || "B2" || salt[32] ||
//!   opslimit[8] || memlimit[8] || key_id[8] || ed25519_sk[64] || checksum[32])` with
//!   `checksum = BLAKE2b-256("Ed" || key_id || ed25519_sk)`. We write and read only the
//!   unencrypted form (`kdf_alg` = 0, like `minisign -G -W`); a password-protected
//!   minisign key (`Sc`) is refused with a hint to sign with minisign itself.
//!
//! Written from the public format description; no minisign code is copied. The test
//! vectors of the minisign-verify crate (MIT) are used as interop checks.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signature, VerifyingKey};

#[cfg(feature = "sign")]
mod sign;
#[cfg(feature = "sign")]
pub use sign::{SecretKey, sign};

/// Signature algorithm id of keys (both forms) and of legacy signatures.
const ALG_ED: [u8; 2] = *b"Ed";
/// Signature algorithm id of pre-hashed signatures (BLAKE2b-512 of the file).
const ALG_ED_PREHASHED: [u8; 2] = *b"ED";
const UNTRUSTED: &str = "untrusted comment: ";
const TRUSTED: &str = "trusted comment: ";
/// Longest signature file we read (the four lines are far shorter).
pub const MAX_SIG_BYTES: usize = 4096;

/// Why a key or signature was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not the expected format (bad base64, length, algorithm, missing lines).
    Format(String),
    /// Signed by a key that is not in the accepted list.
    UnknownKey(String),
    /// The signature does not verify.
    BadSignature,
    /// I/O or argument problem (tool).
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Format(s) => write!(f, "format: {s}"),
            Error::UnknownKey(id) => write!(f, "signed by key {id}, which is not an accepted release key"),
            Error::BadSignature => write!(f, "signature does not verify"),
            Error::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

fn fmt_err(s: impl Into<String>) -> Error {
    Error::Format(s.into())
}

/// Key id as minisign prints it: the 8 bytes as a little-endian u64, upper hex.
pub fn key_id_hex(id: &[u8; 8]) -> String {
    format!("{:016X}", u64::from_le_bytes(*id))
}

/// A release public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub key_id: [u8; 8],
    key: VerifyingKey,
}

impl PublicKey {
    /// `RWQ…` base64 line.
    pub fn from_base64(s: &str) -> Result<Self, Error> {
        let b = B64.decode(s.trim()).map_err(|e| fmt_err(format!("public key base64: {e}")))?;
        if b.len() != 42 || b[..2] != ALG_ED {
            return Err(fmt_err(format!("public key: {} bytes / algorithm, want 42 bytes \"Ed\"", b.len())));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&b[2..10]);
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&b[10..42]);
        let key = VerifyingKey::from_bytes(&pk).map_err(|_| fmt_err("public key: not a valid Ed25519 point"))?;
        if key.is_weak() {
            return Err(fmt_err("public key: weak (small-order) Ed25519 point"));
        }
        Ok(Self { key_id, key })
    }

    // Used by the signing side only (`sign` feature); the service builds without it.
    #[cfg_attr(not(feature = "sign"), allow(dead_code))]
    pub(crate) fn from_parts(key_id: [u8; 8], key: VerifyingKey) -> Self {
        Self { key_id, key }
    }

    pub fn to_base64(&self) -> String {
        let mut b = Vec::with_capacity(42);
        b.extend_from_slice(&ALG_ED);
        b.extend_from_slice(&self.key_id);
        b.extend_from_slice(self.key.as_bytes());
        B64.encode(b)
    }

    pub fn key_id_hex(&self) -> String {
        key_id_hex(&self.key_id)
    }

    /// minisign `.pub` file text.
    pub fn to_file_text(&self) -> String {
        format!("{UNTRUSTED}minisign public key {}\n{}\n", self.key_id_hex(), self.to_base64())
    }
}

/// Accepted release keys from `installer/release-signers.txt`: one base64 public key per
/// line; empty lines, `#` comments and `untrusted comment:` lines are ignored. Duplicate
/// key ids are refused (the key id selects the key). Each line is trimmed of Unicode
/// whitespace and U+FEFF (a BOM from Notepad / PowerShell 5) at both ends, exactly as
/// [`has_signer_lines`] reads it.
pub fn parse_signers(text: &str) -> Result<Vec<PublicKey>, Error> {
    let mut out: Vec<PublicKey> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim_matches(|c: char| is_line_blank(c as u32));
        if l.is_empty() || l.starts_with('#') || l.starts_with(UNTRUSTED) {
            continue;
        }
        let k = PublicKey::from_base64(l).map_err(|e| fmt_err(format!("line {}: {e}", n + 1)))?;
        if out.iter().any(|o| o.key_id == k.key_id) {
            return Err(fmt_err(format!("line {}: key id {} listed twice", n + 1, k.key_id_hex())));
        }
        out.push(k);
    }
    Ok(out)
}

/// Characters trimmed from a signer line: Unicode `White_Space` (what `char::is_whitespace`
/// and `str::trim` use) plus U+FEFF. Const, for [`has_signer_lines`].
const fn is_line_blank(c: u32) -> bool {
    matches!(
        c,
        0x09..=0x0D
            | 0x20
            | 0x85
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}

/// Decode one UTF-8 scalar at `i` (valid UTF-8: the input is a `&str`): (code point, length).
const fn utf8_at(b: &[u8], i: usize) -> (u32, usize) {
    let c = b[i];
    if c < 0x80 {
        (c as u32, 1)
    } else if c < 0xE0 {
        ((((c & 0x1F) as u32) << 6) | (b[i + 1] & 0x3F) as u32, 2)
    } else if c < 0xF0 {
        ((((c & 0x0F) as u32) << 12) | (((b[i + 1] & 0x3F) as u32) << 6) | (b[i + 2] & 0x3F) as u32, 3)
    } else {
        (
            (((c & 0x07) as u32) << 18)
                | (((b[i + 1] & 0x3F) as u32) << 12)
                | (((b[i + 2] & 0x3F) as u32) << 6)
                | (b[i + 3] & 0x3F) as u32,
            4,
        )
    }
}

/// `true` when the signer list text has at least one key line (const: used by the
/// service's release-build assertion). Reads lines exactly like [`parse_signers`] (same
/// trimming, BOM included): for any text that parses, `has_signer_lines(t) ==
/// !parse_signers(t)?.is_empty()`.
pub const fn has_signer_lines(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let mut end = i;
        while end < b.len() && b[end] != b'\n' {
            end += 1;
        }
        // Skip leading blanks (whitespace, BOM) within the line.
        let mut j = i;
        while j < end {
            let (c, n) = utf8_at(b, j);
            if !is_line_blank(c) {
                break;
            }
            j += n;
        }
        if j < end && b[j] != b'#' && !starts_with_at(b, j, UNTRUSTED.as_bytes()) {
            return true;
        }
        i = end + 1;
    }
    false
}

const fn starts_with_at(b: &[u8], at: usize, p: &[u8]) -> bool {
    if at + p.len() > b.len() {
        return false;
    }
    let mut k = 0;
    while k < p.len() {
        if b[at + k] != p[k] {
            return false;
        }
        k += 1;
    }
    true
}

/// A decoded `.minisig` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureFile {
    pub untrusted_comment: String,
    pub key_id: [u8; 8],
    signature: [u8; 64],
    pub trusted_comment: String,
    global_signature: [u8; 64],
}

fn b64_fixed<const N: usize>(what: &str, s: &str) -> Result<[u8; N], Error> {
    let b = B64.decode(s.trim()).map_err(|e| fmt_err(format!("{what} base64: {e}")))?;
    b.try_into().map_err(|b: Vec<u8>| fmt_err(format!("{what}: {} bytes, want {N}", b.len())))
}

impl SignatureFile {
    pub fn decode(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_SIG_BYTES {
            return Err(fmt_err("signature file too large"));
        }
        let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
        let mut next = |what: &str| lines.next().ok_or_else(|| fmt_err(format!("signature file: missing {what}")));
        let untrusted = next("untrusted comment")?.trim_start_matches('\u{feff}');
        let untrusted_comment = untrusted
            .strip_prefix(UNTRUSTED)
            .ok_or_else(|| fmt_err("signature file: first line is not an untrusted comment"))?
            .to_string();
        let sig: [u8; 74] = b64_fixed("signature", next("signature")?)?;
        if sig[..2] == ALG_ED {
            return Err(fmt_err("legacy (not pre-hashed) signature; sign with a current minisign or pulsar-seat-release"));
        }
        if sig[..2] != ALG_ED_PREHASHED {
            return Err(fmt_err("unknown signature algorithm"));
        }
        let trusted_comment = next("trusted comment")?
            .strip_prefix(TRUSTED)
            .ok_or_else(|| fmt_err("signature file: third line is not a trusted comment"))?
            .to_string();
        let global_signature: [u8; 64] = b64_fixed("global signature", next("global signature")?)?;
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&sig[2..10]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&sig[10..74]);
        Ok(Self { untrusted_comment, key_id, signature, trusted_comment, global_signature })
    }

    #[cfg_attr(not(feature = "sign"), allow(dead_code))]
    pub(crate) fn encode(&self) -> String {
        let mut sig = Vec::with_capacity(74);
        sig.extend_from_slice(&ALG_ED_PREHASHED);
        sig.extend_from_slice(&self.key_id);
        sig.extend_from_slice(&self.signature);
        format!(
            "{UNTRUSTED}{}\n{}\n{TRUSTED}{}\n{}\n",
            self.untrusted_comment,
            B64.encode(sig),
            self.trusted_comment,
            B64.encode(self.global_signature)
        )
    }
}

/// BLAKE2b-512 of the data (the pre-hash minisign signs).
pub(crate) fn prehash(data: &[u8]) -> [u8; 64] {
    let mut h = Blake2b512::new();
    h.update(data);
    h.finalize().into()
}

/// Which accepted key signed, and its trusted comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub key_id: String,
    pub trusted_comment: String,
}

/// Trusted comment of a release checksum signature:
/// `timestamp:<unix s>\tfile:<name>\tversion:<X.Y.Z>\thashed` (minisign's own fields plus
/// `version`). The service requires `version` to equal the release tag and refuses a
/// `timestamp` older than the newest one it accepted before (rollback guard).
pub fn release_trusted_comment(timestamp: u64, file: &str, version: &str) -> String {
    format!("timestamp:{timestamp}\tfile:{file}\tversion:{version}\thashed")
}

/// Value of `key:` in a tab-separated trusted comment (`None` when absent or repeated:
/// a repeated field is ambiguous and never trusted).
pub fn trusted_comment_field<'a>(comment: &'a str, key: &str) -> Option<&'a str> {
    let mut found = None;
    for part in comment.split('\t') {
        if let Some((k, v)) = part.split_once(':')
            && k == key
        {
            if found.is_some() {
                return None;
            }
            found = Some(v);
        }
    }
    found
}

/// The one release version of every `PulsarSeat_<X.Y.Z>_<arch>.msi` listed in a
/// `SHA256SUMS.txt` text. Error when none is listed or two versions are mixed.
pub fn sums_version(sums: &str) -> Result<String, String> {
    let mut version: Option<String> = None;
    for line in sums.lines() {
        let Some(name) = line.split_whitespace().nth(1) else { continue };
        let name = name.trim_start_matches('*');
        let Some(rest) = name.strip_prefix("PulsarSeat_").and_then(|r| r.strip_suffix(".msi")) else { continue };
        let Some((v, _arch)) = rest.rsplit_once('_') else { continue };
        match &version {
            Some(have) if have != v => return Err(format!("SHA256SUMS.txt mixes versions {have} and {v}")),
            Some(_) => {}
            None => version = Some(v.to_string()),
        }
    }
    version.ok_or_else(|| "SHA256SUMS.txt lists no PulsarSeat_<version>_<arch>.msi".to_string())
}


/// Verify `data` against the `.minisig` text with one of `keys`.
pub fn verify(keys: &[PublicKey], data: &[u8], sig_text: &str) -> Result<Verified, Error> {
    let s = SignatureFile::decode(sig_text)?;
    let k = keys.iter().find(|k| k.key_id == s.key_id).ok_or_else(|| Error::UnknownKey(key_id_hex(&s.key_id)))?;
    let sig = Signature::from_bytes(&s.signature);
    k.key.verify_strict(&prehash(data), &sig).map_err(|_| Error::BadSignature)?;
    let mut global = Vec::with_capacity(64 + s.trusted_comment.len());
    global.extend_from_slice(&s.signature);
    global.extend_from_slice(s.trusted_comment.as_bytes());
    k.key.verify_strict(&global, &Signature::from_bytes(&s.global_signature)).map_err(|_| Error::BadSignature)?;
    Ok(Verified { key_id: k.key_id_hex(), trusted_comment: s.trusted_comment })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors of the minisign-verify crate (MIT; key E7620F1842B4E81F, data "test").
    const PK: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIG_PREHASHED: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==
";
    const SIG_LEGACY: &str = "untrusted comment: signature from minisign secret key
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=
trusted comment: timestamp:1555779966\tfile:test
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==
";

    #[test]
    fn minisign_vector_verifies() {
        let k = PublicKey::from_base64(PK).unwrap();
        assert_eq!(k.key_id_hex(), "E7620F1842B4E81F");
        assert_eq!(k.to_base64(), PK);
        let v = verify(std::slice::from_ref(&k), b"test", SIG_PREHASHED).unwrap();
        assert_eq!(v.trusted_comment, "timestamp:1556193335\tfile:test");
        assert_eq!(v.key_id, "E7620F1842B4E81F");
        // CRLF line endings (a checkout with autocrlf) still verify.
        assert!(verify(std::slice::from_ref(&k), b"test", &SIG_PREHASHED.replace('\n', "\r\n")).is_ok());
        assert_eq!(verify(std::slice::from_ref(&k), b"Test", SIG_PREHASHED), Err(Error::BadSignature));
        assert!(matches!(verify(&[k], b"test", SIG_LEGACY), Err(Error::Format(_))));
    }

    #[test]
    fn trusted_comment_is_covered() {
        let k = PublicKey::from_base64(PK).unwrap();
        let t = SIG_PREHASHED.replace("file:test", "file:evil");
        assert_eq!(verify(&[k], b"test", &t), Err(Error::BadSignature));
    }

    #[test]
    fn unknown_key_refused() {
        assert!(matches!(verify(&[], b"test", SIG_PREHASHED), Err(Error::UnknownKey(id)) if id == "E7620F1842B4E81F"));
    }

    #[test]
    fn signer_list() {
        let text = format!("# comment\n\nuntrusted comment: minisign public key E7620F1842B4E81F\r\n{PK}\n");
        let keys = parse_signers(&text).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(has_signer_lines(&text));
        assert!(!has_signer_lines("# only comments\n\n  \r\nuntrusted comment: x\n"));
        assert!(!has_signer_lines(""));
        assert!(parse_signers("# none\n").unwrap().is_empty());
        assert!(parse_signers(&format!("{PK}\n{PK}\n")).is_err(), "duplicate key id");
        assert!(parse_signers("RWQnotakey\n").is_err());
        // A secret key or a signature line pasted by mistake is refused.
        assert!(parse_signers("RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=").is_err());
    }

    /// The const check behind the release-build assertion reads lines exactly like
    /// `parse_signers` (BOM, NBSP and other Unicode whitespace included).
    #[test]
    fn signer_line_readers_agree() {
        let inputs = [
            "\u{feff}# SPDX-License-Identifier: GPL-3.0-only\n# only comments\n".to_string(),
            "\u{feff}\r\n\u{feff}  # x\r\n".to_string(),
            "\u{a0}# nbsp comment\n\u{3000}\u{2028}\n".to_string(),
            "\u{a0}untrusted comment: minisign public key\n\u{202f}\n".to_string(),
            format!("\u{feff}# header\n{PK}\n"),
            format!("\u{feff}{PK}\r\n"),
            format!("\u{a0}{PK}\u{a0}\n"),
            format!("# a\n\u{feff} \u{a0}{PK}\u{feff}"),
            "\u{feff}".to_string(),
            "\n\n".to_string(),
        ];
        for t in &inputs {
            let parsed = parse_signers(t).unwrap_or_else(|e| panic!("{t:?}: {e}"));
            assert_eq!(has_signer_lines(t), !parsed.is_empty(), "{t:?}");
        }
        assert!(!has_signer_lines(&inputs[0]), "BOM + comments only is no key");
        // The real file in the repository too.
        let real = include_str!("../../../installer/release-signers.txt");
        assert_eq!(has_signer_lines(real), !parse_signers(real).unwrap().is_empty());
    }

    #[test]
    fn release_comment_fields() {
        let c = release_trusted_comment(42, "SHA256SUMS.txt", "0.2.0");
        assert_eq!(trusted_comment_field(&c, "version"), Some("0.2.0"));
        assert_eq!(trusted_comment_field(&c, "timestamp"), Some("42"));
        assert_eq!(trusted_comment_field("timestamp:1\tfile:x", "version"), None);
        assert_eq!(trusted_comment_field("version:1.0.0\tversion:0.1.0", "version"), None, "repeated field");
        let sums = "aa  PulsarSeat_0.2.0_arm64.msi\nbb *PulsarSeat_0.2.0_x64.msi\n";
        assert_eq!(sums_version(sums).unwrap(), "0.2.0");
        assert!(sums_version("aa  PulsarSeat_0.2.0_arm64.msi\nbb  PulsarSeat_0.1.0_x64.msi\n").is_err());
        assert!(sums_version("aa  other.txt\n").is_err());
    }

    #[test]
    fn malformed_signature_files() {
        let k = PublicKey::from_base64(PK).unwrap();
        for bad in ["", "untrusted comment: x\n", "nope\nRUQ=\ntrusted comment: x\nAA==\n"] {
            assert!(matches!(verify(std::slice::from_ref(&k), b"test", bad), Err(Error::Format(_))), "{bad:?}");
        }
        let no_trusted = SIG_PREHASHED.replace("trusted comment: ", "comment: ");
        assert!(matches!(verify(std::slice::from_ref(&k), b"test", &no_trusted), Err(Error::Format(_))));
        assert!(matches!(verify(&[k], b"test", &"x".repeat(MAX_SIG_BYTES + 1)), Err(Error::Format(_))));
    }
}
