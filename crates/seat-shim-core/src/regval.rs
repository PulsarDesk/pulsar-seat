// SPDX-License-Identifier: GPL-3.0-only
//! Registry value model shared by the seat map, the installer and the fakes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RegValue {
    Dword(u32),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
}

impl RegValue {
    pub fn as_dword(&self) -> Option<u32> {
        match self {
            Self::Dword(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Sz(s) | Self::ExpandSz(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_multi(&self) -> Option<&[String]> {
        match self {
            Self::MultiSz(v) => Some(v),
            _ => None,
        }
    }
}

/// Find a value by name, case-insensitively (registry semantics).
pub fn find<'a>(values: &'a [(String, RegValue)], name: &str) -> Option<&'a RegValue> {
    values.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

/// Windows MULTI_SZ as UTF-16: every string NUL-terminated, then one extra NUL.
pub fn multi_sz_to_utf16(items: &[String]) -> Vec<u16> {
    let mut out = Vec::new();
    for s in items {
        out.extend(s.encode_utf16());
        out.push(0);
    }
    if items.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Parse a UTF-16 MULTI_SZ buffer (tolerates a missing final NUL).
pub fn utf16_to_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_round_trip() {
        let v = vec!["a".to_string(), "bc".to_string()];
        let w = multi_sz_to_utf16(&v);
        assert_eq!(w, vec![b'a' as u16, 0, b'b' as u16, b'c' as u16, 0, 0]);
        assert_eq!(utf16_to_multi_sz(&w), v);
        assert_eq!(multi_sz_to_utf16(&[]), vec![0, 0]);
        assert!(utf16_to_multi_sz(&[0, 0]).is_empty());
    }

    #[test]
    fn find_is_case_insensitive() {
        let v = vec![("SeatId".to_string(), RegValue::Sz("x".into()))];
        assert_eq!(find(&v, "seatid").and_then(RegValue::as_str), Some("x"));
        assert!(find(&v, "other").is_none());
    }
}
