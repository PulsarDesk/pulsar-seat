// SPDX-License-Identifier: GPL-3.0-only
//! HKLM registry through `winreg` (64-bit view; values read raw, never expanded).

use crate::SessionError;
use crate::rds::{RegData, Registry};
use std::borrow::Cow;
use std::io;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, RegType};
use winreg::{RegKey, RegValue};

/// The real HKLM. Writes need an elevated / SYSTEM caller.
#[derive(Debug, Clone, Copy, Default)]
pub struct HklmRegistry;

fn io_err(api: &'static str, e: &io::Error) -> SessionError {
    SessionError::Win32 { api, code: e.raw_os_error().unwrap_or(0) as u32 }
}

fn utf16_bytes(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn decode_sz(b: &[u8]) -> String {
    let w = utf16_bytes(b);
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn decode_multi(b: &[u8]) -> Vec<String> {
    let w = utf16_bytes(b);
    w.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

fn encode_sz(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

/// Decodes raw registry bytes. Unsupported types (binary, qword) read as `None`.
pub(crate) fn decode(v: &RegValue<'_>) -> Option<RegData> {
    match v.vtype {
        RegType::REG_DWORD if v.bytes.len() >= 4 => Some(RegData::Dword(u32::from_le_bytes([v.bytes[0], v.bytes[1], v.bytes[2], v.bytes[3]]))),
        RegType::REG_SZ => Some(RegData::Sz(decode_sz(&v.bytes))),
        RegType::REG_EXPAND_SZ => Some(RegData::ExpandSz(decode_sz(&v.bytes))),
        RegType::REG_MULTI_SZ => Some(RegData::MultiSz(decode_multi(&v.bytes))),
        _ => None,
    }
}

fn encode(d: &RegData) -> RegValue<'static> {
    let (bytes, vtype) = match d {
        RegData::Dword(v) => (v.to_le_bytes().to_vec(), RegType::REG_DWORD),
        RegData::Sz(s) => (encode_sz(s), RegType::REG_SZ),
        RegData::ExpandSz(s) => (encode_sz(s), RegType::REG_EXPAND_SZ),
        RegData::MultiSz(v) => {
            let mut b: Vec<u8> = Vec::new();
            for s in v {
                b.extend(encode_sz(s));
            }
            b.extend([0, 0]);
            (b, RegType::REG_MULTI_SZ)
        }
    };
    RegValue { bytes: Cow::Owned(bytes), vtype }
}

/// Raw bytes of a value (for binary values such as certificate hashes).
pub fn get_binary(key: &str, value: &str) -> Option<Vec<u8>> {
    let k = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_QUERY_VALUE | KEY_WOW64_64KEY).ok()?;
    let v = k.get_raw_value(value).ok()?;
    (v.vtype == RegType::REG_BINARY).then(|| v.bytes.into_owned())
}

impl Registry for HklmRegistry {
    fn get(&self, key: &str, value: &str) -> Result<Option<RegData>, SessionError> {
        let k = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_QUERY_VALUE | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err("RegOpenKeyExW", &e)),
        };
        match k.get_raw_value(value) {
            Ok(v) => Ok(decode(&v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err("RegQueryValueExW", &e)),
        }
    }

    fn set(&self, key: &str, value: &str, data: &RegData) -> Result<(), SessionError> {
        let (k, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .create_subkey_with_flags(key, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY)
            .map_err(|e| io_err("RegCreateKeyExW", &e))?;
        k.set_raw_value(value, &encode(data)).map_err(|e| io_err("RegSetValueExW", &e))?;
        Ok(())
    }

    fn delete(&self, key: &str, value: &str) -> Result<(), SessionError> {
        let k = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_SET_VALUE | KEY_WOW64_64KEY) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err("RegOpenKeyExW", &e)),
        };
        match k.delete_value(value) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("RegDeleteValueW", &e)),
        }
    }

    fn key_exists(&self, key: &str) -> Result<bool, SessionError> {
        match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err("RegOpenKeyExW", &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        for d in [
            RegData::Dword(0xFFFF_FFFF),
            RegData::Sz("termsrv.dll".into()),
            RegData::ExpandSz(r"%SystemRoot%\System32\termsrv.dll".into()),
            RegData::MultiSz(vec!["a".into(), "bç".into()]),
        ] {
            assert_eq!(decode(&encode(&d)), Some(d));
        }
    }
}
