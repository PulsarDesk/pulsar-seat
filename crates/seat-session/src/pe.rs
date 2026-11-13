// SPDX-License-Identifier: GPL-3.0-only
//! Minimal PE header read: the COFF `Machine` field of a DLL, to make sure the wrapper
//! matches the native architecture of the TermService svchost (an x64 DLL cannot load
//! into an ARM64 svchost and TermService would fail to start). Adapted from pulsar-split
//! `split-termwrap`.

use serde::{Deserialize, Serialize};

/// Authenticode state of a file (WinVerifyTrust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureState {
    /// Signed, chain to a trusted root, not tampered.
    Trusted,
    /// Signed but the signature or chain does not verify (tampered, untrusted root,
    /// expired without timestamp, check failed).
    Untrusted,
    /// No embedded signature.
    Unsigned,
}

pub const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;
pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
pub const IMAGE_FILE_MACHINE_ARM64: u16 = 0xAA64;

/// Bytes needed from the start of the file to cover typical headers.
pub const HEADER_PROBE_BYTES: usize = 4096;

/// `Machine` from the COFF header, or `None` if `bytes` is not a PE image.
pub fn parse_machine(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(bytes[0x3c..0x40].try_into().ok()?) as usize;
    let sig = bytes.get(e_lfanew..e_lfanew.checked_add(6)?)?;
    if &sig[0..4] != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes([sig[4], sig[5]]))
}

pub fn machine_name(m: u16) -> &'static str {
    match m {
        IMAGE_FILE_MACHINE_I386 => "x86",
        IMAGE_FILE_MACHINE_AMD64 => "x64",
        IMAGE_FILE_MACHINE_ARM64 => "arm64",
        _ => "unknown",
    }
}

#[cfg(test)]
pub(crate) fn fake_pe(machine: u16) -> Vec<u8> {
    let mut b = vec![0u8; 0x100];
    b[0..2].copy_from_slice(b"MZ");
    b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    b[0x80..0x84].copy_from_slice(b"PE\0\0");
    b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_machine() {
        assert_eq!(parse_machine(&fake_pe(IMAGE_FILE_MACHINE_AMD64)), Some(IMAGE_FILE_MACHINE_AMD64));
        assert_eq!(parse_machine(&fake_pe(IMAGE_FILE_MACHINE_ARM64)), Some(IMAGE_FILE_MACHINE_ARM64));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_machine(b""), None);
        assert_eq!(parse_machine(&[0u8; 0x100]), None);
        let mut b = fake_pe(IMAGE_FILE_MACHINE_AMD64);
        b[0x3c..0x40].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert_eq!(parse_machine(&b), None);
        let mut b = fake_pe(IMAGE_FILE_MACHINE_AMD64);
        b[0x80] = b'X';
        assert_eq!(parse_machine(&b), None);
    }
}
