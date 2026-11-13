// SPDX-License-Identifier: GPL-3.0-only
//! Minimal standard base64 (RFC 4648, with padding) for `binary_b64` journal values.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    let s: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if s.len() % 4 != 0 {
        return None;
    }
    let val = |c: u8| ALPHABET.iter().position(|&a| a == c).map(|p| p as u32);
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for (ci, chunk) in s.chunks(4).enumerate() {
        let last = ci == s.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        for s in [&b""[..], b"h", b"hi", b"hi!", b"\x00\xff\x10\x80"] {
            assert_eq!(super::decode(&super::encode(s)).unwrap(), s);
        }
        assert_eq!(super::encode(b"hi"), "aGk=");
        assert!(super::decode("a=bc").is_none());
        assert!(super::decode("abc").is_none());
    }
}
