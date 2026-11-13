// SPDX-License-Identifier: GPL-3.0-only
//! Persistence of the config document and of seat secrets.
//!
//! - [`ConfigStore`]: production = [`FileStore`] on `%ProgramData%\Pulsar Seat\config.json`
//!   (the service sets the folder ACL, `consts::DATA_DIR_SDDL`); tests = [`MemoryStore`].
//! - [`SecretStore`]: production = LSA private data in `seat-service`
//!   (`consts::LSA_SEAT_SECRET_PREFIX` + two-digit seat for seat passwords,
//!   [`remote_secret_name`] for a remote seat's Pulsar secrets); tests / `--console` dev
//!   mode = [`MemorySecretStore`] (memory only, never on disk). No DPAPI anywhere (merge
//!   decision: LSA for everything, no importer for old Pulsar Split installs).
//!
//! LSA private data limits (checked 2026-11-14, architecture.md section 10.3): a value is
//! an `LSA_UNICODE_STRING` (16-bit byte length, so at most 32 767 UTF-16 units); Microsoft
//! documents "a limited number" of private data objects per system without a figure
//! (HYPOTHESIS from old documentation: a few thousand). Pulsar Seat stores at most one
//! password per seat plus three remote slots per remote seat (16 seats: < 64 objects), and
//! every value is short (identity = 44 base64 characters), so [`MAX_SECRET_CHARS`] is
//! enforced instead of an encrypted-file fallback.

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

/// Longest secret value accepted by every [`SecretStore`] (UTF-16 units). Far below the
/// LSA limit (32 767) and far above every value stored (passwords <= 256, Pulsar secrets
/// <= 128 characters).
pub const MAX_SECRET_CHARS: usize = 4096;

/// The per-seat secrets of a remote seat's Pulsar stream backend (from Pulsar Split, where
/// they were DPAPI blobs under `HKLM\SOFTWARE\PulsarSplit\Secrets\Pulsar`). Keyed by the
/// remote seat's GUID (`RemoteSeat::id`), which survives renames and renumbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RemoteSecretSlot {
    /// 32 random bytes, base64: the seat's identity (its relay id follows from it).
    Identity,
    /// Permanent connect password (optional).
    ConnectPassword,
    /// Relay access key issued to this seat.
    RelayKey,
}

impl RemoteSecretSlot {
    pub const ALL: [RemoteSecretSlot; 3] = [Self::Identity, Self::ConnectPassword, Self::RelayKey];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::ConnectPassword => "connect_password",
            Self::RelayKey => "relay_key",
        }
    }
}

/// LSA name of a remote seat secret: `L$PulsarSeat.Remote.<seat-id>.<slot>` (seat id
/// lower-cased). The id must be 1-64 ASCII hex digits or `-` (a GUID), so a name can never
/// point outside the product's prefix.
pub fn remote_secret_name(seat_id: &str, slot: RemoteSecretSlot) -> Result<String, StoreError> {
    if seat_id.is_empty() || seat_id.len() > 64 || !seat_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(StoreError::Secret("remote seat id must be a GUID".into()));
    }
    Ok(format!("{}{}.{}", crate::consts::LSA_REMOTE_SECRET_PREFIX, seat_id.to_ascii_lowercase(), slot.as_str()))
}

/// Refuse values longer than [`MAX_SECRET_CHARS`] UTF-16 units.
pub fn check_secret_len(value: &Secret) -> Result<(), StoreError> {
    if value.expose().encode_utf16().count() > MAX_SECRET_CHARS {
        return Err(StoreError::Secret(format!("secret longer than {MAX_SECRET_CHARS} characters")));
    }
    Ok(())
}

/// Seat auto-logon passwords and the remote seats' Pulsar secrets. Write-only from the
/// manager's point of view: no pipe request ever returns `get`'s result.
pub trait SecretStore: Send + Sync {
    /// Store (`Some`) or delete (`None`) the seat's password.
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError>;
    fn has(&self, seat: SeatNumber) -> bool;
    /// For a seat start only.
    fn get(&self, seat: SeatNumber) -> Result<Option<Secret>, StoreError>;
    /// Store (`Some`) or delete (`None`; a missing one is fine) a remote seat secret.
    fn set_remote(&self, seat_id: &str, slot: RemoteSecretSlot, value: Option<&Secret>) -> Result<(), StoreError>;
    /// For the remote seat's stream backend only (`pulsar-host` `secrets.load`).
    fn get_remote(&self, seat_id: &str, slot: RemoteSecretSlot) -> Result<Option<Secret>, StoreError>;
}

/// Memory-only secret store (tests, `--console` dev mode).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    inner: Mutex<BTreeMap<SeatNumber, Secret>>,
    remote: Mutex<BTreeMap<String, Secret>>,
}

impl SecretStore for MemorySecretStore {
    fn set(&self, seat: SeatNumber, password: Option<&Secret>) -> Result<(), StoreError> {
        if let Some(p) = password {
            check_secret_len(p)?;
        }
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
    fn set_remote(&self, seat_id: &str, slot: RemoteSecretSlot, value: Option<&Secret>) -> Result<(), StoreError> {
        let name = remote_secret_name(seat_id, slot)?;
        let mut m = self.remote.lock().unwrap_or_else(|e| e.into_inner());
        match value {
            Some(v) => {
                check_secret_len(v)?;
                m.insert(name, v.clone());
            }
            None => {
                m.remove(&name);
            }
        }
        Ok(())
    }
    fn get_remote(&self, seat_id: &str, slot: RemoteSecretSlot) -> Result<Option<Secret>, StoreError> {
        let name = remote_secret_name(seat_id, slot)?;
        Ok(self.remote.lock().unwrap_or_else(|e| e.into_inner()).get(&name).cloned())
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

    #[test]
    fn remote_secret_names_are_confined_to_the_prefix() {
        let id = "0F8FAD5B-D9CB-469F-A165-70867728950E";
        assert_eq!(
            remote_secret_name(id, RemoteSecretSlot::RelayKey).unwrap(),
            "L$PulsarSeat.Remote.0f8fad5b-d9cb-469f-a165-70867728950e.relay_key"
        );
        let long = "a".repeat(65);
        for bad in ["", "..\\x", "a.b", "L$Other", long.as_str()] {
            assert!(remote_secret_name(bad, RemoteSecretSlot::Identity).is_err(), "{bad}");
        }
        let names: std::collections::BTreeSet<String> =
            RemoteSecretSlot::ALL.iter().map(|k| remote_secret_name(id, *k).unwrap()).collect();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn memory_remote_secrets_round_trip_and_size_limit() {
        let s = MemorySecretStore::default();
        let id = "0f8fad5b-d9cb";
        for slot in RemoteSecretSlot::ALL {
            assert!(s.get_remote(id, slot).unwrap().is_none());
            s.set_remote(id, slot, Some(&Secret::new(slot.as_str()))).unwrap();
        }
        assert_eq!(s.get_remote("0F8FAD5B-D9CB", RemoteSecretSlot::RelayKey).unwrap().unwrap().expose(), "relay_key");
        s.set_remote(id, RemoteSecretSlot::RelayKey, None).unwrap();
        s.set_remote(id, RemoteSecretSlot::RelayKey, None).unwrap();
        assert!(s.get_remote(id, RemoteSecretSlot::RelayKey).unwrap().is_none());
        assert_eq!(s.get_remote(id, RemoteSecretSlot::Identity).unwrap().unwrap().expose(), "identity");
        // Seat passwords and remote slots never mix.
        assert!(!s.has(SeatNumber::new(2).unwrap()));
        let big = Secret::new("x".repeat(MAX_SECRET_CHARS + 1));
        assert!(s.set_remote(id, RemoteSecretSlot::Identity, Some(&big)).is_err());
        assert!(s.set(SeatNumber::new(2).unwrap(), Some(&big)).is_err());
        assert!(s.set_remote("not a guid", RemoteSecretSlot::Identity, Some(&Secret::new("x"))).is_err());
    }
}
