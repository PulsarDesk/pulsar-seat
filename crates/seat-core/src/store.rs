// SPDX-License-Identifier: GPL-3.0-only
//! Persistence of the config document and of seat secrets.
//!
//! - [`ConfigStore`]: production = [`FileStore`] on `%ProgramData%\Pulsar Seat\config.json`
//!   (the service sets the folder ACL, `consts::DATA_DIR_SDDL`); tests = [`MemoryStore`].
//! - [`SecretStore`]: production = LSA private data in `seat-service`
//!   (`consts::LSA_SEAT_SECRET_PREFIX` + two-digit seat); tests / `--console` dev mode =
//!   [`MemorySecretStore`] (memory only, never on disk).

use crate::config::{Config, ConfigError};
use crate::ids::SeatNumber;
use crate::secret::Secret;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `save` was based on an older revision than the stored one.
    #[error("revision conflict: stored {stored}, caller based on {based_on}")]
    RevisionConflict { stored: u64, based_on: u64 },
    #[error("secret store: {0}")]
    Secret(String),
}

pub trait ConfigStore: Send + Sync {
    /// Load (and migrate). A missing file yields `Config::default()`.
    fn load(&self) -> Result<Config, StoreError>;
    /// Atomically replace the document. The caller has already bumped `revision`.
    fn save(&self, config: &Config) -> Result<(), StoreError>;
}

/// JSON file with atomic replace (write `config.json.tmp`, flush, rename over).
#[derive(Debug)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ConfigStore for FileStore {
    fn load(&self) -> Result<Config, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(Config::from_json(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, config: &Config) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(config.to_json().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// In-memory config store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Option<Config>>,
}

impl MemoryStore {
    pub fn with(config: Config) -> Self {
        Self { inner: Mutex::new(Some(config)) }
    }
}

impl ConfigStore for MemoryStore {
    fn load(&self) -> Result<Config, StoreError> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default())
    }
    fn save(&self, config: &Config) -> Result<(), StoreError> {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = Some(config.clone());
        Ok(())
    }
}

/// Seat auto-logon passwords. Write-only from the manager's point of view: no pipe request
/// ever returns `get`'s result.
pub trait SecretStore: Send + Sync {
    /// Store (`Some`) or delete (`None`) the seat's password.
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError>;
    fn has(&self, seat: SeatNumber) -> bool;
    /// For a seat start only.
    fn get(&self, seat: SeatNumber) -> Result<Option<Secret>, StoreError>;
}

/// Memory-only secret store (tests, `--console` dev mode).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    inner: Mutex<BTreeMap<SeatNumber, Secret>>,
}

impl SecretStore for MemorySecretStore {
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError> {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match password {
            Some(p) => {
                m.insert(seat, p.clone());
            }
            None => {
                m.remove(&seat);
            }
        }
        Ok(())
    }
    fn has(&self, seat: SeatNumber) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&seat)
    }
    fn get(&self, seat: SeatNumber) -> Result<Option<Secret>, StoreError> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).get(&seat).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip_and_missing() {
        let dir = std::env::temp_dir().join(format!("seat-core-store-{}", std::process::id()));
        let store = FileStore::new(dir.join("config.json"));
        let _ = std::fs::remove_file(store.path());
        assert_eq!(store.load().unwrap(), Config::default());
        let c = Config { revision: 5, ..Default::default() };
        store.save(&c).unwrap();
        assert_eq!(store.load().unwrap().revision, 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn memory_secrets() {
        let s = MemorySecretStore::default();
        let two = SeatNumber::new(2).unwrap();
        assert!(!s.has(two));
        s.set(two, Some(&Secret::new("p"))).unwrap();
        assert_eq!(s.get(two).unwrap().unwrap().expose(), "p");
        s.set(two, None).unwrap();
        assert!(s.get(two).unwrap().is_none());
    }
}
