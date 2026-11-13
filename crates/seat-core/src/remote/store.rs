// SPDX-License-Identifier: GPL-3.0-only
//! Persistence of global settings and seats behind [`ConfigStore`].
//!
//! Production: the Seat config document (`seat-service` `remote::DocStore`, merge step 6;
//! passwords in the LSA `SecretStore`). Here: [`MemoryStore`] for tests and
//! [`FileStore`] (JSON file, kept for tests). Neither of these two persists
//! passwords to disk: they keep them in process memory only (wiped on drop), so a
//! `--console` service can start a seat until it exits.

use crate::Secret;
use crate::remote::{RemoteSeatRecord, RemoteSettings};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad data: {0}")]
    Data(String),
    #[error("seat '{0}' not found")]
    NotFound(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub global: RemoteSettings,
    pub seats: Vec<RemoteSeatRecord>,
}

impl Config {
    pub fn seat(&self, name: &str) -> Option<&RemoteSeatRecord> {
        self.seats.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }
}

pub trait ConfigStore: Send {
    fn load(&self) -> Result<Config, StoreError>;
    fn put_global(&mut self, global: &RemoteSettings) -> Result<(), StoreError>;
    /// Insert or replace the seat with the same name.
    fn put_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), StoreError>;
    fn delete_seat(&mut self, name: &str) -> Result<(), StoreError>;
    /// Rename a seat key, keeping its stored password. The caller writes the new values
    /// with [`ConfigStore::put_seat`] afterwards.
    fn rename_seat(&mut self, old: &str, new: &str) -> Result<(), StoreError>;
    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), StoreError>;
    fn has_password(&self, name: &str) -> bool;
    /// The stored password, for the seat start only (never sent to the manager).
    fn password(&self, name: &str) -> Result<Option<Secret>, StoreError>;
}

impl<T: ConfigStore + ?Sized> ConfigStore for Box<T> {
    fn load(&self) -> Result<Config, StoreError> {
        (**self).load()
    }
    fn put_global(&mut self, global: &RemoteSettings) -> Result<(), StoreError> {
        (**self).put_global(global)
    }
    fn put_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), StoreError> {
        (**self).put_seat(seat)
    }
    fn delete_seat(&mut self, name: &str) -> Result<(), StoreError> {
        (**self).delete_seat(name)
    }
    fn rename_seat(&mut self, old: &str, new: &str) -> Result<(), StoreError> {
        (**self).rename_seat(old, new)
    }
    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), StoreError> {
        (**self).set_password(name, password)
    }
    fn has_password(&self, name: &str) -> bool {
        (**self).has_password(name)
    }
    fn password(&self, name: &str) -> Result<Option<Secret>, StoreError> {
        (**self).password(name)
    }
}

/// Seat names are compared case-insensitively everywhere; keys are stored lowercased.
fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    global: RemoteSettings,
    seats: BTreeMap<String, RemoteSeatRecord>,
    /// In memory only; wiped when replaced or dropped.
    passwords: BTreeMap<String, Zeroizing<String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn from_config(c: Config) -> Self {
        Self {
            global: c.global,
            seats: c.seats.into_iter().map(|s| (key(&s.name), s)).collect(),
            passwords: BTreeMap::new(),
        }
    }

    fn snapshot(&self) -> Config {
        Config { global: self.global.clone(), seats: self.seats.values().cloned().collect() }
    }
}

impl ConfigStore for MemoryStore {
    fn load(&self) -> Result<Config, StoreError> {
        Ok(self.snapshot())
    }

    fn put_global(&mut self, global: &RemoteSettings) -> Result<(), StoreError> {
        self.global = global.clone();
        Ok(())
    }

    fn put_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), StoreError> {
        self.seats.insert(key(&seat.name), seat.clone());
        Ok(())
    }

    fn delete_seat(&mut self, name: &str) -> Result<(), StoreError> {
        self.passwords.remove(&key(name));
        self.seats.remove(&key(name)).map(|_| ()).ok_or_else(|| StoreError::NotFound(name.into()))
    }

    fn rename_seat(&mut self, old: &str, new: &str) -> Result<(), StoreError> {
        let mut seat = self.seats.remove(&key(old)).ok_or_else(|| StoreError::NotFound(old.into()))?;
        seat.name = new.into();
        self.seats.insert(key(new), seat);
        if let Some(pw) = self.passwords.remove(&key(old)) {
            self.passwords.insert(key(new), pw);
        }
        Ok(())
    }

    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), StoreError> {
        if !self.seats.contains_key(&key(name)) {
            return Err(StoreError::NotFound(name.into()));
        }
        self.passwords.insert(key(name), Zeroizing::new(password.0.clone()));
        Ok(())
    }

    fn has_password(&self, name: &str) -> bool {
        self.passwords.contains_key(&key(name))
    }

    fn password(&self, name: &str) -> Result<Option<Secret>, StoreError> {
        Ok(self.passwords.get(&key(name)).map(|p| Secret(p.as_str().to_owned())))
    }
}

/// JSON file store for development. Every change rewrites the file atomically
/// (write a temp file next to it, then rename).
#[derive(Debug)]
pub struct FileStore {
    path: PathBuf,
    mem: MemoryStore,
}

impl FileStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let mem = match std::fs::read(&path) {
            Ok(bytes) => {
                let c: Config = serde_json::from_slice(&bytes).map_err(|e| StoreError::Data(e.to_string()))?;
                MemoryStore::from_config(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => MemoryStore::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, mem })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn flush(&self) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&self.mem.snapshot()).map_err(|e| StoreError::Data(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl ConfigStore for FileStore {
    fn load(&self) -> Result<Config, StoreError> {
        self.mem.load()
    }

    fn put_global(&mut self, global: &RemoteSettings) -> Result<(), StoreError> {
        self.mem.put_global(global)?;
        self.flush()
    }

    fn put_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), StoreError> {
        self.mem.put_seat(seat)?;
        self.flush()
    }

    fn delete_seat(&mut self, name: &str) -> Result<(), StoreError> {
        self.mem.delete_seat(name)?;
        self.flush()
    }

    fn rename_seat(&mut self, old: &str, new: &str) -> Result<(), StoreError> {
        self.mem.rename_seat(old, new)?;
        self.flush()
    }

    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), StoreError> {
        // Dev store: kept in process memory only; never written to the JSON file.
        self.mem.set_password(name, password)
    }

    fn has_password(&self, name: &str) -> bool {
        self.mem.has_password(name)
    }

    fn password(&self, name: &str) -> Result<Option<Secret>, StoreError> {
        self.mem.password(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(name: &str) -> RemoteSeatRecord {
        RemoteSeatRecord { name: name.into(), display_name: name.into(), port: 47989, ..Default::default() }
    }

    #[test]
    fn memory_store_crud_and_rename_keeps_password() {
        let mut s = MemoryStore::new();
        s.put_seat(&seat("A")).unwrap();
        s.set_password("a", &Secret("pw".into())).unwrap();
        assert!(s.has_password("A"));
        s.rename_seat("A", "B").unwrap();
        assert!(!s.has_password("A"));
        assert!(s.has_password("b"));
        assert_eq!(s.password("B").unwrap().unwrap().0, "pw");
        assert_eq!(s.load().unwrap().seats[0].name, "B");
        s.delete_seat("B").unwrap();
        assert!(!s.has_password("B"));
        assert!(matches!(s.delete_seat("B"), Err(StoreError::NotFound(_))));
        assert!(matches!(s.set_password("zz", &Secret("x".into())), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn file_store_persists_but_never_writes_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.json");
        {
            let mut s = FileStore::open(&path).unwrap();
            s.put_global(&RemoteSettings { hid_isolation: true, ..Default::default() }).unwrap();
            s.put_seat(&seat("Salon")).unwrap();
            s.set_password("Salon", &Secret("very-secret-pw".into())).unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("very-secret-pw"));
        let s = FileStore::open(&path).unwrap();
        let c = s.load().unwrap();
        assert!(c.global.hid_isolation);
        assert_eq!(c.seat("salon").unwrap().name, "Salon");
    }

    #[test]
    fn file_store_rejects_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"{ nope").unwrap();
        assert!(matches!(FileStore::open(&path), Err(StoreError::Data(_))));
    }
}
