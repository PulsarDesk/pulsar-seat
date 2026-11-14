// SPDX-License-Identifier: GPL-3.0-only
//! Health frame the module writes once per process to [`crate::HEALTH_PIPE`] (spec 50
//! §12.2): u32 LE payload length + UTF-8 JSON, whole frame at most 4 KiB.

use crate::error::ShimError;
use serde::{Deserialize, Serialize};

pub const HEALTH_VERSION: u32 = 1;
pub const MAX_HEALTH_FRAME: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimHealth {
    pub v: u32,
    pub pid: u32,
    pub session_id: u32,
    /// File name only (no user path, no user names).
    pub exe: String,
    pub module_version: String,
    /// `none` / `console` / `seat` / `kiosk` (round 4).
    pub role: String,
    pub hooks_on: bool,
    /// Reason code, e.g. `shim_kill_switch`, `shim_target_disabled`, `hook_failed`, `fault`.
    pub error: Option<String>,
}

fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut i = max;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
    }
}

/// Never fails: over-long `exe` / `error` strings are cut so the frame fits.
pub fn encode_health(h: &ShimHealth) -> Vec<u8> {
    let mut h = h.clone();
    let mut json = serde_json::to_vec(&h).unwrap_or_default();
    if json.len() + 4 > MAX_HEALTH_FRAME {
        truncate(&mut h.exe, 260);
        if let Some(e) = h.error.as_mut() {
            truncate(e, 256);
        }
        truncate(&mut h.module_version, 32);
        truncate(&mut h.role, 16);
        json = serde_json::to_vec(&h).unwrap_or_default();
    }
    let mut out = Vec::with_capacity(json.len() + 4);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out
}

pub fn decode_health(b: &[u8]) -> Result<ShimHealth, ShimError> {
    if b.len() < 4 || b.len() > MAX_HEALTH_FRAME {
        return Err(ShimError::Decode("health frame size".into()));
    }
    let len = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    if len != b.len() - 4 {
        return Err(ShimError::Decode("health frame length mismatch".into()));
    }
    let h: ShimHealth = serde_json::from_slice(&b[4..]).map_err(|e| ShimError::Decode(format!("health json: {e}")))?;
    if h.v != HEALTH_VERSION {
        return Err(ShimError::Decode(format!("health version {}", h.v)));
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ShimHealth {
        ShimHealth {
            v: 1,
            pid: 4242,
            session_id: 3,
            exe: "explorer.exe".into(),
            module_version: "0.1.0".into(),
            role: "seat".into(),
            hooks_on: true,
            error: None,
        }
    }

    #[test]
    fn round_trip() {
        let f = encode_health(&sample());
        assert_eq!(u32::from_le_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4);
        assert_eq!(decode_health(&f).unwrap(), sample());
    }

    #[test]
    fn long_fields_are_cut_to_fit() {
        let mut h = sample();
        h.exe = "é".repeat(5000);
        h.error = Some("x".repeat(5000));
        let f = encode_health(&h);
        assert!(f.len() <= MAX_HEALTH_FRAME);
        let d = decode_health(&f).unwrap();
        assert!(d.exe.len() <= 260 && d.error.unwrap().len() <= 256);
    }

    #[test]
    fn rejects_bad_frames() {
        let mut f = encode_health(&sample());
        f.push(b' ');
        assert!(decode_health(&f).is_err());
        assert!(decode_health(&[1, 0]).is_err());
        assert!(decode_health(&vec![0u8; 5000]).is_err());
        let mut h = sample();
        h.v = 2;
        assert!(decode_health(&encode_health(&h)).is_err());
        let junk = [3u8, 0, 0, 0, b'{', b'x', b'}'];
        assert!(decode_health(&junk).is_err());
    }
}
