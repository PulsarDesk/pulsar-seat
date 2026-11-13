// SPDX-License-Identifier: GPL-3.0-only
//! HKLM through `winreg`, always the 64-bit view.

use std::borrow::Cow;
use std::io::ErrorKind;

use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, RegType};

use crate::TermwrapError;
use crate::journal::{RegValue, b64_decode, b64_encode};
use crate::system::Registry;

pub struct WinRegistry;

fn err(what: &str, path: &str, name: &str, e: std::io::Error) -> TermwrapError {
    TermwrapError::Registry(format!(r"{what} HKLM\{path}\{name}: {e}"))
}

fn utf16(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn to_bytes(w: impl IntoIterator<Item = u16>) -> Vec<u8> {
    w.into_iter().flat_map(u16::to_le_bytes).collect()
}

fn decode(v: &winreg::RegValue) -> RegValue {
    let b = v.bytes.as_ref();
    match v.vtype {
        RegType::REG_DWORD if b.len() >= 4 => RegValue::Dword(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        RegType::REG_SZ | RegType::REG_EXPAND_SZ => {
            let w = utf16(b);
            let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            let s = String::from_utf16_lossy(&w[..end]);
            if v.vtype == RegType::REG_SZ { RegValue::Sz(s) } else { RegValue::ExpandSz(s) }
        }
        RegType::REG_MULTI_SZ => {
            let w = utf16(b);
            RegValue::MultiSz(
                w.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect(),
            )
        }
        _ => RegValue::BinaryB64(b64_encode(b)),
    }
}

fn encode(v: &RegValue) -> Result<(RegType, Vec<u8>), TermwrapError> {
    Ok(match v {
        RegValue::Absent => unreachable!("handled by caller"),
        RegValue::Dword(d) => (RegType::REG_DWORD, d.to_le_bytes().to_vec()),
        RegValue::Sz(s) => (RegType::REG_SZ, to_bytes(s.encode_utf16().chain([0]))),
        RegValue::ExpandSz(s) => (RegType::REG_EXPAND_SZ, to_bytes(s.encode_utf16().chain([0]))),
        RegValue::MultiSz(list) => {
            let mut w: Vec<u16> = Vec::new();
            for s in list {
                w.extend(s.encode_utf16());
                w.push(0);
            }
            w.push(0);
            (RegType::REG_MULTI_SZ, to_bytes(w))
        }
        RegValue::BinaryB64(s) => {
            (RegType::REG_BINARY, b64_decode(s).ok_or_else(|| TermwrapError::Registry("bad base64 value".into()))?)
        }
    })
}

impl Registry for WinRegistry {
    fn get(&self, path: &str, name: &str) -> Result<RegValue, TermwrapError> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = match hklm.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(RegValue::Absent),
            Err(e) => return Err(err("open", path, name, e)),
        };
        match key.get_raw_value(name) {
            Ok(v) => Ok(decode(&v)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(RegValue::Absent),
            Err(e) => Err(err("read", path, name, e)),
        }
    }

    fn set(&self, path: &str, name: &str, value: &RegValue) -> Result<(), TermwrapError> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if *value == RegValue::Absent {
            let key = match hklm.open_subkey_with_flags(path, KEY_SET_VALUE | KEY_WOW64_64KEY) {
                Ok(k) => k,
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(err("open", path, name, e)),
            };
            return match key.delete_value(name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                Err(e) => Err(err("delete", path, name, e)),
            };
        }
        let (vtype, bytes) = encode(value)?;
        let (key, _) = hklm
            .create_subkey_with_flags(path, KEY_SET_VALUE | KEY_WOW64_64KEY)
            .map_err(|e| err("create", path, name, e))?;
        key.set_raw_value(name, &winreg::RegValue { bytes: Cow::Owned(bytes), vtype })
            .map_err(|e| err("write", path, name, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        for v in [
            RegValue::Dword(7),
            RegValue::Sz("abc".into()),
            RegValue::ExpandSz(r"%SystemRoot%\System32\termsrv.dll".into()),
            RegValue::MultiSz(vec!["a".into(), "bc".into()]),
            RegValue::BinaryB64(b64_encode(&[1, 2, 3, 250])),
        ] {
            let (vtype, bytes) = encode(&v).unwrap();
            assert_eq!(decode(&winreg::RegValue { bytes: Cow::Owned(bytes), vtype }), v);
        }
    }
}
