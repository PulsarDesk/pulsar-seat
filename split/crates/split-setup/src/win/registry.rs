// SPDX-License-Identifier: GPL-3.0-only
//! HKLM registry: read-only probe helpers and the `SetupRegistry` writer (winreg, 64-bit view).

use crate::error::SetupError;
use crate::journal::ValueState;
use crate::system::SetupRegistry;
use winreg::enums::{
    HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ, KEY_WOW64_64KEY, REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ,
    REG_QWORD, REG_SZ,
};
use winreg::{RegKey, RegValue};

fn hklm() -> RegKey {
    RegKey::predef(HKEY_LOCAL_MACHINE)
}

pub(crate) fn open_read(path: &str) -> Option<RegKey> {
    hklm().open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY).ok()
}

fn err(path: &str, e: std::io::Error) -> SetupError {
    SetupError::Registry { path: path.into(), reason: e.to_string() }
}

fn utf16_strings(bytes: &[u8]) -> Vec<String> {
    let w: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    w.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// Typed read of one value; `Absent` when the key or value is missing.
pub(crate) fn read_state(path: &str, name: &str) -> Result<ValueState, SetupError> {
    let Some(k) = open_read(path) else { return Ok(ValueState::Absent) };
    let raw: RegValue = match k.get_raw_value(name) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ValueState::Absent),
        Err(e) => return Err(err(path, e)),
    };
    let b = &raw.bytes;
    Ok(match raw.vtype {
        REG_DWORD if b.len() >= 4 => ValueState::Dword(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        REG_QWORD if b.len() >= 8 => ValueState::Qword(u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]))),
        REG_SZ => ValueState::Sz(utf16_strings(b).into_iter().next().unwrap_or_default()),
        REG_EXPAND_SZ => ValueState::ExpandSz(utf16_strings(b).into_iter().next().unwrap_or_default()),
        REG_MULTI_SZ => ValueState::MultiSz(utf16_strings(b)),
        _ => ValueState::BinaryB64(crate::b64::encode(b)),
    })
}

/// Probe form: DWORDs as decimal, multi-strings joined with `\n`, unexpanded strings.
pub(crate) fn read_string(path: &str, name: &str) -> Option<String> {
    match read_state(path, name).ok()? {
        ValueState::Absent | ValueState::Key => None,
        ValueState::Dword(n) => Some(n.to_string()),
        ValueState::Qword(n) => Some(n.to_string()),
        ValueState::Sz(s) | ValueState::ExpandSz(s) | ValueState::BinaryB64(s) => Some(s),
        ValueState::MultiSz(v) => Some(v.join("\n")),
    }
}

fn to_utf16z(parts: &[&str], double: bool) -> Vec<u8> {
    let mut w: Vec<u16> = Vec::new();
    for p in parts {
        w.extend(p.encode_utf16());
        w.push(0);
    }
    if double || parts.is_empty() {
        w.push(0);
    }
    w.iter().flat_map(|c| c.to_le_bytes()).collect()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WinRegistry;

impl SetupRegistry for WinRegistry {
    fn get(&self, path: &str, name: &str) -> Result<ValueState, SetupError> {
        read_state(path, name)
    }

    fn set(&self, path: &str, name: &str, value: &ValueState) -> Result<(), SetupError> {
        let (k, _) = hklm().create_subkey_with_flags(path, KEY_ALL_ACCESS | KEY_WOW64_64KEY).map_err(|e| err(path, e))?;
        let raw = match value {
            ValueState::Dword(n) => RegValue { vtype: REG_DWORD, bytes: n.to_le_bytes().to_vec().into() },
            ValueState::Qword(n) => RegValue { vtype: REG_QWORD, bytes: n.to_le_bytes().to_vec().into() },
            ValueState::Sz(s) => RegValue { vtype: REG_SZ, bytes: to_utf16z(&[s], false).into() },
            ValueState::ExpandSz(s) => RegValue { vtype: REG_EXPAND_SZ, bytes: to_utf16z(&[s], false).into() },
            ValueState::MultiSz(v) => {
                let parts: Vec<&str> = v.iter().map(String::as_str).collect();
                RegValue { vtype: REG_MULTI_SZ, bytes: to_utf16z(&parts, true).into() }
            }
            ValueState::BinaryB64(s) => RegValue {
                vtype: REG_BINARY,
                bytes: crate::b64::decode(s).ok_or_else(|| SetupError::Registry { path: path.into(), reason: "bad base64".into() })?.into(),
            },
            ValueState::Absent | ValueState::Key => {
                return Err(SetupError::Registry { path: path.into(), reason: "cannot set absent/key".into() });
            }
        };
        k.set_raw_value(name, &raw).map_err(|e| err(path, e))
    }

    fn delete_value(&self, path: &str, name: &str) -> Result<(), SetupError> {
        let Ok(k) = hklm().open_subkey_with_flags(path, KEY_ALL_ACCESS | KEY_WOW64_64KEY) else { return Ok(()) };
        match k.delete_value(name) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err(path, e)),
        }
    }

    fn key_exists(&self, path: &str) -> bool {
        open_read(path).is_some()
    }

    fn create_key(&self, path: &str) -> Result<(), SetupError> {
        hklm().create_subkey_with_flags(path, KEY_ALL_ACCESS | KEY_WOW64_64KEY).map(|_| ()).map_err(|e| err(path, e))
    }

    fn delete_tree(&self, path: &str) -> Result<(), SetupError> {
        let (parent, leaf) = match path.rsplit_once('\\') {
            Some(x) => x,
            None => return Err(SetupError::Registry { path: path.into(), reason: "refusing to delete a top-level key".into() }),
        };
        let Ok(p) = hklm().open_subkey_with_flags(parent, KEY_ALL_ACCESS | KEY_WOW64_64KEY) else { return Ok(()) };
        match p.delete_subkey_all(leaf) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err(path, e)),
        }
    }
}
