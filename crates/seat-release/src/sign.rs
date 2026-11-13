// SPDX-License-Identifier: GPL-3.0-only
//! Key generation and signing (maintainer tool only; feature `sign`).

use super::{ALG_ED, Error, PublicKey, SignatureFile, UNTRUSTED, fmt_err, key_id_hex, prehash};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::Blake2b;
use blake2::Digest;
use blake2::digest::consts::U32;
use ed25519_dalek::{Signer as _, SigningKey};
use zeroize::{Zeroize, Zeroizing};

const KDF_NONE: [u8; 2] = [0, 0];
const KDF_SCRYPT: [u8; 2] = *b"Sc";
const CHK_B2: [u8; 2] = *b"B2";
/// 2 + 2 + 2 + 32 + 8 + 8 + 8 + 64 + 32.
const SK_LEN: usize = 158;

/// A release secret key. Never printed: no `Debug` / `Display`; memory is wiped on drop.
pub struct SecretKey {
    key_id: [u8; 8],
    signing: SigningKey,
}

fn checksum(key_id: &[u8; 8], sk64: &[u8; 64]) -> [u8; 32] {
    let mut h = Blake2b::<U32>::new();
    h.update(ALG_ED);
    h.update(key_id);
    h.update(sk64);
    h.finalize().into()
}

impl SecretKey {
    /// New random key (OS CSPRNG).
    pub fn generate() -> Result<Self, Error> {
        let mut seed = Zeroizing::new([0u8; 32]);
        let mut key_id = [0u8; 8];
        getrandom::fill(seed.as_mut()).map_err(|e| Error::Other(format!("random: {e}")))?;
        getrandom::fill(&mut key_id).map_err(|e| Error::Other(format!("random: {e}")))?;
        Ok(Self { key_id, signing: SigningKey::from_bytes(&seed) })
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from_parts(self.key_id, self.signing.verifying_key())
    }

    pub fn key_id_hex(&self) -> String {
        key_id_hex(&self.key_id)
    }

    /// Unencrypted minisign secret key file (as `minisign -G -W` writes).
    pub fn to_file_text(&self) -> Zeroizing<String> {
        let mut b = Zeroizing::new(Vec::with_capacity(SK_LEN));
        let sk64 = Zeroizing::new(self.signing.to_keypair_bytes());
        b.extend_from_slice(&ALG_ED);
        b.extend_from_slice(&KDF_NONE);
        b.extend_from_slice(&CHK_B2);
        b.extend_from_slice(&[0u8; 32]); // salt (unused without a KDF)
        b.extend_from_slice(&0u64.to_le_bytes()); // opslimit
        b.extend_from_slice(&0u64.to_le_bytes()); // memlimit
        b.extend_from_slice(&self.key_id);
        b.extend_from_slice(sk64.as_ref());
        b.extend_from_slice(&checksum(&self.key_id, &sk64));
        // Every buffer holding the key (raw, base64, the file text) is wiped on drop: the
        // base64 goes straight into a pre-sized Zeroizing buffer (no reallocation copies).
        let enc_len = SK_LEN.div_ceil(3) * 4;
        let mut enc = Zeroizing::new(String::with_capacity(enc_len));
        B64.encode_string(b.as_slice(), &mut enc);
        let head = format!("{UNTRUSTED}pulsar-seat release secret key {} (unencrypted; keep offline)\n", self.key_id_hex());
        let mut out = Zeroizing::new(String::with_capacity(head.len() + enc.len() + 1));
        out.push_str(&head);
        out.push_str(&enc);
        out.push('\n');
        out
    }

    /// Read an unencrypted minisign secret key file.
    pub fn from_file_text(text: &str) -> Result<Self, Error> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with(UNTRUSTED))
            .ok_or_else(|| fmt_err("secret key file: no key line"))?;
        // Decode into a pre-sized Zeroizing buffer: no reallocation leaves key bytes behind.
        // A line longer than the key's base64 is refused before decoding.
        if line.len() > SK_LEN.div_ceil(3) * 4 {
            return Err(fmt_err("secret key file: not a minisign Ed25519 secret key"));
        }
        let mut b = Zeroizing::new(Vec::with_capacity(SK_LEN.div_ceil(3) * 3));
        B64.decode_vec(line, &mut b).map_err(|_| fmt_err("secret key file: bad base64"))?;
        if b.len() != SK_LEN || b[..2] != ALG_ED || b[4..6] != CHK_B2 {
            return Err(fmt_err("secret key file: not a minisign Ed25519 secret key"));
        }
        if b[2..4] == KDF_SCRYPT {
            return Err(fmt_err(
                "password-protected minisign key: sign with `minisign -S -s <key> -m SHA256SUMS.txt` instead (same signature format)",
            ));
        }
        if b[2..4] != KDF_NONE {
            return Err(fmt_err("secret key file: unknown key derivation"));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&b[54..62]);
        let mut sk64 = Zeroizing::new([0u8; 64]);
        sk64.copy_from_slice(&b[62..126]);
        if checksum(&key_id, &sk64)[..] != b[126..158] {
            return Err(fmt_err("secret key file: checksum mismatch (damaged file)"));
        }
        let signing = SigningKey::from_keypair_bytes(&sk64).map_err(|_| fmt_err("secret key file: key pair mismatch"))?;
        Ok(Self { key_id, signing })
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.key_id.zeroize();
    }
}

/// Sign `data`: the `.minisig` text (pre-hashed `ED` form). `trusted_comment` must be one
/// line; it is covered by the global signature.
pub fn sign(sk: &SecretKey, data: &[u8], trusted_comment: &str, untrusted_comment: &str) -> Result<String, Error> {
    for c in [trusted_comment, untrusted_comment] {
        if c.contains(['\r', '\n']) {
            return Err(Error::Other("comments must be a single line".into()));
        }
    }
    let signature = sk.signing.sign(&prehash(data)).to_bytes();
    let mut global = Vec::with_capacity(64 + trusted_comment.len());
    global.extend_from_slice(&signature);
    global.extend_from_slice(trusted_comment.as_bytes());
    let global_signature = sk.signing.sign(&global).to_bytes();
    Ok(SignatureFile {
        untrusted_comment: untrusted_comment.to_string(),
        key_id: sk.key_id,
        signature,
        trusted_comment: trusted_comment.to_string(),
        global_signature,
    }
    .encode())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify;

    #[test]
    fn round_trip_and_minisign_interop() {
        let sk = SecretKey::generate().unwrap();
        let pk = sk.public();
        let data = b"0123  PulsarSeat_0.2.0_arm64.msi\n";
        let sig = sign(&sk, data, "timestamp:1\tfile:SHA256SUMS.txt\thashed", "signature from pulsar-seat release key").unwrap();
        let v = verify(std::slice::from_ref(&pk), data, &sig).unwrap();
        assert_eq!(v.key_id, sk.key_id_hex());
        assert!(verify(std::slice::from_ref(&pk), b"other", &sig).is_err());

        // An independent implementation accepts our key and signature.
        let mpk = minisign_verify::PublicKey::from_base64(&pk.to_base64()).unwrap();
        let msig = minisign_verify::Signature::decode(&sig).unwrap();
        mpk.verify(data, &msig, false).unwrap();
        assert_eq!(msig.trusted_comment(), "timestamp:1\tfile:SHA256SUMS.txt\thashed");
        // And the .pub text decodes there too.
        minisign_verify::PublicKey::decode(pk.to_file_text().trim_end()).unwrap();

        // Secret key file round trip; the file text never contains the public key base64
        // only (it is the whole secret blob), and a damaged file is refused.
        let text = sk.to_file_text();
        // The leak check of docs/release/checklist.md §1 greps for this prefix.
        assert!(text.lines().nth(1).unwrap().starts_with("RWQAAEIy"), "unencrypted key prefix");
        let back = SecretKey::from_file_text(&text).unwrap();
        assert_eq!(back.public(), pk);
        let mut damaged: Vec<char> = text.chars().collect();
        let i = damaged.len() - 10;
        damaged[i] = if damaged[i] == 'A' { 'B' } else { 'A' };
        assert!(SecretKey::from_file_text(&damaged.into_iter().collect::<String>()).is_err());
        assert!(sign(&sk, data, "two\nlines", "x").is_err());
    }

    #[test]
    fn encrypted_key_refused_with_hint() {
        let sk = SecretKey::generate().unwrap();
        let text = sk.to_file_text();
        let line = text.lines().nth(1).unwrap();
        let mut b = B64.decode(line).unwrap();
        b[2..4].copy_from_slice(&KDF_SCRYPT);
        let e = SecretKey::from_file_text(&B64.encode(&b)).err().unwrap();
        assert!(e.to_string().contains("minisign -S"), "{e}");
    }
}
