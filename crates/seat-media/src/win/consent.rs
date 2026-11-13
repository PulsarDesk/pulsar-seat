// SPDX-License-Identifier: GPL-3.0-only
//! Per-user camera consent under `HKEY_USERS\<SID>` (see `gate::camera`). Two values are
//! handled together: `...\ConsentStore\webcam\Value` (apps) and
//! `...\ConsentStore\webcam\NonPackaged\Value` (desktop apps). [`ConsentIo::get`] returns
//! them encoded as `<apps>;<desktop>` (`-` = absent) so the journal restores both; `set`
//! with a plain `Allow` / `Deny` writes both. The service runs as LocalSystem and writes
//! the hive of a signed-in user only. **Untested on hardware.**

use crate::gate::camera::{CONSENT_SUBKEY, CONSENT_VALUE};
use crate::gate::{ConsentIo, valid_sid};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::io::ErrorKind as IoKind;
use winreg::RegKey;
use winreg::enums::{HKEY_USERS, KEY_READ, KEY_WRITE};

const DESKTOP_SUBKEY: &str = "NonPackaged";

fn err(e: std::io::Error) -> BackendError {
    let kind = if e.kind() == IoKind::PermissionDenied { ErrorKind::Denied } else { ErrorKind::Os };
    BackendError::new(kind, "consent_registry_failed", e.to_string())
}

fn check(sid: &str) -> BackendResult<()> {
    if valid_sid(sid) { Ok(()) } else { Err(BackendError::new(ErrorKind::Internal, "bad_sid", "invalid user SID")) }
}

fn paths(sid: &str) -> [String; 2] {
    [format!(r"{sid}\{CONSENT_SUBKEY}"), format!(r"{sid}\{CONSENT_SUBKEY}\{DESKTOP_SUBKEY}")]
}

fn read(path: &str) -> BackendResult<Option<String>> {
    match RegKey::predef(HKEY_USERS).open_subkey_with_flags(path, KEY_READ) {
        Ok(k) => match k.get_value::<String, _>(CONSENT_VALUE) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == IoKind::NotFound => Ok(None),
            Err(e) => Err(err(e)),
        },
        Err(e) if e.kind() == IoKind::NotFound => Ok(None),
        Err(e) => Err(err(e)),
    }
}

fn write(path: &str, value: Option<&str>) -> BackendResult<()> {
    let users = RegKey::predef(HKEY_USERS);
    match value {
        Some(v) => {
            let (k, _) = users.create_subkey_with_flags(path, KEY_WRITE).map_err(err)?;
            k.set_value(CONSENT_VALUE, &v.to_string()).map_err(err)
        }
        None => match users.open_subkey_with_flags(path, KEY_WRITE) {
            Ok(k) => match k.delete_value(CONSENT_VALUE) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == IoKind::NotFound => Ok(()),
                Err(e) => Err(err(e)),
            },
            Err(e) if e.kind() == IoKind::NotFound => Ok(()),
            Err(e) => Err(err(e)),
        },
    }
}

/// `Some("Allow;-")` → `[Some("Allow"), None]`; a plain value applies to both.
fn decode(v: &str) -> [Option<String>; 2] {
    let one = |s: &str| (s != "-" && !s.is_empty()).then(|| s.to_string());
    match v.split_once(';') {
        Some((a, b)) => [one(a), one(b)],
        None => [one(v), one(v)],
    }
}

fn encode(v: &[Option<String>; 2]) -> Option<String> {
    if v.iter().all(Option::is_none) {
        return None;
    }
    let s = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    Some(format!("{};{}", s(&v[0]), s(&v[1])))
}

pub struct WinConsent;

impl ConsentIo for WinConsent {
    fn get(&self, user_sid: &str) -> BackendResult<Option<String>> {
        check(user_sid)?;
        let [a, b] = paths(user_sid);
        Ok(encode(&[read(&a)?, read(&b)?]))
    }

    fn set(&self, user_sid: &str, value: Option<&str>) -> BackendResult<()> {
        check(user_sid)?;
        if !self.hive_loaded(user_sid) {
            return Err(BackendError::new(ErrorKind::NotReady, "user_hive_not_loaded", "user is not signed in"));
        }
        let vals = value.map(decode).unwrap_or([None, None]);
        for (p, v) in paths(user_sid).iter().zip(vals.iter()) {
            write(p, v.as_deref())?;
        }
        Ok(())
    }

    fn hive_loaded(&self, user_sid: &str) -> bool {
        valid_sid(user_sid) && RegKey::predef(HKEY_USERS).open_subkey_with_flags(user_sid, KEY_READ).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_roundtrip() {
        let v = [Some("Allow".to_string()), None];
        let e = encode(&v).unwrap();
        assert_eq!(e, "Allow;-");
        assert_eq!(decode(&e), v);
        assert_eq!(decode("Deny"), [Some("Deny".into()), Some("Deny".into())]);
        assert_eq!(encode(&[None, None]), None);
    }
}
