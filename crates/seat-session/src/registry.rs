// SPDX-License-Identifier: GPL-3.0-only
//! HKLM access behind a trait (64-bit view, values read raw, never expanded). The real
//! implementation is [`crate::win::registry::WinRegistry`]; tests use [`MemoryRegistry`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::journal::RegValue;
use crate::error::SysError;

/// HKLM registry, 64-bit view. `path` is relative to HKLM.
pub trait Registry: Send + Sync {
    fn get(&self, path: &str, name: &str) -> Result<RegValue, SysError>;
    /// `RegValue::Absent` deletes the value (no error if already absent). Creates the key.
    fn set(&self, path: &str, name: &str, value: &RegValue) -> Result<(), SysError>;
    /// The key exists (values or not).
    fn key_exists(&self, path: &str) -> Result<bool, SysError>;
    /// Every value directly under `path` (no subkeys), in no particular order. A missing
    /// key gives an empty list. Added 2026-11-13 (second listener); default: unsupported.
    fn values(&self, path: &str) -> Result<Vec<(String, RegValue)>, SysError> {
        Err(SysError::Other(format!(r"listing HKLM\{path} is not supported here")))
    }
    /// Deletes the key `path` with all its values and subkeys (no error if it is absent).
    /// Default: unsupported.
    fn delete_key(&self, path: &str) -> Result<(), SysError> {
        Err(SysError::Other(format!(r"deleting HKLM\{path} is not supported here")))
    }
    /// Names of the direct subkeys of `path`. A missing key gives an empty list. Added
    /// 2026-11-13 (fix-second-listener: other WinStation listeners); default: unsupported.
    fn subkeys(&self, path: &str) -> Result<Vec<String>, SysError> {
        Err(SysError::Other(format!(r"listing subkeys of HKLM\{path} is not supported here")))
    }
}

impl<T: Registry + ?Sized> Registry for std::sync::Arc<T> {
    fn get(&self, path: &str, name: &str) -> Result<RegValue, SysError> {
        (**self).get(path, name)
    }
    fn set(&self, path: &str, name: &str, value: &RegValue) -> Result<(), SysError> {
        (**self).set(path, name, value)
    }
    fn key_exists(&self, path: &str) -> Result<bool, SysError> {
        (**self).key_exists(path)
    }
    fn values(&self, path: &str) -> Result<Vec<(String, RegValue)>, SysError> {
        (**self).values(path)
    }
    fn delete_key(&self, path: &str) -> Result<(), SysError> {
        (**self).delete_key(path)
    }
    fn subkeys(&self, path: &str) -> Result<Vec<String>, SysError> {
        (**self).subkeys(path)
    }
}

/// DWORD value, or a decimal string (some Windows values are REG_SZ numbers).
pub fn get_u32(reg: &dyn Registry, path: &str, name: &str) -> Result<Option<u32>, SysError> {
    Ok(match reg.get(path, name)? {
        RegValue::Dword(d) => Some(d),
        RegValue::Sz(s) | RegValue::ExpandSz(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// String value (first entry of a MULTI_SZ; a DWORD as decimal).
pub fn get_string(reg: &dyn Registry, path: &str, name: &str) -> Result<Option<String>, SysError> {
    Ok(match reg.get(path, name)? {
        RegValue::Sz(s) | RegValue::ExpandSz(s) => Some(s),
        RegValue::Dword(d) => Some(d.to_string()),
        RegValue::MultiSz(v) => v.into_iter().next(),
        _ => None,
    })
}

fn key(path: &str, name: &str) -> (String, String) {
    (path.to_ascii_lowercase(), name.to_ascii_lowercase())
}

/// In-memory registry (tests, and the service's own tests through feature `fake`).
/// Paths and names are case-insensitive like the real registry.
#[derive(Default)]
pub struct MemoryRegistry {
    values: Mutex<HashMap<(String, String), RegValue>>,
    keys: Mutex<Vec<String>>,
    /// Every `set` fails while true.
    pub fail_writes: AtomicBool,
    /// `Some` = every `set` is recorded as `(path, name)` lower-cased (write-order tests).
    write_log: Mutex<Option<Vec<(String, String)>>>,
}

impl MemoryRegistry {
    /// Test helper: start / stop recording the order of `set` calls.
    pub fn record_writes(&self, on: bool) {
        *self.write_log.lock().unwrap_or_else(|p| p.into_inner()) = on.then(Vec::new);
    }

    /// Recorded `(path, name)` of every `set` since `record_writes(true)`.
    pub fn writes(&self) -> Vec<(String, String)> {
        self.write_log.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default()
    }

    pub fn snapshot(&self) -> HashMap<(String, String), RegValue> {
        self.values.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Test helper: set without the failure switch.
    pub fn put(&self, path: &str, name: &str, value: RegValue) {
        let mut m = self.values.lock().unwrap_or_else(|p| p.into_inner());
        if value == RegValue::Absent {
            m.remove(&key(path, name));
        } else {
            m.insert(key(path, name), value);
        }
    }

    /// Test helper: an (empty) key exists.
    pub fn put_key(&self, path: &str) {
        self.keys.lock().unwrap_or_else(|p| p.into_inner()).push(path.to_ascii_lowercase());
    }
}

impl Registry for MemoryRegistry {
    fn get(&self, path: &str, name: &str) -> Result<RegValue, SysError> {
        Ok(self.values.lock().unwrap_or_else(|p| p.into_inner()).get(&key(path, name)).cloned().unwrap_or(RegValue::Absent))
    }

    fn set(&self, path: &str, name: &str, value: &RegValue) -> Result<(), SysError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(SysError::Registry("fake write failure".into()));
        }
        if let Some(l) = self.write_log.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            l.push(key(path, name));
        }
        self.put(path, name, value.clone());
        Ok(())
    }

    fn key_exists(&self, path: &str) -> Result<bool, SysError> {
        let p = path.to_ascii_lowercase();
        let prefix = format!("{p}\\");
        let has_value = self.values.lock().unwrap_or_else(|e| e.into_inner()).keys().any(|(k, _)| *k == p || k.starts_with(&prefix));
        Ok(has_value || self.keys.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|k| *k == p || k.starts_with(&prefix)))
    }

    /// Names come back lower-cased (the fake keeps no original spelling).
    fn values(&self, path: &str) -> Result<Vec<(String, RegValue)>, SysError> {
        let p = path.to_ascii_lowercase();
        let mut v: Vec<(String, RegValue)> = self
            .values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((k, _), _)| *k == p)
            .map(|((_, n), v)| (n.clone(), v.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }

    fn delete_key(&self, path: &str) -> Result<(), SysError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(SysError::Registry("fake write failure".into()));
        }
        let p = path.to_ascii_lowercase();
        let prefix = format!("{p}\\");
        self.values.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _), _| !(*k == p || k.starts_with(&prefix)));
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| !(*k == p || k.starts_with(&prefix)));
        Ok(())
    }

    /// Names come back lower-cased.
    fn subkeys(&self, path: &str) -> Result<Vec<String>, SysError> {
        let prefix = format!("{}\\", path.to_ascii_lowercase());
        let child = |k: &str| k.strip_prefix(&prefix).and_then(|r| r.split('\\').next()).filter(|c| !c.is_empty()).map(str::to_owned);
        let mut out: Vec<String> = self.values.lock().unwrap_or_else(|e| e.into_inner()).keys().filter_map(|(k, _)| child(k)).collect();
        out.extend(self.keys.lock().unwrap_or_else(|e| e.into_inner()).iter().filter_map(|k| child(k)));
        out.sort();
        out.dedup();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_registry_is_case_insensitive() {
        let r = MemoryRegistry::default();
        r.set(r"SYSTEM\A", "Val", &RegValue::Dword(3)).unwrap();
        assert_eq!(r.get(r"system\a", "VAL").unwrap(), RegValue::Dword(3));
        assert!(r.key_exists(r"System").unwrap());
        assert!(!r.key_exists(r"Sys").unwrap());
        r.set(r"SYSTEM\A", "Val", &RegValue::Absent).unwrap();
        assert_eq!(r.get(r"SYSTEM\A", "Val").unwrap(), RegValue::Absent);
        r.put(r"X", "n", RegValue::Sz("12".into()));
        assert_eq!(get_u32(&r, "X", "n").unwrap(), Some(12));
        assert_eq!(get_string(&r, "X", "n").unwrap().as_deref(), Some("12"));
        r.fail_writes.store(true, Ordering::SeqCst);
        assert!(r.set("X", "n", &RegValue::Dword(1)).is_err());
    }
}
